# Config-file `replicaof` — Spec

## Why

`REPLICAOF` today has no config-file equivalent — it is always a live command sent to an
already-running node (`crates/server/src/dispatcher.rs:1521`, `handle_replicaof`). This is a
documented, deliberate Sprint 5 decision, not an oversight: `docs/superpowers/specs/2026-08-30-sprint-5-spec.md`
states *"the replica role is not persisted — it lives only in `ReplicationHandle`. A restarted
follower comes back as a standalone node holding whatever its own snapshot/AOF say, until a
client sends `REPLICAOF` again"* and calls this out explicitly as *"a real footgun ... called out
here rather than discovered later"*.

That footgun is exactly what this feature closes: a `replicaof` field in the TOML config lets a
node auto-connect to its leader on every startup (first boot or restart-after-crash) without an
operator or external script re-issuing the command. It does not change anything about how
`REPLICAOF` itself behaves once connected — it is purely a new startup-time trigger for the
*existing* `ReplicationHandle::start_replicating_with_auth` call.

**Out of scope:** persisting a runtime `REPLICAOF`/`REPLICAOF NO ONE` change *back* into the TOML
file (that is the separate `CONFIG REWRITE`-equivalent feature, its own future spec) and any form
of automatic failover/promotion (also separate, future work). This spec is one-directional:
file → startup behavior, nothing writes the file.

## Decision: three flat top-level fields, not a nested `[replicaof]` table

```toml
replicaof = "127.0.0.1:6400"
replicaof_auth_username = "app"       # optional
replicaof_auth_password = "changeme"  # optional
```

Mirrors this codebase's existing `tls_cert_path`/`tls_key_path`/`tls_ca_path` convention (three
flat, independently-optional strings) rather than `[[acl.users]]`'s array-of-tables shape, because
there is no repetition here — exactly one `replicaof` target per node, same cardinality as the TLS
paths. This also keeps the fields overridable through the existing `Cli`/`set!` macro and
`ROCKET_MEM_REPLICAOF`/`ROCKET_MEM_REPLICAOF_AUTH_USERNAME`/`ROCKET_MEM_REPLICAOF_AUTH_PASSWORD`
env vars for free — a nested table would not flatten cleanly through figment's `Env` provider
(the existing ACL doc comment in `config.rs` already notes this exact limitation for arrays/nesting).

Add all three fields in the three places every existing `Config` field requires
(`crates/server/src/config.rs`, confirmed no compile-time check catches a missed one):
1. `Config` struct (`config.rs`, alongside the other flat `Option<String>` fields).
2. `Cli` struct (all-`Option`, one per field, exact same pattern as `tls_cert_path`).
3. A `set!(...)` call inside `cli_overrides` for each of the three fields.

Dedicated auth fields, not a lookup into `[[acl.users]]`: `REPLICAOF ... AUTH user pass` already
takes freeform credentials structurally unrelated to this node's own `AclStore` (confirmed at
`dispatcher.rs:1521-1576` → `replication.rs:322`, `start_replicating_with_auth`) — these are
credentials this node *presents to* a leader, not an account it accepts logins as. Conflating the
two would be a modeling error even though it would occasionally save typing two extra lines.

## Decision: validation — fail-hard on a malformed pair, fail-soft on a bad target

Add `validate_replicaof(config: &Config) -> std::io::Result<()>` in `config.rs`, next to the
existing `validate_tls` (not inline in `main.rs`'s match, unlike the `cluster_config`/
`cluster_node_id` precedent) — because this is a leaf-level auth-pair check structurally identical
to the TLS cert/key pair, not a mutually-exclusive top-level mode switch like cluster mode is:

```rust
pub fn validate_replicaof(config: &Config) -> std::io::Result<()> {
    let has_username = config.replicaof_auth_username.is_some();
    let has_password = config.replicaof_auth_password.is_some();
    if has_username != has_password {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "replicaof_auth_username and replicaof_auth_password must both be set, or neither",
        ));
    }
    Ok(())
}
```

`main.rs` calls this and propagates the error with `?` before any listener binds — same hard-abort
convention as `validate_tls`. A `replicaof` value with a missing/unparseable port, or a host that
doesn't resolve, is **not** validated here — that is only discoverable by actually attempting the
connection, exactly like the existing live `REPLICAOF` command already behaves (no host/port
validation at parse time, confirmed at `dispatcher.rs:1573` — it string-formats `"{host}:{port}"`
and lets the connection attempt fail or succeed asynchronously). A bad `replicaof` value at startup
therefore fails soft: the background reconnect loop logs a warning and retries every second
forever, same as any other unreachable leader — it does not block startup or crash the process.

## Decision: startup wiring — fire-and-forget, right after `ReplicationHandle` is built

In `main.rs`, immediately after `let replication = Arc::new(handle);` (the point every builder
method — `with_aof`, `with_own_addr`, `with_replication_tls_client_config`, `with_cluster`,
`with_acl_bootstrap`, `with_slowlog_threshold` — has already been applied):

```rust
if let Some(target) = &config.replicaof {
    let auth = match (&config.replicaof_auth_username, &config.replicaof_auth_password) {
        (Some(u), Some(p)) => Some((u.clone(), p.clone())),
        _ => None,
    };
    replication.start_replicating_with_auth(target.clone(), auth);
}
```

Confirmed safe to place before the listeners bind or `serve()` is called: `start_replicating_with_auth`
(`replication.rs:322-355`) only mutates in-memory state and spawns its own `tokio::spawn`ed task —
it never awaits the connection itself, so call-site ordering relative to `serve()` has no
functional effect. Confirmed safe against "leader not up yet at startup": `connect_and_sync`'s
`TcpStream::connect` failure (`replication.rs:548`) is a plain `?` that returns an `Err` which
`replication_client_loop`'s `match` (`replication.rs:513-528`) treats identically to a later
mid-stream disconnect — log, `link_up = false`, sleep 1s, retry. No special-casing needed for the
startup case; it already falls into the existing reconnect loop.

## Decision: update the startup banner

`main.rs`'s startup banner (`main.rs:343-366`) currently only reports *inbound* replicas
(`replication.registry.addrs()`) and its own comment explicitly claims *"this node is never itself
a replica of anything yet at the moment this banner prints"* — true before this feature, false
after it. Add one banner line reporting outbound role when `config.replicaof` is set:

```
Replicating from 127.0.0.1:6400 (auth: app)     # or "(no auth)" if unset
```

placed alongside the existing "replicas" line, and update the now-stale comment to note the
exception this feature introduces.

## Testing convention

Two kinds of tests, matching this codebase's existing split:

1. **Config-layer unit tests** (`config.rs`'s own `#[cfg(test)] mod tests`) — `validate_replicaof`
   accepts both-set, both-unset, rejects username-without-password and password-without-username;
   `load_layered`/`cli_overrides` round-trip all three new fields through TOML, env, and CLI
   layers, mirroring the existing `tls_cert_path` coverage there.
2. **One real integration test** (`crates/server/tests/replication.rs`, alongside
   `a_follower_syncs_from_an_acl_protected_leader_when_replicaof_auth_is_used`) — spawn a leader
   with ACL configured, spawn a follower via `rocket_mem::serve` but with a `Config` carrying
   `replicaof`/`replicaof_auth_username`/`replicaof_auth_password` pointed at that leader (not
   calling `start_replicating` by hand), and assert it links up and replicates a write, purely
   from config — proving the wiring end-to-end, not just the field parsing.

## Definition of done

- [ ] `replicaof`, `replicaof_auth_username`, `replicaof_auth_password` exist in `Config`, `Cli`,
      and are wired through `cli_overrides`'s `set!` macro.
- [ ] `validate_replicaof` exists in `config.rs`, is called from `main.rs` before any listener
      binds, and hard-fails startup only on a mismatched auth pair.
- [ ] `main.rs` calls `start_replicating_with_auth` right after building `ReplicationHandle` when
      `config.replicaof` is set.
- [ ] Startup banner reports outbound replication target when configured; stale comment at
      `main.rs:343-349` updated.
- [ ] Config-layer unit tests for `validate_replicaof` and layering pass.
- [ ] The new `replication.rs` integration test (config-driven follower links up and replicates)
      passes.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace` all clean.
- [ ] `docs/config-reference.md` and `.claude/manual-testing.md`'s "Replication (`REPLICAOF`)"
      section updated to document the new fields and the fact that `REPLICAOF` now has a
      config-file equivalent for the initial-connect case (state-changes via the live command are
      still not persisted — that remains the separate `CONFIG REWRITE`-equivalent feature).
