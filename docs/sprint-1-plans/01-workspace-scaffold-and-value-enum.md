# Workspace Scaffold & Value Enum Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** stand up the Cargo workspace with four crates and define the `Value` enum every data-type command will operate on.

**Architecture:** a Cargo workspace with `common`, `engine`, `protocol`, `server` crates. `protocol` and `server` are empty placeholders this sprint — Sprint 2 fills them in. See `00-sprint-1-spec.md` for the full layout and rationale.

**Tech Stack:** Rust workspace, `bytes` for zero-copy byte buffers, `thiserror` for error types.

---

### Task 1: Cargo workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `crates/common/Cargo.toml`, `crates/common/src/lib.rs`
- Create: `crates/engine/Cargo.toml`, `crates/engine/src/lib.rs`
- Create: `crates/protocol/Cargo.toml`, `crates/protocol/src/lib.rs`
- Create: `crates/server/Cargo.toml`, `crates/server/src/main.rs`

- [ ] **Step 1: Create the workspace root manifest**

```toml
# Cargo.toml
[workspace]
resolver = "2"
members = ["crates/common", "crates/engine", "crates/protocol", "crates/server"]

[workspace.package]
edition = "2021"
version = "0.1.0"

[workspace.dependencies]
bytes = "1"
thiserror = "1"
tracing = "0.1"
parking_lot = "0.12"
```

- [ ] **Step 2: Create the `common` crate**

```toml
# crates/common/Cargo.toml
[package]
name = "common"
edition.workspace = true
version.workspace = true

[dependencies]
thiserror.workspace = true
```

```rust
// crates/common/src/lib.rs
#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
    #[error("value is not an integer or out of range")]
    NotAnInteger,
}
```

- [ ] **Step 3: Create the `engine` crate shell**

```toml
# crates/engine/Cargo.toml
[package]
name = "engine"
edition.workspace = true
version.workspace = true

[dependencies]
common = { path = "../common" }
bytes.workspace = true
parking_lot.workspace = true
```

```rust
// crates/engine/src/lib.rs
mod value;
pub use value::Value;
```

- [ ] **Step 4: Create empty `protocol` and `server` placeholders**

```toml
# crates/protocol/Cargo.toml
[package]
name = "protocol"
edition.workspace = true
version.workspace = true
```

```rust
// crates/protocol/src/lib.rs
// Built in Sprint 2 — RESP parser/encoder lives here.
```

```toml
# crates/server/Cargo.toml
[package]
name = "rocket-mem"
edition.workspace = true
version.workspace = true

[dependencies]
engine = { path = "../engine" }
```

Note: the folder stays `crates/server` (named by responsibility, matching the other crates), but the package itself is named `rocket-mem` since it produces the final binary — `cargo run --bin rocket-mem` is what someone runs to start the server.

```rust
// crates/server/src/main.rs
fn main() {
    // Networking built in Sprint 2.
}
```

- [ ] **Step 5: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: all four crates compile with no errors

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/
git commit -m "chore: scaffold workspace with common/engine/protocol/server crates"
```

---

### Task 2: `Value` enum

**Files:**
- Create: `crates/engine/src/value.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/engine/src/value.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_values_compare_equal_by_content() {
        let a = Value::String(Bytes::from_static(b"bar"));
        let b = Value::String(Bytes::from_static(b"bar"));
        assert_eq!(a, b);
    }

    #[test]
    fn different_variants_are_not_equal() {
        let s = Value::String(Bytes::from_static(b"x"));
        let l = Value::List(VecDeque::new());
        assert_ne!(s, l);
    }

    #[test]
    fn type_name_matches_redis_naming() {
        assert_eq!(Value::String(Bytes::new()).type_name(), "string");
        assert_eq!(Value::List(VecDeque::new()).type_name(), "list");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine value::tests`
Expected: FAIL — `Value` is not defined yet

- [ ] **Step 3: Write the implementation**

```rust
// crates/engine/src/value.rs (above the test module)
use bytes::Bytes;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Hash(_) => "hash",
            Value::Set(_) => "set",
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p engine value::tests`
Expected: PASS, 3/3

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/value.rs
git commit -m "feat(engine): add Value enum for string/list/hash/set"
```
