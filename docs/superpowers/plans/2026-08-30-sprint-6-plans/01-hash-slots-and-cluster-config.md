# Hash Slots & Static Cluster Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a node can compute any key's Redis-Cluster-compatible hash slot and know, from a static config file, which node in the cluster owns it.

**Architecture:** a new `crates/server/src/cluster.rs` holds a pure `key_slot(&[u8]) -> u16` (hand-rolled CRC16-CCITT/XMODEM over Redis's hash-tag rule, mod 16384) plus a `ClusterConfig` parsed from a four-field-per-line text file and validated at startup to cover all 16384 slots exactly once. The config reaches the dispatcher the same way Sprint 5's `AofWriter` did — an `Option<Arc<ClusterConfig>>` field on `ReplicationHandle`, set by a `with_cluster` builder method that only `main.rs` and the cluster tests call, so every existing call site keeps cluster mode off.

**Tech Stack:** `std` only — no new dependency (CRC16 is 16 lines; the config parser is line-splitting; errors are `std::io::Error(InvalidData, _)` so `main.rs` can `?` them out of its existing `std::io::Result<()>`).

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "16384 slots, `CRC16-CCITT/XMODEM` hand-rolled, hash tags supported" and "slot ownership comes from one static text config file" are authoritative for this plan.

## Global Constraints

- Cluster hash slots (16384, `CRC16(hash_tag(key)) % 16384`, routes keys to *nodes*) are a completely different mechanism from the engine's internal 16 shards (`DefaultHasher(key) % 16`, `crates/engine/src/store.rs:20-24`, routes keys to *locks inside one process*). This plan touches only the former. The `engine` crate is not modified at all.
- No cluster bus, no gossip, no failover, no live resharding, no cross-node forwarding. Nodes never talk to each other; every node reads the same static file.
- `ROCKET_MEM_CLUSTER_CONFIG` unset means cluster mode is off, which must remain byte-for-byte today's behavior for every existing test and deployment.
- Slot-map validation is strict and happens at startup: an incomplete or overlapping cover is a hard startup failure, never a request-time surprise. This is what makes `owner_of` total (returning `&ClusterNode`, not `Option`).

---

### Task 1: `crc16` and `key_slot`

**Files:**
- Create: `crates/server/src/cluster.rs`
- Modify: `crates/server/src/lib.rs` (currently 5 lines: `pub mod aof; pub mod connection; pub mod dispatcher; pub mod replication; pub use connection::serve;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub const SLOT_COUNT: u16` and `pub fn key_slot(key: &[u8]) -> u16` in `crate::cluster`, consumed by Task 3 (`ClusterConfig::owns`) and by `02-cluster-commands-and-moved.md`.

- [ ] **Step 1: Declare the module**

```rust
// crates/server/src/lib.rs — add the new module, keeping the list alphabetical
pub mod aof;
pub mod cluster;
pub mod connection;
pub mod dispatcher;
pub mod replication;
pub use connection::serve;
```

- [ ] **Step 2: Write the failing tests**

```rust
// crates/server/src/cluster.rs — the whole file, for now just the tests
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
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem cluster::tests`
Expected: FAIL to compile with "cannot find function `crc16`"/"cannot find function `key_slot`"/"cannot find value `SLOT_COUNT`" — `cluster.rs` contains only the test module

- [ ] **Step 4: Implement the slot algorithm**

```rust
// crates/server/src/cluster.rs — add above the tests module
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem cluster::tests`
Expected: PASS, all 6 tests

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/lib.rs crates/server/src/cluster.rs
git commit -m "feat(server): add Redis-Cluster-compatible key_slot with CRC16 and hash tags"
```

---

### Task 2: `ClusterNode`, `ClusterConfig::parse` and its validation

**Files:**
- Modify: `crates/server/src/cluster.rs`

**Interfaces:**
- Consumes: `key_slot`/`SLOT_COUNT` (Task 1).
- Produces: `pub struct ClusterNode { pub id: String, pub addr: String, pub first_slot: u16, pub last_slot: u16 }` and `pub struct ClusterConfig` with `pub fn parse(text: &str, node_id: &str) -> std::io::Result<Self>`, consumed by Task 3.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/cluster.rs — add to the existing tests module
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem cluster::tests::parse`
Expected: FAIL to compile with "cannot find type `ClusterConfig`"

- [ ] **Step 3: Implement `ClusterNode`/`ClusterConfig::parse`**

```rust
// crates/server/src/cluster.rs — add above the tests module
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
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem cluster::tests`
Expected: FAIL — the accessors `nodes()` and `myself()` the tests call don't exist yet

- [ ] **Step 5: Add the accessors**

```rust
// crates/server/src/cluster.rs — inside the existing `impl ClusterConfig` block
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
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem cluster::tests`
Expected: PASS, all tests in the module

- [ ] **Step 7: Commit**

```bash
git add crates/server/src/cluster.rs
git commit -m "feat(server): add ClusterConfig parsing with strict slot-map validation"
```

---

### Task 3: `owns`/`owner_of` coverage and `ClusterConfig::load`

**Files:**
- Modify: `crates/server/src/cluster.rs`

**Interfaces:**
- Consumes: `ClusterConfig::parse` (Task 2), `key_slot` (Task 1).
- Produces: `pub fn load(path: &std::path::Path, node_id: &str) -> std::io::Result<Self>`, consumed by Task 5 (`main.rs`).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/cluster.rs — add to the existing tests module
    #[test]
    fn owns_is_true_only_for_this_nodes_own_range() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-b").unwrap();
        assert!(!config.owns(0));
        assert!(!config.owns(5460));
        assert!(config.owns(5461));
        assert!(config.owns(10922));
        assert!(!config.owns(10923));
        assert!(!config.owns(16383));
    }

    #[test]
    fn owner_of_finds_the_right_node_for_every_boundary_slot() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-a").unwrap();
        assert_eq!(config.owner_of(0).id, "shard-a");
        assert_eq!(config.owner_of(5460).id, "shard-a");
        assert_eq!(config.owner_of(5461).id, "shard-b");
        assert_eq!(config.owner_of(10922).id, "shard-b");
        assert_eq!(config.owner_of(10923).id, "shard-c");
        assert_eq!(config.owner_of(16383).id, "shard-c");
    }

    #[test]
    fn every_slot_in_the_whole_space_has_exactly_one_owner() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-a").unwrap();
        for slot in 0..SLOT_COUNT {
            let owner = config.owner_of(slot);
            let matches = config
                .nodes()
                .iter()
                .filter(|n| n.first_slot <= slot && slot <= n.last_slot)
                .count();
            assert_eq!(matches, 1, "slot {slot} has {matches} owners");
            assert!(owner.first_slot <= slot && slot <= owner.last_slot);
        }
    }

    #[test]
    fn the_well_known_reference_keys_land_on_the_expected_shards() {
        let config = ClusterConfig::parse(THREE_SHARDS, "shard-a").unwrap();
        // one key per shard, so this test would fail if any range were misparsed:
        assert_eq!(config.owner_of(key_slot(b"hello")).id, "shard-a"); // slot 866
        assert_eq!(config.owner_of(key_slot(b"counter")).id, "shard-b"); // slot 6680
        assert_eq!(config.owner_of(key_slot(b"foo")).id, "shard-c"); // slot 12182
    }

    #[test]
    fn load_reads_a_config_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster.conf");
        std::fs::write(&path, THREE_SHARDS).unwrap();
        let config = ClusterConfig::load(&path, "shard-c").unwrap();
        assert_eq!(config.myself().id, "shard-c");
    }

    #[test]
    fn load_surfaces_a_missing_file_as_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = ClusterConfig::load(&dir.path().join("nope.conf"), "shard-a").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem cluster::tests::load`
Expected: FAIL to compile with "no function or associated item named `load` found" (the `owns`/`owner_of` tests already pass — they were implemented in Task 2's Step 5 and are regression cover here)

- [ ] **Step 3: Implement `load`**

```rust
// crates/server/src/cluster.rs — inside the existing `impl ClusterConfig` block
    /// Reads the topology file at `path` and delegates to `parse`. A missing file surfaces as
    /// the underlying `NotFound` io::Error rather than being treated as "cluster mode off":
    /// `ROCKET_MEM_CLUSTER_CONFIG` being *set* to a path that doesn't exist is an operator
    /// mistake, and starting up silently in standalone mode would hide it until keys started
    /// landing on the wrong node.
    pub fn load(path: &std::path::Path, node_id: &str) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text, node_id)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem cluster::tests`
Expected: PASS, every test in the module

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/cluster.rs
git commit -m "feat(server): add ClusterConfig::load and slot-ownership lookups"
```

---

### Task 4: `ReplicationHandle::with_cluster`/`cluster()`

**Files:**
- Modify: `crates/server/src/replication.rs` (struct at `:42`, `new` at `:93`, `with_aof` at `:113`, `engine()` at `:120`, `snapshot_path()` at `:125`)

**Interfaces:**
- Consumes: `ClusterConfig` (Tasks 2–3).
- Produces: `ReplicationHandle::with_cluster(mut self, cluster: Arc<ClusterConfig>) -> Self` and `ReplicationHandle::cluster(&self) -> Option<&Arc<ClusterConfig>>`, consumed by `02-cluster-commands-and-moved.md` and Task 5 below.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/server/src/replication.rs — add to the existing tests module (starts at :284)
    #[test]
    fn a_handle_is_not_in_cluster_mode_by_default() {
        let h = ReplicationHandle::default();
        assert!(h.cluster().is_none());
    }

    #[test]
    fn with_cluster_puts_the_handle_into_cluster_mode() {
        let config = crate::cluster::ClusterConfig::parse(
            "shard-a 127.0.0.1:7001 0 8000\nshard-b 127.0.0.1:7002 8001 16383\n",
            "shard-b",
        )
        .unwrap();
        let h = ReplicationHandle::new(Arc::new(Engine::new()), "/tmp/does-not-matter".into())
            .with_cluster(Arc::new(config));
        let cluster = h.cluster().expect("cluster mode should be on");
        assert_eq!(cluster.myself().id, "shard-b");
        assert!(cluster.owns(8001));
        assert!(!cluster.owns(8000));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem replication::tests::a_handle_is_not_in_cluster replication::tests::with_cluster`
Expected: FAIL to compile with "no method named `cluster`"/"no method named `with_cluster`"

- [ ] **Step 3: Add the field and the two methods**

```rust
// crates/server/src/replication.rs — add as the last field of `pub struct ReplicationHandle`
    /// The static cluster topology, when this node was started in cluster mode. `None` -- the
    /// default for `new`/`Default`, i.e. every existing test and every standalone deployment --
    /// means cluster mode is off: no `-MOVED`, no `-CROSSSLOT`, `cluster_enabled:0` in `INFO`.
    /// A builder-set `Option` rather than a third `new` parameter, mirroring `with_aof` above
    /// and for the same reason: the existing `ReplicationHandle::new`/`::default()` call sites
    /// (all of them tests) stay untouched.
    ///
    /// Naming note: this struct now carries a snapshot path, an AOF handle, and a cluster
    /// config -- it is shared *server* state, not a replication handle. Renaming it to
    /// `ServerState` is deferred to Sprint 7, whose dual-protocol work has to touch these
    /// signatures anyway; see ../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md.
    cluster: Option<Arc<crate::cluster::ClusterConfig>>,
```

```rust
// crates/server/src/replication.rs — in `new`'s struct literal (:93), add the field
            cluster: None,
```

```rust
// crates/server/src/replication.rs — in the existing `impl ReplicationHandle` block,
// directly after `with_aof` (:113)
    /// Puts this node into cluster mode with the given static topology. Only `main.rs` and
    /// `crates/server/tests/cluster.rs` call this; everything else leaves cluster mode off.
    pub fn with_cluster(mut self, cluster: Arc<crate::cluster::ClusterConfig>) -> Self {
        self.cluster = Some(cluster);
        self
    }

    /// `None` when cluster mode is off. `dispatch_and_log`'s redirection gate short-circuits on
    /// this before extracting any key, so a standalone node pays one `Option` check per command.
    pub fn cluster(&self) -> Option<&Arc<crate::cluster::ClusterConfig>> {
        self.cluster.as_ref()
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem replication::tests`
Expected: PASS, every test in the module

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/replication.rs
git commit -m "feat(server): thread an optional ClusterConfig through ReplicationHandle"
```

---

### Task 5: `main.rs` wiring for the two new env vars

**Files:**
- Modify: `crates/server/src/main.rs` (40 lines; env vars read at `:5-11`, `ReplicationHandle` built at `:28-34`)

**Interfaces:**
- Consumes: `ClusterConfig::load` (Task 3), `ReplicationHandle::with_cluster` (Task 4).
- Produces: a binary that starts in cluster mode when `ROCKET_MEM_CLUSTER_CONFIG` and `ROCKET_MEM_CLUSTER_NODE_ID` are both set. Nothing later in this sprint consumes it programmatically; `03-cluster-integration-tests.md` builds its configs in-process instead.

- [ ] **Step 1: Add the config load and the builder call**

There is no unit test for `main`, which has no testable seam — the parser and the handle plumbing it calls are both covered by Tasks 2–4. Verification is Step 2's manual run.

```rust
// crates/server/src/main.rs — insert after the snapshot_path lines (:9-11)
    // Cluster mode is opt-in and all-or-nothing: the topology file names every node's slot
    // range, and ROCKET_MEM_CLUSTER_NODE_ID says which line is this process. Both must be set
    // together -- one without the other is an operator mistake that would otherwise start a
    // node in standalone mode while its neighbours redirect keys to it.
    let cluster = match (
        std::env::var("ROCKET_MEM_CLUSTER_CONFIG"),
        std::env::var("ROCKET_MEM_CLUSTER_NODE_ID"),
    ) {
        (Ok(path), Ok(node_id)) => {
            let config =
                rocket_mem::cluster::ClusterConfig::load(std::path::Path::new(&path), &node_id)?;
            println!(
                "Cluster mode enabled: node '{}' at {} owns slots {}-{} of {} nodes",
                config.myself().id,
                config.myself().addr,
                config.myself().first_slot,
                config.myself().last_slot,
                config.nodes().len()
            );
            Some(Arc::new(config))
        }
        (Ok(_), Err(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ROCKET_MEM_CLUSTER_CONFIG is set but ROCKET_MEM_CLUSTER_NODE_ID is not",
            ))
        }
        (Err(_), Ok(_)) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ROCKET_MEM_CLUSTER_NODE_ID is set but ROCKET_MEM_CLUSTER_CONFIG is not",
            ))
        }
        (Err(_), Err(_)) => None,
    };
```

```rust
// crates/server/src/main.rs — replace the `let replication = ...` block (:28-34) with:
    let mut handle = rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        snapshot_path.to_path_buf(),
    )
    .with_aof(Arc::clone(&aof));
    if let Some(cluster) = cluster {
        handle = handle.with_cluster(cluster);
    }
    let replication = Arc::new(handle);
```

- [ ] **Step 2: Verify the binary starts in both modes**

```bash
cargo build --workspace
# standalone: unchanged behavior, no cluster line printed
ROCKET_MEM_ADDR=127.0.0.1:7999 \
  ROCKET_MEM_AOF_PATH=/tmp/rm-standalone.aof \
  ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-standalone.snapshot \
  timeout 2 ./target/debug/rocket-mem

printf 'shard-a 127.0.0.1:7001 0 5460\nshard-b 127.0.0.1:7002 5461 10922\nshard-c 127.0.0.1:7003 10923 16383\n' > /tmp/rm-cluster.conf
# cluster mode: prints the ownership line
ROCKET_MEM_ADDR=127.0.0.1:7001 \
  ROCKET_MEM_AOF_PATH=/tmp/rm-a.aof \
  ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-a.snapshot \
  ROCKET_MEM_CLUSTER_CONFIG=/tmp/rm-cluster.conf \
  ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
  timeout 2 ./target/debug/rocket-mem

# half-configured: exits with the InvalidInput message instead of starting
ROCKET_MEM_CLUSTER_CONFIG=/tmp/rm-cluster.conf timeout 2 ./target/debug/rocket-mem; echo "exit=$?"
```

Expected: the first prints only the existing `Recovered state ...`/`Listening on ...` lines; the second additionally prints `Cluster mode enabled: node 'shard-a' at 127.0.0.1:7001 owns slots 0-5460 of 3 nodes`; the third exits non-zero printing the `ROCKET_MEM_CLUSTER_NODE_ID is not` error.

- [ ] **Step 3: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "feat(server): start in cluster mode from ROCKET_MEM_CLUSTER_CONFIG"
```
