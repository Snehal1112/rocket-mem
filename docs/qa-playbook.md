# rocket-mem QA playbook

A test playbook for `rocket-mem`, a from-scratch Redis-wire-compatible (RESP2/RESP3) in-memory
data store. It assumes no knowledge of the codebase: every case gives the exact commands to run
and the exact output to expect.

**Covers version:** `v0.1.3` (commit `61f40ae`). If you are testing a later build, re-check the
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
| Core data types (`CORE`) | 46 | 45 min | High. Every release candidate. |
| Persistence (`PERSIST`) | 5 | 20 min | High. Data-loss surface. |
| Replication (`REPL`) | 6 | 25 min | Medium. Needs two nodes. |
| Cluster (`CLUSTER`) | 6 | 30 min | Medium. Needs three nodes. |
| Configuration (`CFG`) | 9 | 20 min | Medium. |
| RMP protocol (`RMP`) | 5 | 20 min | Medium. Needs a Rust toolchain. |
| Observability (`OBS`) | 7 | 15 min | Low, unless metrics are part of the release. |
| ACL and authentication (`ACL`) | 18 | 40 min | **Critical.** Security. |
| TLS (`TLS`) | 10 | 25 min | **Critical.** Security. |

If you only have time for one section, run the smoke suite. For two, add ACL. The full pass is
roughly four to five hours including setup.

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
redis_version:rocket-mem-0.1.3
rocket_mem_version:0.1.3
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
ERR index out of range
```

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

Startup banner, byte for byte, on every node regardless of whether the AOF/snapshot files
already existed:

```
Recovered state from <snapshot-path> and <aof-path>
Metrics on http://<metrics-addr>/metrics
RMP listening on <rmp-addr>
Listening on <resp-addr>
```

The "Recovered state from..." line is printed unconditionally — it does not distinguish between
"loaded real data" and "found nothing, started empty." Don't read its presence as proof of a
non-empty recovery; check the actual keys.

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
connected_slaves:1

# Replication
role:slave
master_host:127.0.0.1
master_port:6560
master_link_status:up
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
which silently discards anything the follower had written locally before rejecting further
client writes wasn't yet in effect. There is no replication-offset lag metric either —
`rocket_mem_replication_last_apply_timestamp_seconds` (a timestamp of last applied write, not an
offset) is the documented substitute, confirmed present above; `rocket_mem_connected_replicas`
is the follower-count gauge. Separately (not re-verified in this run, see `README.md`'s Sprint 5
entry and Sprint 8 entry): `PSYNC` has no dedicated auth mechanism of its own — it goes through
the same `AUTH`/ACL gate every other command does, and only actually blocks anything once ACL
users are configured; a server with none configured authenticates nobody, replica included.

**Result:** ☐ Pass ☐ Fail

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
nothing is ever bound there (no cluster bus exists — see CLUSTER-06).

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

**Notes — known limits to expect, not bugs:** no cluster bus or gossip — every node always
reports every configured node `connected` and `cluster_state:ok` regardless of whether the other
processes are even running, because the topology is a static file, not a live membership
protocol; no live resharding or failover — `CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not
exist as commands at all; no request forwarding — a `MOVED` reply is final, the client must
reconnect itself, this server never proxies a request to another shard on the client's behalf;
`CLUSTER SLOTS` is not implemented (deprecated upstream since Redis 7.0 in favor of
`CLUSTER SHARDS`, which is implemented — see CLUSTER-03).

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
INFO server -> Bulk(b"# Server\r\nredis_version:rocket-mem-0.1.3\r\nrocket_mem_version:0.1.3\r\n...")
SAVE -> Simple("OK")
SLOWLOG LEN -> Integer(0)
```
(actual captured run: `SAVE -> Simple("OK")`, `SLOWLOG LEN -> Integer(0)`, `INFO server` first
three lines were `# Server | redis_version:rocket-mem-0.1.3 | rocket_mem_version:0.1.3`)

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
redis_version:rocket-mem-0.1.3
rocket_mem_version:0.1.3
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
Recovered state from /tmp/acltls-qa/acl.snap and /tmp/acltls-qa/acl.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
Listening on 127.0.0.1:6510
```

**Notes:** The PID number will differ; that is the only line that varies by value. The three
listener lines are printed from concurrently-started tasks, so their **order can vary between
runs** — check that all three are present, not that they are in this sequence.

The `Recovered state from ...` line is printed even on a completely fresh run where neither
`acl.snap` nor `acl.aof` exists. It is not evidence that anything was loaded; do not treat its
presence as a recovery signal. (Flagged as a maintainer-facing wording problem, not a test
failure.)

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
Error: Custom { kind: InvalidInput, error: "acl bootstrap: ERR syntax error at 'on'" }
exit=1
Error: Custom { kind: InvalidInput, error: "acl bootstrap: ERR syntax error at '<password token>'" }
exit=1
ports free
```

**Notes:** Two things to check beyond the exit code.

First, the second error says `'<password token>'` — the literal string `secret123` is **not**
echoed. A misconfigured password must not leak into stderr, journald, container logs or CI output.
If you ever see the actual password there, that is a security defect worth filing.

Second, no listener lines are printed at all: the ACL bootstrap check runs before *anything* is
bound, so the process leaves no half-started state. (This differs from the TLS failures in TLS-08,
which abort after the metrics and RMP listeners are already up.)

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
Recovered state from /tmp/acltls-qa/tls.snap and /tmp/acltls-qa/tls.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
TLS listening on 127.0.0.1:6530
RMP TLS listening on 127.0.0.1:6531
Listening on 127.0.0.1:6510
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
`tls_cert_path`, `tls_key_path`), as `ROCKET_MEM_TLS_*` env vars, or as `--tls-*` flags.

Banner line order varies between runs; `ss` row order varies too. Check for presence.

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

**Expected:**
```
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
Error: Custom { kind: InvalidInput, error: "tls_resp_addr is set but tls_cert_path/tls_key_path is not -- TLS requires both" }
exit=1
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }
exit=1
Recovered state from ./dump.snapshot and ./appendonly.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
Error: Custom { kind: InvalidData, error: "no certificate found in cert file" }
exit=1
```

**Notes:** The behavior under test is that all three exit 1. A TLS misconfiguration must never
result in a server that comes up happily with its TLS listener silently missing — that would look
healthy while serving nothing but plaintext.

All three abort **after** the metrics and plaintext RMP listeners are already bound and printed,
so the error scrolls past two success lines. The plaintext `Listening on 127.0.0.1:6510` line
never appears, which is the reliable signal that startup did not complete. (Contrast ACL-16, where
the failure happens before anything binds.)

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

**Expected:**
```
Recovered state from /tmp/acltls-qa/tls.snap and /tmp/acltls-qa/tls.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }
exit=1
Recovered state from /tmp/acltls-qa/tls.snap and /tmp/acltls-qa/tls.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
TLS listening on 127.0.0.1:6530
Listening on 127.0.0.1:6510
exit=124
```

**Notes:** This is a real trap. The **same config file** fails from one directory and starts
cleanly from another, with no diagnostic naming the path it actually tried. `tls_cert_path` and
`tls_key_path` are resolved against the server process's working directory, not against the
directory the config file lives in — which is the intuition most people bring.

`exit=124` on the second run is `timeout` killing a healthy server after 2 seconds. That is the
pass condition; the five banner lines ending in `Listening on 127.0.0.1:6510` are what matters.

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

**Expected:**
```
Recovered state from /tmp/acltls-qa/acltls.snap and /tmp/acltls-qa/acltls.aof
Metrics on http://127.0.0.1:9310/metrics
RMP listening on 127.0.0.1:6511
TLS listening on 127.0.0.1:6530
RMP TLS listening on 127.0.0.1:6531
Listening on 127.0.0.1:6510
NOAUTH Authentication required.

PONG
```

**Notes:** TLS and ACL are independent layers and compose as expected: transport encryption grants
no identity, so a TLS client starts out just as unauthenticated as a plaintext one. Completing the
handshake is not authentication — there is no mutual TLS and no certificate-derived identity
anywhere in this build.

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
| Replication lag metric | No true replication-offset lag metric. Reported metric `rocket_mem_replication_last_apply_timestamp_seconds` measures apply time, not offset distance. | Full-resync-only design means no offsets exist. Timestamp is the honest substitute for offset-based lag. |
| `DEBUG SLEEP` | Capped at 10-second maximum duration. Requests over 10 seconds are rejected with an error. | Prevents accidental server thread blocking indefinitely from client requests. Safety limit, not a bug. |
| `@category` ACL grants | Only explicit `+CMDNAME`/`-CMDNAME` grants and `allcommands`/`nocommands` (or `+@all`/`-@all`) are accepted. Other categories like `+@read`, `+@write` are syntax errors. | Category taxonomy is large and the project prioritizes explicit command grants for clarity. Future backlog item. |
| ACL users persistence | Runtime `ACL SETUSER` is not persisted to AOF or snapshot. Lost on restart unless user is also declared in `[[acl.users]]` bootstrap array in TOML config. | Intentional design: ACL state is in-memory and local. Mirrors real Redis when `ACL SAVE`/`aclfile` is not configured. The project has no `ACL SAVE` command and no `aclfile` equivalent beyond TOML bootstrap. |
| ACL replication | `ACL SETUSER`/`DELUSER` are not logged to AOF or fanned out to replicas. Follower's ACL state can diverge from leader's unless both start from the same bootstrap config. | Intentional design. ACL changes are leader-local. Users must coordinate ACL bootstrap config across deployment. |
| Auth gate | Only `AUTH` and `HELLO` are reachable before an unauthenticated client authenticates. `ACL` deliberately is not exempt, preventing privilege escalation. | Security-first design: an unauthenticated client cannot bootstrap itself an admin account. |
| `ACL LIST` format | Output renders a user's password as `#<hash>` (its stored Argon2 hash), not the plaintext. This `#<hash>` format is not accepted as input back to `ACL SETUSER`; only `>password` (plaintext to hash) and `nopass` are accepted. | Matches real Redis's rendering; the round-trip rejection is intentional. Plaintext passwords are never logged, persisted, or echoed. |
| Cluster gossip | No cluster bus and no gossip. Nodes never talk to each other. Every configured node reports as `connected` and `cluster_state` is always `ok`. | Static config file design: cluster membership is fixed at process start, not dynamic. Honest answers would require inter-node communication, which is out of scope. |
| Cluster resharding | No live resharding and no failover. Slot ownership is fixed at process start via static config file. `CLUSTER SETSLOT`, `MIGRATE`, `ASK`/`ASKING` do not exist. | Static slot assignment is the design constraint. Live resharding requires dynamic slot migration, which is a future backlog item per Sprint 8 spec. |
| Cluster forwarding | No request forwarding. A `-MOVED` reply requires the *client* to reconnect and retry. This server never proxies requests to another shard. | Design choice for simplicity: clients handle redirection, not the server. Standard cluster-aware clients expect and handle this. |
| `CLUSTER SLOTS` | Not implemented. Deprecated since Redis 7.0 in favor of `CLUSTER SHARDS`. | Intentional: `CLUSTER SHARDS` (implemented) is the modern equivalent. |
| `/metrics` authentication | Endpoint is unauthenticated. No ACL check on HTTP requests to the metrics port. | Intentional design: loopback-only default and firewall are the security model. Metrics server is separate from command server. |

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

**Pub/sub:**
- `SUBSCRIBE`, `UNSUBSCRIBE` — subscribe to channels
- `PUBLISH` — publish to channel
- `PSUBSCRIBE` / `PUNSUBSCRIBE` — pattern subscriptions

**Transactions:**
- `MULTI`, `EXEC`, `DISCARD` — transaction blocks
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

**Future backlog note:** Lua scripting, pub/sub, transactions, and streams are explicitly tracked in `docs/rocket-mem-sprint-plan.md` as Phase 5 / follow-on backlog work, not current-sprint out-of-scope.

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
