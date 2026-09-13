# QA Agent Foundations — Playbook Parsing & Matcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure, testable core of the QA agent — parsing `docs/qa-playbook.html`'s
embedded case data, selecting the 114 in-scope cases, and diffing actual command output against
each case's `expected` text — with zero process execution yet. By the end of this plan,
`tools/qa-agent` can load the real playbook and correctly judge pass/fail for any case's output,
fully covered by tests, ready for Plan 2 to wire in actual command execution.

**Architecture:** Three small, independent TypeScript modules (`playbook.ts`, `matcher.ts`,
`suites.ts`), each with its own unit tests, no shared mutable state, no I/O beyond reading the
one HTML file. This is the first of a chain of plans (see "Next plan" at the end) — each capped
at 3 tasks, each producing working, testable software on its own.

**Tech Stack:** TypeScript (ES2022/NodeNext, strict), Yarn (`nodeLinker: node-modules`), `tsx`
for running `.ts` directly, `vitest` for tests — mirrors `tools/review-agent`'s existing
conventions in this repo, minus the Claude Agent SDK (this runner is deterministic, not LLM-driven).

**Spec:** `docs/superpowers/specs/2026-09-12-qa-agent-design.md`

## Global Constraints

- **`tools/` is entirely git-ignored in this repo** (confirmed: `git ls-files tools/` returns
  nothing, even for the existing `tools/review-agent`, which has real committed-looking source
  on disk but zero history). `tools/qa-agent` follows the same precedent: it is local-only,
  uncommitted dev tooling. **Do not run `git add`/`git commit` for any file under
  `tools/qa-agent/` in this plan** — there is nothing to commit. Only this plan document itself
  (and the spec) go through git.
- No LLM judges any case. All matching in `matcher.ts` is deterministic.
- Scope is exactly 114 cases across 8 sections, per the spec: `Environment setup` (only
  `ENV-01`, `ENV-02`, `ENV-03`, `ENV-05`, `ENV-06`), `Smoke suite` (12), `Core data types and
  keys` (54), `Transactions` (10), `Persistence` (5), `Configuration layering` (9), `RMP
  protocol` (5), `Observability` (14).
- Follow `tools/review-agent`'s existing conventions exactly: `tsconfig.json` with
  `target: ES2022`, `module`/`moduleResolution: NodeNext`, `strict: true`, `esModuleInterop:
  true`, `skipLibCheck: true`, `outDir: dist`, `rootDir: src`; its own `.yarnrc.yml` with
  `nodeLinker: node-modules`; `type: module` in `package.json`.

---

### Task 1: Scaffold `tools/qa-agent` and parse the playbook's embedded case data

**Files:**
- Create: `tools/qa-agent/package.json`
- Create: `tools/qa-agent/tsconfig.json`
- Create: `tools/qa-agent/.yarnrc.yml`
- Create: `tools/qa-agent/README.md`
- Create: `tools/qa-agent/src/playbook.ts`
- Test: `tools/qa-agent/src/playbook.test.ts`

**Interfaces:**
- Consumes: `docs/qa-playbook.html` (read-only; its `<script type="application/json"
  id="data">` block holds `[{title: string, cases: [{id, area, title, precondition, steps,
  expected, notes}, ...]}, ...]`, exactly as rendered into the page's UI).
- Produces: `export interface QaCase { id: string; area: string; title: string; precondition:
  string; steps: string; expected: string; notes: string; section: string }` and `export
  function loadPlaybook(htmlPath: string): QaCase[]` — consumed by `suites.ts` (Task 3 below)
  and by `caseRunner.ts` in Plan 2.

- [ ] **Step 1: Create `tools/qa-agent/package.json`**

```json
{
  "name": "qa-agent",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "description": "Deterministic runner for the automatable subset of docs/qa-playbook.html's test cases, pushing live pass/fail results into the page over SSE.",
  "scripts": {
    "test": "vitest run",
    "typecheck": "tsc --noEmit"
  },
  "devDependencies": {
    "@types/node": "^22.0.0",
    "tsx": "^4.19.0",
    "typescript": "^5.6.0",
    "vitest": "^2.1.0"
  }
}
```

- [ ] **Step 2: Create `tools/qa-agent/tsconfig.json`**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "dist",
    "rootDir": "src"
  },
  "include": ["src"]
}
```

- [ ] **Step 3: Create `tools/qa-agent/.yarnrc.yml`**

```yaml
nodeLinker: node-modules
```

- [ ] **Step 4: Create `tools/qa-agent/README.md`**

```markdown
# qa-agent

A deterministic runner for the automatable subset of `docs/qa-playbook.html`'s test cases.
Local-only dev tooling — like `tools/review-agent`, this directory is git-ignored (see the
repo's `.gitignore`, `tools` entry).

See `docs/superpowers/specs/2026-09-12-qa-agent-design.md` for the design this implements.

## Setup

\`\`\`bash
cd tools/qa-agent
yarn install
\`\`\`

## Usage

Not yet wired up — see the implementation plans under `docs/superpowers/plans/` for progress
(`2026-09-13-qa-agent-1-foundations.md` and its chained successors).
```

- [ ] **Step 5: Install dependencies**

Run: `cd tools/qa-agent && yarn install`
Expected: installs cleanly, creates `tools/qa-agent/node_modules/` and
`tools/qa-agent/yarn.lock` (both git-ignored).

- [ ] **Step 6: Write the failing test for `loadPlaybook`**

Create `tools/qa-agent/src/playbook.test.ts`:

```typescript
import { describe, it, expect } from "vitest";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { loadPlaybook } from "./playbook.js";

const PLAYBOOK_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../docs/qa-playbook.html",
);

describe("loadPlaybook", () => {
  it("loads all 184 cases from the real playbook", () => {
    const cases = loadPlaybook(PLAYBOOK_PATH);
    expect(cases.length).toBe(184);
  });

  it("attaches the section title to each case", () => {
    const cases = loadPlaybook(PLAYBOOK_PATH);
    const env01 = cases.find((c) => c.id === "ENV-01");
    expect(env01).toBeDefined();
    expect(env01?.section).toBe("Environment setup");
    expect(env01?.title).toContain("redis-cli");
  });

  it("preserves multi-line steps verbatim, including heredocs", () => {
    const cases = loadPlaybook(PLAYBOOK_PATH);
    const txn01 = cases.find((c) => c.id === "TXN-01");
    expect(txn01).toBeDefined();
    expect(txn01?.steps).toContain("MULTI");
    expect(txn01?.steps).toContain("EXEC");
  });

  it("throws a clear error when the data script block is missing", () => {
    expect(() => loadPlaybook(__filenameForTest())).toThrow(
      /No <script type="application\/json" id="data">/,
    );
  });
});

function __filenameForTest(): string {
  // Any real file without the expected script block — this test file itself works fine.
  return fileURLToPath(import.meta.url);
}
```

- [ ] **Step 7: Run the test to verify it fails**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './playbook.js'` (or equivalent "no such file"), since
`src/playbook.ts` doesn't exist yet.

- [ ] **Step 8: Implement `tools/qa-agent/src/playbook.ts`**

```typescript
import { readFileSync } from "node:fs";

export interface QaCase {
  id: string;
  area: string;
  title: string;
  precondition: string;
  steps: string;
  expected: string;
  notes: string;
  section: string;
}

interface RawCase {
  id: string;
  area: string;
  title: string;
  precondition: string;
  steps: string;
  expected: string;
  notes: string;
}

interface RawSection {
  title: string;
  cases: RawCase[];
}

const DATA_SCRIPT_RE =
  /<script type="application\/json" id="data">([\s\S]*?)<\/script>/;

/**
 * Loads and flattens the case data embedded in docs/qa-playbook.html's
 * `<script type="application/json" id="data">` block.
 */
export function loadPlaybook(htmlPath: string): QaCase[] {
  const html = readFileSync(htmlPath, "utf8");
  const match = html.match(DATA_SCRIPT_RE);
  if (!match) {
    throw new Error(
      `No <script type="application/json" id="data"> block found in ${htmlPath}`,
    );
  }
  const sections: RawSection[] = JSON.parse(match[1]);
  const cases: QaCase[] = [];
  for (const section of sections) {
    for (const c of section.cases) {
      cases.push({ ...c, section: section.title });
    }
  }
  return cases;
}
```

- [ ] **Step 9: Run the test to verify it passes**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all 4 tests in `playbook.test.ts` green.

- [ ] **Step 10: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

---

### Task 2: `matcher.ts` — diff actual output against `expected`, with wildcards and tolerance

**Files:**
- Create: `tools/qa-agent/src/matcher.ts`
- Test: `tools/qa-agent/src/matcher.test.ts`

**Interfaces:**
- Consumes: nothing from Task 1 — pure string-in, string-out logic (a case's `id`, `expected`,
  and a captured `actual` string, which Plan 2's `caseRunner.ts` will supply).
- Produces: `export type Verdict = "pass" | "fail"`, `export interface MatchResult { verdict:
  Verdict; reason?: string }`, `export function matchCase(caseId: string, expected: string,
  actual: string): MatchResult` — consumed by `caseRunner.ts` in Plan 2.

- [ ] **Step 1: Write the failing tests**

Create `tools/qa-agent/src/matcher.test.ts`:

```typescript
import { describe, it, expect } from "vitest";
import { matchCase } from "./matcher.js";

describe("matchCase: generic exact + wildcard matching", () => {
  it("passes on an exact line-for-line match", () => {
    const result = matchCase("ENV-01", "redis-cli 8.10.1", "redis-cli 8.10.1");
    expect(result.verdict).toBe("pass");
  });

  it("fails on a literal mismatch", () => {
    const result = matchCase("ENV-01", "redis-cli 8.10.1", "redis-cli 7.0.0");
    expect(result.verdict).toBe("fail");
    expect(result.reason).toMatch(/line 1/);
  });

  it("treats a single <placeholder> token as a wildcard", () => {
    const result = matchCase(
      "SMOKE-08",
      "process_id:<pid>",
      "process_id:48213",
    );
    expect(result.verdict).toBe("pass");
  });

  it("lets a <placeholder> span multiple words", () => {
    const result = matchCase(
      "SMOKE-11",
      "-rw-rw-r-- 1 <user> <group> 163 <date> dump.snapshot",
      "-rw-rw-r-- 1 alice alice 163 Sep 13 09:00 dump.snapshot",
    );
    expect(result.verdict).toBe("pass");
  });

  it("fails when the line count differs", () => {
    const result = matchCase("ENV-01", "line one\nline two", "line one");
    expect(result.verdict).toBe("fail");
    expect(result.reason).toMatch(/2 line/);
  });

  it("matches a blank line (nil reply) exactly", () => {
    const result = matchCase("CORE-01", "OK\nhello\n\nOK", "OK\nhello\n\nOK");
    expect(result.verdict).toBe("pass");
  });
});

describe("matchCase: numeric tolerance table", () => {
  it("SMOKE-07: passes when TTL has counted down a little", () => {
    const result = matchCase("SMOKE-07", "OK\n1\n99", "OK\n1\n96");
    expect(result.verdict).toBe("pass");
  });

  it("SMOKE-07: fails when TTL counted down too far", () => {
    const result = matchCase("SMOKE-07", "OK\n1\n99", "OK\n1\n80");
    expect(result.verdict).toBe("fail");
  });

  it("SMOKE-07: fails when TTL is higher than expected", () => {
    const result = matchCase("SMOKE-07", "OK\n1\n99", "OK\n1\n100");
    expect(result.verdict).toBe("fail");
  });

  it("CORE-02: passes when both TTL and PTTL have counted down a little", () => {
    const result = matchCase(
      "CORE-02",
      "OK\n99\nOK\n99997",
      "OK\n97\nOK\n99991",
    );
    expect(result.verdict).toBe("pass");
  });

  it("CORE-02: fails when PTTL counted down too far", () => {
    const result = matchCase(
      "CORE-02",
      "OK\n99\nOK\n99997",
      "OK\n97\nOK\n90000",
    );
    expect(result.verdict).toBe("fail");
  });

  it("falls back to generic matching for a case id with no tolerance entry", () => {
    const result = matchCase("CORE-99", "OK\n42", "OK\n42");
    expect(result.verdict).toBe("pass");
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './matcher.js'`.

- [ ] **Step 3: Implement `tools/qa-agent/src/matcher.ts`**

```typescript
export type Verdict = "pass" | "fail";

export interface MatchResult {
  verdict: Verdict;
  reason?: string;
}

// The playbook's own `expected` blocks already use bracketed tokens (<pid>, <n>, <date>,
// <user>, <group>, ...) to mark values that legitimately vary between runs.
const PLACEHOLDER_RE = /<[a-zA-Z_][a-zA-Z0-9_ -]*>/g;

function stripTrailingNewline(s: string): string {
  return s.endsWith("\n") ? s.slice(0, -1) : s;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function lineToRegex(expectedLine: string): RegExp {
  const parts = expectedLine.split(PLACEHOLDER_RE).map(escapeRegExp);
  return new RegExp("^" + parts.join(".+?") + "$");
}

interface NumericTolerance {
  /** How far below the expected numeric value the actual value may fall and still pass. */
  maxDeltaBelow: number;
}

/**
 * Line-for-line match against `expected`, treating <...> tokens as wildcards, with an
 * explicit set of per-line numeric tolerances (used for TTL/PTTL-style countdown values that
 * the playbook documents as legitimately drifting between the write and the read).
 */
function matchLines(
  expected: string,
  actual: string,
  tolerances: Record<number, NumericTolerance> = {},
): MatchResult {
  const expectedLines = stripTrailingNewline(expected).split("\n");
  const actualLines = stripTrailingNewline(actual).split("\n");
  if (expectedLines.length !== actualLines.length) {
    return {
      verdict: "fail",
      reason: `expected ${expectedLines.length} line(s), got ${actualLines.length}`,
    };
  }
  for (let i = 0; i < expectedLines.length; i++) {
    const tolerance = tolerances[i];
    if (tolerance) {
      const expectedNum = Number(expectedLines[i]);
      const actualNum = Number(actualLines[i]);
      const withinTolerance =
        !Number.isNaN(actualNum) &&
        actualNum <= expectedNum &&
        actualNum >= expectedNum - tolerance.maxDeltaBelow;
      if (!withinTolerance) {
        return {
          verdict: "fail",
          reason: `line ${i + 1}: expected ~${expectedLines[i]} (within ${tolerance.maxDeltaBelow} below), got "${actualLines[i]}"`,
        };
      }
    } else if (!lineToRegex(expectedLines[i]).test(actualLines[i])) {
      return {
        verdict: "fail",
        reason: `line ${i + 1}: expected to match "${expectedLines[i]}", got "${actualLines[i]}"`,
      };
    }
  }
  return { verdict: "pass" };
}

// Cases with documented numeric drift that isn't already expressed as a <placeholder> in the
// playbook's own `expected` text. Seeded from the two simplest known cases; expect to grow
// this table during Plan 2+ as the runner is exercised against a live server for real.
const TOLERANCE_TABLE: Record<
  string,
  (expected: string, actual: string) => MatchResult
> = {
  // SMOKE-07: `OK` / `1` / TTL value (seconds), TTL counts down between EXPIRE and TTL.
  "SMOKE-07": (expected, actual) =>
    matchLines(expected, actual, { 2: { maxDeltaBelow: 5 } }),
  // CORE-02: `OK` / TTL (seconds) / `OK` / PTTL (milliseconds).
  "CORE-02": (expected, actual) =>
    matchLines(expected, actual, {
      1: { maxDeltaBelow: 5 },
      3: { maxDeltaBelow: 5000 },
    }),
};

export function matchCase(
  caseId: string,
  expected: string,
  actual: string,
): MatchResult {
  const override = TOLERANCE_TABLE[caseId];
  if (override) return override(expected, actual);
  return matchLines(expected, actual);
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all `matcher.test.ts` cases green, `playbook.test.ts` still green.

- [ ] **Step 5: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

---

### Task 3: `suites.ts` — select the 114 in-scope cases from the real playbook

**Files:**
- Create: `tools/qa-agent/src/suites.ts`
- Test: `tools/qa-agent/src/suites.test.ts`

**Interfaces:**
- Consumes: `QaCase` and `loadPlaybook` from `./playbook.js` (Task 1).
- Produces: `export interface SuiteDef { section: string; caseIds?: string[] }`, `export const
  IN_SCOPE_SUITES: SuiteDef[]`, `export function selectInScopeCases(allCases: QaCase[]):
  QaCase[]` — consumed by `index.ts`'s CLI orchestration in Plan 2/3.

- [ ] **Step 1: Write the failing tests**

Create `tools/qa-agent/src/suites.test.ts`:

```typescript
import { describe, it, expect } from "vitest";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { loadPlaybook } from "./playbook.js";
import { selectInScopeCases } from "./suites.js";

const PLAYBOOK_PATH = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../docs/qa-playbook.html",
);

describe("selectInScopeCases", () => {
  it("selects exactly 114 of the 184 real cases", () => {
    const all = loadPlaybook(PLAYBOOK_PATH);
    const inScope = selectInScopeCases(all);
    expect(all.length).toBe(184);
    expect(inScope.length).toBe(114);
  });

  it("matches the documented per-section breakdown", () => {
    const all = loadPlaybook(PLAYBOOK_PATH);
    const inScope = selectInScopeCases(all);
    const counts: Record<string, number> = {};
    for (const c of inScope) {
      counts[c.section] = (counts[c.section] ?? 0) + 1;
    }
    expect(counts).toEqual({
      "Environment setup": 5,
      "Smoke suite": 12,
      "Core data types and keys": 54,
      "Transactions": 10,
      "Persistence": 5,
      "Configuration layering": 9,
      "RMP protocol": 5,
      "Observability": 14,
    });
  });

  it("excludes out-of-scope sections entirely", () => {
    const all = loadPlaybook(PLAYBOOK_PATH);
    const inScope = selectInScopeCases(all);
    const sections = new Set(inScope.map((c) => c.section));
    for (const excluded of [
      "Replication",
      "Pub/sub",
      "Cluster",
      "ACL and authentication",
      "TLS",
    ]) {
      expect(sections.has(excluded)).toBe(false);
    }
  });

  it("only includes the 5 whitelisted Environment setup cases", () => {
    const all = loadPlaybook(PLAYBOOK_PATH);
    const inScope = selectInScopeCases(all);
    const envIds = inScope
      .filter((c) => c.section === "Environment setup")
      .map((c) => c.id)
      .sort();
    expect(envIds).toEqual([
      "ENV-01",
      "ENV-02",
      "ENV-03",
      "ENV-05",
      "ENV-06",
    ]);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd tools/qa-agent && yarn test`
Expected: FAIL — `Cannot find module './suites.js'`.

- [ ] **Step 3: Implement `tools/qa-agent/src/suites.ts`**

```typescript
import type { QaCase } from "./playbook.js";

export interface SuiteDef {
  /** Must match a section `title` exactly, as it appears in the playbook's JSON. */
  section: string;
  /** If set, only these case ids from the section are in scope. Omit to include the whole section. */
  caseIds?: string[];
}

// Single-instance suites only — no Docker, multi-node, or TLS-cert setup. See
// docs/superpowers/specs/2026-09-12-qa-agent-design.md for why each of these is in (or out
// of) scope for v1.
export const IN_SCOPE_SUITES: SuiteDef[] = [
  {
    section: "Environment setup",
    caseIds: ["ENV-01", "ENV-02", "ENV-03", "ENV-05", "ENV-06"],
  },
  { section: "Smoke suite" },
  { section: "Core data types and keys" },
  { section: "Transactions" },
  { section: "Persistence" },
  { section: "Configuration layering" },
  { section: "RMP protocol" },
  { section: "Observability" },
];

export function selectInScopeCases(allCases: QaCase[]): QaCase[] {
  const bySection = new Map<string, SuiteDef>(
    IN_SCOPE_SUITES.map((s) => [s.section, s]),
  );
  return allCases.filter((c) => {
    const suite = bySection.get(c.section);
    if (!suite) return false;
    if (suite.caseIds && !suite.caseIds.includes(c.id)) return false;
    return true;
  });
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd tools/qa-agent && yarn test`
Expected: PASS — all tests across `playbook.test.ts`, `matcher.test.ts`, and `suites.test.ts`
green (20 tests total: 4 + 12 + 4).

- [ ] **Step 5: Typecheck**

Run: `cd tools/qa-agent && yarn typecheck`
Expected: no errors.

---

## Next plan

`docs/superpowers/plans/2026-09-13-qa-agent-2-execution.md` (not yet written) — process
execution: `processTracker.ts` (resolve/kill `rocket-mem` PIDs via `ss -tlnp` on a suite's known
ports, confirmed as a `rocket-mem` process before any kill — never `pkill -f`), `caseRunner.ts`
(run one case's `steps` verbatim as `bash -c` in a suite-scoped scratch working directory,
substitute `<pid>` for `SMOKE-12`, capture stdout), and wiring them together with this plan's
`playbook.ts` / `suites.ts` / `matcher.ts` to run the full Smoke suite end-to-end against a real
built `rocket-mem` binary and print a pass/fail summary — the walking-skeleton proof the spec
calls for, still with no server/SSE/live-HTML piece yet (that's Plan 3).
