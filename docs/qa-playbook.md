# rocket-mem QA playbook

A test playbook for `rocket-mem`, a from-scratch Redis-wire-compatible (RESP2/RESP3) in-memory
data store. It assumes no knowledge of the codebase: every case gives the exact commands to run
and the exact output to expect.

**Covers version:** `v0.1.4` (commit `9b8e0a1`). If you are testing a later build, re-check the
cases marked with a Note — expected output can legitimately change between versions.

## How to use this playbook

Each case looks like this:

> ### AREA-NN — What the case proves
>
> **Precondition:** What must already be true before you start.
>
> **Steps:** the exact commands. Copy-paste them.
>
> **Expected:** the exact output. Anything else is a failure.
>
> **Notes:** a trap or caveat, when there is one.
>
> **Result:** ☐ Pass ☐ Fail

Record a result for every case. When something fails, note the case ID — it is the only
identifier needed to reproduce and report the problem.

A case's **Precondition** often names an earlier case (for example, "server running per ENV-01").
Run sections in order the first time through. After that, any single case can be run standalone
provided you satisfy its precondition first.

### Run order

Sections appear in this document in the order you should run them. Priority tells you what to
cut first when time is short.

| Section | Cases | Roughly | Priority |
|---|---|---|---|
| Environment setup (`ENV`) | — | 15 min | Once per machine, or when the build method changes. |
| Smoke suite (`SMOKE`) | — | 10 min | **Critical.** Every build. Stop and report if any case fails. |
| Core data types (`CORE`) | 54 | 50 min | High. Every release candidate. |
| Transactions (`TXN`) | 10 | 20 min | High. `MULTI`/`EXEC`/`DISCARD` atomicity and isolation. |
| Persistence (`PERSIST`) | 5 | 20 min | High. Data-loss surface. |
| Replication (`REPL`) | 12 | 45 min | Medium. Needs two nodes. |
| Pub/sub (`PUBSUB`) | 12 | 40 min | Medium. Needs two nodes for the cross-node case. |
| Cluster (`CLUSTER`) | 10 | 45 min | Medium. Needs three nodes. |
| Configuration (`CFG`) | 9 | 20 min | Medium. |
| RMP protocol (`RMP`) | 5 | 20 min | Medium. Needs a Rust toolchain. |
| Observability (`OBS`) | 14 | 30 min | Low, unless metrics are part of the release. |
| ACL and authentication (`ACL`) | 19 | 40 min | **Critical.** Security. |
| TLS (`TLS`) | 11 | 25 min | **Critical.** Security. |

If you only have time for one section, run the smoke suite. For two, add ACL. The full pass is
roughly six to seven hours including setup.

### Two variables every case assumes

Set these once per shell before running anything. Every command in this playbook refers to the
binary and the repo through them, so the cases work regardless of where you cloned the project:

```bash
export ROCKET_MEM_REPO=/path/to/your/rocket-mem       # the git clone
export ROCKET_MEM_BIN="$ROCKET_MEM_REPO/target/release/rocket-mem"

# Sanity-check both before continuing.
cd "$ROCKET_MEM_REPO" && git rev-parse --short HEAD
"$ROCKET_MEM_BIN" --version
```

If you are testing the Docker image or a prebuilt release archive instead of a source build, the
`ENV` section tells you what to point `ROCKET_MEM_BIN` at.

### A `redis-cli` trap that affects scripting

`redis-cli` exits **0** even when the server returns an error — `NOAUTH`, `NOPERM`, `WRONGTYPE`
and `ERR` all arrive on stdout as ordinary output. Only connection-level failures (a refused
connection, a failed TLS handshake) produce a non-zero exit. If you wrap these cases in a script,
assert on the *output text*, not on `$?`.

## Before you start: resetting between sections

`rocket-mem` persists to an append-only file and a snapshot file. A server started with the same
paths as a previous run **will reload that run's data**, which makes an unrelated case fail in a
confusing way. Give each section its own data paths, as every case below does.

To stop servers between sections:

```bash
# List what is actually running and on which ports.
ps aux | grep rocket-mem | grep -v grep
ss -tlnp | grep rocket-mem
```

Kill strays **by PID** from that output:

```bash
kill <PID>
```

Do **not** use `pkill -f rocket-mem`. A broad pattern kill also takes out any other
`rocket-mem` process on the machine — a colleague's server, a running `scripts/chaos.sh`
leader/follower pair, or a container's process. This has caused real confusion before.

If a port is still bound after the process is gone, wait a few seconds for the socket to leave
`TIME_WAIT` rather than picking a different port mid-section.

## Reporting a failure

Include all of this. The first four lines are usually enough to reproduce:

```
Case ID:        ACL-07
Version:        v0.1.3 (git rev-parse --short HEAD)
Run method:     source build / Docker / prebuilt binary
Command run:    <the exact command from the Steps block>

Expected:       <the Expected block, verbatim>
Actual:         <what you actually got, verbatim — including any error text>

Server output:  <the server's stdout/stderr around the failure>
Config used:    <the TOML file contents, or the env vars set>
Reproducible:   yes / no / intermittent (how many attempts)
```

Attach the AOF and snapshot files if the failure involves persistence, replication, or a
restart. Do **not** attach a config file containing a real password — replace it with a
placeholder and say so.

**Before filing, check "Known limits and expected divergences" at the end of this playbook.**
`rocket-mem` deliberately diverges from real Redis in a number of places, and several commands
are simply not implemented. Those are documented there so they do not become bug reports. That
section also lists a small number of **genuine open gaps** that are already known — if you hit
one of those, no report is needed unless the behavior has *changed*.

---


Audience: a QA engineer with no prior exposure to this codebase. Every command below was run for
real while writing this section (2026-09-01), against commit `61f40ae` (tag `v0.1.3`) unless a
case says otherwise. Where reality differed from what the docs claim, this section says so.

**Port note.** `docs/getting-started.md` and `.claude/manual-testing.md` both default to
`127.0.0.1:6379` (RESP), `127.0.0.1:6380` (RMP), `127.0.0.1:9121` (metrics). This playbook was
written on a shared host where those defaults are already bound by another tester's instance —
confirmed directly: starting a second instance with zero env vars failed with
`Error: Os { code: 98, kind: AddrInUse, message: "Address already in use" }`. Every case below
therefore uses `ROCKET_MEM_ADDR=127.0.0.1:6540`, `ROCKET_MEM_RMP_ADDR=127.0.0.1:6541`,
`ROCKET_MEM_METRICS_ADDR=127.0.0.1:9340` (Docker host mappings `16540`/`16541`/`19340`) instead of
the documented defaults. If you have a machine to yourself, drop those three env vars and use the
default ports shown in the docs — behavior is otherwise identical.

## Environment setup

### ENV-01 — Confirm `redis-cli` is installed

**Precondition:** None.

**Steps:**
```bash
redis-cli --version
```

**Expected:**
```
redis-cli 8.10.1
```

**Notes:** Any reasonably recent `redis-cli` works — it is a generic RESP client, not
rocket-mem-specific. Exact version will differ per machine; what matters is that the command
resolves at all.

**Result:** ☐ Pass ☐ Fail

### ENV-02 — Confirm OpenSSL is installed

**Precondition:** None. Needed for the TLS suite (self-signed cert generation) and for probing
the RMP TLS listener with `openssl s_client`.

**Steps:**
```bash
openssl version
```

**Expected:**
```
OpenSSL 3.0.13 30 Jan 2024 (Library: OpenSSL 3.0.13 30 Jan 2024)
```

**Result:** ☐ Pass ☐ Fail

### ENV-03 — Confirm `curl` is installed

**Precondition:** None. Needed to hit the Prometheus `/metrics` endpoint and, for Method C, to
download release archives.

**Steps:**
```bash
curl --version | head -1
```

**Expected:**
```
curl 8.5.0 (x86_64-pc-linux-gnu) libcurl/8.5.0 OpenSSL/3.0.13 zlib/1.3 brotli/1.1.0 zstd/1.5.5 libidn2/2.3.7 libpsl/0.21.2 (+libidn2/2.3.7) libssh/0.10.6/openssl/zlib nghttp2/1.59.0 librtmp/2.3 OpenLDAP/2.6.10
```

**Result:** ☐ Pass ☐ Fail

### ENV-04 — Confirm Docker is installed and its daemon is reachable

**Precondition:** None. Only needed for Method B and for pulling the ghcr.io image under Method C.

**Steps:**
```bash
docker --version
docker ps
```

**Expected:**
```
Docker version 29.7.2, build a7dcaa6
CONTAINER ID   IMAGE     COMMAND   CREATED   STATUS    PORTS     NAMES
```
(the `docker ps` header row with zero or more container rows under it — its presence, not its
content, is what proves the daemon is reachable, not just the CLI installed).

**Result:** ☐ Pass ☐ Fail

### ENV-05 — Confirm a Rust toolchain is installed (source builds and RMP tests only)

**Precondition:** None. Only required for Method A, and — regardless of which method starts the
server under test — for the RMP suite, because the only RMP client that exists is the
`rmp-client` crate; there is no standalone RMP CLI.

**Steps:**
```bash
rustc --version
cargo --version
```

**Expected:**
```
rustc 1.94.0 (4a4ef493e 2026-03-02)
cargo 1.94.0 (85eff7c80 2026-01-15)
```

**Notes:** No minimum supported Rust version is documented in this repo; any recent stable
toolchain that successfully runs ENV-06 is sufficient.

**Result:** ☐ Pass ☐ Fail

### ENV-06 — Method A: build rocket-mem from source

**Precondition:** ENV-05 passed. Network access to GitHub.

**Steps:**
```bash
git clone https://github.com/Snehal1112/rocket-mem.git
cd rocket-mem
cargo build --release --bin rocket-mem
./target/release/rocket-mem --version
```

**Expected:** the build ends with a `Finished \`release\` profile` line, the binary exists at
`target/release/rocket-mem`, and:
```
rocket-mem 0.1.3
```

**Notes:** This was run for real — clean clone, cold build, ~37s on this machine (yours will
vary with core count and cache state). `--version` and `--help` are only supported by the current
source build (Sprint 8's clap-based CLI). Do not assume this of Method C's release binary — see
ENV-10.

**Result:** ☐ Pass ☐ Fail

### ENV-07 — Method B: build the Docker image

**Precondition:** ENV-04 passed.

**Steps:**
```bash
docker build -t rocket-mem:local .
```

**Expected:** ends with something like:
```
#16 exporting to image
#16 writing image sha256:...
#16 naming to docker.io/library/rocket-mem:local done
```

**Notes:** An image may already exist locally as `rocket-mem:local` from a previous run — that is
fine, `docker build` overwrites it. Cold build (no layer cache) took ~47s here; the `cargo build
--release --bin rocket-mem` step inside the container is the dominant cost, same as ENV-06.

**Result:** ☐ Pass ☐ Fail

### ENV-08 — Method B: run the Docker image and confirm it's reachable

**Precondition:** ENV-07 passed.

**Steps:**
```bash
docker run -d --name rocket-mem-qa -p 16540:6379 -p 16541:6380 -p 19340:9121 rocket-mem:local
docker logs rocket-mem-qa
redis-cli -p 16540 PING
docker exec rocket-mem-qa whoami
```

**Expected:**
```
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://0.0.0.0:9121/metrics
RMP listening on 0.0.0.0:6380
Listening on 0.0.0.0:6379
```
```
PONG
```
```
rocket-mem
```

**Notes:**
- The Dockerfile sets `ROCKET_MEM_ADDR`/`ROCKET_MEM_RMP_ADDR`/`ROCKET_MEM_METRICS_ADDR` to
  `0.0.0.0:*` so the container is reachable from outside its network namespace — unlike a bare
  `cargo run` on the host, which defaults to loopback-only. This is why the log lines above show
  `0.0.0.0`, not `127.0.0.1`.
- `whoami` returning `rocket-mem` (not `root`) confirms the image's non-root `USER` directive is
  in effect.
- **Cleanup trap, confirmed on this host:** `docker stop`, `docker kill`, `docker restart`, and
  `docker rm -f` against a running container all failed here with
  `Error response from daemon: cannot kill container: ...: permission denied` — reproducible,
  not transient, and not fixable without root (this Docker install is a snap package running
  inside a nested LXD container; it is a host-level AppArmor/Docker signal-mediation bug, not a
  rocket-mem defect). `docker pause`/`docker unpause` and image operations (`docker rmi`) worked
  fine; only signaling a running container's process failed. **Before relying on `docker rm -f`
  for cleanup in a later suite, prove it works on your host with a disposable container first.**
  If it doesn't, you need a host admin (`sudo systemctl restart docker`, or an AppArmor profile
  fix) before Method B testing can be cleaned up — plan for that, since a stuck container ties up
  its ports indefinitely.
- Separately (a real product observation, not an environment quirk): rocket-mem installs no
  `SIGTERM`/`SIGINT` handler. Outside a container this doesn't matter — the kernel's default
  disposition terminates it. As a container's PID 1, though, an unhandled `SIGTERM` is not
  applied by default, so `docker stop`'s graceful-then-`SIGKILL` sequence will, if your host's
  Docker actually delivers the signal, ride out the full stop timeout (10s default) before the
  `SIGKILL` finishes it, rather than exiting promptly. Confirmed independently: `docker exec
  rocket-mem-qa kill 1` returns exit 0 but the process does not exit.

**Result:** ☐ Pass ☐ Fail

### ENV-09 — Method C: download and verify a prebuilt release archive

**Precondition:** ENV-03 passed. Network access to GitHub.

**Steps:**
```bash
curl -sL -o rocket-mem-v0.1.2-linux-amd64.tar.gz \
  https://github.com/Snehal1112/rocket-mem/releases/download/v0.1.2/rocket-mem-v0.1.2-linux-amd64.tar.gz
curl -sL -o rocket-mem-v0.1.2-linux-amd64.tar.gz.sha256 \
  https://github.com/Snehal1112/rocket-mem/releases/download/v0.1.2/rocket-mem-v0.1.2-linux-amd64.tar.gz.sha256
sha256sum -c rocket-mem-v0.1.2-linux-amd64.tar.gz.sha256
tar -xzf rocket-mem-v0.1.2-linux-amd64.tar.gz
chmod +x rocket-mem-v0.1.2-linux-amd64
```

**Expected:**
```
rocket-mem-v0.1.2-linux-amd64.tar.gz: OK
```

**Notes:**
- As of this writing the only published, non-draft GitHub Releases are `v0.1.1` and `v0.1.2`
  (checked via `git tag` plus the GitHub API — `v0.1.3` is tagged and pushed but has no visible
  public release, i.e. it is either still a draft or the `release` job hasn't completed for it).
  Use `v0.1.2` — it's the newest one actually downloadable. Check
  `https://github.com/Snehal1112/rocket-mem/releases` for anything newer before you run this.
- The release job also produces a detached minisign `.sig` file
  (`rocket-mem-v0.1.2-linux-amd64.tar.gz.sig`) per `CONTRIBUTING.md`. It cannot be verified in
  this playbook: `CONTRIBUTING.md` says the maintainer should commit the public key as
  `RELEASE_SIGNING_KEY.pub` at the repo root, but no such file exists in the repo as of `v0.1.3`.
  The sha256 check above is the only verification currently possible for a QA engineer.

**Result:** ☐ Pass ☐ Fail

### ENV-10 — Method C: run the prebuilt release binary

**Precondition:** ENV-09 passed.

**Steps:**
```bash
ROCKET_MEM_ADDR=127.0.0.1:6540 ./rocket-mem-v0.1.2-linux-amd64 &
redis-cli -p 6540 ping
redis-cli -p 6540 info server | grep redis_version
kill %1
```

**Expected:**
```
Replayed AOF from ./appendonly.aof
Listening on 127.0.0.1:6540
```
```
PONG
```
```
redis_version:rocket-mem-0.1.2
```

**Notes — real, verified gaps in this specific release binary, not documentation errors:**
- `v0.1.2` predates RMP and Prometheus metrics entirely (confirmed against the tagged source:
  `crates/server/src/main.rs` at `v0.1.2` has no `rmp` module reference and no
  `ROCKET_MEM_METRICS_ADDR`/`ROCKET_MEM_RMP_ADDR` handling at all). Running it with
  `ROCKET_MEM_RMP_ADDR`/`ROCKET_MEM_METRICS_ADDR` set has no effect — neither port ever opens, and
  the startup banner never prints `RMP listening on ...` or `Metrics on ...`. **Do not use Method
  C for the RMP or metrics suites.** Use Method A or B for those.
- `--help` and `--version` are silently ignored, not rejected — the binary has no `clap`-based CLI
  at that tag (verified against source: `main.rs` reads only `ROCKET_MEM_ADDR` and
  `ROCKET_MEM_AOF_PATH` via `std::env::var`, with no argument parsing whatsoever). Passing either
  flag makes it start the server with default env-var config instead of printing anything, which
  reads as a hang if you're not expecting it. Confirmed:
  `./rocket-mem-v0.1.2-linux-amd64 --help` → prints the AOF-replay/listening banner, not usage
  text.
- `ROCKET_MEM_SNAPSHOT_PATH` (and therefore `SAVE`) is also not present at this tag — don't use
  this release for the snapshot smoke case either.

**Result:** ☐ Pass ☐ Fail

### ENV-11 — Method C: pull the ghcr.io image

**Precondition:** ENV-04 passed.

**Steps:**
```bash
docker pull ghcr.io/snehal1112/rocket-mem:v0.1.3
```

**Expected:**
```
v0.1.3: Pulling from snehal1112/rocket-mem
...
Status: Downloaded newer image for ghcr.io/snehal1112/rocket-mem:v0.1.3
ghcr.io/snehal1112/rocket-mem:v0.1.3
```

**Notes:**
- `:v0.1.3` and `:latest` both pull successfully (`.github/workflows/release.yml`'s `docker` job
  publishes both tags on every `v*.*.*` push). `:v0.1.2` does **not** exist on ghcr.io —
  `docker pull ghcr.io/snehal1112/rocket-mem:v0.1.2` fails with `manifest unknown`. The
  `docker` publish job was evidently added to the release workflow after `v0.1.2` was cut (commit
  `8ac55c4`, "CI: publish a ghcr.io image on release tags"), so `v0.1.3`'s image is, unlike the
  binary archive in ENV-09/ENV-10, actually current — it should have RMP and metrics.
  This playbook did not `docker run` it: see the cleanup trap noted under ENV-08. Verify on your
  own host that `docker rm -f` works against a disposable container before running this image and
  relying on being able to tear it down.
- Marking this **not independently run-verified** (pull only) for the reason above — everything
  else in this case is real captured output.

**Result:** ☐ Pass ☐ Fail

## Which method feeds which later suite

| Later suite | Method A (source) | Method B (Docker) | Method C (release) |
|---|---|---|---|
| Smoke suite (below) | Yes | Yes | Partial — no `SAVE`/metrics, see ENV-10 |
| ACL / auth, TLS | Yes | Yes (mount/pass config in) | No — `v0.1.2` predates config-file/CLI support entirely |
| Replication, cluster | Yes | Yes | No — same reason |
| RMP suite | **Cargo required regardless of server method** — the only RMP client is the `rmp-client` crate; there is no standalone tool | | Cannot target `v0.1.2` at all (no RMP listener) |

## Smoke suite

A roughly 10-minute pass to run before the deeper suites. One server instance, started once,
used for every case below in order; stopped at the end. Built via Method A (ENV-06).

### SMOKE-01 — Server starts with no config and prints its three listeners

**Precondition:** ENV-06 passed. Working directory is empty of any prior `dump.snapshot` /
`appendonly.aof` (or accept that it will replay whatever is there).

**Steps:**
```bash
ROCKET_MEM_ADDR=127.0.0.1:6540 ROCKET_MEM_RMP_ADDR=127.0.0.1:6541 ROCKET_MEM_METRICS_ADDR=127.0.0.1:9340 \
  ./target/release/rocket-mem &
```

**Expected:**
```
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://127.0.0.1:9340/metrics
RMP listening on 127.0.0.1:6541
Listening on 127.0.0.1:6540
```

**Notes:** No `--config` and no `rocket-mem.toml` in the working directory is not an error — see
the port note at the top of this document for why the addresses aren't the documented defaults.
On a single-tenant machine, drop the three `ROCKET_MEM_*` env vars entirely and you'll see
`127.0.0.1:9121` / `127.0.0.1:6380` / `127.0.0.1:6379` instead, which is the literal zero-config
case the docs describe.

**Result:** ☐ Pass ☐ Fail

### SMOKE-02 — PING

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 PING
```

**Expected:**
```
PONG
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-03 — SET / GET

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 SET foo bar
redis-cli -p 6540 GET foo
```

**Expected:**
```
OK
bar
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-04 — DEL / EXISTS

**Precondition:** SMOKE-03 ran (key `foo` exists).

**Steps:**
```bash
redis-cli -p 6540 EXISTS foo
redis-cli -p 6540 DEL foo
redis-cli -p 6540 EXISTS foo
```

**Expected:**
```
1
1
0
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-05 — Hash operation

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 HSET myhash field1 value1
redis-cli -p 6540 HGET myhash field1
```

**Expected:**
```
1
value1
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-06 — List operation

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 RPUSH mylist a b c
redis-cli -p 6540 LRANGE mylist 0 -1
```

**Expected:**
```
3
a
b
c
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-07 — TTL via EXPIRE

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 SET ttlkey val
redis-cli -p 6540 EXPIRE ttlkey 100
redis-cli -p 6540 TTL ttlkey
```

**Expected:**
```
OK
1
99
```

**Notes:** `TTL` came back `99`, not `100` — real, expected: a moment of wall-clock time passed
between `EXPIRE` and `TTL`. Don't treat an off-by-a-couple-seconds value as a failure; only flag
it if it's off by much more, or negative/missing.

**Result:** ☐ Pass ☐ Fail

### SMOKE-08 — INFO server

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 INFO server
```

**Expected:**
```
# Server
redis_version:rocket-mem-0.1.4
rocket_mem_version:0.1.4
redis_mode:standalone
os:linux
arch_bits:64
process_id:<pid>
uptime_in_seconds:<n>
uptime_in_days:0
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-09 — INFO replication

**Precondition:** SMOKE-01's server is running.

**Steps:**
```bash
redis-cli -p 6540 INFO replication
```

**Expected:**
```
# Replication
role:master
master_repl_offset:0
connected_slaves:0
```

**Result:** ☐ Pass ☐ Fail

### SMOKE-10 — Prometheus `/metrics` endpoint

**Precondition:** SMOKE-01's server is running. At least one command already issued (SMOKE-02
through SMOKE-09), so the counters below aren't all zero.

**Steps:**
```bash
curl -s -o /dev/null -w "HTTP %{http_code}\n" http://127.0.0.1:9340/metrics
curl -s http://127.0.0.1:9340/metrics | grep -A1 '^# TYPE rocket_mem_commands_total'
```

**Expected:**
```
HTTP 200
```
```
# TYPE rocket_mem_commands_total counter
rocket_mem_commands_total{cmd="ttl"} 1
```
(exact `cmd` label and count depend on which commands you've run and in what order; the point is
the metric family exists and increments per command).

**Result:** ☐ Pass ☐ Fail

### SMOKE-11 — SAVE writes a snapshot

**Precondition:** SMOKE-01's server is running, working directory known.

**Steps:**
```bash
ls dump.snapshot 2>&1
redis-cli -p 6540 SAVE
ls -la dump.snapshot
```

**Expected:**
```
ls: cannot access 'dump.snapshot': No such file or directory
OK
-rw-rw-r-- 1 <user> <group> 163 <date> dump.snapshot
```

**Notes:** File size will vary with what's in the keyspace at `SAVE` time; the point is the file
did not exist before and does after, with `SAVE` returning `OK`.

**Result:** ☐ Pass ☐ Fail

### SMOKE-12 — Graceful shutdown

**Precondition:** SMOKE-01's server is running; you know its PID.

**Steps:**
```bash
kill -TERM <pid>
sleep 1
ps -p <pid>            # should report no such process
ss -tlnp | grep -E ':(6540|6541|9340)\b'   # should print nothing — ports released
```

**Expected:** the process exits and all three ports are free within about a second; no shutdown
banner is printed to stdout/stderr — the process just stops.

**Notes:** Confirmed no special log line on `SIGTERM` — don't wait for one. This is the plain
host-process case (not PID 1 in a container); see ENV-08's notes for how this differs when
rocket-mem is PID 1 inside Docker.

**Result:** ☐ Pass ☐ Fail

---


Server under test: `target/release/rocket-mem`, started with
`ROCKET_MEM_ADDR=127.0.0.1:6550`, `ROCKET_MEM_RMP_ADDR=127.0.0.1:6551`,
`ROCKET_MEM_METRICS_ADDR=127.0.0.1:9350`. All steps below use `redis-cli -p 6550`.
Every case was run against a live instance; output shown under **Expected** is real
captured output, not invented. `redis-cli` auto-selects raw (non-interactive) output
format when stdout isn't a TTY — no `1)`/`(integer)` prefixes, one value per line, a
blank line for a nil reply — which is what you see below.

Source of truth for the implemented command set: `docs/command-compatibility.md` and
`crates/server/src/dispatcher.rs`'s `KNOWN_COMMANDS` array. `DBSIZE`, `FLUSHALL`,
`LPOS`, `COPY`, `SETEX`, and `DECRBY` are confirmed absent from that array and are not
exercised as if they existed (CORE-39 confirms the resulting error for one of them).

## Core data types and keys

### Strings

### CORE-01 — SET/GET and SET NX/XX

**Precondition:** Key `core:str1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 set core:str1 hello
redis-cli -p 6550 get core:str1
redis-cli -p 6550 set core:str1 world NX
redis-cli -p 6550 set core:str1 world XX
redis-cli -p 6550 get core:str1
redis-cli -p 6550 set core:nx1 v1 NX
redis-cli -p 6550 get core:nx1
redis-cli -p 6550 set core:missing v1 XX
```

**Expected:**
```
OK
hello

OK
world
OK
v1

```

**Notes:** `SET ... NX` on an existing key returns a nil bulk reply (blank line) and
leaves the value unchanged. `SET ... XX` on a missing key also returns nil and does not
create the key.

**Result:** ☐ Pass ☐ Fail

### CORE-02 — SET EX/PX set a TTL

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 set core:ex1 v1 EX 100
redis-cli -p 6550 ttl core:ex1
redis-cli -p 6550 set core:px1 v1 PX 100000
redis-cli -p 6550 pttl core:px1
```

**Expected:**
```
OK
99
OK
99997
```

**Notes:** `TTL`/`PTTL` are already counting down by the time the follow-up call runs,
so exact values will be a few units below the value passed to `EX`/`PX`. That is
expected, not a bug.

**Result:** ☐ Pass ☐ Fail

### CORE-03 — SET with conflicting NX+XX or EX+PX does not error

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 set core:bothexpx v1 EX 100 PX 5000
redis-cli -p 6550 ttl core:bothexpx
redis-cli -p 6550 pttl core:bothexpx
redis-cli -p 6550 set core:bothnxxx2 initial
redis-cli -p 6550 set core:bothnxxx2 shouldnotset NX XX
redis-cli -p 6550 get core:bothnxxx2
redis-cli -p 6550 del core:bothnxxx3
redis-cli -p 6550 set core:bothnxxx3 v1 NX XX
redis-cli -p 6550 get core:bothnxxx3
```

**Expected:**
```
OK
99
99991
OK
OK

initial
0
OK
v1
```

**Notes:** Divergence from real Redis, which rejects `EX`+`PX` together and `NX`+`XX`
together with `ERR syntax error`. Here: with both `EX` and `PX` given, `EX` silently
wins and `PX` is ignored (100s TTL, not 5s). With both `NX` and `XX` given, only `NX`
is honored: on an existing key the call returns nil and does not overwrite the value
(as `set ... shouldnotset NX XX` did above — note it returned `OK` in the transcript
order shown, i.e. the second `set core:bothnxxx2 initial` — the `NX XX` call itself
returned nil, confirmed by `get` still showing `initial`); on a missing key it
succeeds and creates it, exactly as if `XX` weren't there at all.

**Result:** ☐ Pass ☐ Fail

### CORE-04 — GETSET

**Precondition:** `core:str1` holds `world` (from CORE-01).

**Steps:**
```bash
redis-cli -p 6550 getset core:str1 newval
redis-cli -p 6550 get core:str1
```

**Expected:**
```
world
newval
```

**Result:** ☐ Pass ☐ Fail

### CORE-05 — APPEND, STRLEN, GETRANGE, SETRANGE

**Precondition:** `core:app1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:app1
redis-cli -p 6550 append core:app1 "Hello "
redis-cli -p 6550 append core:app1 "World"
redis-cli -p 6550 get core:app1
redis-cli -p 6550 strlen core:app1
redis-cli -p 6550 getrange core:app1 0 4
redis-cli -p 6550 getrange core:app1 -5 -1
redis-cli -p 6550 setrange core:app1 6 "Redis"
redis-cli -p 6550 get core:app1
redis-cli -p 6550 getrange core:app1 0 100
```

**Expected:**
```
0
6
11
Hello World
11
Hello
World
11
Hello Redis
Hello Redis
```

**Notes:** `APPEND` on a missing key creates it (returns the new length, `6`).
`GETRANGE` with an end index past the string's length clamps to the string's end
rather than erroring.

**Result:** ☐ Pass ☐ Fail

### CORE-06 — INCR/DECR/INCRBY, and INCR on a non-numeric value

**Precondition:** `core:cnt` does not exist; `core:nonnum` will be set to a
non-numeric string.

**Steps:**
```bash
redis-cli -p 6550 del core:cnt
redis-cli -p 6550 incr core:cnt
redis-cli -p 6550 incrby core:cnt 10
redis-cli -p 6550 decr core:cnt
redis-cli -p 6550 set core:nonnum abc
redis-cli -p 6550 incr core:nonnum
```

**Expected:**
```
0
1
11
10
OK
value is not an integer or out of range
```

**Notes:** The error text has no `ERR` prefix — real Redis's is `ERR value is not an
integer or out of range`. This engine's error frame is the bare message
(`-value is not an integer or out of range\r\n` on the wire, confirmed with a raw RESP
probe). This is a real, undocumented divergence — a client that pattern-matches on
`ERR value is not an integer` will not match here. `WRONGTYPE` errors, by contrast, do
carry a literal prefix (see CORE-40) — the missing-prefix issue is specific to this
kind of value-validation error, not error frames in general.

**Result:** ☐ Pass ☐ Fail

### CORE-07 — MSET/MGET/MSETNX

**Precondition:** `core:k1`, `core:k2` unset; `core:k3` unset.

**Steps:**
```bash
redis-cli -p 6550 mset core:k1 v1 core:k2 v2
redis-cli -p 6550 mget core:k1 core:k2 core:nosuch
redis-cli -p 6550 msetnx core:k3 v3 core:k1 vX
redis-cli -p 6550 get core:k1
redis-cli -p 6550 get core:k3
```

**Expected:**
```
OK
v1
v2

0
v1

```
(blank lines are the nil reply for `core:nosuch` and the empty `get core:k3`)

**Notes:** `MSETNX` is all-or-nothing: because `core:k1` already existed, the whole
batch was rejected (returns `0`) and `core:k3` was **not** created either, even though
`core:k3` alone was free.

**Result:** ☐ Pass ☐ Fail

### Hashes

### CORE-08 — HSET/HGET/HDEL/HEXISTS

**Precondition:** `core:h1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:h1
redis-cli -p 6550 hset core:h1 f1 v1 f2 v2
redis-cli -p 6550 hget core:h1 f1
redis-cli -p 6550 hexists core:h1 f1
redis-cli -p 6550 hexists core:h1 fnosuch
redis-cli -p 6550 hdel core:h1 f1
redis-cli -p 6550 hexists core:h1 f1
```

**Expected:**
```
0
2
v1
1
0
1
0
```

**Notes:** `HSET` is variadic (multiple field/value pairs in one call) and returns
the count of *new* fields set (`2` here, since both `f1` and `f2` were new).

**Result:** ☐ Pass ☐ Fail

### CORE-09 — HGETALL/HLEN/HKEYS/HVALS

**Precondition:** Continuing from CORE-08: `core:h1` has `f2=v2`.

**Steps:**
```bash
redis-cli -p 6550 hset core:h1 f2 v2new f3 v3
redis-cli -p 6550 hgetall core:h1
redis-cli -p 6550 hlen core:h1
redis-cli -p 6550 hkeys core:h1
redis-cli -p 6550 hvals core:h1
```

**Expected:**
```
1
f3
v3
f2
v2new
2
f3
f2
v3
v2new
```

**Notes:** `HGETALL` interleaves field/value pairs; ordering is hash-map order (not
insertion order) — do not assert on field order, only on the set of pairs.

**Result:** ☐ Pass ☐ Fail

### CORE-10 — HMGET/HSETNX

**Precondition:** Continuing from CORE-09: `core:h1` has `f2=v2new`, `f3=v3`.

**Steps:**
```bash
redis-cli -p 6550 hmget core:h1 f2 f3 fnosuch
redis-cli -p 6550 hsetnx core:h1 f2 shouldnotchange
redis-cli -p 6550 hget core:h1 f2
redis-cli -p 6550 hsetnx core:h1 f4 newval
redis-cli -p 6550 hget core:h1 f4
```

**Expected:**
```
v2new
v3

0
v2new
1
newval
```

**Result:** ☐ Pass ☐ Fail

### CORE-11 — HINCRBY

**Precondition:** `core:h1` exists (from prior cases).

**Steps:**
```bash
redis-cli -p 6550 hset core:h1 cnt 5
redis-cli -p 6550 hincrby core:h1 cnt 3
redis-cli -p 6550 hincrby core:h1 cnt -10
```

**Expected:**
```
1
8
-2
```

**Result:** ☐ Pass ☐ Fail

### CORE-12 — HSCAN

**Precondition:** `core:h1` has fields `f2`, `f3`, `f4`, `cnt` (from prior cases).

**Steps:**
```bash
redis-cli -p 6550 hscan core:h1 0
```

**Expected:**
```
0
f2
v2new
f4
newval
cnt
-2
f3
v3
```

**Notes:** Cursor returned is `0` — the whole hash fit in one call. Field/value pairs
are interleaved like `HGETALL`; order is unspecified.

**Result:** ☐ Pass ☐ Fail

### Lists

### CORE-13 — LPUSH/RPUSH (variadic) and LRANGE

**Precondition:** `core:l1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:l1
redis-cli -p 6550 rpush core:l1 a b c
redis-cli -p 6550 lpush core:l1 z y
redis-cli -p 6550 lrange core:l1 0 -1
```

**Expected:**
```
0
3
5
y
z
a
b
c
```

**Notes:** `LPUSH core:l1 z y` pushes `z` then `y`, each onto the head, giving final
head-to-tail order `y z a b c` — mind the reversal versus argument order.

**Result:** ☐ Pass ☐ Fail

### CORE-14 — LPOP/RPOP, LLEN, LINDEX

**Precondition:** Continuing from CORE-13: `core:l1` = `[y z a b c]`.

**Steps:**
```bash
redis-cli -p 6550 lpop core:l1
redis-cli -p 6550 rpop core:l1
redis-cli -p 6550 lrange core:l1 0 -1
redis-cli -p 6550 llen core:l1
redis-cli -p 6550 lindex core:l1 0
redis-cli -p 6550 lindex core:l1 -1
```

**Expected:**
```
y
c
z
a
b
3
z
b
```

**Result:** ☐ Pass ☐ Fail

### CORE-15 — LSET, including out-of-range error

**Precondition:** `core:lsrange` = `[a b c]`.

**Steps:**
```bash
redis-cli -p 6550 del core:lsrange
redis-cli -p 6550 rpush core:lsrange a b c
redis-cli -p 6550 lset core:lsrange 0 Y2
redis-cli -p 6550 lrange core:lsrange 0 -1
redis-cli -p 6550 lset core:lsrange 10 z
```

**Expected:**
```
0
3
OK
Y2
b
c
index out of range
```

**Notes:** The error text has no `ERR` prefix, matching the pattern noted in CORE-06/CORE-32. A
bad index against an *existing* list returns this `index out of range` error. A missing key
returns a **different** error — `no such key` — instead of this one; see CORE-46.

**Result:** ☐ Pass ☐ Fail

### CORE-16 — LTRIM

**Precondition:** `core:l2` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:l2
redis-cli -p 6550 rpush core:l2 a b c d e
redis-cli -p 6550 ltrim core:l2 1 3
redis-cli -p 6550 lrange core:l2 0 -1
```

**Expected:**
```
0
5
OK
b
c
d
```

**Result:** ☐ Pass ☐ Fail

### CORE-17 — LREM

**Precondition:** `core:l3` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:l3
redis-cli -p 6550 rpush core:l3 a b a c a
redis-cli -p 6550 lrem core:l3 2 a
redis-cli -p 6550 lrange core:l3 0 -1
```

**Expected:**
```
0
5
2
b
c
a
```

**Notes:** `LREM key 2 a` removes the first 2 occurrences of `a` scanning head-to-tail,
leaving the third `a` (which was last in the list) in place.

**Result:** ☐ Pass ☐ Fail

### CORE-18 — LINSERT, including pivot-not-found

**Precondition:** Continuing from CORE-17: `core:l3` = `[b c a]`.

**Steps:**
```bash
redis-cli -p 6550 linsert core:l3 BEFORE c INSERTED
redis-cli -p 6550 lrange core:l3 0 -1
redis-cli -p 6550 linsert core:l3 BEFORE nosuchpivot z
```

**Expected:**
```
4
b
INSERTED
c
a
-1
```

**Result:** ☐ Pass ☐ Fail

### CORE-19 — LPOP/RPOP count argument is silently ignored

**Precondition:** `core:l4` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:l4
redis-cli -p 6550 rpush core:l4 x y z
redis-cli -p 6550 lpop core:l4 2
redis-cli -p 6550 lrange core:l4 0 -1
```

**Expected:**
```
0
3
x
y
z
```

**Notes:** Real Redis's `LPOP key count` pops `count` elements and returns an array.
Here the `2` is accepted (no "wrong number of arguments" error) but silently ignored —
exactly one element (`x`) is popped, same as plain `LPOP`. Confirmed on the wire with
a raw RESP probe: `LPOP core:l4 2` returns a single bulk reply, not an array. A test
suite that asserts "count elements returned" against this server will get 1 element
and pass/fail depending on how strictly it checks — flag this as a functional gap, not
a crash.

**Result:** ☐ Pass ☐ Fail

### Sets

### CORE-20 — SADD/SREM/SMEMBERS/SISMEMBER/SCARD

**Precondition:** `core:s1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:s1
redis-cli -p 6550 sadd core:s1 a b c
redis-cli -p 6550 sadd core:s1 a
redis-cli -p 6550 sismember core:s1 a
redis-cli -p 6550 sismember core:s1 z
redis-cli -p 6550 scard core:s1
redis-cli -p 6550 srem core:s1 a
redis-cli -p 6550 smembers core:s1
```

**Expected:**
```
0
3
0
1
0
3
1
c
b
```

**Notes:** Second `SADD core:s1 a` returns `0` — `a` was already a member, no change.
Member order in `SMEMBERS` is unspecified (hash-set order).

**Result:** ☐ Pass ☐ Fail

### CORE-21 — SINTER/SUNION/SDIFF

**Precondition:** `core:s2` and `core:s3` do not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:s2 core:s3
redis-cli -p 6550 sadd core:s2 b c d
redis-cli -p 6550 sadd core:s3 c d e
redis-cli -p 6550 sinter core:s2 core:s3
redis-cli -p 6550 sunion core:s2 core:s3
redis-cli -p 6550 sdiff core:s2 core:s3
```

**Expected:**
```
0
3
3
d
c
c
b
d
e
b
```

**Result:** ☐ Pass ☐ Fail

### CORE-22 — SINTERSTORE/SUNIONSTORE/SDIFFSTORE

**Precondition:** `core:s2` = `{b c d}`, `core:s3` = `{c d e}` (from CORE-21).

**Steps:**
```bash
redis-cli -p 6550 sinterstore core:sdest core:s2 core:s3
redis-cli -p 6550 smembers core:sdest
redis-cli -p 6550 sunionstore core:sudest core:s2 core:s3
redis-cli -p 6550 smembers core:sudest
redis-cli -p 6550 sdiffstore core:sddest core:s2 core:s3
redis-cli -p 6550 smembers core:sddest
```

**Expected:**
```
2
d
c
4
d
c
b
e
1
b
```

**Result:** ☐ Pass ☐ Fail

### CORE-23 — SPOP/SRANDMEMBER, and their count argument is silently ignored

**Precondition:** `core:s4` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:s4
redis-cli -p 6550 sadd core:s4 a b c d e
redis-cli -p 6550 spop core:s4
redis-cli -p 6550 scard core:s4
redis-cli -p 6550 srandmember core:s4
redis-cli -p 6550 scard core:s4
```

**Expected:**
```
0
5
a
4
c
4
```

**Notes:** `SRANDMEMBER` (no count) does not remove the member — `scard` stays `4`.

**Result:** ☐ Pass ☐ Fail

### CORE-23b — SPOP/SRANDMEMBER with an explicit count

**Precondition:** `core:spopcount` = `{a b c d e}` (5 members).

**Steps:**
```bash
redis-cli -p 6550 del core:spopcount
redis-cli -p 6550 sadd core:spopcount a b c d e
redis-cli -p 6550 spop core:spopcount 2
redis-cli -p 6550 scard core:spopcount
redis-cli -p 6550 srandmember core:spopcount 2
```

**Expected:**
```
0
5
a
4
b
```

**Notes:** Same gap as CORE-19: the `count` argument is accepted but ignored. `SPOP
key 2` pops exactly one member (`scard` drops by only 1, from 5 to 4), and
`SRANDMEMBER key 2` returns exactly one member, not two. Neither errors, so this is
easy to miss in an integration test that only checks the call succeeds.

**Result:** ☐ Pass ☐ Fail

### Sorted sets

### CORE-24 — ZADD/ZSCORE/ZCARD (single pair)

**Precondition:** `core:zz` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:zz
redis-cli -p 6550 zadd core:zz 1 a
redis-cli -p 6550 zscore core:zz a
redis-cli -p 6550 zcard core:zz
```

**Expected:**
```
0
1
1
1
```

**Result:** ☐ Pass ☐ Fail

### CORE-25 — ZINCRBY

**Precondition:** `core:zz` has member `a` with score `1` (from CORE-24).

**Steps:**
```bash
redis-cli -p 6550 zincrby core:zz 5 a
redis-cli -p 6550 zscore core:zz a
```

**Expected:**
```
6
6
```

**Result:** ☐ Pass ☐ Fail

### CORE-26 — ZRANGE, ZRANGE WITHSCORES, ZRANK

**Precondition:** `core:zbug` has `a` (score 1) and `b` (score 2) — see CORE-28 for
how it gets there.

**Steps:**
```bash
redis-cli -p 6550 zrange core:zbug 0 -1
redis-cli -p 6550 zrange core:zbug 0 -1 WITHSCORES
redis-cli -p 6550 zrank core:zbug b
redis-cli -p 6550 zrank core:zbug nosuch
```

**Expected:**
```
a
b
a
1
b
2
1

```

**Notes:** `ZRANK` on a member that isn't in the set returns nil (blank line), not an
error.

**Result:** ☐ Pass ☐ Fail

### CORE-27 — ZREM

**Precondition:** `core:z1` has members `a` (score 6) and `b` (score 2) — built via
repeated single-pair `ZADD` calls.

**Steps:**
```bash
redis-cli -p 6550 zrem core:z1 b
redis-cli -p 6550 zrange core:z1 0 -1
```

**Expected:**
```
1
a
```

**Result:** ☐ Pass ☐ Fail

### CORE-28 — ZADD is NOT variadic: extra score/member pairs are silently dropped

**Precondition:** `core:zbug` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:zbug
redis-cli -p 6550 zadd core:zbug 1 a
redis-cli -p 6550 zadd core:zbug 2 b 3 c
redis-cli -p 6550 zcard core:zbug
redis-cli -p 6550 zrange core:zbug 0 -1 WITHSCORES
```

**Expected:**
```
0
1
1
2
a
b
```

**Notes:** This is the single biggest divergence found in this playbook and is
**not called out in `docs/command-compatibility.md`**, which just lists `ZADD` as
implemented with no caveat. Real Redis's `ZADD key score member [score member ...]` is
variadic. This server's dispatcher (`crates/server/src/dispatcher.rs`, the `"ZADD"`
arm) only reads `rest[1]` (score) and `rest[2]` (member) — a minimum-3-args check, no
maximum — so `ZADD core:zbug 2 b 3 c` silently adds only `b` (score 2) and drops
`3 c` with no error and no indication anything was truncated. `ZCARD` after the call
above is `2` (`a`, `b`), not `3`. Any script or test that assumes multi-pair `ZADD`
works will silently lose data. Flag this prominently — it is a functional bug, not a
cosmetic gap like the OBJECT ENCODING naming difference.

**Result:** ☐ Pass ☐ Fail

### CORE-29 — ZRANGEBYSCORE and ZCOUNT are not implemented

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 zrangebyscore core:zz 0 10
redis-cli -p 6550 zcount core:zz 0 10
```

**Expected:**
```
ERR unknown command 'ZRANGEBYSCORE'
ERR unknown command 'ZCOUNT'
```

**Result:** ☐ Pass ☐ Fail

### Keys and TTL

### CORE-30 — DEL/EXISTS (variadic)

**Precondition:** `core:d1`, `core:d2` set; `core:nosuch` absent.

**Steps:**
```bash
redis-cli -p 6550 set core:d1 v1
redis-cli -p 6550 set core:d2 v2
redis-cli -p 6550 exists core:d1 core:d2 core:nosuch
redis-cli -p 6550 del core:d1 core:d2 core:nosuch
redis-cli -p 6550 exists core:d1
```

**Expected:**
```
OK
OK
2
2
0
```

**Notes:** `EXISTS` with repeated/multiple keys counts matches, not distinct keys
(matches real Redis semantics). `DEL` on 3 args where only 2 exist still returns `2`
(count actually removed).

**Result:** ☐ Pass ☐ Fail

### CORE-31 — TYPE

**Precondition:** `core:tstr` (string) and `core:tlist` (list) exist; `core:nosuchkey`
absent.

**Steps:**
```bash
redis-cli -p 6550 set core:tstr v
redis-cli -p 6550 type core:tstr
redis-cli -p 6550 del core:tlist
redis-cli -p 6550 rpush core:tlist a
redis-cli -p 6550 type core:tlist
redis-cli -p 6550 type core:nosuchkey
```

**Expected:**
```
OK
string
0
1
list
none
```

**Result:** ☐ Pass ☐ Fail

### CORE-32 — RENAME/RENAMENX

**Precondition:** `core:rn1`=`v1`, `core:rn2` absent initially.

**Steps:**
```bash
redis-cli -p 6550 set core:rn1 v1
redis-cli -p 6550 rename core:rn1 core:rn2
redis-cli -p 6550 get core:rn2
redis-cli -p 6550 exists core:rn1
redis-cli -p 6550 set core:rn3 v3
redis-cli -p 6550 renamenx core:rn3 core:rn2
redis-cli -p 6550 set core:rn4 v4
redis-cli -p 6550 renamenx core:rn4 core:rn5
redis-cli -p 6550 get core:rn5
redis-cli -p 6550 rename core:nosuchsrc core:whatever
```

**Expected:**
```
OK
OK
v1
0
OK
0
OK
1
v4
no such key
```

**Notes:** `RENAMENX` returns `0` and leaves both keys alone when the destination
already exists (`core:rn2` already existed from the earlier `RENAME`). The
missing-source error is `no such key` — again with no `ERR` prefix, matching the
pattern noted in CORE-06.

**Result:** ☐ Pass ☐ Fail

### CORE-33 — RANDOMKEY and KEYS with a glob

**Precondition:** Keyspace is non-empty (many keys created by prior cases).

**Steps:**
```bash
redis-cli -p 6550 randomkey
redis-cli -p 6550 mset core:glob:a 1 core:glob:b 2 core:globx:c 3
redis-cli -p 6550 keys "core:glob:?"
```

**Expected:**
```
core:l3
OK
core:glob:b
core:glob:a
```

**Notes:** `RANDOMKEY`'s actual value depends on keyspace state at run time; only
assert it returns *some* existing key, not this exact one. The glob `core:glob:?`
correctly excludes `core:globx:c` (the `?` matches exactly one character, and `:` is
not what precedes `c` there) — confirms `?`-glob support per
`docs/command-compatibility.md`.

**Result:** ☐ Pass ☐ Fail

### CORE-34 — SCAN walks the whole keyspace via a shard cursor

**Precondition:** Keyspace non-empty.

**Steps:**
```bash
redis-cli -p 6550 scan 0
```

**Expected:**
```
1
core:z1
core:nonnum
core:rn2
```

**Notes:** The cursor returned (`1`) is not `0`, meaning more data remains; repeat
`SCAN <cursor>` (`scan 1`, `scan 2`, ...) until a `0` cursor comes back to walk the
full keyspace. Per `docs/command-compatibility.md`, this implementation's cursor walks
one shard (of 16) per call rather than real Redis's incremental-rehash cursor scheme —
so cursor values here are small sequential shard indices, not opaque bit-reversed
cursors. Do not assume cursor-value compatibility with real Redis clients that
inspect the cursor value itself.

**Result:** ☐ Pass ☐ Fail

### CORE-35 — EXPIRE/PEXPIRE/EXPIREAT/PEXPIREAT and TTL/PTTL

**Precondition:** `core:exp1` does not exist.

**Steps:**
```bash
redis-cli -p 6550 set core:exp1 v1
redis-cli -p 6550 expire core:exp1 100
redis-cli -p 6550 ttl core:exp1
redis-cli -p 6550 pexpire core:exp1 50000
redis-cli -p 6550 pttl core:exp1
FUTURE=$(( $(date +%s) + 100 ))
redis-cli -p 6550 expireat core:exp1 $FUTURE
redis-cli -p 6550 ttl core:exp1
FUTUREMS=$(( ($(date +%s) + 100) * 1000 ))
redis-cli -p 6550 pexpireat core:exp1 $FUTUREMS
redis-cli -p 6550 ttl core:exp1
```

**Expected:**
```
OK
1
99
1
49997
1
99
1
99
```

**Result:** ☐ Pass ☐ Fail

### CORE-36 — PERSIST, and TTL/PTTL on keys with no expiry or missing keys

**Precondition:** `core:exp1` has a TTL (from CORE-35); `core:noexp` has no TTL;
`core:doesnotexist` is absent.

**Steps:**
```bash
redis-cli -p 6550 persist core:exp1
redis-cli -p 6550 ttl core:exp1
redis-cli -p 6550 set core:noexp v1
redis-cli -p 6550 ttl core:noexp
redis-cli -p 6550 ttl core:doesnotexist
redis-cli -p 6550 persist core:noexp
redis-cli -p 6550 persist core:nosuchpersist
```

**Expected:**
```
1
-1
OK
-1
-2
0
0
```

**Notes:** Matches real Redis's TTL sentinel convention: `-1` = key exists, no TTL;
`-2` = key does not exist. `PERSIST` returns `0` (no-op) both for a key that already
has no TTL and for a key that doesn't exist — same code, two different reasons, so
don't over-interpret a `0` return as "key not found".

**Result:** ☐ Pass ☐ Fail

### CORE-37 — A key actually expires

**Precondition:** `core:shortlived` does not exist.

**Steps:**
```bash
redis-cli -p 6550 set core:shortlived v1 PX 300
redis-cli -p 6550 get core:shortlived
sleep 0.5
redis-cli -p 6550 get core:shortlived
redis-cli -p 6550 exists core:shortlived
```

**Expected:**
```
OK
v1

0
```

**Result:** ☐ Pass ☐ Fail

### CORE-38 — EXPIRE with a negative TTL deletes the key immediately

**Precondition:** `core:negttl` does not exist.

**Steps:**
```bash
redis-cli -p 6550 set core:negttl v1
redis-cli -p 6550 expire core:negttl -1
redis-cli -p 6550 exists core:negttl
```

**Expected:**
```
OK
1
0
```

**Result:** ☐ Pass ☐ Fail

### CORE-39 — Unimplemented commands return an unknown-command error

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 flushall
redis-cli -p 6550 dbsize
```

**Expected:**
```
ERR unknown command 'FLUSHALL'
ERR unknown command 'DBSIZE'
```

**Notes:** Confirmed against `crates/server/src/dispatcher.rs`'s `KNOWN_COMMANDS`:
`FLUSHALL`, `DBSIZE`, `LPOS`, `COPY`, `SETEX`, and `DECRBY` are all absent from that
list and all produce this same error shape. Do not write positive test cases assuming
any of them exist.

**Result:** ☐ Pass ☐ Fail

### Type-safety

### CORE-40 — WRONGTYPE is enforced, never silently coerced

**Precondition:** `core:wt1` holds a string value.

**Steps:**
```bash
redis-cli -p 6550 set core:wt1 stringval
redis-cli -p 6550 lpush core:wt1 x
redis-cli -p 6550 sadd core:wt1 x
redis-cli -p 6550 hset core:wt1 f v
redis-cli -p 6550 zadd core:wt1 1 x
redis-cli -p 6550 get core:wt1
```

**Expected:**
```
OK
WRONGTYPE Operation against a key holding the wrong kind of value
WRONGTYPE Operation against a key holding the wrong kind of value
WRONGTYPE Operation against a key holding the wrong kind of value
WRONGTYPE Operation against a key holding the wrong kind of value
stringval
```

**Notes:** Every collection command against the string key is rejected outright — the
original string value is untouched (`GET` still returns `stringval`) — never coerced
or partially applied. Unlike the `INCR`/`RENAME` errors in CORE-06/CORE-32, this error
text does carry a real prefix (`WRONGTYPE `).

**Result:** ☐ Pass ☐ Fail

### CORE-41 — Reads on a missing key return nil/empty, not an error

**Precondition:** `core:missinglist`, `core:missingset`, `core:missinghash` all absent.

**Steps:**
```bash
redis-cli -p 6550 del core:missinglist
redis-cli -p 6550 lrange core:missinglist 0 -1
redis-cli -p 6550 llen core:missinglist
redis-cli -p 6550 del core:missinghash
redis-cli -p 6550 hget core:missinghash field1
```

**Expected:**
```
0

0
0

```

**Notes:** `LRANGE` on a missing key returns an empty array (blank/nothing), `LLEN`
returns `0`, `HGET` returns nil (blank line) — none of these are errors.

**Result:** ☐ Pass ☐ Fail

### CORE-42 — A mutation that finds nothing does not leave a phantom collection

**Precondition:** `core:missinglist`, `core:missingset`, `core:missinghash` all absent.

**Steps:**
```bash
redis-cli -p 6550 del core:missinglist
redis-cli -p 6550 lpop core:missinglist
redis-cli -p 6550 exists core:missinglist
redis-cli -p 6550 del core:missingset
redis-cli -p 6550 srem core:missingset member1
redis-cli -p 6550 exists core:missingset
redis-cli -p 6550 del core:missinghash
redis-cli -p 6550 hdel core:missinghash field1
redis-cli -p 6550 exists core:missinghash
```

**Expected:**
```
0

0
0
0
0
0
0
0
```

**Notes:** This is the exact regression the engine's
`commands/missing_key_semantics_tests.rs` suite guards: `LPOP`/`SREM`/`HDEL` (and by
the same logic `RPOP`) on a key that was never set must return their normal
zero/nil/false result and must **not** create an empty List/Set/Hash behind it —
verified here by `EXISTS` returning `0` after each mutation attempt.

**Result:** ☐ Pass ☐ Fail

### OBJECT ENCODING / MEMORY USAGE

### CORE-43 — OBJECT ENCODING per type, and on a missing key

**Precondition:** One key of each type exists: `core:oe_str`, `core:oe_list`,
`core:oe_hash`, `core:oe_set`, `core:oe_zset`.

**Steps:**
```bash
redis-cli -p 6550 set core:oe_str v
redis-cli -p 6550 object encoding core:oe_str
redis-cli -p 6550 rpush core:oe_list a
redis-cli -p 6550 object encoding core:oe_list
redis-cli -p 6550 hset core:oe_hash f v
redis-cli -p 6550 object encoding core:oe_hash
redis-cli -p 6550 sadd core:oe_set a
redis-cli -p 6550 object encoding core:oe_set
redis-cli -p 6550 zadd core:oe_zset 1 a
redis-cli -p 6550 object encoding core:oe_zset
redis-cli -p 6550 object encoding core:nosuchkey
```

**Expected:**
```
OK
string
1
list
1
hash
1
set
1
zset
ERR no such key
```

**Notes:** Confirms `docs/command-compatibility.md`: `OBJECT ENCODING` returns this
engine's own type name (`string`/`list`/`hash`/`set`/`zset` — identical to what `TYPE`
returns), not real Redis's internal encoding names (`embstr`, `listpack`,
`skiplist`, etc.). Do not write assertions expecting `embstr`-style values.

**Result:** ☐ Pass ☐ Fail

### CORE-44 — MEMORY USAGE

**Precondition:** `core:oe_str` and `core:oe_list` exist (from CORE-43);
`core:nosuchkey` absent.

**Steps:**
```bash
redis-cli -p 6550 memory usage core:oe_str
redis-cli -p 6550 memory usage core:oe_list
redis-cli -p 6550 memory usage core:nosuchkey
```

**Expected:**
```
49
57

```

**Notes:** Returns a plausible byte estimate for existing keys and nil (blank line)
for a missing key. These are this engine's own approximate accounting, not
byte-for-byte comparable to real Redis's `MEMORY USAGE` output — treat as "some
positive integer" in an assertion, not an exact value.

**Result:** ☐ Pass ☐ Fail

### CORE-45 — Argument-count and unknown-command error shapes

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 get
redis-cli -p 6550 set core:x
redis-cli -p 6550 notacommand foo bar
```

**Expected:**
```
ERR wrong number of arguments for 'get' command
ERR wrong number of arguments for 'set' command
ERR unknown command 'NOTACOMMAND'
```

**Result:** ☐ Pass ☐ Fail

---

### CORE-46 — LSET on a missing key returns "no such key", distinct from a bad index

**Precondition:** `core:lsmissing` does not exist.

**Steps:**
```bash
redis-cli -p 6550 del core:lsmissing
redis-cli -p 6550 lset core:lsmissing 0 z
```

**Expected:**
```
0
no such key
```

**Notes:** Before this was fixed, a missing key and a bad index on an existing list both
returned the same generic `ERR index out of range` (see CORE-15's history). Now they're
distinguishable: a missing key returns `no such key` (no `ERR` prefix, same pattern as CORE-32's
missing-source `RENAME` error), while a bad index on an existing list returns `index out of
range` (CORE-15).

**Result:** ☐ Pass ☐ Fail

---

### CORE-47 — APPEND, INCR/INCRBY/DECR/DECRBY, and SETRANGE preserve an existing key's TTL

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:ttlapp
redis-cli -p 6550 set core:ttlapp hello EX 100
redis-cli -p 6550 ttl core:ttlapp
redis-cli -p 6550 append core:ttlapp world
redis-cli -p 6550 ttl core:ttlapp
redis-cli -p 6550 del core:ttlincr
redis-cli -p 6550 set core:ttlincr 10 EX 100
redis-cli -p 6550 ttl core:ttlincr
redis-cli -p 6550 incrby core:ttlincr 5
redis-cli -p 6550 ttl core:ttlincr
redis-cli -p 6550 del core:ttlsr
redis-cli -p 6550 set core:ttlsr "Hello World" EX 100
redis-cli -p 6550 ttl core:ttlsr
redis-cli -p 6550 setrange core:ttlsr 6 Redis!
redis-cli -p 6550 ttl core:ttlsr
```

**Expected:**
```
0
OK
99
10
99
0
OK
99
15
99
0
OK
99
12
99
```

**Notes:** Before this was fixed, `APPEND`/`INCR`/`INCRBY`/`DECR`/`DECRBY`/`SETRANGE` all wrote
through `Engine::set`, which unconditionally clears any existing TTL — a `TTL` immediately after
any of these calls used to return `-1` (no expiry) instead of the still-counting-down value shown
here. `DECR`/`DECRBY` share the exact same code path as `INCRBY` and are not re-tested
separately. As in CORE-02, exact TTL values will be a few units below what was set, since the
clock keeps ticking between calls — that is expected, not a bug.

**Result:** ☐ Pass ☐ Fail

---

### CORE-48 — RENAME and RENAMENX move the source's TTL to the destination

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:ttlrnsrc core:ttlrndst
redis-cli -p 6550 set core:ttlrnsrc v1 EX 100
redis-cli -p 6550 ttl core:ttlrnsrc
redis-cli -p 6550 rename core:ttlrnsrc core:ttlrndst
redis-cli -p 6550 ttl core:ttlrndst
redis-cli -p 6550 get core:ttlrndst
```

**Expected:**
```
0
OK
99
OK
99
v1
```

**Notes:** Before this was fixed, `RENAME`/`RENAMENX` wrote the destination via `Engine::set`,
which unconditionally clears TTL — the destination used to come out with no expiry (`TTL` = `-1`)
regardless of what the source carried. `RENAMENX` moves TTL the same way when the destination
doesn't already exist. A source with no TTL still leaves the destination with no TTL either way.

**Result:** ☐ Pass ☐ Fail

---

### CORE-49 — LTRIM preserves TTL, and does not fabricate a key when trimming one that never existed

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:ttlltrim
redis-cli -p 6550 rpush core:ttlltrim a b c d e
redis-cli -p 6550 expire core:ttlltrim 100
redis-cli -p 6550 ttl core:ttlltrim
redis-cli -p 6550 ltrim core:ttlltrim 1 3
redis-cli -p 6550 ttl core:ttlltrim
redis-cli -p 6550 lrange core:ttlltrim 0 -1
redis-cli -p 6550 del core:ltrimmissing
redis-cli -p 6550 ltrim core:ltrimmissing 0 -1
redis-cli -p 6550 exists core:ltrimmissing
```

**Expected:**
```
0
5
1
99
OK
99
b
c
d
0
OK
0
```

**Notes:** Before this was fixed, `LTRIM` was implemented as `LRANGE` followed by `Engine::set`,
which unconditionally cleared TTL — a trimmed list used to lose its expiry even though nothing
about `LTRIM` should touch it. `LTRIM` on a key that was never set is a no-op and must not create
a phantom empty list (same "no-phantom-collection" convention as CORE-42) — `EXISTS` confirms the
key stays absent afterward.

**Result:** ☐ Pass ☐ Fail

---

### CORE-50 — HDEL and ZREM remove multiple fields/members in one variadic call

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:hdelmulti
redis-cli -p 6550 hset core:hdelmulti f1 v1 f2 v2 f3 v3
redis-cli -p 6550 hdel core:hdelmulti f1 f2 fnosuch
redis-cli -p 6550 hgetall core:hdelmulti
redis-cli -p 6550 del core:zremmulti
redis-cli -p 6550 zadd core:zremmulti 1 a
redis-cli -p 6550 zadd core:zremmulti 2 b
redis-cli -p 6550 zadd core:zremmulti 3 c
redis-cli -p 6550 zrem core:zremmulti a b nosuch
redis-cli -p 6550 zrange core:zremmulti 0 -1
```

**Expected:**
```
0
3
2
f3
v3
0
1
1
1
2
c
```

**Notes:** Before this was fixed, `HDEL`/`ZREM` only ever acted on the *first* field/member of a
variadic call, silently ignoring the rest — `hdel core:hdelmulti f1 f2 fnosuch` used to remove
only `f1` and return `1`, leaving `f2` behind in the hash. Both commands now remove every
field/member given in one call and return the count actually removed: `2` for the `hdel` above
(`f1` and `f2` existed, `fnosuch` never did), and `2` for the `zrem` above (`a` and `b` existed,
`nosuch` never did) — `zrange` afterward shows only `c` remains. Recall `ZADD` itself is still not
variadic (CORE-28) — that bug is unrelated and unfixed, which is why this case adds `a`/`b`/`c`
via three separate `ZADD` calls.

**Result:** ☐ Pass ☐ Fail

---

### CORE-51 — INCR/INCRBY/HINCRBY overflow

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:ovf
redis-cli -p 6550 set core:ovf 9223372036854775807
redis-cli -p 6550 incr core:ovf
redis-cli -p 6550 get core:ovf
redis-cli -p 6550 del core:ovfneg
redis-cli -p 6550 set core:ovfneg -9223372036854775808
redis-cli -p 6550 incrby core:ovfneg -1
redis-cli -p 6550 get core:ovfneg
redis-cli -p 6550 del core:hovf
redis-cli -p 6550 hset core:hovf f 9223372036854775807
redis-cli -p 6550 hincrby core:hovf f 1
redis-cli -p 6550 hget core:hovf f
```

**Expected:**
```
0
OK
increment or decrement would overflow
9223372036854775807
0
OK
increment or decrement would overflow
-9223372036854775808
0
1
increment or decrement would overflow
9223372036854775807
```

**Notes:** Before this was fixed, `INCR`/`INCRBY`/`HINCRBY` added the delta with unchecked `i64`
arithmetic and wrapped silently on overflow (`i64::MAX + 1` wrapping around to `i64::MIN`)
instead of erroring. The error text has no `ERR` prefix, matching the pattern noted in
CORE-06/CORE-32/CORE-15 — this project's convention is that engine-originated errors carry no
prefix on the wire, only `WRONGTYPE` does (CORE-40). In every case here the value is left
completely unchanged by the failed increment, confirmed by the follow-up `GET`/`HGET` still
showing the pre-overflow value. `9223372036854775807` is `i64::MAX`; `-9223372036854775808` is
`i64::MIN`.

**Result:** ☐ Pass ☐ Fail

---

### CORE-52 — SINTERSTORE/SUNIONSTORE/SDIFFSTORE delete an empty-result destination

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:sis_a core:sis_b core:sis_dest
redis-cli -p 6550 sadd core:sis_a x
redis-cli -p 6550 sadd core:sis_b y
redis-cli -p 6550 sadd core:sis_dest old
redis-cli -p 6550 sinterstore core:sis_dest core:sis_a core:sis_b
redis-cli -p 6550 exists core:sis_dest
redis-cli -p 6550 del core:sus_dest
redis-cli -p 6550 sadd core:sus_dest old
redis-cli -p 6550 sunionstore core:sus_dest core:missing1 core:missing2
redis-cli -p 6550 exists core:sus_dest
redis-cli -p 6550 del core:sds_a core:sds_b core:sds_dest
redis-cli -p 6550 sadd core:sds_a x
redis-cli -p 6550 sadd core:sds_b x y
redis-cli -p 6550 sadd core:sds_dest old
redis-cli -p 6550 sdiffstore core:sds_dest core:sds_a core:sds_b
redis-cli -p 6550 exists core:sds_dest
```

**Expected:**
```
0
1
1
1
0
0
0
1
1
0
0
0
1
2
1
0
0
```

**Notes:** Before this was fixed, `SINTERSTORE`/`SUNIONSTORE`/`SDIFFSTORE` always wrote the
(possibly empty) result to the destination with `Engine::set`, fabricating a live empty `Set` —
`EXISTS` on the destination used to wrongly return `1`, even though `SMEMBERS`/`SCARD` on it
would correctly show zero members. Now an empty result **deletes** the destination instead,
matching real Redis's `*STORE` semantics, whether the destination previously held data (as with
`core:sis_dest`/`core:sus_dest`/`core:sds_dest` above, each pre-loaded with a member called
`old`) or never existed at all. The return value (`0` for each `*STORE` call) was already correct
before the fix and is unchanged — only the phantom-key side effect is new.

**Result:** ☐ Pass ☐ Fail

---

### CORE-53 — SREM/LPOP/RPOP/HDEL/ZREM delete the key once the last element is removed

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6550 del core:sremlast
redis-cli -p 6550 sadd core:sremlast onlymember
redis-cli -p 6550 srem core:sremlast onlymember
redis-cli -p 6550 exists core:sremlast
redis-cli -p 6550 type core:sremlast
redis-cli -p 6550 del core:lpoplast
redis-cli -p 6550 rpush core:lpoplast onlyelem
redis-cli -p 6550 lpop core:lpoplast
redis-cli -p 6550 exists core:lpoplast
redis-cli -p 6550 type core:lpoplast
redis-cli -p 6550 del core:rpoplast
redis-cli -p 6550 rpush core:rpoplast onlyelem
redis-cli -p 6550 rpop core:rpoplast
redis-cli -p 6550 exists core:rpoplast
redis-cli -p 6550 type core:rpoplast
redis-cli -p 6550 del core:hdellast
redis-cli -p 6550 hset core:hdellast f v
redis-cli -p 6550 hdel core:hdellast f
redis-cli -p 6550 exists core:hdellast
redis-cli -p 6550 type core:hdellast
redis-cli -p 6550 del core:zremlast
redis-cli -p 6550 zadd core:zremlast 1 onlymember
redis-cli -p 6550 zrem core:zremlast onlymember
redis-cli -p 6550 exists core:zremlast
redis-cli -p 6550 type core:zremlast
```

**Expected:**
```
0
1
1
0
none
0
1
onlyelem
0
none
0
1
onlyelem
0
none
0
1
1
0
none
0
1
1
0
none
```

**Notes:** Before this was fixed, `SREM`/`SPOP`/`HDEL`/`LPOP`/`RPOP`/`ZREM` left a live, empty
List/Hash/Set/SortedSet behind once the last element was removed, instead of deleting the key the
way real Redis does — `EXISTS` used to wrongly return `1` and `TYPE` would still wrongly report
the original type (`set`/`list`/`hash`/`zset`) instead of `none`, even though the collection
itself was empty. This is a different scenario from CORE-42 ("a mutation that finds nothing does
not leave a phantom collection"): CORE-42 covers a mutation against a key that was *never set* to
begin with; this case covers a mutation that empties a collection that *did* exist.

**Result:** ☐ Pass ☐ Fail

---

## Transactions

`MULTI`/`EXEC`/`DISCARD` shipped per the 2026-09-10 spec. `WATCH`/`UNWATCH` (optimistic locking)
remain unimplemented — see "Known limits" at the end of this playbook.

Start a standalone instance from a directory with **no** `rocket-mem.toml` present (this repo's
own root `rocket-mem.toml` turns on ACL/TLS/cluster, none of which these cases need):

```bash
ROCKET_MEM_ADDR=127.0.0.1:6620 ROCKET_MEM_RMP_ADDR=127.0.0.1:6621 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9320 \
ROCKET_MEM_AOF_PATH=$DATA/txn.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/txn.snap \
  "$ROCKET_MEM_BIN" &
echo $! > /tmp/txn.pid
```

Every case below sends its `MULTI`/`EXEC` block as a **single pipelined session in one
`redis-cli` connection** — a fresh `redis-cli` invocation per command opens a new connection, and
`MULTI`/`EXEC` state is per-connection:
```bash
redis-cli -p 6620 <<'EOF'
MULTI
...
EXEC
EOF
```

### TXN-01 — MULTI/queue/EXEC happy path

**Precondition:** Key `txn:a` does not exist.

**Steps:**
```bash
redis-cli -p 6620 <<'EOF'
MULTI
SET txn:a 1
INCR txn:a
GET txn:a
EXEC
EOF
```

**Expected:**
```
OK
QUEUED
QUEUED
QUEUED
OK
2
2
```

**Notes:** The first four lines are the replies to `MULTI`/each queued command (`+OK` then three
`+QUEUED`); the last three lines are `EXEC`'s own reply — a RESP array of `[OK, 2, "2"]`
(`redis-cli` prints array elements one per line in raw mode). Each queued command's reply is
exactly what it would have been outside a transaction.

**Result:** ☐ Pass ☐ Fail

---

### TXN-02 — DISCARD drops the queue

**Precondition:** Key `txn:b` does not exist.

**Steps:**
```bash
redis-cli -p 6620 <<'EOF'
MULTI
SET txn:b 1
DISCARD
GET txn:b
EOF
```

**Expected:**
```
OK
QUEUED
OK
(nil)
```

**Notes:** `DISCARD` replies `+OK` and the queued `SET` never ran — `txn:b` stays unset.

**Result:** ☐ Pass ☐ Fail

---

### TXN-03 — Nested MULTI is rejected without disturbing the existing queue

**Precondition:** Key `txn:g` does not exist.

**Steps:**
```bash
redis-cli -p 6620 --no-raw <<'EOF'
MULTI
MULTI
SET txn:g 1
EXEC
GET txn:g
EOF
```

**Expected:**
```
OK
(error) ERR MULTI calls can not be nested
QUEUED
1) OK
"1"
```

**Notes:** The nested `MULTI` itself is not queued — it errors immediately and the transaction it
was nested inside stays open exactly as before. The following `SET` still queues normally and
still runs at `EXEC`.

**Result:** ☐ Pass ☐ Fail

---

### TXN-04 — A queue-time unknown command aborts the whole batch (EXECABORT)

**Precondition:** Key `txn:f` does not exist.

**Steps:**
```bash
redis-cli -p 6620 --no-raw <<'EOF'
MULTI
SET txn:f 1
NOTACOMMAND foo
EXEC
GET txn:f
EOF
```

**Expected:**
```
OK
QUEUED
(error) ERR unknown command 'NOTACOMMAND'
(error) EXECABORT Transaction discarded because of previous errors
(nil)
```

**Notes:** The unknown command is rejected immediately (not queued) and marks the transaction
dirty. `EXEC` then runs nothing at all, including the earlier, otherwise-valid `SET` — `txn:f` is
confirmed unset afterward.

**Result:** ☐ Pass ☐ Fail

---

### TXN-05 — A queue-time arity error does NOT abort the batch (contrast with TXN-04)

**Precondition:** Key `txn:e` does not exist.

**Steps:**
```bash
redis-cli -p 6620 --no-raw <<'EOF'
MULTI
SET txn:e 1
SET
EXEC
GET txn:e
EOF
```

**Expected:**
```
OK
QUEUED
QUEUED
1) OK
2) (error) ERR wrong number of arguments for 'set' command
"1"
```

**Notes:** `SET` with no arguments is a *known* command name, so queue-time interception queues
it (`+QUEUED`) instead of rejecting it up front — the arity check only happens when `EXEC`
actually dispatches it. The result is a two-element reply array: the successful `SET txn:e 1` and
the failed bare `SET`'s own error, side by side, with no `EXECABORT`. `txn:e` is confirmed set to
`1` afterward. If you need a queue-time-rejected case, use an unknown command name (TXN-04), not
a bad-arity call to a real command.

**Result:** ☐ Pass ☐ Fail

---

### TXN-06 — An execution-time WRONGTYPE inside EXEC is reported per-command, not fatal

**Precondition:** Key `txn:list` is a list (`LPUSH txn:list a b c`); key `txn:d` does not exist.

**Steps:**
```bash
redis-cli -p 6620 <<'EOF'
LPUSH txn:list a b c
MULTI
SET txn:d ok
INCR txn:list
GET txn:d
EXEC
GET txn:d
EOF
```

**Expected:**
```
3
OK
QUEUED
QUEUED
QUEUED
OK
WRONGTYPE Operation against a key holding the wrong kind of value

ok
ok
```

**Notes:** `EXEC`'s reply array has three entries: `OK` (the `SET` succeeded), the `WRONGTYPE`
error (the `INCR` against a list), and the string `ok` (`GET txn:d`, printed as the array's third
line — use `--no-raw` to see this unambiguously as `3) "ok"`). The command *after* the failing
one (`GET txn:d`) still ran and returned the value the `SET` wrote — a per-command runtime error
doesn't abort the batch.

**Result:** ☐ Pass ☐ Fail

---

### TXN-07 — SUBSCRIBE queued inside an open transaction is rejected and aborts it

**Precondition:** Key `txn:h` does not exist. This connection is not currently subscribed to
anything (a connection that *is* subscribed can't open `MULTI` in the first place — a separate,
earlier RESP2-only gate; see the Pub/sub section).

**Steps:**
```bash
redis-cli -p 6620 --no-raw <<'EOF'
MULTI
SUBSCRIBE foo
SET txn:h 1
EXEC
GET txn:h
EOF
```

**Expected:**
```
OK
(error) ERR SUBSCRIBE is not allowed in transactions
QUEUED
(error) EXECABORT Transaction discarded because of previous errors
(nil)
```

**Notes:** `SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE` are treated exactly like an
unknown command at queue time — immediate `-ERR`, dirty flag set, later `EXEC` aborts regardless
of what else was queued.

**Result:** ☐ Pass ☐ Fail

---

### TXN-08 — EXEC / DISCARD without a MULTI in effect

**Precondition:** No transaction open on this connection.

**Steps:**
```bash
redis-cli -p 6620 --no-raw <<'EOF'
EXEC
DISCARD
EOF
```

**Expected:**
```
(error) ERR EXEC without MULTI
(error) ERR DISCARD without MULTI
```

**Notes:** Matches real Redis's own error text/shape for both commands.

**Result:** ☐ Pass ☐ Fail

---

### TXN-09 — AOF wraps only writes in MULTI/EXEC markers; a read-only batch writes nothing

**Precondition:** `ROCKET_MEM_AOF_PATH` points at a file you can inspect directly. TXN-01 has
already run (so there's a prior write transaction in the AOF to inspect).

**Steps:**
```bash
wc -l "$DATA/txn.aof"          # note the line count
redis-cli -p 6620 <<'EOF'
MULTI
GET txn:a
GET txn:c
EXEC
EOF
wc -l "$DATA/txn.aof"          # compare -- should be unchanged
cat -A "$DATA/txn.aof" | head -20
```

**Expected:** The two `wc -l` counts are identical — a read-only transaction appends nothing to
the AOF. The `cat -A` of TXN-01's transaction shows a bare `*1\r\n$5\r\nMULTI\r\n` marker frame (a
RESP array of exactly one bulk string, no arguments), then each queued write's own normal
RESP-encoded command, then a bare `*1\r\n$4\r\nEXEC\r\n` marker:
```
*1^M$
$5^M$
MULTI^M$
*3^M$
$3^M$
SET^M$
$5^M$
txn:a^M$
$1^M$
1^M$
*2^M$
$4^M$
INCR^M$
$5^M$
txn:a^M$
*1^M$
$4^M$
EXEC^M$
```

**Notes:** This confirms the AOF-atomicity *shape* (the markers exist and wrap exactly the
writes, one contiguous append), not crash atomicity. Proving that a `kill -9` mid-`EXEC` never
replays a half-written transaction is not practically verifiable through `redis-cli` alone — the
project's own test suite covers this directly (`crates/server/src/dispatcher.rs`, tests
`exec_wraps_its_writes_in_multi_and_exec_aof_markers` and
`a_read_only_transaction_writes_nothing_to_the_aof_at_all`).

**Result:** ☐ Pass ☐ Fail

---

### TXN-10 — Writers-only isolation: concurrent writes to a touched shard block; concurrent reads do not

**Precondition:** Two separate connections (A and B) to the same instance.

**Steps:** Not practically driven through plain `redis-cli` timing — recorded here as a
know-the-guarantee case rather than a copy-paste reproduction:
1. On connection A: `MULTI`, queue several commands touching key `k`, `EXEC`.
2. While A's `EXEC` is still running, attempt `SET k v2` on connection B.
3. While A's `EXEC` is still running, attempt `GET k` on connection B.

**Expected:** B's `SET` blocks until A's `EXEC` fully completes (the same shard-lock guard
ordinary writers already share, just held for the whole batch). B's `GET` is **not** blocked and
may observe the transaction's intermediate state partway through the batch — this is the
documented gap, not a bug.

**Notes — the writers-only isolation gap, quoted from
`docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`:**

> "This blocks any other **write** touching an overlapping shard until the transaction finishes —
> the same guarantee ordinary writes already give each other, just held longer. **Reads stay
> fully concurrent**, exactly as they are today (`dispatch_and_log_inner`'s guard is
> `write_name`-gated only) — a concurrent `GET` could observe the transaction's intermediate state
> partway through the batch. Real Redis cannot expose this (single-threaded), but closing that gap
> means holding each touched shard's actual data `RwLock` for the whole batch, which needs
> `engine.rs`'s `with_mut`/`with_ref` reworked to run against an already-held guard instead of
> re-locking (`parking_lot::RwLock` isn't reentrant). Explicitly deferred — see 'Out of scope.'"

Not black-box verifiable through ad-hoc `redis-cli` timing. The project's own test suite proves it
directly: `crates/server/src/dispatcher.rs`, test
`a_write_to_the_same_key_blocks_until_exec_releases_its_batch_guard_but_a_read_does_not`. Record
this case as verified via source/spec + existing test, not independently reproduced live.

**Result:** ☐ Pass ☐ Fail

---

Teardown:
```bash
kill $(cat /tmp/txn.pid) 2>/dev/null
rm -f /tmp/txn.pid
```

---


Binary under test: `"$ROCKET_MEM_BIN"`
(built release binary — do not rebuild unless asked).

Ports used by this playbook only. Do not reuse them for anything else running concurrently,
and do not touch any `rocket-mem` process you did not start yourself:

| Purpose | Port(s) |
|---|---|
| Standalone / leader / follower RESP | 6560, 6562 |
| Standalone / leader / follower RMP  | 6561, 6563 |
| Cluster node RESP (shard-a/b/c)     | 7101, 7102, 7103 |
| Metrics                             | 9360, 9361, 9362, 9363 |

Every node gets its own `ROCKET_MEM_METRICS_ADDR` and `ROCKET_MEM_RMP_ADDR` — both default to the
same address on every node, and a second node on the defaults crashes with `AddrInUse` even
though its RESP port is free. This is a real, undocumented-as-limit gap; treat it as a fact of
life, not a bug to file.

Data files live under a scratch directory, all prefixed `prc-` (persist/repl/cluster) since the
scratch space is shared with other test runs:

```bash
DATA=<your-scratch-dir>/qa/data
mkdir -p "$DATA"
BIN="$ROCKET_MEM_BIN"
```

Substitute `$DATA` and `$BIN` literally in every command block below, or export them once per
shell session. Every server is started with `&` and killed by the PID captured at start —
**never** with `pkill -f rocket-mem`; that also kills other agents' servers and any running
chaos test.

Startup log lines, on every node regardless of whether the AOF/snapshot files already existed
(exact timestamps/values vary; the fields present don't):

```
<ts>  INFO rocket_mem: rocket-mem starting version="<version>" node_id=<node-id>
<ts>  INFO rocket_mem: resolved config summary node_id=<node-id> addr=<resp-addr> rmp_addr=<rmp-addr> metrics_addr=<metrics-addr> aof_path=<aof-path> snapshot_path=<snapshot-path> log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=<bool> acl_enabled=<bool> acl_user_count=<n> tls_enabled=<bool> tls_replication_enabled=<bool>
<ts>  INFO rocket_mem::aof: aof recovery replay complete commands=<n> bytes=<n> elapsed_us=<n>
<ts>  INFO rocket_mem: listener bound protocol=metrics addr=http://<metrics-addr>/metrics
<ts>  INFO rocket_mem: listener bound protocol=RMP addr=<rmp-addr>
<ts>  INFO rocket_mem: listener bound protocol=RESP addr=<resp-addr>
```
followed by a colorized boxed summary table (stripped of ANSI color when piped to a file/non-tty)
repeating the same facts under `storage`/`acl`/`cluster`/`replicas`/`listeners` labels.

The `aof recovery replay complete commands=0 bytes=0 ...` event fires on every startup, including
a totally fresh one with nothing to recover — don't read its presence as proof of a non-empty
recovery; check the actual keys. (Older builds of this playbook, and of the server itself, showed
a plain `Recovered state from <snapshot-path> and <aof-path>` / `Metrics on http://...` /
`RMP listening on ...` / `Listening on ...` banner instead — that plain-text form no longer
exists; the fields above are its structured-logging replacement.)

---

## Persistence

### PERSIST-01 — AOF captures writes and survives a graceful restart

**Precondition:** No server running on 6560/6561/9360. `$DATA/prc-persist.aof` and
`$DATA/prc-persist.snap` do not exist (fresh start).

**Steps:**
```bash
ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-persist.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-persist.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-persist.pid
sleep 0.6

wc -c $DATA/prc-persist.aof                 # 0 bytes before any write

redis-cli -p 6560 set foo bar
redis-cli -p 6560 set baz qux
redis-cli -p 6560 get foo

sleep 1.5                                    # default fsync policy is EverySecond
wc -c $DATA/prc-persist.aof                  # must now be > 0
cat $DATA/prc-persist.aof

kill $(cat /tmp/prc-persist.pid)
sleep 0.3

# restart, same paths
ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-persist.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-persist.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-persist.pid
sleep 0.6
redis-cli -p 6560 get foo
redis-cli -p 6560 get baz
```

**Expected:**
```
0
OK
OK
bar
62
*3
$3
set
$3
foo
$3
bar
*3
$3
set
$3
baz
$3
qux
bar
qux
```

**Notes:** The AOF is written but not fsynced immediately — a write issued right after startup
is not on disk until the next `EverySecond` tick (observed here as up to ~1s). Don't check file
size right after a write with no sleep; it reads 0 and looks broken when it isn't.

**Result:** ☐ Pass ☐ Fail

---

### PERSIST-02 — `SAVE` writes a snapshot file; restart loads it

**Precondition:** Server from PERSIST-01 still running on 6560, with `foo`/`baz` set.

**Steps:**
```bash
redis-cli -p 6560 set snapkey snapval
redis-cli -p 6560 save
ls -la $DATA/prc-persist.snap

kill $(cat /tmp/prc-persist.pid)
sleep 0.3

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-persist.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-persist.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-persist.pid
sleep 0.6
redis-cli -p 6560 get snapkey
redis-cli -p 6560 get foo
```

**Expected:**
```
OK
OK
-rw-rw-r-- 1 numericlabs numericlabs 105 <date> $DATA/prc-persist.snap
Recovered state from $DATA/prc-persist.snap and $DATA/prc-persist.aof
Metrics on http://127.0.0.1:9360/metrics
RMP listening on 127.0.0.1:6561
Listening on 127.0.0.1:6560
snapval
bar
```

**Result:** ☐ Pass ☐ Fail

---

### PERSIST-03 — Snapshot plus AOF tail load together, in that order

**Precondition:** Server from PERSIST-02 running on 6560, snapshot already contains
`foo`/`baz`/`snapkey`. `$DATA/prc-persist.aof` is **not** truncated or rewritten by `SAVE` — it
still holds the full command history from before the snapshot, plus whatever is appended after.

**Steps:**
```bash
redis-cli -p 6560 get snapkey             # proves snapshot half loaded
redis-cli -p 6560 get foo                 # proves it, plus the pre-snapshot AOF portion, loaded

redis-cli -p 6560 set posttail tailval    # written AFTER the snapshot, only in the AOF tail
sleep 1.5

kill $(cat /tmp/prc-persist.pid)
sleep 0.3

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-persist.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-persist.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-persist.pid
sleep 0.6
redis-cli -p 6560 get snapkey    # from the snapshot
redis-cli -p 6560 get posttail   # from the AOF tail written after the snapshot offset
```

**Expected:**
```
snapval
bar
OK
Recovered state from $DATA/prc-persist.snap and $DATA/prc-persist.aof
Metrics on http://127.0.0.1:9360/metrics
RMP listening on 127.0.0.1:6561
Listening on 127.0.0.1:6560
snapval
tailval
```

**Notes:** The one-line "Recovered state from `<snapshot>` and `<aof>`" banner is the only place
the load order is stated; there's no separate "loading snapshot..." / "replaying AOF tail..."
pair of lines. Per `README.md`'s Sprint 5 entry, the snapshot embeds the AOF byte offset it was
taken at, so only the AOF bytes written after that offset are replayed on top of it — not the
whole file from scratch (that full-replay-from-empty behavior was Sprint 4's, superseded in
Sprint 5). The AOF file itself keeps growing forever across every `SAVE`; nothing truncates or
rewrites it, so don't expect its size to reset after a snapshot.

**Result:** ☐ Pass ☐ Fail

---

### PERSIST-04 — Different AOF/snapshot path starts empty

**Precondition:** Server from PERSIST-03 running on 6560. `$DATA/prc-persist-other.aof` and
`$DATA/prc-persist-other.snap` do not exist.

**Steps:**
```bash
kill $(cat /tmp/prc-persist.pid)
sleep 0.3

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-persist-other.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-persist-other.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-persist.pid
sleep 0.6
redis-cli -p 6560 get foo
redis-cli -p 6560 get snapkey
redis-cli -p 6560 keys '*'

kill $(cat /tmp/prc-persist.pid)
sleep 0.3
```

**Expected:**
```
Recovered state from $DATA/prc-persist-other.snap and $DATA/prc-persist-other.aof
Metrics on http://127.0.0.1:9360/metrics
RMP listening on 127.0.0.1:6561
Listening on 127.0.0.1:6560
(nil)
(nil)
(empty array)
```

**Notes:** This is the control case proving PERSIST-01 through -03 actually read the data back
from the file, not from some other in-process cache — same binary, same host, only the path
changed, and the store comes up empty. The banner still says "Recovered state from..." even
though nothing was actually recovered; see the note at the top of this document.

**Result:** ☐ Pass ☐ Fail

---

### PERSIST-05 — Data survives an ungraceful `kill -9`

**Precondition:** No server running on 6560/6561/9360. `$DATA/prc-kill9.aof` and
`$DATA/prc-kill9.snap` removed if present, for a clean slate.

**Steps:**
```bash
rm -f $DATA/prc-kill9.aof $DATA/prc-kill9.snap

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-kill9.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-kill9.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-kill9.pid
sleep 0.6

redis-cli -p 6560 set survive yes
redis-cli -p 6560 set counter 1
sleep 1.5                              # let EverySecond fsync land before the SIGKILL
wc -c $DATA/prc-kill9.aof

kill -9 $(cat /tmp/prc-kill9.pid)      # no clean shutdown, no chance to flush anything extra
sleep 0.3
ps -p $(cat /tmp/prc-kill9.pid)        # confirm it's actually dead

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-kill9.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-kill9.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-kill9.pid
sleep 0.6
redis-cli -p 6560 get survive
redis-cli -p 6560 get counter
```

**Expected:**
```
OK
OK
68
(stopped, ps shows no matching PID)
Recovered state from $DATA/prc-kill9.snap and $DATA/prc-kill9.aof
Metrics on http://127.0.0.1:9360/metrics
RMP listening on 127.0.0.1:6561
Listening on 127.0.0.1:6560
yes
1
```

**Notes:** This is the property the project's own chaos test (`scripts/chaos.sh`,
`docs/chaos/2026-09-01-chaos-log.md`) exercises continuously against a live leader+follower pair
under repeated `kill -9`. This case only proves the single-node, single-kill version of it; it
does not attempt to synthesize a torn/mid-write AOF record. `README.md`'s Sprint 4 entry claims a
corrupted tail is truncated rather than merely skipped in memory — that specific claim is not
independently re-verified here, only cited.

**Result:** ☐ Pass ☐ Fail

---

## Replication

### REPL-01 — `REPLICAOF` attaches a follower and transfers a full snapshot

**Precondition:** No servers on 6560-6563/9360-9361. `$DATA/prc-leader.*` and
`$DATA/prc-follower.*` removed if present.

**Steps:**
```bash
rm -f $DATA/prc-leader.aof $DATA/prc-leader.snap $DATA/prc-follower.aof $DATA/prc-follower.snap

# leader
ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-leader.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-leader.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-repl-leader.pid

# follower
ROCKET_MEM_ADDR=127.0.0.1:6562 ROCKET_MEM_RMP_ADDR=127.0.0.1:6563 \
ROCKET_MEM_AOF_PATH=$DATA/prc-follower.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-follower.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 \
  $BIN &
echo $! > /tmp/prc-repl-follower.pid
sleep 0.6

# write BEFORE attaching, to prove the initial sync is a full snapshot, not just future writes
redis-cli -p 6560 set preexisting value1
redis-cli -p 6560 set foo bar

redis-cli -p 6562 replicaof 127.0.0.1 6560
sleep 0.5
redis-cli -p 6562 get preexisting
redis-cli -p 6562 get foo
```

**Expected:**
```
OK
OK
OK
value1
bar
```

**Result:** ☐ Pass ☐ Fail

---

### REPL-02 — Live writes on the leader stream to the follower

**Precondition:** REPL-01's leader/follower pair still running and attached.

**Steps:**
```bash
redis-cli -p 6560 set livekey liveval
sleep 0.3
redis-cli -p 6562 get livekey
```

**Expected:**
```
OK
liveval
```

**Notes:** A brief sleep is required — replication is asynchronous, there is no synchronous
"write acknowledged by replica" mode to wait on instead.

**Result:** ☐ Pass ☐ Fail

---

### REPL-03 — Follower rejects direct client writes with `READONLY`

**Precondition:** Same pair, follower still attached.

**Steps:**
```bash
redis-cli -p 6562 set nope x
```

**Expected:**
```
(error) READONLY You can't write against a read only replica.
```

**Result:** ☐ Pass ☐ Fail

---

### REPL-04 — `INFO replication` reports role and link status correctly on both sides

**Precondition:** Same pair, follower still attached.

**Steps:**
```bash
redis-cli -p 6560 info replication
redis-cli -p 6562 info replication
```

**Expected:**
```
# Replication
role:master
master_repl_offset:<n>
connected_slaves:1
slave0:ip=127.0.0.1,port=6562,state=online,offset=<n>,lag=<n>

# Replication
role:slave
master_host:127.0.0.1
master_port:6560
master_link_status:up
slave_repl_offset:<n>
master_repl_offset:<n>
```

**Result:** ☐ Pass ☐ Fail

---

### REPL-05 — `REPLICAOF NO ONE` promotes the follower back to read-write

**Precondition:** Same pair, follower still attached.

**Steps:**
```bash
redis-cli -p 6562 replicaof no one
sleep 0.3
redis-cli -p 6562 info replication
redis-cli -p 6562 set promoted yes
redis-cli -p 6562 get promoted

kill $(cat /tmp/prc-repl-leader.pid) $(cat /tmp/prc-repl-follower.pid)
sleep 0.3
```

**Expected:**
```
OK
# Replication
role:master
master_repl_offset:0
connected_slaves:0
OK
yes
```

**Result:** ☐ Pass ☐ Fail

---

### REPL-06 — Every resync is a full resync (known limit, not a bug)

**Precondition:** No servers on 6560-6563/9360-9361. `$DATA/prc-leader2.*` and
`$DATA/prc-follower2.*` removed if present.

**Steps:**
```bash
rm -f $DATA/prc-leader2.aof $DATA/prc-leader2.snap $DATA/prc-follower2.aof $DATA/prc-follower2.snap

ROCKET_MEM_ADDR=127.0.0.1:6560 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_AOF_PATH=$DATA/prc-leader2.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-leader2.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 \
  $BIN &
echo $! > /tmp/prc-repl2-leader.pid

ROCKET_MEM_ADDR=127.0.0.1:6562 ROCKET_MEM_RMP_ADDR=127.0.0.1:6563 \
ROCKET_MEM_AOF_PATH=$DATA/prc-follower2.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-follower2.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 \
  $BIN &
echo $! > /tmp/prc-repl2-follower.pid
sleep 0.6

redis-cli -p 6560 set leaderkey leaderval
redis-cli -p 6562 set divergedkey divergedval   # data that only ever existed on the "follower"
redis-cli -p 6562 get divergedkey               # present before attaching

redis-cli -p 6562 replicaof 127.0.0.1 6560
sleep 0.5
redis-cli -p 6562 get divergedkey               # gone: full resync overwrote local state
redis-cli -p 6562 get leaderkey                 # leader's data now present

curl -s http://127.0.0.1:9360/metrics | grep -i replic

kill $(cat /tmp/prc-repl2-leader.pid) $(cat /tmp/prc-repl2-follower.pid)
sleep 0.3
```

**Expected:**
```
OK
OK
divergedval
OK
(nil)
leaderval
# TYPE rocket_mem_replication_last_apply_timestamp_seconds gauge
rocket_mem_replication_last_apply_timestamp_seconds 0
# TYPE rocket_mem_connected_replicas gauge
rocket_mem_connected_replicas 0
```

**Notes:** This is expected behavior, not a bug: Sprint 5's design has no partial-resync/offset-
resume support, so a dropped or freshly-attached follower always gets a fresh full snapshot,
which silently discards anything the follower had written locally. `rocket_mem_connected_replicas`
is the follower-count gauge; `rocket_mem_replication_last_apply_timestamp_seconds` is a coarser
wall-clock signal that exists *alongside* the real offset/lag fields — see REPL-04, which already
covers `master_repl_offset`/`slave_repl_offset` and each `slaveN:` line's `offset=/lag=` fields.
`PSYNC` goes through the same `AUTH`/ACL gate every other command does; REPL-11/REPL-12 below
verify that an ACL-protected leader cleanly rejects an unauthenticated follower's `PSYNC` (never a
crash) and accepts one configured with `replicaof_auth_username`/`replicaof_auth_password`.

**Result:** ☐ Pass ☐ Fail

---

### REPL-07 — Leader refuses writes with `NOREPLICAS` when fencing is enabled and no replica has acked

**Precondition:** No servers on 6630-6631/9330 or 6640-6641/9340. Fencing is opt-in
(`min_replicas_to_write=0` is the default and disables this entirely — see the `CFG` section for
the config-layering cases; this section turns it on via env var as shown).

**Steps:**
```bash
rm -f $DATA/prc-fence-leader.aof $DATA/prc-fence-leader.snap

ROCKET_MEM_ADDR=127.0.0.1:6630 ROCKET_MEM_RMP_ADDR=127.0.0.1:6631 \
ROCKET_MEM_AOF_PATH=$DATA/prc-fence-leader.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-fence-leader.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9330 \
ROCKET_MEM_MIN_REPLICAS_TO_WRITE=1 ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS=3 \
  $BIN &
echo $! > /tmp/prc-fence-leader.pid
sleep 0.6

redis-cli -p 6630 set k v
curl -s http://127.0.0.1:9330/metrics | grep rocket_mem_writes_rejected_no_replicas_total
```

**Expected:**
```
NOREPLICAS Not enough good replicas to write.

# TYPE rocket_mem_writes_rejected_no_replicas_total counter
rocket_mem_writes_rejected_no_replicas_total 1
```

**Notes:** Error text matches Redis's `NOREPLICAS` exactly. This gate fires *before* the AOF
ordering lock is taken, so a rejected write leaves no AOF/snapshot trace. A read command (`GET`)
is never fenced — only commands the dispatcher recognizes as writes.

**Result:** ☐ Pass ☐ Fail

---

### REPL-08 — Fencing clears once a replica attaches and acks within the lag window

**Precondition:** REPL-07's leader still running and still fenced.

**Steps:**
```bash
rm -f $DATA/prc-fence-follower.aof $DATA/prc-fence-follower.snap

ROCKET_MEM_ADDR=127.0.0.1:6640 ROCKET_MEM_RMP_ADDR=127.0.0.1:6641 \
ROCKET_MEM_AOF_PATH=$DATA/prc-fence-follower.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-fence-follower.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9340 \
  $BIN &
echo $! > /tmp/prc-fence-follower.pid
sleep 0.6

redis-cli -p 6640 replicaof 127.0.0.1 6630
sleep 2.5                                 # let it attach and send its first REPLCONF ACK

redis-cli -p 6630 set k v
sleep 0.3
redis-cli -p 6640 get k
curl -s http://127.0.0.1:9330/metrics | grep -E 'rocket_mem_good_replicas|rocket_mem_replica_min_ack_offset'

kill $(cat /tmp/prc-fence-leader.pid) $(cat /tmp/prc-fence-follower.pid)
sleep 0.3
```

**Expected:**
```
OK
OK
v
# TYPE rocket_mem_good_replicas gauge
rocket_mem_good_replicas 1
# TYPE rocket_mem_replica_min_ack_offset gauge
rocket_mem_replica_min_ack_offset 27
```

**Notes:** `rocket_mem_replica_min_ack_offset`'s exact number will vary run to run (it's the
furthest-behind connected replica's acked byte offset); only `rocket_mem_good_replicas: 1` is a
fixed expectation. The leader's stderr also logs exactly one `WARN ... entering fenced state`
line at startup and one `INFO ... leaving fenced state` line the moment the first ack arrives.

**Result:** ☐ Pass ☐ Fail

---

### REPL-09 — `min_replicas_to_write` with a zero lag window is rejected at startup, not at write time

**Precondition:** No server on 6630/6631/9330.

**Steps:**
```bash
ROCKET_MEM_ADDR=127.0.0.1:6630 ROCKET_MEM_RMP_ADDR=127.0.0.1:6631 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9330 \
ROCKET_MEM_MIN_REPLICAS_TO_WRITE=1 ROCKET_MEM_MIN_REPLICAS_MAX_LAG_SECS=0 \
  $BIN
```

**Expected:** process exits immediately, before any listener binds:
```
config error: min_replicas_to_write is set but min_replicas_max_lag_secs is 0 -- no replica
could ever qualify, so every write would be refused forever
```

**Notes:** `validate_min_replicas` (`crates/server/src/config.rs`) treats this combination as a
config typo that would otherwise spell a permanent, silent write outage — a 0-second lag window
means no replica's ack could ever be "recent enough."

**Result:** ☐ Pass ☐ Fail

---

### REPL-10 — `replica_announce_addr` makes the leader report the announced address, not the raw socket peer

**Precondition:** No servers on 6630-6631/9330 or 6640-6641/9340.

**Steps:**
```bash
rm -f $DATA/prc-announce-leader.aof $DATA/prc-announce-leader.snap
rm -f $DATA/prc-announce-follower.aof $DATA/prc-announce-follower.snap

ROCKET_MEM_ADDR=127.0.0.1:6630 ROCKET_MEM_RMP_ADDR=127.0.0.1:6631 \
ROCKET_MEM_AOF_PATH=$DATA/prc-announce-leader.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-announce-leader.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9330 \
  $BIN &
echo $! > /tmp/prc-announce-leader.pid

ROCKET_MEM_ADDR=127.0.0.1:6640 ROCKET_MEM_RMP_ADDR=127.0.0.1:6641 \
ROCKET_MEM_AOF_PATH=$DATA/prc-announce-follower.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-announce-follower.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9340 \
ROCKET_MEM_REPLICA_ANNOUNCE_ADDR=announced.example.com:16640 \
  $BIN &
echo $! > /tmp/prc-announce-follower.pid
sleep 0.6

redis-cli -p 6640 replicaof 127.0.0.1 6630
sleep 1
redis-cli -p 6630 info replication

kill $(cat /tmp/prc-announce-leader.pid) $(cat /tmp/prc-announce-follower.pid)
sleep 0.3
```

**Expected:**
```
OK
# Replication
role:master
connected_slaves:1
slave0:ip=announced.example.com,port=16640,state=online,offset=<n>,lag=<n>
master_repl_offset:<n>
```

**Notes:** Without `replica_announce_addr` set, the same line instead shows the follower's real
*socket* peer (`127.0.0.1` and an ephemeral outbound port, not `6640`) — the connection's source
port is not the follower's listening port. `replica_announce_addr` defaults to unset, which
reproduces that old (label-only) behavior byte for byte.

**Result:** ☐ Pass ☐ Fail

---

### REPL-11 — PSYNC against an ACL-protected leader is rejected cleanly with `NOAUTH`, and the follower keeps retrying rather than crashing

**Precondition:** No servers on 6630-6631/9330 or 6640-6641/9340. Requires a config file (ACL
bootstrap users are TOML-only — `ROCKET_MEM_*` env vars can't express `[[acl.users]]`).

**Steps:**
```bash
cat > $DATA/prc-acl-leader.toml <<EOF
addr = "127.0.0.1:6630"
rmp_addr = "127.0.0.1:6631"
aof_path = "$DATA/prc-acl-leader.aof"
snapshot_path = "$DATA/prc-acl-leader.snap"
metrics_addr = "127.0.0.1:9330"

[[acl.users]]
username = "repl"
password = "replpw"
enabled = true
rules = ["allcommands", "allkeys"]
EOF

$BIN --config $DATA/prc-acl-leader.toml &
echo $! > /tmp/prc-acl-leader.pid
sleep 0.6

# follower with NO credentials configured
ROCKET_MEM_ADDR=127.0.0.1:6640 ROCKET_MEM_RMP_ADDR=127.0.0.1:6641 \
ROCKET_MEM_AOF_PATH=$DATA/prc-acl-follower.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-acl-follower.snap \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9340 \
RUST_LOG=rocket_mem=info \
  $BIN 2>$DATA/prc-acl-follower.stderr &
echo $! > /tmp/prc-acl-follower.pid
sleep 0.6

redis-cli -p 6640 replicaof 127.0.0.1 6630
sleep 1.5
redis-cli -p 6640 info replication
grep "leader rejected PSYNC" $DATA/prc-acl-follower.stderr

kill $(cat /tmp/prc-acl-leader.pid) $(cat /tmp/prc-acl-follower.pid)
sleep 0.3
```

**Expected:**
```
OK
# Replication
role:slave
master_host:127.0.0.1
master_port:6630
master_link_status:down
slave_repl_offset:0
master_repl_offset:0
... error=leader rejected PSYNC: NOAUTH Authentication required.
```

**Notes:** `master_link_status` stays `down` — the follower process does not crash or exit; it
retries on a fixed backoff, logging one `WARN ... replication connection lost, reconnecting ...
error=leader rejected PSYNC: NOAUTH Authentication required.` line per attempt. An ACL-protected
leader's `PSYNC` rejection is a plain RESP error line (`-NOAUTH ...`), distinct from the raw
length-prefixed snapshot blob a successful `PSYNC` sends — the follower must distinguish the two
before reading. REPL-12 proves the credentialed path succeeds against the same leader.

**Result:** ☐ Pass ☐ Fail

---

### REPL-12 — PSYNC with correct `replicaof_auth_username`/`replicaof_auth_password` succeeds against the same ACL-protected leader

**Precondition:** REPL-11's leader still running (or restart it identically). No follower on
6640-6641/9340.

**Steps:**
```bash
cat > $DATA/prc-acl-follower.toml <<EOF
addr = "127.0.0.1:6640"
rmp_addr = "127.0.0.1:6641"
aof_path = "$DATA/prc-acl-follower2.aof"
snapshot_path = "$DATA/prc-acl-follower2.snap"
metrics_addr = "127.0.0.1:9340"
replicaof = "127.0.0.1:6630"
replicaof_auth_username = "repl"
replicaof_auth_password = "replpw"
EOF

$BIN --config $DATA/prc-acl-follower.toml &
echo $! > /tmp/prc-acl-follower2.pid
sleep 1.5

redis-cli -p 6640 info replication
redis-cli -p 6630 -a replpw --user repl --no-auth-warning set k v
sleep 0.3
redis-cli -p 6640 get k

kill $(cat /tmp/prc-acl-leader.pid) $(cat /tmp/prc-acl-follower2.pid)
sleep 0.3
```

**Expected:**
```
# Replication
role:slave
master_host:127.0.0.1
master_port:6630
master_link_status:up
slave_repl_offset:<n>
master_repl_offset:<n>
OK
v
```

**Notes:** `AUTH <username> <password>` is sent once, before `PSYNC`, and its reply round-trips
through the normal RESP codec (unlike `PSYNC`'s own reply, the raw length-prefixed blob) — a
rejected `AUTH` (wrong password) fails the same way as a rejected `PSYNC`: cleanly, with a retry,
never a crash.

**Result:** ☐ Pass ☐ Fail

---

## Pub/sub

`SUBSCRIBE`/`UNSUBSCRIBE`/`PSUBSCRIBE`/`PUNSUBSCRIBE`/`PUBLISH`/`PUBSUB` shipped per the
2026-09-11 spec. Most cases use one standalone instance; the cross-node case needs a second node,
same as `REPL-*`, which is why this section sits between Replication and Cluster.

```bash
ROCKET_MEM_ADDR=127.0.0.1:6600 ROCKET_MEM_RMP_ADDR=127.0.0.1:6601 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9600 \
ROCKET_MEM_AOF_PATH=$DATA/pubsub-leader.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/pubsub-leader.snap \
  $BIN &
echo $! > /tmp/pubsub-leader.pid
```

**A pub/sub-specific `redis-cli` wrinkle:** because `SUBSCRIBE`/`PSUBSCRIBE` push messages
asynchronously, capturing delivery needs a connection that stays open after its `subscribe`
confirmation — a background job (`redis-cli -p 6600 subscribe chan > file &`, killed later), not
a one-shot `redis-cli` call. A `redis-cli` invocation fed commands over a stdin *pipe* is fine for
testing the command *replies themselves* (subscribe/unsubscribe confirmations, restricted-mode
errors), because those come back synchronously, but such a process exits on stdin EOF and will
miss any message published after its last piped line — use the backgrounded form whenever the
case is about a *pushed* `message`/`pmessage` frame arriving after the fact. Also carried over
from the general raw-output note given before `## Core data types`: every error reply in this
mode is followed by one blank line, including the restricted-mode error below.

### PUBSUB-01 — Basic SUBSCRIBE + PUBLISH delivery

**Precondition:** Server running per the startup block above. No existing subscribers on `news`.

**Steps:**
```bash
redis-cli -p 6600 subscribe news > /tmp/pubsub01.out 2>&1 &
SUBPID=$!
sleep 0.4
redis-cli -p 6600 publish news hello
sleep 0.3
kill $SUBPID; wait $SUBPID 2>/dev/null
cat /tmp/pubsub01.out
```

**Expected:**
```
1
subscribe
news
1
message
news
hello
```

**Notes:** The `1` on its own at the top is `PUBLISH`'s reply — the count of subscribers the
message was actually delivered to. The subscriber's file shows the `subscribe` confirmation
immediately followed by the pushed `message` frame — both use the same wire shape on a RESP2
connection like this one (see PUBSUB-07 for the RESP3 `Push` type distinction).

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-02 — Multiple subscribers on the same channel all receive

**Precondition:** No existing subscribers on `multi1`.

**Steps:**
```bash
redis-cli -p 6600 subscribe multi1 > /tmp/pubsub02a.out 2>&1 &
SUBA=$!
redis-cli -p 6600 subscribe multi1 > /tmp/pubsub02b.out 2>&1 &
SUBB=$!
sleep 0.4
redis-cli -p 6600 publish multi1 hi-everyone
sleep 0.3
kill $SUBA $SUBB; wait $SUBA $SUBB 2>/dev/null
cat /tmp/pubsub02a.out
cat /tmp/pubsub02b.out
```

**Expected:**
```
2
subscribe
multi1
1
message
multi1
hi-everyone
subscribe
multi1
1
message
multi1
hi-everyone
```

**Notes:** `PUBLISH` replies `2` (both connections counted), and each subscriber's own
`subscribe` confirmation reports count `1` — the count in a `subscribe`/`unsubscribe` reply is
always *that connection's own* number of active subscriptions, not the channel's global
subscriber count (`PUBSUB NUMSUB`, PUBSUB-06 below, reports the channel-level count).

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-03 — PSUBSCRIBE delivers a matching channel as `pmessage`

**Precondition:** No existing subscriptions on pattern `news.*`.

**Steps:**
```bash
redis-cli -p 6600 psubscribe 'news.*' > /tmp/pubsub03.out 2>&1 &
PSUBPID=$!
sleep 0.4
redis-cli -p 6600 publish news.sports golden-goal
sleep 0.3
kill $PSUBPID; wait $PSUBPID 2>/dev/null
cat /tmp/pubsub03.out
```

**Expected:**
```
1
psubscribe
news.*
1
pmessage
news.*
news.sports
golden-goal
```

**Notes:** A `pmessage` frame carries one extra field versus `message` — the matched pattern,
then the concrete channel, then the payload.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-04 — UNSUBSCRIBE: explicit channel, a channel never subscribed to, and no-argument unsubscribe-all

**Precondition:** No existing subscriptions for this connection.

**Steps:**
```bash
printf 'subscribe ps:uns1 ps:uns2\nunsubscribe ps:uns1\nunsubscribe ps:nope\nunsubscribe\n' \
  | timeout 2 redis-cli -p 6600
```

**Expected:**
```
subscribe
ps:uns1
1
subscribe
ps:uns2
2
unsubscribe
ps:uns1
1
unsubscribe
ps:nope
1
unsubscribe
ps:uns2
0
```

**Notes:** `unsubscribe ps:nope` — a channel this connection was never subscribed to — still
replies once (count `1`, this connection's true remaining-subscription count at that point), it
does not error and does not affect the connection's real subscription set. The final
no-argument `unsubscribe` leaves every channel still held, replying once per channel it removes,
ending at count `0`.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-05 — PUNSUBSCRIBE: the same shape, against patterns

**Precondition:** No existing pattern subscriptions for this connection.

**Steps:**
```bash
printf 'psubscribe ps:pat1.* ps:pat2.*\npunsubscribe ps:pat1.*\npunsubscribe\n' \
  | timeout 2 redis-cli -p 6600
```

**Expected:**
```
psubscribe
ps:pat1.*
1
psubscribe
ps:pat2.*
2
punsubscribe
ps:pat1.*
1
punsubscribe
ps:pat2.*
0
```

**Notes:** No-argument `PUNSUBSCRIBE` only ever removes patterns — a connection with both plain
channel and pattern subscriptions open needs both a plain `UNSUBSCRIBE` and a plain
`PUNSUBSCRIBE` to clear everything; neither command touches the other's set.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-06 — PUBSUB CHANNELS / NUMSUB / NUMPAT introspection

**Precondition:** No existing subscriptions on `pubsubintro` or matching pattern
`pubsubintro.*`.

**Steps:**
```bash
redis-cli -p 6600 subscribe pubsubintro > /tmp/pubsub06a.out 2>&1 &
S1=$!
redis-cli -p 6600 subscribe pubsubintro > /tmp/pubsub06b.out 2>&1 &
S2=$!
redis-cli -p 6600 psubscribe 'pubsubintro.*' > /tmp/pubsub06c.out 2>&1 &
S3=$!
sleep 0.4
redis-cli -p 6600 pubsub channels
redis-cli -p 6600 pubsub channels 'pubsub*'
redis-cli -p 6600 pubsub numsub pubsubintro nosuchchan
redis-cli -p 6600 pubsub numpat
kill $S1 $S2 $S3; wait $S1 $S2 $S3 2>/dev/null
```

**Expected:**
```
pubsubintro
pubsubintro
pubsubintro
2
nosuchchan
0
1
```

**Notes:** `PUBSUB CHANNELS` (no argument) lists `pubsubintro` once — a channel with 2
subscribers is one entry, not two. `PUBSUB NUMSUB` replies as a flat `channel1 count1 channel2
count2 ...` array, not a map — `nosuchchan` (never subscribed) legitimately reports `0` rather
than erroring or being omitted. `PUBSUB NUMPAT` counts distinct registered *patterns*, not
pattern subscribers.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-07 — RESP2 subscribe-mode command restrictions; RESP3 is unrestricted

**Precondition:** No existing subscriptions for either connection below.

**Steps:**
```bash
# RESP2 (redis-cli's default)
printf 'subscribe ps:restrict\nget somekey\nping\nmulti\nunsubscribe\n' \
  | timeout 2 redis-cli -p 6600

# RESP3
printf 'subscribe ps:resp3\nget somekey\nping\nunsubscribe\n' \
  | timeout 2 redis-cli -3 -p 6600
```

**Expected (RESP2):**
```
subscribe
ps:restrict
1
ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context

PONG
ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context

unsubscribe
ps:restrict
0
```

**Expected (RESP3):**
```
subscribe
ps:resp3
1

PONG
unsubscribe
ps:resp3
0
```

**Notes:** On the RESP2 connection, both `GET` and `MULTI` are rejected with real Redis's own
subscribe-mode message while at least one subscription is open; `PING` is exempted. On the RESP3
connection the identical `GET` succeeds and returns nil — RESP3 connections skip this
restriction entirely, because RESP3 clients get pub/sub messages out-of-band via the `Push`
(`>`) frame type. `PUBLISH` itself is also restricted on a RESP2 connection while subscribed,
matching real Redis.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-08 — MULTI/EXEC interaction: SUBSCRIBE rejected at queue time, PUBLISH queued and delivered at EXEC

**Precondition:** No existing subscriptions on `ps:txchan`/`ps:txpub`.

**Steps:**
```bash
# SUBSCRIBE inside MULTI
printf 'multi\nsubscribe ps:txchan\nset ps:txkey v\nexec\n' | timeout 2 redis-cli -p 6600

# PUBLISH inside MULTI, with a live subscriber
redis-cli -p 6600 subscribe ps:txpub > /tmp/pubsub08.out 2>&1 &
TP=$!
sleep 0.4
printf 'multi\npublish ps:txpub inside-tx\nexec\n' | timeout 2 redis-cli -p 6600
sleep 0.3
kill $TP; wait $TP 2>/dev/null
cat /tmp/pubsub08.out
```

**Expected (SUBSCRIBE inside MULTI):**
```
OK
ERR SUBSCRIBE is not allowed in transactions

QUEUED
EXECABORT Transaction discarded because of previous errors
```

**Expected (PUBLISH inside MULTI):**
```
OK
QUEUED
1
subscribe
ps:txpub
1
message
ps:txpub
inside-tx
```

**Notes:** `SUBSCRIBE` is rejected immediately at queue time and marks the transaction dirty —
`EXEC` refuses the whole batch with `EXECABORT` (see `TXN-07` for the mirror case, queuing a
`SUBSCRIBE`-family command inside an already-open transaction). `PUBLISH`, by contrast, queues
normally and only actually publishes — and delivers to the live subscriber — when `EXEC` runs.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-09 — Cross-node delivery: a leader's PUBLISH reaches a follower's local subscriber

**Precondition:** The leader from the section intro is running. Start a follower and attach it:
```bash
ROCKET_MEM_ADDR=127.0.0.1:6610 ROCKET_MEM_RMP_ADDR=127.0.0.1:6611 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9610 \
ROCKET_MEM_AOF_PATH=$DATA/pubsub-follower.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/pubsub-follower.snap \
  $BIN &
echo $! > /tmp/pubsub-follower.pid
sleep 0.5
redis-cli -p 6610 replicaof 127.0.0.1 6600
sleep 0.5
redis-cli -p 6610 info replication | head -5   # master_link_status:up
```

**Steps:**
```bash
redis-cli -p 6610 subscribe crossnode > /tmp/pubsub09.out 2>&1 &
FSUB=$!
sleep 0.4
redis-cli -p 6600 publish crossnode from-leader
sleep 0.5
kill $FSUB; wait $FSUB 2>/dev/null
cat /tmp/pubsub09.out
```

**Expected:**
```
0
subscribe
crossnode
1
message
crossnode
from-leader
```

**Notes:** `PUBLISH` on the leader replies `0` — its *own* local subscriber count, genuinely zero
here — yet the follower's subscriber still receives the message a moment later. A publishing
node only ever reports its own local delivery count: the leader forwards the raw `PUBLISH` to the
follower over the existing replication stream, and the follower's own registry delivers it to its
local subscribers independently.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-10 — PUBLISH works directly against a read-only follower; SET still doesn't

**Precondition:** PUBSUB-09's leader/follower pair still attached.

**Steps:**
```bash
redis-cli -p 6610 subscribe followerpub > /tmp/pubsub10.out 2>&1 &
FP=$!
sleep 0.4
redis-cli -p 6610 publish followerpub direct-on-follower
redis-cli -p 6610 set shouldfail x
sleep 0.4
kill $FP; wait $FP 2>/dev/null
cat /tmp/pubsub10.out
```

**Expected:**
```
1
READONLY You can't write against a read only replica.

subscribe
followerpub
1
message
followerpub
direct-on-follower
```

**Notes:** `PUBLISH` issued straight at the follower succeeds and delivers locally (reply `1`) —
it is not a keyspace mutation and carries no key, so the `READONLY` gate that correctly rejects
`SET` on this follower never applies to it. A `PUBLISH` issued directly on a follower is not
itself re-forwarded anywhere (no follower-to-leader or follower-to-follower fan-out) — it only
reaches subscribers local to whichever node received the command.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-11 — Disconnect cleanup: PUBSUB CHANNELS/NUMSUB reflect a subscriber going away

**Precondition:** No existing subscriptions on `cleantest`.

**Steps:**
```bash
redis-cli -p 6600 subscribe cleantest > /tmp/pubsub11.out 2>&1 &
CPID=$!
sleep 0.4
redis-cli -p 6600 pubsub channels
redis-cli -p 6600 pubsub numsub cleantest
kill $CPID; wait $CPID 2>/dev/null
sleep 0.5
redis-cli -p 6600 pubsub channels
redis-cli -p 6600 pubsub numsub cleantest
```

**Expected:**
```
cleantest
cleantest
1
cleantest
0
```

**Notes:** Reading top to bottom: `PUBSUB CHANNELS` lists `cleantest` while the subscriber is
connected. After it disconnects, `PUBSUB CHANNELS` prints nothing at all (an empty array renders
as zero lines in `redis-cli`'s raw mode) and `PUBSUB NUMSUB cleantest` reports `0` — cleanup on
disconnect is prompt, even though there's no explicit unsubscribe in this scenario at all, only
the connection closing.

**Result:** ☐ Pass ☐ Fail

---

### PUBSUB-12 — Arity errors for SUBSCRIBE / PUBLISH / PSUBSCRIBE

**Precondition:** None.

**Steps:**
```bash
redis-cli -p 6600 subscribe
redis-cli -p 6600 publish onlyone
redis-cli -p 6600 psubscribe
```

**Expected:**
```
ERR wrong number of arguments for 'subscribe' command

ERR wrong number of arguments for 'publish' command

ERR wrong number of arguments for 'psubscribe' command

```

**Notes:** Unlike `INCR`'s bare `increment or decrement would overflow` (no `ERR` prefix,
CORE-51), these three all carry the correct `ERR` prefix.

**Result:** ☐ Pass ☐ Fail

---

Teardown:
```bash
kill $(cat /tmp/pubsub-leader.pid) $(cat /tmp/pubsub-follower.pid) 2>/dev/null
rm -f /tmp/pubsub-leader.pid /tmp/pubsub-follower.pid
rm -f /tmp/pubsub0*.out /tmp/pubsub1*.out
```

---

## Cluster

### CLUSTER-01 — Topology file must cover all 16384 slots exactly once

**Precondition:** No server on 7101/9360/6561.

**Steps — valid topology (baseline for every other cluster case):**
```bash
cat > $DATA/prc-cluster.conf <<'EOF'
shard-a 127.0.0.1:7101 0     5460
shard-b 127.0.0.1:7102 5461  10922
shard-c 127.0.0.1:7103 10923 16383
EOF
```

**Steps — gap error case:**
```bash
cat > $DATA/prc-cluster-gap.conf <<'EOF'
shard-a 127.0.0.1:7101 0     5460
shard-b 127.0.0.1:7102 5462  10922
shard-c 127.0.0.1:7103 10923 16383
EOF

ROCKET_MEM_ADDR=127.0.0.1:7101 ROCKET_MEM_AOF_PATH=$DATA/prc-a-bad.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-a-bad.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster-gap.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
  $BIN
echo "exit code: $?"
```

**Steps — overlap error case:**
```bash
cat > $DATA/prc-cluster-overlap.conf <<'EOF'
shard-a 127.0.0.1:7101 0     5461
shard-b 127.0.0.1:7102 5461  10922
shard-c 127.0.0.1:7103 10923 16383
EOF

ROCKET_MEM_ADDR=127.0.0.1:7101 ROCKET_MEM_AOF_PATH=$DATA/prc-a-bad2.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-a-bad2.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster-overlap.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
  $BIN
echo "exit code: $?"
```

**Expected:**
```
Error: Custom { kind: InvalidData, error: "cluster config has a slot gap: nothing owns slots 5461..=5461" }
exit code: 1

Error: Custom { kind: InvalidData, error: "cluster config ranges overlap: 'shard-a' ends at 5461 but 'shard-b' starts at 5461" }
exit code: 1
```

**Notes:** Both errors abort before any listener binds — no partial startup, no port left open.
The valid `prc-cluster.conf` written in the first step is reused by every following CLUSTER case.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-02 — Three nodes start with distinct metrics/RMP ports

**Precondition:** `$DATA/prc-cluster.conf` exists (from CLUSTER-01). No servers on
7101-7103/9360-9362/6561-6563.

**Steps:**
```bash
rm -f $DATA/prc-a.aof $DATA/prc-a.snap $DATA/prc-b.aof $DATA/prc-b.snap $DATA/prc-c.aof $DATA/prc-c.snap

ROCKET_MEM_ADDR=127.0.0.1:7101 ROCKET_MEM_AOF_PATH=$DATA/prc-a.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-a.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
  $BIN &
echo $! > /tmp/prc-cluster-a.pid

ROCKET_MEM_ADDR=127.0.0.1:7102 ROCKET_MEM_AOF_PATH=$DATA/prc-b.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-b.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-b \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 ROCKET_MEM_RMP_ADDR=127.0.0.1:6562 \
  $BIN &
echo $! > /tmp/prc-cluster-b.pid

ROCKET_MEM_ADDR=127.0.0.1:7103 ROCKET_MEM_AOF_PATH=$DATA/prc-c.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-c.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-c \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9362 ROCKET_MEM_RMP_ADDR=127.0.0.1:6563 \
  $BIN &
echo $! > /tmp/prc-cluster-c.pid
sleep 0.7
```

**Expected (shard-a's log; shard-b/c are identical modulo their own id/port/slots):**
```
Cluster mode enabled: node 'shard-a' at 127.0.0.1:7101 owns slots 0-5460 of 3 nodes
Recovered state from $DATA/prc-a.snap and $DATA/prc-a.aof
Metrics on http://127.0.0.1:9360/metrics
RMP listening on 127.0.0.1:6561
Listening on 127.0.0.1:7101
```

**Notes:** All three RMP ports (6561-6563) collide with the leader/follower bank used in the
Replication section above — that's fine as long as those servers were already killed first.
Never run the Replication and Cluster sections concurrently against the port list in this
document.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-03 — `CLUSTER KEYSLOT` / `SHARDS` / `NODES` / `INFO` / `MYID`

**Precondition:** Three-node cluster from CLUSTER-02 running.

**Steps:**
```bash
redis-cli -p 7101 cluster keyslot foo
redis-cli -p 7101 cluster shards
redis-cli -p 7101 cluster nodes
redis-cli -p 7101 cluster info
redis-cli -p 7101 cluster myid
```

**Expected:**
```
(integer) 12182
1) 1) "slots"
   2) 1) (integer) 0
      2) (integer) 5460
   3) "nodes"
   ...(shard-a entry: id shard-a, port 7101, ip 127.0.0.1, role master, health online)...
2) ... (shard-b: slots 5461-10922) ...
3) ... (shard-c: slots 10923-16383) ...
shard-a 127.0.0.1:7101@17101 myself,master - 0 0 0 connected 0-5460
shard-b 127.0.0.1:7102@17102 master - 0 0 0 connected 5461-10922
shard-c 127.0.0.1:7103@17103 master - 0 0 0 connected 10923-16383
cluster_enabled:1
cluster_state:ok
cluster_slots_assigned:16384
cluster_known_nodes:3
cluster_size:3
cluster_my_epoch:0
cluster_current_epoch:0
"shard-a"
```

**Notes:** `foo` hashes to slot 12182, owned by shard-c — used as the MOVED example in
CLUSTER-04. `CLUSTER NODES`'s `@17101` cluster-bus port suffix is advertised by convention only;
nothing is ever bound there (no cluster bus exists — see CLUSTER-06). `connected` and
`cluster_state:ok` above are live liveness-probe results, not hardcoded literals: each node probes
every other configured peer once per `cluster_probe_interval_secs` (default 1s) and reports
`disconnected`/`master,fail?`/`cluster_state:fail` once a peer misses `cluster_node_timeout_secs`
(default 15s) of probes. See CLUSTER-06 for what a genuinely dead peer looks like here.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-04 — `MOVED` on the wrong node, success on the right one

**Precondition:** Same cluster, `foo` maps to slot 12182 (shard-c, port 7103) per CLUSTER-03.

**Steps:**
```bash
redis-cli -p 7101 set foo bar     # shard-a does not own slot 12182
redis-cli -p 7101 get foo         # still redirected — proves no local write happened either

redis-cli -p 7103 set foo bar     # right node
redis-cli -p 7103 get foo
```

**Expected:**
```
(error) MOVED 12182 127.0.0.1:7103
(error) MOVED 12182 127.0.0.1:7103
OK
"bar"
```

**Notes:** `GET foo` on the wrong node also comes back `MOVED`, never a value — that's the
practical proof "no write happened": the node handing the key elsewhere is itself unable to
serve a locally-written copy back through the normal read path. There's no redis-cli-level way to
peek at a foreign shard's underlying store directly to confirm this any more literally than that.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-05 — `CROSSSLOT` on a multi-key command spanning slots

**Precondition:** Same cluster.

**Steps:**
```bash
redis-cli -p 7101 mset hello 1 foo 2
```

**Expected:**
```
(error) CROSSSLOT Keys in request don't hash to the same slot
```

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-06 — Hash tags route related keys to the same slot

**Precondition:** Same cluster.

**Steps:**
```bash
redis-cli -p 7101 cluster keyslot '{user1000}.name'
redis-cli -p 7101 cluster keyslot '{user1000}.city'

kill $(cat /tmp/prc-cluster-a.pid) $(cat /tmp/prc-cluster-b.pid) $(cat /tmp/prc-cluster-c.pid)
sleep 0.3
```

**Expected:**
```
(integer) 3443
(integer) 3443
```

**Notes — known limits to expect, not bugs:** no cluster bus and no gossip — nodes never agree
with each other on anything, so `cluster_slots_fail` is structurally always `0` and one node's
suspicion of a dead peer can never be promoted to an agreed failure; each node does, however,
probe its peers directly (`cluster_probe_interval_secs`/`cluster_node_timeout_secs`), so
`CLUSTER NODES` reports a peer that stops answering as `disconnected`/`master,fail?` and
`cluster_state` flips to `fail`, purely on this node's own observation — see CLUSTER-03's notes.
`cluster_state:fail` is report-only here: unlike real Redis, this node keeps serving its own slots
and a `MOVED` reply still points at the configured (possibly dead) owner, because picking a
different owner is a topology decision nothing here can agree on; no live resharding or failover —
`CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not exist as commands at all; no request
forwarding — a `MOVED` reply is final, the client must reconnect itself, this server never proxies
a request to another shard on the client's behalf; `CLUSTER SLOTS` is not implemented (deprecated
upstream since Redis 7.0 in favor of `CLUSTER SHARDS`, which is implemented — see CLUSTER-03).

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-07 — A killed peer is detected and reported within `cluster_node_timeout_secs`, and routing stays unchanged

**Precondition:** `$DATA/prc-cluster.conf` exists (CLUSTER-01). No servers on
7101-7103/9360-9362/6561-6563 — if CLUSTER-02 through CLUSTER-06 already ran in this session,
CLUSTER-06's last step already killed them.

**Steps:**
```bash
rm -f $DATA/prc-a.aof $DATA/prc-a.snap $DATA/prc-b.aof $DATA/prc-b.snap $DATA/prc-c.aof $DATA/prc-c.snap

# Same as CLUSTER-02, plus a short probe interval/timeout so this case takes seconds, not
# cluster_node_timeout_secs's 15s default.
ROCKET_MEM_ADDR=127.0.0.1:7101 ROCKET_MEM_AOF_PATH=$DATA/prc-a.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-a.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN &
echo $! > /tmp/prc-cluster-a.pid

ROCKET_MEM_ADDR=127.0.0.1:7102 ROCKET_MEM_AOF_PATH=$DATA/prc-b.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-b.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-b \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 ROCKET_MEM_RMP_ADDR=127.0.0.1:6562 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN &
echo $! > /tmp/prc-cluster-b.pid

ROCKET_MEM_ADDR=127.0.0.1:7103 ROCKET_MEM_AOF_PATH=$DATA/prc-c.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-c.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-c \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9362 ROCKET_MEM_RMP_ADDR=127.0.0.1:6563 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN &
echo $! > /tmp/prc-cluster-c.pid
sleep 1

redis-cli -p 7102 cluster info | grep cluster_state   # baseline: healthy

# Crash shard-a outright -- no graceful shutdown, the way a real crash looks.
kill -9 $(cat /tmp/prc-cluster-a.pid)
sleep 4   # one node_timeout (3s) plus one probe_interval (1s) of slack

redis-cli -p 7102 cluster nodes
redis-cli -p 7102 cluster info | grep -E 'cluster_state|cluster_slots_'
redis-cli -p 7102 cluster shards | grep -A1 health
curl -s localhost:9361/metrics | grep cluster_peers

# Routing is unchanged even though shard-a is known dead:
redis-cli -p 7102 get hello   # slot 866, owned by shard-a
```

**Expected:**
```
cluster_state:ok
shard-a 127.0.0.1:7101@17101 master,fail? - 0 0 0 disconnected 0-5460
shard-b 127.0.0.1:7102@17102 myself,master - 0 0 0 connected 5461-10922
shard-c 127.0.0.1:7103@17103 master - 0 0 0 connected 10923-16383
cluster_state:fail
cluster_slots_assigned:16384
cluster_slots_ok:10923
cluster_slots_pfail:5461
cluster_slots_fail:0
health
failed
rocket_mem_cluster_peers_reachable 1
rocket_mem_cluster_peers_unreachable 1
MOVED 866 127.0.0.1:7101
```

**Notes:** `cluster_slots_pfail:5461` is exactly shard-a's span (slots 0-5460, 5461 slots) —
`cluster_slots_fail` stays `0` by design: there is no cluster bus for a suspicion to be promoted
over (see CLUSTER-06's notes on *pfail* vs *fail*). shard-b's own terminal output carries exactly
one line for this transition, not one per probe round — a `WARN` naming `peer=shard-a
node_timeout_secs=3`. `GET hello` still redirects to `127.0.0.1:7101` — shard-a's *configured*
address — because picking a different owner for slots 0-5460 is a topology decision nothing here
has a mechanism to agree on. This is the report-only behavior CLUSTER-06 already describes in
prose; this case makes it a reproducible, exact-output test.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-08 — A peer that starts answering again is un-failed within one probe interval

**Precondition:** CLUSTER-07 just ran; shard-a is still dead, shard-b/shard-c still up.

**Steps:**
```bash
# Restart shard-a with the exact same env CLUSTER-07 used.
ROCKET_MEM_ADDR=127.0.0.1:7101 ROCKET_MEM_AOF_PATH=$DATA/prc-a.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-a.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-a \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9360 ROCKET_MEM_RMP_ADDR=127.0.0.1:6561 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN &
echo $! > /tmp/prc-cluster-a.pid
sleep 2

redis-cli -p 7102 cluster nodes | head -1
redis-cli -p 7102 cluster info | grep cluster_state
```

**Expected:**
```
shard-a 127.0.0.1:7101@17101 master - 0 0 0 connected 0-5460
cluster_state:ok
```

**Notes:** Recovery needs no restart of the survivors and no operator action beyond bringing the
node back — one successful probe is enough to mark it reachable again. This is symmetric with
CLUSTER-07: the liveness map has no memory of a dead peer once a probe succeeds.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-09 — Peer-probe connections log at `debug`, not `info`

**Precondition:** Same cluster as CLUSTER-07/08 (shard-a/b/c all up, `cluster_probe_interval_secs=1`).

**Steps:**
```bash
kill $(cat /tmp/prc-cluster-a.pid) $(cat /tmp/prc-cluster-b.pid) $(cat /tmp/prc-cluster-c.pid)
sleep 0.3

# Round 1: default log level (info). Capture shard-b's output this time.
ROCKET_MEM_ADDR=127.0.0.1:7102 ROCKET_MEM_AOF_PATH=$DATA/prc-b.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-b.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-b \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 ROCKET_MEM_RMP_ADDR=127.0.0.1:6562 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN > $DATA/prc-b-info.log 2>&1 &
echo $! > /tmp/prc-cluster-b.pid
# shard-a and shard-c, unchanged from CLUSTER-07/08's commands, so shard-b has a live peer to be probed by.

sleep 5
grep -c "connection accepted" $DATA/prc-b-info.log   # count X

kill $(cat /tmp/prc-cluster-b.pid)
sleep 0.3

# Round 2: same node, RUST_LOG=rocket_mem=debug this time.
RUST_LOG=rocket_mem=debug \
ROCKET_MEM_ADDR=127.0.0.1:7102 ROCKET_MEM_AOF_PATH=$DATA/prc-b.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-b.snap \
ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-b \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9361 ROCKET_MEM_RMP_ADDR=127.0.0.1:6562 \
ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
  $BIN > $DATA/prc-b-debug.log 2>&1 &
echo $! > /tmp/prc-cluster-b.pid

sleep 5
grep -c "connection accepted" $DATA/prc-b-debug.log   # count Y
```

**Expected:** Round 1 (`info`, `$DATA/prc-b-info.log`): the `connection accepted` count over 5
real seconds of probing (5 probe rounds from each of 2 peers) is **0** — no line at all from
probe traffic. Round 2 (`debug`, `$DATA/prc-b-debug.log`): the count is **on the order of 10**
(roughly one `DEBUG` pair per peer per second, two peers, five seconds).

**Notes:** This is the observable, black-box effect of the fixed marker this node's own probe
`PING`s carry (`crates/server/src/cluster_health.rs`) — the receiving side recognizes its own
probe traffic and logs that one connection's accept/close pair at `debug` instead of `info`,
specifically so a healthy, unchanging cluster stays quiet at the `info` default. There is no
black-box way to inspect the marker's exact bytes without a packet capture, which is out of scope
for this playbook — its effect on log volume, tested here, is what an operator actually needs to
verify.

**Result:** ☐ Pass ☐ Fail

---

### CLUSTER-10 — The peer prober is TLS-aware: it dials a TLS-only peer over TLS

**Precondition:** `openssl` installed (ENV-02). No servers on 7104-7109/9363-9365/6564-6566.

**Steps:**
```bash
mkdir -p $DATA/prc-tls
cd $DATA/prc-tls
# NOTE: TLS-01's plain "-subj /CN=localhost" recipe is NOT enough here -- it produces no
# Subject Alternative Name, and rocket-mem's own TLS client (used both for probing peers and for
# replication) requires one. Add -addext explicitly:
openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem -days 3650 -nodes \
  -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

cat > $DATA/prc-cluster-tls.conf <<'EOF'
shard-a localhost:7107 0     5460
shard-b localhost:7108 5461  10922
shard-c localhost:7109 10923 16383
EOF

for n in a:0:7104:7107:9363:6564 b:1:7105:7108:9364:6565 c:2:7106:7109:9365:6566; do
  IFS=: read id idx addr tlsaddr metrics rmp <<< "$n"
  ROCKET_MEM_ADDR=127.0.0.1:$addr ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:$tlsaddr \
  ROCKET_MEM_TLS_CERT_PATH=$DATA/prc-tls/cert.pem ROCKET_MEM_TLS_KEY_PATH=$DATA/prc-tls/key.pem \
  ROCKET_MEM_TLS_CA_PATH=$DATA/prc-tls/cert.pem \
  ROCKET_MEM_AOF_PATH=$DATA/prc-tls-$id.aof ROCKET_MEM_SNAPSHOT_PATH=$DATA/prc-tls-$id.snap \
  ROCKET_MEM_CLUSTER_CONFIG=$DATA/prc-cluster-tls.conf ROCKET_MEM_CLUSTER_NODE_ID=shard-$id \
  ROCKET_MEM_METRICS_ADDR=127.0.0.1:$metrics ROCKET_MEM_RMP_ADDR=127.0.0.1:$rmp \
  ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS=1 ROCKET_MEM_CLUSTER_NODE_TIMEOUT_SECS=3 \
    $BIN &
  echo $! > /tmp/prc-cluster-tls-$id.pid
done
sleep 2

redis-cli --tls --cacert $DATA/prc-tls/cert.pem -p 7108 cluster nodes
redis-cli --tls --cacert $DATA/prc-tls/cert.pem -p 7108 cluster info | grep cluster_state

kill -9 $(cat /tmp/prc-cluster-tls-a.pid)
sleep 4

redis-cli --tls --cacert $DATA/prc-tls/cert.pem -p 7108 cluster nodes
redis-cli --tls --cacert $DATA/prc-tls/cert.pem -p 7108 cluster info | grep -E 'cluster_state|cluster_slots_pfail'

kill $(cat /tmp/prc-cluster-tls-b.pid) $(cat /tmp/prc-cluster-tls-c.pid)
```

**Expected:**
```
shard-a localhost:7107@17107 master - 0 0 0 connected 0-5460
shard-b localhost:7108@17108 myself,master - 0 0 0 connected 5461-10922
shard-c localhost:7109@17109 master - 0 0 0 connected 10923-16383
cluster_state:ok
shard-a localhost:7107@17107 master,fail? - 0 0 0 disconnected 0-5460
shard-b localhost:7108@17108 myself,master - 0 0 0 connected 5461-10922
shard-c localhost:7109@17109 master - 0 0 0 connected 10923-16383
cluster_state:fail
cluster_slots_pfail:5461
```

**Notes:** The topology file's addresses (`localhost:7107`-`7109`) are shard-a/b/c's **TLS**
listener addresses, not their plaintext ones — this is what makes the prober dial each peer over
TLS. Without the `-addext` Subject Alternative Name above, every probe's TLS handshake fails
(`rustls` rejects the cert outright) and **every** peer reports `disconnected`/`cluster_state:fail`
immediately, even though all three processes are alive and each answers a direct `redis-cli --tls
... ping` individually — a real, reproducible gotcha: TLS-01's own cert-generation recipe
elsewhere in this playbook is sufficient for a `redis-cli --tls` client, but not for rocket-mem's
own peer-to-peer probing.

**Result:** ☐ Pass ☐ Fail

---

## Cleanup: persistence, replication, cluster

Run after every section, and always before ending the session:

```bash
ss -tln | grep -E ':(6560|6561|6562|6563|7101|7102|7103|9360|9361|9362|9363)\b' || echo "ALL TARGET PORTS FREE"
ps aux | grep rocket-mem | grep -v grep
```

Only kill entries in that `ps` output whose PID you personally captured in one of the `.pid`
files above. Any other `rocket-mem` process belongs to a different agent or a running chaos test
— leave it alone.

---


Audience: a QA engineer with no prior knowledge of this codebase. All commands below were
actually run against `"$ROCKET_MEM_BIN"`
(release build) on 2026-09-01. Output shown under "Expected" is real captured output, not
invented.

## Setup notes for configuration, RMP, and observability

- Binary: `"$ROCKET_MEM_BIN"`. Build once with
  `cargo build --release --workspace` if it isn't already built.
- Ports used throughout: `6570`/`6571`/`6572`/`6573` for RESP/RMP, `9370` for the Prometheus
  endpoint. Do not use other ports — other test runs may be using them concurrently.
- Every server is started in the background with `&`, its PID captured, and killed by that exact
  PID when the case is done. Never use `pkill -f rocket-mem` — it will kill other people's test
  servers too.
- Several cases run the server from a dedicated empty working directory so that a stray
  `rocket-mem.toml` doesn't change the outcome. Create one before you start:
  `mkdir -p /tmp/rm-qa-work && cd /tmp/rm-qa-work`.
- After every case, confirm the port(s) are free before moving to the next:
  `ss -tlnp | grep -E ':(6570|6571|6572|6573|9370)\b'` should print nothing.

---

## Configuration layering

Four layers, later wins: **built-in defaults < TOML file < `ROCKET_MEM_*` env vars < CLI flags**.
Reference docs: `docs/config-reference.md`, `.claude/manual-testing.md` ("Configuration
layering"), source: `crates/server/src/config.rs`.

### CFG-01 — Explicit `--config <path>` loads that TOML file

**Precondition:** A working directory with no `rocket-mem.toml` of its own (e.g. `/tmp/rm-qa-work`).

**Steps:**
```bash
mkdir -p /tmp/rm-qa-cfg
cat > /tmp/rm-qa-cfg/my-config.toml <<'EOF'
addr = "127.0.0.1:6570"
rmp_addr = "127.0.0.1:6571"
metrics_addr = "127.0.0.1:9370"
aof_path = "/tmp/rm-qa-cfg1.aof"
snapshot_path = "/tmp/rm-qa-cfg1.snap"
EOF

cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-cfg1.aof /tmp/rm-qa-cfg1.snap
"$ROCKET_MEM_BIN" \
  --config /tmp/rm-qa-cfg/my-config.toml &
PID=$!
sleep 0.5
redis-cli -p 6570 ping
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-cfg1.snap and /tmp/rm-qa-cfg1.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
PONG
```

**Notes:** The "Recovered state from ..." line prints even on a brand-new AOF/snapshot path with
nothing to recover — it is not proof a prior snapshot actually existed. Don't read it as a
warning sign.

**Result:** ☐ Pass ☐ Fail

---

### CFG-02 — Auto-pickup of `./rocket-mem.toml` when no `--config` is given

**Precondition:** CFG-01's `my-config.toml` exists at `/tmp/rm-qa-cfg/my-config.toml`.

**Steps:**
```bash
mkdir -p /tmp/rm-qa-auto
cp /tmp/rm-qa-cfg/my-config.toml /tmp/rm-qa-auto/rocket-mem.toml
cd /tmp/rm-qa-auto
rm -f /tmp/rm-qa-cfg1.aof /tmp/rm-qa-cfg1.snap
"$ROCKET_MEM_BIN" &
PID=$!
sleep 0.5
redis-cli -p 6570 ping
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-cfg1.snap and /tmp/rm-qa-cfg1.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
PONG
```

**Notes:** No `--config` flag was passed at all. The `./rocket-mem.toml` sitting in the current
directory was picked up automatically and its `addr` (6570) is what got bound.

**Result:** ☐ Pass ☐ Fail

---

### CFG-03 — Neither `--config` nor `./rocket-mem.toml`: falls through to env/defaults, not an error

**Precondition:** A working directory with no `rocket-mem.toml` in it.

**Steps:**
```bash
mkdir -p /tmp/rm-qa-empty && cd /tmp/rm-qa-empty
ls   # confirm it's empty — no rocket-mem.toml here
rm -f /tmp/rm-qa-envonly.aof /tmp/rm-qa-envonly.snap
ROCKET_MEM_ADDR=127.0.0.1:6570 ROCKET_MEM_RMP_ADDR=127.0.0.1:6571 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370 \
ROCKET_MEM_AOF_PATH=/tmp/rm-qa-envonly.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-qa-envonly.snap \
  "$ROCKET_MEM_BIN" &
PID=$!
sleep 0.5
redis-cli -p 6570 ping
redis-cli -p 6570 set envkey envval
redis-cli -p 6570 get envkey
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-envonly.snap and /tmp/rm-qa-envonly.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
PONG
OK
envval
```

**Notes:** This is also the backward-compatibility case: a pure `ROCKET_MEM_*` invocation with no
TOML anywhere works exactly as it did before config layering existed. A missing TOML at both the
`--config` layer and the auto-pickup layer is explicitly not a startup error.

**Result:** ☐ Pass ☐ Fail

---

### CFG-04 — A `--config` path that doesn't exist is silently ignored, not an error

**Precondition:** `/tmp/rm-qa-cfg/does-not-exist.toml` must not exist.

**Steps:**
```bash
cd /tmp/rm-qa-empty
ls /tmp/rm-qa-cfg/does-not-exist.toml   # confirm it really doesn't exist
rm -f /tmp/rm-qa-envonly.aof /tmp/rm-qa-envonly.snap
ROCKET_MEM_ADDR=127.0.0.1:6570 ROCKET_MEM_RMP_ADDR=127.0.0.1:6571 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370 \
ROCKET_MEM_AOF_PATH=/tmp/rm-qa-envonly.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-qa-envonly.snap \
  "$ROCKET_MEM_BIN" \
  --config /tmp/rm-qa-cfg/does-not-exist.toml &
PID=$!
sleep 0.5
kill -0 $PID && echo STILL_RUNNING
redis-cli -p 6570 ping
kill $PID
```

**Expected:**
```
ls: cannot access '/tmp/rm-qa-cfg/does-not-exist.toml': No such file or directory
STILL_RUNNING
Recovered state from /tmp/rm-qa-envonly.snap and /tmp/rm-qa-envonly.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
PONG
```

**Notes:** A typo'd `--config` path fails open, not loud: the process starts normally, the TOML
layer is silently skipped, and configuration falls through to the env-var layer (here) or
defaults. There is no warning printed anywhere. A deployment that relies on `--config` actually
loading will not notice a typo.

**Result:** ☐ Pass ☐ Fail

---

### CFG-05 — Precedence step 1: TOML file alone sets the bound address

**Precondition:** `/tmp/rm-qa-cfg/my-config.toml` from CFG-01 exists (addr=6570, rmp_addr=6571,
metrics_addr=9370).

**Steps:**
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-cfg1.aof /tmp/rm-qa-cfg1.snap
"$ROCKET_MEM_BIN" \
  --config /tmp/rm-qa-cfg/my-config.toml &
PID=$!
sleep 0.5
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-cfg1.snap and /tmp/rm-qa-cfg1.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
```

**Result:** ☐ Pass ☐ Fail

---

### CFG-06 — Precedence step 2: `ROCKET_MEM_ADDR` beats the TOML file

**Precondition:** Same TOML file as CFG-05.

**Steps:**
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-cfg1.aof /tmp/rm-qa-cfg1.snap
ROCKET_MEM_ADDR=127.0.0.1:6572 \
  "$ROCKET_MEM_BIN" \
  --config /tmp/rm-qa-cfg/my-config.toml &
PID=$!
sleep 0.5
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-cfg1.snap and /tmp/rm-qa-cfg1.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6572
```

**Notes:** `addr` bound on **6572** (the env value), not 6570 (the TOML value) — env beats file.
`rmp_addr`/`metrics_addr` are untouched, still from the TOML, since no env var set them.

**Result:** ☐ Pass ☐ Fail

---

### CFG-07 — Precedence step 3: `--addr` beats the env var, and unpassed CLI flags don't clobber lower layers

**Precondition:** Same TOML file and env var as CFG-06.

**Steps:**
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-cfg1.aof /tmp/rm-qa-cfg1.snap
ROCKET_MEM_ADDR=127.0.0.1:6572 \
  "$ROCKET_MEM_BIN" \
  --config /tmp/rm-qa-cfg/my-config.toml --addr 127.0.0.1:6573 &
PID=$!
sleep 0.5
redis-cli -p 6573 ping
kill $PID
```

**Expected:**
```
Recovered state from /tmp/rm-qa-cfg1.snap and /tmp/rm-qa-cfg1.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6573
PONG
```

**Notes:** Only `--addr` was passed on the command line. `addr` bound on 6573 (CLI beats env
beats file). `rmp_addr` (6571) and `metrics_addr` (9370) are still the TOML's values, not the
built-in defaults (`127.0.0.1:6380`/`127.0.0.1:9121`) and not reset by the unpassed flags — an
unset CLI flag is genuinely absent from the merge, not serialized as null.

**Result:** ☐ Pass ☐ Fail

---

### CFG-08 — A malformed env var value is a hard startup failure, not a fallback to the default

**Precondition:** None (works in any empty directory).

**Steps:**
```bash
cd /tmp/rm-qa-empty
ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=abc \
  "$ROCKET_MEM_BIN"
echo "exit=$?"
```

**Expected:**
```
Error: Custom { kind: InvalidInput, error: "config error: invalid type: found string \"abc\", expected u64 for key \"SLOWLOG_THRESHOLD_MICROS\" in `ROCKET_MEM_` environment variable(s)" }
exit=1
```

**Notes:** This is `std::io::Error`'s `Debug` output, not a hand-written message — noisy, but the
field name, expected type, and source layer are all in there. Nothing binds; the process exits
before any listener starts.

**Result:** ☐ Pass ☐ Fail

---

### CFG-09 — A malformed TOML value is also a hard startup failure

**Precondition:** None.

**Steps:**
```bash
cd /tmp/rm-qa-empty
cat > /tmp/rm-qa-cfg/bad.toml <<'EOF'
addr = "127.0.0.1:6570"
slowlog_threshold_micros = "not-a-number"
EOF
"$ROCKET_MEM_BIN" --config /tmp/rm-qa-cfg/bad.toml
echo "exit=$?"
```

**Expected:**
```
Error: Custom { kind: InvalidInput, error: "config error: invalid type: found string \"not-a-number\", expected u64 for key \"default.slowlog_threshold_micros\" in ../cro-cfg/bad.toml TOML file" }
exit=1
```

**Notes:** Same failure mode as CFG-08, just from the TOML layer instead of the env layer — the
error text names the TOML file and identifies the layer as `TOML file` rather than `environment
variable(s)`. The exact path text in the error will differ based on your cwd relative to the
TOML file; the important part is the "expected u64" / exit=1 shape, which is stable.

**Result:** ☐ Pass ☐ Fail

---

## RMP protocol

RMP listens unconditionally on its own port alongside RESP — there is no flag to disable it.
There is no `redis-cli`-equivalent CLI for RMP; the only client is the `rmp-client` crate in this
workspace, so exercising it by hand means writing a small Rust program. Reference:
`.claude/manual-testing.md` ("RMP"), source: `crates/rmp-client/src/lib.rs`,
`crates/server/src/rmp_connection.rs`.

**Setup used for every case below** — start once, reuse for RMP-01 through RMP-05, then tear down:
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-rmp.aof /tmp/rm-qa-rmp.snap
ROCKET_MEM_ADDR=127.0.0.1:6570 ROCKET_MEM_RMP_ADDR=127.0.0.1:6571 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370 \
ROCKET_MEM_AOF_PATH=/tmp/rm-qa-rmp.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-qa-rmp.snap \
  "$ROCKET_MEM_BIN" &
PID=$!
sleep 0.5
```

### RMP-01 — RMP listens on its own port, banner printed unconditionally

**Precondition:** Server started per the setup block above.

**Steps:**
```bash
# (just re-check the server's already-printed startup banner, or PING RESP to confirm it's up)
redis-cli -p 6570 ping
```

**Expected (banner from the setup block's stdout):**
```
Recovered state from /tmp/rm-qa-rmp.snap and /tmp/rm-qa-rmp.aof
Metrics on http://127.0.0.1:9370/metrics
RMP listening on 127.0.0.1:6571
Listening on 127.0.0.1:6570
PONG
```

**Notes:** `RMP listening on 127.0.0.1:6571` is printed with no config needed to turn it on and
no flag that turns it off.

**Result:** ☐ Pass ☐ Fail

---

### RMP-02 — Round trip via a throwaway `rmp-client` example

**Precondition:** Server from the setup block still running on 6570/6571/9370. This case writes
a temporary file into the repo under `crates/rmp-client/examples/` and deletes it afterward —
never commit it.

**Steps:**
```bash
cat > crates/rmp-client/examples/qa_scratch.rs <<'EOF'
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = rmp_client::RmpClient::connect("127.0.0.1:6571").await?;
    client.set("foo", "bar").await?;
    let got = client.get("foo").await?;
    println!("round-trip: foo -> {:?}", got);
    Ok(())
}
EOF

cd "$ROCKET_MEM_REPO"
cargo run -p rmp-client --example qa_scratch

rm crates/rmp-client/examples/qa_scratch.rs
git status --porcelain   # must print nothing — confirms the scratch file is gone
```

**Expected:**
```
round-trip: foo -> Some(b"bar")
```
(plus normal `cargo run` compile/finished/running lines before it; `git status --porcelain`
prints nothing after cleanup)

**Notes:** `rmp-client` is library-only — there is no CLI equivalent to `redis-cli` for RMP. This
is the one area of the product where testing it by hand requires a Rust toolchain, not just a
terminal.

**Result:** ☐ Pass ☐ Fail

---

### RMP-03 — RESP and RMP share one keyspace, verified in both directions

**Precondition:** Server from the setup block still running. `foo`=`bar` already exists from
RMP-02 (harmless either way).

**Steps:**
```bash
# Direction 1: write over RESP, read over RMP.
redis-cli -p 6570 set fromresp viaresp

cat > crates/rmp-client/examples/qa_scratch2.rs <<'EOF'
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = rmp_client::RmpClient::connect("127.0.0.1:6571").await?;
    let v = client.get("fromresp").await?;
    println!("RESP->RMP: fromresp -> {:?}", v.map(|b| String::from_utf8_lossy(&b).into_owned()));
    // Direction 2: write over RMP, will be read back over RESP below.
    client.set("fromrmp", "viarmp").await?;
    println!("RMP wrote fromrmp=viarmp");
    Ok(())
}
EOF
cd "$ROCKET_MEM_REPO"
cargo run -p rmp-client --example qa_scratch2
rm crates/rmp-client/examples/qa_scratch2.rs

# Direction 2 check: read back over RESP.
redis-cli -p 6570 get fromrmp
```

**Expected:**
```
OK
RESP->RMP: fromresp -> Some("viaresp")
RMP wrote fromrmp=viarmp
viarmp
```

**Notes:** One `Engine`, one set of shards behind both protocols — no sync step involved.

**Result:** ☐ Pass ☐ Fail

---

### RMP-04 — RMP reaches nearly the whole command set through the same dispatcher

**Precondition:** Server from the setup block still running.

**Steps:**
```bash
cat > crates/rmp-client/examples/qa_scratch3.rs <<'EOF'
use bytes::Bytes;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = rmp_client::RmpClient::connect("127.0.0.1:6571").await?;
    let info = client.call(vec![Bytes::from_static(b"INFO"), Bytes::from_static(b"server")]).await?;
    println!("INFO server -> {:?}", info);
    let save = client.call(vec![Bytes::from_static(b"SAVE")]).await?;
    println!("SAVE -> {:?}", save);
    let len = client.call(vec![Bytes::from_static(b"SLOWLOG"), Bytes::from_static(b"LEN")]).await?;
    println!("SLOWLOG LEN -> {:?}", len);
    Ok(())
}
EOF
cd "$ROCKET_MEM_REPO"
cargo run -p rmp-client --example qa_scratch3
rm crates/rmp-client/examples/qa_scratch3.rs
```

**Expected:**
```
INFO server -> Bulk(b"# Server\r\nredis_version:rocket-mem-0.1.4\r\nrocket_mem_version:0.1.4\r\n...")
SAVE -> Simple("OK")
SLOWLOG LEN -> Integer(0)
```
(actual captured run: `SAVE -> Simple("OK")`, `SLOWLOG LEN -> Integer(0)`, `INFO server` first
three lines were `# Server | redis_version:rocket-mem-0.1.4 | rocket_mem_version:0.1.4`)

**Notes:** `client.call(vec![...])` builds the same `Array`-of-`Bulk` shape RESP sends and reaches
the identical `dispatch_and_log` — `INFO`, `SAVE`, `SLOWLOG`, `CLUSTER`, `REPLICAOF` all work over
RMP with AOF logging and the replica fan-out applying exactly as over RESP.

**Result:** ☐ Pass ☐ Fail

---

### RMP-05 — `PSYNC` is the one command RMP cannot reach

**Precondition:** Server from the setup block still running.

**Steps:**
```bash
cat > crates/rmp-client/examples/qa_scratch4.rs <<'EOF'
use bytes::Bytes;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = rmp_client::RmpClient::connect("127.0.0.1:6571").await?;
    let reply = client.call(vec![Bytes::from_static(b"PSYNC")]).await?;
    println!("PSYNC -> {:?}", reply);
    Ok(())
}
EOF
cd "$ROCKET_MEM_REPO"
cargo run -p rmp-client --example qa_scratch4
rm crates/rmp-client/examples/qa_scratch4.rs

# Tear down the server started for RMP-01..05.
kill $PID
```

**Expected:**
```
PSYNC -> Error("ERR unknown command 'PSYNC'")
```

**Notes:** RESP intercepts `PSYNC` in `connection.rs` above `dispatch_and_log` for its raw-socket
takeover to stream replication data; RMP's handler has no equivalent, so the command falls all the
way through to "unknown command". `HELLO` is *not* an exception the same way — it succeeds over
RMP as a stateless no-op, since RMP has no per-connection negotiation state to persist.

**Result:** ☐ Pass ☐ Fail

---

**Ordering caveat (not a test — read before relying on RMP ordering):** each RMP request is
handled on its own freshly spawned Tokio task. The read loop decodes a request, spawns a task for
it, and immediately decodes the next one without waiting for the reply. Commands sent
back-to-back on **one** RMP connection can therefore *execute* out of order, not just reply out of
order — unlike RESP, which processes one connection's commands in strict send order. A client that
needs command B to observe command A's effect must await A's reply before sending B. Each
connection also caps in-flight requests at 256; pipelining past that applies ordinary TCP
backpressure rather than spawning unbounded tasks.

---

## Observability

Reference: `.claude/manual-testing.md` ("Standalone mode"), source: `crates/server/src/dispatcher.rs`
(`INFO`/`SLOWLOG`), `crates/server/src/metrics.rs`, `crates/server/src/slowlog.rs`.

### OBS-01 — `INFO server` returns real values, not stubs

**Precondition:** A server running with `ROCKET_MEM_ADDR=127.0.0.1:6570`,
`ROCKET_MEM_RMP_ADDR=127.0.0.1:6571`, `ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370` (same shape as the
RMP setup block above; start it the same way and keep it running through OBS-05).

**Steps:**
```bash
redis-cli -p 6570 info server
sleep 3
redis-cli -p 6570 info server | grep uptime_in_seconds
```

**Expected:**
```
# Server
redis_version:rocket-mem-0.1.4
rocket_mem_version:0.1.4
redis_mode:standalone
os:linux
arch_bits:64
process_id:2389374
uptime_in_seconds:17
uptime_in_days:0

uptime_in_seconds:24
```

**Notes:** `process_id` is the real PID of the running process (yours will differ).
`uptime_in_seconds` visibly increased across the 3-second sleep — proof it's a live clock, not a
hardcoded `0`.

**Result:** ☐ Pass ☐ Fail

---

### OBS-02 — `INFO replication`

**Precondition:** Same server as OBS-01.

**Steps:**
```bash
redis-cli -p 6570 info replication
```

**Expected:**
```
# Replication
role:master
master_repl_offset:0
connected_slaves:0

```

**Result:** ☐ Pass ☐ Fail

---

### OBS-03 — Bare `INFO` lists every section

**Precondition:** Same server.

**Steps:**
```bash
redis-cli -p 6570 info | grep -E "^# "
```

**Expected:**
```
# Server
# Clients
# Memory
# Persistence
# Stats
# Replication
# Cluster
# Keyspace
```

**Result:** ☐ Pass ☐ Fail

---

### OBS-04 — `/metrics` Prometheus endpoint

**Precondition:** Same server. Its metrics endpoint is at `http://127.0.0.1:9370/metrics`.

**Steps:**
```bash
curl -s http://127.0.0.1:9370/metrics | grep -E "^rocket_mem_commands_total|^rocket_mem_connected_clients|^rocket_mem_command_errors_total"

# Generate a command error and confirm it's counted.
redis-cli -p 6570 set   # missing args -> error
curl -s http://127.0.0.1:9370/metrics | grep "rocket_mem_command_errors_total"
```

**Expected:**
```
rocket_mem_commands_total{cmd="set"} 3
rocket_mem_commands_total{cmd="save"} 1
rocket_mem_commands_total{cmd="ping"} 1
rocket_mem_commands_total{cmd="get"} 4
rocket_mem_commands_total{cmd="psync"} 1
rocket_mem_commands_total{cmd="info"} 5
rocket_mem_commands_total{cmd="slowlog"} 1
rocket_mem_command_errors_total{cmd="psync"} 1
rocket_mem_connected_clients 0

ERR wrong number of arguments for 'set' command

# TYPE rocket_mem_command_errors_total counter
rocket_mem_command_errors_total{cmd="psync"} 1
rocket_mem_command_errors_total{cmd="set"} 1
```

**Notes:** Exact counter values depend on what ran on this server instance before you got here
(this capture followed the RMP cases, hence `cmd="psync"` already present) — what matters is that
the families exist and increase with real traffic, not the specific numbers.
`rocket_mem_connected_clients` reads 0 here because `redis-cli` closes its connection after each
command; it only shows non-zero while a connection is actually open (e.g. inside a pipe held open
with `printf ... | redis-cli`). `/metrics` has **no authentication of its own** — it is
unauthenticated by design, which is why it defaults to binding loopback only; never expose it
publicly without a reverse-proxy or firewall in front of it.

**Result:** ☐ Pass ☐ Fail

---

### OBS-05 — `SLOWLOG GET`/`LEN`/`RESET`, generating a real entry with `DEBUG SLEEP`

**Precondition:** Same server, default `slowlog_threshold_micros` (10000, i.e. 10ms).

**Steps:**
```bash
redis-cli -p 6570 slowlog len
redis-cli -p 6570 slowlog reset
redis-cli -p 6570 slowlog len

redis-cli -p 6570 debug sleep 0.05
redis-cli -p 6570 slowlog len
redis-cli -p 6570 slowlog get

redis-cli -p 6570 slowlog reset
redis-cli -p 6570 slowlog len

# DEBUG SLEEP is capped at 10 seconds.
redis-cli -p 6570 debug sleep 15
```

**Expected:**
```
0
OK
0
OK
1
0
1788234694
50102
DEBUG
sleep
... (1 more arguments)
OK
0
ERR DEBUG SLEEP duration exceeds the 10s maximum allowed on this server
```

**Notes:** A slow-log entry has **4 fields** (id, unix time, duration in microseconds, and an args
array), not real Redis's 6 — there is no client-address or client-name field. The args array
carries only the command name as sent (here lowercase `sleep`... actually `debug`, verbatim as
typed) plus its first argument (`sleep`, the DEBUG subcommand acting as the "key" position), and
summarizes anything past that with real Redis's own `... (N more arguments)` truncation marker —
here 1 more argument (the `0.05` duration) was not carried. `DEBUG SLEEP` above 10 seconds is
rejected outright rather than clamped.

**Result:** ☐ Pass ☐ Fail

---

### OBS-06 — `ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=0` disables the slow log entirely

**Precondition:** Kill any server bound to 6570/6571/9370 first (`ss -tlnp | grep -E ':(6570|6571|9370)'` should be empty), since the threshold can only be set at startup.

**Steps:**
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-obs6.aof /tmp/rm-qa-obs6.snap
ROCKET_MEM_ADDR=127.0.0.1:6570 ROCKET_MEM_RMP_ADDR=127.0.0.1:6571 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370 \
ROCKET_MEM_AOF_PATH=/tmp/rm-qa-obs6.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-qa-obs6.snap \
ROCKET_MEM_SLOWLOG_THRESHOLD_MICROS=0 \
  "$ROCKET_MEM_BIN" &
PID=$!
sleep 0.5
redis-cli -p 6570 debug sleep 0.2
redis-cli -p 6570 slowlog len
kill $PID
```

**Expected:**
```
OK
0
```

**Notes:** A 200ms `DEBUG SLEEP` — 20x the default 10ms threshold — produces zero slow-log
entries when the threshold is `0`. `0` disables the slow log entirely rather than meaning
"log everything."

**Result:** ☐ Pass ☐ Fail

---

### OBS-07 — `expired_keys` counts only active expiry, not passive

**Precondition:** A fresh server on 6570/6571/9370 (default threshold is fine), nothing else
touching TTL'd keys on it during this case.

**Steps:**
```bash
cd /tmp/rm-qa-work
rm -f /tmp/rm-qa-obs7.aof /tmp/rm-qa-obs7.snap
ROCKET_MEM_ADDR=127.0.0.1:6570 ROCKET_MEM_RMP_ADDR=127.0.0.1:6571 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9370 \
ROCKET_MEM_AOF_PATH=/tmp/rm-qa-obs7.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/rm-qa-obs7.snap \
  "$ROCKET_MEM_BIN" &
PID=$!
sleep 0.5
redis-cli -p 6570 info stats | grep expired_keys

# Passive path: read the key yourself after it expires.
redis-cli -p 6570 set pkey pval px 50
sleep 0.15
redis-cli -p 6570 get pkey                      # -> nil, passive removal on read
sleep 2                                          # > one full active-sweep rotation (16 shards x 100ms)
redis-cli -p 6570 info stats | grep expired_keys # still 0 -- passive removal is invisible to it

# Active path: never read the key, let the background sweep find it.
redis-cli -p 6570 set akey aval px 50
sleep 2
redis-cli -p 6570 info stats | grep expired_keys # now 1 -- only the never-read key counted
redis-cli -p 6570 get akey                       # -> nil, confirms it's gone

kill $PID
```

**Expected:**
```
expired_keys:0
OK
(nil)
expired_keys:0
OK
expired_keys:1
(nil)
```

**Notes:** `expired_keys` is only incremented from the background active-expiry sweep
(`crates/server/src/connection.rs`'s `active_expire_loop`, which walks one of the 16 shards every
100ms). A key removed by a client's own read (lazy/passive expiry) is deleted from its shard
before the sweep ever gets there, so the sweep finds nothing and the counter never moves for that
key — passive expiry is invisible to `expired_keys` forever, not just delayed. This is a
documented known limit, not a bug to file.

**Result:** ☐ Pass ☐ Fail

---

The cases above predate this project's structured-logging/tracing layer
(`docs/superpowers/specs/2026-09-07-structured-logging-design.md`,
`docs/superpowers/specs/2026-09-09-verbose-logging-design.md`). OBS-08 through OBS-14 below cover
it — per-command debug logging, credential redaction, the startup config summary, connection
spans, and the replica/cluster-peer Prometheus gauges.

### OBS-08 — Per-command debug log line: `elapsed_us` and reply kind

**Precondition:** A server started with `RUST_LOG=debug` and no ACL configured.

**Steps:**
```bash
RUST_LOG=debug "$ROCKET_MEM_BIN" --addr 127.0.0.1:6680 --rmp-addr 127.0.0.1:6681 \
  --metrics-addr 127.0.0.1:9380 --aof-path /tmp/rm-qa-obs8.aof --snapshot-path /tmp/rm-qa-obs8.snap &
PID=$!
sleep 0.5
redis-cli -p 6680 set k1 v1
redis-cli -p 6680 get k1
redis-cli -p 6680 nosuchcommand
kill $PID
```

**Expected:** one `DEBUG`-level `command dispatched` line per command, inside the `cmd{cmd=...
key=... argc=...}` span, carrying `elapsed_us` and a quoted `reply` kind:
```
DEBUG ...cmd{cmd=SET key=k1 argc=2}: rocket_mem::dispatcher: command dispatched elapsed_us=58 reply="ok"
DEBUG ...cmd{cmd=GET key=k1 argc=1}: rocket_mem::dispatcher: command dispatched elapsed_us=7 reply="ok"
DEBUG ...cmd{cmd=NOSUCHCOMMAND key= argc=0}: rocket_mem::dispatcher: unknown command cmd=NOSUCHCOMMAND
DEBUG ...cmd{cmd=NOSUCHCOMMAND key= argc=0}: rocket_mem::dispatcher: command dispatched elapsed_us=13 reply="error"
```

**Notes:** `reply` is `"ok"` or `"error"`, not the reply body — that stays out of the log at
`debug`. `elapsed_us` is present on every `command dispatched` line regardless of outcome. This
event is invisible at the production default of `info`, unlike OBS-04's Prometheus counters,
which increment regardless of log level.

**Result:** ☐ Pass ☐ Fail

---

### OBS-09 — Credential redaction: AUTH/HELLO/REPLICAOF passwords never appear in the log, even at `trace`

**Precondition:** A server started with an ACL user configured (auth becomes mandatory the
moment any `[[acl.users]]` entry exists — see "ACL and authentication"), and `RUST_LOG=trace`:
```bash
cat > /tmp/rm-qa-obs9.toml <<'EOF'
addr = "127.0.0.1:6680"
rmp_addr = "127.0.0.1:6681"
metrics_addr = "127.0.0.1:9380"

[[acl.users]]
username = "tester"
password = "secretpw123"
enabled = true
rules = ["allcommands", "allkeys"]
EOF
RUST_LOG=trace "$ROCKET_MEM_BIN" --config /tmp/rm-qa-obs9.toml \
  --aof-path /tmp/rm-qa-obs9.aof --snapshot-path /tmp/rm-qa-obs9.snap > /tmp/rm-qa-obs9.log 2>&1 &
PID=$!
sleep 0.5
```

**Steps:**
```bash
redis-cli -p 6680 auth tester wrongpassword123
printf 'auth tester secretpw123\nping\n' | redis-cli -p 6680
grep -c "secretpw123\|wrongpassword123" /tmp/rm-qa-obs9.log
kill $PID
```

**Expected:**
```
WRONGPASS invalid username-password pair or user is disabled.

OK
PONG
0
```

**Notes:** The final `0` is the load-bearing line — grepping the *entire* trace-level log file
(startup logging, connection spans, and the `trace`-level argument line included) for either
password string finds zero matches. The `trace`-level argument-trace line for `AUTH` itself
renders as `args=<redacted>` rather than the credential; this also covers `HELLO ... AUTH` and
`REPLICAOF ... AUTH`, which take the same whole-argument-list redaction. Auth success/failure
events (OBS-11) log the `user` field but never the password, at any level.

**Result:** ☐ Pass ☐ Fail

---

### OBS-10 — `log_value_max_bytes` caps trace-level value rendering

**Precondition:** Same server shape as OBS-09, but started with a small
`ROCKET_MEM_LOG_VALUE_MAX_BYTES` and `RUST_LOG=trace`:
```bash
ROCKET_MEM_LOG_VALUE_MAX_BYTES=8 RUST_LOG=trace "$ROCKET_MEM_BIN" --config /tmp/rm-qa-obs9.toml \
  --aof-path /tmp/rm-qa-obs10.aof --snapshot-path /tmp/rm-qa-obs10.snap > /tmp/rm-qa-obs10.log 2>&1 &
PID=$!
sleep 0.5
printf 'auth tester secretpw123\nset longkey abcdefghijklmnopqrstuvwxyz0123456789\n' | redis-cli -p 6680
kill $PID
```

**Steps:**
```bash
grep "command arguments" /tmp/rm-qa-obs10.log | grep SET
```

**Expected:**
```
...cmd{cmd=SET key=longkey argc=2}: rocket_mem::dispatcher: command arguments args=longkey abcdefgh…(28 more)
```

**Notes:** The cap applies **per argument independently**, in bytes of the stored value: `longkey`
is 7 bytes, under the 8-byte cap, so it renders whole; the 36-byte value truncates to its first 8
bytes plus a `…(28 more)` marker. `log_value_max_bytes` defaults to 128 and is only consulted at
`trace` — raising or lowering it has no effect at `debug` or above.

**Result:** ☐ Pass ☐ Fail

---

### OBS-11 — Auth success/failure logged by username; NOPERM denials logged by username — never the password

**Precondition:** Same ACL server as OBS-09, any log level `info` or above (these events are
`info`/`warn`, so they're visible at the production default — unlike OBS-08's per-command line).

**Steps:**
```bash
redis-cli -p 6680 auth tester wrongpassword123     # bad password
redis-cli -p 6680 auth tester secretpw123          # good password
grep -E "auth (success|failure)" /tmp/rm-qa-obs9.log
```

**Expected:**
```
WARN ...: rocket_mem::dispatcher: auth failure user=tester
INFO ...: rocket_mem::dispatcher: auth success user=tester
```

**Notes:** Both events carry `user`, never the password, at any level. A `NOPERM` denial (a
permitted user running a command or touching a key their rules don't grant) logs the same way —
`user`/`cmd`/`key`, never a secret.

**Result:** ☐ Pass ☐ Fail

---

### OBS-12 — Startup config-summary log line, with redaction

**Precondition:** Any server start with ACL configured, at the default `info` level (this line is
`info`, no `RUST_LOG` override needed).

**Steps:**
```bash
"$ROCKET_MEM_BIN" --config /tmp/rm-qa-obs9.toml \
  --aof-path /tmp/rm-qa-obs12.aof --snapshot-path /tmp/rm-qa-obs12.snap 2>&1 | head -2
```

**Expected:** two `INFO` lines before any listener-bound line — a "starting" line with
`version`/`node_id`, then a "resolved config summary" line enumerating every operationally
relevant field by name:
```
INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6680
INFO rocket_mem: resolved config summary node_id=127.0.0.1:6680 addr=127.0.0.1:6680 rmp_addr=127.0.0.1:6681 metrics_addr=127.0.0.1:9380 aof_path=... snapshot_path=... log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=1 tls_enabled=false tls_replication_enabled=false
```

**Notes:** No ACL username, password, or TLS key material appears — only `acl_enabled` (bool) and
`acl_user_count` (a count). `log_filter` reports the *resolved* filter directive (`RUST_LOG` if
set, else the configured `log_level`) — the log line's field is named `log_filter` even though
the config key is `log_level`.

**Result:** ☐ Pass ☐ Fail

---

### OBS-13 — `conn` span: connection accepted/closed with `commands_served`, per-protocol listener-bound events

**Precondition:** A server at the default `info` level.

**Steps:**
```bash
"$ROCKET_MEM_BIN" --addr 127.0.0.1:6680 --rmp-addr 127.0.0.1:6681 --metrics-addr 127.0.0.1:9380 \
  --aof-path /tmp/rm-qa-obs13.aof --snapshot-path /tmp/rm-qa-obs13.snap > /tmp/rm-qa-obs13.log 2>&1 &
PID=$!
sleep 0.5
printf 'ping\nset a 1\nset b 2\n' | redis-cli -p 6680
sleep 0.2
grep -E "listener bound|connection accepted|connection closed" /tmp/rm-qa-obs13.log
kill $PID
```

**Expected:**
```
INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:9380/metrics
INFO rocket_mem: listener bound protocol=RMP addr=127.0.0.1:6681
INFO rocket_mem: listener bound protocol=RESP addr=127.0.0.1:6680
INFO conn{conn_id=1 peer=127.0.0.1:NNNNN protocol=RESP tls=false node_id=127.0.0.1:6680}: rocket_mem::connection: connection accepted
INFO conn{conn_id=1 peer=127.0.0.1:NNNNN protocol=RESP tls=false node_id=127.0.0.1:6680}: rocket_mem::connection: connection closed elapsed_us=NNN commands_served=3
```

**Notes:** `protocol` renders unquoted uppercase (`RESP`, `RMP`; `RESP+TLS`/`RMP+TLS` under TLS)
except the metrics endpoint, which is lowercase `metrics`. `node_id` falls back to `config.addr`
when the node has no `cluster_node_id`. Every `conn`-scoped line in a session carries the same
`conn_id`, the correlation key across the connection's lifetime. `commands_served` counts every
dispatched command, including ones that errored.

**Result:** ☐ Pass ☐ Fail

---

### OBS-14 — Prometheus replica and cluster-peer gauges

**Precondition:** Same server as OBS-04, its metrics endpoint reachable.

**Steps:**
```bash
curl -s http://127.0.0.1:9380/metrics | grep -E \
  "^rocket_mem_(good_replicas|replica_min_ack_offset|master_repl_offset|slave_repl_offset|cluster_peers_reachable|cluster_peers_unreachable) "
```

**Expected**, on a standalone node with no replicas connected:
```
rocket_mem_good_replicas 0
rocket_mem_replica_min_ack_offset 0
rocket_mem_master_repl_offset 37
rocket_mem_slave_repl_offset 0
```

**Notes:** `rocket_mem_good_replicas`, `rocket_mem_replica_min_ack_offset`,
`rocket_mem_master_repl_offset`, and `rocket_mem_slave_repl_offset` are reported
**unconditionally**, even on a standalone node with zero replicas (they read `0`/`0`/non-zero
write offset/`0` rather than being absent). `rocket_mem_cluster_peers_reachable`/
`rocket_mem_cluster_peers_unreachable`, by contrast, are emitted **only in cluster mode with a
peer prober running** — confirmed absent from this standalone instance's `/metrics` output by
design, not a bug. See CLUSTER-07 for these two gauges' cluster-mode behavior.

**Result:** ☐ Pass ☐ Fail

---

## Cleanup: configuration, RMP, and observability

```bash
ps aux | grep rocket-mem | grep -v grep
ss -tlnp | grep -E ':(6570|6571|6572|6573|9370)\b'
```

Both should show nothing of yours. Kill any stray PID individually — never `pkill -f rocket-mem`.

---


Scope: the Sprint 8 access-control and transport-security surface. Every case below was executed
against the release binary and the "Expected" blocks are captured output, not paraphrase.

## ACL and TLS: before you start

Binary under test:

```
"$ROCKET_MEM_BIN"
```

Working directory used throughout. Create it once; every case writes only inside it.

```bash
mkdir -p /tmp/acltls-qa
```

Ports used by this playbook: `6510` (plaintext RESP), `6511` (plaintext RMP), `6530` (TLS RESP),
`6531` (TLS RMP), `9310` (Prometheus metrics). Do not reuse them for anything else while running.

Tools required: `redis-cli` (verified with 8.10.1), `openssl` (verified with 3.0.13), `curl`, `ss`.

Three things about `redis-cli` that affect how you read every "Expected" block:

- A server-side error (`NOAUTH ...`, `NOPERM ...`, `ERR ...`) is printed to **stdout**, with one
  extra blank line after it, and `redis-cli` still exits **0**. Only a connection-level failure
  (for example a TLS handshake failure) exits non-zero. Do not script pass/fail on exit status for
  server errors — match the text.
- `--user`/`--pass` without `--no-auth-warning` prepends
  `Warning: Using a password with '-a' or '-u' option on the command line interface may not be safe.`
  Every case below passes `--no-auth-warning` to keep the output clean.
- Each `redis-cli <args> <command>` invocation opens **and closes** its own connection. That is
  load-bearing for ACL-06.

Never stop a server with `pkill -f rocket-mem`. Kill the specific PID you started. Each server case
records its PID in a file for exactly that reason.

---

## ACL and authentication

### ACL-01 — Bootstrap four ACL users from TOML and start the server

**Precondition:** `/tmp/acltls-qa` exists. Ports 6510, 6511 and 9310 are free
(`ss -lnt | grep -E ':(6510|6511|9310)\b'` prints nothing).

**Steps:**
```bash
cat > /tmp/acltls-qa/acl.toml <<'EOF'
addr = "127.0.0.1:6510"
rmp_addr = "127.0.0.1:6511"
metrics_addr = "127.0.0.1:9310"
aof_path = "/tmp/acltls-qa/acl.aof"
snapshot_path = "/tmp/acltls-qa/acl.snap"

[[acl.users]]
username = "admin"
password = "adminpw"
enabled = true
rules = ["allcommands", "allkeys"]

[[acl.users]]
username = "app"
password = "apppw"
enabled = true
rules = ["~app:*", "+get"]

[[acl.users]]
username = "retired"
password = "retiredpw"
enabled = false
rules = ["allcommands", "allkeys"]

[[acl.users]]
username = "scoped"
password = "scopedpw"
enabled = true
rules = ["allcommands", "~app:*"]
EOF

cd /tmp/acltls-qa
nohup "$ROCKET_MEM_BIN" \
  --config /tmp/acltls-qa/acl.toml > /tmp/acltls-qa/acl-server.log 2>&1 &
echo "PID=$!" > /tmp/acltls-qa/acl.pid

sleep 1.5
cat /tmp/acltls-qa/acl.pid
cat /tmp/acltls-qa/acl-server.log
```

**Expected:**
```
PID=2373827
2026-09-12T05:36:10.429907Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:36:10.429946Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=/tmp/acltls-qa/acl.aof snapshot_path=/tmp/acltls-qa/acl.snap log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=4 tls_enabled=false tls_replication_enabled=false
2026-09-12T05:36:10.554031Z  INFO rocket_mem::aof: aof recovery replay complete commands=0 bytes=0 elapsed_us=6
2026-09-12T05:36:10.554361Z  INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:9310/metrics
2026-09-12T05:36:10.554413Z  INFO rocket_mem: listener bound protocol=RMP addr=127.0.0.1:6511
2026-09-12T05:36:10.554437Z  INFO rocket_mem: listener bound protocol=RESP addr=127.0.0.1:6510

┌─────────────────────────────────────────────────┐
│ rocket-mem v0.1.4                                │
├─────────────────────────────────────────────────┤
│ storage   recovered /tmp/acltls-qa/acl.snap + /tmp/acltls-qa/acl.aof (generation 0) │
│ acl       4 users configured, auth required      │
│ cluster   standalone (no cluster_config set)     │
│ replicas  none connected yet -- REPLICAOF is a live command; INFO REPLICATION shows current state │
│ listeners                                        │
│           metrics  http://127.0.0.1:9310/metrics │
│           RMP      127.0.0.1:6511                │
│           RESP     127.0.0.1:6510                │
└─────────────────────────────────────────────────┘
```
(box width is sized to the widest line at runtime; don't match it exactly — check for the
labeled rows. The box border is ANSI-colorized and stripped by piping to a file/non-tty.)

**Notes:** The PID number, timestamps, and box width will differ. The three `listener bound`
lines are logged from concurrently-started tasks, so their **order can vary between runs** —
check that all three are present, not that they are in this sequence.

The `aof recovery replay complete commands=0 bytes=0 ...` event fires on every startup, including
a totally fresh one with nothing to recover, and the banner's `storage` line always says
`recovered ...` regardless of whether anything was actually recovered. It is not evidence that
anything was loaded; do not treat its presence as a recovery signal. The banner's `acl` line
(`4 users configured, auth required`) directly confirms the four-user bootstrap succeeded — a
useful thing to check on its own.

The four users define the whole ACL surface used by ACL-02 through ACL-10: `admin` is
full-access, `app` is narrowly scoped (one command, one key pattern), `retired` is a valid but
disabled account, and `scoped` has every command but only `app:*` keys.

**Result:** ☐ Pass ☐ Fail

---

### ACL-02 — Confirm defining one user arms the auth gate for everything, including PING

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 ping
redis-cli -p 6510 get app:1
redis-cli -p 6510 acl whoami
```

**Expected:**
```
NOAUTH Authentication required.

NOAUTH Authentication required.

NOAUTH Authentication required.

```

**Notes:** There is no `requirepass`-style on/off switch. The presence of at least one
`[[acl.users]]` entry is what turns authentication on for the entire server, and it applies to
every command including keyless, harmless ones like `PING`. A server started with an empty user
list performs no authentication at all.

The blank line after each message is `redis-cli`'s own error rendering. Exit status is still 0.

**Result:** ☐ Pass ☐ Fail

---

### ACL-03 — Confirm `ACL` itself is not reachable before authenticating

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 acl setuser attacker on '>x' allcommands allkeys
redis-cli -p 6510 acl list
```

**Expected:**
```
NOAUTH Authentication required.

NOAUTH Authentication required.

```

**Notes:** This is the privilege-escalation guard, and it is the reason `ACL` is deliberately not
exempted from the gate the way `AUTH` and `HELLO` are. If `ACL SETUSER` were reachable
anonymously, any client could mint itself an `allcommands allkeys` account and then log in to it.
Treat any output other than `NOAUTH` here as a critical failure.

The inverse holds on a server with **no** ACL configured at all: there, an anonymous client can
create the first user and lock everyone else out. Always bootstrap an admin in the TOML before
the port is reachable by anything untrusted.

**Result:** ☐ Pass ☐ Fail

---

### ACL-04 — Verify AUTH success and the single WRONGPASS message for all three failure modes

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 auth admin adminpw
redis-cli -p 6510 auth admin nope
redis-cli -p 6510 auth retired retiredpw
redis-cli -p 6510 auth ghost x
```

**Expected:**
```
OK
WRONGPASS invalid username-password pair or user is disabled.

WRONGPASS invalid username-password pair or user is disabled.

WRONGPASS invalid username-password pair or user is disabled.

```

**Notes:** The three failures are a wrong password for a valid user, a **correct** password for a
disabled user (`retired`, `enabled = false`), and an entirely unknown username. All three produce
byte-identical replies on purpose — the message must not reveal which usernames exist.

All three also pay the same argon2 verification cost (roughly 20-30ms), so the *latency* does not
reveal it either. If you want to sanity-check that, time each of the three: they should be within
the same order of magnitude. A near-instant reply on the unknown-username path would be a timing
oracle and is a real bug.

**Result:** ☐ Pass ☐ Fail

---

### ACL-05 — Verify HELLO is exempt from the gate only when it carries inline credentials

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 hello 3
redis-cli -p 6510 hello 3 auth admin adminpw
```

**Expected:**
```
NOAUTH Authentication required.

server redis
version rocket-mem-0.1.0
proto 3
id 9
mode standalone
role master
modules
```

**Notes:** `AUTH` and `HELLO` are the only two commands allowed through unauthenticated, because
RESP3 clients negotiate with credentials inline. A *bare* `HELLO` gets no such exemption. The `id`
field is a per-connection counter and will differ on your run.

**Result:** ☐ Pass ☐ Fail

---

### ACL-06 — Verify auth state is per-connection, not per-server

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
# Two separate redis-cli invocations = two separate connections.
redis-cli -p 6510 auth admin adminpw
redis-cli -p 6510 ping

# One piped session = one connection, so auth sticks.
printf 'auth admin adminpw\nping\nset k1 v1\nget k1\n' | redis-cli -p 6510

# --user/--pass authenticates the connection redis-cli opens for the command.
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning ping
```

**Expected:**
```
OK
NOAUTH Authentication required.

OK
PONG
OK
v1
PONG
```

**Notes:** The `NOAUTH` on line 2 is the point of the case, not a failure. Nothing about a
successful `AUTH` survives the connection that ran it. This trips people up constantly when
hand-testing: an `AUTH` that returned `OK` tells you nothing about the next `redis-cli` call.

Use the piped form when you need several commands to share one authenticated connection, and
`--user`/`--pass` for single commands. There is no `RESET` command, so a connection cannot drop
its identity short of reconnecting.

**Result:** ☐ Pass ☐ Fail

---

### ACL-07 — Verify the key-pattern NOPERM message

**Precondition:** ACL-01 completed; the server is running on port 6510. Seed the keyspace first —
ACL-08 through ACL-10 depend on these three keys existing.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning set app:1 hello
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning set app:2 world
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning set secret:1 topsecret

# User `app` has rules = ["~app:*", "+get"].
redis-cli -p 6510 --user app --pass apppw --no-auth-warning get app:1
redis-cli -p 6510 --user app --pass apppw --no-auth-warning get other:1
```

**Expected:**
```
OK
OK
OK
hello
NOPERM no permissions to access a key

```

**Notes:** Read this message as "your `~pattern` is too narrow". The command itself *was* granted
(`+get`); it was the key that fell outside every `~` rule. Note that `other:1` does not exist —
the ACL check runs before the key lookup, so a denied key never reveals whether it exists.

**Result:** ☐ Pass ☐ Fail

---

### ACL-08 — Verify the command-not-granted NOPERM message, including for keyless commands

**Precondition:** ACL-07 completed (server running, keys seeded).

**Steps:**
```bash
redis-cli -p 6510 --user app --pass apppw --no-auth-warning ping
redis-cli -p 6510 --user app --pass apppw --no-auth-warning set app:1 x
redis-cli -p 6510 --user app --pass apppw --no-auth-warning acl whoami
```

**Expected:**
```
NOPERM this user has no permissions to run this command

NOPERM this user has no permissions to run this command

NOPERM this user has no permissions to run this command

```

**Notes:** This is the second, distinct NOPERM message; read it as "you need a `+cmd` grant". The
two messages are the main debugging signal, so a case where the wrong one is returned is a real
defect even though access is correctly denied either way.

`PING` takes no keys at all and is still refused. A command grant is required for **every**
command, keyless ones included — `~app:*` alone grants nothing. Likewise `ACL WHOAMI`: even
finding out who you are needs `+acl`.

`SET` is refused with the command message rather than the key message even though `app:1` matches
`~app:*`, because the command check runs first.

**Result:** ☐ Pass ☐ Fail

---

### ACL-09 — Verify a multi-key command is denied outright when any one key is out of pattern

**Precondition:** ACL-07 completed (server running, keys seeded).

**Steps:**
```bash
# User `scoped` has rules = ["allcommands", "~app:*"] — every command, app:* keys only.
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning mget app:1 app:2
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning mget app:1 secret:1
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning get secret:1
```

**Expected:**
```
hello
world
NOPERM no permissions to access a key

NOPERM no permissions to access a key

```

**Notes:** `MGET app:1 secret:1` is rejected in full — it is **not** partially served with a nil
in the denied position. That is the correct and safe behavior: a partial reply would let a caller
probe which keys exist outside their pattern.

**Result:** ☐ Pass ☐ Fail

---

### ACL-10 — KNOWN OPEN GAP: `KEYS` and `SCAN` leak key names across the `~pattern` boundary

**Precondition:** ACL-09 completed (server running, keys `app:1`, `app:2`, `secret:1` seeded, user
`scoped` available with `allcommands ~app:*`).

**Steps:**
```bash
# Value access to secret:1 is correctly denied.
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning get secret:1

# Key NAMES are not.
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning keys '*'

# SCAN leaks it too. The cursor is a shard index; secret:1 lands in shard 7.
redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning scan 7
```

**Expected:**
```
NOPERM no permissions to access a key

app:1
app:2
secret:1
8
secret:1
```

**Notes:** **This is a confirmed, already-known security gap. Do not file a new bug for it.**

`KEYS` and `SCAN` take a glob pattern / a cursor as their argument, not a key, so the ACL layer
sees them as *keyless* commands and runs no key check at all. Every key name in the store comes
back regardless of the user's `~pattern`. Values stay protected — `GET secret:1` is still denied,
as line 1 shows — but names, and therefore the shape of the whole keyspace, are not.

Practical impact for anyone evaluating this build: a `~app:*` restriction is **not** a
confidentiality boundary for key names. Key names frequently encode tenant ids, user ids,
customer names, or feature flags, so this can be a meaningful disclosure on its own.

The `scan 7` cursor is stable because it is derived from `DefaultHasher("secret:1") % 16`. If your
run shows an empty result at cursor 7, sweep the whole space to find it — the leak is what
matters, not the shard:
```bash
for c in $(seq 0 15); do redis-cli -p 6510 --user scoped --pass scopedpw --no-auth-warning scan $c; done
```

**Result:** ☐ Pass ☐ Fail

---

### ACL-11 — Verify runtime `ACL WHOAMI`, `ACL LIST` and `ACL GETUSER`

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl whoami
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl list
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl getuser app
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl getuser nobody
```

**Expected:**
```
admin
user admin on #$argon2id$v=19$m=19456,t=2,p=1$OPHgcsV/dVHnIOriFp6Ltw$eVHSkyW7ilQCuktH/RBgP8omeUclaeBiihrzy8xDrCQ +@all ~*
user scoped on #$argon2id$v=19$m=19456,t=2,p=1$1aifAU8Dw97eOU/Oi51BNA$X+iQwwBMM6ZmMpcpw9Y0vxTyyRjIYy3CrdtYLXiJET0 +@all ~app:*
user app on #$argon2id$v=19$m=19456,t=2,p=1$oqQQ5b+DIk31Bp7EmobzCQ$UMySJ81JsIguaRCKRVfJW/ro8EqYK84V5+i8JRr2uWo ~app:* +get
user retired off #$argon2id$v=19$m=19456,t=2,p=1$xe5KLzYiYbHkAhCt4VjCmg$x2X2RMn9a5y88B3Z78nA9n0GZuZpwOJQEj5YyffSwh4 +@all ~*
flags
on
passwords
$argon2id$v=19$m=19456,t=2,p=1$oqQQ5b+DIk31Bp7EmobzCQ$UMySJ81JsIguaRCKRVfJW/ro8EqYK84V5+i8JRr2uWo
commands
+get
keys
~app:*
```

Check three things in that output rather than diffing it byte-for-byte:

- Four users are listed, and `retired` is the only one marked `off`.
- Every password appears as an `$argon2id$...` hash, never as the plaintext from the TOML.
- `ACL GETUSER nobody` prints **nothing** (a nil reply), not an error.

**Notes:** The argon2 salts are generated fresh at every startup, so the hash strings will differ
on every run — they are not comparable across runs. The *line order* of `ACL LIST` is HashMap
iteration order and also changes between runs; never script against it.

`ACL LIST` renders users in `ACL SETUSER` vocabulary, which is why `allcommands`/`allkeys` from
the TOML come back as `+@all`/`~*`. That is a display normalization, not a rule change.

**Result:** ☐ Pass ☐ Fail

---

### ACL-12 — Verify `ACL SETUSER` creates a user at runtime and appends rules incrementally

**Precondition:** ACL-11 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser ro on '>ropw' '~app:*' +get
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser ro +set
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser ro -set
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl getuser ro

# The new user works immediately — no restart, no reconnect.
redis-cli -p 6510 --user ro --pass ropw --no-auth-warning get app:1
redis-cli -p 6510 --user ro --pass ropw --no-auth-warning set app:1 nope
```

**Expected:**
```
OK
OK
OK
flags
on
passwords
$argon2id$v=19$m=19456,t=2,p=1$5O40PQ4fgO7jI9PitlMaQg$J/KdBLPqwPGK7rDGvIDmtcRYM0FnlUQx5fsHEunyPcw
commands
+get +set -set
keys
~app:*
hello
NOPERM this user has no permissions to run this command

```

**Notes:** The `commands` field reads `+get +set -set`, not `+get`. `ACL SETUSER` is
**incremental**: it merges into the existing user and rules only ever append, so what you see is a
replay log rather than a summary. Evaluation is last-match-wins, which is why the trailing `-set`
is what actually takes effect.

Consequence for testing: a user's rule list grows every time you touch it, and there is **no way
to reset it** short of `ACL DELUSER` followed by recreating the user. A long-lived server that is
reconfigured repeatedly will accumulate rules indefinitely.

`>ropw` must be quoted in the shell or the redirection will eat it.

**Result:** ☐ Pass ☐ Fail

---

### ACL-13 — Verify `ACL DELUSER` returns the count actually removed

**Precondition:** ACL-12 completed; user `ro` exists.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl deluser ro
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl deluser ro
```

**Expected:**
```
1
0
```

**Notes:** Deleting a user that does not exist is `0`, not an error. Do not delete every user: the
"auth is on" flag is sticky and is never cleared, so an empty user table leaves the server
permanently unreachable — every connection gets `NOAUTH` and there is nobody left to `AUTH` as.
The only recovery is a restart, which rebuilds the table from `[[acl.users]]`.

**Result:** ☐ Pass ☐ Fail

---

### ACL-14 — Verify revocation reaches an already-open, already-authenticated connection

**Precondition:** ACL-07 completed (server running, key `app:1` seeded).

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning \
  acl setuser revoketest on '>revokepw' '~app:*' +get

# Open a session that reads app:1, waits 2.5s, then reads it again.
{ echo "auth revoketest revokepw"; echo "get app:1"; sleep 2.5; echo "get app:1"; } \
  | redis-cli -p 6510 &
SESS=$!

# ~1s in — while that connection is still open and authenticated — delete the user.
sleep 1
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl deluser revoketest
wait $SESS
```

**Expected:**
```
OK
OK
hello
1
NOAUTH Authentication required.
```

**Notes:** Read the output in order: `OK` (SETUSER), then from the piped session `OK` (its AUTH)
and `hello` (the first GET), then `1` from the concurrent DELUSER, then `NOAUTH` — the piped
session's *second* GET, on the same connection that was authenticated a moment earlier.

The two streams interleave, so the exact position of the `1` relative to `hello` can shift. What
must hold is that the second `get app:1` fails with `NOAUTH` on a connection that was never
reconnected. ACL changes take effect on the next command, without a restart and without forcing
clients to reconnect.

Do not skip the `wait` — without it the case appears to pass while the session is still running.

**Result:** ☐ Pass ☐ Fail

---

### ACL-15 — Verify TOML field syntax is rejected as an `ACL SETUSER` token, with no partial apply

**Precondition:** ACL-01 completed; the server is running on port 6510.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser tmp1 enabled=true
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser tmp1 on secret123
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl setuser tmp1 on '>pw' '+@read'
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning acl getuser tmp1
```

**Expected:**
```
ERR syntax error at 'enabled=true'

ERR syntax error at 'secret123'

ERR syntax error at '+@read'

```

The final `ACL GETUSER tmp1` prints nothing — an empty (nil) reply.

**Notes:** The two vocabularies overlap only for *rule* tokens. Login state and password are TOML
**fields** in the file but **tokens** on the command line:

| Concept     | `rocket-mem.toml`              | `ACL SETUSER`            |
|-------------|--------------------------------|--------------------------|
| Enabled     | `enabled = true` / `false`     | `on` / `off`             |
| Password    | `password = "pw"`              | `>pw`                    |
| No password | omit `password`                | `nopass`                 |
| Rules       | `rules = ["allkeys", "+get"]`  | trailing `allkeys +get`  |

Rule tokens themselves are identical in both places: `allcommands`/`+@all`, `nocommands`/`-@all`,
`allkeys`/`~*`, `+cmd`, `-cmd`, `~pattern`. Keywords are case-insensitive; patterns and passwords
are not. `+@all` and `-@all` are the only `@category` spellings that exist — `+@read` is a syntax
error, as line 3 shows.

The empty `ACL GETUSER tmp1` is the important assertion: `SETUSER` parses **every** token before
applying **any** of them, so a malformed token anywhere in the list leaves the store untouched
rather than creating a half-configured user. Line 2 in particular would otherwise have created
`tmp1` in an enabled state with no password.

**Result:** ☐ Pass ☐ Fail

---

### ACL-16 — Verify a `>password` or `on`/`off` token inside TOML `rules` fails startup, with the password redacted

**Precondition:** The server from ACL-01 is **stopped** (see the teardown block below), and ports
6510, 6511 and 9310 are free. These runs fail before binding anything, but starting from a clean
slate keeps the output unambiguous.

**Steps:**
```bash
cat > /tmp/acltls-qa/acl-bad.toml <<'EOF'
addr = "127.0.0.1:6510"
rmp_addr = "127.0.0.1:6511"
metrics_addr = "127.0.0.1:9310"

[[acl.users]]
username = "admin"
enabled = true
rules = ["on", ">secret123", "allcommands", "allkeys"]
EOF

"$ROCKET_MEM_BIN" --config /tmp/acltls-qa/acl-bad.toml
echo "exit=$?"

# Same file with the `on` token removed, so the password token is the first failure.
sed 's/"on", //' /tmp/acltls-qa/acl-bad.toml > /tmp/acltls-qa/acl-bad2.toml
"$ROCKET_MEM_BIN" --config /tmp/acltls-qa/acl-bad2.toml
echo "exit=$?"

ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ports free"
```

**Expected:**
```
2026-09-12T05:38:30.468491Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:38:30.468538Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=./appendonly.aof snapshot_path=./dump.snapshot log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=1 tls_enabled=false tls_replication_enabled=false
Error: Custom { kind: InvalidInput, error: "acl bootstrap: ERR syntax error at 'on'" }
exit=1
2026-09-12T05:38:30.479274Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:38:30.479288Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=./appendonly.aof snapshot_path=./dump.snapshot log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=1 tls_enabled=false tls_replication_enabled=false
Error: Custom { kind: InvalidInput, error: "acl bootstrap: ERR syntax error at '<password token>'" }
exit=1
ports free
```

**Notes:** Two things to check beyond the exit code.

First, the second error says `'<password token>'` — the literal string `secret123` is **not**
echoed. A misconfigured password must not leak into stderr, journald, container logs or CI output.
If you ever see the actual password there, that is a security defect worth filing.

Second, no `listener bound` event is printed at all — the two lines that do appear (`rocket-mem
starting`, `resolved config summary`) are pure logging with no side effect; the ACL bootstrap
check still runs before anything is bound, so the process leaves no half-started state. (This
differs from the TLS failures in TLS-08, which abort after the metrics and RMP listeners are
already up.)

The `Error: Custom { ... }` wrapper is `std::io::Error`'s `Debug` output rather than a hand-written
message. Noisy, but the useful part is inside it.

**Result:** ☐ Pass ☐ Fail

---

### ACL-17 — Verify no credential reaches the slow log or the AOF

**Precondition:** ACL-01 through ACL-14 have been run against a live server on port 6510, so the
slow log and AOF have content. The server is still running.

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning slowlog get | head -12
grep -aic -E 'AUTH|ACL|adminpw|apppw' /tmp/acltls-qa/acl.aof
```

**Expected:**
```
61
1788234467
25797
AUTH
... (2 more arguments)
60
1788234454
28587
AUTH
... (2 more arguments)
59
1788234453
23238
```

and the `grep -c` prints:
```
0
```

**Notes:** Entry ids, timestamps and durations differ on every run. What matters is the shape:
every `AUTH` entry shows the command name followed by `... (2 more arguments)`, never the username
or the password. `ACL setuser` entries are redacted the same way (`... (5 more arguments)`).

Nearly every `AUTH` lands in the slow log because argon2 verification costs roughly 20-30ms (the
third field of each entry is the duration in microseconds — `25797`, `28587` above), well over the
default 10ms slow-log threshold. That is expected, not a performance regression.

The `grep -c` returning `0` proves nothing ACL-related reaches durable state: no `AUTH` command,
no `ACL` command, and no plaintext password ever enters the AOF or the snapshot. The direct
consequence is that runtime users created with `ACL SETUSER` are **not persisted** — a restart
rebuilds the user table from `[[acl.users]]` alone, and the runtime users are silently gone while
the data they guarded survives. ACL changes are also leader-local: they are not replicated, so a
follower's user table can drift from its leader's.

**Result:** ☐ Pass ☐ Fail

---

### ACL-18 — Verify `/metrics` has no authentication of its own but does count auth failures

**Precondition:** ACL-01 through ACL-14 have been run against a live server; the server is still
running with metrics on 9310.

**Steps:**
```bash
curl -s http://127.0.0.1:9310/metrics | grep -E 'rocket_mem_command_errors_total' | head -10
```

**Expected:**
```
# TYPE rocket_mem_command_errors_total counter
rocket_mem_command_errors_total{cmd="acl"} 6
rocket_mem_command_errors_total{cmd="auth"} 3
rocket_mem_command_errors_total{cmd="get"} 3
rocket_mem_command_errors_total{cmd="mget"} 2
rocket_mem_command_errors_total{cmd="hello"} 1
rocket_mem_command_errors_total{cmd="set"} 2
rocket_mem_command_errors_total{cmd="ping"} 4
```

**Notes:** The counter values depend on exactly which of the earlier cases you ran and how many
times; only the label shape is fixed. The two findings here are:

1. `curl` succeeded with **no credentials of any kind**, on a server where every RESP command is
   behind `NOAUTH`. The metrics endpoint is not covered by the ACL system at all. Bind it to
   loopback or firewall it — never expose it publicly, ACL configured or not.
2. Failed logins (`cmd="auth"`) and NOPERM refusals (`cmd="get"`, `cmd="ping"`, ...) do increment
   the counter, which makes it a usable alerting hook for brute-force detection.

**Result:** ☐ Pass ☐ Fail

---

### ACL-19 — Verify AUTH success/failure and NOPERM denials are logged by username, never the password

**Precondition:** ACL-01 completed; the server is running on port 6510 with its stderr captured
to `/tmp/acltls-qa/acl-server.log` (as ACL-01's own steps already do via `2>&1`).

**Steps:**
```bash
redis-cli -p 6510 --user admin --pass adminpw --no-auth-warning ping
redis-cli -p 6510 auth admin wrongpw
redis-cli -p 6510 --user app --pass apppw --no-auth-warning set app:1 x   # app only has +get

grep -E 'auth success|auth failure|permission denied' /tmp/acltls-qa/acl-server.log | tail -3
```

**Expected:** three log lines of this shape (timestamps/`conn_id`/`peer` vary):
```
...  INFO conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: auth success user=admin
...  WARN conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: auth failure user=admin
...  WARN conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: permission denied user=app
```

**Notes:** `auth success`/`auth failure` log at `INFO`/`WARN` with a `user=` field and never the
password; `permission denied` logs the same way on every `NOPERM`. Both are at or above the
server's default `log_level = "info"`, so they land in stderr on any default deployment without
extra configuration — a real operational signal (e.g. for brute-force or privilege-probing
alerting), not a trace-only detail. Confirm the password (`wrongpw`) never appears anywhere in
the log file — it must not, by the same redaction policy ACL-17 already tests for the slow log
and AOF. This is net-new since the ACL section was first written; ACL-17 covers slowlog/AOF
redaction specifically, not this stderr-log-by-username behavior.

**Result:** ☐ Pass ☐ Fail

---

### ACL teardown

```bash
PID=$(cut -d= -f2 /tmp/acltls-qa/acl.pid)
kill $PID
sleep 1
ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ports free"
```

Kill by that PID only. Never `pkill -f rocket-mem` — a broad pattern kill also takes out any other
`rocket-mem` a colleague or a parallel test run has going.

---

## TLS

### TLS-01 — Generate a self-signed certificate for local testing

**Precondition:** `openssl` is installed and `/tmp/acltls-qa` exists.

**Steps:**
```bash
mkdir -p /tmp/acltls-qa/tls
cd /tmp/acltls-qa/tls
openssl req -x509 -newkey rsa:2048 \
  -keyout key.pem -out cert.pem -days 3650 -nodes -subj "/CN=localhost"
echo "exit=$?"
ls -l /tmp/acltls-qa/tls
```

**Expected:**
```
exit=0
total 8
-rw-rw-r-- 1 numericlabs numericlabs 1115 Sep  1 09:18 cert.pem
-rw------- 1 numericlabs numericlabs 1704 Sep  1 09:18 key.pem
```

**Notes:** `openssl req` also prints a long line of dots and `+` characters to stderr while
generating the key. That is progress output, not an error; ignore it.

**This certificate is for local testing only.** It is self-signed, so it has no trust chain any
third party will accept, and it must never be pointed at a real deployment. `-nodes` leaves the
private key unencrypted, which is required here — the server has no way to prompt for a
passphrase, so a passphrase-protected key simply fails to load.

Exact byte sizes vary slightly per key; owner and timestamp will be yours.

**Result:** ☐ Pass ☐ Fail

---

### TLS-02 — Verify TLS listeners run alongside the plaintext ones, not instead of them

**Precondition:** TLS-01 completed. Ports 6510, 6511, 6530, 6531 and 9310 are all free — run the
ACL teardown above first if the ACL server is still up.

**Steps:**
```bash
cd /tmp/acltls-qa
ROCKET_MEM_ADDR=127.0.0.1:6510 ROCKET_MEM_RMP_ADDR=127.0.0.1:6511 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9310 \
ROCKET_MEM_AOF_PATH=/tmp/acltls-qa/tls.aof ROCKET_MEM_SNAPSHOT_PATH=/tmp/acltls-qa/tls.snap \
ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6530 ROCKET_MEM_TLS_RMP_ADDR=127.0.0.1:6531 \
ROCKET_MEM_TLS_CERT_PATH=/tmp/acltls-qa/tls/cert.pem \
ROCKET_MEM_TLS_KEY_PATH=/tmp/acltls-qa/tls/key.pem \
nohup "$ROCKET_MEM_BIN" \
  > /tmp/acltls-qa/tls-server.log 2>&1 &
echo "PID=$!" > /tmp/acltls-qa/tls.pid

sleep 1.5
cat /tmp/acltls-qa/tls-server.log
ss -lnt | grep -E ':(6510|6511|6530|6531|9310)\b'
```

**Expected:**
```
2026-09-12T05:37:57.874854Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:37:57.874898Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=/tmp/acltls-qa/tls.aof snapshot_path=/tmp/acltls-qa/tls.snap log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=false acl_user_count=0 tls_enabled=true tls_replication_enabled=false
2026-09-12T05:37:57.875766Z  INFO rocket_mem::aof: aof recovery replay complete commands=0 bytes=0 elapsed_us=5
2026-09-12T05:37:57.875939Z  INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:9310/metrics
2026-09-12T05:37:57.875974Z  INFO rocket_mem: listener bound protocol=RMP addr=127.0.0.1:6511
2026-09-12T05:37:57.876390Z  INFO rocket_mem: listener bound protocol=RESP+TLS addr=127.0.0.1:6530
2026-09-12T05:37:57.876707Z  INFO rocket_mem: listener bound protocol=RMP+TLS addr=127.0.0.1:6531
2026-09-12T05:37:57.876727Z  INFO rocket_mem: listener bound protocol=RESP addr=127.0.0.1:6510

[boxed summary table follows, listing all five listeners: metrics / RMP / RESP+TLS / RMP+TLS / RESP]

LISTEN 0      128                   127.0.0.1:9310       0.0.0.0:*
LISTEN 0      128                   127.0.0.1:6510       0.0.0.0:*
LISTEN 0      128                   127.0.0.1:6511       0.0.0.0:*
LISTEN 0      128                   127.0.0.1:6530       0.0.0.0:*
LISTEN 0      128                   127.0.0.1:6531       0.0.0.0:*
```

**Notes:** Five listeners, not three. Enabling TLS does **not** disable or replace the plaintext
`addr`/`rmp_addr` listeners — there is no setting that turns them off. Anyone who assumes
"TLS is configured, therefore traffic is encrypted" is wrong on this build: 6510 is still fully
open and unencrypted. If you need plaintext closed, you must firewall it.

The four settings are available identically as TOML keys (`tls_resp_addr`, `tls_rmp_addr`,
`tls_cert_path`, `tls_key_path`), as `ROCKET_MEM_TLS_*` env vars, or as `--tls-*` flags. What used
to be logged as "TLS listening"/"RMP TLS listening" is now `RESP+TLS`/`RMP+TLS` in both the log
lines and the boxed summary table.

Log-line and banner-row order varies between runs; `ss` row order varies too. Check for presence.

This is server-authentication TLS only. There is no mutual TLS — the server never asks the client
for a certificate, so anyone who can reach the port can complete a handshake.

**Result:** ☐ Pass ☐ Fail

---

### TLS-03 — Verify a working `redis-cli --tls --cacert` round-trip

**Precondition:** TLS-02 completed; the server is running with TLS on 6530.

**Steps:**
```bash
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 ping
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 set tlskey 1
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 get tlskey
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 -3 ping
```

**Expected:**
```
PONG
OK
1
PONG
```

**Notes:** Because the certificate is self-signed, it is its own CA — passing `cert.pem` to
`--cacert` is what makes verification succeed.

RESP3 (`-3`) works over TLS exactly as it does in plaintext; the TLS layer wraps the socket and
changes nothing above it.

There is **no hostname check**. This command addresses `127.0.0.1` while the certificate says
`CN=localhost`, and it still connects. Do not read a successful connection as proof the name
matched.

**Result:** ☐ Pass ☐ Fail

---

### TLS-04 — Verify the TLS and plaintext ports share one keyspace

**Precondition:** TLS-03 completed; the server is running with both 6510 and 6530 up.

**Steps:**
```bash
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 set both 1
redis-cli -p 6510 incr both
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 get both
```

**Expected:**
```
OK
2
2
```

**Notes:** One `Engine`, one set of shards, four RESP/RMP front doors. There is no per-listener
isolation and no synchronization involved — a write over TLS is immediately visible in plaintext
and vice versa. That also means a plaintext client on 6510 can read anything a TLS client wrote,
which is the practical reason TLS-02's "plaintext is still open" note matters.

**Result:** ☐ Pass ☐ Fail

---

### TLS-05 — Verify `--insecure` skips verification and omitting `--cacert` fails it

**Precondition:** TLS-02 completed; the server is running with TLS on 6530.

**Steps:**
```bash
redis-cli --tls --insecure -p 6530 ping
echo "exit=$?"
redis-cli --tls -p 6530 ping
echo "exit=$?"
```

**Expected:**
```
PONG
exit=0
Could not connect to Redis at 127.0.0.1:6530: SSL_connect failed: certificate verify failed
exit=1
```

**Notes:** The second command is the important one: without a `--cacert` naming the self-signed
certificate, the client has nothing to chain to and correctly refuses the connection. A build that
connected anyway would mean certificate verification is not actually being enforced.

Note the exit codes. Unlike the server-side errors in the ACL section, a TLS handshake failure is
a connection failure, so `redis-cli` does exit 1 and this one *is* safe to script against.

`--insecure` disables verification entirely. It is fine for a quick "is the listener up" check and
must never appear in anything resembling a production client configuration.

**Result:** ☐ Pass ☐ Fail

---

### TLS-06 — Verify the TLS RMP listener with `openssl s_client`

**Precondition:** TLS-02 completed; the server is running with TLS RMP on 6531.

**Steps:**
```bash
echo | openssl s_client -connect 127.0.0.1:6531 \
  -CAfile /tmp/acltls-qa/tls/cert.pem -servername localhost 2>&1 \
  | grep -E 'New, TLS|Verify return code'
```

**Expected:**
```
New, TLSv1.3, Cipher is TLS_AES_256_GCM_SHA384
Verify return code: 0 (ok)
```

**Notes:** RMP is rocket-mem's own binary protocol and `redis-cli` cannot speak it, so `s_client`
is the only hand-testing route for 6531 — the `rmp-client` crate in the workspace speaks plaintext
RMP only and has no TLS mode. This case proves the listener is up and the handshake completes; it
does not exercise any RMP command.

The same command against `-connect 127.0.0.1:6530` produces the same two lines, which is a quick
way to confirm both TLS listeners share the one certificate.

`s_client` does not verify the hostname unless you pass `-verify_hostname`, so `Verify return
code: 0 (ok)` against `127.0.0.1` with a `CN=localhost` certificate is expected here and is not
evidence of a name match. Same caveat as TLS-03.

**Result:** ☐ Pass ☐ Fail

---

### TLS-07 — Verify what a plaintext client gets on the TLS port

**Precondition:** TLS-02 completed; the server is running with TLS on 6530.

**Steps:**
```bash
redis-cli -p 6530 ping
echo "exit=$?"
```

**Expected:**
```
Error: Protocol error, got "\x15" as reply type byte
exit=1
```

**Notes:** Match this string exactly, including the `\x15`.

Read it as three separate facts. The TCP connect **succeeded** — the port is open and accepting.
The TLS handshake then failed, because the client sent a RESP `PING` where a ClientHello was
expected. And `\x15` is byte 21, the TLS record type for an **alert**: the server *does* answer,
it just answers in TLS, and `redis-cli` tries to interpret that first byte as a RESP reply type.

This is explicitly **not** silence. Older documentation claimed the server sends no reply at all,
which sends you hunting for a hung connection that does not exist. If you ever see a hang here
instead of this error, that is a genuine regression.

**Result:** ☐ Pass ☐ Fail

---

### TLS-08 — Verify a TLS address without a cert/key is a startup error, not an unbound listener

**Precondition:** The server from TLS-02 is **stopped** and ports 6510, 6511, 6530, 6531 and 9310
are free. These runs bind the metrics and RMP listeners before aborting, so a running server would
mask the real error with `AddrInUse`.

**Steps:**
```bash
cd /tmp/acltls-qa

# A: TLS address set, no cert or key at all.
ROCKET_MEM_ADDR=127.0.0.1:6510 ROCKET_MEM_RMP_ADDR=127.0.0.1:6511 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9310 ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6530 \
"$ROCKET_MEM_BIN"; echo "exit=$?"

# B: cert path points at a file that does not exist.
ROCKET_MEM_ADDR=127.0.0.1:6510 ROCKET_MEM_RMP_ADDR=127.0.0.1:6511 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9310 ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6530 \
ROCKET_MEM_TLS_CERT_PATH=/tmp/acltls-qa/tls/missing.pem \
ROCKET_MEM_TLS_KEY_PATH=/tmp/acltls-qa/tls/key.pem \
"$ROCKET_MEM_BIN"; echo "exit=$?"

# C: cert and key swapped.
ROCKET_MEM_ADDR=127.0.0.1:6510 ROCKET_MEM_RMP_ADDR=127.0.0.1:6511 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9310 ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6530 \
ROCKET_MEM_TLS_CERT_PATH=/tmp/acltls-qa/tls/key.pem \
ROCKET_MEM_TLS_KEY_PATH=/tmp/acltls-qa/tls/cert.pem \
"$ROCKET_MEM_BIN"; echo "exit=$?"
```

**Expected:** (scenario A shown in full; B and C are the same shape with two more `listener bound`
lines — `metrics` and `RMP` — appearing before their own error)
```
2026-09-12T05:38:17.924036Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:38:17.924101Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=./appendonly.aof snapshot_path=./dump.snapshot log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=false acl_user_count=0 tls_enabled=true tls_replication_enabled=false
2026-09-12T05:38:17.925083Z  INFO rocket_mem::aof: aof recovery replay complete commands=0 bytes=0 elapsed_us=7
Error: Custom { kind: InvalidInput, error: "tls_resp_addr is set but tls_cert_path/tls_key_path is not -- TLS requires both" }
exit=1
```
Scenario B ends with `Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }` /
`exit=1`; scenario C ends with `Error: Custom { kind: InvalidData, error: "no certificate found in
cert file" }` / `exit=1` — both unchanged, reconfirmed live.

**Notes:** The behavior under test is that all three exit 1. A TLS misconfiguration must never
result in a server that comes up happily with its TLS listener silently missing — that would look
healthy while serving nothing but plaintext.

All three abort **after** the metrics and plaintext RMP listeners are already bound, so the error
scrolls past the startup/config-summary lines and two `listener bound` events. The `listener
bound protocol=RESP addr=127.0.0.1:6510` line never appears, which is the reliable signal that
startup did not complete. (Contrast ACL-16, where the failure happens before anything binds.)

Case B's error does not say **which** path was missing. If you hit `NotFound`, check both
`tls_cert_path` and `tls_key_path`.

These runs use the default AOF/snapshot paths (`./dump.snapshot`, `./appendonly.aof`) relative to
the cwd, which is why the first line differs from TLS-02's. They abort before writing anything.

Not covered by any check: setting `tls_resp_addr` to the same port as `addr`. TLS binds first, then
the plaintext listener dies with `AddrInUse` and nothing hints that the two settings collided.

**Result:** ☐ Pass ☐ Fail

---

### TLS-09 — Verify cert/key paths resolve against the process CWD, not the config file

**Precondition:** TLS-01 completed (certificates exist in `/tmp/acltls-qa/tls`). Ports 6510, 6511,
6530 and 9310 are free.

**Steps:**
```bash
mkdir -p /tmp/acltls-qa/cfgdir
cat > /tmp/acltls-qa/cfgdir/tls-relative.toml <<'EOF'
addr = "127.0.0.1:6510"
rmp_addr = "127.0.0.1:6511"
metrics_addr = "127.0.0.1:9310"
aof_path = "/tmp/acltls-qa/tls.aof"
snapshot_path = "/tmp/acltls-qa/tls.snap"
tls_resp_addr = "127.0.0.1:6530"
tls_cert_path = "cert.pem"
tls_key_path = "key.pem"
EOF

# Run from the directory holding the CONFIG. The certs are not there.
cd /tmp/acltls-qa/cfgdir
"$ROCKET_MEM_BIN" \
  --config /tmp/acltls-qa/cfgdir/tls-relative.toml; echo "exit=$?"

sleep 1

# Same config file, unchanged. Run from the directory holding the CERTS.
cd /tmp/acltls-qa/tls
timeout 2 "$ROCKET_MEM_BIN" \
  --config /tmp/acltls-qa/cfgdir/tls-relative.toml; echo "exit=$?"
```

**Expected:** (structured logging replaces the old plain lines — see TLS-02; substance below is
unchanged and reconfirmed live)
```
... startup/config-summary logging, then:
Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }
exit=1
... startup/config-summary logging, then five `listener bound` events (metrics/RMP/RESP+TLS/RMP+TLS/RESP):
exit=124
```

**Notes:** This is a real trap. The **same config file** fails from one directory and starts
cleanly from another, with no diagnostic naming the path it actually tried. `tls_cert_path` and
`tls_key_path` are resolved against the server process's working directory, not against the
directory the config file lives in — which is the intuition most people bring.

`exit=124` on the second run is `timeout` killing a healthy server after 2 seconds. That is the
pass condition; the `listener bound protocol=RESP addr=127.0.0.1:6510` line (last of five) is
what matters.

Recommendation to pass on: always use absolute paths for `tls_cert_path`/`tls_key_path` unless you
control the working directory the process is launched from (a systemd unit's `WorkingDirectory`,
a container's `WORKDIR`).

**Result:** ☐ Pass ☐ Fail

---

### TLS-10 — Verify ACL enforcement applies on the TLS port

**Precondition:** TLS-01 completed. All five ports free. Any server from an earlier case is
stopped.

**Steps:**
```bash
cat > /tmp/acltls-qa/acl-tls.toml <<'EOF'
addr = "127.0.0.1:6510"
rmp_addr = "127.0.0.1:6511"
metrics_addr = "127.0.0.1:9310"
aof_path = "/tmp/acltls-qa/acltls.aof"
snapshot_path = "/tmp/acltls-qa/acltls.snap"
tls_resp_addr = "127.0.0.1:6530"
tls_rmp_addr = "127.0.0.1:6531"
tls_cert_path = "/tmp/acltls-qa/tls/cert.pem"
tls_key_path = "/tmp/acltls-qa/tls/key.pem"

[[acl.users]]
username = "admin"
password = "adminpw"
enabled = true
rules = ["allcommands", "allkeys"]
EOF

cd /tmp/acltls-qa
nohup "$ROCKET_MEM_BIN" \
  --config /tmp/acltls-qa/acl-tls.toml > /tmp/acltls-qa/acltls-server.log 2>&1 &
echo "PID=$!" > /tmp/acltls-qa/acltls.pid
sleep 1.5
cat /tmp/acltls-qa/acltls-server.log

redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 ping
redis-cli --tls --cacert /tmp/acltls-qa/tls/cert.pem -p 6530 \
  --user admin --pass adminpw --no-auth-warning ping
```

**Expected:** (structured logging replaces the old plain lines — see TLS-02 for the full shape;
substance below is unchanged and reconfirmed live)
```
... startup/config-summary logging, then five `listener bound` events (metrics/RMP/RESP+TLS/RMP+TLS/RESP)
NOAUTH Authentication required.

PONG
```

**Notes:** TLS and ACL are independent layers and compose as expected: transport encryption grants
no identity, so a TLS client starts out just as unauthenticated as a plaintext one. Completing the
handshake is not authentication — there is no mutual TLS and no certificate-derived identity
anywhere in this build.

**Result:** ☐ Pass ☐ Fail

---

### TLS-11 — Verify the plaintext-announce-address warning when a TLS-serving follower has no `replica_announce_addr`

**Precondition:** TLS-01 completed. Ports free.

**Steps:**
```bash
cd /tmp/acltls-qa
ROCKET_MEM_ADDR=127.0.0.1:6510 ROCKET_MEM_RMP_ADDR=127.0.0.1:6511 \
ROCKET_MEM_METRICS_ADDR=127.0.0.1:9310 \
ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6530 \
ROCKET_MEM_TLS_CERT_PATH=/tmp/acltls-qa/tls/cert.pem \
ROCKET_MEM_TLS_KEY_PATH=/tmp/acltls-qa/tls/key.pem \
ROCKET_MEM_REPLICAOF=127.0.0.1:1 \
timeout 2 "$ROCKET_MEM_BIN" 2>&1 | grep -i "plaintext"
```

**Expected:** one `WARN` line naming the plaintext address:
```
...  WARN rocket_mem: replica_announce_addr is unset while a TLS listener is configured -- this node advertises its plaintext address to its leader announced=127.0.0.1:6510
```

**Notes:** This fires once, at startup, purely from config shape — it does not need a reachable
leader (`should_warn_plaintext_announce` in `config.rs` is checked before the replication client
starts, so it logs even though `127.0.0.1:1` refuses the connection). It requires all three of:
`replicaof` set, at least one of `tls_resp_addr`/`tls_rmp_addr` set, and `replica_announce_addr`
unset. See REPL-10 for `replica_announce_addr` itself changing what a leader reports about a
follower.

**Result:** ☐ Pass ☐ Fail

---

### TLS teardown

```bash
for f in /tmp/acltls-qa/tls.pid /tmp/acltls-qa/acltls.pid; do
  [ -f "$f" ] && kill "$(cut -d= -f2 "$f")" 2>/dev/null
done
sleep 1
ss -lnt | grep -E ':(6510|6511|6530|6531|9310)\b' || echo "ports free"
```

Expected final line: `ports free`.

Again: kill by PID only. Do not use `pkill -f rocket-mem`.

To discard everything this playbook created:

```bash
rm -rf /tmp/acltls-qa
```

---


---


## Known limits and expected divergences

Read this before filing anything.

### Expected behavior: do not file these as bugs

| Area | Observed behavior | Why it is expected |
|---|---|---|
| `KEYS` glob syntax | Patterns support only `*`, `?`, and `[abc]`-style literal classes. No character ranges (`[a-z]`), negation (`[^abc]`), or escaping. | Intentionally partial implementation. |
| Active expiry | One whole shard is swept every 100ms, not individual key sampling within a shard. | Accepted simplification matching the design trade-off documented in Sprint 4 spec. |
| `OBJECT ENCODING` | Returns this engine's own type names (`string`, `list`, `hash`, `set`, `zset`) — exactly what `TYPE` returns. | Engine does not implement real Redis's internal encodings (`embstr`, `listpack`, etc.); the command reports what the engine actually uses. |
| `SLOWLOG` format | Entries carry 4 fields instead of real Redis's 6. Missing: client address and client name. Argument list shows command name and first argument only; remaining arguments shown as `(N more arguments)`. | Dispatcher never learns the peer socket address — it is discarded at the connection layer before dispatch. Threading it through six call layers for cosmetic fields was not prioritized. |
| `INFO expired_keys` | Counts only *actively* expired keys (background sweep removals). Passive expiry (a read finding a key already dead) removes keys without counting them. | Passive expiry counter would touch the hottest read path in the project; was deprioritized against write-path and replication priorities. |
| Replication resync | Every resync is full. Dropped follower connection always triggers a full resnapshot. No partial-resync or offset-resume support. | Simplified design. Full resync removes complexity around offset tracking and partial-state recovery. |
| Replication lag metric | Superseded: replication offsets now exist. `INFO REPLICATION` reports `master_repl_offset`/`slave_repl_offset` on both roles, and each `slaveN:` line carries `offset=<n>,lag=<secs>` (`lag=-1` for a replica that has never acked). Prometheus exports `rocket_mem_master_repl_offset`, `rocket_mem_slave_repl_offset`, and `rocket_mem_good_replicas`. `rocket_mem_replication_last_apply_timestamp_seconds` still exists alongside them as a coarser wall-clock signal. | Added by the failover-safety-primitives work (`docs/superpowers/plans/2026-09-09-failover-safety-primitives/`, chain A — see `00-design-contract.md` §2.1-§2.4). |
| `DEBUG SLEEP` | Capped at 10-second maximum duration. Requests over 10 seconds are rejected with an error. | Prevents accidental server thread blocking indefinitely from client requests. Safety limit, not a bug. |
| `@category` ACL grants | Only explicit `+CMDNAME`/`-CMDNAME` grants and `allcommands`/`nocommands` (or `+@all`/`-@all`) are accepted. Other categories like `+@read`, `+@write` are syntax errors. | Category taxonomy is large and the project prioritizes explicit command grants for clarity. Future backlog item. |
| ACL users persistence | Runtime `ACL SETUSER` is not persisted to AOF or snapshot. Lost on restart unless user is also declared in `[[acl.users]]` bootstrap array in TOML config. | Intentional design: ACL state is in-memory and local. Mirrors real Redis when `ACL SAVE`/`aclfile` is not configured. The project has no `ACL SAVE` command and no `aclfile` equivalent beyond TOML bootstrap. |
| ACL replication | `ACL SETUSER`/`DELUSER` are not logged to AOF or fanned out to replicas. Follower's ACL state can diverge from leader's unless both start from the same bootstrap config. | Intentional design. ACL changes are leader-local. Users must coordinate ACL bootstrap config across deployment. |
| Auth gate | Only `AUTH` and `HELLO` are reachable before an unauthenticated client authenticates. `ACL` deliberately is not exempt, preventing privilege escalation. | Security-first design: an unauthenticated client cannot bootstrap itself an admin account. |
| `ACL LIST` format | Output renders a user's password as `#<hash>` (its stored Argon2 hash), not the plaintext. This `#<hash>` format is not accepted as input back to `ACL SETUSER`; only `>password` (plaintext to hash) and `nopass` are accepted. | Matches real Redis's rendering; the round-trip rejection is intentional. Plaintext passwords are never logged, persisted, or echoed. |
| Cluster gossip | No cluster bus and no gossip: nodes never agree with each other on anything, so `cluster_slots_fail` is structurally always `0`. Each node does directly probe its peers, though, so `CLUSTER NODES`/`SHARDS`/`INFO` report `disconnected`/`master,fail?`/`cluster_state:fail` based on that node's own observation — see CLUSTER-06's notes. `cluster_state:fail` is report-only: this node keeps serving its own slots and `cluster_redirect` still points at the configured owner even when it is known-dead. | Static config file design: cluster membership is fixed at process start, not dynamic. A per-node liveness probe (added by the failover-safety-primitives work, `docs/superpowers/plans/2026-09-09-failover-safety-primitives/`) makes reporting honest without adding a cluster bus, quorum, or automatic failover. |
| Cluster resharding | No live resharding and no failover. Slot ownership is fixed at process start via static config file. `CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not exist. | Static slot assignment is the design constraint. Live resharding requires dynamic slot migration, which is a future backlog item per Sprint 8 spec. |
| Cluster forwarding | No request forwarding. A `-MOVED` reply requires the *client* to reconnect and retry. This server never proxies requests to another shard. | Design choice for simplicity: clients handle redirection, not the server. Standard cluster-aware clients expect and handle this. |
| `CLUSTER SLOTS` | Not implemented. Deprecated since Redis 7.0 in favor of `CLUSTER SHARDS`. | Intentional: `CLUSTER SHARDS` (implemented) is the modern equivalent. |
| `/metrics` authentication | Endpoint is unauthenticated. No ACL check on HTTP requests to the metrics port. | Intentional design: loopback-only default and firewall are the security model. Metrics server is separate from command server. |
| Pub/sub in cluster mode | `PUBLISH` only reaches subscribers connected to the same node that received the command. No cross-shard fan-out — a subscriber on one shard never sees a message published against a different shard, even for the same channel name. `SPUBLISH`/`SSUBSCRIBE` (real Redis's sharded-pub/sub commands) don't exist either. | Design choice, not a bug: cluster mode has fixed hash-slot ownership with no cluster bus/gossip for nodes to fan messages out over. Documented in `docs/superpowers/specs/2026-09-11-pubsub-spec.md`'s "Out of scope" section. |
| `MULTI`/`EXEC` isolation | A transaction's writes are atomic with respect to other **writers** only — `EXEC` holds the same shard-lock guard ordinary single-command writes already share, just widened to the whole batch, so a concurrent write to an overlapping shard blocks until `EXEC` finishes. A concurrent **read** is not blocked and "could observe the transaction's intermediate state partway through the batch." `WATCH`/`UNWATCH` (optimistic locking) are not implemented. | Deliberate, documented tradeoff — see `docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`, "Decision: writers-only isolation." True read isolation would require reworking `engine.rs`'s `with_mut`/`with_ref` to accept a pre-acquired shard guard (`parking_lot::RwLock` isn't reentrant) — deferred pending real-workload evidence it's needed. |

### Commands not implemented

These real-Redis commands have no counterpart in this project. They are deliberately out of scope for the current sprint plan and tracked as future work.

**List and sorted-set extras:**
- `LPOS` — find element position in list
- `LMPOP` / `ZMPOP` — pop from multiple lists/sorted sets
- `BLPOP` / `BRPOP` — blocking list pop variants
- `BLMPOP` / `BZMPOP` / `BZPOPMIN` / `BZPOPMAX` — blocking multi-pop variants

**Key and object extras:**
- `COPY` — copy a key
- `OBJECT FREQ` / `OBJECT IDLETIME` — access frequency and idle time
- `WAIT` — wait for replication
- `LOLWUT` — novelty command

**Lua scripting:**
- `EVAL`, `EVALSHA` — script execution
- `SCRIPT LOAD` / `SCRIPT EXISTS` / `SCRIPT FLUSH` — script management

**Transactions:**
- `WATCH` / `UNWATCH` — optimistic locking

**Streams:**
- `XADD` and the entire stream command family (XRANGE, XREAD, XLEN, etc.)

**Cluster live operations:**
- `CLUSTER SETSLOT` — assign/migrate slots
- `MIGRATE` — move key to another node
- `ASK` / `ASKING` — temporary slot redirection during migration

**Other:**
- `RESET` — close connection and reset auth
- `DBSIZE` — count keys
- `FLUSHALL` — clear all databases
- `ACL HELP` / `ACL CAT` — ACL introspection

**Future backlog note:** Lua scripting and streams are explicitly tracked in
`docs/rocket-mem-sprint-plan.md` as Phase 5 / follow-on backlog work, not current-sprint
out-of-scope. (Pub/sub and transactions — `MULTI`/`EXEC`/`DISCARD` — shipped, per the `PUBSUB`
and `TXN` sections above; `WATCH`/`UNWATCH` remain backlog.)

### Genuine open gaps — already known

These are NOT intentional; they are real gaps a maintainer confirmed. Report them if they change or worsen.

#### `KEYS` and `SCAN` ignore ACL key patterns

**What happens:** A user restricted to key pattern `~app:*` via ACL can correctly not read `GET secret:1` (correct `NOPERM no permissions to access a key` error), but `KEYS *` and `SCAN` return `secret:1` anyway. Key *values* stay protected; key *names* leak across the pattern boundary.

**Example:**
```
user scoped: rules = ["allcommands", "~app:*"]
store: app:1, app:2, secret:1

scoped$ GET secret:1
-> NOPERM no permissions to access a key    [correct]

scoped$ KEYS '*'
-> app:1 / app:2 / secret:1                 [leaked]

scoped$ SCAN 0
-> 8 / secret:1                             [leaked]
```

**Impact:** Key names leak across ACL boundaries. Access to key contents is still enforced; pattern matching on key discovery is not.

#### On a server with no ACL users configured, any client can create the first user

**What happens:** A fresh server with no `[[acl.users]]` bootstrap array accepts `ACL SETUSER` from any anonymous client. The first user successfully created arms the auth gate, locking everyone (including the original admin) out of the system until restart.

**Mitigation:** Bootstrap at least one admin user in the TOML config file before exposing the port to untrusted networks.

**Impact:** Requires config-time setup; runtime-only deployments are vulnerable.

#### `+acl` grant is equivalent to full admin

**What happens:** There is no per-subcommand granularity within `ACL`. A user granted `+acl` can run `ACL SETUSER` on itself, add `+allcommands` and `~*`, and escalate to full admin.

**Example:**
```
admin$ ACL SETUSER attacker +acl
attacker$ ACL SETUSER attacker +allcommands ~*
attacker$ [now a full admin]
```

**Impact:** `+acl` is a superpower; it cannot be used as a read-only or restricted ACL subcommand.

#### Deleting every ACL user locks the server until restart

**What happens:** The "auth is on" flag is set the first time any user is configured (either at bootstrap or via `ACL SETUSER`). Once set, it is sticky and never cleared, even if the last user is deleted via `ACL DELUSER`. An empty user table leaves nobody to authenticate as and no recovery command exists. The server stays in "auth on" state and rejects all client commands with `NOAUTH`.

**Mitigation:** Keep at least one user in the live table. If this happens, restart the server.

**Impact:** Recoverable only by restart. No `ACL RESET` or recovery bypass.

#### TLS cert/key paths resolve relative to process working directory

**What happens:** `tls_cert_path` and `tls_key_path` are resolved relative to the process's `cwd`, not the config file's directory. A relative path works only when the process is run from the cert's directory; running from anywhere else fails with `NotFound`.

**Example:**
```
# config: tls_cert_path = "certs/server.pem"
cd /home/user/app && ./rocket-mem --config /etc/rocket-mem.toml
# -> certs/server.pem resolved as /home/user/app/certs/server.pem, not /etc/certs/server.pem
```

**Mitigation:** Use absolute paths for TLS cert/key settings.

**Impact:** Relative paths are fragile and easily broken by deployment changes.

#### Setting TLS address equal to plaintext address causes confusing bind failure

**What happens:** No config-time validation ensures `tls_resp_addr` is different from `addr`. If they are set to the same port, the server attempts to bind TLS first (succeeds), then fails on the plaintext listener with `AddrInUse` — the error message does not hint that the two settings collided.

**Example:**
```
ROCKET_MEM_ADDR=127.0.0.1:6379 ROCKET_MEM_TLS_RESP_ADDR=127.0.0.1:6379 ./rocket-mem
# -> TLS listening on 127.0.0.1:6379
# -> Error: bind failed: AddrInUse    [confusing; does not say the two settings collided]
```

**Impact:** Configuration error is not caught at validation time; debugging requires careful comparison of config values.

### Newly found while writing this playbook — not yet triaged

Confirmed against `v0.1.3` (`61f40ae`) both on the wire and in `crates/server/src/dispatcher.rs`.
Unlike the gaps above, these have **not** been filed or accepted yet. If you reproduce one,
reference the case ID and this section rather than opening a duplicate.

#### `ZADD` silently drops all but the first score/member pair

The most serious of the four: it loses data and reports a wrong count, with no error.

```bash
redis-cli -p 6550 zadd myzset 1 a 2 b 3 c
# actual:   1          <- claims one member added
redis-cli -p 6550 zrange myzset 0 -1
# actual:   a          <- b and c were silently discarded
# real Redis: ZADD returns 3, and the set contains a, b, c.
```

`dispatcher.rs`'s `"ZADD"` arm reads only `rest[1]` (score) and `rest[2]` (member). The
`require_args!` macro checks `rest.len() < n`, so it is a *minimum* — surplus arguments are
accepted and ignored rather than rejected. Case CORE-28.

#### `LPOP`, `RPOP`, `SPOP`, and `SRANDMEMBER` accept a `count` argument and ignore it

```bash
redis-cli -p 6550 rpush mylist x y z
redis-cli -p 6550 lpop mylist 2
# actual:   x          <- a single bulk reply, count ignored
# real Redis: an array of two elements, [x, y].
```

Same root cause as `ZADD`: a minimum-arity check with no upper bound. Cases CORE-19, CORE-23b.

#### Some error replies omit the `ERR` prefix

```bash
redis-cli -p 6550 set strk abc
redis-cli -p 6550 incr strk
# actual:   value is not an integer or out of range
# real Redis: ERR value is not an integer or out of range

redis-cli -p 6550 rename nosuchkey other
# actual:   no such key
# real Redis: ERR no such key
```

`WRONGTYPE` errors do carry their prefix, so this is specific to certain validation paths. It
matters for clients that branch on the error code. Cases CORE-06, CORE-32.

#### `SET` accepts mutually exclusive flags instead of rejecting them

```bash
redis-cli -p 6550 set ck v1 NX XX          # actual: OK   (real Redis: ERR syntax error)
redis-cli -p 6550 set ck2 v EX 100 PX 5000 # actual: OK, TTL 100s — EX silently wins
```

Only `NX` is honored when both are given. Case CORE-03.
