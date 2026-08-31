# Sprint 8 — Auth, ACLs, TLS & Release: Spec & Design

**Goal:** `v1.0.0` tagged — authenticated, TLS-capable, chaos-tested, documented, and shipped as a Docker image — matching `../../rocket-mem-sprint-plan.md`'s Sprint 8 goal.

**Scope:** covers Sprint 8's 6 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 8, and `../../rocket-mem-production-plan.md`, Weeks 15–16). This doc fixes the shared design decisions — the ACL data model and where its check lives in the command pipeline, the `Session` type that replaces the bare `Protocol` parameter, TLS listener wiring, config-file layering, the chaos-test script's shape, documentation scope, and the release/Docker pipeline — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** unlike Sprint 7 (one new subsystem), Sprint 8 is six mostly-independent pieces unified by one constraint carried forward from the Sprint 7 follow-up hardening plan: **`dispatcher::dispatch_and_log` and `dispatch` stay the only place command behavior lives.** ACL enforcement is a new interception inside `dispatch_and_log_inner`, following the exact precedent `CLUSTER`/`SLOWLOG`/`INFO` already set — not a check bolted onto `connection.rs`/`rmp_connection.rs`. The one signature change this forces: `dispatch_and_log`'s `protocol: &mut Protocol` parameter becomes `session: &Session`, a small struct carrying both RESP's existing protocol-negotiation state and a new shared, per-*connection* (not per-request) authenticated-user cell — the first piece of real shared connection state RMP has ever needed, since every RMP request today builds a throwaway `Protocol::default()` with nothing to persist across calls. TLS, config layering, the chaos test, docs, and Docker/release are additive and don't touch the dispatcher at all.

## Global Constraints

- **New dependencies, one per concern, no alternatives evaluated further than the production plan's own shortlist:** `argon2` (password hashing), `tokio-rustls` + `rustls-pemfile` (TLS), `figment` (config layering, `toml` + `env` features) + `clap` (`derive` feature, the CLI-flag layer figment itself doesn't provide). No other new runtime dependency lands anywhere in the workspace this sprint.
- **Zero behavior change with no configuration.** No ACL users configured ⇒ the auth gate is skipped entirely (checked once, cheaply, per command) ⇒ every one of the ~600 existing tests and every deployment through Sprint 7 keeps working unmodified. No TLS cert configured ⇒ no TLS listener binds ⇒ today's plaintext-only behavior, unchanged. This mirrors real Redis's own default user (`nopass`, `allcommands`, `allkeys`).
- **Existing env vars keep working.** `figment`'s `Env::prefixed("ROCKET_MEM_")` provider means every `ROCKET_MEM_*` variable `main.rs` reads today continues to work identically; config layering adds a TOML file and CLI flags *underneath/above* that, it does not replace it.
- **No `@category` ACL grants** (`+@read`, `+@write`, ...). Real Redis's category taxonomy is large and nothing here needs it yet; explicit `+CMDNAME`/`-CMDNAME` grants plus `allcommands`/`nocommands` cover the production plan's own example test and everything realistically needed this sprint. A stated, honest gap — additive to extend later, per this project's own documented-gaps convention (see Sprint 6's `INFO` fields).
- **No multi-tenancy / logical-DB work.** ACL's `~pattern` key-restriction already gives per-user key-prefix isolation, which the production plan names as the explicit alternative to a full tenant model. `SELECT` stays the Sprint 2 single-logical-DB no-op it already is.
- **`v1.0.0` is a version bump, not a feature.** `Cargo.toml`'s `workspace.package.version` moves from `0.1.2` to `1.0.0` as the last step of sprint close, after everything else is merged and green.

---

## Decision: ACL data model, storage, and command surface

**`crates/server/src/acl.rs`** (new module):

```rust
pub struct AclUser {
    pub username: String,
    pub password_hash: Option<String>, // argon2 hash; None = nopass (any/no password accepted)
    pub enabled: bool,                 // "on" / "off" — an "off" user can never authenticate
    pub rules: Vec<AclRule>,           // applied in order; last matching rule wins, real-Redis style
}

pub enum AclRule {
    AllCommands,          // "allcommands" / "+@all"
    NoCommands,           // "nocommands" / "-@all" — also every new user's implicit starting point
    AllowCommand(String), // "+get" — command name, uppercased at parse time
    DenyCommand(String),  // "-set"
    AllKeys,               // "~*" / "allkeys"
    KeyPattern(String),    // "~app:*" — glob, reusing engine::glob::glob_match
}

pub struct AclStore {
    users: std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<AclUser>>>,
}

impl AclStore {
    pub fn is_empty(&self) -> bool;                                   // the fast-path check
    pub fn authenticate(&self, username: &str, password: &str) -> Option<Arc<AclUser>>;
    pub fn set_user(&self, username: String, rule_tokens: &[Bytes]) -> Result<(), AclError>;
    pub fn del_user(&self, username: &str) -> bool;
    pub fn get_user(&self, username: &str) -> Option<Arc<AclUser>>;
    pub fn list(&self) -> Vec<Arc<AclUser>>;
    pub fn is_allowed(&self, user: &AclUser, command: &str, keys: &[&Bytes]) -> bool;
}
```

`is_allowed` folds `rules` left-to-right tracking two booleans (`command_allowed`, defaulting `false`; `key_allowed`, defaulting `false` unless no keys are involved), then checks every key in `keys` (reusing `dispatcher::key_spec`/`command_keys`, already `pub(crate)`) against every `KeyPattern`/`AllKeys` rule — a key command is permitted only if the command is allowed *and* every one of its keys matches at least one key rule.

**Password hashing:** `argon2` with its own randomly-generated salt per user, using the crate's recommended default parameters (no manual tuning this sprint — the production plan lists `argon2`/`bcrypt` as alternatives precisely because either default is adequate here). `ACL SETUSER`'s `>password` token hashes immediately; `authenticate` verifies via `argon2::verify`.

**Storage location:** `AclStore` lives on `ReplicationHandle` (mirroring `SlowLog`, `Option<Arc<ClusterConfig>>`), constructed via a new `ReplicationHandle::with_acl_bootstrap(users: Vec<AclUser>)` builder method, called from `main.rs` only when the config's `[[acl.users]]` array is non-empty. **Not persisted to the AOF or snapshot** — a runtime `ACL SETUSER` is in-memory only, lost on restart unless it's also in the bootstrap config, exactly like real Redis's `ACL SETUSER` unless `ACL SAVE`/`aclfile` is configured (out of scope — bootstrap-from-TOML is this project's equivalent of `aclfile`, and no runtime `ACL SAVE` command is added).

**Command surface**, all intercepted in `dispatch_and_log_inner` like `CLUSTER`/`SLOWLOG`:

| Command | Behavior |
|---|---|
| `AUTH <password>` | Checks against the reserved `"default"` user. |
| `AUTH <username> <password>` | Checks against the named user. |
| `ACL SETUSER <name> <rule>...` | Parses tokens into `AclRule`s (unknown token ⇒ `ERR syntax error`), creates or replaces the user. |
| `ACL DELUSER <name>` | Removes a user; deleting the currently-authenticated user does not retroactively deauthenticate that connection (matches real Redis). |
| `ACL LIST` | One line per user, real-Redis-shaped (`user default on nopass ~* +@all`-style rendering — reconstructed from `rules`, not stored pre-rendered). |
| `ACL WHOAMI` | The current connection's authenticated username, or `"default"` if unauthenticated (matches real Redis's behavior when no auth is required). |
| `ACL GETUSER <name>` | Structured reply (`Frame::Map`) of the named user's flags/rules, or `Null` if absent. |

`AUTH`/`ACL *` are always permitted regardless of auth state — the alternative (needing to already be authenticated to authenticate) is nonsensical.

**Auth error shapes**, matching real Redis's own text so existing client libraries' error-detection logic (which often pattern-matches on these prefixes) works unmodified:
- No `AUTH` sent, ACL non-empty, command requires auth: `Frame::Error("NOAUTH Authentication required.")`
- Authenticated but command/key not permitted: `Frame::Error("NOPERM this user has no permissions to run this command")` (command-level) or `Frame::Error("NOPERM no permissions to access a key")` (key-level) — the two real-Redis message shapes, kept distinct since operators grep for them differently.
- Wrong password / unknown user: `Frame::Error("WRONGPASS invalid username-password pair or user is disabled.")`

---

## Decision: `Session` replaces the bare `Protocol` parameter

**Problem:** `dispatch_and_log(engine, aof, replication, frame, protocol: &mut Protocol, client_id)` currently takes `Protocol` by exclusive reference. RESP's connection loop owns one `Protocol` value across its lifetime and passes it in each iteration — fine, since RESP handles one request at a time. RMP's connection handler currently builds a **fresh `Protocol::default()` per request** (Sprint 7's spec: "RMP has no protocol-negotiation state to persist between calls"), because nothing before this sprint needed RMP connections to remember anything across requests.

Auth breaks that assumption: once one request on an RMP connection authenticates, every other concurrently-in-flight or later request on that *same connection* must see it as authenticated — real shared state, not per-request state.

**`crates/server/src/dispatcher.rs`** (or a small new `session.rs`):

```rust
pub struct Session {
    protocol: std::sync::Mutex<Protocol>,               // Protocol is Copy; RESP mutates it via HELLO
    authenticated_user: std::sync::Mutex<Option<Arc<AclUser>>>,
}

impl Session {
    pub fn new() -> Self;                              // unauthenticated, Protocol::default()
    pub fn protocol(&self) -> Protocol;
    pub fn set_protocol(&self, p: Protocol);
    pub fn authenticated_user(&self) -> Option<Arc<AclUser>>;
    pub fn set_authenticated_user(&self, user: Option<Arc<AclUser>>);
}
```

`dispatch_and_log`'s signature becomes `dispatch_and_log(engine, aof, replication, frame, session: &Session, client_id) -> Frame`. `handle_hello` reads/writes `session.protocol()`/`set_protocol` instead of `*protocol = ...`. `connection.rs`'s post-dispatch sync line (`framed.codec_mut().protocol = protocol`) becomes `framed.codec_mut().protocol = session.protocol()`.

**Ownership, per protocol:**
- **RESP** (`connection.rs`): `let session = Session::new();` once, outside the request loop — replacing `let mut protocol = Protocol::default();`. Passed as `&session` each iteration. No `Arc` needed; RESP is one task, one connection, sequential.
- **RMP** (`rmp_connection.rs`): `let session = Arc::new(Session::new());` **once per accepted connection**, before the read loop starts — this is the behavioral fix that matters. Each spawned per-request task clones the `Arc<Session>` alongside its existing `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>` clones. `Session`'s interior mutability (both fields `Mutex`) is what makes sharing across concurrently-running tasks sound without redesigning the spawn-per-request model: `Mutex<T>` is `Sync` (for `T: Send`), which is required for `Arc<Session>: Send` — and `tokio::spawn` requires its future (and therefore everything captured into it, including the cloned `Arc<Session>`) to be `Send`. `Cell<T>` is never `Sync`, no matter what `T` is, so a `Cell` field here would make `Session: !Sync` and `Arc<Session>: !Send`, which would not compile against `tokio::spawn`.

**Auth gate placement:** the very first check inside `dispatch_and_log_inner`, ahead of `cluster_redirect` — matching real Redis's own auth-before-everything-else ordering (an unauthenticated client should not learn cluster topology, get read-only-replica errors, or reach the engine at all):

```rust
if !replication.acl.is_empty() {
    if let Some(reply) = acl::check(&replication.acl, session, &frame) {
        return reply; // NOAUTH / NOPERM / WRONGPASS, or None if permitted / handled by AUTH itself
    }
}
```

---

## Decision: TLS — separate ports, no protocol sniffing

**New env vars / config keys** (see the Config Layering decision below for how these are actually supplied):

| Key | Default | Meaning |
|---|---|---|
| `tls_resp_addr` | unset | Bind address for RESP-over-TLS. Unset ⇒ listener not bound. |
| `tls_rmp_addr` | unset | Bind address for RMP-over-TLS. Unset ⇒ listener not bound. |
| `tls_cert_path` | unset | PEM certificate chain path. Required if either `tls_*_addr` is set. |
| `tls_key_path` | unset | PEM private key path. Required if either `tls_*_addr` is set. |

One cert/key pair serves both TLS listeners — no per-protocol certs, since they're the same server identity.

**Wiring:** a new `crates/server/src/tls.rs` with `fn load_server_config(cert_path, key_path) -> io::Result<Arc<rustls::ServerConfig>>` (parses PEM via `rustls-pemfile`, builds a `rustls::ServerConfig` with no client-cert verification — server-auth-only TLS, matching "TLS support for client connections" as scoped). `connection::handle_connection` and `rmp_connection::handle_connection` both become generic over `S: AsyncRead + AsyncWrite + Unpin + Send + 'static` instead of hardcoded to `tokio::net::TcpStream`, so the identical function serves plain and TLS sockets. Each TLS listener's accept loop wraps the accepted `TcpStream` in `TlsAcceptor::accept(socket).await` before calling the shared `handle_connection`; a handshake failure (including a plaintext client sending raw RESP/RMP bytes at a TLS port — its bytes don't parse as a TLS `ClientHello`) simply drops that connection, exactly like any other malformed-input path already does.

**"TLS-only" is a deployment choice, not a server mode:** the production plan's `plaintext_connection_is_rejected_when_tls_only_mode_is_enabled` test is satisfied by starting a server with only `tls_resp_addr` configured and `addr` (the plaintext port) left unbound — no new "TLS required" flag or sniffing logic needed. A `TcpStream::connect` to the TLS port succeeds (TCP layer), but the connection is dropped without a valid reply once the TLS handshake fails to parse the client's plaintext bytes — which is exactly the test's own assertion shape.

---

## Decision: config layering — figment + clap, CLI > env > file > defaults

**`crates/server/src/config.rs`** (new module):

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub addr: String,                          // default "127.0.0.1:6379"
    pub rmp_addr: String,                      // default "127.0.0.1:6380"
    pub metrics_addr: String,                  // default "127.0.0.1:9121"
    pub aof_path: String,                      // default "./appendonly.aof"
    pub snapshot_path: String,                 // default "./dump.snapshot"
    pub slowlog_threshold_micros: u64,         // default 10_000
    pub cluster_config: Option<String>,
    pub cluster_node_id: Option<String>,
    pub tls_resp_addr: Option<String>,
    pub tls_rmp_addr: Option<String>,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
    pub acl: AclBootstrapConfig,               // { users: Vec<AclUserConfig> }, default empty
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AclBootstrapConfig {
    pub users: Vec<AclUserConfig>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AclUserConfig {
    pub username: String,
    pub password: Option<String>,   // plaintext in the TOML file, hashed once at load time; None = nopass
    pub enabled: bool,               // default true
    pub rules: Vec<String>,         // raw rule tokens, parsed the same way ACL SETUSER's tokens are
}

#[derive(clap::Parser)]
struct Cli {
    #[arg(long)] config: Option<std::path::PathBuf>, // TOML file path; default "rocket-mem.toml" if present, else skipped
    #[arg(long)] addr: Option<String>,
    #[arg(long)] rmp_addr: Option<String>,
    // ... one Option<T> field per Config field above, all None by default
}

pub fn load() -> Result<Config, figment::Error> {
    let cli = Cli::parse();
    let mut figment = Figment::from(Serialized::defaults(Config::default()));
    if let Some(path) = cli.config_path() { figment = figment.merge(Toml::file(path)); }
    figment = figment.merge(Env::prefixed("ROCKET_MEM_"));
    figment = figment.merge(Serialized::defaults(cli.into_overrides())); // only Some(_) fields
    figment.extract()
}
```

Example `rocket-mem.toml` fragment for ACL bootstrap:

```toml
[[acl.users]]
username = "readonly-app"
password = "pw"
enabled = true
rules = ["~app:*", "+get", "-set"]
```

Precedence (last merge wins, per the production plan's own example test): **CLI flags > `ROCKET_MEM_*` env vars > TOML file > built-in defaults.** `main.rs` calls `config::load()` once at startup and replaces every individual `std::env::var(...)` call with a field read off the returned `Config` — a mechanical rewrite, no behavior change for anyone who only sets env vars today (`Env::prefixed` reads the identical variable names).

`Cargo.toml` gains `figment = { version = "0.10", features = ["toml", "env"] }` and `clap = { version = "4", features = ["derive"] }`, both workspace dependencies, used only by the `server` crate.

---

## Decision: chaos test is a committed script + committed log, not a CI job

Same shape as Sprint 6's `redis-benchmark` report (`../../specs/2026-08-30-sprint-6-spec.md`'s benchmark decision): an overnight run cannot fit inside CI's time budget, so this is a manual/scheduled operational artifact, not a gated test.

**`scripts/chaos.sh`** (committed): starts a leader + one follower (own `Engine`/AOF/snapshot dirs, own ports, matching `tests/replication.rs`'s in-process node shape but as real separate processes here since the script must survive `kill -9`ing them). A load generator (a small Rust binary, `crates/server/src/bin/chaos_load.rs` or a script using `redis-cli`/`rmp-client`) writes keys continuously and **logs every write it issues to its own file** — this independent record is what makes verification meaningful, not just "the server didn't crash." A driver loop, `N` times (200, matching the production plan's own pseudocode, scaled to fit an "overnight" wall-clock budget): pick leader or follower at random, `kill -9` it, wait a random 0–30s, restart it, wait for it to finish recovery/resync, then diff the **actual** recovered/resynced keyspace (via `KEYS *` + value reads) against the load generator's independent write log. Any mismatch fails the run immediately with the iteration number and the specific key(s) that diverged.

**Output:** `docs/chaos/<date>-chaos-log.md`, mirroring `docs/benchmarks/`'s precedent — committed with the real log from one full overnight run, including the total iteration count, wall-clock duration, and (if any occurred) exact failure detail rather than a hand-waved "it passed."

---

## Decision: documentation scope and location

Top-level `docs/`, matching `docs/benchmarks/`'s existing precedent (not `docs/superpowers/`, which stays planning-artifacts-only per this project's own convention):

- **`docs/getting-started.md`** — install/build, minimal `rocket-mem.toml`, first `redis-cli`/`rmp-client` session, where to go next.
- **`docs/config-reference.md`** — every `Config` field from the decision above: TOML key, env var name, CLI flag, default, one-line meaning. Generated by hand once, not auto-generated (no doc-generation tooling added this sprint).
- **`docs/command-compatibility.md`** — full command table (already tracked informally in the README's command-coverage section per Sprint 3's DoD) reorganized as: command, supported, and any noted divergence from real Redis (the kind of gap already called out inline through Sprints 4–7 — `SCAN` cursor semantics, `SLOWLOG`'s 4 vs 6 fields, `expired_keys` active-only counting, etc. — collected in one place instead of scattered across sprint specs).
- **`docs/architecture.md`** — the three-layer story (Protocol → Command Dispatcher → Storage Engine) pulled together from the production plan's ADR, `docs/design/sharding-decision.md`, and the "why `dispatch_and_log` is the one place command behavior lives" invariant this sprint's ACL work depended on directly.

README gains short pointers to each, not inline copies.

---

## Decision: Docker image + release pipeline extension

**`Dockerfile`** (new, repo root): multi-stage — a `rust:1-slim` (or pinned equivalent) build stage running `cargo build --release --bin rocket-mem`, then a minimal runtime stage (`debian:bookworm-slim` or `gcr.io/distroless/cc`) copying only the built binary, running as a non-root user, `EXPOSE 6379 6380 9121`, default `CMD` pointing at `ROCKET_MEM_ADDR`-style defaults so `docker run rocket-mem` "just works" per the production plan's own DoD wording.

**Release workflow:** extends the *existing* `.github/workflows/release.yml` (already builds/signs/publishes cross-platform binaries on `v*.*.*` tags) with one additional job — build the `Dockerfile` and push to `ghcr.io/<owner>/rocket-mem:<tag>` plus `:latest` on the same tag trigger, using the workflow's existing `GITHUB_TOKEN` (already has `contents: write`; needs `packages: write` added). No new workflow file, no new external account/secret.

---

## Testing strategy

- **ACL unit tests** (`crates/server/src/acl.rs`): rule parsing (valid and malformed `ACL SETUSER` token streams), `is_allowed`'s left-to-right last-rule-wins semantics (including the production plan's own `+get -set` example), password hash/verify round-trip, `nopass` accepting any password, an `AllKeys` rule short-circuiting per-key pattern checks.
- **Session/auth-gate tests** (`crates/server/src/dispatcher.rs`): empty `AclStore` ⇒ every command passes through untouched (the critical backward-compatibility guarantee — a dedicated test, not an incidental one); `NOAUTH` on an unauthenticated connection once a user exists; `NOPERM` for a command/key outside the authenticated user's rules; `AUTH`/`ACL *` always reachable regardless of auth state.
- **RMP session-sharing integration test** (`crates/server/tests/rmp.rs`): `AUTH` on one request, followed by a second, concurrently-dispatched request on the *same* RMP connection, proving the second sees the first's authentication — the test that would have caught the pre-Session per-request-`Protocol::default()` gap.
- **TLS integration test** (`crates/server/tests/tls.rs`, new): a real `tokio-rustls` client connects and completes a `GET`/`SET` round trip against the TLS listener; a plain `TcpStream` pointed at the TLS-only port connects but never gets a valid reply (the production plan's own named test).
- **Config layering test** (`crates/server/src/config.rs`): the production plan's own named example — CLI flag overrides env var overrides file value overrides default, asserted for at least one field of each kind (a string and a numeric).
- **Chaos test:** verified by running `scripts/chaos.sh` for a full pass and committing its log, per the Decision above — not a `cargo test` target.

## Definition of done

(Concretizes `../../rocket-mem-sprint-plan.md`'s Sprint 8 DoD.)

- [ ] This spec doc committed
- [ ] `acl.rs` implemented with the full unit test suite above; `AclStore` wired onto `ReplicationHandle`
- [ ] `Session` replaces `Protocol` in `dispatch_and_log`'s signature across both `connection.rs` and `rmp_connection.rs`; RMP session-sharing test passes
- [ ] `AUTH`/`ACL SETUSER/DELUSER/LIST/WHOAMI/GETUSER` reachable over both RESP and RMP, with zero behavior change when no ACL users are configured
- [ ] TLS listeners implemented for both RESP and RMP; TLS integration test (including the plaintext-rejected-at-a-TLS-port case) passes
- [ ] `config.rs` implemented; config-layering precedence test passes; `main.rs` fully migrated off direct `std::env::var` calls
- [ ] `scripts/chaos.sh` run to completion at least once; its log committed to `docs/chaos/`, zero corruption incidents
- [ ] `docs/getting-started.md`, `docs/config-reference.md`, `docs/command-compatibility.md`, `docs/architecture.md` written; README links to each
- [ ] `Dockerfile` builds and `docker run` serves traffic on the default port; release workflow's new job pushes to `ghcr.io` on a tag push
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace` all green
- [ ] `Cargo.toml`'s workspace version bumped to `1.0.0`; `v1.0.0` tagged

## Plan breakdown

Maps to `../plans/2026-08-31-sprint-8-plans/`. Dependency order:

1. **Config layering** (`config.rs`, `figment`/`clap`, `main.rs` migration) — foundational; TLS and ACL bootstrap both read `Config` fields this plan defines. No dependency on anything else in this list.
2. **ACL/AUTH system** (`acl.rs`, `Session`, the `dispatch_and_log_inner` auth gate, `AUTH`/`ACL *` command interceptions) — depends on (1) for bootstrap-config parsing; independent of (3).
3. **TLS** (`tls.rs`, generic `handle_connection`, the two new TLS listeners) — depends on (1) for cert/key path config; independent of (2).
4. **Chaos test** (`scripts/chaos.sh`, the load-generator binary, the committed log) — depends on nothing new from (1)–(3) beyond an optional config file for convenience; exercises Sprints 4–5's already-shipped durability/replication.
5. **Documentation** (`docs/getting-started.md`, `docs/config-reference.md`, `docs/command-compatibility.md`, `docs/architecture.md`, README updates) — depends on (1)–(4) being complete, since it documents the finished feature set.
6. **Docker + release** (`Dockerfile`, `release.yml` extension) — depends on (1) for sane container defaults; sequenced near-last since it packages the finished binary.
7. **Sprint close** (version bump to `1.0.0`, `../../rocket-mem-sprint-plan.md` status tick, final full-workspace verification, `v1.0.0` tag) — depends on 1–6.
