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
