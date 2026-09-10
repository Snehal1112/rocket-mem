# Cluster Health In The `CLUSTER` Replies Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make the three `CLUSTER` reply builders report what the prober actually observed instead of three hardcoded lies — `connected`, `health: online`, `cluster_state:ok`/`cluster_slots_pfail:0` — so a node whose leader was killed outright stops being indistinguishable from a healthy one. Routing is deliberately untouched, and so is `cluster_slots_fail`, which is `0` for a structural reason rather than as a placeholder (contract §2.6).

**Architecture:** `cluster_nodes_text`, `cluster_shards_reply`, and `cluster_info_text` in `crates/server/src/dispatcher.rs` grow a second parameter, `health: Option<&Arc<PeerHealth>>`, and share one small `node_is_reachable` helper that special-cases this node's own entry. `cluster_shards_reply` gains a third parameter, this node's real `master_repl_offset`. `handle_cluster` — their only call site — reads both off `ReplicationHandle`. `cluster_slots_reply` is left alone on purpose: `CLUSTER SLOTS`'s grammar has no health or state field, so there is nothing there to make honest.

**Tech Stack:** nothing new. Builds on plan 12 (`PeerHealth`, `ReplicationHandle::peer_health`) and, for the offset field only, on chain A's plan 01 (`ReplicationHandle::master_repl_offset`).

**Spec:** [`../../specs/2026-09-09-sentinel-failover-spec.md`](../../specs/2026-09-09-sentinel-failover-spec.md) — the "Cluster mode compounds this: the topology has zero health-awareness" section, and the "cheapest honest first step" sentence in the "Additionally, for clustered deployments" paragraph.

## Global Constraints

- **Read [`00-design-contract.md`](00-design-contract.md) first, in full.** It is normative; §2.6 fixes every state mapping used below. Where it and this plan disagree, it wins — report the disagreement rather than deviating.
- **Chain D's plan 13 depends on chain A's plan 01.** Contract §2.6 says so outright, and §0's chain table lists the chains in priority order, **not** as independent tracks. Task 2 reports `CLUSTER SHARDS`'s `replication-offset` from `ReplicationHandle::master_repl_offset()` and its test calls `advance_master_repl_offset`, both introduced by [`01-leader-replication-offset.md`](01-leader-replication-offset.md) (contract §2.4). If those methods are not in the tree, Task 2 cannot compile — land plan 01 first. Tasks 1 and 3 have no such dependency and can proceed either way.
- **This plan changes reporting only. It must not change routing.** `cluster_redirect` still redirects to the configured owner of a slot even when this node knows that owner is dead, because choosing a different owner is a topology decision this project has no mechanism to agree on — no cluster bus, `cluster_current_epoch` pinned to `0`, and `cluster.conf` with no field for a replica. Task 3's integration test pins this by asserting a `-MOVED` to the *dead* node's address. Do not "improve" that assertion into a failover.
- **`cluster_slots_fail` is always `0`, and that is not a placeholder** (contract §2.6, "pfail vs fail"). Redis's *fail* means a majority agreed over the cluster bus that a node is down. This project has no cluster bus and no quorum mechanism, so a suspicion here can **never** be promoted to an agreed failure — the field is structurally, permanently zero. `cluster_slots_pfail` carries the suspected node's slot span, `cluster_slots_ok` subtracts that span once, and `cluster_state` is this node's own operational verdict. **Do not inflate `cluster_slots_fail` to match `cluster_state`.** `cluster_state:fail` beside `cluster_slots_fail:0` reads oddly the first time, and the fix for that is the doc comment written in Task 3, never a field that asserts a consensus this system cannot produce. Every field must be individually true; trading a true field for a tidier-looking set is precisely the habit this whole spec exists to end.
- **Wire-compatibility divergence, to document rather than hide:** in real Redis `cluster_state:fail` *also* means the node refuses to serve. Here it is report-only — this node keeps serving its own slots and `cluster_redirect` keeps routing normally. A cluster-aware client that gates on `cluster_state` before sending commands may therefore behave unexpectedly against rocket-mem. Contract §2.6 requires this to be stated in the doc comment, in `docs/config-reference.md`, and in the manual-testing section (the latter two land in plan 14).
- **No health map means "as configured".** With `peer_health() == None` — every standalone deployment, every existing test, any build where the prober is not running — the three builders must produce byte-identical output to what they produce today. That is what keeps this change backward compatible, and several existing tests are exactly that assertion.
- **A node's own entry is always reachable.** It is the one answering the command. `PeerHealth::for_cluster` does not even hold an entry for it; `node_is_reachable` short-circuits on the id as well, so both layers agree.
- **Do not weaken an existing test.** Where an existing assertion still holds, update its *comment* to say why it holds for a new reason. Where one no longer holds, update it to the new correct expectation in the same commit as the change that broke it.
- **The three CI gates must be clean before every commit:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- **Comment style** (project `CLAUDE.md`): short, easy, full sentences ending in a punctuation mark. No emojis.
- `ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns` in `crates/server/tests/kill_and_recover.rs` is a known pre-existing flake (contract §4). Not yours.

---

### Task 1: `CLUSTER NODES` — `connected`/`disconnected` and the `fail?` flag

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Produces: `fn node_is_reachable(node: &ClusterNode, my_id: &str, health: Option<&Arc<PeerHealth>>) -> bool` (private, shared by all three builders); `cluster_nodes_text(cluster, health)`.
- Consumes: `PeerHealth::is_reachable`, `ReplicationHandle::peer_health` (plan 12).
- Test helper produced: `fn cluster_handle_with_dead(node_id: &str, dead: &[&str]) -> ReplicationHandle`, used by all three tasks.

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s `mod tests`, next to the existing `cluster_handle` helper:

```rust
    /// A cluster-mode handle whose peer-health map reports every node named in `dead` as failed.
    /// The stamps are pushed an hour into the past rather than waiting out a real node timeout,
    /// so these tests are instant and can never flake on timing.
    fn cluster_handle_with_dead(node_id: &str, dead: &[&str]) -> ReplicationHandle {
        let config = std::sync::Arc::new(
            crate::cluster::ClusterConfig::parse(
                "shard-a 127.0.0.1:7001 0 5460\n\
                 shard-b 127.0.0.1:7002 5461 10922\n\
                 shard-c 127.0.0.1:7003 10923 16383\n",
                node_id,
            )
            .unwrap(),
        );
        let health = std::sync::Arc::new(crate::cluster_health::PeerHealth::for_cluster(
            &config,
            std::time::Duration::from_secs(15),
        ));
        for id in dead {
            health.set_last_ok_unix(id, crate::replication::unix_now_secs() - 3600);
        }
        ReplicationHandle::default()
            .with_cluster(config)
            .with_peer_health(health)
    }

    #[test]
    fn cluster_nodes_reports_an_unreachable_peer_as_disconnected_and_flagged_fail() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle_with_dead("shard-b", &["shard-a"]),
            cmd(&[b"CLUSTER", b"NODES"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "shard-a 127.0.0.1:7001@17001 master,fail? - 0 0 0 disconnected 0-5460"
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
    fn cluster_nodes_never_reports_this_node_as_disconnected_from_itself() {
        // A node answering this command is trivially alive. `PeerHealth::for_cluster` holds no
        // entry for it at all, and `node_is_reachable` short-circuits on the id too -- this pins
        // both layers, so neither can regress alone.
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle_with_dead("shard-b", &["shard-b"]),
            cmd(&[b"CLUSTER", b"NODES"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "shard-b 127.0.0.1:7002@17002 myself,master - 0 0 0 connected 5461-10922"
        );
    }

    #[test]
    fn cluster_nodes_reports_every_peer_connected_when_the_whole_cluster_answers() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle_with_dead("shard-b", &[]),
            cmd(&[b"CLUSTER", b"NODES"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert_eq!(text.lines().filter(|l| l.contains("fail?")).count(), 0);
        assert_eq!(text.lines().filter(|l| l.contains(" connected ")).count(), 3);
    }
```

Also update the comment on the existing `cluster_nodes_lists_every_node_with_myself_flagged` test, which now pins the no-prober fallback rather than an unconditional literal. Add this line directly above its `let Frame::Bulk(text) = ...`:

```rust
        // `cluster_handle` attaches no peer-health map, so this is the no-prober case: with no
        // liveness information at all, every node is reported exactly as configured. That is the
        // backward-compatibility guarantee, not a leftover hardcoded `connected`.
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster_nodes`
Expected failure: `cluster_nodes_reports_an_unreachable_peer_as_disconnected_and_flagged_fail` fails on
`assertion 'left == right' failed
  left: "shard-a 127.0.0.1:7001@17001 master - 0 0 0 connected 0-5460"
 right: "shard-a 127.0.0.1:7001@17001 master,fail? - 0 0 0 disconnected 0-5460"` —
the builder ignores health entirely today. (If `with_peer_health` is missing, plan 12 has not landed; stop and execute it first.)

- [ ] **Step 3: Make `cluster_nodes_text` health-aware**

In `crates/server/src/dispatcher.rs`, add the shared helper immediately above `cluster_info_text`:

```rust
/// Whether `node` should be reported as up.
///
/// This process's own entry is always reachable -- it is the one answering the command. With no
/// health map (cluster mode off, or a build with no prober running) every node reports reachable,
/// which is exactly what these builders did before the prober existed: absence of information is
/// reported as "the topology as configured", never as "failed".
fn node_is_reachable(
    node: &crate::cluster::ClusterNode,
    my_id: &str,
    health: Option<&std::sync::Arc<crate::cluster_health::PeerHealth>>,
) -> bool {
    if node.id == my_id {
        return true;
    }
    match health {
        Some(health) => health.is_reachable(&node.id),
        None => true,
    }
}
```

Then replace `cluster_nodes_text` in full:

```rust
/// `CLUSTER NODES`'s body, one `\n`-terminated line per node in real Redis's space-separated
/// format (that payload uses `\n`, not `\r\n`, inside the bulk string). The `@<cport>` cluster-bus
/// port is the Redis convention of `port + 10000`; it is **advertised but never bound**, because
/// there is no cluster bus -- the field is not optional in the grammar clients parse, so the
/// conventional value is emitted and the caveat is recorded in the README.
///
/// The link state and the `fail?` flag come from `health`, the map the peer prober maintains: a
/// peer that has not answered a probe within `cluster_node_timeout_secs` is reported
/// `master,fail?` and `disconnected`. `fail?` is Redis's spelling for *pfail* -- one node's own
/// suspicion. It never becomes a plain `fail` here, because promoting a suspicion to an agreed
/// failure needs a quorum over a cluster bus this project does not have.
///
/// Nothing about this changes routing: `cluster_redirect` still sends clients to a dead node's
/// configured address, because picking a different owner is a topology decision nothing here can
/// agree on. This line is how an operator finds out, not a failover.
fn cluster_nodes_text(
    cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>,
    health: Option<&std::sync::Arc<crate::cluster_health::PeerHealth>>,
) -> String {
    let Some(cluster) = cluster else {
        return String::new();
    };
    let my_id = &cluster.myself().id;
    cluster
        .nodes()
        .iter()
        .map(|n| {
            let (_, port) = split_addr(&n.addr);
            let reachable = node_is_reachable(n, my_id, health);
            let flags = if &n.id == my_id {
                "myself,master"
            } else if reachable {
                "master"
            } else {
                "master,fail?"
            };
            let link = if reachable { "connected" } else { "disconnected" };
            format!(
                "{} {}@{} {} - 0 0 0 {link} {}-{}\n",
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
```

And update its call site in `handle_cluster`. Add the health lookup next to the existing cluster lookup:

```rust
    let cluster = replication.cluster();
    let health = replication.peer_health();
```

then change the `NODES` arm to:

```rust
        "NODES" => Frame::Bulk(Bytes::from(cluster_nodes_text(cluster, health))),
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster`
Expected: green, including the untouched `cluster_nodes_lists_every_node_with_myself_flagged` and `cluster_nodes_is_empty_when_cluster_mode_is_off` — both are the no-health-map path, which is unchanged by construction.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/dispatcher.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Report a dead peer as disconnected in CLUSTER NODES`.

---

### Task 2: `CLUSTER SHARDS` — real health and a real `replication-offset`

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Produces: `cluster_shards_reply(cluster, health, my_repl_offset: u64) -> Frame`.
- Consumes: `node_is_reachable` (Task 1), `ReplicationHandle::master_repl_offset` (chain A, plan 01), `ReplicationHandle::advance_master_repl_offset` in the test (same plan).

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s `mod tests`:

```rust
    /// Pulls one alternating key/value pair out of a `CLUSTER SHARDS` entry's single node, so a
    /// test can name the field it cares about instead of counting array indexes.
    fn shard_node_field(shard: &Frame, key: &[u8]) -> Frame {
        let Frame::Array(entry) = shard else {
            panic!("expected a shard Array")
        };
        let Frame::Array(nodes) = &entry[3] else {
            panic!("expected a nodes Array")
        };
        let Frame::Array(node) = &nodes[0] else {
            panic!("expected a node Array")
        };
        node.chunks(2)
            .find(|pair| pair[0] == Frame::Bulk(Bytes::copy_from_slice(key)))
            .map(|pair| pair[1].clone())
            .unwrap_or_else(|| panic!("no field named {}", String::from_utf8_lossy(key)))
    }

    #[test]
    fn cluster_shards_reports_an_unreachable_peer_as_failed() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Array(shards) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle_with_dead("shard-a", &["shard-c"]),
            cmd(&[b"CLUSTER", b"SHARDS"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(shards.len(), 3);
        assert_eq!(
            shard_node_field(&shards[0], b"health"),
            Frame::Bulk(Bytes::from_static(b"online")),
            "this node is answering, so it is online"
        );
        assert_eq!(
            shard_node_field(&shards[1], b"health"),
            Frame::Bulk(Bytes::from_static(b"online"))
        );
        assert_eq!(
            shard_node_field(&shards[2], b"health"),
            Frame::Bulk(Bytes::from_static(b"failed"))
        );
        // A failed master is still a master. `role` reports what the node is configured as, not
        // whether it is answering -- `health` is the field that carries liveness.
        assert_eq!(
            shard_node_field(&shards[2], b"role"),
            Frame::Bulk(Bytes::from_static(b"master"))
        );
    }

    #[test]
    fn cluster_shards_reports_this_nodes_real_replication_offset_and_zero_for_peers() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let handle = cluster_handle_with_dead("shard-a", &[]);
        handle.advance_master_repl_offset(4096);
        let Frame::Array(shards) = dispatch_and_log(
            &engine,
            &aof,
            &handle,
            cmd(&[b"CLUSTER", b"SHARDS"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(
            shard_node_field(&shards[0], b"replication-offset"),
            Frame::Integer(4096),
            "shard-a is myself, and its offset is knowable"
        );
        // A peer's offset stays 0: there is no cluster bus, so this node genuinely does not know
        // it. Reporting its own offset for a peer would be a fabrication.
        assert_eq!(
            shard_node_field(&shards[1], b"replication-offset"),
            Frame::Integer(0)
        );
        assert_eq!(
            shard_node_field(&shards[2], b"replication-offset"),
            Frame::Integer(0)
        );
    }
```

Also update the comment inside the existing `cluster_shards_describes_every_shards_slots_and_its_one_node`, above its `assert_eq!(node[11], Frame::Integer(0));` line, since that `0` now means something specific rather than "always zero":

```rust
        // `cluster_handle` never wrote anything, so this node's own `master_repl_offset` is
        // genuinely 0 -- not the hardcoded literal it used to be.
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster_shards`
Expected failure: `cluster_shards_reports_an_unreachable_peer_as_failed` fails with
`assertion 'left == right' failed
  left: Bulk(b"online")
 right: Bulk(b"failed")`,
and `cluster_shards_reports_this_nodes_real_replication_offset_and_zero_for_peers` fails with `left: Integer(0), right: Integer(4096)`. (If `advance_master_repl_offset` does not resolve, chain A's plan 01 has not landed — execute it first, per this plan's Global Constraints.)

- [ ] **Step 3: Make `cluster_shards_reply` health-aware and offset-aware**

Replace `cluster_shards_reply` in full:

```rust
/// `CLUSTER SHARDS`'s reply: one entry per configured node, each an `Array` of alternating
/// key/value frames rather than a `Map`, so RESP2 and RESP3 clients see identical output and this
/// helper needs no `Protocol` state. Each shard still has exactly one node: `cluster.conf`'s
/// four-field format has no field for a replica, so the topology cannot express one.
///
/// `role` is always `master` -- a node that stopped answering is still configured as a master,
/// and `health` is the field that carries liveness. `health` is `online` for a peer answering
/// probes and `failed` for one that has not answered within `cluster_node_timeout_secs`; with no
/// prober running, every node reports `online`, exactly as before the prober existed.
///
/// `replication-offset` reports this node's real `master_repl_offset` for its own entry, and `0`
/// for every peer. A peer's offset is genuinely unknown here: there is no cluster bus to carry it,
/// and echoing this node's own number under a peer's name would be a fabrication of exactly the
/// kind this reply is being fixed to stop.
fn cluster_shards_reply(
    cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>,
    health: Option<&std::sync::Arc<crate::cluster_health::PeerHealth>>,
    my_repl_offset: u64,
) -> Frame {
    let Some(cluster) = cluster else {
        return Frame::Array(vec![]);
    };
    let my_id = &cluster.myself().id;
    Frame::Array(
        cluster
            .nodes()
            .iter()
            .map(|n| {
                let (host, port) = split_addr(&n.addr);
                let reachable = node_is_reachable(n, my_id, health);
                let offset = if &n.id == my_id {
                    my_repl_offset as i64
                } else {
                    0
                };
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
                    Frame::Integer(offset),
                    Frame::Bulk(Bytes::from_static(b"health")),
                    Frame::Bulk(if reachable {
                        Bytes::from_static(b"online")
                    } else {
                        Bytes::from_static(b"failed")
                    }),
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
```

And update the call site in `handle_cluster`:

```rust
        "SHARDS" => cluster_shards_reply(cluster, health, replication.master_repl_offset()),
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster`
Expected: green, including the pre-existing `cluster_shards_describes_every_shards_slots_and_its_one_node` (no health map, no writes, so `online` and `0` are still correct) and `cluster_shards_is_empty_when_cluster_mode_is_off`.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green.

```bash
git add crates/server/src/dispatcher.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Report real health and offset in CLUSTER SHARDS`.

---

### Task 3: `CLUSTER INFO` — real state and slot counts, end to end over the wire

**Files:**
- Modify: `crates/server/src/dispatcher.rs`
- Modify: `crates/server/tests/cluster.rs`

**Interfaces:**
- Produces: `cluster_info_text(cluster, health) -> String`; the integration helper `spawn_cluster_with_one_dead_node()`.
- Consumes: `node_is_reachable` (Task 1), `cluster_health::spawn_peer_prober` and `ReplicationHandle::with_peer_health` (plan 12).

- [ ] **Step 1: Write the failing tests**

Add to `crates/server/src/dispatcher.rs`'s `mod tests`:

```rust
    #[test]
    fn cluster_info_reports_fail_and_the_dead_nodes_slot_span_when_a_peer_is_unreachable() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            // shard-c owns 10923-16383, which is 5461 slots.
            &cluster_handle_with_dead("shard-a", &["shard-c"]),
            cmd(&[b"CLUSTER", b"INFO"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_state:fail\r\n"), "{text}");
        assert!(text.contains("cluster_slots_assigned:16384\r\n"), "{text}");
        assert!(text.contains("cluster_slots_ok:10923\r\n"), "{text}");
        assert!(text.contains("cluster_slots_pfail:5461\r\n"), "{text}");
        // Structurally zero, not a placeholder: `fail` means a quorum agreed over a cluster bus,
        // and this project has neither. One node's suspicion can never be promoted here, so this
        // counter has no value it could ever honestly take other than 0.
        assert!(text.contains("cluster_slots_fail:0\r\n"), "{text}");
        // Every node is still known and still counted: nothing was removed from the topology.
        assert!(text.contains("cluster_known_nodes:3\r\n"), "{text}");
        assert!(text.contains("cluster_size:3\r\n"), "{text}");
    }

    #[test]
    fn cluster_info_sums_the_slot_spans_of_every_unreachable_node() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            // shard-b owns 5461 slots and shard-c owns 5461, so 10922 are unreachable.
            &cluster_handle_with_dead("shard-a", &["shard-b", "shard-c"]),
            cmd(&[b"CLUSTER", b"INFO"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_slots_pfail:10922\r\n"), "{text}");
        assert!(text.contains("cluster_slots_ok:5462\r\n"), "{text}");
        assert!(
            text.contains("cluster_slots_fail:0\r\n"),
            "however many peers are suspected, none of it is agreed: {text}"
        );
    }

    #[test]
    fn cluster_info_reports_ok_while_every_peer_answers() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle_with_dead("shard-a", &[]),
            cmd(&[b"CLUSTER", b"INFO"]),
            &Session::new(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_state:ok\r\n"), "{text}");
        assert!(text.contains("cluster_slots_ok:16384\r\n"), "{text}");
        assert!(text.contains("cluster_slots_pfail:0\r\n"), "{text}");
        assert!(text.contains("cluster_slots_fail:0\r\n"), "{text}");
    }
```

Update the comment inside the existing `cluster_info_emits_every_field_real_redis_always_includes`, which claims failure detection does not exist. Replace those two comment lines above its `cluster_slots_ok`/`pfail`/`fail` assertions with:

```rust
        // `cluster_handle` attaches no peer-health map, so nothing is even suspected and
        // `cluster_slots_pfail` is honestly zero. `cluster_slots_fail` is zero for a stronger
        // reason -- it always is, because nothing here can agree that a node has failed. See
        // `cluster_info_reports_fail_and_the_dead_nodes_slot_span_when_a_peer_is_unreachable`.
```

Then add the wire-level test to `crates/server/tests/cluster.rs`:

```rust
/// Two live nodes plus one address nothing listens on, each live node running the peer prober.
/// This reproduces the live-verified failure this plan chain exists for: a shard's leader process
/// killed outright, seen from a surviving node.
///
/// The timers are deliberate. `last_ok_unix` has one-second resolution, so a one-second node
/// timeout takes between one and two seconds of real time to trip; the test sleeps past that.
async fn spawn_cluster_with_one_dead_node() -> (Vec<tempfile::TempDir>, Vec<String>, String) {
    let mut listeners = Vec::new();
    let mut addrs = Vec::new();
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addrs.push(listener.local_addr().unwrap().to_string());
        listeners.push(listener);
    }
    // Bound to claim an ephemeral port, then dropped: a connect there is refused immediately, so
    // the prober sees a dead node without this test waiting on a real network timeout.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap().to_string();
    drop(dead);

    let config_text = format!(
        "live-a {} 0 5460\nlive-b {} 5461 10922\ndead-c {} 10923 16383\n",
        addrs[0], addrs[1], dead_addr
    );
    let ids = ["live-a", "live-b"];
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
        let cluster = Arc::new(rocket_mem::cluster::ClusterConfig::parse(&config_text, ids[i]).unwrap());
        let health = rocket_mem::cluster_health::spawn_peer_prober(
            &cluster,
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(1),
        );
        let replication = Arc::new(
            rocket_mem::replication::ReplicationHandle::new(
                Arc::clone(&engine),
                dir.path().join("node.snapshot"),
            )
            .with_cluster(Arc::clone(&cluster))
            .with_peer_health(health),
        );
        tokio::spawn(rocket_mem::serve(listener, engine, aof, replication));
        dirs.push(dir);
    }
    (dirs, addrs, dead_addr)
}

#[tokio::test]
async fn a_dead_node_is_reported_as_failed_by_its_surviving_peers_without_changing_routing() {
    let (_dirs, addrs, dead_addr) = spawn_cluster_with_one_dead_node().await;
    // Past the one-second node timeout, allowing for the one-second stamp resolution.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let mut c = connect(&addrs[0]).await;

    let Frame::Bulk(nodes) = send(&mut c, &[b"CLUSTER", b"NODES"]).await else {
        panic!("expected Bulk")
    };
    let nodes = String::from_utf8(nodes.to_vec()).unwrap();
    let dead_line = nodes
        .lines()
        .find(|l| l.starts_with("dead-c "))
        .unwrap_or_else(|| panic!("{nodes}"));
    assert!(dead_line.contains("master,fail?"), "{nodes}");
    assert!(dead_line.contains(" disconnected "), "{nodes}");
    let live_line = nodes
        .lines()
        .find(|l| l.starts_with("live-b "))
        .unwrap_or_else(|| panic!("{nodes}"));
    assert!(live_line.contains(" connected "), "{nodes}");
    assert!(!live_line.contains("fail?"), "{nodes}");

    let Frame::Bulk(info) = send(&mut c, &[b"CLUSTER", b"INFO"]).await else {
        panic!("expected Bulk")
    };
    let info = String::from_utf8(info.to_vec()).unwrap();
    assert!(info.contains("cluster_state:fail\r\n"), "{info}");
    assert!(info.contains("cluster_slots_pfail:5461\r\n"), "{info}");
    assert!(info.contains("cluster_slots_ok:10923\r\n"), "{info}");
    // `fail` stays 0 beside a `fail` state, on purpose: this node suspects dead-c, and no quorum
    // exists anywhere in this project that could turn that suspicion into an agreed failure.
    assert!(info.contains("cluster_slots_fail:0\r\n"), "{info}");

    // dead-c owns 10923-16383 and sorts last, so it is the third shard entry.
    let Frame::Array(shards) = send(&mut c, &[b"CLUSTER", b"SHARDS"]).await else {
        panic!("expected Array")
    };
    let Frame::Array(entry) = &shards[2] else {
        panic!("expected a shard Array")
    };
    let Frame::Array(shard_nodes) = &entry[3] else {
        panic!("expected a nodes Array")
    };
    let Frame::Array(node) = &shard_nodes[0] else {
        panic!("expected a node Array")
    };
    assert_eq!(node[12], Frame::Bulk(Bytes::from_static(b"health")));
    assert_eq!(node[13], Frame::Bulk(Bytes::from_static(b"failed")));

    // Routing is deliberately unchanged. "foo" is slot 12182, which dead-c owns, and this node
    // still redirects there -- picking a different owner is a topology decision nothing in this
    // project can agree on. Honest reporting is the whole deliverable; failover is not.
    assert_eq!(
        send(&mut c, &[b"GET", b"foo"]).await,
        Frame::Error(format!("MOVED 12182 {dead_addr}"))
    );
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster_info && cargo test -p rocket-mem --test cluster`
Expected failure: `cluster_info_reports_fail_and_the_dead_nodes_slot_span_when_a_peer_is_unreachable` fails on `assert!(text.contains("cluster_state:fail\r\n"))` (the body still carries the literal `cluster_state:ok`), and the integration test fails the same way after its `CLUSTER NODES` assertions pass (Task 1 already landed those).

- [ ] **Step 3: Make `cluster_info_text` health-aware**

Replace `cluster_info_text` in full:

```rust
/// `CLUSTER INFO`'s body.
///
/// `cluster_state`, `cluster_slots_ok`, and `cluster_slots_pfail` are derived from the peer
/// prober's health map: every node this one cannot reach contributes its whole slot span to the
/// pfail count, and any suspected slot makes the state `fail`. With no prober running -- cluster
/// mode off, or a build that never started one -- nothing is suspected, so the counters are zero
/// and the state is `ok`, exactly as before the prober existed.
///
/// **`cluster_slots_fail` is always `0`, and that is a fact about this system rather than a
/// placeholder.** Redis distinguishes *pfail* (`fail?` -- one node suspects a peer) from *fail*
/// (a majority agreed over the cluster bus that it is down). This project has no cluster bus and
/// no quorum mechanism of any kind, so a suspicion can never be promoted to an agreed failure:
/// there is no value this counter could ever honestly take but zero. Do not "fix" it to match
/// `cluster_state` -- a non-zero `cluster_slots_fail` would assert a consensus that does not
/// exist, which is the same class of confident falsehood this whole reply was rewritten to stop.
/// `cluster_slots_ok` subtracts the pfail span once, and nothing subtracts twice.
///
/// `cluster_state:fail` therefore sits beside `cluster_slots_fail:0`. That reads oddly the first
/// time and is nonetheless the truthful pair: this node's own verdict is that it cannot reach the
/// owner of some slots (`state`), while nothing anywhere has agreed that that owner is dead
/// (`slots_fail`).
///
/// `cluster_state:fail` here is also a **report, not a mode**: unlike real Redis, this node keeps
/// serving its own slots and `cluster_redirect` keeps routing normally. Nothing is promoted and no
/// topology is rewritten. This is a deliberate wire-compatibility divergence -- a cluster-aware
/// client that gates on `cluster_state` before sending commands may behave unexpectedly here --
/// and it is documented in `docs/config-reference.md` and `.claude/manual-testing.md` too.
///
/// The epochs stay `0` and both `stats_messages_*` counters stay `0` honestly: there is no
/// resharding, no failover, and no cluster bus, so no epoch was ever bumped and no gossip message
/// was ever sent. `cluster_enabled` is a deliberate extra; real Redis reports it in `INFO`'s
/// Cluster section rather than here, but clients read it from both and an additional key breaks
/// no parser.
fn cluster_info_text(
    cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>,
    health: Option<&std::sync::Arc<crate::cluster_health::PeerHealth>>,
) -> String {
    let (enabled, assigned, count, pfail) = match cluster {
        Some(c) => {
            let my_id = &c.myself().id;
            let pfail: u32 = c
                .nodes()
                .iter()
                .filter(|n| !node_is_reachable(n, my_id, health))
                .map(|n| n.last_slot as u32 - n.first_slot as u32 + 1)
                .sum();
            (1, crate::cluster::SLOT_COUNT as u32, c.nodes().len(), pfail)
        }
        None => (0, 0, 0, 0),
    };
    let state = if pfail == 0 { "ok" } else { "fail" };
    let slots_ok = assigned - pfail;
    format!(
        "cluster_enabled:{enabled}\r\n\
         cluster_state:{state}\r\n\
         cluster_slots_assigned:{assigned}\r\n\
         cluster_slots_ok:{slots_ok}\r\n\
         cluster_slots_pfail:{pfail}\r\n\
         cluster_slots_fail:0\r\n\
         cluster_known_nodes:{count}\r\n\
         cluster_size:{count}\r\n\
         cluster_my_epoch:0\r\n\
         cluster_current_epoch:0\r\n\
         cluster_stats_messages_sent:0\r\n\
         cluster_stats_messages_received:0\r\n\
         total_cluster_links_buffer_limit_exceeded:0\r\n"
    )
}
```

And update its call site in `handle_cluster`:

```rust
        "INFO" => Frame::Bulk(Bytes::from(cluster_info_text(cluster, health))),
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p rocket-mem dispatcher::tests::cluster && cargo test -p rocket-mem --test cluster`
Expected: green. `cluster_info_reports_zero_slots_ok_when_no_config_was_loaded` and `cluster_info_reports_disabled_when_no_config_was_loaded` still pass: the `None` arm produces the same zeros as before.

- [ ] **Step 5: Gate check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. If any other test in the workspace pinned the old literals, update it to the new correct expectation now, in this commit — do not weaken an assertion to make it pass.

```bash
git add crates/server/src/dispatcher.rs crates/server/tests/cluster.rs
```
Commit through the `1-git-commit` skill. Suggested subject: `Report real cluster state and slot counts in CLUSTER INFO`.

---

## Next plan

[`14-cluster-health-observability.md`](14-cluster-health-observability.md) — the two peer gauges, one log line per peer state transition, and the documentation for both new config fields.
