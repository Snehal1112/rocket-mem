# QA Agent Live-Cluster Ports — Kill-Safety and Port Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/qa-agent` work again — and safely — now that `docs/qa-playbook.html`'s
single-instance suites were deliberately migrated (commit `b73631e`, user-directed) to use the
live cluster's own addresses (`numericlabs.lxd:6379`/`7379`/`9121` for Smoke suite) instead of
isolated scratch ports. Today `yarn test` is 15/51 red because Plan 3's `assertNoLiveClusterAddresses`
guard correctly refuses to run against this data — that guard's premise (these addresses should
never appear) is now wrong and must be replaced with a premise that's actually true: qa-agent may
share ports with the live cluster, but must **never** kill a real cluster node, and must refuse
to even attempt starting a suite while the live cluster is already using that port.

**Architecture:** A new runtime check — `CLUSTER MYID` — replaces the retired static
address-ban. Real rocket-mem source (`crates/server/src/dispatcher.rs`,
`cluster_myid_returns_this_nodes_id_or_a_zero_id_when_disabled`) confirms this command returns
the node's real configured id (e.g. `"shard-a"`) when cluster mode is on, or 40 zero-characters
when cluster mode is disabled — exactly the signal needed to tell "the live cluster's real node"
apart from "qa-agent's own bare scratch instance," queryable at any time regardless of the
process's age (unlike startup-log-only signals). `processTracker.ts` gains this check and
`killIfRocketMem` refuses to kill anything that answers with a real id. `sessionRunner`/`server.ts`
gain a symmetric pre-flight check: refuse to even attempt a suite while its port is already held
by a confirmed real cluster node, with a clear error instead of a confusing `AddrInUse`.

**Tech Stack:** Same as Plans 1-3. `tools/qa-agent` is git-ignored. This plan's one git-tracked
change (if any survive self-review) would be `docs/qa-playbook.html`/spec doc updates — check
per task; none are currently planned, this is qa-agent-internal reconciliation work.

**Spec:** `docs/superpowers/specs/2026-09-12-qa-agent-design.md`

## Global Constraints

- **Never kill anything without confirming, via `CLUSTER MYID`, that it answers with the
  disabled-cluster id (40 zero characters) — not just that it's a `rocket-mem` process on the
  expected port.** A real cluster node passing the existing "confirmed rocket-mem" check is
  exactly the case this plan exists to stop from being killed.
- **The live hand-started cluster is 6 plain processes, not systemd-managed** (confirmed:
  `systemctl --user list-units 'rocket-mem*'` shows all 3 shard units `inactive dead`). Each is
  simply `./target/release/rocket-mem --config <file>.toml &`, run from the repo root
  (`/home/numericlabs/data/rocket/rocket-mem`): `rocket-mem.toml` (shard-a leader),
  `rocket-mem-shard-b.toml`, `rocket-mem-shard-c.toml`, and the three `-replica.toml` variants
  for the replicas. Stopping/restarting it for verification means: resolve each of the 6 PIDs
  fresh via `ss -tlnp`/`pgrep -af rocket-mem` (never trust a previously-noted PID — it may have
  changed), `kill -TERM` each by confirmed PID, then restart each with its own exact
  `--config <file>.toml &` from the repo root once all 6 have exited and their ports are free,
  and confirm all 6 are back with `ss -tlnp` before considering the task done. Never `pkill -f`.
- **Retire `tools/qa-agent/src/safety.ts`'s `assertNoLiveClusterAddresses`** — its premise (these
  addresses are always wrong) is now false. Task 2 removes it and its call sites; the `CLUSTER
  MYID` check is its replacement, applied at kill-time and at pre-flight, not at load time
  against static text.
- Work happens directly in the main checkout at `/home/numericlabs/data/rocket/rocket-mem`
  (`tools/qa-agent` only durably exists there).
- **A suite's own tests (`caseRunner.test.ts`, `sessionRunner.test.ts`, `server.test.ts`) can now
  only pass while the live cluster is stopped**, since they exercise the real Smoke suite against
  its real (now shared) port. This is a real, accepted trade-off, not a bug to design around —
  Task 3's job is to prove the whole thing works correctly during that window, not to make tests
  usable while the live cluster is up too.

---

### Task 1: `CLUSTER MYID`-based node detection in `processTracker.ts`

**Files:**
- Modify: `tools/qa-agent/src/processTracker.ts`
- Test: `tools/qa-agent/src/processTracker.test.ts`

**Interfaces:**
- Consumes: nothing new.
- Produces: `export function isClusterNode(port: number): boolean` (true only if a process is
  listening on `port` AND answers `CLUSTER MYID` with something other than 40 zero characters;
  false if nothing is listening, the process isn't rocket-mem, or it answers with the disabled
  id) — consumed by `killIfRocketMem` (this task) and by Task 2's pre-flight check.

- [ ] **Step 1: Write the failing tests**

Create/extend `tools/qa-agent/src/processTracker.test.ts` with a new `describe` block. This
needs TWO real scratch instances distinguishable by cluster config — one plain (today's existing
pattern), one with a minimal, disposable cluster config of its own (NOT the live cluster's
`cluster.conf`). The real config's exact format, confirmed by reading the checked-in
`cluster.conf` at the repo root (`<node-id> <host:port> <first-slot> <last-slot>`, `#`-comments)
and `crates/server/src/config.rs`'s CLI flags (`--cluster-config <path>` / `--cluster-node-id
<id>`, exposed as `ROCKET_MEM_CLUSTER_CONFIG` / `ROCKET_MEM_CLUSTER_NODE_ID` via this project's
established env-var-mirrors-CLI-flag convention, e.g. `ROCKET_MEM_CLUSTER_PROBE_INTERVAL_SECS`
already confirmed for a sibling field): a single-node topology file covering the full slot range
(`0 16383`) is enough for a throwaway "cluster" that only exists to make `CLUSTER MYID` answer
with a real id.

```typescript
import { describe, it, expect, afterEach } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import { connect } from "node:net";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { isClusterNode, killIfRocketMem } from "./processTracker.js";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../",
);
const BIN = path.join(REPO_ROOT, "target/release/rocket-mem");
const PLAIN_PORT = 18771;
const CLUSTER_PORT = 18774;

let child: ChildProcess | undefined;

function waitForPortOpen(port: number, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const socket = connect({ port, host: "127.0.0.1" }, () => {
        socket.end();
        resolve();
      });
      socket.on("error", () => {
        socket.destroy();
        if (Date.now() > deadline) reject(new Error(`port ${port} never opened`));
        else setTimeout(attempt, 100);
      });
    };
    attempt();
  });
}

/** Spawns a throwaway, single-node "cluster" (not the live one) covering all 16384 slots, so
 * CLUSTER MYID answers with `nodeId` instead of the disabled-cluster zero id. */
function spawnClusterNode(cwd: string, port: number, nodeId: string): ChildProcess {
  const confPath = path.join(cwd, "test-cluster.conf");
  writeFileSync(
    confPath,
    `# throwaway single-node test cluster config\n${nodeId} 127.0.0.1:${port} 0 16383\n`,
  );
  return spawn(BIN, [], {
    cwd,
    env: {
      ...process.env,
      ROCKET_MEM_ADDR: `127.0.0.1:${port}`,
      ROCKET_MEM_RMP_ADDR: `127.0.0.1:${port + 1}`,
      ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${port + 2}`,
      ROCKET_MEM_CLUSTER_CONFIG: confPath,
      ROCKET_MEM_CLUSTER_NODE_ID: nodeId,
    },
  });
}

afterEach(() => {
  if (child) {
    try {
      child.kill("SIGKILL");
    } catch {
      /* already exited */
    }
    child = undefined;
  }
});

describe("isClusterNode", () => {
  it("returns false for a bare scratch instance (no cluster config)", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-clustercheck-plain-"));
    child = spawn(BIN, [], {
      cwd,
      env: {
        ...process.env,
        ROCKET_MEM_ADDR: `127.0.0.1:${PLAIN_PORT}`,
        ROCKET_MEM_RMP_ADDR: `127.0.0.1:${PLAIN_PORT + 1}`,
        ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${PLAIN_PORT + 2}`,
      },
    });
    await waitForPortOpen(PLAIN_PORT);
    expect(isClusterNode(PLAIN_PORT)).toBe(false);
  });

  it("returns true for a scratch instance with its own throwaway cluster config", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-clustercheck-cluster-"));
    child = spawnClusterNode(cwd, CLUSTER_PORT, "testnode");
    await waitForPortOpen(CLUSTER_PORT);
    expect(isClusterNode(CLUSTER_PORT)).toBe(true);
  });

  it("returns false when nothing is listening", () => {
    expect(isClusterNode(18773)).toBe(false);
  });
});

describe("killIfRocketMem refuses to kill a cluster node", () => {
  it("does not kill and returns false for a confirmed cluster node", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-killcheck-cluster-"));
    child = spawnClusterNode(cwd, CLUSTER_PORT, "testnode");
    await waitForPortOpen(CLUSTER_PORT);
    expect(killIfRocketMem(CLUSTER_PORT)).toBe(false);
    // Confirm it's still alive and still answering as a cluster node, not killed:
    expect(isClusterNode(CLUSTER_PORT)).toBe(true);
  });

  it("still kills a plain scratch instance as before", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-killcheck-plain-"));
    child = spawn(BIN, [], {
      cwd,
      env: {
        ...process.env,
        ROCKET_MEM_ADDR: `127.0.0.1:${PLAIN_PORT}`,
        ROCKET_MEM_RMP_ADDR: `127.0.0.1:${PLAIN_PORT + 1}`,
        ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${PLAIN_PORT + 2}`,
      },
    });
    await waitForPortOpen(PLAIN_PORT);
    expect(killIfRocketMem(PLAIN_PORT)).toBe(true);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test src/processTracker.test.ts`
Expected: FAIL — `isClusterNode` doesn't exist yet, and the cluster-config fixture (once you've
filled in the real format) won't yet produce a real id since `killIfRocketMem` doesn't check it.

- [ ] **Step 3: Implement `isClusterNode` and wire it into `killIfRocketMem`**

Add to `tools/qa-agent/src/processTracker.ts`:

```typescript
const DISABLED_CLUSTER_ID = "0".repeat(40);

/**
 * True only if a confirmed rocket-mem process on `port` answers CLUSTER MYID with a real
 * (non-disabled) node id — i.e. it's a real, cluster-configured node, not a bare scratch
 * instance. This is the one thing that lets qa-agent share ports with the live hand-started
 * cluster without ever being able to kill it: killIfRocketMem refuses whenever this is true.
 */
export function isClusterNode(port: number): boolean {
  if (resolveRocketMemPid(port) === null) return false;
  try {
    const reply = execFileSync(
      "redis-cli",
      ["-p", String(port), "CLUSTER", "MYID"],
      { encoding: "utf8", timeout: 3000 },
    ).trim();
    return reply.length > 0 && reply !== DISABLED_CLUSTER_ID;
  } catch {
    // redis-cli itself failing (connection refused, timeout) is not "it's a cluster node" —
    // treat it the same as "couldn't confirm", which callers should treat cautiously (see
    // killIfRocketMem below: an inability to confirm the safe case is NOT the same as
    // confirming safety, so killIfRocketMem's own logic does not simply invert this function).
    return false;
  }
}
```

Update `killIfRocketMem`:

```typescript
export function killIfRocketMem(port: number): boolean {
  const pid = resolveRocketMemPid(port);
  if (pid === null) return false;
  if (isClusterNode(port)) return false;
  try {
    process.kill(pid, "SIGTERM");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ESRCH") return false;
    throw err;
  }
  return true;
}
```

(`execFileSync` needs importing from `node:child_process` if not already imported in this file.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test src/processTracker.test.ts`
Expected: PASS — all cases green, including the cluster-node-refuses-kill case.

- [ ] **Step 5: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

- [ ] **Step 6: Confirm the live cluster is untouched**

Run: `ss -tlnp | grep -E ':(6379|6380|6381|7379|7380|7381|9121|9122|9123)\b'`
Expected: unaffected — this task's tests only ever use ports 18771-18773, far from the live
cluster's addresses.

---

### Task 2: Retire the static address ban; add pre-flight port-ownership check; update ports

**Files:**
- Modify: `tools/qa-agent/src/suites.ts` (`SUITE_SERVER_PORT` values)
- Delete: `tools/qa-agent/src/safety.ts`, `tools/qa-agent/src/safety.test.ts`
- Modify: `tools/qa-agent/src/server.ts` (remove `assertNoLiveClusterAddresses` call/import; add
  pre-flight check)
- Modify: `tools/qa-agent/src/index.ts` (remove `assertNoLiveClusterAddresses` call/import; add
  pre-flight check)
- Modify: `tools/qa-agent/src/sessionRunner.ts` (surface a clear pre-flight error, not a bare
  bind failure)
- Test: `tools/qa-agent/src/sessionRunner.test.ts` (extend)

**Interfaces:**
- Consumes: `isClusterNode` from `./processTracker.js` (Task 1).
- Produces: `export function assertSuitePortIsFree(section: string, port: number): void` in
  `sessionRunner.ts` (throws a clear `Error` if `isClusterNode(port)` is true; does nothing
  otherwise — a plain scratch instance or nothing listening are both fine, since a plain scratch
  instance either belongs to a still-alive prior session (handled by the existing liveness
  check in `suiteSessionFor`) or will be cleanly superseded) — called from `runSuite` before it
  runs the first not-yet-run case in a chain.

- [ ] **Step 1: Update `SUITE_SERVER_PORT` in `suites.ts`**

```typescript
export const SUITE_SERVER_PORT: Record<string, number> = {
  "Smoke suite": 6379,
};
```

(Confirm this against the real committed `docs/qa-playbook.html`'s `SMOKE-01` steps before
finalizing — the plan text above was verified against it at the time this plan was written, but
re-check in case it's changed again since.)

- [ ] **Step 2: Delete the retired safety module and its call sites**

Delete `tools/qa-agent/src/safety.ts` and `tools/qa-agent/src/safety.test.ts`. Remove the
`import { assertNoLiveClusterAddresses } from "./safety.js";` line and its call
(`assertNoLiveClusterAddresses(inScopeCases);` or similar) from both `server.ts`'s `startServer`
and `index.ts`'s CLI entry.

- [ ] **Step 3: Write the failing test for the pre-flight check**

Add to `tools/qa-agent/src/sessionRunner.test.ts`:

```typescript
import { spawn, type ChildProcess } from "node:child_process";
import { connect } from "node:net";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { assertSuitePortIsFree } from "./sessionRunner.js";

const CLUSTER_CHECK_PORT = 18775;
let clusterChild: ChildProcess | undefined;

// If this file already has an equivalent helper (e.g. reused from a shared pattern), use that
// one instead of adding a second copy — this is the same wait-for-TCP-connect logic used in
// processTracker.test.ts and server.test.ts.
function waitForPortOpen(port: number, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const socket = connect({ port, host: "127.0.0.1" }, () => {
        socket.end();
        resolve();
      });
      socket.on("error", () => {
        socket.destroy();
        if (Date.now() > deadline) reject(new Error(`port ${port} never opened`));
        else setTimeout(attempt, 100);
      });
    };
    attempt();
  });
}

describe("assertSuitePortIsFree", () => {
  afterEach(() => {
    if (clusterChild) {
      try {
        clusterChild.kill("SIGKILL");
      } catch {
        /* already exited */
      }
      clusterChild = undefined;
    }
  });

  it("does not throw when nothing is listening on the port", () => {
    expect(() => assertSuitePortIsFree("Smoke suite", 18781)).not.toThrow();
  });

  it("throws when the port is held by a confirmed cluster node", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-preflight-cluster-"));
    const confPath = path.join(cwd, "test-cluster.conf");
    writeFileSync(confPath, `testnode 127.0.0.1:${CLUSTER_CHECK_PORT} 0 16383\n`);
    clusterChild = spawn(BIN, [], {
      cwd,
      env: {
        ...process.env,
        ROCKET_MEM_ADDR: `127.0.0.1:${CLUSTER_CHECK_PORT}`,
        ROCKET_MEM_RMP_ADDR: `127.0.0.1:${CLUSTER_CHECK_PORT + 1}`,
        ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${CLUSTER_CHECK_PORT + 2}`,
        ROCKET_MEM_CLUSTER_CONFIG: confPath,
        ROCKET_MEM_CLUSTER_NODE_ID: "testnode",
      },
    });
    await waitForPortOpen(CLUSTER_CHECK_PORT);
    expect(() => assertSuitePortIsFree("Smoke suite", CLUSTER_CHECK_PORT)).toThrow(
      /held by a real cluster node/,
    );
  });
});
```

(`BIN`/`path`/`REPO_ROOT` follow the same pattern already established in this file for the real
Smoke-suite walking-skeleton test — reuse those existing constants rather than redefining them.)

- [ ] **Step 4: Run the test to verify it fails**

Run: `cd tools/qa-agent && yarn test src/sessionRunner.test.ts`
Expected: FAIL — `assertSuitePortIsFree` doesn't exist yet.

- [ ] **Step 5: Implement `assertSuitePortIsFree` and call it from `runSuite`**

Add to `tools/qa-agent/src/sessionRunner.ts`:

```typescript
import { isClusterNode } from "./processTracker.js";

export function assertSuitePortIsFree(section: string, port: number): void {
  if (isClusterNode(port)) {
    throw new Error(
      `"${section}"'s port ${port} is currently held by a real cluster node — stop the live cluster before running this suite.`,
    );
  }
}
```

At the top of `runSuite` (before the `for` loop over cases), if `options.pidPort` is set, call
`assertSuitePortIsFree(<section — thread this through RunSuiteOptions if not already
available, e.g. from the first case's `.section`>, options.pidPort)`. (Exact wiring: `cases[0]?.section`
is available without adding a new option, since every case in the array shares one section by
construction — use that rather than adding a redundant parameter.)

- [ ] **Step 6: Close a residual gap — `<pid>` substitution must not hand a real cluster node's PID to a case's own `kill -TERM <pid>` step**

`SMOKE-12`'s `steps` run `kill -TERM <pid>` as literal shell text — this never goes through
`killIfRocketMem`'s new `isClusterNode` guard at all, since it's a raw shell command, not a
TypeScript call. The chain's own semantics make this hard to hit in practice (`SMOKE-01` must
already have passed for `SMOKE-12` to run at all, and `SMOKE-01` fails immediately with an
`AddrInUse`-shaped mismatch if the live cluster already holds the port — so the chain can't even
reach `SMOKE-12` while the live cluster legitimately owns the port). But a narrow window remains:
if qa-agent's own scratch instance were to die mid-chain and the live cluster got restarted on
that exact port before `SMOKE-12` runs, `<pid>` would resolve to the live cluster's real PID.

Close it in `sessionRunner.ts`, at the same spot found above: only pass `pid` through when it's
confirmed NOT a cluster node.

```typescript
const rawPid =
  options.pidPort !== undefined ? resolveRocketMemPid(options.pidPort) ?? undefined : undefined;
const pid = rawPid !== undefined && !isClusterNode(options.pidPort!) ? rawPid : undefined;
```

(Adjust to fit the surrounding code's exact current shape.) With `pid` left `undefined`, `<pid>`
in `SMOKE-12`'s steps is never substituted, so the literal string `<pid>` reaches bash as-is —
which fails harmlessly (bash reads `<pid>` as an input redirection from a nonexistent file named
`pid`, an ordinary shell error) rather than ever handing a real PID to `kill`.

Add a test to `sessionRunner.test.ts`'s fake-`runCaseFn` suite: construct a case whose id would
trigger `<pid>` substitution logic (or, more directly, test this at the `runCase`/`caseRunner`
boundary if that's where the substitution actually lives — check the current code first) with
`pidPort` pointed at a confirmed cluster-node port (reuse the `spawnClusterNode` pattern from
Step 3 above), and assert the substituted command never receives the real PID.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS for the new tests. Other Smoke-suite-dependent tests (`caseRunner.test.ts`,
`sessionRunner.test.ts`'s real-server tests, `server.test.ts`) will still be RED at this point if
the live cluster is currently running — that's expected and is Task 3's job to verify properly,
not this task's.

- [ ] **Step 8: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

---

### Task 3: Stop the live cluster, verify everything end-to-end, restart it

**Files:** none (verification-only task; fixes anything Task 1/2 missed as they're found)

**Interfaces:** none new.

- [ ] **Step 1: Resolve and record the live cluster's current PIDs fresh**

```bash
ss -tlnp | grep -E ':(6379|6380|6381|7379|7380|7381|9121|9122|9123)\b'
pgrep -af 'target/release/rocket-mem --config'
```

Record all 6 PIDs and their exact `--config <file>.toml` argument. Do not reuse any PID noted
earlier in this conversation — resolve fresh, right before stopping.

- [ ] **Step 2: Stop all 6 by confirmed PID**

```bash
for pid in <pid1> <pid2> <pid3> <pid4> <pid5> <pid6>; do
  kill -TERM "$pid"
done
sleep 2
ss -tlnp | grep -E ':(6379|6380|6381|7379|7380|7381|9121|9122|9123)\b' || echo "all clear"
```

Expected: "all clear" — confirm before proceeding, do not move to Step 3 until the ports are
actually free.

- [ ] **Step 3: Run the full qa-agent verification while the live cluster is down**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn test 2>&1 | tail -30
yarn typecheck
yarn cli --suite "Smoke suite"
```

Expected: all tests pass (51+ from Plan 3, plus Task 1/2's new ones), typecheck clean, CLI
reports `12/12 passed, 0 failed` against the real `numericlabs.lxd:6379`/etc. addresses.

- [ ] **Step 4: Verify the on-demand Run button + kill-safety end-to-end**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn serve &
SERVE_PID=$!
sleep 1
curl -s -X POST http://127.0.0.1:4848/api/run -d '{"caseId":"SMOKE-12"}' | head -c 200
echo
kill "$SERVE_PID" 2>/dev/null || true
```

Expected: `12/12` (or the full chain) reported pass, and — since `SMOKE-12` kills the suite's
own server as its last step — confirm the port is free afterward:
`ss -tlnp | grep -E ':(6379|7379|9121)\b'` prints nothing.

- [ ] **Step 5: Restart the live cluster exactly as it was**

From the repo root, using each PID's recorded `--config` argument from Step 1:

```bash
cd /home/numericlabs/data/rocket/rocket-mem
./target/release/rocket-mem --config rocket-mem.toml &
./target/release/rocket-mem --config rocket-mem-shard-b.toml &
./target/release/rocket-mem --config rocket-mem-shard-c.toml &
./target/release/rocket-mem --config rocket-mem-shard-a-replica.toml &
./target/release/rocket-mem --config rocket-mem-shard-b-replica.toml &
./target/release/rocket-mem --config rocket-mem-shard-c-replica.toml &
sleep 2
ss -tlnp | grep -E ':(6379|6380|6381|7379|7380|7381|9121|9122|9123)\b'
```

Expected: all 6 back up, listening on their expected ports. Confirm the count matches (6 lines).

- [ ] **Step 6: Final safety confirmation**

```bash
redis-cli -p 6379 CLUSTER MYID
```

Expected: the real configured id (not 40 zeros) — confirms the restarted process is genuinely
the cluster node, not something qa-agent's own tooling could mistake for a scratch instance.

## Next plan

None queued. Remaining known follow-on work (from the spec's "Open items" and Plan 3's final
review, unaffected by this plan): wiring up the other 7 in-scope suites' prose-derived setup so
`--all`/other `--suite` names work; the Observability `...`/`NNNNN` matching-mode gap; the wider
stale-banner cleanup for `PERSIST-02`..`05`/`CFG-01`..`07`/`RMP-01`; and, per Plan 3's final
review, making `/api/run` return `202` with SSE-only progress before wiring a suite as large as
Core (54 cases) so a cold on-demand chain doesn't hang one request for the whole thing.
