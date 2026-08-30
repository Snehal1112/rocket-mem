// crates/server/tests/kill_and_recover.rs
use redis::AsyncCommands;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// Spawns the real compiled binary bound to an OS-assigned port, reading its own stdout to
/// discover which port it actually got. Returns the child (so the caller can kill it) and a
/// `redis://` URL ready to connect to.
fn spawn_server(aof_path: &std::path::Path) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rocket-mem"))
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", aof_path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn the rocket-mem binary");

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    let mut addr = None;
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF — the process exited before printing anything useful
            Ok(_) => {
                if let Some(rest) = line.trim().strip_prefix("Listening on ") {
                    addr = Some(rest.to_string());
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let addr = addr.expect("server never printed its listening address on stdout");
    (child, format!("redis://{addr}"))
}

#[tokio::test]
async fn kill_dash_nine_then_restart_preserves_all_written_keys() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("kill-test.aof");

    let (mut child, url) = spawn_server(&aof_path);
    {
        let client = redis::Client::open(url).unwrap();
        let mut con = client.get_multiplexed_async_connection().await.unwrap();
        for i in 0..200 {
            let _: () = con.set(format!("k{i}"), format!("v{i}")).await.unwrap();
        }
    }

    // main.rs ships FsyncPolicy::EverySecond, and under that policy AofWriter::append only
    // fills a BufWriter — nothing has reached even the OS until the periodic fsync loop's
    // next tick flushes it. SIGKILL a process whose writes are still in its own userspace
    // buffer and they're gone outright, so this wait is load-bearing, not padding: it's the
    // "up to one second of writes may be lost" window that everysec semantics define (real
    // Redis documents the identical tradeoff for `appendfsync everysec`).
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    // a real SIGKILL — std::process::Child::kill() is documented to be exactly this on Unix,
    // not a graceful shutdown request the process could catch and clean up after
    child.kill().expect("failed to SIGKILL the server");
    child.wait().expect("failed to reap the killed process");

    let (mut child2, url2) = spawn_server(&aof_path);
    let client = redis::Client::open(url2).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    for i in 0..200 {
        let v: String = con.get(format!("k{i}")).await.unwrap();
        assert_eq!(v, format!("v{i}"));
    }

    let _ = child2.kill();
    let _ = child2.wait();
}

#[tokio::test]
async fn ttls_set_before_the_kill_come_back_as_absolute_deadlines_not_restarted_countdowns() {
    // The one place 05-aof-dispatch-wiring.md's EXPIRE-family→PEXPIREAT rewrite is provable
    // end-to-end: a *relative* TTL logged verbatim would restart its countdown from replay
    // time, silently extending every key's life by however long the process was down.
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("kill-ttl.aof");

    let (mut child, url) = spawn_server(&aof_path);
    {
        let client = redis::Client::open(url).unwrap();
        let mut con = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = con.set("keeper", "v").await.unwrap();
        let _: () = con.set("doomed", "v").await.unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg("keeper")
            .arg(3600)
            .query_async(&mut con)
            .await
            .unwrap();
        let _: () = redis::cmd("PEXPIRE")
            .arg("doomed")
            .arg(500)
            .query_async(&mut con)
            .await
            .unwrap();
    }
    // same load-bearing EverySecond-fsync wait as the test above; it also carries "doomed"
    // well past its 500ms deadline, so it is already logically gone before the kill
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    child.kill().expect("failed to SIGKILL the server");
    child.wait().expect("failed to reap the killed process");

    let (mut child2, url2) = spawn_server(&aof_path);
    let client = redis::Client::open(url2).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    // the hour-long TTL survived the restart rather than being lost entirely
    let ttl: i64 = redis::cmd("TTL")
        .arg("keeper")
        .query_async(&mut con)
        .await
        .unwrap();
    assert!(
        (3000..=3600).contains(&ttl),
        "unexpected surviving TTL: {ttl}"
    );
    // and the already-elapsed one stays elapsed: replaying an absolute PEXPIREAT whose
    // timestamp is now in the past deletes the key immediately, whereas a relative replay
    // would have resurrected it with a fresh 500ms to live
    let doomed: Option<String> = con.get("doomed").await.unwrap();
    assert_eq!(doomed, None);

    let _ = child2.kill();
    let _ = child2.wait();
}
