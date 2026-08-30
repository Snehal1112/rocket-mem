# Slow Log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** commands slower than a configurable threshold land in a bounded in-memory ring buffer an operator can read with `SLOWLOG GET`, size with `SLOWLOG LEN`, and clear with `SLOWLOG RESET`.

**Architecture:** a new `crates/server/src/slowlog.rs` holds a fixed 128-entry `VecDeque` behind a `std::sync::Mutex`, owned by `ReplicationHandle` (constructed by `new`/`Default`, so no existing call site changes). The recording hook is one line in the `dispatch_and_log` wrapper that `04-prometheus-metrics.md` created — the same single place command latency is already measured. `SLOWLOG` itself is a dispatcher interception, like `CLUSTER`/`INFO`.

**Tech Stack:** `std` only, plus the `metrics` facade already added in plan 04 for the `rocket_mem_slowlog_entries_total` counter.

**Spec:** ../../specs/2026-08-30-sprint-6-spec.md — "the slow log is a fixed 128-entry ring buffer holding the command name and its first key" is authoritative for this plan. Depends on `04-prometheus-metrics.md` (the wrapper and its `elapsed`).

## Global Constraints

- **Entries carry the command name and its first argument, not the full argument list.** `dispatch` consumes the `Frame` (`frame_to_args` moves each `Bulk`'s `Bytes` out rather than cloning), so anything the slow log wants must be captured *before* the call — i.e. on every command, for the benefit of the rare slow one. Cloning one `Bytes` is a refcount bump; cloning the whole frame is a `Vec` allocation plus N bumps on the hot path `07-benchmark-and-flamegraph.md` exists to shrink. The first argument is the key for ~70 of the 84 commands, which is what an operator actually needs.
- **Entries have 4 fields, not real Redis's 6.** The client address and client name are omitted: `dispatch_and_log` never learns the peer address (`handle_connection` has the `TcpStream` but passes only `client_id` down), and threading a `SocketAddr` through six call layers for two cosmetic fields is not worth it this sprint. Clients that index positionally read the same first four either way.
- **A threshold of `0` means disabled here**, deliberately diverging from real Redis, where `slowlog-log-slower-than 0` means "log everything": filling a 128-entry ring from every command would evict itself faster than an operator could read it, and someone who wants per-command timings already has `rocket_mem_command_duration_seconds`.
- The buffer is capacity-bounded and never grows: the oldest entry is dropped when a new one arrives at capacity.

---

### Task 1: the `SlowLog` ring buffer

**Files:**
- Create: `crates/server/src/slowlog.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub const SLOWLOG_CAPACITY: usize`, `pub struct SlowLogEntry { pub id: u64, pub unix_time_secs: i64, pub duration_micros: i64, pub command: String, pub key: Option<Bytes>, pub arg_count: usize }`, and `SlowLog::{with_threshold, maybe_record, get, len, is_empty, reset}` plus `impl Default for SlowLog`. Consumed by Tasks 2 and 3.

- [x] **Step 1: Declare the module**

```rust
// crates/server/src/lib.rs — add the module, keeping the list alphabetical
pub mod aof;
pub mod cluster;
pub mod connection;
pub mod dispatcher;
pub mod metrics;
pub mod replication;
pub mod slowlog;
pub use connection::serve;
```

- [x] **Step 2: Write the failing tests**

```rust
// crates/server/src/slowlog.rs — the whole file, for now just the tests
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(s: &'static [u8]) -> Option<Bytes> {
        Some(Bytes::from_static(s))
    }

    #[test]
    fn a_command_under_the_threshold_is_not_recorded() {
        let log = SlowLog::with_threshold(Duration::from_millis(10));
        log.maybe_record("GET", key(b"k"), 1, Duration::from_micros(50));
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn a_command_at_or_over_the_threshold_is_recorded_with_its_details() {
        let log = SlowLog::with_threshold(Duration::from_millis(10));
        log.maybe_record("LRANGE", key(b"mylist"), 3, Duration::from_millis(25));
        let entries = log.get(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 0);
        assert_eq!(entries[0].command, "LRANGE");
        assert_eq!(entries[0].key, key(b"mylist"));
        assert_eq!(entries[0].arg_count, 3);
        assert_eq!(entries[0].duration_micros, 25_000);
        assert!(entries[0].unix_time_secs > 1_700_000_000);
    }

    #[test]
    fn a_zero_threshold_disables_recording_entirely() {
        let log = SlowLog::with_threshold(Duration::ZERO);
        log.maybe_record("GET", key(b"k"), 1, Duration::from_secs(5));
        assert!(log.is_empty());
    }

    #[test]
    fn get_returns_the_newest_entries_first_and_respects_count() {
        let log = SlowLog::with_threshold(Duration::from_micros(1));
        for i in 0..5u32 {
            log.maybe_record("SET", None, i as usize, Duration::from_millis(1));
        }
        let all = log.get(100);
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].id, 4, "newest first");
        assert_eq!(all[4].id, 0);
        let two = log.get(2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].id, 4);
        assert_eq!(two[1].id, 3);
    }

    #[test]
    fn the_buffer_is_bounded_and_drops_the_oldest_entries() {
        let log = SlowLog::with_threshold(Duration::from_micros(1));
        for _ in 0..(SLOWLOG_CAPACITY + 10) {
            log.maybe_record("SET", None, 2, Duration::from_millis(1));
        }
        assert_eq!(log.len(), SLOWLOG_CAPACITY);
        let entries = log.get(SLOWLOG_CAPACITY);
        assert_eq!(entries[0].id as usize, SLOWLOG_CAPACITY + 9);
        assert_eq!(entries[SLOWLOG_CAPACITY - 1].id, 10);
    }

    #[test]
    fn reset_clears_the_entries_but_ids_keep_counting_up() {
        let log = SlowLog::with_threshold(Duration::from_micros(1));
        log.maybe_record("SET", None, 2, Duration::from_millis(1));
        log.reset();
        assert!(log.is_empty());
        log.maybe_record("SET", None, 2, Duration::from_millis(1));
        // ids are monotonic across a reset, matching real Redis -- an operator correlating a
        // logged id with a later GET must not find it reused.
        assert_eq!(log.get(1)[0].id, 1);
    }

    #[test]
    fn the_default_threshold_is_ten_milliseconds() {
        let log = SlowLog::default();
        log.maybe_record("GET", None, 1, Duration::from_millis(9));
        assert!(log.is_empty());
        log.maybe_record("GET", None, 1, Duration::from_millis(10));
        assert_eq!(log.len(), 1);
    }
}
```

- [x] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem slowlog::tests`
Expected: FAIL to compile with "cannot find type `SlowLog`"

- [x] **Step 4: Implement `SlowLog`**

```rust
// crates/server/src/slowlog.rs — add above the tests module
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// How many entries the ring holds before the oldest is dropped. Fixed rather than configurable:
/// a slow log is something an operator reads interactively, and 128 is already more than fits on
/// a screen. Making it configurable would be one more knob with no decision behind it.
pub const SLOWLOG_CAPACITY: usize = 128;

/// One recorded slow command. Four fields, not real Redis's six -- see this plan's Global
/// Constraints for why the client address and name are omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowLogEntry {
    /// Monotonic and never reused, including across `reset`.
    pub id: u64,
    pub unix_time_secs: i64,
    pub duration_micros: i64,
    /// The uppercase command name.
    pub command: String,
    /// The command's first argument -- the key for ~70 of the 84 commands.
    pub key: Option<Bytes>,
    /// How many arguments followed the command name, so `SLOWLOG GET` can render real Redis's
    /// `... (N more arguments)` truncation marker for the ones it doesn't carry.
    pub arg_count: usize,
}

/// A bounded ring of recently-slow commands. `entries` is a plain `std::sync::Mutex`: every
/// access is a push or a drain measured in nanoseconds and never held across an `.await`,
/// matching `ReplicaRegistry`'s choice for the same reason.
pub struct SlowLog {
    entries: Mutex<VecDeque<SlowLogEntry>>,
    next_id: AtomicU64,
    threshold: Duration,
}

impl SlowLog {
    pub fn with_threshold(threshold: Duration) -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(SLOWLOG_CAPACITY)),
            next_id: AtomicU64::new(0),
            threshold,
        }
    }

    /// Records `command` if it took at least the configured threshold. A no-op otherwise, which
    /// is the overwhelmingly common case -- this is the only slow-log work on the hot path.
    /// `Duration::ZERO` means disabled, not "record everything"; see this plan's Global
    /// Constraints.
    pub fn maybe_record(
        &self,
        command: &str,
        key: Option<Bytes>,
        arg_count: usize,
        elapsed: Duration,
    ) {
        if self.threshold.is_zero() || elapsed < self.threshold {
            return;
        }
        let entry = SlowLogEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            unix_time_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            duration_micros: elapsed.as_micros().min(i64::MAX as u128) as i64,
            command: command.to_string(),
            key,
            arg_count,
        };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() == SLOWLOG_CAPACITY {
            entries.pop_front();
        }
        entries.push_back(entry);
        ::metrics::counter!("rocket_mem_slowlog_entries_total").increment(1);
    }

    /// Up to `count` entries, newest first -- the order real Redis returns them in.
    pub fn get(&self, count: usize) -> Vec<SlowLogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .take(count)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Required by `clippy::len_without_is_empty`, which `-D warnings` makes a hard error.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reset(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// 10ms, the same order of magnitude as real Redis's own 10ms default. Used by
/// `ReplicationHandle::new`/`Default`, so tests get sane behavior without touching the
/// environment; `main.rs` overrides it from `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS`.
impl Default for SlowLog {
    fn default() -> Self {
        Self::with_threshold(Duration::from_millis(10))
    }
}
```

- [x] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem slowlog::tests`
Expected: PASS, all 7 tests

- [x] **Step 6: Commit**

```bash
git add crates/server/src/lib.rs crates/server/src/slowlog.rs
git commit -m "feat(server): add a bounded slow-log ring buffer"
```

---

### Task 2: hook it into the dispatch wrapper

**Files:**
- Modify: `crates/server/src/replication.rs` (struct at `:42`, `new` at `:93`)
- Modify: `crates/server/src/dispatcher.rs` (the `dispatch_and_log` wrapper from `04-prometheus-metrics.md`)
- Modify: `crates/server/src/main.rs`

**Interfaces:**
- Consumes: `SlowLog` (Task 1), `command_name_upper` and the wrapper's `elapsed` (`04-prometheus-metrics.md`).
- Produces: `ReplicationHandle::slowlog` (a public field) and `ReplicationHandle::with_slowlog_threshold(mut self, threshold: Duration) -> Self`, plus `fn command_key_and_arity(frame: &Frame) -> (Option<Bytes>, usize)` in `dispatcher.rs`. Consumed by Task 3.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    /// A handle whose slow-log threshold is 1ns, so every command qualifies. Nothing else about
    /// it differs from `ReplicationHandle::default()`.
    fn slowlog_handle() -> ReplicationHandle {
        ReplicationHandle::default()
            .with_slowlog_threshold(std::time::Duration::from_nanos(1))
    }

    #[test]
    fn command_key_and_arity_reads_the_first_argument_and_the_count() {
        assert_eq!(
            command_key_and_arity(&cmd(&[b"SET", b"k", b"v"])),
            (Some(Bytes::from_static(b"k")), 2)
        );
        assert_eq!(command_key_and_arity(&cmd(&[b"PING"])), (None, 0));
        assert_eq!(
            command_key_and_arity(&cmd(&[b"LRANGE", b"mylist", b"0", b"-1"])),
            (Some(Bytes::from_static(b"mylist")), 3)
        );
        assert_eq!(command_key_and_arity(&Frame::Simple("x".into())), (None, 0));
    }

    #[test]
    fn a_slow_command_is_recorded_with_its_name_key_and_arity() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let entries = replication.slowlog.get(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "SET");
        assert_eq!(entries[0].key, Some(Bytes::from_static(b"k")));
        assert_eq!(entries[0].arg_count, 2);
    }

    #[test]
    fn a_fast_command_is_not_recorded_at_the_default_threshold() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default(); // 10ms threshold
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PING"]),
            &mut Protocol::default(),
            1,
        );
        assert!(replication.slowlog.is_empty());
    }
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::command_key_and_arity dispatcher::tests::a_slow_command dispatcher::tests::a_fast_command`
Expected: FAIL to compile with "no method named `with_slowlog_threshold`"/"no field `slowlog`"/"cannot find function `command_key_and_arity`"

- [x] **Step 3: Add the field and the builder**

```rust
// crates/server/src/replication.rs — add as a field of `pub struct ReplicationHandle`
    /// Recently-slow commands, recorded by the `dispatch_and_log` wrapper. A plain field, not an
    /// `Option`: it is always present and always cheap when nothing is slow, so there is nothing
    /// to configure away. `main.rs` sets its threshold from the environment via
    /// `with_slowlog_threshold`; `new`/`Default` use the 10ms default.
    pub slowlog: crate::slowlog::SlowLog,
```

```rust
// crates/server/src/replication.rs — add to `new`'s struct literal (:93)
            slowlog: crate::slowlog::SlowLog::default(),
```

```rust
// crates/server/src/replication.rs — add to the existing `impl ReplicationHandle` block,
// beside `with_aof`/`with_cluster`
    /// Overrides the slow-log threshold. `Duration::ZERO` disables recording entirely -- see
    /// ../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md for why that differs from real
    /// Redis's meaning for 0.
    pub fn with_slowlog_threshold(mut self, threshold: std::time::Duration) -> Self {
        self.slowlog = crate::slowlog::SlowLog::with_threshold(threshold);
        self
    }
```

- [x] **Step 4: Record from the wrapper**

```rust
// crates/server/src/dispatcher.rs — add beside `command_name_upper`
/// The command's first argument (cloned -- one `Bytes` refcount bump, no data copy) and how many
/// arguments followed the name. Read before `frame` is moved into `dispatch_and_log_inner`,
/// because `dispatch` consumes the frame; see this plan's Global Constraints for why the slow log
/// carries this instead of the whole argument list.
fn command_key_and_arity(frame: &Frame) -> (Option<Bytes>, usize) {
    let Frame::Array(items) = frame else {
        return (None, 0);
    };
    let key = match items.get(1) {
        Some(Frame::Bulk(b)) => Some(b.clone()),
        _ => None,
    };
    (key, items.len().saturating_sub(1))
}
```

```rust
// crates/server/src/dispatcher.rs — in the `dispatch_and_log` wrapper, add this line beside the
// existing `let name = command_name_upper(&frame);`
    let (first_key, arg_count) = command_key_and_arity(&frame);
```

```rust
// crates/server/src/dispatcher.rs — in the same wrapper, as the last statement before `reply`
    replication
        .slowlog
        .maybe_record(&name, first_key, arg_count, elapsed);
```

- [x] **Step 5: Read the threshold in `main.rs`**

```rust
// crates/server/src/main.rs — beside the other env vars, before the handle is built
    // Microseconds, not milliseconds: 10ms is already a very long time for an in-memory store,
    // so the useful tuning range is below it. 0 disables the slow log.
    let slowlog_threshold = std::env::var("ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(std::time::Duration::from_micros)
        .unwrap_or_else(|| std::time::Duration::from_millis(10));
```

```rust
// crates/server/src/main.rs — chain it onto the handle builder, after `.with_aof(...)`
    .with_slowlog_threshold(slowlog_threshold);
```

- [x] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, every test in the module

- [x] **Step 7: Commit**

```bash
git add crates/server/src/replication.rs crates/server/src/dispatcher.rs crates/server/src/main.rs
git commit -m "feat(server): record slow commands from the dispatch wrapper"
```

---

### Task 3: `SLOWLOG GET`/`LEN`/`RESET`

**Files:**
- Modify: `crates/server/src/dispatcher.rs` (new helpers below `handle_hello`; interception wired into `dispatch_and_log_inner`)

**Interfaces:**
- Consumes: `ReplicationHandle::slowlog` (Task 2).
- Produces: `fn handle_slowlog(frame: &Frame, replication: &ReplicationHandle) -> Option<Frame>`; nothing later depends on it.

- [x] **Step 1: Write the failing tests**

```rust
// crates/server/src/dispatcher.rs — add to the existing tests module
    #[test]
    fn slowlog_len_counts_recorded_entries() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        // the SLOWLOG LEN command is itself recorded only *after* its reply is built, so it
        // reports the one SET that preceded it
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SLOWLOG", b"LEN"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
    }

    #[test]
    fn slowlog_get_returns_id_timestamp_duration_and_arguments() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"LRANGE", b"mylist", b"0", b"-1"]),
            &mut Protocol::default(),
            1,
        );

        let Frame::Array(entries) = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SLOWLOG", b"GET"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(entries.len(), 1);
        let Frame::Array(entry) = &entries[0] else {
            panic!("expected each entry to be an Array")
        };
        assert_eq!(entry.len(), 4);
        assert_eq!(entry[0], Frame::Integer(0)); // id
        let Frame::Integer(timestamp) = entry[1] else {
            panic!("expected an integer timestamp")
        };
        assert!(timestamp > 1_700_000_000);
        assert!(matches!(entry[2], Frame::Integer(micros) if micros >= 0));
        assert_eq!(
            entry[3],
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"LRANGE")),
                Frame::Bulk(Bytes::from_static(b"mylist")),
                // real Redis's own truncation marker, for the arguments the entry doesn't carry
                Frame::Bulk(Bytes::from_static(b"... (2 more arguments)")),
            ])
        );
    }

    #[test]
    fn slowlog_get_honours_an_explicit_count() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        for _ in 0..3 {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"PING"]),
                &mut Protocol::default(),
                1,
            );
        }
        let Frame::Array(entries) = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SLOWLOG", b"GET", b"2"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn slowlog_reset_replies_ok_and_empties_the_buffer() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PING"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(replication.slowlog.len(), 1);
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SLOWLOG", b"RESET"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        // RESET emptied the buffer; the wrapper then recorded the RESET itself, so exactly one
        // entry remains -- and it is the RESET, not the PING.
        assert_eq!(replication.slowlog.len(), 1);
        assert_eq!(replication.slowlog.get(1)[0].command, "SLOWLOG");
    }

    #[test]
    fn an_unknown_slowlog_subcommand_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"SLOWLOG", b"HELP"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown SLOWLOG subcommand 'HELP'".into())
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"SLOWLOG"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'slowlog' command".into())
        );
    }
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p rocket-mem dispatcher::tests::slowlog an_unknown_slowlog`
Expected: FAIL — `SLOWLOG` currently falls through to `dispatch`'s unknown-command arm

- [x] **Step 3: Implement the interception**

```rust
// crates/server/src/dispatcher.rs — add below `handle_hello`
/// Renders one slow-log entry's argument array. The entry carries only the command name and its
/// first argument, so anything beyond that is summarised with real Redis's own truncation
/// marker -- a shape real Redis itself emits (it truncates at 32 arguments), so tooling parses
/// it without special-casing.
fn slowlog_args_frame(entry: &crate::slowlog::SlowLogEntry) -> Frame {
    let mut args = vec![Frame::Bulk(Bytes::from(entry.command.clone()))];
    let shown = usize::from(entry.key.is_some());
    if let Some(key) = &entry.key {
        args.push(Frame::Bulk(key.clone()));
    }
    if entry.arg_count > shown {
        args.push(Frame::Bulk(Bytes::from(format!(
            "... ({} more arguments)",
            entry.arg_count - shown
        ))));
    }
    Frame::Array(args)
}

/// Returns `Some(reply)` if `frame` was `SLOWLOG`. Intercepted here, like `CLUSTER` and `INFO`,
/// because the ring buffer lives on `ReplicationHandle`, which plain `dispatch` cannot see.
///
/// Three subcommands only: `GET [count]`, `LEN`, `RESET`. `SLOWLOG HELP` is out of scope for the
/// same reason `CLUSTER SLOTS` is -- nothing in this repo consumes it.
fn handle_slowlog(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"SLOWLOG") {
        return None;
    }
    let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'slowlog' command".into(),
        ));
    };
    let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
    Some(match sub.as_str() {
        "GET" => {
            // Default 10, matching real Redis. A negative count means "everything", also
            // matching real Redis; anything unparseable is an error rather than a silent 10.
            let count = match items.get(2) {
                None => 10usize,
                Some(Frame::Bulk(raw)) => match std::str::from_utf8(raw).ok().and_then(|s| s.parse::<i64>().ok()) {
                    Some(n) if n < 0 => crate::slowlog::SLOWLOG_CAPACITY,
                    Some(n) => n as usize,
                    None => {
                        return Some(Frame::Error(
                            "ERR value is not an integer or out of range".into(),
                        ))
                    }
                },
                Some(_) => {
                    return Some(Frame::Error(
                        "ERR value is not an integer or out of range".into(),
                    ))
                }
            };
            Frame::Array(
                replication
                    .slowlog
                    .get(count)
                    .iter()
                    .map(|entry| {
                        Frame::Array(vec![
                            Frame::Integer(entry.id as i64),
                            Frame::Integer(entry.unix_time_secs),
                            Frame::Integer(entry.duration_micros),
                            slowlog_args_frame(entry),
                        ])
                    })
                    .collect(),
            )
        }
        "LEN" => Frame::Integer(replication.slowlog.len() as i64),
        "RESET" => {
            replication.slowlog.reset();
            Frame::Simple("OK".into())
        }
        _ => Frame::Error(format!("ERR unknown SLOWLOG subcommand '{sub}'")),
    })
}
```

```rust
// crates/server/src/dispatcher.rs — inside dispatch_and_log_inner, directly after the
// `handle_hello` interception added by 05-info-and-hello-overhaul.md
    if let Some(reply) = handle_slowlog(&frame, replication) {
        return reply;
    }
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p rocket-mem dispatcher::tests`
Expected: PASS, every test in the module

- [x] **Step 5: Run the full workspace verification**

Run: `cargo build --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --all -- --check && cargo test --workspace`
Expected: all clean/green

- [x] **Step 6: Commit**

```bash
git add crates/server/src/dispatcher.rs
git commit -m "feat(server): add SLOWLOG GET/LEN/RESET"
```
