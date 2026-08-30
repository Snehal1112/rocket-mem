# `CLUSTER` Commands & `MOVED` Redirection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a cluster-mode node answers `CLUSTER KEYSLOT`/`MYID`/`INFO`/`SHARDS`/`NODES`, and refuses any key it does not own with `-MOVED <slot> <host>:<port>` before touching the engine, the AOF, or any lock.

**Architecture:** both features are interceptions inside `dispatch_and_log`, exactly like Sprint 5's `SAVE` and `REPLICAOF` — plain `dispatch` never learns they exist, which is what keeps AOF replay and the follower apply loop (both of which call `dispatch` directly) from ever being redirected. A `KeySpec` table maps each of the 84 known command names to which of its arguments are keys; a command whose keys span more than one slot is rejected with `-CROSSSLOT` rather than half-executed on the wrong node.

**Tech Stack:** `std` only. Reuses `crate::cluster::{key_slot, ClusterConfig, SLOT_COUNT}` from `01-hash-slots-and-cluster-config.md`.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "`CLUSTER` is a dispatcher interception with five subcommands" and "`-MOVED` is checked at the top of `dispatch_and_log`, before `-READONLY`" are authoritative for this plan. Depends on `01-hash-slots-and-cluster-config.md` (`key_slot`, `ClusterConfig`, `ReplicationHandle::cluster()`).

## Global Constraints

- **`-MOVED` beats `-READONLY`.** A node can be both a cluster shard and a Sprint-5 follower, so both gates can fire for one command. The redirect goes first: it says *which node should handle this key at all*, and a client that gets `-READONLY` from the wrong node has no path to success, while a client that follows `-MOVED` reaches the owner and gets a correct `-READONLY` there if that node is itself a follower.
- **Never in `dispatch`.** `aof::replay` (`crates/server/src/aof.rs:238`) and `replication::sync_once` call `dispatch` directly and must apply every frame they are handed regardless of slot ownership — redirecting there would silently drop writes during recovery and replication.
- **A redirected command touches nothing.** The check runs before the `-READONLY` gate, before the `SAVE`/`REPLICAOF`/`CLUSTER` interceptions, and long before `extract_write_command_name` acquires `aof.lock_for_ordering()`.
- **No `ASK`/`ASKING`, no `CLUSTER SLOTS`, no `CLUSTER SETSLOT`.** `ASK` exists in real Redis only to cover an in-progress slot migration, and there are no migrations here; `CLUSTER SLOTS` has been deprecated since Redis 7.0 in favour of `CLUSTER SHARDS`, which this plan implements.
- **Cluster mode off must stay byte-for-byte today's behavior**, at the cost of one `Option` check per command. `CLUSTER KEYSLOT` is the single exception: it is a pure function of the key and answers identically either way.

---

### Task 1: the `KeySpec` routing table

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (add below `extract_write_command_name`, `:926-937`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub(crate) const KNOWN_COMMANDS: &[&str]` (sorted; also consumed by `04-prometheus-metrics.md`'s `metric_label`), `enum KeySpec`, `fn key_spec(name: &str) -> KeySpec`, and `fn command_keys(frame: &Frame) -> Vec<&Bytes>`, consumed by Task 3.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module (starts at :1296)
    #[test]
    fn known_commands_is_sorted_so_binary_search_works() {
        let mut sorted = KNOWN_COMMANDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, KNOWN_COMMANDS.to_vec());
        assert!(KNOWN_COMMANDS.binary_search(&"GET").is_ok());
        assert!(KNOWN_COMMANDS.binary_search(&"ZSCORE").is_ok());
        assert!(KNOWN_COMMANDS.binary_search(&"NOSUCHCOMMAND").is_err());
    }

    #[test]
    fn command_keys_finds_the_single_key_of_an_ordinary_command() {
        assert_eq!(
            command_keys(&cmd(&[b"GET", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"SET", b"foo", b"bar", b"EX", b"10"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"HSET", b"h", b"field", b"value"])),
            vec![&Bytes::from_static(b"h")]
        );
    }

    #[test]
    fn command_keys_is_empty_for_commands_that_take_no_key() {
        for c in [
            cmd(&[b"PING"]),
            cmd(&[b"ECHO", b"hello"]),
            cmd(&[b"SELECT", b"0"]),
            cmd(&[b"COMMAND"]),
            cmd(&[b"INFO", b"replication"]),
            cmd(&[b"HELLO", b"3"]),
            cmd(&[b"KEYS", b"*"]),
            cmd(&[b"SCAN", b"0"]),
            cmd(&[b"RANDOMKEY"]),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
            cmd(&[b"SAVE"]),
            cmd(&[b"REPLICAOF", b"NO", b"ONE"]),
            cmd(&[b"PSYNC"]),
            cmd(&[b"SLOWLOG", b"GET"]),
        ] {
            assert!(command_keys(&c).is_empty(), "expected no keys for {c:?}");
        }
    }

    #[test]
    fn command_keys_is_empty_for_an_unknown_command() {
        // An unknown command must fall through to dispatch's "ERR unknown command" error, not
        // get redirected on a slot computed from an argument that isn't a key.
        assert!(command_keys(&cmd(&[b"NOSUCHCOMMAND", b"foo"])).is_empty());
    }

    #[test]
    fn command_keys_takes_the_second_argument_for_memory_usage_and_object_encoding() {
        assert_eq!(
            command_keys(&cmd(&[b"MEMORY", b"USAGE", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"OBJECT", b"ENCODING", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert!(command_keys(&cmd(&[b"MEMORY"])).is_empty());
    }

    #[test]
    fn command_keys_takes_every_argument_for_variadic_key_commands() {
        assert_eq!(
            command_keys(&cmd(&[b"DEL", b"a", b"b", b"c"])),
            vec![
                &Bytes::from_static(b"a"),
                &Bytes::from_static(b"b"),
                &Bytes::from_static(b"c")
            ]
        );
        assert_eq!(
            command_keys(&cmd(&[b"MGET", b"a", b"b"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"RENAME", b"a", b"b"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        // the destination is a key this node would WRITE, so it must be routed too
        assert_eq!(
            command_keys(&cmd(&[b"SINTERSTORE", b"dest", b"s1", b"s2"])),
            vec![
                &Bytes::from_static(b"dest"),
                &Bytes::from_static(b"s1"),
                &Bytes::from_static(b"s2")
            ]
        );
    }

    #[test]
    fn command_keys_takes_every_other_argument_for_mset() {
        assert_eq!(
            command_keys(&cmd(&[b"MSET", b"a", b"1", b"b", b"2"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"MSETNX", b"a", b"1", b"b", b"2"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
    }
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::command_keys dispatcher::tests::known_commands`
Expected: FAIL to compile with "cannot find function `command_keys`"/"cannot find value `KNOWN_COMMANDS`"

- [x] **Step 3: Implement the table**

```rust
// crates/server/src/dispatcher.rs — add directly below `extract_write_command_name` (:937)
/// Every command name this server answers -- `dispatch`'s match arms plus the interceptions
/// `dispatch_and_log` handles (`SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER`, `SLOWLOG`). Sorted, so
/// `binary_search` is valid; `known_commands_is_sorted_so_binary_search_works` is the guard that
/// keeps it that way when a future sprint adds one.
///
/// Two consumers: `key_spec` below (an unknown command has no keys, so it falls through to
/// dispatch's unknown-command error instead of being redirected on a slot computed from a
/// non-key argument), and `04-prometheus-metrics.md`'s `metric_label` (which collapses anything
/// not in this list to `other`, bounding Prometheus label cardinality).
///
/// **Every command added to `dispatch` from now on must be added here too.** A missing name is
/// not a compile error: it silently becomes `KeySpec::None`, which means that command is never
/// slot-routed in cluster mode -- it would be served by whichever node the client happened to
/// reach, quietly breaking the routing invariant. Step 3a below is the check.
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "APPEND", "CLUSTER", "COMMAND", "DECR", "DEL", "ECHO", "EXISTS", "EXPIRE", "EXPIREAT", "GET",
    "GETRANGE", "GETSET", "HDEL", "HELLO", "HEXISTS", "HGET", "HGETALL", "HINCRBY", "HKEYS",
    "HLEN", "HMGET", "HSCAN", "HSET", "HSETNX", "HVALS", "INCR", "INCRBY", "INFO", "KEYS",
    "LINDEX", "LINSERT", "LLEN", "LPOP", "LPUSH", "LRANGE", "LREM", "LSET", "LTRIM", "MEMORY",
    "MGET", "MSET", "MSETNX", "OBJECT", "PERSIST", "PEXPIRE", "PEXPIREAT", "PING", "PSYNC",
    "PTTL", "RANDOMKEY", "RENAME", "RENAMENX", "REPLICAOF", "RPOP", "RPUSH", "SADD", "SAVE",
    "SCAN", "SCARD", "SDIFF", "SDIFFSTORE", "SELECT", "SET", "SETRANGE", "SINTER", "SINTERSTORE",
    "SISMEMBER", "SLOWLOG", "SMEMBERS", "SPOP", "SRANDMEMBER", "SREM", "STRLEN", "SUNION",
    "SUNIONSTORE", "TTL", "TYPE", "ZADD", "ZCARD", "ZINCRBY", "ZRANGE", "ZRANK", "ZREM", "ZSCORE",
];

/// Which of a command's arguments are keys, for cluster-slot routing. Total over every command
/// this server answers; `First` is the default because it is correct for ~70 of the 84, and
/// every exception is enumerated in `key_spec`.
enum KeySpec {
    /// No keys at all -- never redirected. Also the answer for unknown commands.
    None,
    /// The first argument (`GET k`, `SET k v`, `ZADD k ...`).
    First,
    /// The second argument (`MEMORY USAGE k`, `OBJECT ENCODING k`).
    Second,
    /// Every argument (`DEL a b c`, `RENAME a b`, `SINTERSTORE dest s1 s2` -- the destination is
    /// a key this node would write, so it must hash to the same slot as the sources).
    All,
    /// Arguments 0, 2, 4, ... (`MSET k1 v1 k2 v2`).
    EveryOther,
}

fn key_spec(name: &str) -> KeySpec {
    match name {
        "PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
        | "RANDOMKEY" | "CLUSTER" | "SAVE" | "REPLICAOF" | "PSYNC" | "SLOWLOG" => KeySpec::None,
        "MEMORY" | "OBJECT" => KeySpec::Second,
        "DEL" | "EXISTS" | "MGET" | "RENAME" | "RENAMENX" | "SINTER" | "SUNION" | "SDIFF"
        | "SINTERSTORE" | "SUNIONSTORE" | "SDIFFSTORE" => KeySpec::All,
        "MSET" | "MSETNX" => KeySpec::EveryOther,
        _ if KNOWN_COMMANDS.binary_search(&name).is_ok() => KeySpec::First,
        _ => KeySpec::None, // unknown command: no keys, so dispatch's own error reaches the client
    }
}

/// The keys `frame`'s command operates on, borrowed from the frame. Empty for a malformed frame,
/// a keyless command, or an unknown command -- all three of which must reach their normal
/// handling rather than being redirected.
fn command_keys(frame: &Frame) -> Vec<&Bytes> {
    let Frame::Array(items) = frame else {
        return Vec::new();
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return Vec::new();
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    let args: Vec<&Bytes> = items[1..]
        .iter()
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b),
            _ => None,
        })
        .collect();
    match key_spec(&name) {
        KeySpec::None => Vec::new(),
        KeySpec::First => args.into_iter().take(1).collect(),
        KeySpec::Second => args.into_iter().skip(1).take(1).collect(),
        KeySpec::All => args,
        KeySpec::EveryOther => args.into_iter().step_by(2).collect(),
    }
}
```

- [x] **Step 3a: Verify the list still matches `dispatch`'s match arms**

The list above was transcribed from `dispatch`'s match on `2026-08-30` and covers 84 names. If
any command has been added to `dispatch` since (this repo had an uncommitted `SSCAN` arm in flight
at the time this plan was written), it must be added to `KNOWN_COMMANDS` in sorted position, and
given a `key_spec` entry if it is not a plain first-argument-key command. Check with:

```bash
grep -nE '^        "[A-Z]+"( *\| *"[A-Z]+")* =>' crates/server/src/dispatcher.rs \
  | grep -oE '"[A-Z]+"' | tr -d '"' | sort -u > /tmp/dispatch-arms.txt
# then compare /tmp/dispatch-arms.txt against KNOWN_COMMANDS -- every name in the file must
# appear in the list (the list additionally holds CLUSTER, SAVE, REPLICAOF, PSYNC and SLOWLOG,
# which are interceptions rather than match arms, plus INFO/HELLO if plan 05 has already moved
# them out of the match).
```

For each name that is in the file but not in the list: insert it in sorted position, and decide
its `KeySpec` (an `SSCAN key cursor`, for example, is the default `First`). Note the second `grep`
also picks up uppercase string literals that happen to sit on a match-arm line — `OK` from
`"SELECT" => Frame::Simple("OK".into())` is the one that shows up today — so ignore anything that
is obviously not a command name.

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::command_keys dispatcher::tests::known_commands`
Expected: PASS, all 7 tests

- [x] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add the cluster key-extraction table for slot routing"
```

---

### Task 2: `CLUSTER KEYSLOT`/`MYID`/`INFO`/`SHARDS`/`NODES`

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (new helpers below `handle_replicaof`, `:952-984`; interception added inside `dispatch_and_log`, `:1062-1067`)

**Interfaces:**
- Consumes: `ReplicationHandle::cluster()` and `crate::cluster::{key_slot, ClusterConfig, SLOT_COUNT}` (plan 01).
- Produces: `fn handle_cluster(frame: &Frame, replication: &ReplicationHandle) -> Option<Frame>`, reachable over the wire; consumed end-to-end by `03-cluster-integration-tests.md`.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    /// A three-shard topology whose ranges are the even thirds of the slot space, with this
    /// process being `node_id`. Uses `ReplicationHandle::default()` (its own throwaway Engine
    /// and the `./dump.snapshot` path) because none of these tests issue a SAVE.
    fn cluster_handle(node_id: &str) -> ReplicationHandle {
        let config = crate::cluster::ClusterConfig::parse(
            "shard-a 127.0.0.1:7001 0 5460\n\
             shard-b 127.0.0.1:7002 5461 10922\n\
             shard-c 127.0.0.1:7003 10923 16383\n",
            node_id,
        )
        .unwrap();
        ReplicationHandle::default().with_cluster(std::sync::Arc::new(config))
    }

    #[test]
    fn cluster_keyslot_answers_the_reference_slot_even_with_cluster_mode_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Integer(12182));
    }

    #[test]
    fn cluster_keyslot_honours_hash_tags() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"{user1000}.following"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Integer(3443));
    }

    #[test]
    fn cluster_keyslot_with_wrong_arity_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"KEYSLOT"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("ERR wrong number of arguments for 'cluster|keyslot' command".into())
        );
    }

    #[test]
    fn cluster_myid_returns_this_nodes_id_or_a_zero_id_when_disabled() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-b"),
                cmd(&[b"CLUSTER", b"MYID"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"shard-b"))
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"MYID"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from("0".repeat(40)))
        );
    }

    #[test]
    fn cluster_info_reports_enabled_and_the_node_count() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_enabled:1\r\n"), "{text}");
        assert!(text.contains("cluster_state:ok\r\n"), "{text}");
        assert!(text.contains("cluster_slots_assigned:16384\r\n"), "{text}");
        assert!(text.contains("cluster_known_nodes:3\r\n"), "{text}");
        assert!(text.contains("cluster_size:3\r\n"), "{text}");
    }

    #[test]
    fn cluster_info_reports_disabled_when_no_config_was_loaded() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"CLUSTER", b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_enabled:0\r\n"), "{text}");
        assert!(text.contains("cluster_known_nodes:0\r\n"), "{text}");
    }

    #[test]
    fn cluster_nodes_lists_every_node_with_myself_flagged() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-b"),
            cmd(&[b"CLUSTER", b"NODES"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "shard-a 127.0.0.1:7001@17001 master - 0 0 0 connected 0-5460"
        );
        assert_eq!(
            lines[1],
            "shard-b 127.0.0.1:7002@17002 myself,master - 0 0 0 connected 5461-10922"
        );
        assert_eq!(
            lines[2],
            "shard-c 127.0.0.1:7003@17003 master - 0 0 0 connected 10923-16383"
        );
    }

    #[test]
    fn cluster_nodes_is_empty_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"NODES"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b""))
        );
    }

    #[test]
    fn cluster_shards_describes_every_shards_slots_and_its_one_node() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Array(shards) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"SHARDS"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(shards.len(), 3);
        let Frame::Array(first) = &shards[0] else {
            panic!("expected each shard to be an Array of alternating key/value frames")
        };
        assert_eq!(first[0], Frame::Bulk(Bytes::from_static(b"slots")));
        assert_eq!(
            first[1],
            Frame::Array(vec![Frame::Integer(0), Frame::Integer(5460)])
        );
        assert_eq!(first[2], Frame::Bulk(Bytes::from_static(b"nodes")));
        let Frame::Array(nodes) = &first[3] else {
            panic!("expected a nodes array")
        };
        assert_eq!(nodes.len(), 1, "a shard has exactly one node this sprint");
        let Frame::Array(node) = &nodes[0] else {
            panic!("expected the node to be an Array of alternating key/value frames")
        };
        assert_eq!(node[0], Frame::Bulk(Bytes::from_static(b"id")));
        assert_eq!(node[1], Frame::Bulk(Bytes::from_static(b"shard-a")));
        assert_eq!(node[2], Frame::Bulk(Bytes::from_static(b"port")));
        assert_eq!(node[3], Frame::Integer(7001));
        assert_eq!(node[4], Frame::Bulk(Bytes::from_static(b"ip")));
        assert_eq!(node[5], Frame::Bulk(Bytes::from_static(b"127.0.0.1")));
        assert_eq!(node[6], Frame::Bulk(Bytes::from_static(b"endpoint")));
        assert_eq!(node[7], Frame::Bulk(Bytes::from_static(b"127.0.0.1")));
        assert_eq!(node[8], Frame::Bulk(Bytes::from_static(b"role")));
        assert_eq!(node[9], Frame::Bulk(Bytes::from_static(b"master")));
        assert_eq!(
            node[10],
            Frame::Bulk(Bytes::from_static(b"replication-offset"))
        );
        assert_eq!(node[11], Frame::Integer(0));
        assert_eq!(node[12], Frame::Bulk(Bytes::from_static(b"health")));
        assert_eq!(node[13], Frame::Bulk(Bytes::from_static(b"online")));
    }

    #[test]
    fn cluster_shards_is_empty_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"SHARDS"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn an_unknown_cluster_subcommand_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-a"),
                cmd(&[b"CLUSTER", b"RESHARD"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown CLUSTER subcommand 'RESHARD'".into())
        );
    }

    #[test]
    fn cluster_with_no_subcommand_is_an_arity_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-a"),
                cmd(&[b"CLUSTER"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'cluster' command".into())
        );
    }
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster_`
Expected: FAIL — `CLUSTER` currently falls through to `dispatch`'s unknown-command arm, so every assertion gets `Frame::Error("ERR unknown command 'CLUSTER'")`

- [x] **Step 3: Implement the handlers**

```rust
// crates/server/src/dispatcher.rs — add directly below `handle_replicaof` (:984)
/// Splits a config `host:port` into its parts. Falls back to the whole string and port 0 on a
/// malformed address; `ClusterConfig::parse` does not validate the address shape (it is echoed
/// to clients verbatim, so it must not be normalized), and this is the one place that needs the
/// halves separately.
fn split_addr(addr: &str) -> (&str, i64) {
    match addr.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(0)),
        None => (addr, 0),
    }
}

/// `CLUSTER INFO`'s body. `cluster_state` is unconditionally `ok` and every epoch is
/// unconditionally `0` because a static config has no way to know otherwise -- there is no
/// gossip to learn a peer is down, and no epoch bumping without resharding or failover. Pinning
/// the fields we cannot compute to the value that is true by construction beats fabricating one.
fn cluster_info_text(cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>) -> String {
    let (enabled, assigned, count) = match cluster {
        Some(c) => (1, crate::cluster::SLOT_COUNT as u32, c.nodes().len()),
        None => (0, 0, 0),
    };
    format!(
        "cluster_enabled:{enabled}\r\n\
         cluster_state:ok\r\n\
         cluster_slots_assigned:{assigned}\r\n\
         cluster_known_nodes:{count}\r\n\
         cluster_size:{count}\r\n\
         cluster_my_epoch:0\r\n\
         cluster_current_epoch:0\r\n"
    )
}

/// `CLUSTER NODES`'s body, one `\n`-terminated line per node in real Redis's space-separated
/// format (that payload uses `\n`, not `\r\n`, inside the bulk string). The `@<cport>` cluster-bus
/// port is the Redis convention of `port + 10000`; it is **advertised but never bound**, because
/// there is no cluster bus -- the field is not optional in the grammar clients parse, so the
/// conventional value is emitted and the caveat is recorded in the README. `connected` is
/// likewise unconditional: nothing here can observe a peer disconnecting.
fn cluster_nodes_text(cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>) -> String {
    let Some(cluster) = cluster else {
        return String::new();
    };
    let my_id = &cluster.myself().id;
    cluster
        .nodes()
        .iter()
        .map(|n| {
            let (_, port) = split_addr(&n.addr);
            let flags = if &n.id == my_id {
                "myself,master"
            } else {
                "master"
            };
            format!(
                "{} {}@{} {} - 0 0 0 connected {}-{}\n",
                n.id,
                n.addr,
                port + 10000,
                flags,
                n.first_slot,
                n.last_slot
            )
        })
        .collect()
}

/// `CLUSTER SHARDS`'s reply: one entry per configured node, each an `Array` of alternating
/// key/value frames rather than a `Map`, so RESP2 and RESP3 clients see identical output and
/// this helper needs no `Protocol` state. `role` is always `master` and each shard has exactly
/// one node: this sprint's cluster has no shard-level replicas. `replication-offset` is 0
/// because this project has no replication offsets at all (Sprint 5 made every resync a full
/// one), and the field is present only because clients parse for it.
fn cluster_shards_reply(
    cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>,
) -> Frame {
    let Some(cluster) = cluster else {
        return Frame::Array(vec![]);
    };
    Frame::Array(
        cluster
            .nodes()
            .iter()
            .map(|n| {
                let (host, port) = split_addr(&n.addr);
                let node = Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"id")),
                    Frame::Bulk(Bytes::from(n.id.clone())),
                    Frame::Bulk(Bytes::from_static(b"port")),
                    Frame::Integer(port),
                    Frame::Bulk(Bytes::from_static(b"ip")),
                    Frame::Bulk(Bytes::from(host.to_string())),
                    Frame::Bulk(Bytes::from_static(b"endpoint")),
                    Frame::Bulk(Bytes::from(host.to_string())),
                    Frame::Bulk(Bytes::from_static(b"role")),
                    Frame::Bulk(Bytes::from_static(b"master")),
                    Frame::Bulk(Bytes::from_static(b"replication-offset")),
                    Frame::Integer(0),
                    Frame::Bulk(Bytes::from_static(b"health")),
                    Frame::Bulk(Bytes::from_static(b"online")),
                ]);
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"slots")),
                    Frame::Array(vec![
                        Frame::Integer(n.first_slot as i64),
                        Frame::Integer(n.last_slot as i64),
                    ]),
                    Frame::Bulk(Bytes::from_static(b"nodes")),
                    Frame::Array(vec![node]),
                ])
            })
            .collect(),
    )
}

/// Returns `Some(reply)` if `frame` was a `CLUSTER` command -- handled entirely here, never
/// reaching `dispatch` -- or `None` if it was some other command. Same interception shape as
/// `handle_replicaof` above, and for the same reason: this needs `ReplicationHandle`, which
/// plain `dispatch` has no parameter for.
///
/// `CLUSTER KEYSLOT` is answered even when cluster mode is off: it is a pure function of the
/// key, real Redis answers it in non-cluster mode too, and making it conditional would leave
/// this sprint's headline algorithm untestable over the wire on a plain node.
fn handle_cluster(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"CLUSTER") {
        return None;
    }
    let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'cluster' command".into(),
        ));
    };
    let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
    let cluster = replication.cluster();
    Some(match sub.as_str() {
        "KEYSLOT" => match items.get(2) {
            Some(Frame::Bulk(key)) if items.len() == 3 => {
                Frame::Integer(crate::cluster::key_slot(key) as i64)
            }
            _ => Frame::Error("ERR wrong number of arguments for 'cluster|keyslot' command".into()),
        },
        "MYID" => Frame::Bulk(match cluster {
            Some(c) => Bytes::from(c.myself().id.clone()),
            // 40 zeroes: real Redis's "no cluster identity" shape, rather than inventing one.
            None => Bytes::from("0".repeat(40)),
        }),
        "INFO" => Frame::Bulk(Bytes::from(cluster_info_text(cluster))),
        "SHARDS" => cluster_shards_reply(cluster),
        "NODES" => Frame::Bulk(Bytes::from(cluster_nodes_text(cluster))),
        _ => Frame::Error(format!("ERR unknown CLUSTER subcommand '{sub}'")),
    })
}
```

- [x] **Step 4: Wire the interception into `dispatch_and_log`**

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log, directly after the existing
// `if let Some(reply) = handle_replicaof(&frame, replication) { return reply; }` (:1065-1067)
    if let Some(reply) = handle_cluster(&frame, replication) {
        return reply;
    }
```

- [x] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster_ dispatcher::tests::an_unknown_cluster`
Expected: PASS, all 11 tests

- [x] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add CLUSTER KEYSLOT/MYID/INFO/SHARDS/NODES"
```

---

### Task 3: `-MOVED` and `-CROSSSLOT` redirection

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (new `cluster_redirect` below `command_keys`; the gate inserted at the top of `dispatch_and_log`, above the `is_replica` check at `:1054`)

**Interfaces:**
- Consumes: `command_keys` (Task 1), `crate::cluster::key_slot` and `ClusterConfig::owns`/`owner_of` (plan 01).
- Produces: cluster-mode routing enforced for every client command; consumed end-to-end by `03-cluster-integration-tests.md`.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    #[test]
    fn a_key_this_node_owns_is_served_normally() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "hello" hashes to slot 866, which shard-a owns
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"SET", b"hello", b"world"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn a_key_this_node_does_not_own_is_redirected_with_moved() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "foo" hashes to slot 12182, which shard-c owns
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"GET", b"foo"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
    }

    #[test]
    fn a_redirected_write_never_reaches_the_engine() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"SET", b"foo", b"bar"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
        assert_eq!(engine.get(b"foo"), None); // nothing was written
    }

    #[test]
    fn keys_spanning_two_slots_are_rejected_with_crossslot() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "hello" is slot 866, "foo" is slot 12182
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"MSET", b"hello", b"1", b"foo", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("CROSSSLOT Keys in request don't hash to the same slot".into())
        );
        assert_eq!(engine.get(b"hello"), None);
    }

    #[test]
    fn a_hash_tag_keeps_a_multi_key_command_on_one_slot() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // both keys hash on "user1000" => slot 3443, owned by shard-a
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"MSET", b"{user1000}.name", b"ada", b"{user1000}.city", b"london"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert_eq!(
            engine.get(b"{user1000}.city"),
            Some(engine::Value::String(Bytes::from_static(b"london")))
        );
    }

    #[test]
    fn keyless_commands_are_never_redirected() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let handle = cluster_handle("shard-a");
        for (command, expected) in [
            (cmd(&[b"PING"]), Frame::Simple("PONG".into())),
            (cmd(&[b"SELECT", b"0"]), Frame::Simple("OK".into())),
            (
                cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
                Frame::Integer(12182),
            ),
        ] {
            assert_eq!(
                dispatch_and_log(&engine, &aof, &handle, command, &mut Protocol::default(), 1),
                expected
            );
        }
    }

    #[test]
    fn nothing_is_redirected_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"MSET", b"hello", b"1", b"foo", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn moved_takes_precedence_over_readonly_on_a_node_that_is_both() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let handle = cluster_handle("shard-a");
        handle
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // A write to a key this node doesn't own, on a node that is also a read-only follower.
        // MOVED wins: READONLY would send a cluster-aware client into a retry loop against a
        // node that will never accept this key, while MOVED sends it to the owner, where a
        // READONLY (if that node is also a follower) is actionable.
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &handle,
                cmd(&[b"SET", b"foo", b"bar"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("MOVED 12182 127.0.0.1:7003".into())
        );
        // ...and a write to a key it DOES own still gets the READONLY it deserves
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &handle,
                cmd(&[b"SET", b"hello", b"world"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("READONLY You can't write against a read only replica.".into())
        );
    }
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::a_key_this_node dispatcher::tests::keys_spanning dispatcher::tests::moved_takes_precedence dispatcher::tests::a_redirected_write dispatcher::tests::a_hash_tag_keeps`
Expected: FAIL — no redirection exists yet, so `GET foo` returns `Frame::Null`, `MSET` returns `+OK`, and the precedence test gets `READONLY` instead of `MOVED`

- [x] **Step 3: Implement `cluster_redirect`**

```rust
// crates/server/src/dispatcher.rs — add directly below `command_keys`
/// `None` = this node may handle the command. `Some(frame)` = reply with this instead, without
/// touching the engine, the AOF, the replica fan-out, or any lock.
///
/// Called only from `dispatch_and_log`, never from `dispatch`: `aof::replay` and the follower
/// apply loop call `dispatch` directly and must apply every frame they are handed regardless of
/// slot ownership -- redirecting there would silently drop writes during recovery and
/// replication. Keeping the check here makes that impossible by construction.
///
/// When cluster mode is off (the default, and every existing test), this is one `Option` check.
fn cluster_redirect(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let cluster = replication.cluster()?;
    let keys = command_keys(frame);
    let mut slots = keys.into_iter().map(|k| crate::cluster::key_slot(k));
    let first = slots.next()?; // no keys => nothing to route
    if !slots.all(|s| s == first) {
        // Without this, `MSET a 1 b 2` across two slots would be accepted by whichever node owns
        // `a` and would then write `b` onto a node that does not own it -- a silent, permanent
        // violation of the routing invariant, undetectable by any client. Hash tags are how a
        // client legitimately keeps multi-key commands working under this rule.
        return Some(Frame::Error(
            "CROSSSLOT Keys in request don't hash to the same slot".into(),
        ));
    }
    if cluster.owns(first) {
        return None;
    }
    let owner = cluster.owner_of(first);
    Some(Frame::Error(format!("MOVED {first} {}", owner.addr)))
}
```

- [x] **Step 4: Put the gate first in `dispatch_and_log`**

```rust
// crates/server/src/dispatcher.rs — the FIRST statement in dispatch_and_log's body, above the
// existing `if replication.is_replica.load(...)` block (:1054)
    // Checked before everything else, including the -READONLY gate below: a redirect says which
    // node should handle this key at all, and it must land before any lock is taken or any
    // interception runs. See ../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md for the
    // MOVED-beats-READONLY precedence argument.
    if let Some(redirect) = cluster_redirect(&frame, replication) {
        return redirect;
    }
```

- [x] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, every test in the module — including all pre-existing ones, which use `ReplicationHandle::default()` (cluster mode off) and so take the `None` short-circuit

- [x] **Step 6: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 7: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): redirect unowned keys with MOVED and reject CROSSSLOT commands"
```
