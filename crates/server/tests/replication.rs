use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

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
