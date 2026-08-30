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
        aof,
        Arc::clone(&replication),
    ));
    (dir, engine, replication, addr.to_string())
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
    let (_leader_dir, _leader_engine, _leader_replication, leader_addr) = spawn_node().await;
    let (_f1_dir, f1_engine, f1_replication, _f1_addr) = spawn_node().await;
    let (_f2_dir, f2_engine, f2_replication, _f2_addr) = spawn_node().await;

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

#[tokio::test]
async fn a_follower_reconnects_and_resyncs_after_its_connection_drops() {
    let (_leader_dir, _leader_engine, _leader_replication, leader_addr) = spawn_node().await;
    let (_f_dir, f_engine, f_replication, _f_addr) = spawn_node().await;

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
    f_replication.start_replicating(leader_addr.clone());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let _: () = con.set("after-reconnect", "2").await.unwrap();
    // the fresh resync's snapshot must have included the pre-kill write...
    wait_for(&f_engine, b"before-kill", b"1").await;
    // ...and the post-reconnect stream must still be live
    wait_for(&f_engine, b"after-reconnect", b"2").await;
}
