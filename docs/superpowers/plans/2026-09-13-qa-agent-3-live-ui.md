# QA Agent Live UI — Local Server, On-Demand Runs, Run Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `docs/qa-playbook.html` interactive. By the end of this plan: `yarn serve` starts a
local server that serves the real playbook page and pushes live results over SSE; a bulk
`yarn cli --suite "Smoke suite"` run (a separate process) shows up live in any open browser tab;
and each Smoke-suite case in the page gets a **Run** button that, on click, transparently runs
any not-yet-done earlier cases in that suite first, then the clicked case, streaming every result
as it happens — exactly the on-demand design the spec already lays out.

**Architecture:** `server.ts` is a plain Node `http` server (no framework — matches this project's
minimal-dependency convention) with four routes: static playbook serving, an SSE stream, a
POST endpoint for a separate process (the CLI) to push a result, and a POST endpoint that runs
an on-demand chain in-process via `sessionRunner.runSuite`. Per-suite session state (a
persistent scratch cwd + `alreadyRun` set, reused across on-demand clicks within one `yarn serve`
run) lives in memory inside `server.ts`. The browser-side addition to `qa-playbook.html` is a
small, self-contained script block: it tries to reach the server on load, and only shows Run
buttons and opens an `EventSource` if that succeeds — opening the file directly (`file://`, no
server) leaves the page working exactly as it does today, manual Pass/Fail clicks and all.

**Tech Stack:** Same as Plans 1-2 — TypeScript, Yarn, `vitest`, Node's built-in `http`/`node:fs`
modules (no new dependencies). `tools/qa-agent` is git-ignored (Plan 1's Global Constraints).
`docs/qa-playbook.html` is git-tracked — Task 3 in this plan is a real, committed change to it.

**Spec:** `docs/superpowers/specs/2026-09-12-qa-agent-design.md`

## Global Constraints

- **`tools/qa-agent` is git-ignored; `docs/qa-playbook.html` is NOT.** Do not `git add`/`git
  commit` anything under `tools/qa-agent/`. DO commit Task 3's change to `docs/qa-playbook.html`
  — it's a real, shared project artifact, same as the SMOKE-01/08/09/10/12 and OBS-02 fixes in
  Plans 1-2 were.
- Work happens directly in the main checkout at `/home/numericlabs/data/rocket/rocket-mem`
  (`tools/qa-agent` only durably exists there — see Plan 1's final review finding).
- **Only "Smoke suite" is wired up so far** (Plan 2's `index.ts` already gates `--all` and other
  `--suite` names behind a clear error — this plan does not change that scope). The on-demand
  Run button in Task 3 therefore only appears on Smoke-suite cases; `server.ts`'s `/api/status`
  endpoint reports exactly the case ids the button should appear for.
- The local server's default port is `4848` (`QA_AGENT_PORT` env var overrides it) — chosen
  clear of every rocket-mem-related port range already reserved in the spec's Safety section.
- Never `pkill -f rocket-mem`. Any suite server this plan starts (via `sessionRunner`/
  `caseRunner`, same as Plan 2) is only ever killed through `processTracker`'s confirmed-PID
  path.
- **Known limitation, not fixed in this plan:** `yarn serve` does not clean up a suite's
  still-running server on Ctrl+C/process exit — a server started via an on-demand Run click
  keeps running after you stop `yarn serve` (same way a human-started terminal session would).
  Confirm cleanliness manually (`ss -tlnp | grep -E ':(6540|6541|9340)\b'`) if you stop `yarn
  serve` mid-session.

---

### Task 1: `server.ts` core — static serving, SSE, status, and a separate process's result push

**Files:**
- Create: `tools/qa-agent/src/server.ts`
- Test: `tools/qa-agent/src/server.test.ts`
- Modify: `tools/qa-agent/package.json` (add a `"serve"` script)
- Modify: `tools/qa-agent/src/index.ts` (add a `--serve` branch)

**Interfaces:**
- Consumes: `loadPlaybook` from `./playbook.js`, `selectInScopeCases` from `./suites.js` (both
  Plan 1).
- Produces: `export const DEFAULT_PORT: number`, `export interface QaAgentServer { port: number;
  close(): Promise<void> }`, `export function startServer(port?: number):
  Promise<QaAgentServer>` — consumed by Task 2 (adds `/api/run` to the same file) and by
  `index.ts`'s `--serve` branch.

- [ ] **Step 1: Write the failing tests**

Create `tools/qa-agent/src/server.test.ts`:

```typescript
import { describe, it, expect, afterEach } from "vitest";
import { startServer, type QaAgentServer } from "./server.js";

let server: QaAgentServer | undefined;

afterEach(async () => {
  if (server) {
    await server.close();
    server = undefined;
  }
});

describe("server", () => {
  it("serves the real qa-playbook.html", async () => {
    server = await startServer(0);
    const res = await fetch(`http://127.0.0.1:${server.port}/qa-playbook.html`);
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toContain("rocket-mem QA Playbook");
  });

  it("GET /api/status returns the 114 in-scope case ids and empty results initially", async () => {
    server = await startServer(0);
    const res = await fetch(`http://127.0.0.1:${server.port}/api/status`);
    const body = await res.json();
    expect(body.inScope.length).toBe(114);
    expect(body.results).toEqual({});
  });

  it("POST /api/result records the result and broadcasts it over /events", async () => {
    server = await startServer(0);
    const eventsRes = await fetch(`http://127.0.0.1:${server.port}/events`);
    const reader = eventsRes.body!.getReader();
    const decoder = new TextDecoder();

    // Consume the initial ":ok" comment line the SSE stream opens with, before posting.
    await reader.read();

    await fetch(`http://127.0.0.1:${server.port}/api/result`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: "SMOKE-01", verdict: "pass" }),
    });

    const { value } = await reader.read();
    const chunk = decoder.decode(value);
    expect(chunk).toContain('"id":"SMOKE-01"');
    expect(chunk).toContain('"verdict":"pass"');

    const statusRes = await fetch(`http://127.0.0.1:${server.port}/api/status`);
    const statusBody = await statusRes.json();
    expect(statusBody.results["SMOKE-01"]).toBe("pass");

    await reader.cancel();
  });

  it("returns 404 for an unknown path", async () => {
    server = await startServer(0);
    const res = await fetch(`http://127.0.0.1:${server.port}/nope`);
    expect(res.status).toBe(404);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test src/server.test.ts`
Expected: FAIL — `Cannot find module './server.js'`.

- [ ] **Step 3: Implement `tools/qa-agent/src/server.ts`**

```typescript
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { loadPlaybook } from "./playbook.js";
import { selectInScopeCases } from "./suites.js";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../",
);
const PLAYBOOK_PATH = path.join(REPO_ROOT, "docs/qa-playbook.html");

export const DEFAULT_PORT = Number(process.env.QA_AGENT_PORT ?? 4848);

export interface QaAgentServer {
  port: number;
  close(): Promise<void>;
}

interface SseClient {
  res: ServerResponse;
}

function readJsonBody(req: IncomingMessage): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    let raw = "";
    req.on("data", (chunk: Buffer) => (raw += chunk.toString("utf8")));
    req.on("end", () => {
      try {
        resolve(raw ? JSON.parse(raw) : {});
      } catch (err) {
        reject(err);
      }
    });
    req.on("error", reject);
  });
}

export function startServer(port: number = DEFAULT_PORT): Promise<QaAgentServer> {
  const allCases = loadPlaybook(PLAYBOOK_PATH);
  const inScopeIds = selectInScopeCases(allCases).map((c) => c.id);

  const results: Record<string, "pass" | "fail"> = {};
  const clients = new Set<SseClient>();

  function broadcast(id: string, verdict: "pass" | "fail"): void {
    results[id] = verdict;
    const payload = `data: ${JSON.stringify({ id, verdict })}\n\n`;
    for (const client of clients) client.res.write(payload);
  }

  async function handleRequest(
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<void> {
    const url = new URL(req.url ?? "/", "http://localhost");

    if (
      req.method === "GET" &&
      (url.pathname === "/" || url.pathname === "/qa-playbook.html")
    ) {
      res.setHeader("Content-Type", "text/html; charset=utf-8");
      res.end(readFileSync(PLAYBOOK_PATH));
      return;
    }

    if (req.method === "GET" && url.pathname === "/events") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      });
      res.write(":ok\n\n");
      const client: SseClient = { res };
      clients.add(client);
      req.on("close", () => clients.delete(client));
      return;
    }

    if (req.method === "GET" && url.pathname === "/api/status") {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ inScope: inScopeIds, results }));
      return;
    }

    if (req.method === "POST" && url.pathname === "/api/result") {
      const body = await readJsonBody(req);
      if (
        typeof body.id === "string" &&
        (body.verdict === "pass" || body.verdict === "fail")
      ) {
        broadcast(body.id, body.verdict);
      }
      res.statusCode = 204;
      res.end();
      return;
    }

    res.statusCode = 404;
    res.end("not found");
  }

  const server = createServer((req, res) => {
    handleRequest(req, res).catch((err) => {
      res.statusCode = 500;
      res.end(String(err));
    });
  });

  return new Promise((resolve) => {
    server.listen(port, () => {
      const address = server.address();
      const actualPort =
        typeof address === "object" && address ? address.port : port;
      resolve({
        port: actualPort,
        close: () =>
          new Promise((res) => {
            for (const client of clients) client.res.end();
            server.close(() => res());
          }),
      });
    });
  });
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all 4 `server.test.ts` cases green, all prior tests (Plans 1-2, 36 tests) still
green.

- [ ] **Step 5: Add the `serve` script and wire `--serve` into `index.ts`**

In `tools/qa-agent/package.json`, add to `scripts`: `"serve": "tsx src/index.ts --serve"`.

In `tools/qa-agent/src/index.ts`, add near the top of `main()`, before the existing
`--suite`/`--all` usage check:

```typescript
  if (args.includes("--serve")) {
    const { startServer, DEFAULT_PORT } = await import("./server.js");
    const server = await startServer();
    console.log(`qa-agent serving at http://127.0.0.1:${server.port}/qa-playbook.html`);
    return;
  }
```

(Using a dynamic `import()` here, not a top-level one, keeps `index.ts`'s existing `--suite`/
`--all` path from paying for loading `server.ts` when it isn't needed — a minor, low-risk choice;
a static top-level import would also be correct if you find it clearer, either is acceptable.)

- [ ] **Step 6: Run the real server manually and confirm it serves the page**

Run this as a single shell command (background job control does not persist across separate
tool calls — start, curl, and stop it all in one):

```bash
cd tools/qa-agent
yarn serve &
SERVE_PID=$!
sleep 1
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:4848/qa-playbook.html
kill "$SERVE_PID"
```

Expected: `200`. `$SERVE_PID` is the `tsx`/node process itself, not a rocket-mem server —
nothing suite-related has started yet in this task, so an ordinary `kill` is all that's needed.

- [ ] **Step 7: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

---

### Task 2: On-demand chained runs (`/api/run`) and the CLI's best-effort push to the server

**Files:**
- Modify: `tools/qa-agent/src/suites.ts` (add `SUITE_SERVER_PORT`)
- Modify: `tools/qa-agent/src/server.ts` (add suite session state + `/api/run`)
- Modify: `tools/qa-agent/src/index.ts` (use `SUITE_SERVER_PORT`; POST results to the server when
  reachable)
- Test: `tools/qa-agent/src/server.test.ts` (extend with on-demand-chain and CLI-push cases)

**Interfaces:**
- Consumes: `runSuite`/`RunSuiteReport` from `./sessionRunner.js` (Plan 2), `resolveRocketMemPid`
  from `./processTracker.js` (Plan 2).
- Produces: `SUITE_SERVER_PORT: Record<string, number>` exported from `suites.ts` (currently just
  `{"Smoke suite": 6540}`) — replaces `index.ts`'s local `SUITE_CLEANUP_PORT` constant from
  Plan 2 (same values, one source of truth now that `server.ts` needs it too).

- [ ] **Step 1: Add `SUITE_SERVER_PORT` to `suites.ts`**

In `tools/qa-agent/src/suites.ts`, add near `IN_SCOPE_SUITES`:

```typescript
// The RESP port each in-scope suite's server listens on, once started — needed for <pid>
// substitution and end-of-run cleanup. Only suites whose server actually starts today (see
// Plan 2's index.ts scoping) have an entry; extend this as later plans wire up more suites.
export const SUITE_SERVER_PORT: Record<string, number> = {
  "Smoke suite": 6540,
};
```

- [ ] **Step 2: Replace `index.ts`'s local port map with the shared one**

In `tools/qa-agent/src/index.ts`, remove the local `SUITE_CLEANUP_PORT` constant Plan 2 added,
import `SUITE_SERVER_PORT` from `./suites.js` instead, and use it everywhere
`SUITE_CLEANUP_PORT` was used. Behavior is identical (same key, same value) — this is a pure
rename/relocation, not a logic change.

- [ ] **Step 3: Write the failing tests for `/api/run`**

Add to `tools/qa-agent/src/server.test.ts` (new `describe` block, alongside the existing one):

```typescript
describe("server: on-demand chained runs", () => {
  it("running a case partway through Smoke suite runs every not-yet-done earlier case first", async () => {
    server = await startServer(0);
    const eventsRes = await fetch(`http://127.0.0.1:${server.port}/events`);
    const reader = eventsRes.body!.getReader();
    const decoder = new TextDecoder();
    const seen: Array<{ id: string; verdict: string }> = [];
    let buffer = "";

    async function drainUntil(count: number, timeoutMs = 15000): Promise<void> {
      const deadline = Date.now() + timeoutMs;
      while (seen.length < count) {
        if (Date.now() > deadline) throw new Error(`timed out waiting for ${count} events`);
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const parts = buffer.split("\n\n");
        buffer = parts.pop() ?? "";
        for (const part of parts) {
          const match = part.match(/^data: (.+)$/m);
          if (match) seen.push(JSON.parse(match[1]));
        }
      }
    }
    await reader.read(); // consume the initial ":ok" line

    const runRes = await fetch(`http://127.0.0.1:${server.port}/api/run`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ caseId: "SMOKE-03" }),
    });
    expect(runRes.status).toBe(200);
    await drainUntil(3);
    expect(seen.map((s) => s.id)).toEqual(["SMOKE-01", "SMOKE-02", "SMOKE-03"]);
    expect(seen.every((s) => s.verdict === "pass")).toBe(true);

    // Clicking a later case now only runs what hasn't run yet this session.
    const runRes2 = await fetch(`http://127.0.0.1:${server.port}/api/run`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ caseId: "SMOKE-05" }),
    });
    expect(runRes2.status).toBe(200);
    await drainUntil(5);
    expect(seen.map((s) => s.id)).toEqual([
      "SMOKE-01",
      "SMOKE-02",
      "SMOKE-03",
      "SMOKE-04",
      "SMOKE-05",
    ]);

    await reader.cancel();
  }, 40000);

  it("returns 404 for an unknown or out-of-scope case id", async () => {
    server = await startServer(0);
    const res = await fetch(`http://127.0.0.1:${server.port}/api/run`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ caseId: "REPL-01" }),
    });
    expect(res.status).toBe(404);
  });

  afterEach(async () => {
    const { killIfRocketMem } = await import("./processTracker.js");
    killIfRocketMem(6540);
  });
});
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test src/server.test.ts`
Expected: FAIL — `/api/run` currently 404s unconditionally (Task 1 only implemented the routes
listed there), so `runRes.status` is `404`, not `200`.

- [ ] **Step 5: Implement suite session state and `/api/run` in `server.ts`**

Add to `tools/qa-agent/src/server.ts` (imports first, then inside `startServer`, before the
`handleRequest` function):

```typescript
import { mkdtempSync, existsSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { runSuite } from "./sessionRunner.js";
import { SUITE_SERVER_PORT } from "./suites.js";
```

```typescript
  const inScopeCases = selectInScopeCases(allCases);
  const caseById = new Map(inScopeCases.map((c) => [c.id, c]));
  const casesBySection = new Map<string, typeof inScopeCases>();
  for (const c of inScopeCases) {
    const list = casesBySection.get(c.section) ?? [];
    list.push(c);
    casesBySection.set(c.section, list);
  }

  interface SuiteSession {
    cwd: string;
    alreadyRun: Set<string>;
  }
  const suiteSessions = new Map<string, SuiteSession>();

  function suiteSessionFor(section: string): SuiteSession {
    let session = suiteSessions.get(section);
    if (!session) {
      const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-serve-"));
      const targetBinary = path.join(REPO_ROOT, "target/release/rocket-mem");
      if (!existsSync(targetBinary)) {
        throw new Error(
          `${targetBinary} not found — run \`cargo build --release --bin rocket-mem\` from the repo root first.`,
        );
      }
      symlinkSync(path.join(REPO_ROOT, "target"), path.join(cwd, "target"), "dir");
      session = { cwd, alreadyRun: new Set() };
      suiteSessions.set(section, session);
    }
    return session;
  }
```

(Note `inScopeIds` from Task 1 can now be derived as `[...caseById.keys()]` — keep both if you
prefer minimal diff, or consolidate; either is fine as long as `/api/status` still reports all
114 ids.)

In `handleRequest`, add before the final `res.statusCode = 404` fallback:

```typescript
    if (req.method === "POST" && url.pathname === "/api/run") {
      const body = await readJsonBody(req);
      const caseId = body.caseId;
      const target = typeof caseId === "string" ? caseById.get(caseId) : undefined;
      if (!target) {
        res.statusCode = 404;
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify({ error: `unknown or out-of-scope case id: ${caseId}` }));
        return;
      }
      const sectionCases = casesBySection.get(target.section) ?? [];
      const session = suiteSessionFor(target.section);
      const pidPort = SUITE_SERVER_PORT[target.section];
      const report = await runSuite(sectionCases, {
        cwd: session.cwd,
        alreadyRun: session.alreadyRun,
        stopAfterCaseId: caseId,
        pidPort,
        onResult: (r) => {
          if (r.verdict === "pass") session.alreadyRun.add(r.id);
          broadcast(r.id, r.verdict);
        },
      });
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify(report));
      return;
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all `server.test.ts` cases green (including the real on-demand chain against
the Smoke suite), all prior tests still green.

- [ ] **Step 7: Wire the CLI's bulk run to push results to the server when reachable**

In `tools/qa-agent/src/index.ts`, change the `onResult` passed to `runSuite` in the per-section
loop so it also attempts (best-effort, never blocking or failing the run) to notify a locally
running server:

```typescript
    const report = await runSuite(cases, {
      cwd,
      pidPort: SUITE_SERVER_PORT[section],
      onResult: (r) => {
        printResult(r);
        fetch(`http://127.0.0.1:${DEFAULT_PORT}/api/result`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ id: r.id, verdict: r.verdict }),
        }).catch(() => {
          /* no server listening — the runner works standalone, per the spec */
        });
      },
    });
```

Import `DEFAULT_PORT` from `./server.js` at the top of `index.ts` (a static import here is fine
— `index.ts` already needs `server.js` dynamically for `--serve`, but importing just the
constant statically doesn't pull in the HTTP server logic's side effects since `startServer` is
only called when actually invoked).

- [ ] **Step 8: Manual verification of the CLI-to-server push**

Run this as a single shell command (background job control does not persist across separate
tool calls — start the server, run the CLI, check status, and stop the server all in one):

```bash
cd tools/qa-agent
yarn serve &
SERVE_PID=$!
sleep 1
yarn cli --suite "Smoke suite"
curl -s http://127.0.0.1:4848/api/status | head -c 400
echo
kill "$SERVE_PID"
```

Expected: the CLI prints its usual `12/12 passed, 0 failed`, and the `curl` afterward shows a
`results` object with all 12 Smoke case ids recorded — confirming the CLI (a separate process
from `yarn serve`) successfully pushed its results over HTTP. `$SERVE_PID` is the plain Node
`yarn serve` process, not a rocket-mem server — ordinary `kill` is fine.

- [ ] **Step 9: Typecheck and live-cluster check**

Run: `cd tools/qa-agent && yarn typecheck` — expect no errors.
Run: `ss -tlnp | grep -E ':(6379|6380|6381)\b'` — expect only the live cluster's own listeners,
unaffected.

---

### Task 3: The Run button in `docs/qa-playbook.html`

**Files:**
- Modify: `docs/qa-playbook.html` (git-tracked — commit this task's change)

**Interfaces:**
- Consumes: `server.ts`'s `GET /api/status`, `POST /api/run`, `GET /events` (Tasks 1-2).
- Produces: no new exported interface — this is the browser-side integration point the spec's
  "On-demand single-case runs" section describes.

- [ ] **Step 1: Add the Run button to each case's verdict row**

In `docs/qa-playbook.html`'s `render()` function, find:

```javascript
        html += '<div class="verdict">'
          + '<button class="vb p" data-v="pass" aria-pressed="'+(st==='pass')+'">Pass</button>'
          + '<button class="vb f" data-v="fail" aria-pressed="'+(st==='fail')+'">Fail</button>'
          + '<button class="vb c" data-v="todo">Clear</button></div>';
```

Replace it with:

```javascript
        html += '<div class="verdict">'
          + (runnable.has(c.id) ? '<button class="vb run" data-run="'+attr(c.id)+'">Run</button>' : '')
          + '<button class="vb p" data-v="pass" aria-pressed="'+(st==='pass')+'">Pass</button>'
          + '<button class="vb f" data-v="fail" aria-pressed="'+(st==='fail')+'">Fail</button>'
          + '<button class="vb c" data-v="todo">Clear</button></div>';
```

- [ ] **Step 2: Add the `runnable` set and `triggerRun`/`connectLive` functions**

Near the top of the script, alongside the existing `var filter = 'all', query = '', allOpen =
false;` line, add:

```javascript
  var runnable = new Set();
```

Add these two functions after `setVerdict` (they're used by the click handler added in Step 3
and called once at the bottom of the script in Step 4):

```javascript
  function triggerRun(id){
    var btn = list.querySelector('.vb.run[data-run="'+id+'"]');
    if (btn){ btn.disabled = true; btn.textContent = 'Running…'; }
    fetch('/api/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ caseId: id })
    }).catch(function(){ /* results, if any, still arrive over /events */ })
      .finally(function(){
        var again = list.querySelector('.vb.run[data-run="'+id+'"]');
        if (again){ again.disabled = false; again.textContent = 'Run'; }
      });
  }

  function connectLive(){
    fetch('/api/status').then(function(r){
      if (!r.ok) throw new Error('no server');
      return r.json();
    }).then(function(status){
      runnable = new Set(status.inScope);
      Object.keys(status.results).forEach(function(id){ state[id] = status.results[id]; });
      save();
      render();
      var es = new EventSource('/events');
      es.onmessage = function(e){
        var msg = JSON.parse(e.data);
        if (!msg.id) return;
        state[msg.id] = msg.verdict;
        save();
        render();
      };
    }).catch(function(){
      // No server reachable (e.g. the page was opened directly as a file) — the page still
      // works exactly as it does today, just without Run buttons or live updates.
    });
  }
```

- [ ] **Step 3: Wire the Run button into the existing delegated click handler**

Find, in the `list.addEventListener('click', ...)` handler:

```javascript
    var cp = e.target.closest('.copy');
    if (cp){ copyFrom(cp); return; }
    var vb = e.target.closest('.vb');
    if (vb) setVerdict(art, vb.getAttribute('data-v'));
```

Replace with:

```javascript
    var cp = e.target.closest('.copy');
    if (cp){ copyFrom(cp); return; }
    var runBtn = e.target.closest('.vb.run');
    if (runBtn){ triggerRun(runBtn.getAttribute('data-run')); return; }
    var vb = e.target.closest('.vb');
    if (vb) setVerdict(art, vb.getAttribute('data-v'));
```

(Checking `.vb.run` first, before the generic `.vb` check, is what keeps a Run-button click from
also being mistakenly treated as a Pass/Fail/Clear click — `.vb.run` still inherits `.vb`'s base
button styling via CSS, it just needs its own branch in the click handler.)

- [ ] **Step 4: Call `connectLive()` on load**

Find the last two lines of the script:

```javascript
  render();
})();
```

Replace with:

```javascript
  render();
  connectLive();
})();
```

- [ ] **Step 5: Add minimal CSS for the Run button**

Find, in the `<style>` block:

```css
.vb.c{margin-left:auto;font-weight:500}
.vb.c:hover{color:var(--ink)}
```

Add after it:

```css
.vb.run{color:var(--accent);border-color:var(--accent-line)}
.vb.run:hover{background:var(--accent);border-color:var(--accent);color:#fff}
.vb.run:disabled{opacity:.6;cursor:default}
```

- [ ] **Step 6: Manual verification**

Run `cd tools/qa-agent && yarn serve`, open `http://127.0.0.1:4848/qa-playbook.html` in a
browser. Confirm: no Run buttons visible before the page finishes its initial `/api/status`
fetch is misleading to test visually — instead, confirm Run buttons appear on every Smoke-suite
case (and only Smoke-suite cases) shortly after the page loads. Click **Run** on `SMOKE-05`:
confirm `SMOKE-01` through `SMOKE-05`'s ticks flip to green in order, live, without a page reload.
Then open the same URL in a second browser tab and click **Run** on `SMOKE-08` in the first tab
— confirm the second tab's ticks update too (both tabs share one `/events` stream). Close both
tabs, stop `yarn serve`, and confirm the live cluster is untouched:
`ss -tlnp | grep -E ':(6379|6380|6381)\b'`.

Also verify graceful degradation: open `docs/qa-playbook.html` directly as a `file://` URL (no
server running) — confirm the page loads and manual Pass/Fail/Clear clicks still work exactly as
before, with no Run buttons and no console errors beyond the expected failed `fetch` to
`/api/status` (open devtools to confirm nothing throws unhandled).

- [ ] **Step 7: Commit**

```bash
git add docs/qa-playbook.html
git commit -m "$(cat <<'EOF'
Add a live Run button to Smoke-suite cases in the QA playbook

Each Smoke-suite case now gets a Run button, wired to qa-agent's
local server (tools/qa-agent, started via `yarn serve`). Clicking it
transparently runs any not-yet-done earlier cases in the suite first,
then the clicked case, streaming every result live over SSE -- the
same visual ticks a manual Pass/Fail click already drives. Opening
the page without the server running (or as a file:// URL) still
works exactly as before; the button only appears once a live server
is actually reachable.
EOF
)"
```

---

## Next plan

No further plan is queued yet. Follow-on work, in priority order per the final reviews of Plans
1-2 (already recorded in the spec's "Open items" section, not repeated here): wiring up the
remaining 7 in-scope suites' prose-derived setup instructions so `--all`/other `--suite` names
work; the Observability `...`/`NNNNN` matching-mode gap; and the wider stale-banner cleanup
(`PERSIST-02`..`05`, `CFG-01`..`07`, `RMP-01`). Any of these could become "Plan 4" when picked
up.
