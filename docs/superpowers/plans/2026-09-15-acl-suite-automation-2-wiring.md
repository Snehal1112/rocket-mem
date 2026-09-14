# ACL Suite Automation — Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tools/qa-agent` actually recognize and run the ACL suite, so `docs/qa-playbook.html` shows "Run" buttons for `ACL-01`..`ACL-19` the same way it does for `Core data types and keys`.

**Architecture:** `tools/qa-agent/src/playbook.ts` loads case data from `docs/qa-playbook.html`'s embedded `<script type="application/json" id="data">` block — **not** from `docs/qa-playbook.md`. The `.md` is the human-facing source of truth; the `.html` JSON is a hand-synced copy the runner actually executes against, per `docs/qa-playbook.html`'s own footer note. This plan (a) syncs that JSON to the `.md` changes Plan 1 already made, (b) teaches the matcher that `ACL-11`'s four `ACL LIST` lines can come back in any order (HashMap iteration order — same shape as `CORE-33`'s existing entry), and (c) registers the suite in `tools/qa-agent/src/suites.ts` so `selectInScopeCases`/`SUITE_SERVER_PORT`/`SUITE_STARTS_OWN_SERVER` all know about it.

**Tech Stack:** Node.js/TypeScript (`tools/qa-agent`), run via `yarn` (per your global preference) inside that directory.

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-acl-suite-automation-1-doc-fixes.md`).

## Global Constraints

- `tools/qa-agent` uses Yarn (`.yarnrc.yml` present) — every command in this plan runs as `yarn <script>` from inside `tools/qa-agent`, never `npm`.
- The JSON sync in Task 1 must produce byte-for-byte the same field values Plan 1 wrote into `docs/qa-playbook.md` for the same case ids — the whole point is these two files agree.
- Do not touch any other suite's entries in `docs/qa-playbook.html`'s JSON or in `tools/qa-agent/src/suites.ts`.
- **`tools/` is entirely gitignored** (`.gitignore:44`, a bare `tools` entry — confirmed via `git check-ignore -v tools/qa-agent/src/suites.ts`), so `tools/qa-agent` is local-only dev tooling that is never committed. Tasks 2 and 3 below edit and test files under `tools/qa-agent` but do **not** `git add`/`git commit` them — only Task 1's `docs/qa-playbook.html` change (a tracked file) gets committed.

---

### Task 1: Sync the ACL section of `docs/qa-playbook.html`'s embedded JSON to Plan 1's markdown changes

**Files:**
- Modify: `docs/qa-playbook.html` (the `<script type="application/json" id="data">` block only — do not touch anything else in the file, including the unrelated font/styling changes already present in the working tree)
- Create (scratch, not committed): `/tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/039d13c1-3929-41eb-a081-58327fbd5d0d/scratchpad/sync-acl-json.mjs`

**Interfaces:**
- Consumes: the JSON structure documented by `tools/qa-agent/src/playbook.ts`'s `RawSection`/`RawCase` types — an array of `{title, cases: [{id, area, title, precondition, steps, expected, notes}]}`. The section title to target is the literal string `"ACL and authentication"`.
- Produces: an updated `docs/qa-playbook.html` whose `ACL and authentication` section's 19 case objects match `docs/qa-playbook.md`'s post-Plan-1 content field-for-field, with `ACL-16` moved to the end of that section's `cases` array.

- [x] **Step 1: Write the sync script**

Write a Node script that parses the embedded JSON, and for the `"ACL and authentication"` section applies the *exact same substitutions* Plan 1 applied to `docs/qa-playbook.md`, field by field, by case id:

- `ACL-01.expected`: same PID/timestamp → `<pid>`/`<date>` substitutions as Plan 1 Task 1 Step 1.
- `ACL-05.expected`: `id 9` → `id <n>` (Plan 1 Task 1 Step 2).
- `ACL-11.expected`: five `$argon2id$...` hashes → `<hash>` (Plan 1 Task 1 Step 3).
- `ACL-12.expected`: one `$argon2id$...` hash → `<hash>` (Plan 1 Task 1 Step 4).
- `ACL-16.expected`: four timestamps → `<date>` (Plan 1 Task 1 Step 5), **plus** prepend `"ports free\n"` for the new leading teardown check (Plan 1 Task 3 Step 2's new Steps start with a `kill` + `ss` check before the two failed-start attempts).
- `ACL-16.precondition`: replace with Plan 1 Task 3 Step 2's new precondition text (`"ACL-01 through ACL-19 completed; the server from ACL-01 is still running on port 6510."`).
- `ACL-16.steps`: replace with Plan 1 Task 3 Step 2's new steps (the `kill $PID` / `sleep 1` / `ss` block prepended to the existing two-attempt bad-config script).
- `ACL-16.notes`: append the one sentence from Plan 1 Task 3 Step 2 explaining the fold-in.
- `ACL-17.expected`: nine numeric values → `<n>` (Plan 1 Task 2 Step 1).
- `ACL-18.expected`: seven counter values → `<n>` (Plan 1 Task 2 Step 2).
- `ACL-18.notes`: prepend the automation-note sentence from Plan 1 Task 2 Step 2.
- `ACL-19.expected`: `conn_id=N`/`peer=...:PORT`/`node_id=...` → `conn_id=<n>`/`peer=...:<port>`/`node_id=127.0.0.1:6510`, and the leading `...` → `<date>`, for all three lines (Plan 1 Task 2 Step 3).
- `ACL-19.notes`: append the automation-note sentence from Plan 1 Task 2 Step 2 (same note applies to both — it covers "confirm live before registering").
- Reorder the section's `cases` array so `ACL-16` is last (after `ACL-19`), matching Plan 1 Task 3's reordering of the markdown.

Read each case's *current* field value out of the parsed JSON first (`node -e` a quick dump, as used earlier while investigating this file) so the substitution's "old" string is copied verbatim rather than retyped — a single mismatched character means `.replace()` silently no-ops.

Serialize with `JSON.stringify(data)` (no indentation — the original is minified) and write it back into `docs/qa-playbook.html` by replacing only the contents between `<script type="application/json" id="data">` and `</script>`, leaving every other byte of the file untouched.

- [x] **Step 2: Run the script**

```bash
cd /home/numericlabs/data/rocket/rocket-mem
node /tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/039d13c1-3929-41eb-a081-58327fbd5d0d/scratchpad/sync-acl-json.mjs
```

- [x] **Step 3: Verify the sync**

```bash
node -e '
const fs = require("fs");
const html = fs.readFileSync("docs/qa-playbook.html", "utf8");
const data = JSON.parse(html.match(/<script type="application\/json" id="data">([\s\S]*?)<\/script>/)[1]);
const md = fs.readFileSync("docs/qa-playbook.md", "utf8");
const acl = data.find(s => s.title === "ACL and authentication");
console.log("order:", acl.cases.map(c => c.id).join(","));
for (const c of acl.cases) {
  if (!md.includes(c.expected.split("\n")[0])) console.log("MISMATCH first expected line:", c.id);
}
'
```

Expected: `order:` ends with `...,ACL-15,ACL-17,ACL-18,ACL-19,ACL-16` and no `MISMATCH` lines print. Then spot-check by eye that `grep -n '2026-09-1[0-9]T\|PID=[0-9]\+\|\$argon2id\$\|conn_id=N' docs/qa-playbook.html` has no hits inside the ACL section (cross-check against `docs/qa-playbook.md`'s ACL section headings the same way Plan 1 Task 1 Step 6 did).

- [x] **Step 4: Commit**

```bash
git add docs/qa-playbook.html
git commit -m "docs: sync qa-playbook.html's ACL section JSON to the markdown fixes"
```

---

### Task 2: Teach the matcher that ACL-11's `ACL LIST` lines are unordered

**Files:**
- Modify: `tools/qa-agent/src/matcher.ts`
- Test: `tools/qa-agent/src/matcher.test.ts`

**Interfaces:**
- Consumes: `matchLinesWithUnorderedGroups(expected: string, actual: string, groups: {start: number, count: number}[]): MatchResult` — already defined in this file and already used by the `UNORDERED_TABLE["CORE-33"]` entry.
- Produces: `UNORDERED_TABLE["ACL-11"]`, read by `matchCase()` (`tools/qa-agent/src/matcher.ts:310`).

- [x] **Step 1: Write the failing test**

In `tools/qa-agent/src/matcher.test.ts`, find the existing `describe`/`it` block that exercises `UNORDERED_TABLE["CORE-33"]` via `matchCase("CORE-33", ...)` and add a sibling test:

```typescript
it("ACL-11 treats the four ACL LIST lines as an unordered group", () => {
  const expected = [
    "admin",
    "user admin on #<hash> +@all ~*",
    "user scoped on #<hash> +@all ~app:*",
    "user app on #<hash> ~app:* +get",
    "user retired off #<hash> +@all ~*",
    "flags",
    "on",
    "passwords",
    "<hash>",
    "commands",
    "+get",
    "keys",
    "~app:*",
  ].join("\n");
  // Same four lines, reordered — real HashMap iteration order for a different run.
  const actual = [
    "admin",
    "user retired off #$argon2id$fake1 +@all ~*",
    "user app on #$argon2id$fake2 ~app:* +get",
    "user admin on #$argon2id$fake3 +@all ~*",
    "user scoped on #$argon2id$fake4 +@all ~app:*",
    "flags",
    "on",
    "passwords",
    "$argon2id$fake2",
    "commands",
    "+get",
    "keys",
    "~app:*",
  ].join("\n");
  const result = matchCase("ACL-11", expected, actual);
  expect(result.verdict).toBe("pass");
});
```

- [x] **Step 2: Run the test to verify it fails**

```bash
cd tools/qa-agent && yarn test src/matcher.test.ts
```
Expected: FAIL — `ACL-11` isn't in `UNORDERED_TABLE` yet, so `matchCase` falls through to plain `matchLines`, which compares positionally and reports a mismatch on line 1 (`"user admin on ..."` expected vs `"user retired off ..."` actual).

- [x] **Step 3: Add the `ACL-11` entry**

In `tools/qa-agent/src/matcher.ts`, add to `UNORDERED_TABLE` (immediately before its closing `};` at line 252), following the exact style of the adjacent `CORE-33` entry:

```typescript
  // ACL-11: the four `ACL LIST` lines (index 1-4, right after the leading `admin` line from
  // `ACL WHOAMI`) are HashMap iteration order — same non-determinism as CORE-33's KEYS glob
  // match, and explicitly called out in the case's own Notes ("never script against it").
  "ACL-11": (expected, actual) =>
    matchLinesWithUnorderedGroups(expected, actual, [{ start: 1, count: 4 }]),
```

- [x] **Step 4: Run the test to verify it passes**

```bash
cd tools/qa-agent && yarn test src/matcher.test.ts
```
Expected: PASS.

- [x] **Step 5: Run the full test suite and typecheck**

```bash
cd tools/qa-agent && yarn test && yarn typecheck
```
Expected: all green — this also catches any JSON-syntax mistake Task 1's script might have introduced, since `suites.test.ts`/`playbook.test.ts` load the real `docs/qa-playbook.html`.

`tools/qa-agent` is gitignored (see Global Constraints) — no commit step here; the edit stays in the local working tree only.

---

### Task 3: Register the ACL suite in `tools/qa-agent/src/suites.ts`

**Files:**
- Modify: `tools/qa-agent/src/suites.ts`
- Test: `tools/qa-agent/src/suites.test.ts`

**Interfaces:**
- Consumes: `IN_SCOPE_SUITES: SuiteDef[]`, `SUITE_SERVER_PORT: Record<string, number>`, `SUITE_STARTS_OWN_SERVER: Set<string>` — all already defined in this file (see `tools/qa-agent/src/suites.ts:17`, `:69`, `:170`).
- Produces: `selectInScopeCases()` now includes all 19 `ACL-*` cases; `serverOwnerSuite(6510)` now returns `"ACL and authentication"`.

- [x] **Step 1: Write the failing test**

In `tools/qa-agent/src/suites.test.ts`, find the test that asserts on `IN_SCOPE_SUITES`/`selectInScopeCases` membership for an already-registered own-server suite (e.g. Observability or Configuration layering) and add a sibling:

```typescript
it("includes all 19 ACL and authentication cases", () => {
  const aclCases = allCases.filter((c) => c.section === "ACL and authentication");
  const selected = selectInScopeCases(allCases).filter(
    (c) => c.section === "ACL and authentication",
  );
  expect(aclCases.length).toBe(19);
  expect(selected.length).toBe(19);
});

it("ACL and authentication starts its own server on port 6510", () => {
  expect(SUITE_STARTS_OWN_SERVER.has("ACL and authentication")).toBe(true);
  expect(SUITE_SERVER_PORT["ACL and authentication"]).toBe(6510);
  expect(serverOwnerSuite(6510)).toBe("ACL and authentication");
});
```

(Match this test file's existing setup for `allCases` — it already loads `docs/qa-playbook.html` via `loadPlaybook`, per the pattern the surrounding tests use.)

- [x] **Step 2: Run the tests to verify they fail**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```
Expected: FAIL — `selected.length` is `0` (ACL isn't in `IN_SCOPE_SUITES` yet) and `SUITE_STARTS_OWN_SERVER.has(...)` is `false`.

- [x] **Step 3: Register the suite**

In `tools/qa-agent/src/suites.ts`, add to `IN_SCOPE_SUITES` (after the `Cluster` entry, following this file's declaration-order convention of listing suites in the order they appear in the playbook):

```typescript
  { section: "ACL and authentication" },
```

Add to `SUITE_SERVER_PORT` (after the `Cluster` entry):

```typescript
  // Its own scratch bank (6510 RESP, 6511 RMP, 9310 metrics) — distinct from every other
  // suite's ports and from the live hand-started cluster (numericlabs.lxd:6379/7379/9121), per
  // the port note at the top of the ACL section in docs/qa-playbook.md. ACL-16 (last case in
  // execution order — see the 2026-09-15 ACL suite automation plans) stops this server as its
  // own final step, same shape as Smoke suite's SMOKE-12.
  "ACL and authentication": 6510,
```

Add to `SUITE_STARTS_OWN_SERVER`:

```typescript
  "ACL and authentication",
```

- [x] **Step 4: Run the tests to verify they pass**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```
Expected: PASS.

- [x] **Step 5: Run the full test suite and typecheck**

```bash
cd tools/qa-agent && yarn test && yarn typecheck
```
Expected: all green.

`tools/qa-agent` is gitignored (see Global Constraints) — no commit step here; the edit stays in the local working tree only.

## Status: Done (2026-09-15)

Executed inline in the same session. Task 1's `docs/qa-playbook.html` JSON sync landed in
`5423457 docs: sync qa-playbook.html's ACL section JSON to the markdown fixes` (isolated via a
hash-object splice so it didn't pick up the unrelated font/styling changes also sitting in the
working tree). Tasks 2/3 (`matcher.ts`'s `ACL-11` entry, `suites.ts` registration, and both test
files) are in `tools/qa-agent`, which is gitignored — no commit exists for them, confirmed
correctly in place by direct inspection and a clean `yarn test`/`yarn typecheck` run (2026-09-15,
124/124 tests passing). Plan 3 (`2026-09-15-acl-suite-automation-3-live-verify.md`) is also done —
see its own Status section.

## Next plan

`docs/superpowers/plans/2026-09-15-acl-suite-automation-3-live-verify.md` — actually run the ACL suite through `tools/qa-agent` against a real `rocket-mem` binary, fix any real mismatches the placeholder edits above didn't anticipate (in particular `ACL-05`'s suspicious `rocket-mem-0.1.0` version string, `ACL-18`'s exact `cmd=` label set, and `ACL-19`'s `node_id` literal), and confirm the "Run" button actually appears and works for the ACL suite in `docs/qa-playbook.html`.
