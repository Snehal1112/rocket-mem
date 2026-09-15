# TLS Suite Automation — Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/qa-agent` actually recognize and run the `TLS` suite, so `docs/qa-playbook.html` shows "Run" buttons for `TLS-01`..`TLS-11` the same way it does for `Core data types and keys`.

**Architecture:** `tools/qa-agent/src/playbook.ts` loads case data from `docs/qa-playbook.html`'s embedded `<script type="application/json" id="data">` block, not from `docs/qa-playbook.md`. This plan (a) syncs that JSON to the previous plan's two markdown fixes, and (b) registers `"TLS"` in `tools/qa-agent/src/suites.ts` — it currently isn't in `IN_SCOPE_SUITES` at all (unlike ACL, which was already there before its own wiring plan). Unlike every other in-scope suite, `TLS` needs more than one extra port tracked: its own server binds two additional TLS listeners (`16379`/`17379`) beyond the shared `6379`/`7379`/`9121` bank every suite uses — the same shape `Cluster`'s `SUITE_EXTRA_OWN_SERVER_PORTS` entry already covers for its shard-b/c ports.

**Tech Stack:** Node.js/TypeScript (`tools/qa-agent`), run via `yarn` inside that directory.

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-tls-suite-automation-1-doc-fixes.md`).

## Global Constraints

- `tools/qa-agent` uses Yarn (`.yarnrc.yml` present) — every command runs as `yarn <script>` from inside `tools/qa-agent`, never `npm`.
- The JSON sync in Task 1 must produce byte-for-byte the same field values Plan 1 wrote into `docs/qa-playbook.md` for the same case ids.
- Do not touch any other suite's entries in `docs/qa-playbook.html`'s JSON or in `tools/qa-agent/src/suites.ts`.
- `tools/` is entirely gitignored (`.gitignore:44`) — Task 2's `suites.ts`/`suites.test.ts` edits are never committed, only Task 1's `docs/qa-playbook.html` change.
- `TLS`'s own ports (`6379`/`7379`/`9121`/`16379`/`17379`) match the live hand-started cluster's real address (see the port note at the top of `docs/qa-playbook.md` and the "ACL and TLS: before you start" section intro) — registering the suite here does not itself run anything against those ports; that only happens once a live run is attempted in the next plan, and the existing pre-flight gates in `server.ts` (`assertSuitePortIsFree`, the "port already in use" checks) already refuse to start against a real cluster node. Do not weaken or bypass those gates in this plan.

---

### Task 1: Sync the TLS section of `docs/qa-playbook.html`'s embedded JSON to Plan 1's markdown changes

**Files:**
- Modify: `docs/qa-playbook.html` (the `<script type="application/json" id="data">` block only)
- Create (scratch, not committed): a short Node sync script under this session's scratchpad directory

**Interfaces:**
- Consumes: the JSON structure documented by `tools/qa-agent/src/playbook.ts`'s `RawSection`/`RawCase` types — `{title, cases: [{id, area, title, precondition, steps, expected, notes}]}`. Section title: the literal string `"TLS"`.
- Produces: an updated `docs/qa-playbook.html` whose `TLS` section's case objects match `docs/qa-playbook.md`'s post-Plan-1 content field-for-field.

- [ ] **Step 1: Write the sync script**

Write a Node script that parses the embedded JSON and, for the `"TLS"` section, applies the exact same substitutions Plan 1 applied to `docs/qa-playbook.md`:

- `TLS-01.expected`: `total 8` → `total <n>`; both `-rw------- 1 numericlabs numericlabs <size> Sep 13 08:36 <file>.pem` lines → `-rw------- 1 <user> <group> <n> <date> <file>.pem` (Plan 1 Task 1 Step 1).
- `TLS-11.expected`: leading `...` → `<date>` (Plan 1 Task 2 Step 1).

Read each case's *current* field value out of the parsed JSON first (`node -e` a quick dump) so the substitution's "old" string is copied verbatim rather than retyped. Serialize with `JSON.stringify(data)` (no indentation — the original is minified) and write it back by replacing only the contents between `<script type="application/json" id="data">` and `</script>`, leaving every other byte of the file untouched.

- [ ] **Step 2: Run the script and verify**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
node <path-to-sync-script>
node -e '
const fs = require("fs");
const html = fs.readFileSync("docs/qa-playbook.html", "utf8");
const data = JSON.parse(html.match(/<script type="application\/json" id="data">([\s\S]*?)<\/script>/)[1]);
const tls = data.find(s => s.title === "TLS");
console.log("TLS case ids:", tls.cases.map(c => c.id).join(","));
console.log("TLS-01 last 2 expected lines:", JSON.stringify(tls.cases.find(c => c.id === "TLS-01").expected.split("\n").slice(-2)));
console.log("TLS-11 first expected line:", JSON.stringify(tls.cases.find(c => c.id === "TLS-11").expected.split("\n")[0]));
'
```

Expected: `TLS case ids:` lists `TLS-01` through `TLS-11` in order; `TLS-01`'s last two lines both start with `-rw------- 1 <user> <group> <n> <date>`; `TLS-11`'s first line starts with `<date>  WARN`.

- [ ] **Step 3: Commit**

```bash
git add docs/qa-playbook.html
git commit -m "docs: sync qa-playbook.html's TLS section JSON to the doc fixes"
```

---

### Task 2: Register the TLS suite in `tools/qa-agent/src/suites.ts`

**Files:**
- Modify: `tools/qa-agent/src/suites.ts`
- Test: `tools/qa-agent/src/suites.test.ts`

**Interfaces:**
- Consumes: `IN_SCOPE_SUITES: SuiteDef[]`, `SUITE_SERVER_PORT: Record<string, number>`, `SUITE_EXTRA_OWN_SERVER_PORTS: Record<string, number[]>`, `SUITE_STARTS_OWN_SERVER: Set<string>` (`tools/qa-agent/src/suites.ts:17`, `:70`, `:157`, `:177`).
- Produces: `selectInScopeCases()` now includes all 11 `TLS-*` cases; `serverOwnerSuite(6379)` still returns `"Smoke suite"` (declaration order unchanged — `TLS` is appended after `ACL and authentication`, not inserted earlier); `suitePorts("TLS")` returns `[6379, 16379, 17379]`.

- [ ] **Step 1: Write the failing tests**

In `tools/qa-agent/src/suites.test.ts`, add:

```typescript
it("includes all 11 TLS cases", () => {
  const tlsCases = allCases.filter((c) => c.section === "TLS");
  const selected = selectInScopeCases(allCases).filter((c) => c.section === "TLS");
  expect(tlsCases.length).toBe(11);
  expect(selected.length).toBe(11);
});

it("TLS starts its own server on port 6379 with extra TLS-listener ports tracked", () => {
  expect(SUITE_STARTS_OWN_SERVER.has("TLS")).toBe(true);
  expect(SUITE_SERVER_PORT["TLS"]).toBe(6379);
  expect(suitePorts("TLS")).toEqual([6379, 16379, 17379]);
});

it("TLS does not become the reported owner of port 6379 ahead of Smoke suite", () => {
  // Declaration order in SUITE_STARTS_OWN_SERVER matters (see suites.ts's own comment on
  // serverOwnerSuite) — Smoke suite must stay the suite a reuse suite (Core/Transactions) is
  // told to "run first", unchanged by TLS's addition later in the set.
  expect(serverOwnerSuite(6379)).toBe("Smoke suite");
});
```

Match this test file's existing setup for `allCases`/imports (it already loads `docs/qa-playbook.html` via `loadPlaybook`, per the pattern the ACL suite's own tests in this file use — `suitePorts`/`serverOwnerSuite` are already imported there too).

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```

Expected: FAIL — `selected.length` is `0` (`TLS` isn't in `IN_SCOPE_SUITES` yet), `SUITE_STARTS_OWN_SERVER.has("TLS")` is `false`, `suitePorts("TLS")` is `[]`.

- [ ] **Step 3: Register the suite**

In `tools/qa-agent/src/suites.ts`, add to `IN_SCOPE_SUITES` (after the `ACL and authentication` entry, following this file's declaration-order convention of listing suites in playbook order):

```typescript
  { section: "TLS" },
```

Add to `SUITE_SERVER_PORT` (after `ACL and authentication`):

```typescript
  // Same shared numericlabs.lxd:6379/7379/9121 bank every single-instance suite above uses —
  // TLS's own two extra listeners (16379/17379) are tracked separately below, not here, per
  // suitePorts()'s "one representative port plus extras" design. This is also the live hand-
  // started cluster's real address (see the "ACL and TLS: before you start" section intro in
  // docs/qa-playbook.md) — the existing assertSuitePortIsFree/port-in-use pre-flight gates in
  // server.ts (unchanged by this plan) refuse to start against a real cluster node.
  "TLS": 6379,
```

Add to `SUITE_EXTRA_OWN_SERVER_PORTS` (after `"Pub/sub"`):

```typescript
  // The TLS suite's own two TLS listeners, bound by the same process as its representative port
  // 6379 above. Needed so a genuine fresh start refuses cleanly (per the "orphaned leftover"
  // reasoning the Cluster entry above already documents) if one of these is somehow still held
  // by a stray process while 6379 itself is free.
  TLS: [16379, 17379],
```

Add to `SUITE_STARTS_OWN_SERVER` (after `"ACL and authentication"`):

```typescript
  "TLS",
```

- [ ] **Step 4: Run the tests to verify they pass, then the full suite**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green.

`tools/qa-agent` is gitignored — no commit for this task.

---

### Task 3: Final regression and a documented handoff note for live verification

**Files:** none modified — verification and documentation only.

**Interfaces:** N/A.

- [ ] **Step 1: Confirm the JSON/suites.ts registration agree on case count**

```bash
cd tools/qa-agent
node -e '
const { loadPlaybook } = require("./dist/playbook.js") ?? {};
' 2>/dev/null || true
yarn test src/playbook.test.ts src/suites.test.ts
```

If `dist` isn't built, the `yarn test` run alone (via `vitest`, which runs the TypeScript source directly) is sufficient — the `node -e` line above is a no-op fallback, not a required step. Expected: green.

- [ ] **Step 2: Full regression**

```bash
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green — confirms Task 1/Task 2 didn't regress any other suite.

- [ ] **Step 3: Record the operational note for the next plan**

Before any live run of the TLS suite (next plan in this chain), the live hand-started cluster (if up) must be stopped — `TLS` targets the exact same `numericlabs.lxd:6379/7379/9121/16379/17379` addresses. This is a shared-system action (stopping a running cluster other work may depend on) and needs explicit confirmation from the user before it happens, not something to do automatically inside a plan step. Flag this at the very start of the next plan rather than assuming it away.

No commit for this task (nothing changed).

## Next plan

`docs/superpowers/plans/2026-09-15-tls-suite-automation-3-scratch-cleanup.md` — teach the "Clean environment" button to actually remove TLS's own scratch files under `/tmp/acltls-qa` (certs, AOF/snapshot, TOML configs, PID/log files), not just kill the port — the second half of the original ask, and a gap that exists for every suite today (the button currently only kills processes).
