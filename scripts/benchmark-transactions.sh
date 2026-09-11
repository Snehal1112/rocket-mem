#!/usr/bin/env bash
# MULTI/EXEC throughput: N complete 2-command transactions, sent as one raw RESP stream over a
# plain TCP socket. redis-benchmark's -t has no multi-command-transaction mode, and redis-cli
# --pipe's completion-detection handshake does not work against rocket-mem at all (confirmed:
# it fails identically for plain, non-transactional commands piped the same way, so it is an
# unrelated, pre-existing gap -- not something this script works around by masking a real
# transactions bug). A small stdlib-only Python client sends the stream and counts RESP replies
# directly instead. Reports transactions/sec for both real Redis and rocket-mem, same
# matched-durability setup as scripts/benchmark.sh.
set -euo pipefail

for bin in redis-server redis-cli python3; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "error: '$bin' is not on PATH. Install a Redis distribution first" >&2
    exit 1
  fi
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REDIS_PORT=7787
ROCKET_PORT=7788
N=20000

echo "Building rocket-mem in release mode..." >&2
cargo build --release --workspace --manifest-path "$ROOT/Cargo.toml" >&2

WORK="$(mktemp -d)"
REDIS_PID=""
ROCKET_PID=""
cleanup() {
  [ -n "$REDIS_PID" ] && kill "$REDIS_PID" 2>/dev/null || true
  [ -n "$ROCKET_PID" ] && kill "$ROCKET_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

redis-server --port "$REDIS_PORT" --save '' --appendonly yes --appendfsync everysec \
  --dir "$WORK" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!

ROCKET_MEM_ADDR="127.0.0.1:$ROCKET_PORT" \
ROCKET_MEM_AOF_PATH="$WORK/rocket.aof" \
ROCKET_MEM_SNAPSHOT_PATH="$WORK/rocket.snapshot" \
ROCKET_MEM_METRICS_ADDR="127.0.0.1:9187" \
ROCKET_MEM_RMP_ADDR="127.0.0.1:9188" \
  "$ROOT/target/release/rocket-mem" --config "$WORK/unused.toml" >"$WORK/rocket.log" 2>&1 &
ROCKET_PID=$!

sleep 1
redis-cli -p "$REDIS_PORT" ping >/dev/null
redis-cli -p "$ROCKET_PORT" ping >/dev/null

# One complete transaction: MULTI, a SET against a fixed key, EXEC. Built once and repeated N
# times, rather than generated per-key, since a distinct key per transaction would make this
# file O(N) in size for no benchmark-relevant reason -- throughput here is about transaction
# dispatch overhead, not keyspace variety (scripts/benchmark.sh's -r flag already covers
# keyspace-size effects for ordinary commands).
STREAM="$WORK/transactions.resp"
: >"$STREAM"
one_tx=$'*1\r\n$5\r\nMULTI\r\n*3\r\n$3\r\nSET\r\n$3\r\ntxk\r\n$3\r\ntxv\r\n*1\r\n$4\r\nEXEC\r\n'
for _ in $(seq 1 "$N"); do
  printf '%s' "$one_tx" >>"$STREAM"
done

# Minimal RESP frame counter (stdlib only): sends the whole stream, then counts top-level
# replies until it has seen 3*N of them -- MULTI's own +OK, the queued SET's +QUEUED, and EXEC's
# one-element reply array -- which is exactly what this fixed transaction shape produces from
# either server. Never fully decodes values, only frame boundaries, so it stays correct for any
# reply content without hardcoding "+OK"/"+QUEUED" text.
PYCLIENT="$WORK/tx_client.py"
cat >"$PYCLIENT" <<'PYEOF'
import socket
import sys
import time


def consume_frame(buf, pos):
    if pos >= len(buf):
        return None
    kind = buf[pos : pos + 1]
    if kind in (b"+", b"-", b":"):
        end = buf.find(b"\r\n", pos)
        return None if end == -1 else end + 2
    if kind == b"$":
        end = buf.find(b"\r\n", pos)
        if end == -1:
            return None
        length = int(buf[pos + 1 : end])
        if length == -1:
            return end + 2
        data_end = end + 2 + length + 2
        return None if data_end > len(buf) else data_end
    if kind == b"*":
        end = buf.find(b"\r\n", pos)
        if end == -1:
            return None
        elems = int(buf[pos + 1 : end])
        p = end + 2
        if elems == -1:
            return p
        for _ in range(elems):
            p = consume_frame(buf, p)
            if p is None:
                return None
        return p
    return None


host, port, path, n = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
with open(path, "rb") as f:
    data = f.read()

sock = socket.create_connection((host, port))
start = time.time()
sock.sendall(data)

expected = n * 3
buf = b""
pos = 0
count = 0
while count < expected:
    chunk = sock.recv(65536)
    if not chunk:
        break
    buf += chunk
    while True:
        new_pos = consume_frame(buf, pos)
        if new_pos is None:
            break
        pos = new_pos
        count += 1
elapsed = time.time() - start
sock.close()
print(
    f"{n} transactions in {elapsed:.6f}s ({n / elapsed:.2f} transactions/sec), "
    f"replies_seen={count} (expected {expected})"
)
PYEOF

run_case() { # $1=label $2=port
  echo -n "$1: "
  python3 "$PYCLIENT" 127.0.0.1 "$2" "$STREAM" "$N"
}

echo "--- MULTI/EXEC throughput ($N transactions, 2 commands each) ---"
run_case "redis-server" "$REDIS_PORT"
run_case "rocket-mem" "$ROCKET_PORT"
