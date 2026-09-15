# Environment Setup Suite Automation — Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/qa-agent` recognize and run the `Environment setup` suite, so `docs/qa-playbook.html` shows "Run" buttons for `ENV-01`, `ENV-02`, `ENV-03`, `ENV-05` the same way it does for `Core data types and keys` — a separate, independently-scoped chain from the TLS suite automation chain (`docs/superpowers/plans/2026-09-15-tls-suite-automation-*.md`), started in parallel per the user's own two requests.

**Architecture:** `Environment setup` is already partly registered — `tools/qa-agent/src/suites.ts`'s `IN_SCOPE_SUITES` already lists it with `caseIds: ["ENV-01", "ENV-02", "ENV-03", "ENV-05", "ENV-06"]`. It has never been runnable, though: `server.ts`'s `runnableCases` filter (`c.section in SUITE_SERVER_PORT`) requires a port entry, and `Environment setup` has none — because, unlike every other suite, its cases (`redis-cli --version`, `openssl version`, ...) never start a `rocket-mem` server at all. This plan adds a new `NO_SERVER_SUITES` set to `suites.ts` for exactly this shape, and teaches `server.ts` to treat it as a third case alongside "reuses another suite's server" (Core/Transactions) and "starts its own server" (everything else): "needs no server, no port gates apply." It also excludes `ENV-06` (clone-and-build-from-source) from the automated scope — a real, slow, network-dependent side effect, not a repeatable version check, same reasoning this file already used to exclude `TXN-10`/`CLUSTER-10`.

**Tech Stack:** Node.js/TypeScript (`tools/qa-agent`), `yarn`.

**Spec:** none (bounded task; independent of the TLS chain).

## Global Constraints

- `tools/qa-agent` uses Yarn — every command runs as `yarn <script>` from inside `tools/qa-agent`.
- Do not touch any other suite's entries in `tools/qa-agent/src/suites.ts` or `server.ts`.
- `tools/` is gitignored (`.gitignore:44`) — nothing in this plan is committed; every edit is under `tools/qa-agent`, and `docs/qa-playbook.md`/`docs/qa-playbook.html` are untouched (no case text changes needed — see Task 1's own reasoning for why `ENV-06` is excluded via `suites.ts` alone, not by editing the doc).
- No "Clean environment" button for `Environment setup`: its in-scope cases (`ENV-01`/`02`/`03`/`05`) are read-only version checks that start no process and write no file — there is nothing for that button to clear. Do not add it to `clearableSections`/`SUITE_STARTS_OWN_SERVER`. This is a deliberate scope decision for this plan, not an oversight — flag it to the user in the handoff rather than silently building a button that would do nothing.

---

### Task 1: Exclude `ENV-06` and add `NO_SERVER_SUITES` to `suites.ts`

**Files:**
- Modify: `tools/qa-agent/src/suites.ts` (`IN_SCOPE_SUITES`'s `"Environment setup"` entry at `tools/qa-agent/src/suites.ts:18`-`21`; add `NO_SERVER_SUITES` after `SUITE_STARTS_OWN_SERVER` at `:187`)
- Test: `tools/qa-agent/src/suites.test.ts`

**Interfaces:**
- Produces: `NO_SERVER_SUITES: Set<string>`, read by `server.ts` in Task 2. `selectInScopeCases()` now returns only `ENV-01`/`02`/`03`/`05` for `Environment setup` (not `ENV-06`).

- [ ] **Step 1: Write the failing tests**

In `tools/qa-agent/src/suites.test.ts`, add:

```typescript
it("excludes ENV-06 (build from source) from Environment setup's automated scope", () => {
  const envCases = selectInScopeCases(allCases).filter((c) => c.section === "Environment setup");
  expect(envCases.map((c) => c.id)).toEqual(["ENV-01", "ENV-02", "ENV-03", "ENV-05"]);
});

it("Environment setup needs no server", () => {
  expect(NO_SERVER_SUITES.has("Environment setup")).toBe(true);
  expect(SUITE_STARTS_OWN_SERVER.has("Environment setup")).toBe(false);
  expect("Environment setup" in SUITE_SERVER_PORT).toBe(false);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```

Expected: FAIL — the first test sees `ENV-06` still included (current `envCases` is 5 long, not 4); `NO_SERVER_SUITES` doesn't exist yet.

- [ ] **Step 3: Make the changes**

In `tools/qa-agent/src/suites.ts`, replace the `Environment setup` entry:

```typescript
  {
    section: "Environment setup",
    caseIds: ["ENV-01", "ENV-02", "ENV-03", "ENV-05", "ENV-06"],
  },
```

with:

```typescript
  {
    // ENV-06 (build from source) is excluded from automation: it clones a fresh repo over the
    // network and does a cold release build (~37s per its own Notes) — a real, slow, network-
    // dependent side effect, not a repeatable local check, unsuitable for a "Run" button click.
    // Same reasoning as this file's own TXN-10/CLUSTER-10 exclusions above. It stays in the doc
    // for manual QA; ENV-01/02/03/05 (locally-installed tool versions) are what's automated.
    section: "Environment setup",
    caseIds: ["ENV-01", "ENV-02", "ENV-03", "ENV-05"],
  },
```

Then add, after the closing `]);` of `SUITE_STARTS_OWN_SERVER`:

```typescript
// Suites whose in-scope cases need no server/process at all — pure environment checks (tool
// versions, daemon reachability). Distinct from SUITE_STARTS_OWN_SERVER (also starts a process)
// and from a reuse suite like Core/Transactions (needs someone ELSE's server already running):
// a suite in this set needs neither. server.ts's runnableCases filter and /api/run's pre-flight
// gates both check this set to skip every port-related check that doesn't apply to them.
export const NO_SERVER_SUITES = new Set<string>(["Environment setup"]);
```

- [ ] **Step 4: Run the tests to verify they pass, then the full suite**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green.

---

### Task 2: Teach `server.ts` to run a no-server suite

**Files:**
- Modify: `tools/qa-agent/src/server.ts` (import block `:7`-`14`; `runnableCases` at `:58`; `suiteSessionFor`'s binary check at `:129`-`135`; `/api/run`'s 400 gate at `:223`-`232`; `/api/run`'s first port gate at `:244`)
- Test: `tools/qa-agent/src/server.test.ts`

**Interfaces:**
- Consumes: `NO_SERVER_SUITES` from `./suites.js` (Task 1).
- Produces: `/api/status`'s `runnable` array now includes `ENV-01`/`02`/`03`/`05`; `POST /api/run {"caseId":"ENV-01"}` succeeds with no port/server involved.

- [ ] **Step 1: Write the failing tests**

In `tools/qa-agent/src/server.test.ts`, add:

```typescript
it("runs Environment setup cases with no server or port involved at all", async () => {
  server = await startServer(0);
  const res = await fetch(`http://127.0.0.1:${server.port}/api/run`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ caseId: "ENV-01" }),
  });
  expect(res.status).toBe(200);
  const report = await res.json();
  expect(report.ran.map((r: { id: string }) => r.id)).toContain("ENV-01");
  expect(report.ran.find((r: { id: string }) => r.id === "ENV-01").verdict).toBe("pass");
});

it("Environment setup's cases are runnable but the suite is never clearable", async () => {
  server = await startServer(0);
  const res = await fetch(`http://127.0.0.1:${server.port}/api/status`);
  const body = await res.json();
  expect(body.runnable).toEqual(
    expect.arrayContaining(["ENV-01", "ENV-02", "ENV-03", "ENV-05"]),
  );
  expect(body.runnable).not.toContain("ENV-06");
  expect(body.clearable).not.toContain("Environment setup");
});
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd tools/qa-agent && yarn test src/server.test.ts -t "Environment setup"
```

Expected: FAIL — `/api/run` returns 400 (`"Environment setup" isn't wired up for on-demand runs yet`), and `runnable` doesn't contain any `ENV-*` id at all.

- [ ] **Step 3: Make the changes**

In `tools/qa-agent/src/server.ts`:

1. Add `NO_SERVER_SUITES` to the existing `./suites.js` import list.

2. Change the `runnableCases` line:

```typescript
  const runnableCases = inScopeCases.filter((c) => c.section in SUITE_SERVER_PORT);
```

to:

```typescript
  const runnableCases = inScopeCases.filter(
    (c) => c.section in SUITE_SERVER_PORT || NO_SERVER_SUITES.has(c.section),
  );
```

3. In `suiteSessionFor`, guard the binary-existence check and symlink so a no-port suite doesn't need a release build to run its (server-free) cases — change:

```typescript
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-serve-"));
    const targetBinary = path.join(REPO_ROOT, "target/release/rocket-mem");
    if (!existsSync(targetBinary)) {
      throw new Error(
        `${targetBinary} not found — run \`cargo build --release --bin rocket-mem\` from the repo root first.`,
      );
    }
    symlinkSync(path.join(REPO_ROOT, "target"), path.join(cwd, "target"), "dir");
```

to:

```typescript
    const cwd = mkdtempSync(path.join(tmpdir(), "qa-agent-serve-"));
    // A no-server suite (Environment setup) never invokes the binary under test at all — its
    // own cases check locally-installed tool versions, not rocket-mem itself.
    if (port !== undefined) {
      const targetBinary = path.join(REPO_ROOT, "target/release/rocket-mem");
      if (!existsSync(targetBinary)) {
        throw new Error(
          `${targetBinary} not found — run \`cargo build --release --bin rocket-mem\` from the repo root first.`,
        );
      }
      symlinkSync(path.join(REPO_ROOT, "target"), path.join(cwd, "target"), "dir");
    }
```

4. In the `/api/run` handler, widen the initial 400 gate:

```typescript
      if (!(target.section in SUITE_SERVER_PORT)) {
```

to:

```typescript
      if (!(target.section in SUITE_SERVER_PORT) && !NO_SERVER_SUITES.has(target.section)) {
```

5. Guard the first port-liveness gate (the only one a no-server suite would otherwise wrongly hit — the second gate already only applies to `SUITE_STARTS_OWN_SERVER` members, which `Environment setup` isn't):

```typescript
      if (!runPromise && !SUITE_STARTS_OWN_SERVER.has(section) && resolveRocketMemPid(port) === null) {
```

to:

```typescript
      if (port !== undefined && !runPromise && !SUITE_STARTS_OWN_SERVER.has(section) && resolveRocketMemPid(port) === null) {
```

`pidPort`/`env` further down already handle an `undefined` port defensively (`pidPort !== undefined` guards in the existing `onResult` callback) — no further changes needed there.

- [ ] **Step 4: Run the tests to verify they pass, then the full suite**

```bash
cd tools/qa-agent && yarn test src/server.test.ts -t "Environment setup"
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green.

---

### Task 3: Full regression and handoff note

**Files:** none modified — verification only.

**Interfaces:** N/A.

- [ ] **Step 1: Full regression**

```bash
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green — confirms Task 1/Task 2 didn't regress the TLS chain's own registration or any other suite.

- [ ] **Step 2: Confirm `docs/qa-playbook.html`/`docs/qa-playbook.md` need no changes for this plan**

```bash
git status --short docs/qa-playbook.md docs/qa-playbook.html
```

Expected: no changes from this plan (everything so far lives in gitignored `tools/qa-agent`) — if this shows unrelated pre-existing changes from other in-progress work, that's not this plan's concern; just confirm nothing *new* appeared.

- [ ] **Step 3: Record the handoff note**

No Clean Environment button will appear for `Environment setup` — see this plan's Global Constraints for why. If the user disagrees once they see the running UI, that's a follow-up, not something to guess at now.

## Next plan

`docs/superpowers/plans/2026-09-15-env-suite-automation-2-live-verify.md` — run `ENV-01`/`02`/`03`/`05` live through `tools/qa-agent`, confirm this machine's actual installed tool versions match the doc's literal `Expected` text (fix any real mismatch — a stale doc literal, not a wildcard, per `ENV-01`'s own "exact version will differ per machine" caveat only mattering machine-to-machine, not run-to-run on this one machine), and confirm the Run button works end to end in `docs/qa-playbook.html`.
