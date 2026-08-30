/// 16384, matching real Redis Cluster. Not configurable: a different count would make every
/// off-the-shelf cluster client compute a different slot for the same key, which defeats the
/// entire point of being wire-compatible.
pub const SLOT_COUNT: u16 = 16384;

/// CRC16-CCITT/XMODEM — poly 0x1021, init 0x0000, no reflection, no final XOR. The exact
/// variant real Redis Cluster ships in `crc16.c`, so slots computed here match any cluster-aware
/// client's own computation. Bit-by-bit rather than table-driven: this runs once per command
/// (only in cluster mode) over a key-length input, and a 512-byte table would need its own
/// correctness test to earn its place.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Returns the substring between the first `{` and the first `}` that follows it, when that
/// substring is non-empty; otherwise the whole key. Matches real Redis Cluster's rule exactly,
/// including its two edge cases: `{}foo` (empty tag) and `{foo` (unclosed) both hash the *whole*
/// key. Hash tags are the only mechanism a client has to force related keys onto one node, which
/// is what keeps multi-key commands usable under the CROSSSLOT rule.
fn hash_tag(key: &[u8]) -> &[u8] {
    let Some(open) = key.iter().position(|b| *b == b'{') else {
        return key;
    };
    let Some(close_offset) = key[open + 1..].iter().position(|b| *b == b'}') else {
        return key;
    };
    if close_offset == 0 {
        return key; // `{}` -- an empty tag is not a tag
    }
    &key[open + 1..open + 1 + close_offset]
}

/// The slot a key belongs to: `CRC16(hash_tag(key)) mod 16384`. Pure -- no config, no state --
/// so `CLUSTER KEYSLOT` can answer it identically whether or not this node is in cluster mode.
pub fn key_slot(key: &[u8]) -> u16 {
    crc16(hash_tag(key)) % SLOT_COUNT
}

/// One node of the static cluster topology. `addr` is stored and echoed back verbatim: a
/// `-MOVED` reply must name an address the *client* can reach, which is not necessarily one
/// this process could resolve or would bind, so re-resolving or canonicalizing it here would
/// only ever be a way to be wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNode {
    pub id: String,
    pub addr: String,
    pub first_slot: u16,
    pub last_slot: u16,
}

/// The whole cluster topology, identical on every node, read once at startup. There is no
/// gossip and no resharding, so this never changes for the life of the process.
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    nodes: Vec<ClusterNode>, // sorted by first_slot; validated to cover 0..=16383 exactly once
    myself: usize,           // index into `nodes`
}

fn invalid(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

impl ClusterConfig {
    /// Parses the four-field-per-line topology in `text` and picks out `node_id` as this
    /// process's own entry. Blank lines and `#` comments are skipped. Every failure is an
    /// `InvalidData` io::Error naming the offending line, so `main.rs` can `?` it straight out
    /// of its existing `std::io::Result<()>` -- no new error type, no new dependency.
    ///
    /// Validation is deliberately strict and startup-time: the slot map must cover 0..=16383
    /// exactly once, with no gaps and no overlaps. That is what makes `owner_of` total, so no
    /// request path anywhere ever has to handle an unowned slot.
    pub fn parse(text: &str, node_id: &str) -> std::io::Result<Self> {
        let mut nodes: Vec<ClusterNode> = Vec::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() != 4 {
                return Err(invalid(format!(
                    "cluster config line {}: expected 4 fields (<node-id> <host:port> <first-slot> <last-slot>), found {}",
                    lineno + 1,
                    fields.len()
                )));
            }
            let parse_slot = |raw: &str| -> std::io::Result<u16> {
                let n: u32 = raw.parse().map_err(|_| {
                    invalid(format!(
                        "cluster config line {}: '{raw}' is not a slot number in 0..=16383",
                        lineno + 1
                    ))
                })?;
                if n >= SLOT_COUNT as u32 {
                    return Err(invalid(format!(
                        "cluster config line {}: slot {n} is out of range, the last slot is 16383",
                        lineno + 1
                    )));
                }
                Ok(n as u16)
            };
            let first_slot = parse_slot(fields[2])?;
            let last_slot = parse_slot(fields[3])?;
            if first_slot > last_slot {
                return Err(invalid(format!(
                    "cluster config line {}: first slot {first_slot} is greater than last slot {last_slot}",
                    lineno + 1
                )));
            }
            if nodes.iter().any(|n| n.id == fields[0]) {
                return Err(invalid(format!(
                    "cluster config line {}: duplicate node id '{}'",
                    lineno + 1,
                    fields[0]
                )));
            }
            if nodes.iter().any(|n| n.addr == fields[1]) {
                return Err(invalid(format!(
                    "cluster config line {}: duplicate address '{}'",
                    lineno + 1,
                    fields[1]
                )));
            }
            nodes.push(ClusterNode {
                id: fields[0].to_string(),
                addr: fields[1].to_string(),
                first_slot,
                last_slot,
            });
        }

        if nodes.is_empty() {
            return Err(invalid("cluster config has no nodes".to_string()));
        }
        nodes.sort_by_key(|n| n.first_slot);
        if nodes[0].first_slot != 0 {
            return Err(invalid(format!(
                "cluster config does not assign slot 0 (lowest assigned slot is {})",
                nodes[0].first_slot
            )));
        }
        for pair in nodes.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            if next.first_slot <= prev.last_slot {
                return Err(invalid(format!(
                    "cluster config ranges overlap: '{}' ends at {} but '{}' starts at {}",
                    prev.id, prev.last_slot, next.id, next.first_slot
                )));
            }
            if next.first_slot != prev.last_slot + 1 {
                return Err(invalid(format!(
                    "cluster config has a slot gap: nothing owns slots {}..={}",
                    prev.last_slot + 1,
                    next.first_slot - 1
                )));
            }
        }
        let last = nodes.last().expect("checked non-empty above");
        if last.last_slot != SLOT_COUNT - 1 {
            return Err(invalid(format!(
                "cluster config does not assign every slot: the highest assigned slot is {}, expected 16383",
                last.last_slot
            )));
        }

        let myself = nodes
            .iter()
            .position(|n| n.id == node_id)
            .ok_or_else(|| invalid(format!("cluster config has no node with id '{node_id}'")))?;
        Ok(Self { nodes, myself })
    }

    /// Every node, ordered by `first_slot` -- what `CLUSTER SHARDS`/`CLUSTER NODES` iterate.
    pub fn nodes(&self) -> &[ClusterNode] {
        &self.nodes
    }

    /// This process's own entry, named by `ROCKET_MEM_CLUSTER_NODE_ID`.
    pub fn myself(&self) -> &ClusterNode {
        &self.nodes[self.myself]
    }

    /// Total, never `None`: `parse` validated that the slot map covers every slot exactly once,
    /// so every slot in 0..16384 has exactly one owner.
    pub fn owner_of(&self, slot: u16) -> &ClusterNode {
        self.nodes
            .iter()
            .find(|n| n.first_slot <= slot && slot <= n.last_slot)
            .expect("parse validated that the slot map covers 0..=16383 with no gaps")
    }

    /// Whether this node owns `slot` -- the check `-MOVED` redirection is built on.
    pub fn owns(&self, slot: u16) -> bool {
        let me = self.myself();
        me.first_slot <= slot && slot <= me.last_slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_matches_the_published_xmodem_check_value() {
        // 0x31C3 is the CRC16-CCITT/XMODEM check value for "123456789" -- this pins the
        // *variant* (poly 0x1021, init 0x0000, no reflection, no final XOR), not just the code.
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn keyslot_matches_the_real_redis_cluster_algorithm() {
        assert_eq!(key_slot(b"foo"), 12182); // known reference value from real Redis Cluster
        assert_eq!(key_slot(b"bar"), 5061);
        assert_eq!(key_slot(b"hello"), 866);
        assert_eq!(key_slot(b"user1000"), 3443);
    }

    #[test]
    fn a_hash_tag_makes_related_keys_share_one_slot() {
        assert_eq!(key_slot(b"{user1000}.following"), 3443);
        assert_eq!(key_slot(b"{user1000}.followers"), 3443);
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"user1000"));
    }

    #[test]
    fn only_the_first_brace_pair_counts_as_the_hash_tag() {
        assert_eq!(key_slot(b"foo{bar}{zap}"), key_slot(b"bar"));
        assert_eq!(key_slot(b"foo{bar}{zap}"), 5061);
        // first `{` at index 3, first `}` after it at index 8 => the tag is literally "{bar"
        assert_eq!(key_slot(b"foo{{bar}}zap"), 4015);
    }

    #[test]
    fn an_empty_or_unclosed_tag_hashes_the_whole_key() {
        assert_eq!(key_slot(b"{}foo"), 9500);
        assert_eq!(key_slot(b"{user1000"), 8723);
        assert_ne!(key_slot(b"{user1000"), key_slot(b"user1000"));
    }

    #[test]
    fn every_slot_is_inside_the_16384_slot_space() {
        for i in 0..2000u32 {
            assert!(key_slot(format!("key:{i}").as_bytes()) < SLOT_COUNT);
        }
    }

    const THREE_SHARDS: &str = "\
# <node-id> <host:port> <first-slot> <last-slot>
shard-a 127.0.0.1:7001 0     5460

shard-b 127.0.0.1:7002 5461  10922
shard-c 127.0.0.1:7003 10923 16383
";

    #[test]
    fn parse_reads_every_node_and_skips_comments_and_blank_lines() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-b").unwrap();
        assert_eq!(config.nodes().len(), 3);
        assert_eq!(config.nodes()[0].id, "shard-a");
        assert_eq!(config.nodes()[0].addr, "127.0.0.1:7001");
        assert_eq!(config.nodes()[0].first_slot, 0);
        assert_eq!(config.nodes()[0].last_slot, 5460);
        assert_eq!(config.nodes()[2].last_slot, 16383);
    }

    #[test]
    fn parse_picks_out_this_nodes_own_entry() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-b").unwrap();
        assert_eq!(config.myself().id, "shard-b");
        assert_eq!(config.myself().addr, "127.0.0.1:7002");
    }

    #[test]
    fn parse_sorts_nodes_by_first_slot_regardless_of_file_order() {
        let shuffled = "\
shard-c 127.0.0.1:7003 10923 16383
shard-a 127.0.0.1:7001 0 5460
shard-b 127.0.0.1:7002 5461 10922
";
        let config = ClusterConfig::parse(shuffled, "shard-a").unwrap();
        assert_eq!(config.nodes()[0].id, "shard-a");
        assert_eq!(config.nodes()[1].id, "shard-b");
        assert_eq!(config.nodes()[2].id, "shard-c");
    }

    #[test]
    fn parse_rejects_an_unknown_node_id() {
        let err = ClusterConfig::parse(THREE_SHARDS, "shard-z").unwrap_err();
        assert!(err.to_string().contains("shard-z"));
    }

    #[test]
    fn parse_rejects_a_line_without_exactly_four_fields() {
        let err = ClusterConfig::parse("shard-a 127.0.0.1:7001 0\n", "shard-a").unwrap_err();
        assert!(err.to_string().contains("expected 4 fields"));
    }

    #[test]
    fn parse_rejects_a_slot_gap() {
        let gapped = "\
shard-a 127.0.0.1:7001 0 5460
shard-b 127.0.0.1:7002 5462 16383
";
        let err = ClusterConfig::parse(gapped, "shard-a").unwrap_err();
        assert!(err.to_string().contains("gap"), "{err}");
    }

    #[test]
    fn parse_rejects_overlapping_ranges() {
        let overlapping = "\
shard-a 127.0.0.1:7001 0 8000
shard-b 127.0.0.1:7002 7000 16383
";
        let err = ClusterConfig::parse(overlapping, "shard-a").unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
    }

    #[test]
    fn parse_rejects_a_map_that_does_not_start_at_zero_or_end_at_16383() {
        let short_start = "shard-a 127.0.0.1:7001 1 16383\n";
        assert!(ClusterConfig::parse(short_start, "shard-a")
            .unwrap_err()
            .to_string()
            .contains("slot 0"));
        let short_end = "shard-a 127.0.0.1:7001 0 16382\n";
        assert!(ClusterConfig::parse(short_end, "shard-a")
            .unwrap_err()
            .to_string()
            .contains("16383"));
    }

    #[test]
    fn parse_rejects_a_slot_number_outside_the_slot_space() {
        let err = ClusterConfig::parse("shard-a 127.0.0.1:7001 0 16384\n", "shard-a").unwrap_err();
        assert!(err.to_string().contains("16383"), "{err}");
    }

    #[test]
    fn parse_rejects_first_slot_greater_than_last_slot() {
        let err = ClusterConfig::parse("shard-a 127.0.0.1:7001 900 100\n", "shard-a").unwrap_err();
        assert!(err.to_string().contains("first slot"), "{err}");
    }

    #[test]
    fn parse_rejects_duplicate_ids_and_duplicate_addresses() {
        let dup_id = "\
shard-a 127.0.0.1:7001 0 8000
shard-a 127.0.0.1:7002 8001 16383
";
        assert!(ClusterConfig::parse(dup_id, "shard-a")
            .unwrap_err()
            .to_string()
            .contains("duplicate node id"));
        let dup_addr = "\
shard-a 127.0.0.1:7001 0 8000
shard-b 127.0.0.1:7001 8001 16383
";
        assert!(ClusterConfig::parse(dup_addr, "shard-a")
            .unwrap_err()
            .to_string()
            .contains("duplicate address"));
    }

    #[test]
    fn parse_rejects_an_empty_config() {
        let err = ClusterConfig::parse("# nothing but a comment\n", "shard-a").unwrap_err();
        assert!(err.to_string().contains("no nodes"), "{err}");
    }

    #[test]
    fn a_single_node_owning_every_slot_is_valid() {
        let config = ClusterConfig::parse("solo 127.0.0.1:6379 0 16383\n", "solo").unwrap();
        assert_eq!(config.nodes().len(), 1);
        assert_eq!(config.myself().id, "solo");
    }
}
