# Docs: Architecture & README Pointers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `docs/architecture.md` — the three-layer story pulled together from the production plan's ADR, `docs/design/sharding-decision.md`, and this sprint's own "`dispatch_and_log` is the one place command behavior lives" invariant — plus short README pointers to all four Sprint 8 docs (this one and plan 12's three).

**Architecture:** documentation only.

**Tech Stack:** none.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: documentation scope and location" section.

## Global Constraints

- This plan is sequenced after plan 12 (and after every code plan) — `docs/architecture.md` explains the *finished* Sprint 8 auth/TLS/config story, so it must be written once, correctly, against the real merged code, not drafted early and left stale.

---

### Task 1: `docs/architecture.md`

**Files:**
- Create: `docs/architecture.md`

**Interfaces:**
- Consumes: `docs/rocket-mem-production-plan.md`'s Architecture Decision Record, `docs/design/sharding-decision.md`, `README.md`'s "Architecture" section (`README.md:9-25`), this sprint's spec.
- Produces: nothing new.

- [ ] **Step 1: Write the file**

Required sections, in order:

1. **The three layers** — reproduce `README.md`'s existing ASCII diagram (`README.md:14-22`) and its one-paragraph explanation, then go one level deeper than the README does for each layer:
   - **Protocol layer**: RESP2/RESP3 (`protocol::codec::RespCodec`) and RMP (`protocol::rmp`), both producing/consuming the same `protocol::Frame` value model — the point being that a third protocol could be added the same way without touching the dispatcher.
   - **Command dispatcher**: `crates/server/src/dispatcher.rs`'s `dispatch` (pure, protocol-agnostic, called directly by AOF replay and the follower apply loop) and `dispatch_and_log` (the full pipeline: `auth_gate`, cluster redirect, the `-READONLY` gate, `AUTH`/`ACL`/`SAVE`/`REPLICAOF`/`CLUSTER`/`INFO`/`HELLO`/`SLOWLOG` interception, AOF logging, replica fan-out) — state this sprint's own finding explicitly: **`dispatch_and_log` is the single place command behavior lives**, which is what let both RMP (Sprint 7) and ACL enforcement (this sprint) be added without touching that pipeline's internals for RMP, or by adding exactly one new interception for ACL.
   - **Storage engine**: `crates/engine`'s `Value`/`Store`/`Engine`, unchanged by every protocol- or auth-layer sprint since Sprint 1 — the concrete proof that "protocol-agnostic engine" was a real property, not an aspiration, tracing through Sprints 2 (RESP), 5 (replication reuses the same `dispatch`), 7 (RMP), and 8 (ACL/TLS) as evidence.
2. **Concurrency model** — reproduce `README.md`'s one-line summary (`README.md:24`) and expand: 16 shards (`docs/design/sharding-decision.md`'s rationale, and its Sprint 6 update on why shard count was left unchanged after profiling), one Tokio task per RESP connection, one task per in-flight RMP request (Sprint 7's spawn-per-request model) sharing one `Session` per RMP connection (this sprint).
3. **The `Session` / auth boundary** — new for this sprint: why `Session` replaced a bare `Protocol` parameter (the RMP per-connection state problem, from this sprint's spec), and where the `auth_gate` sits relative to cluster redirection and the `-READONLY` gate (auth first, matching real Redis's own ordering).
4. **Durability & replication recap** — one paragraph pointing at `docs/superpowers/specs/2026-08-30-sprint-4-spec.md` (AOF) and `2026-08-30-sprint-5-spec.md` (snapshot/replication) rather than re-deriving them; this document's job is the cross-cutting shape, not re-explaining each sprint's own spec.
5. **Where to go deeper** — a table linking every `docs/superpowers/specs/*.md` file to the one-line "what it decided" summary, so a reader can jump straight to the sprint that answers their question instead of reading this file plus eight specs looking for it.

- [ ] **Step 2: Verify every cross-reference resolves**

Run: `grep -oE 'docs/[a-zA-Z0-9./_-]+\.md' docs/architecture.md | while read -r f; do test -f "$f" || echo "BROKEN: $f"; done`
Expected: no output (every linked file exists).

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/architecture.md`.

---

### Task 2: README pointers

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: `docs/getting-started.md`, `docs/config-reference.md`, `docs/command-compatibility.md` (plan 12), `docs/architecture.md` (Task 1 here).
- Produces: nothing new.

- [ ] **Step 1: Add a Sprint 8 "Status" paragraph**

In `README.md`'s "Status" section, after the Sprint 6 paragraph (`README.md`, right before wherever the file currently ends its sprint-by-sprint history), add a Sprint 8 paragraph in the same voice as the existing Sprint 1–6 ones (see the Sprint 6 paragraph starting `**Sprint 6 (clustering & observability) — done.**` for the exact tone/density to match — direct, specific, no marketing language), covering: `AUTH`/ACL (users, command/key-pattern rules, both config-file bootstrap and runtime `ACL SETUSER`), TLS for both RESP and RMP, `figment`/`clap` config layering with full env-var backward compatibility, the chaos-test result (link to `docs/chaos/<date>-chaos-log.md`), and the four new docs.

- [ ] **Step 2: Update the "Documentation" section**

In `README.md`'s existing "Documentation" list (`README.md:312-318`), add four new bullets, in this order (getting-started first, since it's the natural entry point for a new reader):

```markdown
- [`docs/getting-started.md`](docs/getting-started.md) — install, first run, first `redis-cli`/`rmp-client` session, enabling TLS.
- [`docs/config-reference.md`](docs/config-reference.md) — every config field: TOML key, env var, CLI flag, default.
- [`docs/command-compatibility.md`](docs/command-compatibility.md) — full command table plus known divergences from real Redis, collected in one place.
- [`docs/architecture.md`](docs/architecture.md) — the three-layer design and concurrency model, pulling together the per-sprint specs.
```

- [ ] **Step 2 continued: Update the env-var table**

In `README.md`'s "Running with persistence and replication" section's environment-variable table (`README.md:181-190`), add the four new TLS rows (`ROCKET_MEM_TLS_RESP_ADDR`, `ROCKET_MEM_TLS_RMP_ADDR`, `ROCKET_MEM_TLS_CERT_PATH`, `ROCKET_MEM_TLS_KEY_PATH`) and a one-line pointer: "See `docs/config-reference.md` for the full list including TOML/CLI equivalents and ACL bootstrap — this table covers only the env-var layer, matching this section's existing scope."

- [ ] **Step 3: Full-workspace check and commit**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all green (README-only change, but this is the final documentation task of the sprint's doc plans — a good checkpoint to confirm nothing upstream regressed).

Use the `1-git-commit` skill/command to commit `README.md`.
