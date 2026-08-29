# Kill-and-Recover Test Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** prove — against the real, compiled `rocket-mem` binary, killed with a real `SIGKILL` — that data written before the kill is still there after a restart. This is the sprint's headline deliverable: "data survives a `kill -9` and restart."

**Architecture:** a new integration test file, `crates/server/tests/kill_and_recover.rs`, spawns the actual compiled binary via `std::process::Command` (located via Cargo's `CARGO_BIN_EXE_rocket-mem` compile-time environment variable — no manual `cargo build` shell-out needed), pointed at a temp-directory AOF path and an OS-assigned port (parsed from the binary's own startup log line). `Child::kill()` on Unix is already a real `SIGKILL`, not a graceful shutdown request.

**Tech Stack:** `std::process::Command` (std), `redis` (existing dev-dependency, already used by `tests/integration.rs`).

**Spec:** `../../specs/2026-08-30-sprint-4-spec.md` — "the one deliberate exception to in-process-only testing" is authoritative for why this plan spawns a real subprocess where every other integration test in this project doesn't.

**Depends on:** `03-expire-family-and-set-ttl-dispatcher.md`, `05-aof-dispatch-wiring.md`, and `06-aof-replay-and-corrupt-recovery.md` — the real binary must already replay on startup and log writes for this to have anything to prove.

---

### Task 1: spawn/readiness helper and the kill-and-restart test

**Files:**
- Create: `crates/server/tests/kill_and_recover.rs`

**Interfaces:**
- Consumes: the compiled `rocket-mem` binary (via `env!("CARGO_BIN_EXE_rocket-mem")`), `ROCKET_MEM_ADDR`/`ROCKET_MEM_AOF_PATH` (from `05-aof-dispatch-wiring.md`'s `main.rs`, superseded by `06-aof-replay-and-corrupt-recovery.md`'s Task 2 version which also replays on startup), the `EXPIRE`/`PEXPIRE`/`TTL` dispatcher arms (from `03-expire-family-and-set-ttl-dispatcher.md`), and `dispatch_and_log`'s `EXPIRE`-family→`PEXPIREAT` rewrite (from `05`).
- Note on `FsyncPolicy`: `main.rs` hardcodes `EverySecond` (it isn't env-configurable), so both tests below must wait out that window before killing — see the inline comments. Don't remove those waits; without them nothing has left the writer's `BufWriter` and a `SIGKILL` loses every write.
- Produces: nothing consumed by other plans — this is a leaf, CI-facing test file.

- [ ] **Step 1: Write the failing test**

```rust
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
    assert!((3000..=3600).contains(&ttl), "unexpected surviving TTL: {ttl}");
    // and the already-elapsed one stays elapsed: replaying an absolute PEXPIREAT whose
    // timestamp is now in the past deletes the key immediately, whereas a relative replay
    // would have resurrected it with a fresh 500ms to live
    let doomed: Option<String> = con.get("doomed").await.unwrap();
    assert_eq!(doomed, None);

    let _ = child2.kill();
    let _ = child2.wait();
}
```

- [ ] **Step 2: Run the tests**

This plan adds no new production code — it's a pure integration proof of what
`03-expire-family-and-set-ttl-dispatcher.md`, `05-aof-dispatch-wiring.md`, and
`06-aof-replay-and-corrupt-recovery.md` already built, so there's no red-then-green cycle
here, only a single confirming run.

Run: `cargo test -p rocket-mem --test kill_and_recover`
Expected: PASS, both tests (each takes ~2-3s of real time — the fsync waits plus two process
spawns — so don't mistake the pause for a hang). If either fails, one of `03`/`05`/`06` isn't fully wired into the
real binary — stop and check those plans before continuing, per
`superpowers:executing-plans`'s "stop when blocked" rule, rather than patching around it here.

- [ ] **Step 3: Verify the workspace-wide checks pass**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check`
Expected: clean

- [ ] **Step 4: Commit**

Invoke the `1-git-commit` skill (`Skill` tool, name `1-git-commit`) to stage and commit
`crates/server/tests/kill_and_recover.rs` — do not compose the commit message freeform.
Suggested subject: `test(server): add real SIGKILL-and-restart durability proof`.
