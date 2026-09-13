# rocket-mem-mcp: Hash Family Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the full Hash command family as MCP tools in a new `tools/hash.rs` module, composed
into the existing router alongside the String and Key families Plan 2 shipped.

**Architecture:** One new file, `src/tools/hash.rs`, holding a `#[tool_router(router =
hash_router, vis = "pub")]` impl block — the same one-file-per-family pattern Plan 2 established
for `tools/string.rs` and `tools/keys.rs`. `server.rs`'s `tool_router()` gains a third term:
`Self::string_router() + Self::keys_router() + Self::hash_router()`. Every tool issues the real
wire command via `redis::cmd("COMMAND").arg(...)` against the pool's `ConnectionManager`, exactly
like every tool in Plan 1 and Plan 2.

**Tech Stack:** Rust 2021, `rmcp` 3.3, `redis` 1.7, `tokio`, `schemars`+`serde`. Same crate, same
dependencies as Plans 1-2 — no new ones needed.

**Spec:** `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md` — see its "Command
semantics reference" section, "Plan 3 (Hash)" subsection, for background. **Correction to that
subsection, found while writing this plan:** it says "`HSCAN` is NOT implemented — drop from Plan
3's scope." This is now wrong. `HSCAN` **is** implemented (`crates/server/src/dispatcher.rs:505-534`,
and listed in `README.md`'s "Command coverage" table). It's a real, if degenerate, implementation:
since a hash already lives fully in memory (unlike the keyspace, which is chunked one shard per
`SCAN` call), `HSCAN` internally just calls the same `hgetall` every time, filters by `MATCH`, and
always reports cursor `"0"` (scan complete) in a single page — but it is a real, working command
an agent can call, so this plan includes it. **Tool count is 12, not 11.** (The spec's own
document will be corrected to match after this plan ships — see "Next plan" below.)

## Global Constraints

- One MCP tool per rocket-mem command (never a generic multi-op dispatch tool).
- Every tool call goes through the pool's `ConnectionManager` via `redis::cmd(...)` — never a
  synthesized substitute command; the MCP tool and the wire command it fronts must match exactly.
- Tests run against a real, locally-spawned rocket-mem instance (`tests/support/mod.rs`'s
  `spawn_rocket_mem`) — never a mock.
- String values stay `String` (UTF-8), matching every tool in Plans 1-2 — raw `Bytes` widening is
  Plan 13's job, not this one's.
- Router composition uses `Self::<family>_router()` (an associated fn `#[tool_router(router =
  <name>, ...)]` generates on `RocketMemMcpServer` itself) — never `crate::tools::<family>::...`,
  which does not compile against the installed `rmcp` 3.3 (confirmed during Plan 2).
- Every scalar/collection-returning tool sets `structured_content` (Plan 2's final review flagged
  this as inconsistent — 7 of its 30 tools set it, 23 didn't, with no stated rule; this plan
  settles the convention by doing it for all 12 of its own tools, matching `get`'s original
  precedent from Plan 1).

---

## Before you start

Build the real `rocket-mem` binary once, from the main repo, if you haven't already for Plans 1-2:

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build -p rocket-mem
```

## File Structure

```
rocket-mem-mcp/
├── src/
│   ├── server.rs        — modify: tool_router() gains + Self::hash_router()
│   └── tools/
│       ├── mod.rs        — modify: add `pub mod hash;`
│       └── hash.rs        — new: 12 tools (all of the Hash family)
└── tests/
    └── hash_tools.rs      — new
```

## Command semantics reference (verified against current source, 2026-09-13)

Read directly from `crates/engine/src/commands/hash.rs` and
`crates/server/src/dispatcher.rs:444-600` while writing this plan:

- **`HSET` is variadic at the wire level** (`dispatcher.rs:444-463`): the dispatcher loops over
  `field, value` pairs from a single `HSET key f1 v1 f2 v2 ...` command, summing how many were
  newly added (not overwritten) into the final `Integer` reply. This plan's `hset` tool mirrors
  that — it accepts an array of pairs and issues one `HSET` command with all of them, exactly like
  Plan 2's `mset` tool did for `MSET`. **Contrast with `HSETNX`**, which is NOT variadic
  (`dispatcher.rs:592-599` requires exactly 3 args: `key field value`) — `hsetnx`'s tool takes a
  single field/value pair, not an array.
- **`HDEL`/`HMGET` are variadic** (`dispatcher.rs:487-493`, `578-591`): both take `key` plus one
  or more fields in a single call. Tools take a `fields: Vec<String>` array.
- **Missing key ≠ error, everywhere in this family.** `HGET`/`HEXISTS`/`HGETALL`/`HLEN`/`HKEYS`/
  `HVALS`/`HMGET`/`HDEL` on a missing key all return an empty/zero/null result, never an error
  (`hash.rs:45-97,147-165`). Only a WRONGTYPE key (the key exists and holds a non-hash value)
  errors.
- **`HINCRBY` has the same two extra error variants as Plan 2's `incr_by`**: `NotAnInteger` (the
  field's current value doesn't parse as an i64) and `IncrementOverflow` (the result would
  overflow i64) — both distinct tool-level errors from `WRONGTYPE`, both already exercised end to
  end via `redis_error_to_tool_result`'s existing passthrough (no special-casing needed, per Plan
  2's `incr_by` precedent).
- **`HGETALL` reply shape**: a flat, interleaved `[field1, value1, field2, value2, ...]` array on
  the wire (`dispatcher.rs:494-503`) — the `redis` crate's `FromRedisValue` impl for
  `HashMap<String, String>` decodes this directly with no extra work, the same way it would decode
  a real Redis `HGETALL` reply.
- **`HSCAN` reply shape**: `[cursor_bulk, [field1, value1, field2, value2, ...]]`
  (`dispatcher.rs:505-534`) — same two-element structure as Plan 2's top-level `SCAN`, but the
  second element decodes as a `HashMap<String, String>` instead of a flat key list. Supports
  `MATCH` (filters fields by glob); does **not** support `TYPE` (real Redis's `HSCAN` doesn't
  either — `ScanOptions::parse`'s `allow_type` is `false` for `HSCAN`, confirmed at
  `dispatcher.rs:518`); `COUNT` is parsed-but-ignored, same rationale as Plan 2's `scan` tool
  (there's no page to size — one call always returns everything), so this tool doesn't expose it.
  Cursor is always `"0"` after one call — there is no second page to fetch.

## Testing pattern

Every test in this plan drives tools through a real MCP client via `support::connect_client_and_server`
(from `tests/support/mod.rs`, already `pub` since Plan 2's fix round) against a real spawned
rocket-mem instance (`support::spawn_rocket_mem`) — never a mock, matching every prior plan.

---

### Task 1: Router scaffold + core hash tools — `HSET`, `HGET`, `HDEL`, `HEXISTS`, `HSETNX`

**Files:**
- Create: `rocket-mem-mcp/src/tools/hash.rs`
- Modify: `rocket-mem-mcp/src/tools/mod.rs` (add `pub mod hash;`)
- Modify: `rocket-mem-mcp/src/server.rs` (`tool_router()` gains `+ Self::hash_router()`)
- Test: `rocket-mem-mcp/tests/hash_tools.rs` (created here, extended in Task 2)

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result` (Plan 1), `RocketMemMcpServer::pool()` (Plan 2
  Task 1), `tests/support/mod.rs`'s `spawn_rocket_mem`/`connect_client_and_server` (Plan 1/2).
- Produces: `tools::hash::hash_router() -> ToolRouter<RocketMemMcpServer>` (associated fn via the
  `#[tool_router(router = hash_router, vis = "pub")]` macro — same shape as `string_router`/
  `keys_router`). Five tools: `hset`, `hget`, `hdel`, `hexists`, `hsetnx`.

- [ ] **Step 1: Write the failing tests**

Create `rocket-mem-mcp/tests/hash_tools.rs`:

```rust
mod support;

use rmcp::model::{object, CallToolRequestParams};
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn hset_hget_hexists_and_hdel_round_trip_a_field() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hset.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hset_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "f1", "value": "v1"}, {"field": "f2", "value": "v2"}]
        })))
        .await
        .unwrap();
    assert_ne!(hset_result.is_error, Some(true));
    assert!(format!("{:?}", hset_result.content).contains('2'));

    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget")
                .with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    let structured = hget_result
        .structured_content
        .expect("hget must return structured_content");
    assert_eq!(structured["found"], true);
    assert_eq!(structured["value"], "v1");

    let hexists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    assert_eq!(
        hexists_result.structured_content.unwrap()["exists"],
        true
    );

    let hdel_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hdel")
                .with_arguments(object!({"key": "h", "fields": ["f1", "missing"]})),
        )
        .await
        .unwrap();
    assert_eq!(hdel_result.structured_content.unwrap()["removed"], 1);

    let hexists_after_delete = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "h", "field": "f1"})),
        )
        .await
        .unwrap();
    assert_eq!(
        hexists_after_delete.structured_content.unwrap()["exists"],
        false
    );

    guard.kill();
}

#[tokio::test]
async fn hget_and_hexists_on_a_missing_key_are_not_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hget-missing.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget")
                .with_arguments(object!({"key": "never-created", "field": "f"})),
        )
        .await
        .unwrap();
    assert_ne!(hget_result.is_error, Some(true));
    assert_eq!(hget_result.structured_content.unwrap()["found"], false);

    let hexists_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hexists")
                .with_arguments(object!({"key": "never-created", "field": "f"})),
        )
        .await
        .unwrap();
    assert_eq!(
        hexists_result.structured_content.unwrap()["exists"],
        false
    );

    guard.kill();
}

#[tokio::test]
async fn hget_on_a_wrongtype_key_surfaces_the_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hget-wrongtype.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();

    let mut raw_conn = pool.connection();
    let _: () = redis::cmd("SET")
        .arg("string-key")
        .arg("x")
        .query_async(&mut raw_conn)
        .await
        .unwrap();

    let client = support::connect_client_and_server(pool).await;
    let hget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hget")
                .with_arguments(object!({"key": "string-key", "field": "f"})),
        )
        .await
        .unwrap();
    assert_eq!(hget_result.is_error, Some(true));
    let text = format!("{:?}", hget_result.content);
    assert!(text.contains("WRONGTYPE"), "expected WRONGTYPE, got: {text}");

    guard.kill();
}

#[tokio::test]
async fn hsetnx_sets_only_when_the_field_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hsetnx.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool.clone()).await;

    let first = client
        .peer()
        .call_tool(CallToolRequestParams::new("hsetnx").with_arguments(object!({
            "key": "h", "field": "f", "value": "first"
        })))
        .await
        .unwrap();
    assert_eq!(first.structured_content.unwrap()["applied"], true);

    let second = client
        .peer()
        .call_tool(CallToolRequestParams::new("hsetnx").with_arguments(object!({
            "key": "h", "field": "f", "value": "second"
        })))
        .await
        .unwrap();
    assert_eq!(second.structured_content.unwrap()["applied"], false);

    let mut conn = pool.connection();
    let value: String = redis::cmd("HGET")
        .arg("h")
        .arg("f")
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(value, "first");

    guard.kill();
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test hash_tools`
Expected: compile error — `hset`/`hget`/`hdel`/`hexists`/`hsetnx` tools don't exist yet.

- [ ] **Step 3: Implement `tools/hash.rs` and wire it into `server.rs`**

Create `rocket-mem-mcp/src/tools/hash.rs`:

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
pub struct HashFieldValuePair {
    field: String,
    value: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HSetParams {
    key: String,
    /// One or more field/value pairs to set. HSET is variadic at the wire level — all pairs are
    /// sent in a single HSET command.
    pairs: Vec<HashFieldValuePair>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HashFieldParams {
    key: String,
    field: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HashFieldsParams {
    key: String,
    fields: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HSetNxParams {
    key: String,
    /// HSETNX (unlike HSET) takes exactly one field/value pair — it is not variadic.
    field: String,
    value: String,
}

#[tool_router(router = hash_router, vis = "pub")]
impl RocketMemMcpServer {
    #[tool(description = "Set one or more field/value pairs in a hash, creating the hash if \
        missing. Returns the count of fields that were newly added (fields that already existed \
        were overwritten but not counted).")]
    async fn hset(
        &self,
        Parameters(HSetParams { key, pairs }): Parameters<HSetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HSET");
        command.arg(&key);
        for pair in &pairs {
            command.arg(&pair.field).arg(&pair.value);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(added) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(added.to_string())]);
                r.structured_content = Some(json!({ "added": added }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the value of a field in a hash. A missing key or a missing field \
        is not an error — both come back as found: false.")]
    async fn hget(
        &self,
        Parameters(HashFieldParams { key, field }): Parameters<HashFieldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Option<String>, redis::RedisError> = redis::cmd("HGET")
            .arg(&key)
            .arg(&field)
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

    #[tool(description = "Delete one or more fields from a hash. Returns the count of fields \
        that actually existed and were removed — not the count of fields given.")]
    async fn hdel(
        &self,
        Parameters(HashFieldsParams { key, fields }): Parameters<HashFieldsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HDEL");
        command.arg(&key);
        for field in &fields {
            command.arg(field);
        }
        let result: Result<i64, redis::RedisError> = command.query_async(&mut conn).await;
        match result {
            Ok(removed) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(removed.to_string())]);
                r.structured_content = Some(json!({ "removed": removed }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Check whether a field exists in a hash. A missing key reports false, \
        not an error.")]
    async fn hexists(
        &self,
        Parameters(HashFieldParams { key, field }): Parameters<HashFieldParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("HEXISTS")
            .arg(&key)
            .arg(&field)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(exists) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    if exists { "1" } else { "0" }.to_string(),
                )]);
                r.structured_content = Some(json!({ "exists": exists }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Set a hash field's value only if the field does not already exist. \
        Unlike hset, this takes exactly one field/value pair. Returns false (not an error) if \
        the field already existed.")]
    async fn hsetnx(
        &self,
        Parameters(HSetNxParams { key, field, value }): Parameters<HSetNxParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<bool, redis::RedisError> = redis::cmd("HSETNX")
            .arg(&key)
            .arg(&field)
            .arg(&value)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(applied) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(
                    if applied { "1" } else { "0" }.to_string(),
                )]);
                r.structured_content = Some(json!({ "applied": applied }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
}
```

Create `rocket-mem-mcp/src/tools/mod.rs` (modify — add the new module alongside the existing two):

```rust
pub mod hash;
pub mod keys;
pub mod string;
```

Modify `rocket-mem-mcp/src/server.rs`'s router composition:

```rust
impl RocketMemMcpServer {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::string_router() + Self::keys_router() + Self::hash_router()
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test hash_tools`
Expected: PASS — all 4 tests green.

Run the full suite to confirm the 3-router composition still works for every existing tool too:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/hash.rs rocket-mem-mcp/src/tools/mod.rs \
  rocket-mem-mcp/src/server.rs rocket-mem-mcp/tests/hash_tools.rs
git commit -m "Add rocket-mem-mcp core hash tools (hset/hget/hdel/hexists/hsetnx)"
```

---

### Task 2: Remaining hash tools — `HGETALL`, `HLEN`, `HINCRBY`, `HKEYS`, `HVALS`, `HMGET`, `HSCAN`

**Files:**
- Modify: `rocket-mem-mcp/src/tools/hash.rs` (add 7 tools to the existing `hash_router` impl block)
- Test: `rocket-mem-mcp/tests/hash_tools.rs` (extend)

**Interfaces:**
- Consumes: `errors::redis_error_to_tool_result`, `RocketMemMcpServer::pool()`, the
  `HashFieldsParams`/`HashFieldParams` structs Task 1 already defined (reused, not redefined).
- Produces: 7 more tools — `hgetall`, `hlen`, `hincrby`, `hkeys`, `hvals`, `hmget`, `hscan` — added
  to the same `hash_router` Task 1 created. All 12 Hash-family tools exist after this task.

- [ ] **Step 1: Write the failing tests**

Add to `rocket-mem-mcp/tests/hash_tools.rs` (append these test functions; keep the existing
`use` lines and the 4 tests from Task 1 unchanged):

```rust
use std::collections::HashMap;

#[tokio::test]
async fn hgetall_hlen_hkeys_and_hvals_report_the_whole_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hgetall.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "f1", "value": "v1"}, {"field": "f2", "value": "v2"}]
        })))
        .await
        .unwrap();

    let hgetall_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hgetall").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let fields = hgetall_result.structured_content.unwrap()["fields"].clone();
    assert_eq!(fields["f1"], "v1");
    assert_eq!(fields["f2"], "v2");

    let hlen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hlen").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    assert_eq!(hlen_result.structured_content.unwrap()["length"], 2);

    let hkeys_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hkeys").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let keys = hkeys_result.structured_content.unwrap()["fields"].clone();
    let keys: Vec<String> = keys
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(keys.contains(&"f1".to_string()) && keys.contains(&"f2".to_string()));

    let hvals_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hvals").with_arguments(object!({"key": "h"})))
        .await
        .unwrap();
    let vals = hvals_result.structured_content.unwrap()["values"].clone();
    let vals: Vec<String> = vals
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(vals.contains(&"v1".to_string()) && vals.contains(&"v2".to_string()));

    guard.kill();
}

#[tokio::test]
async fn hgetall_hlen_hkeys_and_hvals_on_a_missing_key_are_empty_not_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hgetall-missing.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let hgetall_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hgetall").with_arguments(object!({"key": "missing"})),
        )
        .await
        .unwrap();
    assert_ne!(hgetall_result.is_error, Some(true));
    let fields = hgetall_result.structured_content.unwrap()["fields"].clone();
    assert!(fields.as_object().unwrap().is_empty());

    let hlen_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hlen").with_arguments(object!({"key": "missing"})))
        .await
        .unwrap();
    assert_eq!(hlen_result.structured_content.unwrap()["length"], 0);

    guard.kill();
}

#[tokio::test]
async fn hincrby_moves_a_field_and_surfaces_not_an_integer() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hincrby.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    let r1 = client
        .peer()
        .call_tool(CallToolRequestParams::new("hincrby").with_arguments(object!({
            "key": "h", "field": "counter", "delta": 5
        })))
        .await
        .unwrap();
    assert_eq!(r1.structured_content.unwrap()["value"], 5);

    let r2 = client
        .peer()
        .call_tool(CallToolRequestParams::new("hincrby").with_arguments(object!({
            "key": "h", "field": "counter", "delta": -2
        })))
        .await
        .unwrap();
    assert_eq!(r2.structured_content.unwrap()["value"], 3);

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h", "pairs": [{"field": "not-a-number", "value": "abc"}]
        })))
        .await
        .unwrap();
    let bad_result = client
        .peer()
        .call_tool(CallToolRequestParams::new("hincrby").with_arguments(object!({
            "key": "h", "field": "not-a-number", "delta": 1
        })))
        .await
        .unwrap();
    assert_eq!(bad_result.is_error, Some(true));

    guard.kill();
}

#[tokio::test]
async fn hmget_returns_null_for_missing_fields_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hmget.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h", "pairs": [{"field": "f1", "value": "v1"}]
        })))
        .await
        .unwrap();

    let hmget_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hmget")
                .with_arguments(object!({"key": "h", "fields": ["f1", "missing"]})),
        )
        .await
        .unwrap();
    let values = hmget_result.structured_content.unwrap()["values"].clone();
    let values = values.as_array().unwrap();
    assert_eq!(values[0], "v1");
    assert!(values[1].is_null());

    guard.kill();
}

#[tokio::test]
async fn hscan_returns_every_field_in_one_page_with_cursor_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (mut guard, addr) = support::spawn_rocket_mem(&dir.path().join("hscan.aof"));
    let pool = Pool::connect(&addr, None, None, None).await.unwrap();
    let client = support::connect_client_and_server(pool).await;

    client
        .peer()
        .call_tool(CallToolRequestParams::new("hset").with_arguments(object!({
            "key": "h",
            "pairs": [{"field": "alpha", "value": "1"}, {"field": "beta", "value": "2"}]
        })))
        .await
        .unwrap();

    let hscan_result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("hscan").with_arguments(object!({"key": "h", "cursor": 0})),
        )
        .await
        .unwrap();
    let structured = hscan_result.structured_content.unwrap();
    assert_eq!(structured["cursor"], 0);
    let fields = &structured["fields"];
    assert_eq!(fields["alpha"], "1");
    assert_eq!(fields["beta"], "2");

    guard.kill();
}
```

The `HashMap` import above is unused by these specific assertions (they compare against
`serde_json::Value` directly) — remove the `use std::collections::HashMap;` line if `cargo build`
flags it as unused once this task is implemented; it's included here only because it's a natural
type to reach for when reading hash results, not because a test strictly requires it.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd rocket-mem-mcp && cargo test --test hash_tools`
Expected: FAIL — `hgetall`/`hlen`/`hincrby`/`hkeys`/`hvals`/`hmget`/`hscan` don't exist yet (the 4
tests from Task 1 still pass).

- [ ] **Step 3: Add the 7 remaining tools to `tools/hash.rs`**

Add this parameter struct above the `#[tool_router(...)]` block (alongside Task 1's structs):

```rust
#[derive(Deserialize, JsonSchema)]
pub struct HashKeyParams {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HIncrByParams {
    key: String,
    field: String,
    /// Amount to add. Negative decrements. There is no HINCRBYFLOAT in rocket-mem.
    delta: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct HScanParams {
    key: String,
    /// 0 starts a new scan. rocket-mem's HSCAN always completes in one page (a hash lives fully
    /// in memory already), so the returned cursor is always 0 — there is no second page to fetch.
    cursor: u64,
    /// Optional glob filter over field names. Same partial glob support as the top-level `keys`
    /// tool.
    #[serde(default)]
    match_pattern: Option<String>,
}
```

Add these methods inside the existing `impl RocketMemMcpServer` block under
`#[tool_router(router = hash_router, vis = "pub")]`, after `hsetnx`:

```rust
    #[tool(description = "Get every field and value in a hash. A missing key returns an empty \
        set of fields, not an error.")]
    async fn hgetall(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<std::collections::HashMap<String, String>, redis::RedisError> =
            redis::cmd("HGETALL").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(fields) => {
                let text = fields
                    .iter()
                    .map(|(f, v)| format!("{f}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Get the number of fields in a hash. A missing key returns 0, not an \
        error.")]
    async fn hlen(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<usize, redis::RedisError> =
            redis::cmd("HLEN").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(len) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(len.to_string())]);
                r.structured_content = Some(json!({ "length": len }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "Add delta (may be negative) to an integer hash field. A missing field \
        initializes as if it were 0. There is no HINCRBYFLOAT in rocket-mem. Errors if the \
        existing value is not an integer, or if the result would overflow i64.")]
    async fn hincrby(
        &self,
        Parameters(HIncrByParams { key, field, delta }): Parameters<HIncrByParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<i64, redis::RedisError> = redis::cmd("HINCRBY")
            .arg(&key)
            .arg(&field)
            .arg(delta)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(n) => {
                let mut r = CallToolResult::success(vec![ContentBlock::text(n.to_string())]);
                r.structured_content = Some(json!({ "value": n }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "List every field name in a hash. A missing key returns an empty list, \
        not an error.")]
    async fn hkeys(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> =
            redis::cmd("HKEYS").arg(&key).query_async(&mut conn).await;
        match result {
            Ok(fields) => {
                let text = fields.join(", ");
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }

    #[tool(description = "List every value in a hash (order matches hkeys' field order for the \
        same hash). A missing key returns an empty list, not an error.")]
    async fn hvals(
        &self,
        Parameters(HashKeyParams { key }): Parameters<HashKeyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let result: Result<Vec<String>, redis::RedisError> =
            redis::cmd("HVALS").arg(&key).query_async(&mut conn).await;
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

    #[tool(description = "Get multiple hash fields' values at once, in the order requested. A \
        missing field comes back as null in that position; a missing key returns null for every \
        requested field.")]
    async fn hmget(
        &self,
        Parameters(HashFieldsParams { key, fields }): Parameters<HashFieldsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HMGET");
        command.arg(&key);
        for field in &fields {
            command.arg(field);
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

    #[tool(description = "Iterate a hash's fields. Unlike the top-level scan tool, rocket-mem's \
        HSCAN always completes in a single call (a hash already lives fully in memory) — the \
        returned cursor is always 0, meaning there is never a second page to fetch. MATCH \
        filters field names by glob (same partial support as the keys/scan tools); there is no \
        TYPE option for HSCAN in real Redis either, and COUNT would have nothing to act on \
        (there's no paging), so neither is exposed here.")]
    async fn hscan(
        &self,
        Parameters(HScanParams {
            key,
            cursor,
            match_pattern,
        }): Parameters<HScanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut conn = self.pool().connection();
        let mut command = redis::cmd("HSCAN");
        command.arg(&key).arg(cursor);
        if let Some(pattern) = &match_pattern {
            command.arg("MATCH").arg(pattern);
        }
        let result: Result<(u64, std::collections::HashMap<String, String>), redis::RedisError> =
            command.query_async(&mut conn).await;
        match result {
            Ok((next_cursor, fields)) => {
                let text = format!(
                    "cursor={next_cursor}, fields=[{}]",
                    fields
                        .iter()
                        .map(|(f, v)| format!("{f}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
                r.structured_content = Some(json!({ "cursor": next_cursor, "fields": fields }));
                Ok(r)
            }
            Err(err) => redis_error_to_tool_result(err),
        }
    }
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cd rocket-mem-mcp && cargo test --test hash_tools`
Expected: PASS — all 9 tests green (4 from Task 1 + 5 new).

Run the full suite, then lint:

Run: `cd rocket-mem-mcp && cargo test && cargo fmt -- --check && cargo clippy --all-targets -- -D warnings`
Expected: all green, clean. This confirms all 12 Hash tools plus every tool from Plans 1-2 (30
more) work together through the 3-router composition — 42 tools total.

- [ ] **Step 5: Commit**

```bash
git add rocket-mem-mcp/src/tools/hash.rs rocket-mem-mcp/tests/hash_tools.rs
git commit -m "Add remaining rocket-mem-mcp hash tools (hgetall/hlen/hincrby/hkeys/hvals/hmget/hscan)"
```

---

## Next plan

Per `project_rocket_mem_mcp_roadmap` (memory) and the design spec's "Command semantics reference,"
**Plan 4: List family** is next — `LPUSH`, `RPUSH` (variadic), `LPOP`, `RPOP`, `LRANGE`, `LLEN`,
`LINDEX`, `LSET`, `LTRIM`, `LREM`, `LINSERT` (11 tools). Notable grounding already recorded for
that plan: `LPUSH`/`RPUSH` take variadic values (array param, matching this plan's `hset`
precedent for variadic wire commands); `LINSERT`'s before/after is a plain bool parameter, not
`BEFORE`/`AFTER` keywords; `LSET` distinguishes `NoSuchKey` (missing key) from `IndexOutOfRange`
(bad index on an existing list) as two different tool-level errors worth describing separately.

**Housekeeping before or during Plan 4:** this plan found the design spec's "Plan 3 (Hash)"
subsection wrong — it says `HSCAN` isn't implemented, but it is (see "Correction" note above). Fix
that subsection in `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md` and the
tool-count total in the roadmap memory (which currently sums the old, incorrect per-family counts)
to account for this plan's real 12-tool delivery, not the previously-assumed 11.
