use bytes::Bytes;
use std::collections::VecDeque;
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

/// `entries` and `next_id` together, guarded by one `Mutex` -- see `SlowLog`'s doc comment for
/// why they can't be two separately-locked fields.
struct SlowLogState {
    entries: VecDeque<SlowLogEntry>,
    next_id: u64,
}

/// A bounded ring of recently-slow commands. `state` is a plain `std::sync::Mutex`: every access
/// is a push or a drain measured in nanoseconds and never held across an `.await`, matching
/// `ReplicaRegistry`'s choice for the same reason. `entries` and `next_id` share the one lock
/// (rather than `next_id` being a separate `AtomicU64`) so that assigning an entry's id and
/// pushing it into the deque happen as a single critical section -- otherwise two threads
/// racing through `maybe_record` could be assigned ids in one order but push in the other,
/// leaving `get()`'s "newest first" order occasionally disagreeing with id order.
pub struct SlowLog {
    state: Mutex<SlowLogState>,
    threshold: Duration,
}

impl SlowLog {
    pub fn with_threshold(threshold: Duration) -> Self {
        Self {
            state: Mutex::new(SlowLogState {
                entries: VecDeque::with_capacity(SLOWLOG_CAPACITY),
                next_id: 0,
            }),
            threshold,
        }
    }

    /// The configured threshold, as `CONFIG GET slowlog-log-slower-than` reports it.
    pub fn threshold(&self) -> Duration {
        self.threshold
    }

    /// Records `command` if it took at least the configured threshold. A no-op otherwise, which
    /// is the overwhelmingly common case -- this is the only slow-log work on the hot path.
    /// `Duration::ZERO` means disabled, not "record everything"; see this plan's Global
    /// Constraints.
    ///
    /// `key` is the command's first argument and is what gets *stored* and handed back by
    /// `SLOWLOG GET`. `log_key` is the key-spec-aware key (`dispatcher::logged_key`) and is used
    /// only by the `warn!` below -- never stored. The two differ for `MEMORY USAGE <key>` and
    /// `OBJECT ENCODING <key>`, where the first argument is a subcommand, and for every keyless
    /// command, where the first argument is a client-supplied value the log must not carry.
    /// Keeping them separate is deliberate: the stored entry is a client-visible surface this
    /// logging work must not change.
    pub fn maybe_record(
        &self,
        command: &str,
        key: Option<Bytes>,
        arg_count: usize,
        elapsed: Duration,
        log_key: Option<&Bytes>,
    ) {
        if self.threshold.is_zero() || elapsed < self.threshold {
            return;
        }
        let unix_time_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let duration_micros = elapsed.as_micros().min(i64::MAX as u128) as i64;

        // Reached only past the early return above -- i.e. only when a command actually
        // crosses the operator-configured threshold, never on the common under-threshold path
        // every other command takes (the hottest path in the project). `warn`, not `debug`:
        // the threshold is operator-configured, so crossing it is by definition something the
        // operator asked to be told about.
        //
        // `cmd`/`key` here duplicate the enclosing `cmd` span's own fields -- but only when
        // that span happens to be visible. The span is opened with `debug_span!`, so at this
        // project's documented production default of `info` the span is never entered at all
        // (verified: a `warn!` nested in a filtered-out `debug_span!` carries none of the
        // span's fields -- there's no ambient context to omit these in favour of). Since this
        // event fires at `warn`, which *is* visible at that default, it has to carry `cmd`/
        // `key` itself, or a production-default deployment's slowlog warning would name no
        // command and no key at all. `key` goes through `key_field` -- the same lossy-UTF-8
        // rendering the `cmd` span already uses -- never `?` on the raw `Bytes`, which renders
        // byte-by-byte. It renders `log_key`, not the stored `key`: see this fn's doc comment.
        tracing::warn!(
            cmd = %command,
            key = %crate::logging::key_field(log_key),
            elapsed_us = duration_micros,
            "slow command recorded"
        );

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = SlowLogEntry {
            id: state.next_id,
            unix_time_secs,
            duration_micros,
            command: command.to_string(),
            key,
            arg_count,
        };
        state.next_id += 1;
        if state.entries.len() == SLOWLOG_CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(entry);
        drop(state);
        ::metrics::counter!("rocket_mem_slowlog_entries_total").increment(1);
    }

    /// Up to `count` entries, newest first -- the order real Redis returns them in.
    pub fn get(&self, count: usize) -> Vec<SlowLogEntry> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .rev()
            .take(count)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    /// Required by `clippy::len_without_is_empty`, which `-D warnings` makes a hard error.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reset(&self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
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
        log.maybe_record("GET", key(b"k"), 1, Duration::from_micros(50), None);
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn a_command_at_or_over_the_threshold_is_recorded_with_its_details() {
        let log = SlowLog::with_threshold(Duration::from_millis(10));
        log.maybe_record("LRANGE", key(b"mylist"), 3, Duration::from_millis(25), None);
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
        log.maybe_record("GET", key(b"k"), 1, Duration::from_secs(5), None);
        assert!(log.is_empty());
    }

    #[test]
    fn get_returns_the_newest_entries_first_and_respects_count() {
        let log = SlowLog::with_threshold(Duration::from_micros(1));
        for i in 0..5u32 {
            log.maybe_record("SET", None, i as usize, Duration::from_millis(1), None);
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
            log.maybe_record("SET", None, 2, Duration::from_millis(1), None);
        }
        assert_eq!(log.len(), SLOWLOG_CAPACITY);
        let entries = log.get(SLOWLOG_CAPACITY);
        assert_eq!(entries[0].id as usize, SLOWLOG_CAPACITY + 9);
        assert_eq!(entries[SLOWLOG_CAPACITY - 1].id, 10);
    }

    #[test]
    fn reset_clears_the_entries_but_ids_keep_counting_up() {
        let log = SlowLog::with_threshold(Duration::from_micros(1));
        log.maybe_record("SET", None, 2, Duration::from_millis(1), None);
        log.reset();
        assert!(log.is_empty());
        log.maybe_record("SET", None, 2, Duration::from_millis(1), None);
        // ids are monotonic across a reset, matching real Redis -- an operator correlating a
        // logged id with a later GET must not find it reused.
        assert_eq!(log.get(1)[0].id, 1);
    }

    #[test]
    fn concurrent_maybe_record_calls_keep_id_and_insertion_order_in_agreement() {
        // Id assignment and the deque push happen under the same lock (see SlowLog's doc
        // comment), so no interleaving of concurrent callers can produce an entry whose id
        // doesn't match its physical position in the ring. Without that, two threads racing
        // through maybe_record could grab ids in one order but push in the other, and get()
        // (newest first) would occasionally disagree with id order.
        let log = std::sync::Arc::new(SlowLog::with_threshold(Duration::from_micros(1)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let log = std::sync::Arc::clone(&log);
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        log.maybe_record("SET", None, 1, Duration::from_millis(1), None);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        // 80 entries recorded, comfortably under SLOWLOG_CAPACITY (128), so none were evicted.
        let all = log.get(80);
        assert_eq!(all.len(), 80);
        for pair in all.windows(2) {
            assert!(
                pair[0].id > pair[1].id,
                "entries must be strictly descending by id, newest first: {} then {}",
                pair[0].id,
                pair[1].id
            );
        }
    }

    #[test]
    fn the_default_threshold_is_ten_milliseconds() {
        let log = SlowLog::default();
        log.maybe_record("GET", None, 1, Duration::from_millis(9), None);
        assert!(log.is_empty());
        log.maybe_record("GET", None, 1, Duration::from_millis(10), None);
        assert_eq!(log.len(), 1);
    }

    // A capture assertion on `maybe_record`'s `warn!` output does not live here. `tracing`
    // caches callsite `Interest` per callsite, process-globally, the first time that callsite
    // is reached (see `test(logging): stop asserting on captured logs from unit-test binaries`
    // for the two tests this bit for real). Every other test above calls `maybe_record` with an
    // elapsed time at or over its threshold and *no* subscriber installed, which would reach
    // this file's `warn!` callsite first and could permanently decide it's uninteresting before
    // a later test's own capture subscriber gets a turn -- in the ~575-test unit binary this
    // file compiles into, whose subscribers install and drop constantly, that risk is real
    // rather than theoretical. `maybe_record_only_warns_when_the_command_actually_gets_recorded`
    // lives in `crates/server/tests/logging.rs` instead, a separate, much smaller integration
    // binary where this callsite is touched by nothing else. Do not re-add a capture assertion
    // here.
}
