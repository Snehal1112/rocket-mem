# RESP3 Protocol Support: Spec & Design

**Goal:** rocket-mem negotiates RESP3 with clients that request it via `HELLO`, replies
correctly in whichever protocol was negotiated, and keeps every existing RESP2 client
(`redis-cli`, `redis-py` without `protocol=3`, `redis-rs`, the existing integration
tests) working unchanged.

**Scope:** this reverses `2026-08-29-sprint-2-spec.md`'s "RESP3/`HELLO` decision —
reject, force RESP2." That decision was correct for Sprint 2 (no client in scope
hard-failed on `HELLO` erroring) — it's revisited now because `docs/phase-1-retro.md`
already flagged that `redis-py` 8.1.0's default connection health-check *does* hard-fail
on `HELLO` unless the caller passes `protocol=2` explicitly, and because RESP3 support
was explicitly requested as a new, separate piece of work.

**Non-goals:** this is negotiation plus correct encoding, not full RESP3 adoption.
`Double`, `Boolean`, `BigNumber`, `Set`, `Verbatim`, and `Push` frame types are **not**
added — nothing in rocket-mem's command surface produces them, and adding unused
variants would be dead code the moment it lands. `HELLO`'s `AUTH`/`SETNAME` clauses are
out of scope — rocket-mem has no authentication and no `CLIENT SETNAME` yet. Revisit
both only when a command actually needs them.

---

## Why `Map` is the one new `Frame` variant needed

Every existing command reply (`Simple`, `Error`, `Integer`, `Bulk`, `Null`, `Array`) is
byte-identical between RESP2 and RESP3 — RESP3 only changes the wire format for types
RESP2 never had. The one place this bites is `HELLO` itself: real Redis's `HELLO` reply
is RESP3's native `Map` type when a client negotiates protocol 3, and RESP3-aware client
libraries (redis-py's protocol-3 response parser, for one) expect that specific shape
back, not a flat array. Replying to `HELLO 3` with the same array encoding RESP2 uses
would make the connection "work" at the byte level while still confusing any client that
actually branches on the negotiated protocol to decide how to parse the reply — so `Map`
is required for real interoperability, not just theoretical completeness.

```rust
// crates/protocol/src/frame.rs — add one variant
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
    Map(Vec<(Frame, Frame)>), // new
}
```

## `decode()` needs zero changes

Clients only ever send commands as an `Array` of `Bulk` strings — this is true under
RESP2 *and* RESP3; the richer RESP3 types (`Map`, `Double`, `Boolean`, `Null`'s `_\r\n`
form, etc.) are reply-only, never appear on the request side. `RespCodec::decode()`
already only recognizes `+ - : $ *`, and that stays exactly as-is. Only `encode()`
changes, and only for `Null` and the new `Map` variant.

## `RespCodec` becomes stateful

```rust
// crates/protocol/src/codec.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Resp2,
    Resp3,
}

#[derive(Default)]
pub struct RespCodec {
    pub protocol: Protocol,
}
```

`RespCodec` was previously a zero-sized unit struct constructed as a bare value
(`RespCodec`, `Framed::new(socket, RespCodec)`). Every existing call site — `codec.rs`'s
own ~14 tests, `connection.rs`'s 3 tests — switches to `RespCodec::default()`, which
is behaviorally identical (defaults to RESP2, matching current behavior exactly).

### Wire format changes in `encode()`

| Frame variant | RESP2 encoding (unchanged) | RESP3 encoding (new) |
|---|---|---|
| `Null` | `$-1\r\n` | `_\r\n` |
| `Map(pairs)` | `*{2N}\r\n` — flattened key,value,key,value,... (Redis's own RESP2 map-emulation convention) | `%{N}\r\n` followed by each pair's key then value, each encoded normally (recursing through `self.encode`) |

Every other variant's encoding is unchanged and protocol-independent.

## Protocol negotiation flow

`dispatch()`'s signature grows one parameter:

```rust
// crates/server/src/dispatcher.rs
pub fn dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame
```

**Refinement found during self-review:** `HELLO`'s reply needs the connection's client
ID (see below), which isn't available from `&Engine` and can't be derived from
`&mut Protocol`. Rather than invent a bundling struct for two pieces of state, `dispatch`
takes `client_id` as a second, plain-by-value parameter — immutable, since a
connection's ID never changes after being assigned. This is a small addition beyond the
literal "`dispatch()` takes `&mut Protocol`" framing approved earlier, in the same
spirit: thread through exactly the state each command needs, nothing more.

Every existing arm ignores `protocol` and `client_id` — only `HELLO`'s arm reads or
writes `protocol`, and only `HELLO` reads `client_id`. This keeps a single home for all
command routing: `HELLO` sits in the same match as `PING`/`SELECT`/`COMMAND`/`INFO`, the
other "protocol-adjacent, engine-untouching" commands already living there. The cost is
mechanical: every existing dispatcher test call site (`dispatch(&engine,
cmd(&[...]))`) gains two more arguments (`&mut Protocol::default(), 1` for tests that
don't care about either).

`connection.rs` owns one `Protocol` value and one `client_id` per connection (both local
to `handle_connection`, not shared via `Arc` — each connection negotiates
independently; `client_id` is assigned once by the accept loop and never changes):

```rust
// crates/server/src/connection.rs — accept loop + handle_connection, sketch
pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let client_id = next_client_id;
        next_client_id += 1;
        let engine = Arc::clone(&engine);
        tokio::spawn(handle_connection(socket, engine, client_id));
    }
}

async fn handle_connection(socket: TcpStream, engine: Arc<Engine>, client_id: u64) {
    let mut framed = Framed::new(socket, RespCodec::default());
    let mut protocol = Protocol::default();
    while let Some(result) = framed.next().await {
        let frame = match result { Ok(f) => f, Err(_) => return };
        let response = dispatcher::dispatch(&engine, frame, &mut protocol, client_id);
        framed.codec_mut().protocol = protocol; // sync BEFORE sending this reply
        if framed.send(response).await.is_err() { return; }
    }
}
```

The sync happens *before* `send()` so `HELLO`'s own reply is encoded in the
newly-negotiated protocol — matching real Redis, where the `HELLO 3` response is itself
RESP3-encoded.

## `HELLO` semantics

Matches real Redis:

- **`HELLO` (no args):** reports the *current* protocol without switching. Reply is a
  `Map` if currently RESP3, an `Array` if currently RESP2 (map-emulated).
- **`HELLO 2` / `HELLO 3`:** switches `*protocol` to the requested version, replies in
  the *new* protocol's encoding.
- **`HELLO <anything else>`** (e.g. `HELLO 4`, `HELLO foo`): returns
  `Frame::Error("NOPROTO unsupported protocol version")` and leaves `*protocol`
  unchanged — the literal "NOPROTO" prefix matters, since some client libraries check
  for it specifically to detect an incompatible server.
- **`HELLO <protover> AUTH ...`** or **`... SETNAME ...`**: any extra arguments beyond
  a bare protover are a syntax error (`Frame::Error("ERR syntax error")`) — not silently
  accepted and ignored. Rocket-mem has no auth and no `CLIENT SETNAME`; silently
  swallowing these flags would be worse than rejecting them, since a client that thinks
  `AUTH` succeeded when it didn't is a worse failure mode than a clear syntax error.

**Reply fields** (as `Map` pairs / flattened `Array` pairs, matching real Redis's own
field set for a vanilla non-cluster, non-replicated, module-free server):

| Field | Value |
|---|---|
| `server` | `Bulk("redis")` — client libraries key off this exact string, not a rocket-mem-branded one |
| `version` | `Bulk("rocket-mem-0.1.0")` — reuses `INFO`'s existing version-string convention |
| `proto` | `Integer(2)` or `Integer(3)` — the now-current protocol |
| `id` | `Integer(<n>)` — a real per-connection counter (see below), not a placeholder |
| `mode` | `Bulk("standalone")` |
| `role` | `Bulk("master")` |
| `modules` | `Array(vec![])` |

**Client ID counter:** a plain `u64` (starting at 1), incremented once per accepted
connection inside `serve()`'s accept loop, then moved into the spawned
`handle_connection` task alongside the socket and shared `Engine`. No atomic needed —
`serve()`'s `loop { listener.accept().await ... }` is already single-threaded and
sequential; only that loop ever increments the counter, so a plain local variable is
correct and simpler than reaching for `AtomicU64`. This is genuinely trivial to do
correctly, so there's no reason to hardcode a placeholder — nothing else depends on ID
uniqueness yet (no `CLIENT LIST`/`CLIENT ID` command exists), but a fake shared constant
would be a code smell for zero savings in effort.

## Error handling

- Malformed `HELLO` syntax (extra/unrecognized args) → `Frame::Error("ERR syntax
  error")`, protocol unchanged — consistent with the existing `require_args!`-style
  convention of returning a clean RESP error rather than panicking.
- Unsupported protover → `Frame::Error("NOPROTO unsupported protocol version")`,
  protocol unchanged.
- Everything else about `HELLO` argument extraction reuses the existing
  `frame_to_args`/`rest` conventions already established in `dispatcher.rs` — no new
  parsing infrastructure needed.

## Testing

- **`codec.rs`:** new encode tests for `Frame::Null` and `Frame::Map` under both
  `Protocol::Resp2` and `Protocol::Resp3` — 4 new cases covering the table above. All
  ~14 existing encode/decode tests migrate from `RespCodec` to `RespCodec::default()`
  with no behavior change (still defaults to RESP2).
- **`dispatcher.rs`:** new tests for the four `HELLO` cases (no-arg report, switch to
  2, switch to 3, invalid protover → NOPROTO), plus confirming a non-`HELLO` command
  leaves `*protocol` untouched. All ~40 existing dispatcher tests gain two more
  arguments (`&mut Protocol::default(), 1`) — mechanical, no assertion changes.
- **`connection.rs`:** one new test proving a `HELLO 3` frame followed by a normal
  command (e.g. `SET`/`GET`) round-trips correctly over a real socket, with the `GET`
  reply's `Null`-on-missing-key case (if exercised) coming back as `_\r\n` rather than
  `$-1\r\n` — i.e., proving the codec's protocol state actually persists connection-wide
  after negotiation, not just for `HELLO`'s own reply.
- **Manual verification:** reuse the `redis-py`/`ioredis` setup already installed for
  `07-manual-client-verification.md` (see `client-verification-results.md`), this time
  passing `protocol=3` (redis-py) / RESP3 options (ioredis) explicitly, confirming both
  now connect *without* the `protocol=2` workaround `phase-1-retro.md` flagged, and that
  the existing SET/GET/HSET/RPUSH/SADD smoke sequence still passes under RESP3.
