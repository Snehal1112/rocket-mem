# rocket-mem-mcp: String/Key Rest + `tools/` Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `server.rs`'s tool methods into a `tools/` module (one file per command family), then
implement the rest of the String/Key command family — everything README.md's "Command coverage"
table lists under String/Key except `GET`/`SET`, which Plan 1 already shipped.

**Architecture:** Each command family gets its own file under `src/tools/`, each holding a
`#[tool_router(router = <name>_router, vis = "pub")]` impl block for `RocketMemMcpServer`.
`server.rs` composes the family routers with `+` into the router `#[tool_handler]` uses. Every
tool issues the real wire command via `redis::cmd("COMMAND").arg(...)` against the pool's
`ConnectionManager` — never a synthesized substitute command — so the MCP tool and the wire
command it fronts always match exactly.

**Tech Stack:** Rust 2021, `rmcp` 3.3, `redis` 1.7 (`tokio-comp`/`connection-manager` features),
`tokio`, `schemars`+`serde`. Same crate, same dependencies as Plan 1 — no new ones needed.

**Spec:** `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md` — see its "Command
semantics reference (verified 2026-09-13)" section, "Plan 2 (String/Key rest) + Plan 13
(Bytes/EX)" subsection, for the verified edge cases this plan's tool descriptions and tests are
grounded in.

## Global Constraints

- One MCP tool per rocket-mem command (never a generic multi-op dispatch tool) — same rule Plan 1
  established for `get`/`set`.
- No read-only mode, no command denylist, no auth layer of this crate's own — security is
  entirely delegated to whatever the target rocket-mem instance's own ACL allows.
- Tests run against a real, locally-spawned rocket-mem instance (via `tests/support/mod.rs`'s
  `spawn_rocket_mem`) — never a mock of the wire protocol.
- Every tool call goes through the pool's `ConnectionManager` via `redis::cmd(...)` — this plan
  does not introduce any new connection or pooling mechanism.
- String values stay `String` (UTF-8) for this plan, matching Plan 1's `get`/`set` — widening to
  raw `Bytes` and adding `SET ... EX` is Plan 13's job, not this one's.

---

## Before you start

Build the real `rocket-mem` binary once, from the main repo (this crate's tests spawn it), if you
haven't already for Plan 1:

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build -p rocket-mem
```

## File Structure

```
rocket-mem-mcp/
├── src/
│   ├── lib.rs         — adds `pub mod tools;`
│   ├── server.rs        — RocketMemMcpServer struct + ServerHandler; composes family routers
│   └── tools/
│       ├── mod.rs        — `pub mod string; pub mod keys;`
│       ├── string.rs      — GET/SET (moved from server.rs) + 11 string-value tools
│       └── keys.rs        — 17 key-metadata tools (new file)
└── tests/
    ├── get_set_tool.rs      — unchanged; must stay green through the Task 1 refactor
    ├── string_tools.rs       — Task 2 test
    └── key_tools.rs          — Task 3 test
```

---

### Task 1: Split `server.rs` into a `tools/` module (behavior-preserving refactor)

**Files:**
- Create: `rocket-mem-mcp/src/tools/mod.rs`
- Create: `rocket-mem-mcp/src/tools/string.rs` (receives `GetParams`/`SetParams`/`get`/`set`,
  moved verbatim from `server.rs`)
- Modify: `rocket-mem-mcp/src/server.rs` (strip down to the struct, constructor, and
  `ServerHandler` impl)
- Modify: `rocket-mem-mcp/src/lib.rs` (add `pub mod tools;`)

**Interfaces:**
- Consumes: nothing new — this task moves existing code, it doesn't add behavior.
- Produces: `tools::string::string_router() -> rmcp::handler::server::router::tool::ToolRouter<RocketMemMcpServer>`
  (the `#[tool_router(router = string_router, vis = "pub")]` macro generates this associated
  function on `RocketMemMcpServer`). `server::RocketMemMcpServer` keeps the exact same public
  shape (`new(pool: Pool) -> Self`) — no caller of Plan 1's code needs to change.

This task has no new test to write — the existing `get_set_tool.rs` suite from Plan 1 is the
regression net. The steps below keep it green through every intermediate state.

- [ ] **Step 1: Confirm the baseline is green**

Run: `cd rocket-mem-mcp && cargo test`
Expected: PASS — all of Plan 1's and the ACL/TLS task's existing tests pass. This is the state
every later step must return to.

- [ ] **Step 2: Create `tools/mod.rs` and move `get`/`set` into `tools/string.rs`**

Create `rocket-mem-mcp/src/tools/mod.rs`:

```rust
pub mod string;
```

Create `rocket-mem-mcp/src/tools/string.rs` with the `GetParams`/`SetParams` structs and the
`get`/`set` tool methods, moved verbatim out of `server.rs`, with the router named explicitly so
`server.rs` can compose it:

```rust
use redis::AsyncCommands;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct GetParams {
    /// The key to read.
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetParams {
    /// The key to write.
    key: String,
    /// The value to store.
    value: String,
}

#[tool_router(router = string_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(description = "Get the string value of a key. A missing key is not an error.")]
    async fn get(
        &self,
        Parameters(GetParams { key }): Parameters<GetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let value: Result<Option<String>, redis::RedisError> = conn.get(&key).await;
        match value {
            Ok(Some(value)) => {
                let mut result = CallToolResult::success(vec![ContentBlock::text(value.clone())]);
                result.structured_content = Some(json!({ "found": true, "value": value }));
                Ok(result)
            }
            Ok(None) => {
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text("(nil)".to_string())]);
                result.structured_content = Some(json!({ "found": false }));
                Ok(result)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set the string value of a key.")]
    async fn set(
        &self,
        Parameters(SetParams { key, value }): Parameters<SetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = conn.set(&key, &value).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
```

Note the one real change from the moved code, beyond the router name: tool methods in
`tools/string.rs` are no longer in the same file as the `pool` field, so they can't access
`self.pool` directly (private field, different module). Add a `pub(crate) fn pool(&self) -> &Pool`
accessor on `RocketMemMcpServer` in `server.rs` (Step 3) and use `self.pool()` here instead of
`self.pool`.

- [ ] **Step 3: Slim `server.rs` down to the struct, constructor, and composed handler**

Replace the entire contents of `rocket-mem-mcp/src/server.rs` with:

```rust
use rmcp::{tool_handler, ServerHandler};

use crate::pool::Pool;

/// The MCP-facing view of one rocket-mem instance. Holds a `Pool`; each tool method (spread
/// across `crate::tools::*`, one file per rocket-mem command family) borrows a connection from
/// it for the duration of one call.
#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl RocketMemMcpServer {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        crate::tools::string::string_router()
    }
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
```

**If this doesn't compile as written:** the exact mechanics of composing more than one
`#[tool_router]`-generated function into the router `#[tool_handler]` picks up (is the combining
method named `tool_router` by convention, does `#[tool_handler]` look for an instance method or
an associated function, does `+` between two `ToolRouter<Self>` values work as shown) were
inferred from this project's earlier design-phase research, not re-verified against the installed
`rmcp` 3.3 source right before writing this plan. Run `cargo doc -p rmcp --open` and search for
`tool_router` and `ToolRouter` before Task 1 is done, and adjust the composition shown here (and
in Task 3, which adds a second router to combine) to match whatever the macro actually expects.
Nothing in Task 2 depends on this mechanism working any particular way — only Task 1 and Task 3
touch router composition.

- [ ] **Step 4: Update `lib.rs`**

Modify `rocket-mem-mcp/src/lib.rs` to add the new module:

```rust
pub mod config;
pub mod errors;
pub mod pool;
pub mod server;
pub mod tools;
```

- [ ] **Step 5: Confirm the baseline is still green**

Run: `cd rocket-mem-mcp && cargo test`
Expected: PASS — the exact same tests as Step 1, unchanged, still pass. If `get_set_tool.rs`
fails or doesn't compile, the refactor broke something; fix it before moving on, since Task 2
builds on this file structure.

Also lint and format:

Run: `cd rocket-mem-mcp && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add rocket-mem-mcp/src/tools/mod.rs rocket-mem-mcp/src/tools/string.rs \
  rocket-mem-mcp/src/server.rs rocket-mem-mcp/src/lib.rs
git commit -m "Split rocket-mem-mcp tools into a tools/ module"
```

---

### Task 2: String-value tools — `GETSET`, `APPEND`, `STRLEN`, `INCR`/`DECR`/`INCRBY`, `GETRANGE`, `SETRANGE`, `MSET`/`MGET`/`MSETNX`

**Files:**
- Modify: `rocket-mem-mcp/src/tools/string.rs` (add 11 tools to the existing `string_router` impl
  block from Task 1)
- Test: `rocket-mem-mcp/tests/string_tools.rs`

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result` (Plan 1), `RocketMemMcpServer::pool()` (Task 1).
- Produces: 11 new tools — `getset`, `append`, `strlen`, `incr`, `decr`, `incr_by`, `get_range`,
  `set_range`, `mset`, `mget`, `msetnx` — added to the same `string_router` Task 1 created, so no
  further router wiring is needed in `server.rs`.

Verified semantics this task's tools and tests must match (from the spec's Plan 2 subsection):
`MSET`/`MSETNX`/`MGET` are variadic (array-shaped params, not fixed arity); `MGET` never errors
on a WRONGTYPE key — it returns `null` for that key instead; `SETRANGE` with an empty value is a
total no-op that must not create a missing key; `GETRANGE` on a missing key returns an empty
string, never an error; `INCR`/`DECR`/`INCRBY` return `NotAnInteger`/`IncrementOverflow` as
distinct tool-level errors from `WRONGTYPE`.

- [ ] **Step 1: Write the failing tests**

Create `rocket-mem-mcp/tests/string_tools.rs`:

```rust
mod support;

use rmcp::model::{object, CallToolRequestParams};
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

async fn connect_client_and_server(
    pool: Pool,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    use rmcp::ServiceExt;
    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    ()
        .serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake")
}

#[tokio::test]
async fn getset_returns_the_old_value_and_writes_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("getset.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool.clone()).await;

    let mut conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("k")
        .arg("old")
        .query_async(&mut conn)
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("getset").with_arguments(object!({"key": "k", "value": "new"})),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains("old"), "expected the old value in the reply, got: {text}");

    child.kill().ok();
}

#[tokio::test]
async fn append_extends_an_existing_value_and_reports_the_new_length() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("append.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "hello"})),
        )
        .await
        .unwrap();
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("append")
                .with_arguments(object!({"key": "k", "value": " world"})),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains("11"), "expected new length 11, got: {text}");

    child.kill().ok();
}

#[tokio::test]
async fn strlen_on_a_missing_key_is_zero_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("strlen.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("strlen").with_arguments(object!({"key": "missing"})),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains('0'), "expected 0, got: {text}");

    child.kill().ok();
}

#[tokio::test]
async fn incr_decr_and_incr_by_move_a_counter() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("incr.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    let r1 = client
        .peer()
        .call_tool(CallToolRequestParams::new("incr").with_arguments(object!({"key": "counter"})))
        .await
        .unwrap();
    assert!(format!("{:?}", r1.content).contains('1'));

    let r2 = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("incr_by")
                .with_arguments(object!({"key": "counter", "delta": 9})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", r2.content).contains("10"));

    let r3 = client
        .peer()
        .call_tool(CallToolRequestParams::new("decr").with_arguments(object!({"key": "counter"})))
        .await
        .unwrap();
    assert!(format!("{:?}", r3.content).contains('9'));

    child.kill().ok();
}

#[tokio::test]
async fn incr_on_a_non_integer_string_surfaces_not_an_integer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("incr-bad.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "abc"})),
        )
        .await
        .unwrap();
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("incr").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));

    child.kill().ok();
}

#[tokio::test]
async fn getrange_and_setrange_slice_and_patch_a_string() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("range.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "k", "value": "Hello World"})),
        )
        .await
        .unwrap();

    let getrange_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get_range")
                .with_arguments(object!({"key": "k", "start": 0, "end": 4})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", getrange_result.content).contains("Hello"));

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set_range")
                .with_arguments(object!({"key": "k", "offset": 6, "value": "Redis!"})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let final_value: String = redis::cmd("GET").arg("k").query_async(&mut conn).await.unwrap();
    assert_eq!(final_value, "Hello Redis!");

    child.kill().ok();
}

#[tokio::test]
async fn setrange_with_an_empty_value_on_a_missing_key_does_not_create_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("setrange-noop.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set_range")
                .with_arguments(object!({"key": "never-set", "offset": 0, "value": ""})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let exists: i64 = redis::cmd("EXISTS")
        .arg("never-set")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0);

    child.kill().ok();
}

#[tokio::test]
async fn mset_mget_and_msetnx_round_trip_multiple_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("mset.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}]
        })))
        .await
        .unwrap();

    let mget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("mget")
                .with_arguments(object!({"keys": ["a", "b", "missing"]})),
        )
        .await
        .unwrap();
    let text = format!("{:?}", mget_result.content);
    assert!(text.contains('1') && text.contains('2'));

    let msetnx_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("msetnx").with_arguments(object!({
            "pairs": [{"key": "a", "value": "should-not-apply"}, {"key": "c", "value": "3"}]
        })))
        .await
        .unwrap();
    assert!(format!("{:?}", msetnx_result.content).contains('0'));
    let mut conn = pool.connection();
    let c_exists: i64 = redis::cmd("EXISTS").arg("c").query_async(&mut conn).await.unwrap();
    assert_eq!(c_exists, 0, "msetnx must apply nothing when any key already exists");

    child.kill().ok();
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test string_tools`
Expected: FAIL — none of these tools exist yet, so every `call_tool` returns an unknown-tool
error.

- [ ] **Step 3: Add the 11 tools to `tools/string.rs`**

Add these parameter structs above the `#[tool_router(...)]` block (alongside `GetParams`/
`SetParams`):

```rust
#[derive(Deserialize, JsonSchema)]
pub struct KeyValueParams {
    key: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyOnlyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct IncrByParams {
    key: String,
    /// Amount to add. Negative decrements. Real rocket-mem has no INCRBYFLOAT — this is
    /// integer-only, matching INCR/DECR/INCRBY exactly (README's "Command coverage" table).
    delta: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetRangeParams {
    key: String,
    /// Start index. Negative counts from the end (-1 is the last byte). Out-of-range clamps
    /// rather than erroring.
    start: i64,
    /// End index, inclusive (unlike a typical Rust range). Negative counts from the end.
    end: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetRangeParams {
    key: String,
    offset: usize,
    /// The bytes to write at `offset`. An empty value is a documented no-op: it will not create
    /// a missing key and will not modify an existing one.
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyValuePair {
    key: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MSetParams {
    pairs: Vec<KeyValuePair>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MGetParams {
    keys: Vec<String>,
}
```

Add these methods inside the existing `impl RocketMemMcpServer` block under
`#[tool_router(router = string_router, vis = "pub")]`, after `set`:

```rust
    #[tool(description = "Set a key's value and return its previous value. A missing key \
        returns null for the old value and still gets the new one written.")]
    async fn getset(
        &self,
        Parameters(KeyValueParams { key, value }): Parameters<KeyValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("GETSET")
            .arg(&key)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(old) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    old.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "old_value": old }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Append a value to a string key, creating it if missing. Returns the \
        string's length after the append.")]
    async fn append(
        &self,
        Parameters(KeyValueParams { key, value }): Parameters<KeyValueParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> = redis::cmd("APPEND")
            .arg(&key)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(len.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the length of a string value. A missing key returns 0, not an error.")]
    async fn strlen(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("STRLEN").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(len.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Increment an integer string value by 1. A missing key initializes to \
        1. Errors if the existing value is not an integer, or if the increment would overflow i64.")]
    async fn incr(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("INCR").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Decrement an integer string value by 1. A missing key initializes to \
        -1. Errors if the existing value is not an integer, or if the decrement would overflow i64.")]
    async fn decr(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("DECR").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Add `delta` (may be negative) to an integer string value. A missing \
        key initializes as if it were 0. There is no INCRBYFLOAT or DECRBY in rocket-mem — this \
        integer-only tool covers both directions.")]
    async fn incr_by(
        &self,
        Parameters(IncrByParams { key, delta }): Parameters<IncrByParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("INCRBY")
            .arg(&key)
            .arg(delta)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get a substring by byte index, inclusive on both ends. Negative \
        indices count from the end (-1 is the last byte). Out-of-range indices clamp to an \
        empty result rather than erroring. A missing key returns an empty string, not an error.")]
    async fn get_range(
        &self,
        Parameters(GetRangeParams { key, start, end }): Parameters<GetRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> = redis::cmd("GETRANGE")
            .arg(&key)
            .arg(start)
            .arg(end)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Overwrite part of a string starting at a byte offset, zero-padding \
        first if the offset extends past the current length. An empty value is a documented \
        no-op: it will not create a missing key and will not modify an existing one. Returns \
        the string's length after the write.")]
    async fn set_range(
        &self,
        Parameters(SetRangeParams { key, offset, value }): Parameters<SetRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> = redis::cmd("SETRANGE")
            .arg(&key)
            .arg(offset)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => Ok(CallToolResult::success(vec![ContentBlock::text(len.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set multiple key/value pairs at once. Always succeeds and returns OK.")]
    async fn mset(
        &self,
        Parameters(MSetParams { pairs }): Parameters<MSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MSET");
        for pair in &pairs {
            command.arg(&pair.key).arg(&pair.value);
        }
        let result: Result<(), redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("OK".to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get multiple keys' string values at once, in the order requested. A \
        missing key or a non-string (WRONGTYPE) key both come back as null in that position — \
        MGET never errors on WRONGTYPE, unlike every other string command.")]
    async fn mget(
        &self,
        Parameters(MGetParams { keys }): Parameters<MGetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MGET");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<Vec<Option<String>>, redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok(values) => {
                let text = values
                    .iter()
                    .map(|v| v.clone().unwrap_or_else(|| "(nil)".to_string()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "values": values }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set multiple key/value pairs only if none of the keys already exist. \
        If any key exists, nothing is written and this returns false.")]
    async fn msetnx(
        &self,
        Parameters(MSetParams { pairs }): Parameters<MSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("MSETNX");
        for pair in &pairs {
            command.arg(&pair.key).arg(&pair.value);
        }
        let result: Result<bool, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test string_tools`
Expected: PASS — all 8 tests green.

Run the full suite to confirm nothing else broke, then lint:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/string.rs rocket-mem-mcp/tests/string_tools.rs
git commit -m "Add rocket-mem-mcp string-value tools"
```

---

### Task 3: Key-metadata tools — `DEL`/`EXISTS`, `KEYS`/`SCAN`, `RENAME`/`RENAMENX`, `TYPE`, `RANDOMKEY`, TTL family, `MEMORY USAGE`, `OBJECT ENCODING`

**Files:**
- Create: `rocket-mem-mcp/src/tools/keys.rs`
- Modify: `rocket-mem-mcp/src/tools/mod.rs` (add `pub mod keys;`)
- Modify: `rocket-mem-mcp/src/server.rs` (compose `keys_router` into `tool_router`)
- Test: `rocket-mem-mcp/tests/key_tools.rs`

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result` (Plan 1), `RocketMemMcpServer::pool()` (Task 1).
- Produces: 17 new tools — `del`, `exists`, `keys`, `scan`, `rename`, `rename_nx`, `type_`
  (MCP-facing name `type`), `randomkey`, `expire`, `pexpire`, `expire_at`, `pexpire_at`, `ttl`,
  `pttl`, `persist`, `memory_usage`, `object_encoding`.

Verified semantics this task's tools and tests must match (from the spec's Plan 2 subsection):
`DEL`/`EXISTS` are variadic and count matches, not args; `KEYS`'s glob support is partial;
`SCAN` is cursor-based (`(next_cursor, keys)` — this is not a one-shot call like `KEYS`, a caller
must loop until `next_cursor` is `0`) and rocket-mem's `SCAN` validates but silently ignores
`COUNT` (documented in `dispatcher.rs`'s `ScanOptions::parse` as a deliberate no-op, since this
engine's cursor already advances one whole shard per call) — this tool exposes `MATCH` and `TYPE`
but not `COUNT`, since exposing a parameter that does nothing would mislead a caller; `RENAME`
errors `NoSuchKey` if the source is missing and preserves the source's TTL on the destination;
`RENAMENX` returns `false` (not an error) if the destination already exists; `TTL`/`PTTL` return
`-2` for a missing key, `-1` for no expiry, and floor a real remaining duration at `1` (never `0`,
even if less than a second/millisecond remains); `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT` clamp
a negative argument to `0` (an immediate expiry) rather than erroring; `OBJECT ENCODING` returns
the engine's type name (e.g. `"string"`), not a real Redis internal encoding, and errors
`"no such key"` on a missing key — unlike `TTL`'s `-2` sentinel, this is a real tool-level error.

- [ ] **Step 1: Write the failing tests**

Create `rocket-mem-mcp/tests/key_tools.rs`:

```rust
mod support;

use rmcp::model::{object, CallToolRequestParams};
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

async fn connect_client_and_server(
    pool: Pool,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    use rmcp::ServiceExt;
    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    ()
        .serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake")
}

#[tokio::test]
async fn del_and_exists_count_matching_keys_not_args() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("del.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}]
        })))
        .await
        .unwrap();

    let exists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("exists")
                .with_arguments(object!({"keys": ["a", "a", "missing"]})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", exists_result.content).contains('2'));

    let del_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("del").with_arguments(object!({"keys": ["a", "b", "missing"]})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", del_result.content).contains('2'));

    child.kill().ok();
}

#[tokio::test]
async fn keys_and_scan_find_the_same_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("keys.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("mset").with_arguments(object!({
            "pairs": [{"key": "alpha", "value": "1"}, {"key": "beta", "value": "2"}]
        })))
        .await
        .unwrap();

    let keys_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("keys").with_arguments(object!({"pattern": "*"})))
        .await
        .unwrap();
    let keys_text = format!("{:?}", keys_result.content);
    assert!(keys_text.contains("alpha") && keys_text.contains("beta"));

    // SCAN is cursor-based: loop until the returned cursor comes back 0.
    let mut cursor: u64 = 0;
    let mut found = Vec::new();
    loop {
        let scan_result = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("scan").with_arguments(object!({"cursor": cursor})),
            )
            .await
            .unwrap();
        let structured = scan_result.structured_content.expect("scan must return structured_content");
        let next_cursor = structured["cursor"].as_u64().unwrap();
        for k in structured["keys"].as_array().unwrap() {
            found.push(k.as_str().unwrap().to_string());
        }
        if next_cursor == 0 {
            break;
        }
        cursor = next_cursor;
    }
    assert!(found.contains(&"alpha".to_string()));
    assert!(found.contains(&"beta".to_string()));

    child.kill().ok();
}

#[tokio::test]
async fn rename_moves_a_value_and_renamenx_refuses_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("rename.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "src", "value": "v"})),
        )
        .await
        .unwrap();
    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename")
                .with_arguments(object!({"source": "src", "destination": "dst"})),
        )
        .await
        .unwrap();
    let mut conn = pool.connection();
    let moved: String = redis::cmd("GET").arg("dst").query_async(&mut conn).await.unwrap();
    assert_eq!(moved, "v");

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "other", "value": "x"})),
        )
        .await
        .unwrap();
    let renamenx_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename_nx")
                .with_arguments(object!({"source": "other", "destination": "dst"})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", renamenx_result.content).contains('0'));

    let missing_rename = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("rename")
                .with_arguments(object!({"source": "does-not-exist", "destination": "z"})),
        )
        .await
        .unwrap();
    assert_eq!(missing_rename.is_error, Some(true));

    child.kill().ok();
}

#[tokio::test]
async fn type_and_randomkey_report_real_state() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("type.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();
    let type_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("type").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", type_result.content).contains("string"));

    let randomkey_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("randomkey").with_arguments(object!({})))
        .await
        .unwrap();
    assert!(format!("{:?}", randomkey_result.content).contains('k'));

    child.kill().ok();
}

#[tokio::test]
async fn expire_family_and_ttl_family_and_persist_agree() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("ttl.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();

    let missing_ttl = client
        .peer()
        .call_tool(CallToolRequestParams::new("ttl").with_arguments(object!({"key": "no-expiry-yet"})))
        .await
        .unwrap();
    assert!(format!("{:?}", missing_ttl.content).contains("-2"));

    let expire_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("expire")
                .with_arguments(object!({"key": "k", "seconds": 100})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", expire_result.content).contains('1'));

    let ttl_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ttl").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    let ttl_text = format!("{:?}", ttl_result.content);
    assert!(!ttl_text.contains("-1"), "key should have a real TTL now, got: {ttl_text}");

    let persist_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("persist").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", persist_result.content).contains('1'));

    let after_persist_ttl = client
        .peer()
        .call_tool(CallToolRequestParams::new("ttl").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert!(format!("{:?}", after_persist_ttl.content).contains("-1"));

    child.kill().ok();
}

#[tokio::test]
async fn memory_usage_and_object_encoding_report_real_state_and_error_on_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, addr) = support::spawn_rocket_mem(&dir.path().join("memory.aof"));
    let pool = Pool::connect(&addr).await.unwrap();
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set").with_arguments(object!({"key": "k", "value": "v"})),
        )
        .await
        .unwrap();

    let usage_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("memory_usage").with_arguments(object!({"key": "k"})))
        .await
        .unwrap();
    assert_ne!(usage_result.is_error, Some(true));

    let encoding_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("object_encoding").with_arguments(object!({"key": "k"})),
        )
        .await
        .unwrap();
    assert!(format!("{:?}", encoding_result.content).contains("string"));

    let missing_encoding = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("object_encoding")
                .with_arguments(object!({"key": "does-not-exist"})),
        )
        .await
        .unwrap();
    assert_eq!(missing_encoding.is_error, Some(true));

    child.kill().ok();
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test key_tools`
Expected: FAIL — none of these tools exist yet.

- [ ] **Step 3: Implement `tools/keys.rs` and wire it into `server.rs`**

Create `rocket-mem-mcp/src/tools/keys.rs`:

```rust
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::errors::redis_error_to_tool_result;
use crate::server::RocketMemMcpServer;

#[derive(Deserialize, JsonSchema)]
pub struct KeysListParams {
    keys: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeysGlobParams {
    /// A glob pattern. rocket-mem's glob support is partial — see
    /// docs/command-compatibility.md for exactly which glob features are supported.
    pattern: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ScanParams {
    /// 0 starts a new scan. Pass back whatever cursor the previous call returned until it comes
    /// back 0, which means the scan is complete.
    cursor: u64,
    /// Optional glob filter, same partial support as the `keys` tool.
    #[serde(default)]
    match_pattern: Option<String>,
    /// Optional type filter (e.g. "string", "hash").
    #[serde(default)]
    type_filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenameParams {
    source: String,
    destination: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeyOnlyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct NoParams {}

#[derive(Deserialize, JsonSchema)]
pub struct ExpireParams {
    key: String,
    /// A negative value is clamped to 0 (an immediate expiry), not rejected as an error.
    seconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct PExpireParams {
    key: String,
    milliseconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExpireAtParams {
    key: String,
    unix_seconds: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct PExpireAtParams {
    key: String,
    unix_milliseconds: i64,
}

#[tool_router(router = keys_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(description = "Delete one or more keys. Returns the count of keys that actually \
        existed and were deleted — not the count of keys given.")]
    async fn del(
        &self,
        Parameters(KeysListParams { keys }): Parameters<KeysListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("DEL");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Count how many of the given keys exist. Duplicate keys in the input \
        are each counted separately if the key exists.")]
    async fn exists(
        &self,
        Parameters(KeysListParams { keys }): Parameters<KeysListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("EXISTS");
        for key in &keys {
            command.arg(key);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "List every key matching a glob pattern. rocket-mem's glob support is \
        partial — see docs/command-compatibility.md. For a large keyspace, prefer `scan` instead \
        of `keys`, which returns everything in one call.")]
    async fn keys(
        &self,
        Parameters(KeysGlobParams { pattern }): Parameters<KeysGlobParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> =
            redis::cmd("KEYS").arg(&pattern).query_async(&mut conn).await;
        match result {
            Ok(keys) => {
                let text = keys.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "keys": keys }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Iterate the keyspace one shard at a time. Pass cursor 0 to start; \
        keep calling with the returned cursor until it comes back 0, which means the scan is \
        complete. rocket-mem's SCAN accepts but ignores a COUNT option in real Redis (its \
        cursor already advances a whole shard per call), so this tool does not expose one — it \
        would do nothing.")]
    async fn scan(
        &self,
        Parameters(ScanParams {
            cursor,
            match_pattern,
            type_filter,
        }): Parameters<ScanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("SCAN");
        command.arg(cursor);
        if let Some(pattern) = &match_pattern {
            command.arg("MATCH").arg(pattern);
        }
        if let Some(type_name) = &type_filter {
            command.arg("TYPE").arg(type_name);
        }
        let result: Result<(u64, Vec<String>), redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok((next_cursor, keys)) => {
                let text = format!("cursor={next_cursor}, keys=[{}]", keys.join(", "));
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "cursor": next_cursor, "keys": keys }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Rename a key. Errors if the source key does not exist. The \
        destination's TTL always ends up matching the source's — if the source had a \
        remaining TTL, the destination gets it; if not, the destination ends up with no TTL, \
        even if it previously had one.")]
    async fn rename(
        &self,
        Parameters(RenameParams { source, destination }): Parameters<RenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("RENAME")
            .arg(&source)
            .arg(&destination)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("OK".to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Rename a key, but only if the destination does not already exist. \
        Returns false (not an error) if the destination exists; errors if the source is missing.")]
    async fn rename_nx(
        &self,
        Parameters(RenameParams { source, destination }): Parameters<RenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("RENAMENX")
            .arg(&source)
            .arg(&destination)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(name = "type", description = "Report a key's type: \"string\", \"hash\", \"list\", \
        \"set\", \"zset\", or \"none\" if the key does not exist. Unlike most commands, a \
        missing key is reported as the type \"none\", not an error.")]
    async fn type_(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> =
            redis::cmd("TYPE").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(type_name) => Ok(CallToolResult::success(vec![ContentBlock::text(type_name)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Return one key chosen uniformly at random from the whole keyspace. \
        Returns null, not an error, if the keyspace is empty.")]
    async fn randomkey(
        &self,
        Parameters(NoParams {}): Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("RANDOMKEY").query_async(&mut conn).await;
        match result {
            Ok(key) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    key.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "key": key }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set a key's expiry, in seconds from now. A negative value is clamped \
        to 0 (an immediate expiry), not rejected. Returns true if the key existed and got the \
        expiry set, false if the key did not exist.")]
    async fn expire(
        &self,
        Parameters(ExpireParams { key, seconds }): Parameters<ExpireParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(seconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set a key's expiry, in milliseconds from now. A negative value is \
        clamped to 0. Returns true if the key existed and got the expiry set, false otherwise.")]
    async fn pexpire(
        &self,
        Parameters(PExpireParams { key, milliseconds }): Parameters<PExpireParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("PEXPIRE")
            .arg(&key)
            .arg(milliseconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set a key's expiry to an absolute Unix timestamp, in seconds. A \
        timestamp in the past is clamped to an immediate expiry, not rejected. Returns true if \
        the key existed and got the expiry set, false otherwise.")]
    async fn expire_at(
        &self,
        Parameters(ExpireAtParams { key, unix_seconds }): Parameters<ExpireAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("EXPIREAT")
            .arg(&key)
            .arg(unix_seconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set a key's expiry to an absolute Unix timestamp, in milliseconds. \
        Returns true if the key existed and got the expiry set, false otherwise.")]
    async fn pexpire_at(
        &self,
        Parameters(PExpireAtParams { key, unix_milliseconds }): Parameters<PExpireAtParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("PEXPIREAT")
            .arg(&key)
            .arg(unix_milliseconds)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get a key's remaining time-to-live in seconds. Returns -2 if the key \
        does not exist, -1 if the key exists but has no expiry, or the remaining seconds \
        (floored at 1 — never 0, even with under a second left) otherwise.")]
    async fn ttl(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("TTL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get a key's remaining time-to-live in milliseconds. Returns -2 if the \
        key does not exist, -1 if the key exists but has no expiry, or the remaining \
        milliseconds (floored at 1) otherwise.")]
    async fn pttl(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> =
            redis::cmd("PTTL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(n) => Ok(CallToolResult::success(vec![ContentBlock::text(n.to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Remove a key's expiry, making it persist forever. Returns true if the \
        key existed and had an expiry that was removed, false otherwise (including if the key \
        existed but already had no expiry).")]
    async fn persist(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> =
            redis::cmd("PERSIST").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(applied) => Ok(CallToolResult::success(vec![ContentBlock::text(
                if applied { "1" } else { "0" }.to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the approximate memory footprint of a key's value, in bytes. \
        Returns null, not an error, if the key does not exist.")]
    async fn memory_usage(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<i64>, redis::RedisError> = redis::cmd("MEMORY")
            .arg("USAGE")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(usage) => {
                let text = usage.map(|n| n.to_string()).unwrap_or_else(|| "(nil)".to_string());
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "bytes": usage }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Report the engine's internal type name for a key's value (e.g. \
        \"string\", \"hash\") — this is rocket-mem's own type name, not a real Redis encoding \
        like \"listpack\" or \"embstr\". Unlike TTL's -2 sentinel, a missing key here is a real \
        tool-level error: \"no such key\".")]
    async fn object_encoding(
        &self,
        Parameters(KeyOnlyParams { key }): Parameters<KeyOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<String, redis::RedisError> = redis::cmd("OBJECT")
            .arg("ENCODING")
            .arg(&key)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(encoding) => Ok(CallToolResult::success(vec![ContentBlock::text(encoding)])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
```

Modify `rocket-mem-mcp/src/tools/mod.rs`:

```rust
pub mod keys;
pub mod string;
```

Modify the `tool_router` composition in `rocket-mem-mcp/src/server.rs`:

```rust
impl RocketMemMcpServer {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        crate::tools::string::string_router() + crate::tools::keys::keys_router()
    }
}
```

**If the `+` composition doesn't compile as written:** see Task 1's fallback note — check
`cargo doc -p rmcp --open` for `ToolRouter`'s actual composition operator/method before assuming
this is wrong; the underlying macro mechanics were flagged as unverified there, and this is the
step that actually exercises composing two routers together.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test key_tools`
Expected: PASS — all 6 tests green.

Run the full suite, then lint:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean. This confirms the full String/Key command family (28 tools) plus
`get`/`set` (30 total) all work together through one composed router.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/keys.rs rocket-mem-mcp/src/tools/mod.rs \
  rocket-mem-mcp/src/server.rs rocket-mem-mcp/tests/key_tools.rs
git commit -m "Add rocket-mem-mcp key-metadata tools"
```

---

## Next plan

Per `project_rocket_mem_mcp_roadmap` (memory) and
`docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md`'s "Command semantics
reference," **Plan 3: Hash family** is next — `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HGETALL`,
`HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSETNX` (11 tools; `HSCAN` is not implemented in
rocket-mem, so it is out of scope). Notable grounding already recorded for that plan: `HSET`/
`HSETNX` are single-pair at the engine level (the dispatcher loops internally for real Redis's
variadic multi-pair `HSET` — Plan 3's tool needs to either accept one field/value pair, matching
the engine directly, or accept an array and loop the way the dispatcher does); `HDEL`/`HMGET` are
already variadic; `HINCRBY` has two extra error types beyond `WRONGTYPE` (`NotAnInteger`,
`IncrementOverflow`), the same two `incr_by` surfaces in this plan's `incr`/`incr_by` tools.

After Hash: List (Plan 4), Set (Plan 5), Sorted Set (Plan 6), Server/Cluster/Slowlog admin
(Plan 7), ACL admin (Plan 8), `run_transaction` (Plan 9), Pub/Sub + session (Plan 10), Streamable
HTTP transport (Plan 11), TOML config-file layer (Plan 12), and `Bytes`/`EX` value typing widening
this plan's string tools (Plan 13) — see the roadmap memory for full detail on each.
