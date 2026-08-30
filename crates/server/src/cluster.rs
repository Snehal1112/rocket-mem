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
