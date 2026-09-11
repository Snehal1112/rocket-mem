use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

/// Spawns the real compiled binary, reading its own stdout for the `RESP <addr>` banner row to
/// discover which port it got -- same technique as `tests/kill_and_recover.rs::spawn_server`.
/// Unlike `spawn_node_with_min_replicas`, this goes through `main.rs` end to end: env vars,
/// `Config`, and the real `ReplicationHandle::with_min_replicas` call site, not a handle this
/// test file built by hand. That is the one path none of this plan's other tests exercise, and
/// exactly the path `main.rs`'s wiring could silently drop the fields on.
fn spawn_real_binary_with_min_replicas(to_write: u64, max_lag_secs: u64) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_rocket-mem"))
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env(
            "ROCKET_MEM_AOF_PATH",
            tempfile::tempdir().unwrap().keep().join("node.aof"),
        )
        .env("ROCKET_MEM_MIN_REPLICAS_TO_WRITE", to_write.to_string())
        .env(
            "ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS",
            max_lag_secs.to_string(),
        )
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn the rocket-mem binary")
}

/// Kills the wrapped child on drop, including on an unwind from a failed assertion -- the plain
/// `child.kill()` at a test's tail never runs if `expect_err`/`assert_eq!` panics first, which
/// otherwise leaves the spawned server alive with its stderr inherited from this test process.
/// That keeps the underlying pipe open past the test's own exit, which is what made this
/// exact regression hang the whole `cargo test` invocation instead of just failing it.
struct KillOnDrop(std::process::Child);

impl std::ops::Deref for KillOnDrop {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.0
    }
}

impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.0
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_resp_addr_from_banner(child: &mut std::process::Child) -> String {
    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().trim_matches(|c| c == '│' || c == ' ');
                let mut parts = trimmed.split_whitespace();
                if parts.next() == Some("RESP") {
                    if let Some(addr) = parts.next() {
                        return addr.to_string();
                    }
                }
            }
            Err(_) => break,
        }
    }
    panic!("server never printed its listening address on stdout");
}

#[tokio::test]
async fn the_real_binary_wires_min_replicas_config_onto_the_replication_handle() {
    let mut child = KillOnDrop(spawn_real_binary_with_min_replicas(1, 10));
    let addr = read_resp_addr_from_banner(&mut child);

    let client = redis::Client::open(format!("redis://{addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let result: Result<(), redis::RedisError> = con.set("k", "v").await;
    assert_eq!(
        result
            .expect_err(
                "ROCKET_MEM_MIN_REPLICAS_TO_WRITE=1 must reach the real ReplicationHandle \
                 through main.rs and refuse the write -- if this passes, main.rs built the \
                 handle without calling with_min_replicas"
            )
            .code(),
        Some("NOREPLICAS")
    );
}

/// Spawns one fully independent node — its own `Engine`, `AofWriter`, `ReplicationHandle`,
/// and TCP listener — and returns everything a test needs to drive it or inspect its state.
/// The `TempDir` must be kept alive by the caller for as long as the node runs (it owns the
/// node's AOF/snapshot files on disk).
async fn spawn_node() -> (
    tempfile::TempDir,
    Arc<engine::Engine>,
    Arc<rocket_mem::aof::AofWriter>,
    Arc<rocket_mem::replication::ReplicationHandle>,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&engine),
        dir.path().join("node.snapshot"),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
        Arc::from("test-node"),
    ));
    (dir, engine, aof, replication, addr.to_string())
}

/// Polls `engine` for `key == value` until it matches or 2 seconds pass, whichever comes
/// first — the "bounded time window" shape this plan's Global Constraints require, instead of
/// a fixed sleep plus a single assertion.
async fn wait_for(engine: &engine::Engine, key: &[u8], value: &[u8]) {
    let expected = Some(engine::Value::String(bytes::Bytes::copy_from_slice(value)));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if engine.get(key) == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never saw {:?}={:?} within the deadline",
            String::from_utf8_lossy(key),
            String::from_utf8_lossy(value)
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Like `spawn_node`, but applies `with_min_replicas` to the `ReplicationHandle` before serving
/// -- `spawn_node` itself has no fencing knobs, since every other test in this file needs fencing
/// off. A separate helper, not a parameter added to `spawn_node`, so every existing `spawn_node()`
/// call site in this file stays untouched.
async fn spawn_node_with_min_replicas(
    to_write: u64,
    max_lag: std::time::Duration,
) -> (
    tempfile::TempDir,
    Arc<engine::Engine>,
    Arc<rocket_mem::aof::AofWriter>,
    Arc<rocket_mem::replication::ReplicationHandle>,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("node.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&engine),
            dir.path().join("node.snapshot"),
        )
        .with_min_replicas(to_write, max_lag),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        listener,
        Arc::clone(&engine),
        Arc::clone(&aof),
        Arc::clone(&replication),
        Arc::from("test-node"),
    ));
    (dir, engine, aof, replication, addr.to_string())
}

#[tokio::test]
async fn a_leader_with_fencing_enabled_and_no_acked_replicas_refuses_writes_with_noreplicas() {
    let (_dir, _engine, _aof, _replication, addr) =
        spawn_node_with_min_replicas(1, std::time::Duration::from_secs(10)).await;

    let client = redis::Client::open(format!("redis://{addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let result: Result<(), redis::RedisError> = con.set("k", "v").await;
    assert_eq!(
        result
            .expect_err("must refuse the write with no replicas connected")
            .code(),
        Some("NOREPLICAS")
    );
}

#[tokio::test]
async fn a_leader_with_fencing_disabled_accepts_writes_with_zero_replicas_connected() {
    let (_dir, engine, _aof, _replication, addr) =
        spawn_node_with_min_replicas(0, std::time::Duration::from_secs(10)).await;

    let client = redis::Client::open(format!("redis://{addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();
    wait_for(&engine, b"k", b"v").await;
}

/// Sends one `INFO replication` over raw RESP and returns the bulk body. Raw rather than the
/// `redis` crate on purpose: the exact `slaveN:` field spelling is what these tests assert, and
/// the `redis` crate would parse that spelling away. `tests/cluster.rs` uses the same shape for
/// the same reason.
async fn info_replication(addr: &str) -> String {
    let mut framed = tokio_util::codec::Framed::new(
        tokio::net::TcpStream::connect(addr).await.unwrap(),
        protocol::codec::RespCodec::default(),
    );
    framed
        .send(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"INFO")),
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"replication")),
        ]))
        .await
        .unwrap();
    match framed.next().await.unwrap().unwrap() {
        protocol::Frame::Bulk(body) => String::from_utf8_lossy(&body).into_owned(),
        other => panic!("INFO replied with {other:?}"),
    }
}

/// Returns the whole `slave0:` line from an `INFO replication` body, or `None` when the leader
/// has no replica registered yet. Kept separate from the parser below so a failing test can
/// print the line verbatim.
fn slave0_line(info: &str) -> Option<&str> {
    info.lines().find(|l| l.starts_with("slave0:"))
}

/// Pulls `offset` and `lag` out of an `INFO replication` body's `slave0:` line. `None` when
/// there is no such line yet (no replica has attached), or when either field is missing or
/// unparseable -- which is a failure worth surfacing at the call site rather than defaulting
/// away to a number that would quietly satisfy an assertion.
fn slave0_offset_and_lag(info: &str) -> Option<(u64, i64)> {
    let line = slave0_line(info)?;
    let mut offset = None;
    let mut lag = None;
    for field in line.trim_start_matches("slave0:").split(',') {
        match field.split_once('=') {
            Some(("offset", v)) => offset = v.parse().ok(),
            Some(("lag", v)) => lag = v.parse().ok(),
            _ => {}
        }
    }
    Some((offset?, lag?))
}

#[tokio::test]
async fn snapshot_plus_tail_recovery_reconstructs_identical_state_to_full_aof_replay() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("bench.aof");

    let aof =
        rocket_mem::aof::AofWriter::open(&aof_path, rocket_mem::aof::FsyncPolicy::Never).unwrap();
    for i in 0..5000 {
        aof.append(protocol::Frame::Array(vec![
            protocol::Frame::Bulk(bytes::Bytes::from_static(b"SET")),
            protocol::Frame::Bulk(bytes::Bytes::from(format!("k{i}"))),
            protocol::Frame::Bulk(bytes::Bytes::from(format!("v{i}"))),
        ]))
        .unwrap();
    }
    aof.fsync().unwrap();

    let full_replay_start = Instant::now();
    let full_replay_engine =
        rocket_mem::aof::recover(&aof_path, &dir.path().join("missing.snapshot")).unwrap();
    let full_replay_elapsed = full_replay_start.elapsed();

    // Snapshot the fully-replayed state at the AOF's current (full) length, so the "tail" the
    // hybrid path replays afterward is empty -- isolating "load a snapshot" against "replay
    // 5000 commands," which is exactly what this benchmark is meant to compare.
    let snapshot_path = dir.path().join("bench.snapshot");
    let offset = aof.current_offset().unwrap();
    std::fs::write(&snapshot_path, full_replay_engine.snapshot(offset)).unwrap();

    let hybrid_start = Instant::now();
    let hybrid_engine = rocket_mem::aof::recover(&aof_path, &snapshot_path).unwrap();
    let hybrid_elapsed = hybrid_start.elapsed();

    println!(
        "recovery benchmark (5000 keys): full AOF replay {full_replay_elapsed:?}, snapshot+tail {hybrid_elapsed:?}"
    );

    for i in 0..5000 {
        let key = format!("k{i}");
        assert_eq!(
            full_replay_engine.get(key.as_bytes()),
            hybrid_engine.get(key.as_bytes()),
            "mismatch at {key}"
        );
    }
}

#[tokio::test]
async fn one_leader_two_followers_propagates_writes_within_a_bounded_time_window() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f1_dir, f1_engine, _f1_aof, f1_replication, _f1_addr) = spawn_node().await;
    let (_f2_dir, f2_engine, _f2_aof, f2_replication, _f2_addr) = spawn_node().await;

    f1_replication.start_replicating(leader_addr.clone());
    f2_replication.start_replicating(leader_addr.clone());
    // give both followers a moment to connect and receive their initial (empty) snapshot
    // before the write below -- not load-bearing for correctness (wait_for's deadline would
    // still catch the write eventually), just avoids a needless first slow poll cycle
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();

    wait_for(&f1_engine, b"k", b"v").await;
    wait_for(&f2_engine, b"k", b"v").await;
}

/// The payoff of the offset chain: a leader and a caught-up follower report the *same* number.
/// Both halves are exercised -- a write taken before the follower attached (carried across in
/// the snapshot header) and one taken after (counted in the apply loop) -- because either half
/// alone would let the two sides agree by accident.
#[tokio::test]
async fn a_followers_replication_offset_converges_on_its_leaders() {
    let (_leader_dir, _leader_engine, _leader_aof, leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // Write *before* the follower attaches, so the leader's offset is already non-zero when the
    // snapshot header carries it across.
    let _: () = con.set("before", "1").await.unwrap();
    assert!(
        leader_replication.master_repl_offset() > 0,
        "the pre-attach write should have advanced the leader's offset"
    );

    f_replication.start_replicating(leader_addr.clone());
    wait_for(&f_engine, b"before", b"1").await;

    // And one *after*, so the apply loop's per-frame advance is exercised too.
    let _: () = con.set("after", "2").await.unwrap();
    wait_for(&f_engine, b"after", b"2").await;

    // A bounded poll rather than a bare assertion: the apply loop stores the new offset just
    // after the dispatch that `wait_for` observes, so the two can race by a few instructions.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let leader = leader_replication.master_repl_offset();
        let follower = f_replication.slave_repl_offset();
        if leader == follower {
            assert!(
                follower > 0,
                "both offsets converged on zero, which proves nothing"
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "follower offset {follower} never caught up to leader offset {leader}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Reproduces the real-world scenario `REPLICAOF ... AUTH` exists to fix: before it, a follower
/// could never sync from a leader with ACL users configured at all -- an unauthenticated PSYNC
/// is rejected with NOAUTH, and (before a separate earlier fix) that rejection actually crashed
/// the follower outright. This proves both the follower actually links up against the
/// ACL-protected leader, and that writes still replicate afterward.
#[tokio::test]
async fn a_follower_syncs_from_an_acl_protected_leader_when_replicaof_auth_is_used() {
    let (_leader_dir, _leader_engine, _leader_aof, leader_replication, leader_addr) =
        spawn_node().await;
    leader_replication
        .acl
        .set_user(
            "app",
            &[
                bytes::Bytes::from_static(b"on"),
                bytes::Bytes::from_static(b">changeme"),
                bytes::Bytes::from_static(b"allcommands"),
                bytes::Bytes::from_static(b"allkeys"),
            ],
        )
        .unwrap();

    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;
    f_replication.start_replicating_with_auth(
        leader_addr.clone(),
        Some(("app".to_string(), "changeme".to_string())),
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "follower never linked up against the ACL-protected leader"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let client = redis::Client::open(format!("redis://app:changeme@{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();

    wait_for(&f_engine, b"k", b"v").await;
}

#[tokio::test]
async fn a_node_configured_with_replicaof_auto_connects_on_startup() {
    let (_leader_dir, _leader_engine, _leader_aof, leader_replication, leader_addr) =
        spawn_node().await;
    leader_replication
        .acl
        .set_user(
            "app",
            &[
                bytes::Bytes::from_static(b"on"),
                bytes::Bytes::from_static(b">changeme"),
                bytes::Bytes::from_static(b"allcommands"),
                bytes::Bytes::from_static(b"allkeys"),
            ],
        )
        .unwrap();

    // spawn_node's node is otherwise indistinguishable from one main.rs would start -- there's
    // no need to hand-build Engine/AofWriter/ReplicationHandle/listener here, because the
    // config-driven auto-connect below is fire-and-forget and has no dependency on when (or
    // whether) any listener has bound yet, exactly like main.rs's own startup wiring, which
    // fires it before binding any of its own listeners.
    let (_f_dir, f_engine, _f_aof, f_replication, f_addr) = spawn_node().await;

    // A real `Config` carrying `replicaof`/auth fields, exactly as a TOML file or `--replicaof`
    // CLI flag would produce -- then driven through `start_replicating_from_config`, the same
    // shared helper main.rs's startup wiring calls. This is what actually proves the Config ->
    // auth-tuple mapping and the auto-connect wiring itself, rather than re-testing
    // `start_replicating_with_auth` directly (already covered by the ACL-auth test above).
    let config = rocket_mem::config::Config {
        replicaof: Some(leader_addr.clone()),
        replicaof_auth_username: Some("app".to_string()),
        replicaof_auth_password: Some("changeme".to_string()),
        ..rocket_mem::config::Config::default()
    };
    f_replication.start_replicating_from_config(&config);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "config-driven follower never linked up against the leader"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let client = redis::Client::open(format!("redis://app:changeme@{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("k", "v").await.unwrap();

    wait_for(&f_engine, b"k", b"v").await;

    // Prove the follower actually came up as read-only via the config-driven path too, not
    // just linked -- same assertion shape as a_follower_rejects_client_writes_over_a_real_...
    let f_client = redis::Client::open(format!("redis://{f_addr}")).unwrap();
    let mut f_con = f_client.get_multiplexed_async_connection().await.unwrap();
    let result: Result<(), redis::RedisError> = f_con.set("nope", "x").await;
    assert_eq!(
        result.expect_err("must be read-only").code(),
        Some("READONLY")
    );
}

// multi_thread, not the default current_thread flavor: serve_replica's snapshot-walk and
// registry-register have no `.await` between them, so on a single-threaded runtime they'd be
// atomic with respect to every other task on that same thread regardless of whether the lock
// is actually held -- the race this test exists to catch only shows up under genuine
// cross-thread parallelism.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn psync_snapshot_and_register_stay_atomic_under_concurrent_writes() {
    // Proves serve_replica's snapshot+register critical section (connection.rs) is
    // load-bearing, not just present. Taken apart, a write landing between the snapshot walk
    // and registration reaches neither the blob nor the stream -- lost permanently, since a
    // reconnect just re-snapshots a leader that has already moved past it. RPUSH (not SET) is
    // used so a duplicated delivery (the opposite failure mode -- registering before
    // snapshotting) is also observable: replaying it twice would show up as an extra element,
    // where re-applying an idempotent SET would not.
    const WRITERS: usize = 8;
    const PUSHES_PER_WRITER: usize = 250;
    const PUSHES: usize = WRITERS * PUSHES_PER_WRITER;

    let (_leader_dir, leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    // Several concurrent writer connections, not one sequential one: this keeps the leader's
    // ordering lock almost continuously held by *some* write for the whole burst, which is
    // what makes it likely that PSYNC's connect below actually lands its snapshot-walk and
    // register mid-write rather than in an idle gap between writes.
    let mut writers = Vec::with_capacity(WRITERS);
    for w in 0..WRITERS {
        let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
        let mut con = client.get_multiplexed_async_connection().await.unwrap();
        writers.push(tokio::spawn(async move {
            for i in 0..PUSHES_PER_WRITER {
                let _: () = con.rpush("list", format!("w{w}-{i}")).await.unwrap();
            }
        }));
    }

    // Connect PSYNC (via a real follower node) while the writers above are still hammering
    // RPUSH -- this is what actually exercises the snapshot+register critical section under
    // concurrency, instead of only ever running safely before or after the whole burst.
    f_replication.start_replicating(leader_addr.clone());

    for writer in writers {
        writer.await.unwrap();
    }

    // Bounded poll until the follower's list converges to the leader's (or the deadline
    // expires) -- a lost or duplicated write from a broken lock would show up as the two
    // never converging to the same Vec.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let leader_list = leader_engine.get(b"list");
        let follower_list = f_engine.get(b"list");
        if follower_list == leader_list {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "follower list never converged to the leader's: leader={leader_list:?} follower={follower_list:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Final sanity: exactly PUSHES elements, so a lost or duplicated write can't hide behind
    // a coincidentally-equal-length-but-wrong-content match.
    match leader_engine.get(b"list") {
        Some(engine::Value::List(l)) => assert_eq!(l.len(), PUSHES),
        other => panic!("expected a List with {PUSHES} elements, got {other:?}"),
    }
}

#[tokio::test]
async fn a_follower_reconnects_and_resyncs_after_its_connection_drops() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("before-kill", "1").await.unwrap();
    wait_for(&f_engine, b"before-kill", b"1").await;

    // "Kill" the follower's connection: per the sprint-5 spec, aborting
    // replication_client_loop and re-issuing REPLICAOF is the in-process-test-shape
    // equivalent of a dropped connection or a leader-side restart, since there's no
    // subprocess here to actually sever a socket against. start_replicating itself does the
    // abort-old/spawn-new sequence -- calling it again is the "kill and reconnect."
    //
    // Remove the pre-kill key from the follower's own state first: otherwise wait_for below
    // would be satisfied by leftover state from before the "kill" on its very first poll,
    // proving nothing about whether a fresh full resync actually happened.
    f_engine.del(b"before-kill");
    f_replication.start_replicating(leader_addr.clone());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let _: () = con.set("after-reconnect", "2").await.unwrap();
    // the fresh resync's snapshot must have included the pre-kill write...
    wait_for(&f_engine, b"before-kill", b"1").await;
    // ...and the post-reconnect stream must still be live
    wait_for(&f_engine, b"after-reconnect", b"2").await;
}

#[tokio::test]
async fn a_follower_rejects_client_writes_over_a_real_connection_and_keeps_its_aof_quiescent() {
    let (_leader_dir, _leader_engine, leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (f_dir, f_engine, f_aof, f_replication, f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let f_aof_path = f_dir.path().join("node.aof");
    // fsync first -- FsyncPolicy::Never buffers writes, so metadata().len() alone would read
    // 0 regardless of what's been logged, proving nothing.
    f_aof.fsync().unwrap();
    leader_aof.fsync().unwrap();
    let f_aof_len_before_write = std::fs::metadata(&f_aof_path).unwrap().len();
    let leader_aof_path = _leader_dir.path().join("node.aof");
    let leader_aof_len_before_write = std::fs::metadata(&leader_aof_path).unwrap().len();

    // A real client hitting a real follower over TCP must be rejected with READONLY --
    // covered elsewhere only by a dispatch_and_log unit test, not end-to-end.
    let follower_client = redis::Client::open(format!("redis://{f_addr}")).unwrap();
    let mut follower_con = follower_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let result: Result<(), redis::RedisError> = follower_con.set("k", "v").await;
    let err = result.expect_err("a write against a read-only replica must be rejected");
    assert_eq!(err.code(), Some("READONLY"));

    // Reads against the follower still work.
    let leader_client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut leader_con = leader_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: () = leader_con.set("k", "v").await.unwrap();
    wait_for(&f_engine, b"k", b"v").await;
    let got: String = follower_con.get("k").await.unwrap();
    assert_eq!(got, "v");

    // Positive control: the leader's own AOF DID grow from that write -- if it hadn't, the
    // follower assertion below would be trivially true for the wrong reason.
    leader_aof.fsync().unwrap();
    let leader_aof_len_after_write = std::fs::metadata(&leader_aof_path).unwrap().len();
    assert!(leader_aof_len_after_write > leader_aof_len_before_write);

    // The follower's own AOF stays quiescent: replicated writes are applied via the
    // non-logging dispatch(), never dispatch_and_log(), so its AOF file's length is
    // unchanged by the leader write that just propagated above.
    f_aof.fsync().unwrap();
    let f_aof_len_after_write = std::fs::metadata(&f_aof_path).unwrap().len();
    assert_eq!(f_aof_len_after_write, f_aof_len_before_write);
}

/// Chaining (leader -> follower -> sub-replica) is not supported: a follower's own
/// replication-apply loop applies frames via plain `dispatch`, never `dispatch_and_log`, so it
/// never calls `ReplicaRegistry::broadcast` -- a sub-replica PSYNCing off a follower would get a
/// one-time snapshot and then silently never see another write. Rather than allow that trap,
/// PSYNC against a node that is itself currently a replica must be refused outright.
#[tokio::test]
async fn a_node_that_is_itself_a_replica_refuses_an_incoming_psync() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, _f_engine, _f_aof, f_replication, f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "follower never linked up against the leader"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // A third node attempting to chain off the follower, as if it were a leader.
    let stream = tokio::net::TcpStream::connect(&f_addr).await.unwrap();
    let mut framed = tokio_util::codec::Framed::new(stream, protocol::codec::RespCodec::default());
    framed
        .send(protocol::Frame::Array(vec![protocol::Frame::Bulk(
            bytes::Bytes::from_static(b"PSYNC"),
        )]))
        .await
        .unwrap();

    let reply = framed.next().await.unwrap().unwrap();
    assert_eq!(
        reply,
        protocol::Frame::Error(
            "ERR PSYNC refused: this node is itself a replica; chaining is not supported".into()
        )
    );
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Real-socket proof that a follower can resync from a leader over TLS, pinned to the leader's
/// own certificate: without this, `sync_once`'s `TcpStream::connect` is always plaintext, so a
/// leader/follower pair replicating across an untrusted network exposes every key/value it
/// carries -- including anything sensitive a consumer might cache there -- in cleartext,
/// regardless of any TLS configured for ordinary client connections. The leader here only
/// accepts TLS (`serve_tls`, no plaintext listener at all), so a successful resync is only
/// possible if the follower actually spoke TLS on the replication connection.
#[tokio::test]
async fn a_follower_resyncs_over_tls_when_pinned_to_the_leaders_certificate() {
    let leader_dir = tempfile::tempdir().unwrap();
    let leader_engine = Arc::new(engine::Engine::new());
    // Present before replication starts, so it can only reach the follower via the snapshot
    // blob read over the TLS-wrapped socket in `sync_once` -- not the plaintext streamed-frame
    // path, which this test never exercises.
    leader_engine.set(
        bytes::Bytes::from_static(b"pre-existing"),
        engine::Value::String(bytes::Bytes::from_static(b"secret")),
    );
    let leader_aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &leader_dir.path().join("leader.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let leader_replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&leader_engine),
        leader_dir.path().join("leader.snapshot"),
    ));
    let leader_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let leader_addr = leader_listener.local_addr().unwrap();
    let server_tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
            .unwrap();
    tokio::spawn(rocket_mem::serve_tls(
        leader_listener,
        server_tls_config,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
        Arc::from("test-node"),
    ));

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    let client_tls_config = rocket_mem::tls::load_client_config(&fixture("test-cert.pem")).unwrap();
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_replication_tls_client_config(client_tls_config),
    );

    follower_replication.start_replicating(leader_addr.to_string());
    wait_for(&follower_engine, b"pre-existing", b"secret").await;
}

/// A follower pinned to the wrong certificate must never resync -- proving `load_client_config`
/// actually validates the leader's presented certificate against the pinned one, rather than
/// accepting any certificate (which would make the previous test's success meaningless) or
/// silently falling back to plaintext when the handshake fails.
#[tokio::test]
async fn a_follower_never_resyncs_when_pinned_to_the_wrong_certificate() {
    let leader_dir = tempfile::tempdir().unwrap();
    let leader_engine = Arc::new(engine::Engine::new());
    leader_engine.set(
        bytes::Bytes::from_static(b"pre-existing"),
        engine::Value::String(bytes::Bytes::from_static(b"secret")),
    );
    let leader_aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &leader_dir.path().join("leader.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let leader_replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&leader_engine),
        leader_dir.path().join("leader.snapshot"),
    ));
    let leader_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let leader_addr = leader_listener.local_addr().unwrap();
    // The leader really does present `test-cert.pem` -- same as the positive test.
    let server_tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
            .unwrap();
    tokio::spawn(rocket_mem::serve_tls(
        leader_listener,
        server_tls_config,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
        Arc::from("test-node"),
    ));

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    // Pinned to a different certificate than the one the leader actually presents.
    let client_tls_config =
        rocket_mem::tls::load_client_config(&fixture("wrong-cert.pem")).unwrap();
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_replication_tls_client_config(client_tls_config),
    );

    follower_replication.start_replicating(leader_addr.to_string());
    // No bounded-wait helper for "never happens" -- a fixed window past the point the positive
    // test already resyncs within is the standard way to assert a negative here.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(follower_engine.get(b"pre-existing"), None);
    assert!(!follower_replication.link_up());
}

/// The TLS test above proves a follower can read the *snapshot blob* over TLS. It says so in its
/// own comment, and it stops there. Everything the bidirectional restructure of `serve_replica`
/// touched is on the other side of that handshake: the split socket, and the `select!` loop that
/// writes streamed frames while a read is parked on the same TLS stream. This test drives a write
/// *after* the follower has synced and requires it to arrive, which is the only way that
/// interleaving gets exercised over `TlsStream` rather than only over a plain `TcpStream`.
///
/// The leader binds two listeners over one shared engine/AOF/handle: a TLS one the follower
/// PSYNCs to, and a plaintext one the ordinary client below writes through. Both feed the same
/// `ReplicaRegistry`, so the write fans out down the TLS replica connection.
#[tokio::test]
async fn a_tls_follower_keeps_receiving_streamed_writes_after_its_resync() {
    let leader_dir = tempfile::tempdir().unwrap();
    let leader_engine = Arc::new(engine::Engine::new());
    let leader_aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &leader_dir.path().join("leader.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    let leader_replication = Arc::new(rocket_mem::replication::ReplicationHandle::new(
        Arc::clone(&leader_engine),
        leader_dir.path().join("leader.snapshot"),
    ));

    let tls_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tls_addr = tls_listener.local_addr().unwrap();
    let server_tls_config =
        rocket_mem::tls::load_server_config(&fixture("test-cert.pem"), &fixture("test-key.pem"))
            .unwrap();
    tokio::spawn(rocket_mem::serve_tls(
        tls_listener,
        server_tls_config,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
        Arc::from("test-node"),
    ));

    let plain_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plain_addr = plain_listener.local_addr().unwrap();
    tokio::spawn(rocket_mem::serve(
        plain_listener,
        Arc::clone(&leader_engine),
        Arc::clone(&leader_aof),
        Arc::clone(&leader_replication),
        Arc::from("test-node"),
    ));

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    let client_tls_config = rocket_mem::tls::load_client_config(&fixture("test-cert.pem")).unwrap();
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_replication_tls_client_config(client_tls_config),
    );
    follower_replication.start_replicating(tls_addr.to_string());

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !follower_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the follower never linked up over TLS"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Written only after the link is up, so it can only reach the follower through the streamed
    // path on the split TLS socket -- never through the snapshot blob.
    let client = redis::Client::open(format!("redis://{plain_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = con.set("streamed-over-tls", "yes").await.unwrap();

    wait_for(&follower_engine, b"streamed-over-tls", b"yes").await;
}

/// Polls a leader's own `INFO replication` until its `slave0:` line reports an offset `accept`
/// is happy with, then returns that line verbatim alongside the parsed offset and lag. A
/// bounded poll with an explicit deadline, never a bare sleep: the ack cadence is one second,
/// so five seconds is generous headroom even for a *second* ack on a loaded box.
async fn wait_for_slave0_offset(
    leader_addr: &str,
    accept: impl Fn(u64) -> bool,
) -> (String, u64, i64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let info = info_replication(leader_addr).await;
        if let Some((offset, lag)) = slave0_offset_and_lag(&info) {
            if accept(offset) {
                return (slave0_line(&info).unwrap().to_string(), offset, lag);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leader's slave0 offset never reached what this test requires:\n{info}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The whole chain, end to end, over real sockets. A write on the leader advances its
/// `master_repl_offset`, is streamed to the follower, is applied there and advances the
/// follower's `slave_repl_offset`, is acked back up the same connection, is recorded on the
/// leader's registry entry, and finally shows up in the leader's own `INFO REPLICATION` as a
/// non-zero `offset` with a small `lag`.
///
/// Before this chain, that line read `state=online` with nothing behind it: the leader could not
/// answer "how caught up is this replica" at all, which is exactly why the spec refused to build
/// failover on top of it.
///
/// Two writes, not one, and the second offset must be strictly greater than the first. A single
/// snapshot of `INFO` cannot tell an advancing offset from one stuck at whatever value it
/// happened to reach at sync time, and this chain has already shipped one assertion that passed
/// vacuously. `lag` is likewise required to be a real small number: `-1` is the "never acked"
/// sentinel, so an assertion that tolerated it would pass against a follower that never acks at
/// all -- precisely the state this chain exists to escape.
#[tokio::test]
async fn a_leader_reports_its_followers_advancing_offset_and_small_lag_in_info() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;
    let (_f_dir, f_engine, _f_aof, f_replication, _f_addr) = spawn_node().await;

    f_replication.start_replicating(leader_addr.clone());
    let link_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !f_replication.link_up() {
        assert!(
            tokio::time::Instant::now() < link_deadline,
            "the follower never linked up"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // First write: this is what takes the reported offset off zero at all. The follower attached
    // to an empty leader, so its snapshot offset was 0 and only a streamed, applied, acked frame
    // can move it.
    let _: () = con.set("first", "1").await.unwrap();
    wait_for(&f_engine, b"first", b"1").await;
    let (first_line, first_offset, first_lag) =
        wait_for_slave0_offset(&leader_addr, |offset| offset > 0).await;
    assert!(
        (0..=2).contains(&first_lag),
        "a replica acking every second must report a lag of about 0s, and never the -1 \
         never-acked sentinel: {first_line}"
    );

    // Second write: the reported offset must actually move. `first_offset` is captured before
    // this write is sent, so nothing but a fresh ack can satisfy the predicate below.
    let _: () = con.set("second", "2").await.unwrap();
    wait_for(&f_engine, b"second", b"2").await;
    let (second_line, second_offset, second_lag) =
        wait_for_slave0_offset(&leader_addr, |offset| offset > first_offset).await;
    println!("leader rendered: {second_line}");

    assert!(
        (0..=2).contains(&second_lag),
        "a replica acking every second must report a lag of about 0s, and never the -1 \
         never-acked sentinel: {second_line}"
    );
    // The follower's own view must agree with what the leader is reporting about it. Both sides
    // count the same encoded bytes, so a mismatch means the re-encoding invariant is broken.
    assert_eq!(
        f_replication.slave_repl_offset(),
        second_offset,
        "the leader's recorded ack must match the follower's own position: {second_line}"
    );
}

/// The whole path the announce-address spec cares about, end to end: a `Config` carrying
/// `replica_announce_addr` -> `config::announce_addr` -> `with_own_addr` -> the follower's
/// `PSYNC <addr>` frame -> the leader's `ReplicaRegistry` -> the leader's `INFO REPLICATION`
/// `slaveN:` line. Driving the composed expression `main.rs` uses, rather than re-testing
/// `announce_addr` (unit-tested in config.rs) or `sync_once`'s outgoing PSYNC frame (pinned in
/// replication.rs) in isolation -- neither of those would catch `main.rs` passing `config.addr`.
#[tokio::test]
async fn a_configured_replica_announce_addr_is_what_the_leader_reports_in_info_replication() {
    let (_leader_dir, _leader_engine, _leader_aof, _leader_replication, leader_addr) =
        spawn_node().await;

    let follower_dir = tempfile::tempdir().unwrap();
    let follower_engine = Arc::new(engine::Engine::new());
    let config = rocket_mem::config::Config {
        // Deliberately three different values. `addr` is what a pre-this-feature node would have
        // announced; `replica_announce_addr` is what it must announce now; neither is the
        // ephemeral source port of the connection the leader actually sees. Nothing binds either
        // one -- the announced address is informational, so an unbound value is a legitimate
        // configuration and makes the assertion below unambiguous.
        addr: "127.0.0.1:6479".to_string(),
        replicaof: Some(leader_addr.clone()),
        replica_announce_addr: Some("announced.example:16479".to_string()),
        ..rocket_mem::config::Config::default()
    };
    let follower_replication = Arc::new(
        rocket_mem::replication::ReplicationHandle::new(
            Arc::clone(&follower_engine),
            follower_dir.path().join("follower.snapshot"),
        )
        .with_own_addr(rocket_mem::config::announce_addr(&config)),
    );
    follower_replication.start_replicating_from_config(&config);

    let client = redis::Client::open(format!("redis://{leader_addr}")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // A bounded poll, not a fixed sleep: registration happens when the leader handles the PSYNC,
    // which is a scheduling race with this connection. `let info = loop { ... break info; }`
    // rather than a `let mut` seeded with an empty String, which would trip rustc's
    // `unused_assignments` lint and so fail the `-D warnings` gate.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let info = loop {
        let info: String = redis::cmd("INFO")
            .arg("replication")
            .query_async(&mut con)
            .await
            .unwrap();
        if info.contains("slave0:ip=announced.example,port=16479,state=online") {
            break info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leader never reported the follower's announced address, last INFO was:\n{info}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(
        !info.contains("port=6479"),
        "the bound `addr` must not be what gets announced once the field is set, got:\n{info}"
    );
}
