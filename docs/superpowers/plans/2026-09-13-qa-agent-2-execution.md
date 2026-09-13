# QA Agent Execution — Process Tracking, Case Running, Session Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/qa-agent` actually run cases against a real `rocket-mem` server. By the end
of this plan, `yarn cli --suite "Smoke suite"` (or `--all`) builds the in-scope case list, runs
every case's `steps` verbatim against a real, freshly-started `rocket-mem` instance, judges each
one with Plan 1's `matcher.ts`, prints a pass/fail summary, and cleans up the server it started —
the walking-skeleton CLI the spec calls for. This plan also builds the suite-orchestration engine
(`sessionRunner.ts`) with the "stop after case X, skip what's already run" support Plan 3 needs
for the browser's on-demand Run button — but the HTTP server, SSE, and the button itself are
Plan 3's job, not this one.

**Architecture:** Three layers, each independently testable: `processTracker.ts` (resolve/kill a
`rocket-mem` PID by port, confirmed via `ss -tlnp`'s own process-name column — never `pkill -f`),
`caseRunner.ts` (run one case's `steps`, handling the async-background-launch problem a
synchronous exec would hang on), and `sessionRunner.ts` (run a suite's cases in order, with
dependency-injectable execution so its control flow is unit-testable without a real server). A
small `reporter.ts` and `index.ts` CLI entry wire them together into a real, runnable tool.

**Tech Stack:** Same as Plan 1 — TypeScript, Yarn, `vitest`. `tools/qa-agent` is git-ignored (see
Plan 1's Global Constraints) — this plan adds no new git-tracked files.

**Spec:** `docs/superpowers/specs/2026-09-12-qa-agent-design.md`

## Global Constraints

- **`tools/qa-agent` is git-ignored.** Do not `git add`/`git commit` anything under it — there is
  nothing to commit. Work from the main checkout at `/home/numericlabs/data/rocket/rocket-mem/`
  (not a worktree copy — Plan 1's final review found the code only reaches a real, durable
  location there; there is no other copy to build from).
- **Never `pkill -f rocket-mem`.** Every kill goes through `processTracker.killIfRocketMem(port)`,
  which resolves and confirms the PID via `ss -tlnp` first.
- **Precondition:** `target/release/rocket-mem` must exist. If it doesn't, run
  `cargo build --release --bin rocket-mem` from the repo root before starting Task 1 — several
  tests in this plan spawn the real binary.
- **Test ports must never collide with the live hand-started cluster** (6379-6381/7379-7381/
  9121-9123/16379-16381/17379-17381) **or any documented playbook suite port.** This plan
  introduces one new scratch port triple for `processTracker`'s own tests: `18761`/`18762`/
  `18763` (RESP/RMP/metrics) — chosen clear of every range in the spec's Safety section. Tasks 2
  and 3 use the real Smoke suite's own documented ports (`6540`/`6541`/`9340`) because those
  tasks exercise the *real* `SMOKE-01`..`SMOKE-12` cases, whose `steps` hardcode those ports
  verbatim (case text can't be parameterized without violating "run steps verbatim").
- **Test files must run sequentially, not in parallel**, since multiple test files in this plan
  bind the same real ports in sequence (Task 2's and Task 3's tests both use the Smoke suite's
  ports). Task 1 adds a `vitest.config.ts` setting `fileParallelism: false` for this reason —
  every later task's tests rely on it.
- Every case run gets a timeout so a hung command can't stall the suite: `caseRunner`'s foreground
  path defaults to 15000ms per case (overridable), the background-launch path always waits a
  fixed 1500ms grace period, never longer.

---

### Task 1: `processTracker.ts` — resolve and kill a `rocket-mem` PID by port

**Files:**
- Create: `tools/qa-agent/vitest.config.ts`
- Create: `tools/qa-agent/src/processTracker.ts`
- Test: `tools/qa-agent/src/processTracker.test.ts`

**Interfaces:**
- Consumes: nothing from Plan 1.
- Produces: `export function resolveRocketMemPid(port: number): number | null` and
  `export function killIfRocketMem(port: number): boolean` — consumed by `caseRunner.ts` (Task 2)
  and `sessionRunner.ts`/`index.ts` (Task 3).

- [ ] **Step 1: Create `tools/qa-agent/vitest.config.ts`**

```typescript
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Several tests in this project spin up real rocket-mem processes on fixed, documented
    // ports (matching the playbook's own port assignments). Running test files in parallel
    // would race on those ports, so keep the whole suite single-threaded.
    fileParallelism: false,
  },
});
```

- [ ] **Step 2: Confirm the binary is built**

Run: `ls target/release/rocket-mem` from the repo root (`/home/numericlabs/data/rocket/rocket-mem`).
Expected: the file exists. If it doesn't, run `cargo build --release --bin rocket-mem` from the
repo root first (this can take a minute or two) — do not proceed to Step 3 until it exists.

- [ ] **Step 3: Write the failing tests**

Create `tools/qa-agent/src/processTracker.test.ts`:

```typescript
import { describe, it, expect, afterEach } from "vitest";
import { spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { connect } from "node:net";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { resolveRocketMemPid, killIfRocketMem } from "./processTracker.js";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../",
);
const BIN = path.join(REPO_ROOT, "target/release/rocket-mem");
const PORT = 18761;
const RMP_PORT = 18762;
const METRICS_PORT = 18763;

let child: ChildProcess | undefined;
let stubServer: Server | undefined;

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

function waitForPortClosed(port: number, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const socket = connect({ port, host: "127.0.0.1" }, () => {
        socket.destroy();
        if (Date.now() > deadline) reject(new Error(`port ${port} never closed`));
        else setTimeout(attempt, 100);
      });
      socket.on("error", () => {
        socket.destroy();
        resolve();
      });
    };
    attempt();
  });
}

afterEach(async () => {
  if (child) {
    try {
      child.kill("SIGKILL");
    } catch {
      /* already exited */
    }
    child = undefined;
  }
  if (stubServer) {
    await new Promise<void>((resolve) => stubServer!.close(() => resolve()));
    stubServer = undefined;
  }
});

describe("processTracker", () => {
  it("returns null when nothing listens on the port", () => {
    expect(resolveRocketMemPid(PORT)).toBeNull();
  });

  it("refuses to resolve a non-rocket-mem process on the port", async () => {
    stubServer = createServer((_req, res) => res.end("not rocket-mem"));
    await new Promise<void>((resolve) => stubServer!.listen(PORT, "127.0.0.1", resolve));
    expect(resolveRocketMemPid(PORT)).toBeNull();
  });

  it("resolves the PID of a real rocket-mem process on the port", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-processtracker-"));
    child = spawn(BIN, [], {
      cwd,
      env: {
        ...process.env,
        ROCKET_MEM_ADDR: `127.0.0.1:${PORT}`,
        ROCKET_MEM_RMP_ADDR: `127.0.0.1:${RMP_PORT}`,
        ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${METRICS_PORT}`,
      },
    });
    await waitForPortOpen(PORT);
    expect(resolveRocketMemPid(PORT)).toBe(child.pid);
  });

  it("kills a real rocket-mem process and frees the port", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-processtracker-"));
    child = spawn(BIN, [], {
      cwd,
      env: {
        ...process.env,
        ROCKET_MEM_ADDR: `127.0.0.1:${PORT}`,
        ROCKET_MEM_RMP_ADDR: `127.0.0.1:${RMP_PORT}`,
        ROCKET_MEM_METRICS_ADDR: `127.0.0.1:${METRICS_PORT}`,
      },
    });
    await waitForPortOpen(PORT);
    expect(killIfRocketMem(PORT)).toBe(true);
    await waitForPortClosed(PORT);
    expect(resolveRocketMemPid(PORT)).toBeNull();
  }, 10000);
});
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './processTracker.js'`.

- [ ] **Step 5: Implement `tools/qa-agent/src/processTracker.ts`**

```typescript
import { execFileSync } from "node:child_process";

/**
 * Resolves the PID of whatever process is listening on `port`, confirming via `ss -tlnp`'s own
 * process-name column that it's a rocket-mem process before returning it. Returns null if
 * nothing is listening, or if a process is listening but isn't rocket-mem — never assume.
 */
export function resolveRocketMemPid(port: number): number | null {
  const output = execFileSync("ss", ["-tlnp"], { encoding: "utf8" });
  const found = findListener(output, port);
  if (!found || !found.comm.includes("rocket-mem")) return null;
  return found.pid;
}

/**
 * Sends SIGTERM to whatever rocket-mem process is listening on `port`. Returns false (and does
 * nothing) if nothing is listening there, or if it isn't confirmed to be rocket-mem. Never uses
 * a broad pattern kill — only a PID resolved and confirmed this way.
 */
export function killIfRocketMem(port: number): boolean {
  const pid = resolveRocketMemPid(port);
  if (pid === null) return false;
  process.kill(pid, "SIGTERM");
  return true;
}

function findListener(
  ssOutput: string,
  port: number,
): { pid: number; comm: string } | null {
  const portMarker = `:${port} `;
  for (const line of ssOutput.split("\n")) {
    if (!line.includes(portMarker)) continue;
    const match = line.match(/users:\(\("([^"]+)",pid=(\d+)/);
    if (!match) continue;
    return { comm: match[1], pid: Number(match[2]) };
  }
  return null;
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all 4 `processTracker.test.ts` cases green.

- [ ] **Step 7: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

- [ ] **Step 8: Confirm the live cluster is untouched**

Run: `ss -tlnp | grep -E ':(6379|6380|6381)\b'`
Expected: shows the live cluster's own listeners, unaffected by anything this task did (ports
18761-18763 never overlap with 6379-6381).

---

### Task 2: `caseRunner.ts` — execute one case's `steps` against a live server

**Files:**
- Create: `tools/qa-agent/src/caseRunner.ts`
- Test: `tools/qa-agent/src/caseRunner.test.ts`

**Interfaces:**
- Consumes: `QaCase` from `./playbook.js`, `matchCase`/`MatchResult` from `./matcher.js` (both
  Plan 1), `resolveRocketMemPid` from `./processTracker.js` (Task 1, used only by this task's
  test — `caseRunner` itself takes a `pid` value, it doesn't resolve one).
- Produces: `export interface CaseResult { id: string; verdict: "pass" | "fail"; reason?:
  string; actual: string; exitCode?: number }`, `export interface CaseRunOptions { cwd: string;
  pid?: number; timeoutMs?: number }`, `export async function runCase(qaCase: QaCase, options:
  CaseRunOptions): Promise<CaseResult>` — consumed by `sessionRunner.ts` (Task 3).

- [ ] **Step 1: Write the failing tests**

Create `tools/qa-agent/src/caseRunner.test.ts`:

```typescript
import { describe, it, expect } from "vitest";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { loadPlaybook, type QaCase } from "./playbook.js";
import { runCase } from "./caseRunner.js";
import { resolveRocketMemPid } from "./processTracker.js";

const PLAYBOOK_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../docs/qa-playbook.html",
);

function findCase(id: string): QaCase {
  const c = loadPlaybook(PLAYBOOK_PATH).find((x) => x.id === id);
  if (!c) throw new Error(`case ${id} not found in real playbook`);
  return c;
}

describe("runCase", () => {
  it("runs a simple foreground case with no server involved", async () => {
    const env01 = findCase("ENV-01");
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-caserunner-"));
    const result = await runCase(env01, { cwd });
    expect(result.id).toBe("ENV-01");
    expect(result.verdict).toBe("pass");
  });

  it("runs a real Smoke suite lifecycle: start server, use it, kill it via <pid> substitution", async () => {
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-caserunner-smoke-"));

    const smoke01 = findCase("SMOKE-01");
    const startResult = await runCase(smoke01, { cwd });
    expect(startResult.verdict).toBe("pass");

    const smoke02 = findCase("SMOKE-02");
    const pingResult = await runCase(smoke02, { cwd });
    expect(pingResult.verdict).toBe("pass");

    const pid = resolveRocketMemPid(6540);
    expect(pid).not.toBeNull();

    const smoke12 = findCase("SMOKE-12");
    const stopResult = await runCase(smoke12, { cwd, pid: pid! });
    expect(stopResult.verdict).toBe("pass");

    expect(resolveRocketMemPid(6540)).toBeNull();
  }, 20000);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './caseRunner.js'`.

- [ ] **Step 3: Implement `tools/qa-agent/src/caseRunner.ts`**

```typescript
import { spawn } from "node:child_process";
import type { QaCase } from "./playbook.js";
import { matchCase } from "./matcher.js";

export interface CaseResult {
  id: string;
  verdict: "pass" | "fail";
  reason?: string;
  actual: string;
  /** The shell's exit code, for diagnostics only — never used to judge pass/fail (the playbook
   * itself documents that `redis-cli` exits 0 even on a server-side error; matching is always
   * output-text based, per the spec). Undefined for a background-launch case, since its exit
   * code is never observed — the server is left running, not waited on. */
  exitCode?: number;
}

export interface CaseRunOptions {
  cwd: string;
  /** Substituted for a literal `<pid>` token in `steps`, if present (e.g. SMOKE-12). */
  pid?: number;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 15000;
const BACKGROUND_STARTUP_GRACE_MS = 1500;

/**
 * Heuristic for "this case's steps start a new background rocket-mem server" (an open item
 * flagged in the spec, resolved here): contains ROCKET_MEM_ADDR= and its last non-blank line
 * ends with a single `&` (not `&&`, a foreground command chain).
 */
function looksLikeBackgroundLaunch(steps: string): boolean {
  const trimmed = steps.trimEnd();
  return (
    steps.includes("ROCKET_MEM_ADDR=") &&
    trimmed.endsWith("&") &&
    !trimmed.endsWith("&&")
  );
}

interface ShellRun {
  output: string;
  exitCode?: number;
}

function runForeground(
  steps: string,
  cwd: string,
  timeoutMs: number,
): Promise<ShellRun> {
  return new Promise((resolve) => {
    const child = spawn("bash", ["-c", steps], {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString("utf8")));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
    }, timeoutMs);
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ output, exitCode: code ?? undefined });
    });
  });
}

/**
 * A background launch's own shell exits almost immediately after backgrounding the server, but
 * the server process keeps the same stdout pipe open — waiting for the pipe to close (as
 * runForeground does) would hang until the server itself exits. Instead, capture whatever
 * prints within a short grace window and move on, leaving the server running (so its exit code
 * is never observed — `exitCode` is always undefined for this path).
 */
function runBackgroundLaunch(steps: string, cwd: string): Promise<ShellRun> {
  return new Promise((resolve) => {
    const child = spawn("bash", ["-c", steps], {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (output += chunk.toString("utf8")));
    child.unref();
    setTimeout(() => resolve({ output }), BACKGROUND_STARTUP_GRACE_MS);
  });
}

export async function runCase(
  qaCase: QaCase,
  options: CaseRunOptions,
): Promise<CaseResult> {
  const steps =
    options.pid !== undefined
      ? qaCase.steps.replaceAll("<pid>", String(options.pid))
      : qaCase.steps;
  const { output: actual, exitCode } = looksLikeBackgroundLaunch(steps)
    ? await runBackgroundLaunch(steps, options.cwd)
    : await runForeground(steps, options.cwd, options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const match = matchCase(qaCase.id, qaCase.expected, actual);
  return { id: qaCase.id, verdict: match.verdict, reason: match.reason, actual, exitCode };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all `caseRunner.test.ts` cases green, all prior tests still green.

- [ ] **Step 5: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

- [ ] **Step 6: Confirm the live cluster is untouched**

Run: `ss -tlnp | grep -E ':(6379|6380|6381)\b'`
Expected: unaffected — this task only ever touches port 6540/6541/9340 (Smoke suite's own
documented ports) and always kills what it starts (the second test's last assertion confirms
port 6540 is free again).

---

### Task 3: `sessionRunner.ts` + `reporter.ts` + `index.ts` — the walking-skeleton CLI

**Files:**
- Create: `tools/qa-agent/src/sessionRunner.ts`
- Create: `tools/qa-agent/src/reporter.ts`
- Create: `tools/qa-agent/src/index.ts`
- Test: `tools/qa-agent/src/sessionRunner.test.ts`

**Interfaces:**
- Consumes: `QaCase` from `./playbook.js`, `loadPlaybook` from `./playbook.js`,
  `selectInScopeCases`/`IN_SCOPE_SUITES` from `./suites.js` (all Plan 1), `CaseResult`/`runCase`
  from `./caseRunner.js` (Task 2), `resolveRocketMemPid`/`killIfRocketMem` from
  `./processTracker.js` (Task 1).
- Produces: `export type RunCaseFn = (qaCase: QaCase, opts: { cwd: string; pid?: number;
  timeoutMs?: number }) => Promise<CaseResult>`, `export interface RunSuiteOptions { cwd: string;
  alreadyRun?: Set<string>; stopAfterCaseId?: string; pidPort?: number; onResult?: (result:
  CaseResult) => void; runCaseFn?: RunCaseFn }`, `export interface RunSuiteReport { ran:
  CaseResult[]; blockedBeforeTarget: boolean }`, `export async function runSuite(cases: QaCase[],
  options: RunSuiteOptions): Promise<RunSuiteReport>` — this is what Plan 3's `server.ts` will
  call for both bulk suite runs and on-demand Run-button clicks (passing `stopAfterCaseId` and a
  per-suite `alreadyRun` set it tracks itself).

- [ ] **Step 1: Write the failing tests**

Create `tools/qa-agent/src/sessionRunner.test.ts`:

```typescript
import { describe, it, expect, afterAll } from "vitest";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { loadPlaybook, type QaCase } from "./playbook.js";
import { selectInScopeCases } from "./suites.js";
import { runSuite } from "./sessionRunner.js";
import { killIfRocketMem } from "./processTracker.js";

const PLAYBOOK_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../docs/qa-playbook.html",
);

function fakeCase(id: string): QaCase {
  return {
    id,
    area: "X",
    title: id,
    precondition: "",
    steps: "",
    expected: "",
    notes: "",
    section: "Fake",
  };
}

describe("runSuite: on-demand chain semantics (fake runCaseFn)", () => {
  it("skips cases already in alreadyRun and stops after stopAfterCaseId", async () => {
    const cases = [fakeCase("A"), fakeCase("B"), fakeCase("C")];
    const calls: string[] = [];
    const runCaseFn = async (c: QaCase) => {
      calls.push(c.id);
      return { id: c.id, verdict: "pass" as const, actual: "" };
    };
    const report = await runSuite(cases, {
      cwd: "/tmp",
      alreadyRun: new Set(["A"]),
      stopAfterCaseId: "B",
      runCaseFn,
    });
    expect(calls).toEqual(["B"]);
    expect(report.ran.map((r) => r.id)).toEqual(["B"]);
    expect(report.blockedBeforeTarget).toBe(false);
  });

  it("stops the chain and reports blockedBeforeTarget when an earlier case fails", async () => {
    const cases = [fakeCase("A"), fakeCase("B"), fakeCase("C")];
    const runCaseFn = async (c: QaCase) => ({
      id: c.id,
      verdict: (c.id === "A" ? "fail" : "pass") as const,
      actual: "",
    });
    const report = await runSuite(cases, {
      cwd: "/tmp",
      stopAfterCaseId: "C",
      runCaseFn,
    });
    expect(report.ran.map((r) => r.id)).toEqual(["A"]);
    expect(report.blockedBeforeTarget).toBe(true);
  });

  it("runs the whole suite when stopAfterCaseId is omitted", async () => {
    const cases = [fakeCase("A"), fakeCase("B")];
    const runCaseFn = async (c: QaCase) => ({
      id: c.id,
      verdict: "pass" as const,
      actual: "",
    });
    const report = await runSuite(cases, { cwd: "/tmp", runCaseFn });
    expect(report.ran.map((r) => r.id)).toEqual(["A", "B"]);
    expect(report.blockedBeforeTarget).toBe(false);
  });
});

describe("runSuite: Smoke suite walking skeleton (real server)", () => {
  it("runs all 12 real Smoke suite cases end-to-end and all pass", async () => {
    const all = loadPlaybook(PLAYBOOK_PATH);
    const inScope = selectInScopeCases(all);
    const smokeCases = inScope.filter((c) => c.section === "Smoke suite");
    expect(smokeCases.length).toBe(12);

    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-smoke-"));
    const report = await runSuite(smokeCases, { cwd, pidPort: 6540 });

    const failures = report.ran.filter((r) => r.verdict === "fail");
    expect(failures).toEqual([]);
    expect(report.ran.length).toBe(12);
  }, 30000);

  afterAll(() => {
    killIfRocketMem(6540);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './sessionRunner.js'`.

- [ ] **Step 3: Implement `tools/qa-agent/src/sessionRunner.ts`**

```typescript
import type { QaCase } from "./playbook.js";
import { runCase, type CaseResult } from "./caseRunner.js";
import { resolveRocketMemPid } from "./processTracker.js";

export type RunCaseFn = (
  qaCase: QaCase,
  opts: { cwd: string; pid?: number; timeoutMs?: number },
) => Promise<CaseResult>;

export interface RunSuiteOptions {
  cwd: string;
  /** Case ids already known to have run successfully against this suite's still-live server
   * this session. Skipped rather than re-run. */
  alreadyRun?: Set<string>;
  /** Stop after this case id runs (inclusive). Omit to run every case in `cases`. */
  stopAfterCaseId?: string;
  /** Port to resolve a `<pid>` substitution against, for cases whose steps need it (e.g.
   * SMOKE-12). Omit for suites with no such case. */
  pidPort?: number;
  onResult?: (result: CaseResult) => void;
  /** Defaults to the real `runCase`. Tests inject a fake to exercise chain control flow without
   * a real server. */
  runCaseFn?: RunCaseFn;
}

export interface RunSuiteReport {
  ran: CaseResult[];
  /** True if the chain stopped early — because an earlier case failed — before ever reaching
   * `stopAfterCaseId`. False if `stopAfterCaseId` was reached (or was never set). */
  blockedBeforeTarget: boolean;
}

export async function runSuite(
  cases: QaCase[],
  options: RunSuiteOptions,
): Promise<RunSuiteReport> {
  const alreadyRun = options.alreadyRun ?? new Set<string>();
  const execute = options.runCaseFn ?? runCase;
  const ran: CaseResult[] = [];

  for (const qaCase of cases) {
    if (alreadyRun.has(qaCase.id)) {
      if (qaCase.id === options.stopAfterCaseId) break;
      continue;
    }

    const pid =
      options.pidPort !== undefined
        ? resolveRocketMemPid(options.pidPort) ?? undefined
        : undefined;
    const result = await execute(qaCase, { cwd: options.cwd, pid });
    ran.push(result);
    options.onResult?.(result);

    const reachedTarget = qaCase.id === options.stopAfterCaseId;
    if (result.verdict === "fail") {
      return {
        ran,
        blockedBeforeTarget: !reachedTarget && options.stopAfterCaseId !== undefined,
      };
    }
    if (reachedTarget) break;
  }

  return { ran, blockedBeforeTarget: false };
}
```

- [ ] **Step 4: Implement `tools/qa-agent/src/reporter.ts`**

```typescript
import type { CaseResult } from "./caseRunner.js";

export function printResult(result: CaseResult): void {
  const mark = result.verdict === "pass" ? "PASS" : "FAIL";
  console.log(`  [${mark}] ${result.id}${result.reason ? ` — ${result.reason}` : ""}`);
}

export function printFinalSummary(results: CaseResult[]): void {
  const passed = results.filter((r) => r.verdict === "pass").length;
  const failed = results.length - passed;
  console.log(`\n${passed}/${results.length} passed, ${failed} failed`);
}
```

- [ ] **Step 5: Implement `tools/qa-agent/src/index.ts`**

```typescript
#!/usr/bin/env node
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { loadPlaybook } from "./playbook.js";
import { selectInScopeCases, IN_SCOPE_SUITES } from "./suites.js";
import { runSuite } from "./sessionRunner.js";
import { killIfRocketMem } from "./processTracker.js";
import { printResult, printFinalSummary } from "./reporter.js";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../",
);
const PLAYBOOK_PATH = path.join(REPO_ROOT, "docs/qa-playbook.html");

// Only the Smoke suite is wired to a cleanup port so far. Later suites (Core, Transactions,
// Persistence, the Config/RMP/Observability shared block) get their own entries here as this
// CLI grows to cover them.
const SUITE_CLEANUP_PORT: Record<string, number> = {
  "Smoke suite": 6540,
};

async function main() {
  const args = process.argv.slice(2);
  const suiteFlagIndex = args.indexOf("--suite");
  const suiteName = suiteFlagIndex >= 0 ? args[suiteFlagIndex + 1] : undefined;
  const runAll = args.includes("--all");

  if (!suiteName && !runAll) {
    console.error("Usage: yarn cli --suite <section title> | --all");
    process.exitCode = 2;
    return;
  }

  const allCases = loadPlaybook(PLAYBOOK_PATH);
  const inScope = selectInScopeCases(allCases);
  const sectionsToRun = runAll
    ? IN_SCOPE_SUITES.map((s) => s.section)
    : [suiteName as string];

  for (const section of sectionsToRun) {
    const cases = inScope.filter((c) => c.section === section);
    if (cases.length === 0) {
      console.error(`No in-scope cases found for section "${section}"`);
      process.exitCode = 1;
      continue;
    }
    console.log(`\n=== ${section} (${cases.length} cases) ===`);
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-run-"));
    const report = await runSuite(cases, { cwd, onResult: printResult });
    printFinalSummary(report.ran);
    const port = SUITE_CLEANUP_PORT[section];
    if (port !== undefined) killIfRocketMem(port);
  }
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all `sessionRunner.test.ts` cases green (including the real 12-case Smoke suite
run), all prior tests (Tasks 1-2, Plan 1) still green.

- [ ] **Step 7: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

- [ ] **Step 8: Run the actual CLI end-to-end**

Add `"cli": "tsx src/index.ts"` to `tools/qa-agent/package.json`'s `scripts` (alongside the
existing `test`/`typecheck` scripts), then run:

```bash
cd tools/qa-agent && yarn cli --suite "Smoke suite"
```

Expected: prints `=== Smoke suite (12 cases) ===`, 12 `[PASS]` lines, then `12/12 passed, 0
failed`. Confirm the server was cleaned up: `ss -tlnp | grep -E ':(6540|6541|9340)\b'` prints
nothing.

- [ ] **Step 9: Confirm the live cluster is untouched**

Run: `ss -tlnp | grep -E ':(6379|6380|6381)\b'`
Expected: shows only the live cluster's own listeners — unaffected by anything in this task.

---

## Next plan

`docs/superpowers/plans/2026-09-13-qa-agent-3-live-ui.md` (not yet written) — the local HTTP+SSE
`server.ts` (`POST /api/result`, `POST /api/run {caseId}` for on-demand chained runs, `GET
/events`, `GET /api/status`), its per-suite session state (server running? which case ids ran
this session? — the `alreadyRun` set `sessionRunner.runSuite` already accepts), and the
`qa-playbook.html` changes: a **Run** button per case row wired to `POST /api/run`, and the
`EventSource('/events')` script block that mutates `state[id]`/`save()`/`render()` exactly like a
manual Pass/Fail click, so every result — whether from a bulk `yarn cli --all` run or a single
Run-button click — appears live. Also budget time in that plan (or a follow-on) for the
Observability matching-mode gap this spec's "Open items" section already flags: `...`/`NNNNN`
normalization and an "expected is a subset" containment check, needed before Observability's 14
cases can run clean.
