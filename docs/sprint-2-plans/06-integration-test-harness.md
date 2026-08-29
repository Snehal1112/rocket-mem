# Integration Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** wire every remaining Sprint 1 command through the dispatcher (closing the "only 8 commands wired" gap left by `03-command-dispatcher.md`), add arg-count validation so malformed commands return a RESP error instead of panicking, and prove the whole stack end-to-end with `redis-rs` driving a real socket against an in-process server.

**Architecture:** `dispatch()`'s `match` grows to cover the full Sprint 1 command surface. A new `crates/server/tests/integration.rs` file (Rust's standard integration-test location — a separate compilation unit from `src/`, only able to see `rocket_mem`'s public API) drives the server exactly the way a real client would.

**Tech Stack:** `redis` (the `redis-rs` crate) as a dev-dependency only, per `00-sprint-2-spec.md`.

**Spec:** `00-sprint-2-spec.md` — the in-process (not subprocess) integration-test approach is authoritative.

**Depends on:** `04-tcp-listener.md` and `05-stub-commands.md` must both be complete.

---

### Task 1: Arg-count validation

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

This closes the gap flagged in `03-command-dispatcher.md`: every arm that indexes into `rest` must first confirm `rest` is long enough, or a short/malformed command panics the connection task instead of returning a RESP error.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/server/src/dispatcher.rs tests module
#[test]
fn set_with_too_few_args_returns_resp_error_not_a_panic() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"SET", b"onlykey"])),
        Frame::Error("ERR wrong number of arguments for 'set' command".into())
    );
}

#[test]
fn hset_with_too_few_args_returns_resp_error_not_a_panic() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"HSET", b"h", b"field"])),
        Frame::Error("ERR wrong number of arguments for 'hset' command".into())
    );
}

#[test]
fn echo_with_no_args_returns_resp_error_not_a_panic() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"ECHO"])),
        Frame::Error("ERR wrong number of arguments for 'echo' command".into())
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL (or panic) — no arg-count checks exist yet; a panicking test counts as a failure here, same as an assertion failure

- [ ] **Step 3: Add a `require_args!` helper and apply it to every arm that indexes `rest`**

```rust
// crates/server/src/dispatcher.rs — add above dispatch()
macro_rules! require_args {
    ($rest:expr, $n:expr, $name:expr) => {
        if $rest.len() < $n {
            return Frame::Error(format!("ERR wrong number of arguments for '{}' command", $name));
        }
    };
}
```

Add one `require_args!(rest, N, "cmdname")` line at the top of every existing arm's body before it indexes `rest`: `GET`→1, `SET`→2, `APPEND`→2, `STRLEN`→1, `INCR`→1, `DECR`→1, `HSET`→3, `HGET`→2, `ECHO`→1. `PING` doesn't need one — `rest.first()` already handles zero args without indexing. `SELECT`/`COMMAND`/`INFO` don't need one — none of them read `rest`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 3 new ones

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "fix(server): validate arg count before indexing, closing the panic gap from 03"
```

---

### Task 2: Wire the remaining Sprint 1 commands

**Files:**
- Modify: `crates/server/src/dispatcher.rs`

**Interfaces:**
- Consumes: `engine::commands::{string::{set_nx, set_xx, incr_by}, hash::{hdel, hgetall, hexists, hlen}, list::{rpush, lpush, rpop, lpop, llen, lrange}, set::{sadd, srem, smembers, sismember, scard}}` — every Sprint 1 command function not already wired in `03-command-dispatcher.md`.

- [ ] **Step 1: Write the failing tests**

```rust
// add to crates/server/src/dispatcher.rs tests module
#[test]
fn set_nx_returns_null_when_key_already_exists() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"k", b"old"]));
    assert_eq!(dispatch(&engine, cmd(&[b"SET", b"k", b"new", b"NX"])), Frame::Null);
    assert_eq!(dispatch(&engine, cmd(&[b"GET", b"k"])), Frame::Bulk(Bytes::from_static(b"old")));
}

#[test]
fn set_with_ex_flag_returns_a_clear_not_implemented_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"SET", b"k", b"v", b"EX", b"10"])),
        Frame::Error("ERR syntax error: EX/PX are not supported yet (planned Sprint 4)".into())
    );
}

#[test]
fn incrby_parses_the_delta_argument() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SET", b"counter", b"10"]));
    assert_eq!(dispatch(&engine, cmd(&[b"INCRBY", b"counter", b"5"])), Frame::Integer(15));
}

#[test]
fn incrby_on_a_non_integer_delta_returns_resp_error() {
    let engine = Engine::new();
    assert_eq!(
        dispatch(&engine, cmd(&[b"INCRBY", b"counter", b"notanumber"])),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
}

#[test]
fn hdel_hgetall_hexists_hlen_round_trip() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"HSET", b"h", b"f", b"v"]));
    assert_eq!(dispatch(&engine, cmd(&[b"HEXISTS", b"h", b"f"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"HLEN", b"h"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"HDEL", b"h", b"f"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"HGETALL", b"h"])), Frame::Array(vec![]));
}

#[test]
fn list_commands_round_trip() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"a"]));
    dispatch(&engine, cmd(&[b"RPUSH", b"l", b"b"]));
    dispatch(&engine, cmd(&[b"LPUSH", b"l", b"z"]));
    assert_eq!(dispatch(&engine, cmd(&[b"LLEN", b"l"])), Frame::Integer(3));
    assert_eq!(
        dispatch(&engine, cmd(&[b"LRANGE", b"l", b"0", b"-1"])),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"z")),
            Frame::Bulk(Bytes::from_static(b"a")),
            Frame::Bulk(Bytes::from_static(b"b")),
        ])
    );
    assert_eq!(dispatch(&engine, cmd(&[b"RPOP", b"l"])), Frame::Bulk(Bytes::from_static(b"b")));
    assert_eq!(dispatch(&engine, cmd(&[b"LPOP", b"l"])), Frame::Bulk(Bytes::from_static(b"z")));
}

#[test]
fn set_type_commands_round_trip() {
    let engine = Engine::new();
    dispatch(&engine, cmd(&[b"SADD", b"s", b"x"]));
    assert_eq!(dispatch(&engine, cmd(&[b"SISMEMBER", b"s", b"x"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"SISMEMBER", b"s", b"y"])), Frame::Integer(0));
    assert_eq!(dispatch(&engine, cmd(&[b"SCARD", b"s"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"SREM", b"s", b"x"])), Frame::Integer(1));
    assert_eq!(dispatch(&engine, cmd(&[b"SMEMBERS", b"s"])), Frame::Array(vec![]));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: FAIL — none of `SET NX/XX/EX/PX`, `INCRBY`, `HDEL`/`HGETALL`/`HEXISTS`/`HLEN`, list commands, or set-type commands are wired yet

- [ ] **Step 3: Replace the `SET` arm and add the remaining arms**

```rust
// crates/server/src/dispatcher.rs — replace the existing "SET" arm with:
"SET" => {
    require_args!(rest, 2, "set");
    let key = rest[0].clone();
    let val = rest[1].clone();
    let flags: Vec<String> = rest[2..]
        .iter()
        .map(|b| String::from_utf8_lossy(b).to_ascii_uppercase())
        .collect();
    if flags.iter().any(|f| f == "EX" || f == "PX") {
        return Frame::Error(
            "ERR syntax error: EX/PX are not supported yet (planned Sprint 4)".into(),
        );
    }
    if flags.iter().any(|f| f == "NX") {
        if commands::string::set_nx(engine, key, val) {
            Frame::Simple("OK".into())
        } else {
            Frame::Null
        }
    } else if flags.iter().any(|f| f == "XX") {
        if commands::string::set_xx(engine, key, val) {
            Frame::Simple("OK".into())
        } else {
            Frame::Null
        }
    } else {
        engine.set(key, Value::String(val));
        Frame::Simple("OK".into())
    }
}
```

```rust
// crates/server/src/dispatcher.rs — add these arms to the match
"INCRBY" => {
    require_args!(rest, 2, "incrby");
    let delta: i64 = match std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::string::incr_by(engine, rest[0].clone(), delta) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error_to_frame(e),
    }
}
"HDEL" => {
    require_args!(rest, 2, "hdel");
    match commands::hash::hdel(engine, &rest[0], &rest[1]) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"HGETALL" => {
    require_args!(rest, 1, "hgetall");
    match commands::hash::hgetall(engine, &rest[0]) {
        Ok(map) => Frame::Array(
            map.into_iter()
                .flat_map(|(f, v)| [Frame::Bulk(f), Frame::Bulk(v)])
                .collect(),
        ),
        Err(e) => engine_error_to_frame(e),
    }
}
"HEXISTS" => {
    require_args!(rest, 2, "hexists");
    match commands::hash::hexists(engine, &rest[0], &rest[1]) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"HLEN" => {
    require_args!(rest, 1, "hlen");
    match commands::hash::hlen(engine, &rest[0]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"RPUSH" => {
    require_args!(rest, 2, "rpush");
    match commands::list::rpush(engine, rest[0].clone(), rest[1].clone()) {
        Ok(()) => match commands::list::llen(engine, &rest[0]) {
            Ok(n) => Frame::Integer(n as i64),
            Err(e) => engine_error_to_frame(e),
        },
        Err(e) => engine_error_to_frame(e),
    }
}
"LPUSH" => {
    require_args!(rest, 2, "lpush");
    match commands::list::lpush(engine, rest[0].clone(), rest[1].clone()) {
        Ok(()) => match commands::list::llen(engine, &rest[0]) {
            Ok(n) => Frame::Integer(n as i64),
            Err(e) => engine_error_to_frame(e),
        },
        Err(e) => engine_error_to_frame(e),
    }
}
"RPOP" => {
    require_args!(rest, 1, "rpop");
    match commands::list::rpop(engine, &rest[0]) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"LPOP" => {
    require_args!(rest, 1, "lpop");
    match commands::list::lpop(engine, &rest[0]) {
        Ok(Some(b)) => Frame::Bulk(b),
        Ok(None) => Frame::Null,
        Err(e) => engine_error_to_frame(e),
    }
}
"LLEN" => {
    require_args!(rest, 1, "llen");
    match commands::list::llen(engine, &rest[0]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
"LRANGE" => {
    require_args!(rest, 3, "lrange");
    let (start, stop) = match (
        std::str::from_utf8(&rest[1]).ok().and_then(|s| s.parse::<i64>().ok()),
        std::str::from_utf8(&rest[2]).ok().and_then(|s| s.parse::<i64>().ok()),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => return Frame::Error("ERR value is not an integer or out of range".into()),
    };
    match commands::list::lrange(engine, &rest[0], start, stop) {
        Ok(items) => Frame::Array(items.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"SADD" => {
    require_args!(rest, 2, "sadd");
    match commands::set::sadd(engine, rest[0].clone(), rest[1].clone()) {
        Ok(()) => Frame::Integer(1),
        Err(e) => engine_error_to_frame(e),
    }
}
"SREM" => {
    require_args!(rest, 2, "srem");
    match commands::set::srem(engine, &rest[0], &rest[1]) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"SMEMBERS" => {
    require_args!(rest, 1, "smembers");
    match commands::set::smembers(engine, &rest[0]) {
        Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
        Err(e) => engine_error_to_frame(e),
    }
}
"SISMEMBER" => {
    require_args!(rest, 2, "sismember");
    match commands::set::sismember(engine, &rest[0], &rest[1]) {
        Ok(true) => Frame::Integer(1),
        Ok(false) => Frame::Integer(0),
        Err(e) => engine_error_to_frame(e),
    }
}
"SCARD" => {
    require_args!(rest, 1, "scard");
    match commands::set::scard(engine, &rest[0]) {
        Ok(n) => Frame::Integer(n as i64),
        Err(e) => engine_error_to_frame(e),
    }
}
```

Note on `RPUSH`/`LPUSH` return value: real Redis's `RPUSH`/`LPUSH` return the list's new length and accept multiple values in one call. `engine::commands::list::{rpush, lpush}` only take one value per call (Sprint 1 scope), so this wiring calls `llen` right after to get the length real clients expect back — accept a single value per call for now; multi-value push is a Sprint 3 "remaining list/hash/set commands" item, not this sprint's scope.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, all tests including the 7 new ones

- [ ] **Step 5: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean

- [ ] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): wire remaining Sprint 1 commands (SET flags, hash/list/set families)"
```

---

### Task 3: `redis-rs` integration test — the real end-to-end proof

**Files:**
- Modify: `crates/server/Cargo.toml`
- Create: `crates/server/tests/integration.rs`

- [ ] **Step 1: Add `redis` as a dev-dependency**

```toml
# crates/server/Cargo.toml
[dev-dependencies]
redis = "0.27"
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/server/tests/integration.rs
use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn spawn_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    tokio::spawn(rocket_mem::serve(listener, engine));
    format!("redis://{addr}")
}

#[tokio::test]
async fn redis_rs_client_can_set_and_get_over_real_tcp() {
    let url = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("foo", "bar").await.unwrap();
    let value: String = con.get("foo").await.unwrap();
    assert_eq!(value, "bar");
}

#[tokio::test]
async fn redis_rs_client_runs_a_mixed_workload_across_all_sprint_1_data_types() {
    let url = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("str", "hello").await.unwrap();
    let _: i64 = con.incr("counter", 5).await.unwrap();
    let _: () = con.hset("hash", "field", "value").await.unwrap();
    let _: () = con.rpush("list", "a").await.unwrap();
    let _: () = con.sadd("set", "member").await.unwrap();

    let str_val: String = con.get("str").await.unwrap();
    let hash_val: String = con.hget("hash", "field").await.unwrap();
    let is_member: bool = con.sismember("set", "member").await.unwrap();

    assert_eq!(str_val, "hello");
    assert_eq!(hash_val, "value");
    assert!(is_member);
}

#[tokio::test]
async fn redis_rs_client_gets_a_real_error_on_wrongtype() {
    let url = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("k", "v").await.unwrap();
    let result: redis::RedisResult<()> = con.hset("k", "f", "v").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p rocket-mem --test integration`
Expected: FAIL if `redis` isn't yet a resolved dependency; run `cargo build --workspace` first to confirm `Cargo.lock` picks it up, then re-run — the tests themselves should compile and PASS once the dependency resolves, since Task 2 already wired every command they touch. If any of them fail against real behavior, that's a real dispatcher bug Task 2 missed — fix the dispatcher, not the test.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rocket-mem --test integration`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

```bash
git add crates/server/Cargo.toml crates/server/tests/integration.rs Cargo.lock
git commit -m "test(server): add redis-rs integration test harness"
```

---

### Task 4: Malformed-input integration test

Sprint 2's Definition of Done requires "Split/malformed-input integration tests pass in CI" — Task 3 covers well-formed traffic; this task proves the server survives garbage input without taking the connection (or the process) down ungracefully.

**Files:**
- Modify: `crates/server/tests/integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
// add to crates/server/tests/integration.rs
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn malformed_resp_input_gets_a_graceful_disconnect_not_a_crash() {
    let url = spawn_test_server().await;
    let addr = url.strip_prefix("redis://").unwrap();

    let mut raw = TcpStream::connect(addr).await.unwrap();
    raw.write_all(b"@this is not RESP\r\n").await.unwrap();
    // the connection should close (EOF), not hang or echo garbage back
    let mut buf = [0u8; 16];
    let n = raw.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "expected EOF on malformed input, got {n} bytes back");

    // the server itself must still be up — a fresh, well-formed connection works fine
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::AsyncCommands::set(&mut con, "still-alive", "yes").await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo test -p rocket-mem --test integration malformed_resp_input_gets_a_graceful_disconnect_not_a_crash`
Expected: if `04-tcp-listener.md`'s `handle_connection` was implemented as specified (return on any decode `Err`), this passes immediately — its purpose is to catch a regression, not add new behavior. A hang here (test times out) means the per-connection loop isn't returning on decode errors; a panic means an unwrap somewhere isn't handling the error path.

- [ ] **Step 3: Fix `handle_connection` if this failed**

The contract: any `Err` from `framed.next()` must `return` from the task immediately, closing the socket, never `unwrap()` or loop again on it.

- [ ] **Step 4: Run the full workspace check**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: clean

- [ ] **Step 5: Commit**

```bash
git add crates/server/tests/integration.rs
git commit -m "test(server): add malformed-input integration test"
```
