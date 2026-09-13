# rocket-mem-mcp: List Family Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the full List command family as MCP tools in a new `tools/list.rs` module, composed
into the existing router alongside the String, Key, and Hash families Plans 2-3 shipped.

**Architecture:** One new file, `src/tools/list.rs`, holding a `#[tool_router(router = list_router,
vis = "pub")]` impl block — the same one-file-per-family pattern established in Plans 2-3.
`server.rs`'s `tool_router()` gains a fourth term: `Self::string_router() + Self::keys_router() +
Self::hash_router() + Self::list_router()`. Every tool issues the real wire command via
`redis::cmd("COMMAND").arg(...)` against the pool's `ConnectionManager`, exactly like every tool
in Plans 1-3.

**Tech Stack:** Rust 2021, `rmcp` 3.3, `redis` 1.7, `tokio`, `schemars`+`serde`. Same crate, same
dependencies as Plans 1-3 — no new ones needed.

**Spec:** `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md` — see its "Command
semantics reference" section, "Plan 4 — List (11 tools)" subsection, and its "`structured_content`
convention" section, which every tool in this plan follows.

## Global Constraints

- One MCP tool per rocket-mem command (never a generic multi-op dispatch tool).
- Every tool call goes through the pool's `ConnectionManager` via `redis::cmd(...)` — never a
  synthesized substitute command; the MCP tool and the wire command it fronts must match exactly.
- Tests run against a real, locally-spawned rocket-mem instance (`tests/support/mod.rs`'s
  `spawn_rocket_mem`) — never a mock.
- String values stay `String` (UTF-8), matching every tool in Plans 1-3.
- Router composition uses `Self::<family>_router()` — never `crate::tools::<family>::...`.
- Per the spec's `structured_content` convention: every tool returning a scalar or collection sets
  `structured_content`; a tool whose only reply is a bare `"OK"` (no scalar/collection payload)
  does not need to — matching Plan 2's `set`/`mset`/`rename` precedent. This plan's `lset`/`ltrim`
  fall into that second category; every other tool in this plan sets `structured_content`. Where a
  "missing" case applies, its shape stays consistent within this family (every not-found case uses
  `{"found": bool, "value": ...}` with `value` always present, `null` when not found — matching
  Plan 3's `hget`, not Plan 1's `get`, per the spec's explicit note that both are valid but a
  family should pick one shape and hold it).

## Command semantics reference (verified against current source, 2026-09-13)

Read directly from `crates/engine/src/commands/list.rs` and
`crates/server/src/dispatcher.rs:601-737` while writing this plan — the design spec's existing
"Plan 4 — List" subsection was cross-checked against this and is accurate, but two details needed
sharpening:

- **`LPUSH`/`RPUSH` are variadic** (`dispatcher.rs:601-614`, `rest[1..]` collected as a
  `Vec<Bytes>`): both tools accept an array of values and issue one command with all of them,
  exactly like Plan 2's `mset`. **`LPUSH` order matters**: pushing multiple values prepends each
  in argument order, so the *last* argument ends up at the front of the list (verified by
  `commands/list.rs`'s own `lpush_with_multiple_values_prepends_each_so_the_last_argument_ends_up_first`
  test) — worth stating explicitly in the tool description since it's easy to assume the opposite.
  An empty `values` array will be rejected by the server with a wrong-number-of-arguments error
  (the wire command requires at least one value; the engine-level "empty values is a no-op"
  behavior documented in `list.rs`'s own tests is reachable only by calling the engine function
  directly, never through the dispatcher this crate talks to).
- **`LPOP`/`RPOP` do not support an optional `count`** (`dispatcher.rs:615-630`,
  `require_args!(rest, 1, ...)` — exactly one arg, the key). Real Redis's `LPOP key [count]` form
  has no equivalent here: these tools pop exactly one element or none, never a batch.
- **`LINSERT`'s `before`/`after` choice is a wire-level keyword, not a plain flag** — this
  corrects the spec's "plain `bool` at the engine level, not `BEFORE`/`AFTER` keywords" framing,
  which is true for the *engine* function (`commands::list::linsert`'s `before: bool` parameter)
  but not for the *wire command* this crate actually issues: `dispatcher.rs:717-726` requires the
  literal token `"BEFORE"` or `"AFTER"` as the second argument (case-insensitive; anything else is
  a syntax error). The MCP tool takes a `before: bool` parameter (the friendlier shape) and
  translates it to the correct keyword when building the `redis::cmd("LINSERT")` call — the
  translation lives in the tool, the agent never sees the keyword.
- **`LSET` distinguishes `NoSuchKey` (key missing entirely) from `IndexOutOfRange` (bad index on
  an existing list)** as two different tool-level errors (`commands/list.rs:206-228`) — confirmed
  still accurate. `WRONGTYPE` applies on top of both for a non-list key.
- **`LTRIM` on a missing key is a silent no-op success**, not an error (`commands/list.rs:236-241`,
  `dispatcher.rs:685-701` returns `Simple("OK")` either way) — it must not fabricate a phantom
  empty list. Trimming to an empty range deletes the key entirely (confirmed by
  `ltrim_to_empty_range_deletes_the_key`).
- **`LREM`'s `count` sign controls scan direction**: positive removes up to `count` matches
  starting from the head, negative removes up to `-count` starting from the tail, zero removes
  every match (`commands/list.rs:263-318`). Missing key returns `0` removed, not an error.
- **`LINSERT`'s reply is a three-way sentinel**: the new list length on success, `-1` if the pivot
  value isn't found anywhere in the list, `0` if the key doesn't exist at all
  (`commands/list.rs:320-341`) — all three come back as a plain `Integer` on the wire, so the tool
  must document the sentinel meanings rather than trying to split them into separate fields (there
  is no way to tell "length happens to be 0" apart from "key missing" from the reply alone, but
  that ambiguity only matters if the pivot could ever be found in an empty list, which is
  impossible — a length of exactly `0` can only mean "key missing").
- **`LRANGE`/`LINDEX` on a missing key return empty/not-found, never an error**
  (`commands/list.rs:167-204`); `LINDEX` on an out-of-range index (on an existing list) also
  returns not-found, not an error — the two cases are indistinguishable in the reply, matching
  real Redis.

## Testing pattern

Every test in this plan drives tools through a real MCP client via `support::connect_client_and_server`
against a real spawned rocket-mem instance (`support::spawn_rocket_mem`) — never a mock, matching
every prior plan.

---

## File Structure

```
rocket-mem-mcp/
├── src/
│   ├── server.rs        — modify: tool_router() gains + Self::list_router()
│   └── tools/
│       ├── mod.rs        — modify: add `pub mod list;`
│       └── list.rs        — new: 11 tools (all of the List family)
└── tests/
    └── list_tools.rs      — new
```

---

### Task 1: Push/pop/read tools — `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LLEN`, `LRANGE`, `LINDEX`

**Files:**
- Create: `rocket-mem-mcp/src/tools/list.rs`
- Modify: `rocket-mem-mcp/src/tools/mod.rs` (add `pub mod list;`)
- Modify: `rocket-mem-mcp/src/server.rs` (`tool_router()` gains `+ Self::list_router()`)
- Test: `rocket-mem-mcp/tests/list_tools.rs` (created here, extended in Task 2)

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result` (Plan 1), `RocketMemMcpServer::pool()` (Plan 2
  Task 1), `tests/support/mod.rs`'s `spawn_rocket_mem`/`connect_client_and_server`.
- Produces: `tools::list::list_router() -> ToolRouter<RocketMemMcpServer>` (associated fn via
  `#[tool_router(router = list_router, vis = "pub")]` — same shape as `string_router`/
  `keys_router`/`hash_router`). Seven tools: `lpush`, `rpush`, `lpop`, `rpop`, `llen`, `lrange`,
  `lindex`. `ListRangeParams { key: String, start: i64, stop: i64 }` (Task 1 defines this; Task 2
  reuses it for `ltrim`, which takes the identical shape).

- [ ] **Step 1: Write the failing tests**

Create `rocket-mem-mcp/tests/list_tools.rs`:

```rust
mod support;

use rmcp::model::{object, CallToolRequestParams};
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn rpush_lpush_and_lrange_order_values_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("push.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let rpush_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();
    assert_eq!(rpush_result.structured_content.unwrap()["length"], 3);

    let lpush_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpush").with_arguments(object!({
            "key": "l", "values": ["x", "y", "z"]
        })))
        .await
        .unwrap();
    assert_eq!(lpush_result.structured_content.unwrap()["length"], 6);

    let lrange_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lrange")
                .with_arguments(object!({"key": "l", "start": 0, "stop": -1})),
        )
        .await
        .unwrap();
    let values = lrange_result.structured_content.unwrap()["values"].clone();
    let values: Vec<String> = values
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    // LPUSH with multiple values prepends each in argument order, so the *last* argument
    // ("z") ends up first.
    assert_eq!(values, vec!["z", "y", "x", "a", "b", "c"]);

    guard.kill();
}

#[tokio::test]
async fn lpop_and_rpop_remove_from_the_correct_end_and_report_found() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("pop.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let lpop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let structured = lpop_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "a");

    let rpop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let structured = rpop_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "c");

    // Only "b" is left. Pop it, then pop again on the now-missing key.
    client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    let empty_pop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lpop").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    assert_ne!(empty_pop_result.is_error, Some(true));
    let structured = empty_pop_result.structured_content.unwrap();
    assert_eq!(structured["found"], false);
    assert!(structured["value"].is_null());

    guard.kill();
}

#[tokio::test]
async fn llen_reports_length_and_zero_for_a_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("llen.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b"]
        })))
        .await
        .unwrap();
    let llen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("llen").with_arguments(object!({"key": "l"})))
        .await
        .unwrap();
    assert_eq!(llen_result.structured_content.unwrap()["length"], 2);

    let missing_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("llen").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_eq!(missing_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}

#[tokio::test]
async fn lindex_supports_negative_indices_and_reports_not_found_out_of_range() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lindex.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let last_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lindex").with_arguments(object!({"key": "l", "index": -1})),
        )
        .await
        .unwrap();
    let structured = last_result.structured_content.unwrap();
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "c");

    let out_of_range_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("lindex").with_arguments(object!({"key": "l", "index": 99})),
        )
        .await
        .unwrap();
    assert_ne!(out_of_range_result.is_error, Some(true));
    assert_eq!(out_of_range_result.structured_content.unwrap()["found"], false);

    guard.kill();
}

#[tokio::test]
async fn rpush_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("wrongtype.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();

    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("string-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = support::connect_client_and_server(pool).await;
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "string-key", "values": ["y"]
        })))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let text = format!("{:?}", result.content);
    assert!(text.contains("WRONGTYPE"), "expected WRONGTYPE, got: {text}");

    guard.kill();
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test list_tools`
Expected: compile error — `lpush`/`rpush`/`lpop`/`rpop`/`llen`/`lrange`/`lindex` tools don't exist
yet.

- [ ] **Step 3: Implement `tools/list.rs` and wire it into `server.rs`**

Create `rocket-mem-mcp/src/tools/list.rs`:

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
pub struct ListValuesParams {
    key: String,
    /// One or more values to push. This is variadic at the wire level — all values are sent in
    /// a single command. An empty array is rejected by the server with a wrong-number-of-
    /// arguments error (at least one value is required per call).
    values: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListKeyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListRangeParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element). Inclusive on both ends.
    start: i64,
    stop: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct LIndexParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element). Out-of-range returns
    /// found: false, not an error.
    index: i64,
}

#[tool_router(router = list_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(description = "Prepend one or more values to the head of a list, creating it if \
        missing. With multiple values, each is prepended in argument order, so the *last* \
        argument ends up at the front of the list. Returns the list's new length.")]
    async fn lpush(
        &self,
        Parameters(ListValuesParams { key, values }): Parameters<ListValuesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("LPUSH");
        command.arg(&key);
        for value in &values {
            command.arg(value);
        }
        let result: Result<usize, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Append one or more values to the tail of a list, creating it if \
        missing, in the order given. Returns the list's new length.")]
    async fn rpush(
        &self,
        Parameters(ListValuesParams { key, values }): Parameters<ListValuesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("RPUSH");
        command.arg(&key);
        for value in &values {
            command.arg(value);
        }
        let result: Result<usize, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Remove and return the first element of a list. A missing or empty \
        list reports found: false, not an error. There is no optional count — this always \
        pops at most one element.")]
    async fn lpop(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("LPOP").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(value) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    value.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "found": value.is_some(), "value": value }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Remove and return the last element of a list. A missing or empty \
        list reports found: false, not an error. There is no optional count — this always \
        pops at most one element.")]
    async fn rpop(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> =
            redis::cmd("RPOP").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(value) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    value.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "found": value.is_some(), "value": value }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the number of elements in a list. A missing key returns 0, not \
        an error.")]
    async fn llen(
        &self,
        Parameters(ListKeyParams { key }): Parameters<ListKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("LLEN").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get a range of elements from a list, inclusive on both ends. \
        Negative indices count from the end (-1 is the last element). A missing key returns an \
        empty list, not an error.")]
    async fn lrange(
        &self,
        Parameters(ListRangeParams { key, start, stop }): Parameters<ListRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> = redis::cmd("LRANGE")
            .arg(&key)
            .arg(start)
            .arg(stop)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(values) => {
                let text = values.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "values": values }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the element at an index in a list. Negative indices count from \
        the end (-1 is the last element). A missing key or an out-of-range index both report \
        found: false, not an error — the reply cannot distinguish the two cases.")]
    async fn lindex(
        &self,
        Parameters(LIndexParams { key, index }): Parameters<LIndexParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("LINDEX")
            .arg(&key)
            .arg(index)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(value) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    value.clone().unwrap_or_else(|| "(nil)".to_string()),
                )]);
                r.structured_content = Some(json!({ "found": value.is_some(), "value": value }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
```

Create `rocket-mem-mcp/src/tools/mod.rs` (modify — add the new module alongside the existing
three):

```rust
pub mod hash;
pub mod keys;
pub mod list;
pub mod string;
```

Modify `rocket-mem-mcp/src/server.rs`'s router composition:

```rust
impl RocketMemMcpServer {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::string_router() + Self::keys_router() + Self::hash_router() + Self::list_router()
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test list_tools`
Expected: PASS — all 5 tests green.

Run the full suite to confirm the 4-router composition still works for every existing tool too:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/list.rs rocket-mem-mcp/src/tools/mod.rs \
  rocket-mem-mcp/src/server.rs rocket-mem-mcp/tests/list_tools.rs
git commit -m "Add rocket-mem-mcp push/pop/read list tools"
```

---

### Task 2: Mutation tools — `LSET`, `LTRIM`, `LREM`, `LINSERT`

**Files:**
- Modify: `rocket-mem-mcp/src/tools/list.rs` (add 4 tools to the existing `list_router` impl
  block)
- Test: `rocket-mem-mcp/tests/list_tools.rs` (extend)

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result`, `RocketMemMcpServer::pool()`, the
  `ListRangeParams` struct Task 1 already defined (reused for `ltrim`, not redefined).
- Produces: 4 more tools — `lset`, `ltrim`, `lrem`, `linsert` — added to the same `list_router`
  Task 1 created. All 11 List-family tools exist after this task.

- [ ] **Step 1: Write the failing tests**

Add to `rocket-mem-mcp/tests/list_tools.rs` (append these test functions; keep the existing 5
tests from Task 1 unchanged):

```rust
#[tokio::test]
async fn lset_updates_an_element_and_distinguishes_missing_key_from_bad_index() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c"]
        })))
        .await
        .unwrap();

    let ok_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "l", "index": 1, "value": "z"
        })))
        .await
        .unwrap();
    assert_ne!(ok_result.is_error, Some(true));
    let mut conn = pool.connection();
    let updated: String = redis::cmd("LINDEX")
        .arg("l")
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(updated, "z");

    let bad_index_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "l", "index": 99, "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(bad_index_result.is_error, Some(true));
    let bad_index_text = format!("{:?}", bad_index_result.content);

    let missing_key_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lset").with_arguments(object!({
            "key": "does-not-exist", "index": 0, "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(missing_key_result.is_error, Some(true));
    let missing_key_text = format!("{:?}", missing_key_result.content);

    // The two error messages must be genuinely distinct, not the same generic text.
    assert_ne!(bad_index_text, missing_key_text);

    guard.kill();
}

#[tokio::test]
async fn ltrim_keeps_only_the_requested_range_and_is_a_noop_on_a_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("ltrim.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "b", "c", "d"]
        })))
        .await
        .unwrap();
    let trim_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ltrim").with_arguments(object!({
            "key": "l", "start": 1, "stop": 2
        })))
        .await
        .unwrap();
    assert_ne!(trim_result.is_error, Some(true));
    let mut conn = pool.connection();
    let remaining: Vec<String> = redis::cmd("LRANGE")
        .arg("l")
        .arg(0)
        .arg(-1)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(remaining, vec!["b".to_string(), "c".to_string()]);

    let noop_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("ltrim").with_arguments(object!({
            "key": "never-existed", "start": 0, "stop": -1
        })))
        .await
        .unwrap();
    assert_ne!(noop_result.is_error, Some(true));
    let exists: i64 = redis::cmd("EXISTS")
        .arg("never-existed")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0);

    guard.kill();
}

#[tokio::test]
async fn lrem_removes_by_count_direction() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("lrem.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "x", "b", "x", "c"]
        })))
        .await
        .unwrap();

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("lrem").with_arguments(object!({
            "key": "l", "count": 0, "value": "x"
        })))
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["removed"], 2);

    guard.kill();
}

#[tokio::test]
async fn linsert_inserts_relative_to_a_pivot_and_reports_sentinels() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("linsert.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("rpush").with_arguments(object!({
            "key": "l", "values": ["a", "c"]
        })))
        .await
        .unwrap();

    let insert_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("linsert").with_arguments(object!({
            "key": "l", "before": true, "pivot": "c", "value": "b"
        })))
        .await
        .unwrap();
    assert_eq!(insert_result.structured_content.unwrap()["length"], 3);

    let missing_pivot_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("linsert").with_arguments(object!({
            "key": "l", "before": true, "pivot": "not-there", "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(missing_pivot_result.structured_content.unwrap()["length"], -1);

    let missing_key_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("linsert").with_arguments(object!({
            "key": "does-not-exist", "before": true, "pivot": "p", "value": "z"
        })))
        .await
        .unwrap();
    assert_eq!(missing_key_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test list_tools`
Expected: FAIL — `lset`/`ltrim`/`lrem`/`linsert` don't exist yet (the 5 tests from Task 1 still
pass).

- [ ] **Step 3: Add the 4 remaining tools to `tools/list.rs`**

Add these parameter structs above the `#[tool_router(...)]` block (alongside Task 1's structs):

```rust
#[derive(Deserialize, JsonSchema)]
pub struct LSetParams {
    key: String,
    /// Negative indices count from the end (-1 is the last element).
    index: i64,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LRemParams {
    key: String,
    /// A positive count removes up to that many matches starting from the head; negative
    /// removes up to -count matches starting from the tail; 0 removes every match.
    count: i64,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LInsertParams {
    key: String,
    /// true inserts before the pivot element, false inserts after it.
    before: bool,
    pivot: String,
    value: String,
}
```

Add these methods inside the existing `impl RocketMemMcpServer` block under
`#[tool_router(router = list_router, vis = "pub")]`, after `lindex`:

```rust
    #[tool(description = "Set the value at an index in a list, replacing what's there. \
        Negative indices count from the end (-1 is the last element). Errors NoSuchKey if the \
        key doesn't exist at all, or IndexOutOfRange if the key exists but the index is out of \
        bounds — these are two distinct tool-level errors, not one generic failure.")]
    async fn lset(
        &self,
        Parameters(LSetParams { key, index, value }): Parameters<LSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("LSET")
            .arg(&key)
            .arg(index)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("OK".to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Trim a list so only the given index range remains, discarding \
        everything else. Negative indices count from the end (-1 is the last element). A \
        missing key is a silent no-op, not an error. Trimming to an empty range deletes the \
        key entirely.")]
    async fn ltrim(
        &self,
        Parameters(ListRangeParams { key, start, stop }): Parameters<ListRangeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<(), redis::RedisError> = redis::cmd("LTRIM")
            .arg(&key)
            .arg(start)
            .arg(stop)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("OK".to_string())])),
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Remove occurrences of a value from a list. A positive count removes \
        up to that many matches starting from the head; a negative count removes up to its \
        absolute value starting from the tail; 0 removes every match. Returns the count \
        actually removed. A missing key returns 0, not an error.")]
    async fn lrem(
        &self,
        Parameters(LRemParams { key, count, value }): Parameters<LRemParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("LREM")
            .arg(&key)
            .arg(count)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(removed) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(removed.to_string())]);
                r.structured_content = Some(json!({ "removed": removed }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Insert a value immediately before or after the first occurrence of \
        a pivot value in a list. Returns the list's new length on success, -1 if the pivot \
        value isn't found anywhere in the list, or 0 if the key doesn't exist at all — a \
        length of exactly 0 can only mean the key is missing, since a pivot can never be found \
        in an empty list.")]
    async fn linsert(
        &self,
        Parameters(LInsertParams {
            key,
            before,
            pivot,
            value,
        }): Parameters<LInsertParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let keyword = if before { "BEFORE" } else { "AFTER" };
        let result: Result<i64, redis::RedisError> = redis::cmd("LINSERT")
            .arg(&key)
            .arg(keyword)
            .arg(&pivot)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test list_tools`
Expected: PASS — all 9 tests green (5 from Task 1 + 4 new).

Run the full suite, then lint:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean. This confirms all 11 List tools plus every tool from Plans 1-3 (42
more, 53 total) work together through the 4-router composition.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/list.rs rocket-mem-mcp/tests/list_tools.rs
git commit -m "Add remaining rocket-mem-mcp list tools (lset/ltrim/lrem/linsert)"
```

---

## Next plan

Per `project_rocket_mem_mcp_roadmap` (memory) and the design spec's "Command semantics reference,"
**Plan 5: Set family (14 tools)** is next — `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`,
`SINTER`, `SUNION`, `SDIFF`, `SINTERSTORE`, `SUNIONSTORE`, `SDIFFSTORE`, `SPOP`, `SRANDMEMBER`,
`SSCAN` — 14 tools, `SSCAN` confirmed implemented (same correction Plan 3 made for `HSCAN`; don't
trust an older "13 tools, no SSCAN" framing anywhere it might still linger). Notable grounding
already recorded for that plan: `SADD`/`SREM` take variadic members; `SINTER`/`SUNION`/`SDIFF` and
their `*STORE` variants take a variadic key list (not a fixed two) plus, for `*STORE`, a
destination key; `SPOP`/`SRANDMEMBER` do **not** support an optional `count` at the engine level —
single-member only, despite real Redis's `[count]` form (the same shape of deviation this plan
found for `LPOP`/`RPOP`'s missing `count` support — worth re-verifying `SSCAN`'s exact dispatcher
line number when Plan 5 is actually written, since line numbers drift).

**Housekeeping carried forward from Plan 3, not yet done:** a real, pre-existing test-harness race
in `tests/support/mod.rs` (`spawn_rocket_mem`/`Pool::connect` occasionally fails "Connection
refused" under full-suite process load) was flagged by Plan 3's final review as worth fixing
before this plan's test file adds a 4th family's worth of spawned servers. If it hasn't been
addressed by the time Plan 5 starts, it should be — the race gets more likely to bite with every
new test file, in a crate CI doesn't gate. Also still open: a recommended (not yet added)
tool-surface assertion test (`list_tools` count + name-uniqueness check), since `ToolRouter`'s `+`
composition silently overwrites a same-named tool with no diagnostic.
