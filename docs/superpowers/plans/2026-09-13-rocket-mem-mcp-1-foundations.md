# rocket-mem-mcp: Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `rocket-mem-mcp` crate's foundation — connection pool, error mapping, and
an MCP server that speaks `stdio` and exposes `get`/`set` as real tools against a live rocket-mem
instance — proving the whole pipeline end to end before adding the rest of the command surface.

**Architecture:** A standalone Rust binary+lib crate outside the Cargo workspace. `pool.rs` wraps
a `redis::aio::ConnectionManager` (auto-reconnecting, cheaply `Clone`, multiplexed — no separate
pooling crate needed). `server.rs` holds an `rmcp` `#[tool_router]`/`ServerHandler` impl. `main.rs`
wires config → pool → server → `stdio()` transport.

**Tech Stack:** Rust 2021, `rmcp` 3.3 (MCP SDK: `server`/`client`/`macros`/`transport-io`/
`transport-async-rw` features), `redis` 1.7 (`tokio-comp`/`connection-manager` features), `tokio`,
`clap` (config), `schemars`+`serde` (tool parameter schemas).

**Spec:** `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md`

## Global Constraints

- This crate is NOT a member of the root workspace's `Cargo.toml` `members` list, and is not
  built or gated by `.github/workflows/ci.yml`.
- Connects over RESP2/RESP3 via the `redis` crate — never RMP, never `crates/rmp-client`.
- No read-only mode, no command denylist, no auth layer of this crate's own — security is
  entirely delegated to whatever the target rocket-mem instance's own ACL allows the configured
  connection to do.
- One MCP tool per rocket-mem command (never a generic multi-op dispatch tool).
- Tests run against a real, locally-spawned rocket-mem instance — never a mock of the wire
  protocol.

---

## Before you start

Build the real `rocket-mem` binary once, from the main repo (this crate's tests spawn it):

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build -p rocket-mem
```

This produces `target/debug/rocket-mem`, which `rocket-mem-mcp`'s test helper (Task 1) finds by
default. If you build it somewhere else, point tests at it with `ROCKET_MEM_BIN=/path/to/rocket-mem`.

## File Structure

```
rocket-mem-mcp/
├── Cargo.toml
├── src/
│   ├── lib.rs        — re-exports config/pool/errors/server as public modules
│   ├── main.rs        — parses Config, connects Pool, builds RocketMemMcpServer, serves over stdio
│   ├── config.rs       — Config (clap::Parser): target_addr
│   ├── pool.rs         — Pool: wraps redis::aio::ConnectionManager
│   ├── errors.rs        — maps redis::RedisError into a tool-level CallToolResult or a
│   │                       protocol-level ErrorData
│   └── server.rs        — RocketMemMcpServer: #[tool_router] impl + ServerHandler
└── tests/
    ├── support/
    │   └── mod.rs      — spawn_rocket_mem(): starts the real binary, discovers its port
    ├── pool.rs         — Task 1 test
    ├── server_handshake.rs — Task 2 test
    └── get_set_tool.rs    — Task 3 test
```

`server.rs` holds the tool methods directly for now (only `get`/`set` exist after this plan). Once
later plans add the rest of the command surface, `server.rs`'s tool bodies move into a `tools/`
module split by command family — not needed yet at two tools.

---

### Task 1: Crate scaffold, config, and the connection pool

**Files:**
- Create: `rocket-mem-mcp/Cargo.toml`
- Create: `rocket-mem-mcp/src/lib.rs`
- Create: `rocket-mem-mcp/src/config.rs`
- Create: `rocket-mem-mcp/src/pool.rs`
- Create: `rocket-mem-mcp/tests/support/mod.rs`
- Test: `rocket-mem-mcp/tests/pool.rs`

**Interfaces:**
- Produces: `pool::Pool::connect(target_addr: &str) -> redis::RedisResult<Pool>`,
  `pool::Pool::connection(&self) -> redis::aio::ConnectionManager` (the returned value is
  `Clone`, cheap to call per-request). `config::Config` (derives `clap::Parser`) with public
  field `target_addr: String`. `support::spawn_rocket_mem(aof_path: &std::path::Path) ->
  (std::process::Child, String)`, returning the spawned process and its bound `"host:port"`.

- [ ] **Step 1: Scaffold the crate and write the failing test**

Create `rocket-mem-mcp/Cargo.toml`:

```toml
[package]
name = "rocket-mem-mcp"
version = "0.1.0"
edition = "2021"

# Intentionally NOT a member of the root rocket-mem workspace — see
# docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md's "Why standalone, not a
# 6th workspace crate". An empty [workspace] table here stops Cargo from trying to fold this
# package into the parent directory's workspace (the root Cargo.toml's `members` list doesn't,
# and must not, include it).
[workspace]

[lib]
name = "rocket_mem_mcp"
path = "src/lib.rs"

[[bin]]
name = "rocket-mem-mcp"
path = "src/main.rs"

[dependencies]
rmcp = { version = "3.3", features = ["server", "client", "macros", "transport-io", "transport-async-rw"] }
redis = { version = "1.7", features = ["tokio-comp", "connection-manager"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-util", "io-std"] }
serde = { version = "1", features = ["derive"] }
schemars = "1.0"
clap = { version = "4", features = ["derive", "env"] }
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

Create `rocket-mem-mcp/src/lib.rs`:

```rust
pub mod config;
pub mod errors;
pub mod pool;
pub mod server;
```

Create `rocket-mem-mcp/tests/support/mod.rs` — adapted from
`crates/server/tests/kill_and_recover.rs`'s `spawn_server` helper. That version locates the
binary via `env!("CARGO_BIN_EXE_rocket-mem")`, which Cargo only injects inside the same
workspace; this crate is deliberately outside it, so the binary path comes from an env var
instead:

```rust
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn rocket_mem_bin() -> PathBuf {
    if let Ok(path) = std::env::var("ROCKET_MEM_BIN") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/rocket-mem")
}

/// Spawns the real compiled `rocket-mem` binary bound to an OS-assigned port, reading its
/// startup banner on stdout to discover which port it actually got. Returns the child (so the
/// caller can kill it) and the bound `"host:port"` string. Mirrors
/// `crates/server/tests/kill_and_recover.rs`'s `spawn_server`.
pub fn spawn_rocket_mem(aof_path: &std::path::Path) -> (Child, String) {
    let bin = rocket_mem_bin();
    let mut child = Command::new(&bin)
        .env("ROCKET_MEM_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_METRICS_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_RMP_ADDR", "127.0.0.1:0")
        .env("ROCKET_MEM_AOF_PATH", aof_path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "failed to spawn {bin:?}: {e} — build it first with `cargo build -p rocket-mem` \
                 from the repo root, or set ROCKET_MEM_BIN"
            )
        });

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut reader = BufReader::new(stdout);
    let mut addr = None;
    for _ in 0..20 {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim().trim_matches(|c| c == '│' || c == ' ');
                let mut parts = trimmed.split_whitespace();
                if parts.next() == Some("RESP") {
                    if let Some(addr_str) = parts.next() {
                        addr = Some(addr_str.to_string());
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let addr = addr.expect("rocket-mem never printed its listening address on stdout");
    (child, addr)
}
```

Create `rocket-mem-mcp/tests/pool.rs`:

```rust
mod support;

use redis::AsyncCommands;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn set_then_get_round_trips_through_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("pool-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);

    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let mut conn = pool.connection();
    let _: () = conn.set("pool-test-key", "pool-test-value").await.unwrap();
    let value: String = conn.get("pool-test-key").await.unwrap();
    assert_eq!(value, "pool-test-value");

    child.kill().ok();
}
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cd rocket-mem-mcp && cargo test --test pool`
Expected: compile error — `pool` module (and `Pool` type) don't exist yet.

- [ ] **Step 3: Implement `config.rs` and `pool.rs`**

Create `rocket-mem-mcp/src/config.rs`:

```rust
use clap::Parser;

/// Startup configuration for rocket-mem-mcp: which rocket-mem instance to connect to.
#[derive(Parser, Debug, Clone)]
#[command(name = "rocket-mem-mcp")]
pub struct Config {
    /// host:port of the rocket-mem instance this server exposes as MCP tools.
    #[arg(long, env = "ROCKET_MEM_MCP_TARGET_ADDR", default_value = "127.0.0.1:6379")]
    pub target_addr: String,
}
```

Create `rocket-mem-mcp/src/pool.rs`:

```rust
use redis::aio::ConnectionManager;
use redis::{Client, RedisResult};

/// A cheap-to-clone handle to a rocket-mem connection. `ConnectionManager` already multiplexes
/// concurrent requests over one underlying connection and reconnects automatically, so no
/// separate pooling crate (bb8/deadpool) is needed — every tool call just clones this and
/// issues its command.
#[derive(Clone)]
pub struct Pool {
    manager: ConnectionManager,
}

impl Pool {
    pub async fn connect(target_addr: &str) -> RedisResult<Self> {
        let client = Client::open(format!("redis://{target_addr}"))?;
        let manager = client.get_connection_manager().await?;
        Ok(Self { manager })
    }

    pub fn connection(&self) -> ConnectionManager {
        self.manager.clone()
    }
}
```

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cd rocket-mem-mcp && cargo test --test pool`
Expected: PASS — `set_then_get_round_trips_through_the_pool ... ok`

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/Cargo.toml rocket-mem-mcp/src/lib.rs rocket-mem-mcp/src/config.rs \
  rocket-mem-mcp/src/pool.rs rocket-mem-mcp/tests/support/mod.rs rocket-mem-mcp/tests/pool.rs \
  rocket-mem-mcp/Cargo.lock
git commit -m "Add rocket-mem-mcp crate scaffold and connection pool"
```

---

### Task 2: Error mapping and the MCP server skeleton over stdio

**Files:**
- Create: `rocket-mem-mcp/src/errors.rs`
- Create: `rocket-mem-mcp/src/server.rs`
- Create: `rocket-mem-mcp/src/main.rs`
- Test: `rocket-mem-mcp/tests/server_handshake.rs`

**Interfaces:**
- Consumes: `pool::Pool` (Task 1) — `Pool::connect`, `Pool::connection() -> ConnectionManager`.
- Produces: `errors::redis_error_to_tool_result(err: redis::RedisError) ->
  Result<rmcp::model::CallToolResult, rmcp::model::ErrorData>` — later tasks' tools call this to
  turn any `redis::RedisError` into the right kind of MCP failure. `server::RocketMemMcpServer`
  (holds a `Pool`), constructed via `RocketMemMcpServer::new(pool: Pool) -> Self`, implementing
  `rmcp::ServerHandler` (no tools registered yet — Task 3 adds the first ones to the same
  `#[tool_router]` impl block).

- [ ] **Step 1: Write the failing test**

Create `rocket-mem-mcp/tests/server_handshake.rs`. This drives the server over an in-process
`tokio::io::duplex` pipe instead of spawning a subprocess and talking to its real stdin/stdout —
`rmcp`'s transport is generic over any `(AsyncRead, AsyncWrite)` pair (the
`transport-async-rw` feature), so a duplex pipe is a faithful stand-in for real stdio without the
overhead of a second process:

```rust
mod support;

use rmcp::ServiceExt;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

#[tokio::test]
async fn mcp_handshake_and_list_tools_succeed_over_a_duplex_transport() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("handshake-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");

    let (server_io, client_io) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server = RocketMemMcpServer::new(pool);
    let server_handle = tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server should complete the MCP handshake");
        running.waiting().await.ok();
    });

    let client = ()
        .serve((client_read, client_write))
        .await
        .expect("client should complete the MCP handshake");
    let tools = client.peer().list_tools(Default::default()).await;
    assert!(tools.is_ok(), "list_tools should succeed: {tools:?}");

    server_handle.abort();
    child.kill().ok();
}
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cd rocket-mem-mcp && cargo test --test server_handshake`
Expected: compile error — `errors` module and `server::RocketMemMcpServer` don't exist yet.

- [ ] **Step 3: Implement `errors.rs`, `server.rs`, and `main.rs`**

Create `rocket-mem-mcp/src/errors.rs`:

```rust
use redis::RedisError;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};

/// Splits a `redis::RedisError` into the two kinds of MCP failure the agent needs to tell
/// apart: a *protocol-level* error (the connection to rocket-mem itself is broken — the agent
/// should treat this as "retry me"), returned as `Err(ErrorData)`; or a *tool-level* failure
/// (WRONGTYPE, unknown command, etc. — the command reached rocket-mem and rocket-mem rejected
/// it), returned as `Ok(CallToolResult::error(..))` so the agent sees the real message and can
/// decide what to do next, rather than the failure being swallowed into a generic error.
pub fn redis_error_to_tool_result(err: RedisError) -> Result<CallToolResult, ErrorData> {
    if err.is_io_error() || err.is_connection_dropped() || err.is_connection_refusal() {
        return Err(ErrorData::internal_error(
            format!("lost connection to rocket-mem: {err}"),
            None,
        ));
    }
    Ok(CallToolResult::error(vec![ContentBlock::text(err.to_string())]))
}
```

Create `rocket-mem-mcp/src/server.rs`:

```rust
use rmcp::{ServerHandler, tool_handler, tool_router};

use crate::pool::Pool;

/// The MCP-facing view of one rocket-mem instance. Holds a `Pool` (Task 1); each tool method
/// (added starting in Task 3) borrows a connection from it for the duration of one call.
#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[tool_router]
impl RocketMemMcpServer {
    // Tool methods land here, starting with `get`/`set` in Task 3.
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
```

Create `rocket-mem-mcp/src/main.rs`:

```rust
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;

use rocket_mem_mcp::config::Config;
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let pool = Pool::connect(&config.target_addr).await?;
    let server = RocketMemMcpServer::new(pool);

    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}
```

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cd rocket-mem-mcp && cargo test --test server_handshake`
Expected: PASS — `mcp_handshake_and_list_tools_succeed_over_a_duplex_transport ... ok`

Also confirm the binary builds (it isn't exercised by any test yet, but must compile):

Run: `cd rocket-mem-mcp && cargo build`
Expected: builds cleanly.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/errors.rs rocket-mem-mcp/src/server.rs rocket-mem-mcp/src/main.rs \
  rocket-mem-mcp/tests/server_handshake.rs rocket-mem-mcp/Cargo.lock
git commit -m "Add rocket-mem-mcp error mapping and stdio server skeleton"
```

---

### Task 3: First tools — `get` and `set`

**Files:**
- Modify: `rocket-mem-mcp/src/server.rs` (add `get`/`set` to the existing `#[tool_router]` block)
- Test: `rocket-mem-mcp/tests/get_set_tool.rs`

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result` (Task 2), `pool::Pool::connection` (Task 1).
- Produces: two MCP tools, `get` and `set`, callable by any MCP client connected to
  `RocketMemMcpServer` — the pattern every later tool (Plan 2 onward) follows.

- [ ] **Step 1: Write the failing test**

Create `rocket-mem-mcp/tests/get_set_tool.rs`. This exercises the tools the same way a real MCP
client would — through `call_tool`, not by calling Rust functions directly — and covers the two
behaviors the spec's "Error handling" section requires: a missing key is not an error, and a
WRONGTYPE from rocket-mem surfaces to the agent verbatim rather than being swallowed:

`CallToolRequestParams` is `#[non_exhaustive]`, so it can't be built with a plain struct
literal or `..Default::default()` from outside the `rmcp` crate — it exposes a builder instead
(`CallToolRequestParams::new(name).with_arguments(...)`, the same shape the sibling
`GetPromptRequestParams` type documents):

```rust
mod support;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, object};
use rocket_mem_mcp::pool::Pool;
use rocket_mem_mcp::server::RocketMemMcpServer;

async fn connect_client_and_server(
    pool: Pool,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
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
async fn set_then_get_round_trips_through_the_mcp_tools() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-set-tool-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(
            CallToolRequestParams::new("set")
                .with_arguments(object!({"key": "mcp-key", "value": "mcp-value"})),
        )
        .await
        .expect("set should succeed");

    let get_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("get").with_arguments(object!({"key": "mcp-key"})))
        .await
        .expect("get should succeed");
    assert_ne!(get_result.is_error, Some(true));

    child.kill().ok();
}

#[tokio::test]
async fn get_on_a_missing_key_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-missing-key-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");
    let client = connect_client_and_server(pool).await;

    let get_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get")
                .with_arguments(object!({"key": "this-key-was-never-set"})),
        )
        .await
        .expect("get on a missing key should still succeed as a tool call");
    assert_ne!(get_result.is_error, Some(true));

    child.kill().ok();
}

#[tokio::test]
async fn get_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("get-wrongtype-test.aof");
    let (mut child, addr) = support::spawn_rocket_mem(&aof_path);
    let pool = Pool::connect(&addr).await.expect("pool should connect");

    // Seed a list key directly, bypassing the tool layer — `get` has no way to create one.
    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("RPUSH")
        .arg("a-list-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = connect_client_and_server(pool).await;
    let get_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("get").with_arguments(object!({"key": "a-list-key"})))
        .await
        .expect("call_tool itself should succeed even though the tool reports an error");
    assert_eq!(get_result.is_error, Some(true));
    let text = format!("{:?}", get_result.content);
    assert!(
        text.contains("WRONGTYPE"),
        "expected the real WRONGTYPE message, got: {text}"
    );

    child.kill().ok();
}
```

**If `CallToolRequestParams::new`/`with_arguments` don't compile as written:** this specific
builder shape was inferred from the sibling `GetPromptRequestParams` type's documented methods
(same non-exhaustive pattern), not confirmed directly against `CallToolRequestParams` itself —
run `cargo doc -p rmcp --open` (or check `docs.rs/rmcp/latest/rmcp/model/struct.CallToolRequestParams.html`)
for its actual constructor and adjust the calls above accordingly; nothing else in this task
depends on the exact method names.

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cd rocket-mem-mcp && cargo test --test get_set_tool`
Expected: FAIL — `get`/`set` tools don't exist, so every `call_tool` returns an unknown-tool
error.

- [ ] **Step 3: Implement `get` and `set` in `server.rs`**

Replace the entire contents of `rocket-mem-mcp/src/server.rs` with:

```rust
use redis::AsyncCommands;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::errors::redis_error_to_tool_result;
use crate::pool::Pool;

#[derive(Deserialize, JsonSchema)]
struct GetParams {
    /// The key to read.
    key: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetParams {
    /// The key to write.
    key: String,
    /// The value to store.
    value: String,
}

#[derive(Clone)]
pub struct RocketMemMcpServer {
    pool: Pool,
}

impl RocketMemMcpServer {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[tool_router]
impl RocketMemMcpServer {
    #[tool(description = "Get the string value of a key. A missing key is not an error.")]
    async fn get(
        &self,
        Parameters(GetParams { key }): Parameters<GetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool.connection();
        let value: Result<Option<String>, redis::RedisError> = conn.get(&key).await;
        match value {
            Ok(Some(value)) => Ok(CallToolResult::success(vec![ContentBlock::text(value)])),
            Ok(None) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "(nil)".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set the string value of a key.")]
    async fn set(
        &self,
        Parameters(SetParams { key, value }): Parameters<SetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool.connection();
        let result: Result<(), redis::RedisError> = conn.set(&key, &value).await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "OK".to_string(),
            )])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}

#[tool_handler]
impl ServerHandler for RocketMemMcpServer {}
```

- [ ] **Step 4: Run the test and confirm it passes**

Run: `cd rocket-mem-mcp && cargo test --test get_set_tool`
Expected: PASS — all three tests green.

Then run the full test suite for this crate to confirm nothing else broke:

Run: `cd rocket-mem-mcp && cargo test`
Expected: all tests across `pool.rs`, `server_handshake.rs`, and `get_set_tool.rs` pass.

Finally, lint and format this crate (outside the workspace CI gate, but held to the same bar):

Run: `cd rocket-mem-mcp && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/server.rs rocket-mem-mcp/tests/get_set_tool.rs rocket-mem-mcp/Cargo.lock
git commit -m "Add get/set as the first rocket-mem-mcp tools"
```

---

## Next plan

This plan only delivered the foundation (pool, error mapping, stdio server, `get`/`set`). A lot
of what the spec (`docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md`) calls for
is still not started. Future plans need to cover, in no particular priority order:

- **The rest of the String/Key command family**: `GETSET`, `GETRANGE`, `SETRANGE`, `APPEND`,
  `STRLEN`, `INCR`/`DECR`/`INCRBY`, `MSET`, `MGET`, `MSETNX`, `RENAME`, `RENAMENX`, `TYPE`,
  `RANDOMKEY`, `KEYS`, `SCAN`, `DEL`/`EXISTS`, `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`,
  `TTL`/`PTTL`, `PERSIST`, `MEMORY USAGE`, `OBJECT ENCODING` (per README.md's "Command coverage"
  table), following the exact `get`/`set` pattern this plan established.
- **A `tools/` module split**: split `server.rs`'s growing tool-method list into a `tools/`
  module directory (`tools/string.rs`, `tools/keys.rs`, etc., per the spec's "Tool surface"
  section) once the flat file gets unwieldy — not needed yet at two tools.
- **ACL username/password credential support.** The spec's security model says the target's
  "address and optional ACL username/password" are both meant to come from this crate's own
  config (mirroring rocket-mem's own layered config convention). Today `config.rs`/`pool.rs`
  only carry `target_addr` — there is no way to authenticate against a target that has ACLs
  enabled. This needs a real, dedicated fix (e.g. `Config` fields for `acl_username`/
  `acl_password` plumbed into `Client::open`/`ConnectionManager` setup, with the password sourced
  from env/CLI the same redacted way rocket-mem's own config layer treats secrets) — interpolating
  a raw `--target-addr` string with embedded credentials to work around this today would leak the
  password via `ps`/shell history, so that is explicitly not an acceptable stopgap.
- **Streamable HTTP transport.** The spec explicitly wants "both stdio and Streamable HTTP from
  the start" over the same tool implementation, with only `main.rs`'s startup wiring differing
  (spawn a child process vs. bind an HTTP listener). Only `stdio` exists so far — Streamable HTTP
  is not implemented at all yet, not even behind a flag.
- **The TOML config-file layer.** The spec's config precedent is defaults → file → env → CLI,
  matching rocket-mem's own convention. Today `config.rs` only layers defaults → env → CLI (via
  `clap`'s `env` attribute) — there is no file layer yet.
- **`run_transaction`.** The spec collapses `MULTI`/`EXEC`/`DISCARD` into one
  `run_transaction(commands: [{name, args}])` tool built on `redis::pipe().atomic()` (see the
  spec's "Connection and session model" section) — not implemented yet.
- **`session.rs` and the pub/sub tools.** The spec's one place session state is unavoidable:
  `subscribe`/`poll_messages`/`unsubscribe` need a per-MCP-session `SubscriptionManager` keyed by
  session id, plus session-teardown cleanup, plus the stateless `publish`/`pubsub_*`
  introspection tools. None of `session.rs`, `subscription.rs`, or any pub/sub tool exists yet.
- **Proper `Bytes`/`EX` value typing for string commands.** Today `get`/`set` are `String`-only
  with no expiry option. The spec's own illustrative signature is
  `set(key: String, value: Bytes, ex: Option<u64>)` — a future plan needs to widen `SetParams`
  (and any other string command taking a value) to accept raw bytes rather than only UTF-8
  strings, and add the `EX`/expiry option `SET` already supports at the rocket-mem wire level.

A future implementer should treat this list, not just the String/Key bullet above, as the
remaining v1 scope — silence on an item elsewhere in this plan does not mean it's done.
