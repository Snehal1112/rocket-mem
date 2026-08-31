# Docs: Getting Started, Config Reference, Command Compatibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** three of the four Sprint 8 documentation deliverables — a getting-started guide, a config reference, and a command-compatibility matrix — landing in top-level `docs/`, matching `docs/benchmarks/`'s existing precedent (plan 13 covers `docs/architecture.md` and README pointers).

**Architecture:** each is a standalone Markdown file with no code changes. Facts (env var names/defaults, command names) are pulled verbatim from the source of truth (plan 01's `Config` struct, `dispatcher.rs`'s `KNOWN_COMMANDS`) rather than re-derived by hand, so they can't silently drift from what actually ships.

**Tech Stack:** none — documentation only.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: documentation scope and location" section.

## Global Constraints

- Every fact in these docs (env var name, default, command name) must be checked against the actual current source at the time of writing (plans 1–11 must all be merged first — this plan is sequenced last among the three doc-adjacent plans for exactly that reason) — not copied from this plan's draft tables below without verifying they still match, since this plan was written before those plans' implementation landed and a field name could have shifted during review.
- These are user-facing docs — the audience is someone who has never read this project's sprint specs, unlike this plan itself.

---

### Task 1: `docs/getting-started.md`

**Files:**
- Create: `docs/getting-started.md`

**Interfaces:**
- Consumes: nothing code-level — points at real commands a fresh reader can run.
- Produces: nothing new.

- [ ] **Step 1: Write the file**

Required sections, in order, with the concrete content shown (write connecting prose around them in this project's established README voice — direct, technical, no marketing tone):

1. **Install/build** — `git clone`, `cargo build --release --bin rocket-mem`, note the built binary path `target/release/rocket-mem`.
2. **First run, no config** — running the binary with zero configuration starts a plaintext RESP listener on `127.0.0.1:6379`, RMP on `127.0.0.1:6380`, metrics on `127.0.0.1:9121`, exactly as before Sprint 8 — config layering (plan 1/2) is additive, not required.
3. **A minimal `rocket-mem.toml`** — a runnable example:
   ```toml
   addr = "127.0.0.1:6379"
   rmp_addr = "127.0.0.1:6380"

   [[acl.users]]
   username = "app"
   password = "changeme"
   enabled = true
   rules = ["allcommands", "allkeys"]
   ```
   Run with `cargo run --release --bin rocket-mem -- --config rocket-mem.toml`.
4. **First `redis-cli` session** — `redis-cli -p 6379 AUTH app changeme`, then `SET foo bar`, `GET foo`.
5. **First RMP session** — the exact `rmp-client` snippet already in `README.md`'s "Running the custom protocol (RMP)" section (copy it verbatim — don't re-derive).
6. **Enabling TLS** — a pointer to `docs/config-reference.md`'s `tls_*` fields, with the one-line `openssl req -x509 ...` command from plan 09, Task 1, Step 2, for generating a self-signed cert for local testing (explicitly labeled "for local testing only — get a real certificate for anything else").
7. **Where to go next** — links to `docs/config-reference.md`, `docs/command-compatibility.md`, `docs/architecture.md`, and `README.md`'s "Running a cluster" section.

- [ ] **Step 2: Verify every command in the file actually works**

Run every command shown in the file (build, start with the example TOML, `AUTH`, `SET`/`GET`, the RMP snippet as a standalone `cargo run --example` or doctest-style check) against a locally built binary. Fix any command that doesn't work as written — this file is only useful if copy-pasting from it works.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/getting-started.md`.

---

### Task 2: `docs/config-reference.md`

**Files:**
- Create: `docs/config-reference.md`

**Interfaces:**
- Consumes: `crates/server/src/config.rs`'s `Config` struct (plan 01) as the source of truth.
- Produces: nothing new.

- [ ] **Step 1: Write the file**

One table, one row per `Config` field, columns: TOML key | env var | CLI flag | default | meaning. Populate every row by reading the current `crates/server/src/config.rs` at the time this task is executed (do not transcribe this plan's draft below without checking it against the real file — field names/defaults may have shifted during plans 1–10's review):

| TOML key | Env var | CLI flag | Default | Meaning |
|---|---|---|---|---|
| `addr` | `ROCKET_MEM_ADDR` | `--addr` | `127.0.0.1:6379` | RESP listener bind address |
| `rmp_addr` | `ROCKET_MEM_RMP_ADDR` | `--rmp-addr` | `127.0.0.1:6380` | RMP listener bind address |
| `metrics_addr` | `ROCKET_MEM_METRICS_ADDR` | `--metrics-addr` | `127.0.0.1:9121` | Prometheus `/metrics` bind address |
| `aof_path` | `ROCKET_MEM_AOF_PATH` | `--aof-path` | `./appendonly.aof` | Append-only file path |
| `snapshot_path` | `ROCKET_MEM_SNAPSHOT_PATH` | `--snapshot-path` | `./dump.snapshot` | Snapshot file path |
| `slowlog_threshold_micros` | `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS` | `--slowlog-threshold-micros` | `10000` | Slow-log threshold; `0` disables |
| `cluster_config` | `ROCKET_MEM_CLUSTER_CONFIG` | `--cluster-config` | unset | Cluster topology file path |
| `cluster_node_id` | `ROCKET_MEM_CLUSTER_NODE_ID` | `--cluster-node-id` | unset | This node's id in that file |
| `tls_resp_addr` | `ROCKET_MEM_TLS_RESP_ADDR` | `--tls-resp-addr` | unset | RESP-over-TLS bind address; unset means no TLS RESP listener |
| `tls_rmp_addr` | `ROCKET_MEM_TLS_RMP_ADDR` | `--tls-rmp-addr` | unset | RMP-over-TLS bind address |
| `tls_cert_path` | `ROCKET_MEM_TLS_CERT_PATH` | `--tls-cert-path` | unset | PEM certificate chain, shared by both TLS listeners |
| `tls_key_path` | `ROCKET_MEM_TLS_KEY_PATH` | `--tls-key-path` | unset | PEM private key, shared by both TLS listeners |
| `[[acl.users]]` | (file-only — no flat env var for a list) | (file-only) | empty | ACL bootstrap users; see below |

Below the table:
- **Precedence**: CLI flags > `ROCKET_MEM_*` env vars > TOML file > defaults, with one worked example (the same shape as plan 02's `cli_flag_overrides_env_var_overrides_file_overrides_default` test).
- **The `[[acl.users]]` array**: repeat the `AclUserConfig` shape (`username`, `password`, `enabled`, `rules`) and the same worked example already in this spec's "Decision: config layering" section, plus a pointer to `ACL SETUSER`'s token vocabulary (`docs/command-compatibility.md` or inline) for what `rules` strings mean.
- **Backward compatibility note**: every `ROCKET_MEM_*` variable this project read before Sprint 8 still works identically — config layering is additive.

- [ ] **Step 2: Cross-check against the real `Config` struct**

Run: `grep -n "pub [a-z_]*:" crates/server/src/config.rs` and confirm every field appears in the table above with the correct default (cross-check against `Config::default()`'s literal values in the same file) and the correct env var name (`ROCKET_MEM_` + the field name uppercased, per `Env::prefixed`'s convention).

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/config-reference.md`.

---

### Task 3: `docs/command-compatibility.md`

**Files:**
- Create: `docs/command-compatibility.md`

**Interfaces:**
- Consumes: `README.md`'s existing "Command coverage" table (`README.md:155-175`) and `crates/server/src/dispatcher.rs`'s `KNOWN_COMMANDS` list as the two sources of truth to reconcile.
- Produces: nothing new.

- [ ] **Step 1: Write the file**

Start from `README.md`'s existing "Command coverage" table verbatim (copy the five rows: String/Key, Hash, List, Set, Sorted Set, Server/Cluster) and add a sixth row for this sprint's additions:

| Type | Implemented |
|---|---|
| Auth/ACL | `AUTH` (both single-arg and `<user> <pass>` forms), `ACL SETUSER`/`DELUSER`/`WHOAMI`/`LIST`/`GETUSER` |

Then add a **"Known divergences from real Redis"** section collecting, in one place, every gap already called out piecemeal across the sprint specs and the README's own footnotes — read each source file below and transcribe its actual current wording (do not invent new wording):
- `KEYS`'s partial glob support (README.md's existing paragraph right after the coverage table).
- Active expiry sweeping a whole shard per tick rather than per-key sampling (same paragraph).
- `OBJECT ENCODING` reporting this engine's own type names, not real Redis's internal encodings (same paragraph).
- `SLOWLOG`'s 4-field entries vs. real Redis's 6 (`docs/superpowers/specs/2026-08-30-sprint-6-spec.md`'s slow-log decision).
- `expired_keys` counting only active expiry, not passive (same spec, `INFO` decision section).
- No partial replication resync — every (re)sync is a full resync (`README.md`'s "Known limits" paragraph in the Status section).
- No `@category` ACL grants (`+@read`, `+@write`) — explicit `+CMD`/`-CMD` only (this sprint's own spec, "Global Constraints").
- ACL users are in-memory only, not persisted to the AOF/snapshot; a runtime `ACL SETUSER` is lost on restart unless also in the bootstrap TOML (this sprint's spec, "ACL data model" decision, "Storage location" paragraph).
- `DEBUG SLEEP`'s 10-second cap (README's existing footnote, `[^debug-sleep-cap]`).

Finally, a **"Commands not implemented"** section: real Redis commands with no counterpart here at all — at minimum `LPOS`, `COPY`, `OBJECT FREQ`/`IDLETIME`, `WAIT`, `LOLWUT`, `LMPOP`/`ZMPOP` and their blocking (`B*`) counterparts, Lua scripting (`EVAL`/`EVALSHA`), pub/sub (`SUBSCRIBE`/`PUBLISH`), transactions (`MULTI`/`EXEC`), and streams (`XADD` etc.) — cross-reference `docs/rocket-mem-sprint-plan.md`'s Sprint 8 retro note about a "Phase 5 backlog" (Lua scripting, pub/sub, transactions, streams, live resharding) as where these are tracked, not silently absent.

- [ ] **Step 2: Cross-check against `KNOWN_COMMANDS`**

Run: `grep -A200 "pub(crate) const KNOWN_COMMANDS" crates/server/src/dispatcher.rs | grep -oE '"[A-Z]+"' | tr -d '"'` and diff the resulting list against every command named in the file written in Step 1 — every entry in `KNOWN_COMMANDS` must appear somewhere in the compatibility matrix (it doesn't need its own table row if it's covered by an existing family, e.g. `PEXPIRE` under the `EXPIRE` family, but it must be findable).

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/command-compatibility.md`.
