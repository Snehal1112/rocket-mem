# Verbose Logging Plan 04: `log_value_max_bytes` Config Field

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the `log_value_max_bytes` configuration field that supplies the `cap` argument to `fmt_value` and `redact_args`, wired through the existing four-layer figment precedence.

**Architecture:** One new `u64` field on `Config`, following the exact pattern `slowlog_threshold_micros` already uses. Numeric fields cannot go through the `set!` macro (it calls `Value::from(v.as_str())`, which produces a `Value::String` that fails to extract into a `u64`) — they need the explicit `map.insert` form instead. Getting this wrong produces a confusing `invalid type: found string, expected u64` at load time.

**Tech Stack:** Rust 2021, `figment 0.10` (TOML + env providers), `clap 4` (derive).

**Spec:** [`../../specs/2026-09-09-verbose-logging-design.md`](../../specs/2026-09-09-verbose-logging-design.md) — see the "Configuration" section.

## Global Constraints

Identical to [plan 01](01-baseline-and-dependencies.md#global-constraints); re-read that section before starting.

---

### Task 1: Add the field and its layering

**Files:**
- Modify: `crates/server/src/config.rs` — `Config` struct (near `log_level`, ~line 31), `Config::default` (~line 53), `Cli` struct (~line 164), `cli_overrides` (~line 206)
- Test: `crates/server/src/config.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Config::log_value_max_bytes: u64`, default `128`. Plan 08's per-command `trace!` site reads it; `main.rs` threads it to the dispatcher in plan 08.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/server/src/config.rs`:

```rust
    #[test]
    fn log_value_max_bytes_defaults_to_128() {
        figment::Jail::expect_with(|jail| {
            let cfg = load_layered(None).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 128);
            Ok(())
        });
    }

    #[test]
    fn log_value_max_bytes_is_layered_like_every_other_numeric_field() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rocket-mem.toml", "log_value_max_bytes = 64\n")?;
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 64);

            jail.set_env("ROCKET_MEM_LOG_VALUE_MAX_BYTES", "32"); // env beats file
            let cfg = load_layered(Some(std::path::Path::new("rocket-mem.toml"))).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 32);

            let cli = Cli::parse_from([
                "rocket-mem",
                "--config",
                "rocket-mem.toml",
                "--log-value-max-bytes",
                "16", // CLI beats env
            ]);
            let cfg = load_with_cli(cli).unwrap();
            assert_eq!(cfg.log_value_max_bytes, 16);
            Ok(())
        });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p rocket-mem config::tests::log_value_max_bytes
```

Expected: FAIL to compile with `no field 'log_value_max_bytes' on type 'Config'`.

- [ ] **Step 3: Add the field in all four places**

**3a.** In the `Config` struct in `crates/server/src/config.rs`, directly beneath the existing `pub log_level: String,`:

```rust
    /// Maximum bytes of a value or argument rendered into a `trace`-level log line before
    /// truncation. Only consulted at `trace` -- lower it to keep trace logs readable, raise
    /// it to see whole values. See `logging::fmt_value`.
    pub log_value_max_bytes: u64,
```

**3b.** In `Config::default`, beneath `log_level: "info".to_string(),`:

```rust
            log_value_max_bytes: 128,
```

**3c.** In the `Cli` struct, beneath the existing `log_level` flag:

```rust
    /// Max bytes of a value rendered into a trace-level log line [default: 128]
    #[arg(long)]
    pub log_value_max_bytes: Option<u64>,
```

**3d.** In `cli_overrides`, **not** via the `set!` macro — that macro calls `Value::from(v.as_str())` and only works for `Option<String>` fields. Use the explicit numeric form, directly beside the existing `slowlog_threshold_micros` block:

```rust
    if let Some(v) = cli.log_value_max_bytes {
        map.insert("log_value_max_bytes", Value::from(v));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p rocket-mem config::tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: both new tests PASS alongside every pre-existing config test, fmt clean, clippy clean.

If you see `invalid type: found string, expected u64`, step 3d was done with the `set!` macro instead of the explicit `map.insert` — go back and fix it.

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/config.rs
git commit -m "feat(config): add log_value_max_bytes field"
```

---

### Task 2: Document the field and the trace-level data warning

**Files:**
- Modify: `docs/config-reference.md` (the config table, beside the existing `log_level` row)
- Modify: `README.md` (the config table, ~line 274)

**Interfaces:**
- Consumes: the field from Task 1.
- Produces: nothing consumed by later plans. Plan 21 re-checks these docs are still accurate at the end of the series.

- [ ] **Step 1: Add the row to `docs/config-reference.md`**

Directly beneath the existing `log_level` row, matching that table's column layout (TOML key, env var, CLI flag, default, description):

```markdown
| `log_value_max_bytes` | `ROCKET_MEM_LOG_VALUE_MAX_BYTES` | `--log-value-max-bytes` | `128` | Maximum bytes of a value or command argument rendered into a `trace`-level log line before truncation. Only consulted at `trace`. |
```

Then add this warning as a short prose paragraph directly beneath that table:

```markdown
> **`trace` writes your data to disk.** At `trace`, rocket-mem logs command arguments and
> value contents, so a trace-level log file is a plaintext copy of the dataset and every
> mutation applied to it, capped per value by `log_value_max_bytes`. Credentials are always
> redacted (`AUTH`, `HELLO ... AUTH`, `ACL SETUSER`, `ACL GETUSER`), but ordinary values are
> not. Treat a trace log with the same retention and access controls as the data itself.
```

- [ ] **Step 2: Add the row to `README.md`**

`README.md`'s config table has fewer columns than `docs/config-reference.md` (key, default, description). Directly beneath its `log_level` row at ~line 274:

```markdown
| `log_value_max_bytes` | `128` | Max bytes of a value rendered into a `trace` log line; `trace` logs your data in plaintext |
```

- [ ] **Step 3: Verify the docs render and nothing else drifted**

```bash
grep -n "log_value_max_bytes" README.md docs/config-reference.md
```

Expected: one hit in `README.md`, one in `docs/config-reference.md`.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/config-reference.md
git commit -m "docs: document log_value_max_bytes and the trace-level data warning"
```

---

## Next plan

[`05-resp-connection-span.md`](05-resp-connection-span.md) — opens the `conn` span on the RESP connection handler, the outermost span every later log line inherits its `conn_id` and `peer` from.
