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

## Configuration

All configuration is env-only (plus matching CLI flags for everything except the two secrets
below), read once at startup:

| Setting | Env var | Default | Purpose |
|---|---|---|---|
| `target_addr` | `ROCKET_MEM_MCP_TARGET_ADDR` | `127.0.0.1:6379` | `host:port` of the rocket-mem instance to expose |
| `acl_username` | `ROCKET_MEM_MCP_ACL_USERNAME` | unset | `AUTH` username, if the target has ACLs enabled |
| `acl_password` | `ROCKET_MEM_MCP_ACL_PASSWORD` | unset | `AUTH` password. Env-only, deliberately no CLI flag — a flag value leaks via `ps`/shell history, an env var read once at startup does not |
| `tls_ca_path` | `ROCKET_MEM_MCP_TLS_CA_PATH` | unset | Path to a CA cert (PEM) to verify the target's TLS certificate against. Setting this is what turns TLS on for the connection — there is no separate flag, and no way to skip verification |

Example, connecting to a TLS + ACL-enabled instance (mirroring `rocket-mem.toml`'s own
`tls_resp_addr`/`[[acl.users]]` example):

```bash
ROCKET_MEM_MCP_TARGET_ADDR=numericlabs.lxd:16379 \
ROCKET_MEM_MCP_ACL_USERNAME=app \
ROCKET_MEM_MCP_ACL_PASSWORD=changeme \
ROCKET_MEM_MCP_TLS_CA_PATH=/home/numericlabs/data/tls/root_ca-numericlabs.crt \
./target/debug/rocket-mem-mcp
```

## Building and testing

This crate's tests spawn a real, locally-built `rocket-mem` binary (never a mock of the wire
protocol) — build it once from the repo root before running `cargo test` here:

```bash
# from the main repo root
cargo build -p rocket-mem
```

By default the test helper (`tests/support/mod.rs`) looks for the binary at
`../target/debug/rocket-mem`, relative to this crate. If you've built it somewhere else, point
the tests at it instead:

```bash
ROCKET_MEM_BIN=/path/to/rocket-mem cargo test
```

The ACL and TLS tests (`tests/acl_tls.rs`) reuse the same self-signed test certificate
`crates/server/tests/tls.rs` uses (copied into `tests/fixtures/`, CN/SAN `localhost`/
`127.0.0.1`) and write a temporary `rocket-mem.toml` to bootstrap an ACL user — no external
setup needed beyond the binary above.
