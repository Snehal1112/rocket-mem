# 3-Shard Cluster Integration Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove over real TCP that a 3-shard cluster routes keys deterministically by hash slot and that a client hitting the wrong shard is redirected to — and served by — the right one. This is Sprint 6's first DoD item.

**Architecture:** `crates/server/tests/cluster.rs` follows the in-process shape `tests/replication.rs` already established: three fully independent nodes (own `Engine`, own `AofWriter` over its own `tempfile::tempdir()`, own `ReplicationHandle`) spawned via `serve()` on `127.0.0.1:0` listeners in one process. Because the config file must name the ports the OS actually handed out, the harness binds all three listeners *first*, builds the config text from the real addresses, and feeds it through the same `ClusterConfig::parse` `main.rs` uses.

**Tech Stack:** `tokio`, `tokio_util::codec::Framed`, `protocol::codec::RespCodec`, `futures_util`, `tempfile` — all already dependencies or dev-dependencies of `crates/server`. Raw RESP frames rather than the `redis` crate: these tests assert the *exact* `-MOVED` slot and address, which a client library would abstract away.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "cluster tests run in-process, three nodes, following `-MOVED` explicitly" is authoritative for this plan. Depends on `01-hash-slots-and-cluster-config.md` and `02-cluster-commands-and-moved.md`.

## Global Constraints

- Each node is a genuinely independent triple (`Engine` + `AofWriter` + `ReplicationHandle`). Sharing any of the three between "nodes" would make a test pass for the wrong reason — a single shared `Engine` would serve every key from everywhere.
- The harness binds listeners before writing the config, because slot ownership must be expressed in terms of the ephemeral ports the OS assigned.
- The DoD's "cluster-aware client" is simulated explicitly: read the `-MOVED` reply, parse the address out of it, reconnect there, and re-send. That asserts more than handing the job to a third-party cluster client would, and it avoids depending on `CLUSTER SLOTS`, which this sprint does not implement.
- Slot values used below are fixed by the algorithm and verified in `01-hash-slots-and-cluster-config.md`: `hello` → 866 (shard-a), `counter` → 6680 (shard-b), `foo` → 12182 (shard-c), `{user1000}.*` → 3443 (shard-a).

---

### Task 1: the 3-node harness

**Files:**
- Create: `crates/server/tests/cluster.rs`

**Interfaces:**
- Consumes: `rocket_mem::serve`, `rocket_mem::cluster::ClusterConfig::parse`, `rocket_mem::replication::ReplicationHandle::{new, with_cluster}`, `rocket_mem::aof::{AofWriter, FsyncPolicy}`.
- Produces: `spawn_3_shard_cluster() -> Cluster`, `Cluster::addr(usize)`, `connect(&str)`, `send(&mut Framed<..>, &[&[u8]]) -> Frame`, consumed by Tasks 2 and 3.

- [x] **Step 1: Write the harness and its first, smallest test**

```rust
// crates/server/tests/cluster.rs
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

const NODE_IDS: [&str; 3] = ["shard-a", "shard-b", "shard-c"];
const SLOT_RANGES: [(u16, u16); 3] = [(0, 5460), (5461, 10922), (10923, 16383)];

/// Three independent `rocket-mem` nodes sharing one static topology. The `TempDir`s must stay
/// alive for as long as the nodes run -- they own each node's AOF and snapshot paths.
struct Cluster {
    _dirs: Vec<tempfile::TempDir>,
    addrs: Vec<String>,
}

impl Cluster {
    fn addr(&self, index: usize) -> &str {
        &self.addrs[index]
    }

    /// The index of the node that owns `slot`, per `SLOT_RANGES`.
    fn owner_index(&self, slot: u16) -> usize {
        SLOT_RANGES
            .iter()
            .position(|(first, last)| *first <= slot && slot <= *last)
            .expect("SLOT_RANGES covers the whole slot space")
    }

    /// Any node that does *not* own `slot` -- what a mis-routed client would hit.
    fn non_owner_index(&self, slot: u16) -> usize {
        (self.owner_index(slot) + 1) % 3
    }
}

/// Binds all three listeners first, so the config file can name the ephemeral ports the OS
/// actually assigned, then starts one node per listener with that same config text and its own
/// node id. Each node gets its own Engine/AofWriter/ReplicationHandle -- sharing any of them
/// would let a test pass for the wrong reason.
async fn spawn_3_shard_cluster() -> Cluster {
    let mut listeners = Vec::new();
    let mut addrs = Vec::new();
    for _ in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addrs.push(listener.local_addr().unwrap().to_string());
        listeners.push(listener);
    }

    let config_text: String = (0..3)
        .map(|i| {
            format!(
                "{} {} {} {}\n",
                NODE_IDS[i], addrs[i], SLOT_RANGES[i].0, SLOT_RANGES[i].1
            )
        })
        .collect();

    let mut dirs = Vec::new();
    for (i, listener) in listeners.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(engine::Engine::new());
        let aof = Arc::new(
            rocket_mem::aof::AofWriter::open(
                &dir.path().join("node.aof"),
                rocket_mem::aof::FsyncPolicy::Never,
            )
            .unwrap(),
        );
        let config =
            rocket_mem::cluster::ClusterConfig::parse(&config_text, NODE_IDS[i]).unwrap();
        let replication = Arc::new(
            rocket_mem::replication::ReplicationHandle::new(
                Arc::clone(&engine),
                dir.path().join("node.snapshot"),
            )
            .with_cluster(Arc::new(config)),
        );
        tokio::spawn(rocket_mem::serve(listener, engine, aof, replication));
        dirs.push(dir);
    }

    Cluster { _dirs: dirs, addrs }
}

async fn connect(addr: &str) -> Framed<TcpStream, RespCodec> {
    Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        RespCodec::default(),
    )
}

/// Sends one command and returns its reply frame. Raw RESP rather than the `redis` crate so
/// these tests can assert the exact `-MOVED` slot and address text.
async fn send(framed: &mut Framed<TcpStream, RespCodec>, parts: &[&[u8]]) -> Frame {
    let frame = Frame::Array(
        parts
            .iter()
            .map(|p| Frame::Bulk(Bytes::copy_from_slice(p)))
            .collect(),
    );
    framed.send(frame).await.unwrap();
    framed.next().await.unwrap().unwrap()
}

/// Pulls the `host:port` out of a `-MOVED <slot> <addr>` error, the way a cluster-aware client
/// does before reconnecting.
fn moved_target(reply: &Frame) -> String {
    let Frame::Error(msg) = reply else {
        panic!("expected a MOVED error, got {reply:?}");
    };
    let mut parts = msg.split_whitespace();
    assert_eq!(parts.next(), Some("MOVED"), "not a MOVED error: {msg}");
    let _slot = parts.next().expect("MOVED reply has a slot");
    parts.next().expect("MOVED reply has an address").to_string()
}

#[tokio::test]
async fn every_node_reports_the_same_three_shard_topology() {
    let cluster = spawn_3_shard_cluster().await;
    for i in 0..3 {
        let mut c = connect(cluster.addr(i)).await;
        let Frame::Bulk(info) = send(&mut c, &[b"CLUSTER", b"INFO"]).await else {
            panic!("expected Bulk")
        };
        let info = String::from_utf8(info.to_vec()).unwrap();
        assert!(info.contains("cluster_enabled:1\r\n"), "{info}");
        assert!(info.contains("cluster_known_nodes:3\r\n"), "{info}");

        assert_eq!(
            send(&mut c, &[b"CLUSTER", b"MYID"]).await,
            Frame::Bulk(Bytes::from(NODE_IDS[i]))
        );

        let Frame::Array(shards) = send(&mut c, &[b"CLUSTER", b"SHARDS"]).await else {
            panic!("expected Array")
        };
        assert_eq!(shards.len(), 3);

        let Frame::Bulk(nodes) = send(&mut c, &[b"CLUSTER", b"NODES"]).await else {
            panic!("expected Bulk")
        };
        let nodes = String::from_utf8(nodes.to_vec()).unwrap();
        assert_eq!(nodes.lines().count(), 3, "{nodes}");
        // exactly one line -- this node's own -- carries the `myself` flag
        assert_eq!(
            nodes.lines().filter(|l| l.contains("myself,master")).count(),
            1,
            "{nodes}"
        );
        assert!(
            nodes.lines().nth(i).unwrap().contains("myself,master"),
            "node {i} should flag itself: {nodes}"
        );
    }
}
```

- [x] **Step 2: Run the test to verify it passes**

Run: `cargo test -p rocket-mem --test cluster`
Expected: PASS — the harness and `CLUSTER` support both already exist from plans 01 and 02, so this first test is a wiring check for the harness itself, not a red-then-green step. If it fails, the failure is in the harness (address formatting, config text, node ids), not in the server.

- [x] **Step 3: Commit**

```bash
git add crates/server/tests/cluster.rs
git commit -m "test(server): add an in-process 3-shard cluster harness"
```

---

### Task 2: the DoD routing-and-redirect test

**Files:**
- Modify: `crates/server/tests/cluster.rs`

**Interfaces:**
- Consumes: `spawn_3_shard_cluster`, `connect`, `send`, `moved_target` (Task 1).
- Produces: the sprint's first DoD item, evidenced.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/tests/cluster.rs — append
#[tokio::test]
async fn keys_route_to_the_shard_that_owns_their_slot() {
    let cluster = spawn_3_shard_cluster().await;
    // (key, slot, owning node index) -- slots are fixed by CRC16(key) % 16384
    for (key, slot, owner) in [
        (&b"hello"[..], 866u16, 0usize),
        (&b"counter"[..], 6680, 1),
        (&b"foo"[..], 12182, 2),
    ] {
        assert_eq!(cluster.owner_index(slot), owner);
        let mut c = connect(cluster.addr(owner)).await;
        assert_eq!(
            send(&mut c, &[b"CLUSTER", b"KEYSLOT", key]).await,
            Frame::Integer(slot as i64)
        );
        assert_eq!(
            send(&mut c, &[b"SET", key, b"value"]).await,
            Frame::Simple("OK".into())
        );
        assert_eq!(
            send(&mut c, &[b"GET", key]).await,
            Frame::Bulk(Bytes::from_static(b"value"))
        );
    }
}

#[tokio::test]
async fn a_client_hitting_the_wrong_shard_receives_moved_and_the_right_shard_serves_the_key() {
    let cluster = spawn_3_shard_cluster().await;
    let slot = 12182; // "foo"
    let wrong = cluster.non_owner_index(slot);
    let right = cluster.owner_index(slot);

    let mut wrong_conn = connect(cluster.addr(wrong)).await;
    let reply = send(&mut wrong_conn, &[b"SET", b"foo", b"bar"]).await;
    assert_eq!(
        reply,
        Frame::Error(format!("MOVED {slot} {}", cluster.addr(right)))
    );

    // ...now do what a cluster-aware client does: follow the redirect.
    let target = moved_target(&reply);
    assert_eq!(target, cluster.addr(right));
    let mut right_conn = connect(&target).await;
    assert_eq!(
        send(&mut right_conn, &[b"SET", b"foo", b"bar"]).await,
        Frame::Simple("OK".into())
    );
    assert_eq!(
        send(&mut right_conn, &[b"GET", b"foo"]).await,
        Frame::Bulk(Bytes::from_static(b"bar"))
    );

    // and the wrong shard still refuses to read it, rather than answering a stale nil
    assert_eq!(
        send(&mut wrong_conn, &[b"GET", b"foo"]).await,
        Frame::Error(format!("MOVED {slot} {}", cluster.addr(right)))
    );
}

#[tokio::test]
async fn a_redirected_write_leaves_no_trace_on_the_shard_that_refused_it() {
    let cluster = spawn_3_shard_cluster().await;
    let wrong = cluster.non_owner_index(12182);
    let mut wrong_conn = connect(cluster.addr(wrong)).await;

    let reply = send(&mut wrong_conn, &[b"SET", b"foo", b"bar"]).await;
    assert!(matches!(reply, Frame::Error(ref m) if m.starts_with("MOVED ")));

    // KEYS takes no key, so it is never redirected -- it is the honest way to ask a node what
    // it actually stored. A redirected write must never have reached this node's engine.
    assert_eq!(
        send(&mut wrong_conn, &[b"KEYS", b"*"]).await,
        Frame::Array(vec![])
    );
}

#[tokio::test]
async fn both_non_owning_shards_redirect_to_the_same_owner() {
    let cluster = spawn_3_shard_cluster().await;
    let right = cluster.owner_index(12182);
    for i in 0..3 {
        if i == right {
            continue;
        }
        let mut c = connect(cluster.addr(i)).await;
        assert_eq!(
            send(&mut c, &[b"GET", b"foo"]).await,
            Frame::Error(format!("MOVED 12182 {}", cluster.addr(right)))
        );
    }
}
```

- [x] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --test cluster`
Expected: PASS, all 5 tests. If `a_client_hitting_the_wrong_shard...` fails with an unexpected `+OK` from the wrong shard, the `cluster_redirect` gate from `02-cluster-commands-and-moved.md` is not wired into `dispatch_and_log`; if it fails with the wrong address, the harness config text and the `SLOT_RANGES`/`NODE_IDS` ordering disagree.

- [x] **Step 3: Commit**

```bash
git add crates/server/tests/cluster.rs
git commit -m "test(server): prove 3-shard routing and MOVED redirection over TCP"
```

---

### Task 3: `CROSSSLOT` and hash-tag coverage over TCP

**Files:**
- Modify: `crates/server/tests/cluster.rs`

**Interfaces:**
- Consumes: the harness from Task 1.
- Produces: nothing consumed later; this closes the routing behavior's test surface.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/tests/cluster.rs — append
#[tokio::test]
async fn a_multi_key_command_spanning_slots_is_rejected_with_crossslot() {
    let cluster = spawn_3_shard_cluster().await;
    // "hello" is slot 866 (shard-a), "foo" is slot 12182 (shard-c)
    let mut c = connect(cluster.addr(0)).await;
    assert_eq!(
        send(&mut c, &[b"MSET", b"hello", b"1", b"foo", b"2"]).await,
        Frame::Error("CROSSSLOT Keys in request don't hash to the same slot".into())
    );
    // neither key was written anywhere
    assert_eq!(send(&mut c, &[b"KEYS", b"*"]).await, Frame::Array(vec![]));
    let mut owner_of_foo = connect(cluster.addr(2)).await;
    assert_eq!(
        send(&mut owner_of_foo, &[b"KEYS", b"*"]).await,
        Frame::Array(vec![])
    );
}

#[tokio::test]
async fn hash_tagged_keys_share_one_shard_and_work_with_multi_key_commands() {
    let cluster = spawn_3_shard_cluster().await;
    let slot = 3443; // CRC16("user1000") % 16384
    let owner = cluster.owner_index(slot);
    assert_eq!(owner, 0);

    let mut c = connect(cluster.addr(owner)).await;
    assert_eq!(
        send(&mut c, &[b"CLUSTER", b"KEYSLOT", b"{user1000}.name"]).await,
        Frame::Integer(slot as i64)
    );
    assert_eq!(
        send(&mut c, &[b"CLUSTER", b"KEYSLOT", b"{user1000}.city"]).await,
        Frame::Integer(slot as i64)
    );
    assert_eq!(
        send(
            &mut c,
            &[
                b"MSET",
                b"{user1000}.name",
                b"ada",
                b"{user1000}.city",
                b"london"
            ]
        )
        .await,
        Frame::Simple("OK".into())
    );
    assert_eq!(
        send(&mut c, &[b"MGET", b"{user1000}.name", b"{user1000}.city"]).await,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"ada")),
            Frame::Bulk(Bytes::from_static(b"london")),
        ])
    );

    // the same tagged keys are redirected identically from any other shard
    let mut other = connect(cluster.addr(1)).await;
    assert_eq!(
        send(&mut other, &[b"GET", b"{user1000}.name"]).await,
        Frame::Error(format!("MOVED {slot} {}", cluster.addr(owner)))
    );
}

#[tokio::test]
async fn keyless_commands_work_on_every_shard_without_redirection() {
    let cluster = spawn_3_shard_cluster().await;
    for i in 0..3 {
        let mut c = connect(cluster.addr(i)).await;
        assert_eq!(
            send(&mut c, &[b"PING"]).await,
            Frame::Simple("PONG".into())
        );
        // CLUSTER KEYSLOT answers for any key, on any node, owned or not
        assert_eq!(
            send(&mut c, &[b"CLUSTER", b"KEYSLOT", b"foo"]).await,
            Frame::Integer(12182)
        );
    }
}
```

- [x] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem --test cluster`
Expected: PASS, all 8 tests

- [x] **Step 3: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 4: Commit**

```bash
git add crates/server/tests/cluster.rs
git commit -m "test(server): cover CROSSSLOT rejection and hash-tag routing"
```
