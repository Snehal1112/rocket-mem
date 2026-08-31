# Sprint 7 — Custom Protocol: Spec & Design

**Goal:** rocket-mem's own protocol is live alongside RESP, both reading and writing the same shared keyspace — matching `../../rocket-mem-sprint-plan.md`'s Sprint 7 goal.

**Scope:** covers Sprint 7's 5 backlog items (see `../../rocket-mem-sprint-plan.md`, Sprint 7, and `../../rocket-mem-production-plan.md`, Weeks 13–14). This doc fixes the shared design decisions — the protocol's name and headline capability, its wire format (envelope + value encoding), how it plugs into the existing command pipeline, its connection/concurrency model, listener wiring, and client library scope — that every implementation plan below assumes as ground truth. Individual plans don't re-derive these; they reference this doc.

**Name:** **RMP** (Rocket-mem Protocol). Referred to as "RMP" throughout this doc and the plans below.

**Architecture recap:** RMP is a second wire format for the same command pipeline RESP already drives — it adds no new engine capability and changes no engine data path. Its headline (and only) capability is **request multiplexing**: many in-flight requests on one TCP connection, correlated by a request ID rather than by strict reply order. RESP cannot express this — RESP2/3's grammar has no request-identifying field, so a RESP connection's replies are strictly one-in-one-out in send order (this is exactly why `connection.rs`'s pipelining optimization batches writes but never reorders them). RMP's binary envelope adds the one field RESP structurally lacks.

The key design finding from this doc's own investigation: `dispatcher::dispatch_and_log` already operates on `protocol::Frame::Array(Vec<Frame::Bulk>)` as its canonical "one command" shape, and returns `protocol::Frame` as its canonical reply — and that function already contains the *entire* pipeline (cluster redirect, read-only-replica gate, `SAVE`/`REPLICAOF`/`CLUSTER`/`INFO`/`HELLO`/`SLOWLOG` interception, AOF logging with TTL-rewrite, replica fan-out), not just command matching. RMP therefore does not introduce a second command layer at all: its connection handler decodes each request directly into a `Frame::Array` and calls `dispatch_and_log` completely unchanged, then encodes the returned `Frame` back out. This gets full command parity — including `INFO`, `CLUSTER`, `SAVE`, `REPLICAOF`, `SLOWLOG` — for zero changes to that pipeline. Only `PSYNC` (raw-socket takeover) is genuinely unreachable over RMP: it's intercepted a level above `dispatch_and_log`, in `connection.rs`, before `dispatch_and_log` is even called, and RMP's connection handler (`rmp_connection.rs`) has no equivalent interception, so a `PSYNC` sent over RMP falls through to `dispatch`'s ordinary "unknown command" arm. `HELLO`, by contrast, IS reachable over RMP: it's intercepted *inside* `dispatch_and_log_inner` itself (via `handle_hello`), a step RMP's connection handler goes through identically to RESP, so `HELLO` sent over RMP gets a normal success reply (a `Map`). The catch is that RMP has no protocol-negotiation state to persist between calls, so each RMP call answers `HELLO` from a fresh, throwaway `Protocol::default()` (see the Global Constraints bullet below) — the reply always looks like a freshly negotiated connection, and any "switch" `HELLO` implies has no observable effect on later replies. This is harmless rather than a bug: RMP already always encodes `Map`/`Null` natively regardless of what `HELLO` claims.

New code this sprint is additive only: a new `protocol::rmp` module (envelope + codec, reusing `Frame` as the value model), a new `crates/server/src/rmp_connection.rs` (multiplexed connection handling, wired as a second listener next to the existing RESP and metrics listeners), and a new `crates/rmp-client` crate (minimal Rust client).

## Global Constraints

- **No new runtime dependency.** RMP is hand-rolled over `tokio_util::codec`, exactly like `RespCodec` — timeboxed per the sprint plan's own risk note rather than adopting Protobuf/Cap'n Proto/FlatBuffers.
- **`dispatch_and_log` is not modified.** Every RMP request reaches the engine, AOF, and replication exactly the way a RESP request does, by constructing the same `Frame::Array` shape and calling the same function. This is deliberate risk reduction: Sprints 4–6's durability, replication, and cluster-routing test suites all exercise `dispatch_and_log` today and continue to be the source of truth for that behavior; RMP inherits their correctness rather than re-proving it.
- **RMP has exactly one wire version.** Unlike RESP2/RESP3, there is no negotiation handshake and no legacy-compat tier — `Null` and `Map` always encode natively. `protocol::codec::Protocol` (RESP's RESP2-vs-RESP3 switch) is irrelevant to RMP; `dispatch_and_log`'s `&mut Protocol` parameter is satisfied with a fresh, throwaway `Protocol::default()` per RMP call, so a `HELLO` sent over RMP still gets answered (a normal success `Map`, via the same `handle_hello` interception RESP goes through) but its mutation of that throwaway `Protocol` is simply discarded — harmless, since RMP was going to encode `Map`/`Null` natively either way and has no negotiated state for a later call to observe.
- **RMP gets its own TCP port**, not first-byte protocol detection on the RESP port. Simpler, no sniffing logic, no risk of a byte-pattern collision between the two formats.
- **Full command parity by construction**, not by re-implementing ~84 match arms. See the Architecture recap above.

---

## Decision: wire format — a request-ID envelope around the existing `Frame` value model

**Envelope** (fixed 16-byte header, all multi-byte integers big-endian):

| Field | Size | Meaning |
|---|---|---|
| `magic` | 2 bytes | `0x52 0x4D` (ASCII `"RM"`) — lets a misdirected connection (e.g. a RESP client pointed at the wrong port) fail fast with a clear decode error instead of a confusing hang |
| `version` | 1 byte | `0x01` |
| `msg_type` | 1 byte | `0x00` = Request, `0x01` = Response |
| `request_id` | 8 bytes | Client-chosen for a Request; echoed verbatim on the matching Response. The multiplexing correlation key. |
| `payload_len` | 4 bytes | Length of the payload that follows, in bytes |

Followed by `payload_len` bytes of **payload**: a single recursively-encoded `Frame` value (see below). A **Request**'s payload is always a `Frame::Array` of `Frame::Bulk`s — command name plus arguments, byte-for-byte the same shape `dispatch_and_log` already consumes. A **Response**'s payload is whatever `Frame` `dispatch_and_log` returned.

**Value encoding** (recursive; a `Frame` encodes as a 1-byte tag followed by its content):

| Tag | Frame variant | Content |
|---|---|---|
| `0x00` | `Null` | (none) |
| `0x01` | `Simple(String)` | `u32` len + UTF-8 bytes |
| `0x02` | `Error(String)` | `u32` len + UTF-8 bytes |
| `0x03` | `Integer(i64)` | 8 bytes, big-endian |
| `0x04` | `Bulk(Bytes)` | `u32` len + raw bytes |
| `0x05` | `Array(Vec<Frame>)` | `u32` count, then that many encoded `Frame`s |
| `0x06` | `Map(Vec<(Frame, Frame)>)` | `u32` pair-count, then that many `(key, value)` encoded-`Frame` pairs |

**Size guard:** both `payload_len` in the envelope and every `u32` length/count inside the value encoding are checked against a `MAX_RMP_FRAME_LEN` constant (`64 MiB`) before any allocation. A length exceeding it is a decode error that closes the connection — a length-prefixed format has no other defense against a forged multi-gigabyte prefix, and `RespCodec` gets the equivalent protection for free from parsing incrementally against buffered bytes rather than trusting an attacker-supplied count.

**Worked example — a `GET foo` request and its `bar` reply**, request ID `1`:

Request bytes (name `GET`, one arg `foo`, wrapped as `Array[Bulk("GET"), Bulk("foo")]`):
```
52 4D 01 00                          magic "RM", version 1, msg_type=Request
00 00 00 00 00 00 00 01              request_id = 1
00 00 00 15                          payload_len = 21
-- payload (21 bytes) --
05                                   tag Array
00 00 00 02                          count = 2
04 00 00 00 03 47 45 54              tag Bulk, len 3, "GET"
04 00 00 00 03 66 6F 6F              tag Bulk, len 3, "foo"
```

Response bytes (`Bulk("bar")`), same request ID:
```
52 4D 01 01                          magic "RM", version 1, msg_type=Response
00 00 00 00 00 00 00 01              request_id = 1
00 00 00 08                          payload_len = 8
04 00 00 00 03 62 61 72              tag Bulk, len 3, "bar"
```

**Worked example — a multiplexed `GET a` / `SET b 1` pair on one connection**, proving the point of the envelope: the client sends both requests (IDs `7` and `8`) back-to-back without waiting for either reply, and the server may answer in either order — the client identifies each reply by `request_id`, not by arrival position:
```
Client → Server:  [id=7  Request: GET a]  [id=8  Request: SET b 1]
Server → Client:  [id=8  Response: Simple("OK")]   <- SET finished first
                  [id=7  Response: Bulk("1")]       <- GET's reply arrives second
```
A RESP connection cannot produce this trace: two requests sent back-to-back over RESP must be answered in the order sent, full stop. This is the concrete difference the spec review should check the byte layout above against.

---

## Decision: connection & concurrency model — spawn-per-request, single writer, request-ID fan-in

Each accepted RMP connection runs two halves, mirroring the split `connection.rs` already does for the raw socket during `PSYNC`, but for the whole connection lifetime:

1. **Read loop:** decodes RMP Request frames off the socket in a loop. For each one, it clones the shared `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>` and the connection's `client_id`, then `tokio::spawn`s a task that builds the `Frame::Array` command shape, calls `dispatch_and_log`, and sends `(request_id, reply_frame)` into an `mpsc::unbounded_channel` shared by the whole connection. The read loop then immediately goes back to decoding the next frame — it never waits for a spawned task to finish, which is what makes multiple in-flight requests possible at all. **This means execution order is undefined, not just reply order:** because each decoded request is handed to its own spawned task rather than awaited before the next one is decoded, two commands sent back-to-back on one RMP connection may be *executed* by the engine in either order — a real semantic difference from RESP, where pipelined commands always execute in send order. A client that needs command B to observe command A's effect must await A's reply before sending B, exactly as `rmp_correctly_multiplexes_concurrent_requests_on_one_connection` relies on in practice.
2. **Writer loop:** owns the channel's receiver and the socket's write half. It `recv()`s `(request_id, reply_frame)` pairs and encodes+writes a Response envelope for each, in whatever order they arrive — no reordering, no buffering beyond what `Framed`'s `feed`/`flush` already does per write.

**Shutdown:** the channel's `Sender` is cloned into every spawned task; the read loop holds one more clone. When the read loop ends (EOF or decode error), it drops its sender. The writer loop's `recv()` only returns `None` once *every* clone has dropped — i.e. once every in-flight spawned task has also finished and dropped its own clone — so a client disconnecting mid-flight still gets every reply that was already in progress written out (or a failed write ends the writer loop early, same as RESP's `framed.feed(..).is_err()` check).

This reuses no new synchronization primitive beyond what `dispatch_and_log` already requires: it's already safe to call concurrently (every existing RESP connection is an independent task calling it concurrently today), so one RMP connection spawning several concurrent calls to it is the same situation multiplied, not a new one.

---

## Decision: listener wiring

A new env var, read once in `main.rs` next to `ROCKET_MEM_ADDR`/`ROCKET_MEM_METRICS_ADDR`:

| Variable | Default | Meaning |
|---|---|---|
| `ROCKET_MEM_RMP_ADDR` | `127.0.0.1:6380` | Bind address for the RMP listener |

`main.rs` binds this `TcpListener` and calls a new `rocket_mem::rmp_connection::serve(listener, engine, aof, replication)` alongside the existing `rocket_mem::serve(...)` call — both listeners run concurrently (`tokio::spawn` one, `.await` the other, or `tokio::join!` both), sharing the same `Arc<Engine>`/`Arc<AofWriter>`/`Arc<ReplicationHandle>` instances the RESP listener already uses. RMP is always on — there is no opt-out env var, matching how the metrics listener is unconditional today; if that turns out to be undesirable it's a one-line follow-up, not a design question worth blocking this sprint on.

---

## Decision: client library scope

**`crates/rmp-client`** (new workspace member), a minimal async Rust client:

- `RmpClient::connect(addr) -> RmpClient` — opens the TCP connection, spawns its own background reader task that demultiplexes incoming Responses by `request_id` into a `HashMap<u64, oneshot::Sender<Frame>>` guarded by a `Mutex` (the mirror image of the server's writer-loop fan-in).
- `async fn call(&self, args: Vec<Bytes>) -> Result<Frame, RmpError>` — the general escape hatch: assigns the next `request_id` (an `AtomicU64` counter), registers a `oneshot` receiver for it, sends the Request, awaits the reply. Because full command parity is already established by the server-side design above, `call` alone can drive every command RESP can — `get`/`set`/`del` below are thin convenience wrappers over it, not a smaller command surface.
- `async fn get(&self, key: impl Into<Bytes>) -> Result<Option<Bytes>, RmpError>`, `async fn set(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Result<(), RmpError>`, `async fn del(&self, key: impl Into<Bytes>) -> Result<bool, RmpError>` — cover the sprint plan's "GET/SET-equivalent" DoD line explicitly, built on `call`.

**Second-language client stub (P2, stretch):** this spec document's wire-format section above is written to be sufficient on its own for a Go or TypeScript implementer (concrete byte layouts, no cross-references needed to understand the envelope or value encoding). No code stub is planned unless the final implementation plan finds a gap in that description — if so, the fix is tightening this doc, not writing throwaway Go/TS scaffolding.

---

## Testing strategy

- **Codec unit tests** (`crates/protocol/src/rmp.rs`, mirroring `codec.rs`'s existing test style): round-trip encode/decode for every `Frame` variant including nested `Array`/`Map`; the two worked byte-layout examples above reproduced as exact-byte assertions; split-read tests (header split mid-read, payload split across two reads) mirroring `RespCodec`'s `decode_reassembles_a_bulk_string_split_across_two_reads`-style coverage; an oversized `payload_len` is rejected without allocating.
- **Server integration tests** (`crates/server/tests/rmp.rs`, using the new `rmp-client` crate the way existing tests use `redis-rs`):
  - `resp_write_is_visible_to_a_read_over_rmp` and its mirror `rmp_write_is_visible_to_a_read_over_resp` — the production plan's own named example test, both directions.
  - `rmp_correctly_multiplexes_concurrent_requests_on_one_connection` — sends two requests without awaiting the first (`tokio::join!`), asserts both correlate to the correct reply by content, not by arrival order.
  - `rmp_reaches_info_and_cluster_commands` — a small proof that the "reuse `dispatch_and_log` unchanged" decision above actually delivers parity, not just the string-command families.
  - A disconnect-mid-flight test: drop the client connection while a request is still in flight server-side, assert the server task ends cleanly (mirrors `serve_closes_the_connection_cleanly_when_the_client_disconnects`).

## Definition of done

(Concretizes `../../rocket-mem-sprint-plan.md`'s Sprint 7 DoD.)

- [ ] This spec doc committed
- [ ] `protocol::rmp` codec implemented with the full unit test suite above, `cargo test -p protocol` green
- [ ] `rmp_connection::serve` implemented and wired into `main.rs` as a second listener, `cargo test -p server` green
- [ ] Integration tests prove a RESP write visible via RMP and vice versa, on the same shared engine
- [ ] Multiplexing integration test proves correct request/response correlation independent of reply order
- [ ] `crates/rmp-client` implements `call`/`get`/`set`/`del`, used by the integration tests above
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace` all green

## Plan breakdown

Maps to `../plans/2026-08-31-sprint-7-plans/`:

1. **Wire format & codec** — `protocol::rmp` envelope + `Frame` value codec, full round-trip/split-read/size-guard unit tests. No dependency on anything else in this list.
2. **Server-side RMP connection handling & listener wiring** — `rmp_connection::serve`, spawn-per-request/single-writer model, `main.rs` wiring. Depends on (1).
3. **Rust client library** (`crates/rmp-client`) — depends on (1); independent of (2) except for integration testing.
4. **Integration tests** — shared-keyspace and multiplexing proof, using (2) and (3) together.
