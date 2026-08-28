# Sprint 1 — Engine Core & Data Types: Spec & Design

**Goal:** a tested, sharded in-memory engine implementing Strings, Hashes, Lists, and Sets — callable directly with no networking yet — ready for Sprint 2's RESP layer to sit on top of it.

**Scope:** covers Sprint 1's 8 backlog items (see `rocket-mem-sprint-plan.md`, Sprint 1). This doc fixes the shared design decisions — workspace layout, module boundaries, the `Value` type, the sharding scheme — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Architecture recap:** per the master plan's Architecture Decision Record — layered, sharded, lock-based, task-per-connection. This sprint builds only the bottom layer (storage engine); the protocol/dispatch layers are Sprint 2.

---

## Workspace layout

```
rocket-mem/
├── Cargo.toml                        # workspace manifest
├── crates/
│   ├── common/                       # shared error types, zero deps on other crates
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── engine/                       # storage engine — this sprint's entire focus
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── value.rs              # Value enum
│   │       ├── shard.rs              # single shard: RwLock<HashMap<Bytes, Value>>
│   │       ├── store.rs              # N-shard Store, key→shard routing
│   │       ├── engine.rs             # public Engine facade
│   │       └── commands/
│   │           ├── mod.rs
│   │           ├── string.rs
│   │           ├── hash.rs
│   │           ├── list.rs
│   │           └── set.rs
│   ├── protocol/                     # empty placeholder — built in Sprint 2
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── server/                       # empty placeholder — built in Sprint 2
│       ├── Cargo.toml
│       └── src/main.rs
├── .github/workflows/ci.yml
└── docs/design/sharding-decision.md
```

## The `Value` type
```rust
pub enum Value {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<Bytes, Bytes>),
    Set(HashSet<Bytes>),
}
```
Lives in `crates/engine/src/value.rs`. Every command implementation matches on this — there is exactly one place a new data type would be added.

## Sharding scheme
16 shards, fixed for this sprint (not yet configurable). A key routes to a shard via `std::hash::DefaultHasher` over the key bytes, modulo shard count. This is the concrete implementation of the sharded-lock architecture chosen in the master plan's decision record — see `docs/design/sharding-decision.md` (backlog item 8) for the write-up.

## Error type
```rust
pub enum EngineError {
    WrongType,      // WRONGTYPE — key exists but holds a different data type
    NotAnInteger,   // INCR/DECR on a non-numeric string
}
```
Lives in `crates/common/src/lib.rs` since both `engine` and (eventually) `protocol` need it.

## Scope note: TTL flags
`SET`'s `EX`/`PX` flags are **not** implemented this sprint. Parsing and enforcing them without an active/passive expiry reaper (Sprint 4 / Week 7 in the master plan) would be dead code carried for four sprints. Sprint 1 implements `NX`/`XX` only, since those don't depend on time. This is a deliberate scope cut, not an oversight — flag it if a reviewer questions it.

## Sequencing
Plans depend on each other in this order:
1. `01-workspace-scaffold-and-value-enum.md`
2. `02-sharded-keyspace.md` (depends on 1)
3. `03-engine-facade.md` (depends on 2)
4. `04-string-commands.md` (depends on 3)
5. `05-hash-list-set-commands.md` (depends on 3 — independent of 4, can run in parallel with it)
6. `06-wrongtype-error-handling-test-matrix.md` (depends on 4 and 5)
7. `07-ci-skeleton.md` (independent — can run any time)
8. `08-sharding-design-doc.md` (independent — best done after 2, since it documents that decision)

## Definition of done for the sprint
Matches Sprint 1 in `rocket-mem-sprint-plan.md`:
- [ ] `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` clean
- [ ] Every P0 command has a passing test, including at least one wrong-type/missing-key case
- [ ] Sharding design doc committed
- [ ] CI runs on push
