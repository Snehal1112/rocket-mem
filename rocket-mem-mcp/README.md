# rocket-mem-mcp

An MCP (Model Context Protocol) server that exposes a running rocket-mem instance's command set
as MCP tools, so an LLM agent can read and write it as part of an agent workflow. It connects
over ordinary RESP2/RESP3 via the `redis` crate — the same wire protocol any Redis-compatible
client would use — rather than rocket-mem's own second protocol (RMP).

See `docs/superpowers/specs/2026-09-13-rocket-mem-mcp-server-design.md` (in the main repo) for
the full design rationale: why this is a standalone crate rather than a workspace member, why
RESP2/RESP3 over `redis-rs` rather than RMP, the "one MCP tool per rocket-mem command" decision,
and the connection/session model for stateless commands vs. transactions vs. pub/sub.

## Why this lives outside the Cargo workspace

`rocket-mem-mcp` is an agent-facing *consumer* of rocket-mem, not a wire protocol rocket-mem
itself serves — unlike the `server` crate, which owns RESP/RMP. It has its own `Cargo.toml` with
an empty `[workspace]` table, is not listed in the root `Cargo.toml`'s `members`, and is not built
or gated by `.github/workflows/ci.yml`. This keeps `rmcp` (a pre-1.0, macro-heavy dependency) and
its transitive dependencies out of the workspace's strict `cargo clippy --workspace --all-targets
-- -D warnings` CI gate, matching the precedent `examples/` and `tools/review-agent` already set
for rocket-mem-adjacent tooling that lives alongside the main repo without being part of it.

## Building and testing

This crate's tests spawn a real, locally-built `rocket-mem` binary (never a mock of the wire
protocol) — build it once from the repo root before running `cargo test` here:

```bash
cd /home/numericlabs/data/rocket/rocket-mem
cargo build -p rocket-mem
```

By default the test helper (`tests/support/mod.rs`) looks for the binary at
`../target/debug/rocket-mem`, relative to this crate. If you've built it somewhere else, point
the tests at it instead:

```bash
ROCKET_MEM_BIN=/path/to/rocket-mem cargo test
```
