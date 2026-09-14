# ACL Suite Automation — Live Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the ACL suite actually passes end to end through `tools/qa-agent` against a real `rocket-mem` binary, fix whatever the placeholder edits in the first two plans didn't anticipate, and confirm the "Run" button genuinely works for the ACL suite in `docs/qa-playbook.html` — the original ask.

**Architecture:** `yarn cli --suite "ACL and authentication"` (see `tools/qa-agent/src/index.ts:35`) runs every in-scope ACL case against a real server the same way the browser's "Run" button does, printing a pass/fail per case via `reporter.ts`. This plan runs it, chases down real mismatches (the three flagged in Plan 2's own Next-plan note — `ACL-05`'s version string, `ACL-18`'s label set, `ACL-19`'s `node_id`), and only then restarts the already-running `qa-agent --serve` dev process so the browser UI picks up the new suite registration (`server.ts` loads the playbook and computes the runnable-id list once, at server start — see `tools/qa-agent/src/server.ts:51-59`).

**Tech Stack:** the ACL suite's own `rocket-mem` binary (release build), `tools/qa-agent`'s CLI/server (Node/TypeScript, Yarn).

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-acl-suite-automation-2-wiring.md`).

## Global Constraints

- Ports 6510 (RESP), 6511 (RMP), 9310 (metrics) must be free before each live run (`ss -lnt | grep -E ':(6510|6511|9310)\b'` prints nothing) — confirmed free as of this plan's authoring; re-check before running, since state may have changed.
- Any fix discovered here to a *dynamic* value goes into `docs/qa-playbook.md` **and** `docs/qa-playbook.html`'s embedded JSON together, in the same commit — the two must never drift apart (this is the same discipline Plan 1/2 established).
- Any fix discovered here to a value that turns out to be **wrong, not just variable** (e.g. a stale literal like a version string) is a real doc-accuracy bug, not a placeholder — fix it to the real captured value, don't wildcard it away.
- `tools/qa-agent` is gitignored — no commits for anything under `tools/qa-agent` itself, only for `docs/qa-playbook.md`/`docs/qa-playbook.html`.

---

### Task 1: Run the ACL suite live and record every mismatch

**Files:** none modified — this task only observes.

- [x] **Step 1: Confirm prerequisites**

```bash
ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ACL ports free"
pgrep -af rocket-mem || echo "no rocket-mem processes"
ls -la /home/numericlabs/data/rocket/rocket-mem/target/release/rocket-mem 2>&1 || echo "release binary missing"
```

If the release binary is missing, build it: `cargo build --release --bin rocket-mem` from the repo root. If anything is already listening on 6510/6511/9310 or a stray `rocket-mem` process is running, stop and investigate before proceeding (per this project's standing rule: never `pkill -f rocket-mem`, identify and kill the specific PID, or ask the user if it's not obviously a leftover from an earlier aborted run of this same plan).

- [x] **Step 2: Run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "ACL and authentication" 2>&1 | tee /tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/039d13c1-3929-41eb-a081-58327fbd5d0d/scratchpad/acl-live-run-1.log
```

- [x] **Step 3: Triage the output**

For every case reported `fail`, read `reporter.ts`'s diff output to see exactly which line(s) of `actual` didn't match `expected`. Group findings into three buckets:

1. **A value that should have been wildcarded but wasn't** (Plan 1/2 missed it) — note the case id and the exact new placeholder needed.
2. **A value that's wrong, not just variable** — e.g. if `ACL-05`'s `actual` shows `version rocket-mem-0.1.4` but `expected` still says `rocket-mem-0.1.0`, that's a stale literal to correct outright, not wildcard.
3. **A real product/doc bug** unrelated to automation mechanics (e.g. a case's `Steps` produces genuinely different behavior than documented) — do **not** silently paper over this with a wildcard; stop and flag it for a human decision the same way the playbook's existing "Read before filing a defect" callout does for `CORE-28`/`CORE-19`/etc.

Confirm in particular, using this run's actual output:
- `ACL-18`'s exact `cmd=` label set and whether it matches the seven labels currently in the doc (`acl`, `auth`, `get`, `mget`, `hello`, `set`, `ping`) — some may be missing or extra depending on exactly what ACL-01 through ACL-15/17/19's steps hit.
- `ACL-19`'s `node_id` value — confirm it's really always the literal `127.0.0.1:6510` Plan 2 hard-coded, not something that needs `<node>` after all.

Write the findings as a short list (case id → fix needed) rather than fixing inline yet — Task 2 applies them.

---

### Task 2: Apply fixes and re-run until the suite is fully green

**Files:**
- Modify: `docs/qa-playbook.md`, `docs/qa-playbook.html` (kept in sync, same discipline as Plan 1/2)

**Interfaces:** none beyond what Plan 1/Plan 2 already established.

- [x] **Step 1: Apply every fix from Task 1's findings list**

For each finding, edit both `docs/qa-playbook.md` (by hand, `Edit` tool) and `docs/qa-playbook.html`'s embedded JSON (via a short Node script in the same style as Plan 2 Task 1's `sync-acl-json.mjs`, re-run against the current file). Re-verify sync with the same spot-check Plan 2 Task 1 Step 3 used.

- [x] **Step 2: Re-run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "ACL and authentication" 2>&1 | tee /tmp/claude-1000/-home-numericlabs-data-rocket-rocket-mem/039d13c1-3929-41eb-a081-58327fbd5d0d/scratchpad/acl-live-run-2.log
```

Expected: all 19 cases `pass`. If not, repeat Step 1/Step 2 until they do — do not proceed to Task 3 with a known-failing case.

- [x] **Step 3: Confirm ports are clean afterward**

```bash
ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ACL ports free"
```
Expected: `ACL ports free` — proves `ACL-16`'s folded-in teardown (from Plan 1 Task 3) actually tears the server down, and the CLI's own end-of-suite `killIfRocketMem` safety net (`tools/qa-agent/src/index.ts:172`) has nothing left to do.

- [x] **Step 4: Commit the doc fixes, if any were needed**

```bash
git add docs/qa-playbook.md docs/qa-playbook.html
git commit -m "docs: fix ACL suite expected output found live-verifying qa-agent automation"
```

Skip this step if Task 1 found zero mismatches (nothing to commit).

---

### Task 3: Wire up the live UI and do a final regression pass

**Files:** none modified — verification only.

- [x] **Step 1: Restart the qa-agent dev server so it picks up the new suite registration**

A `qa-agent --serve` process is already running (check with `pgrep -af 'qa-agent|tsx src/index.ts'`). It loaded the playbook and computed the runnable-case list once, at startup (`tools/qa-agent/src/server.ts:51-59`), before this work existed — restart it:

```bash
pkill -f "tsx src/index.ts --serve"
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn serve &
disown
sleep 1
```

- [x] **Step 2: Confirm ACL cases are now reported runnable**

```bash
curl -s http://127.0.0.1:4848/api/status | node -e '
let data = "";
process.stdin.on("data", d => data += d);
process.stdin.on("end", () => {
  const { runnable } = JSON.parse(data);
  const acl = runnable.filter(id => id.startsWith("ACL-"));
  console.log("ACL runnable count:", acl.length, "(expect 19)");
});
'
```

- [x] **Step 3: Confirm the Run button renders in the actual page**

**Deviation:** the literal `curl | grep 'data-run='` check above doesn't work — the button markup
is generated client-side by JS after an async `/api/status` fetch, so raw `curl` of the static
HTML never shows it (confirmed: returned `0`, not `19`). No browser/screenshot tool was available
in this session to render and inspect the DOM directly, so verified the two things that actually
determine whether the button appears and works, instead of the button's rendered markup itself:
1. `/api/status`'s `runnable` array contains all 19 `ACL-*` ids (confirmed: `19`) — this is exactly
   what the client JS's `runnable.has(c.id)` check (`docs/qa-playbook.html:594`) gates the button
   on.
2. `POST /api/run {"caseId":"ACL-01"}` — the exact request the button's click handler sends —
   returns a genuine `pass` verdict (confirmed live), and the result shows up in a follow-up
   `/api/status` call and in `clearable`. This proves the full click-to-execution path works, not
   just that the button's visibility condition is met.

- [x] **Step 4: Full regression**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn test && yarn typecheck
```
Expected: all green — confirms Plan 2's `matcher.test.ts`/`suites.test.ts` additions and this plan's live fixes didn't regress anything else.

- [x] **Step 5: Report back to the user**

Summarize: how many live-run mismatches were found and fixed (Task 1/2), confirmation the Run button works end-to-end for the ACL suite, and the final commit list (`git log --oneline -8`).

## Status: Done (2026-09-15)

Executed inline in the same session. Iterated live several times (not just the one Task 1/Task 2
pass this plan sketched) — real mismatches went well beyond the two flagged in Plan 2's Next-plan
note. Full list: ACL-01 missing its own `mkdir -p /tmp/acltls-qa` step and a wrongly-sized/missing
-trailing-blank-lines banner capture; ACL-02/03/04/07/08/09/11/12/14/15 each missing a trailing
blank line (and discovered along the way that `matchLines` strips exactly one trailing newline
before counting, so a naive single `"\n"` append is a no-op — needed a real extra blank line);
ACL-05's `modules` field has a real trailing space; ACL-10 also returns `k1` (cross-case
contamination from ACL-06); ACL-17's `head -12` truncation shape and the grep count's spacing were
wrong; ACL-18 genuinely includes a `cmd="cluster"` label whenever run via qa-agent (its own
`isClusterNode` safety probe); ACL-19 was missing its three commands' own visible replies entirely
and had the wrong two of its three tail-matched log lines. `ACL-05`'s version string and `ACL-19`'s
`node_id` — the two things Plan 2 explicitly flagged — turned out fine as originally written.
All landed in `3e73819 docs: fix ACL suite expected output found live-verifying qa-agent
automation`. Final state: all 19 ACL cases pass via both `yarn cli --suite "ACL and
authentication"` and a real `POST /api/run` call (the button's own code path); ports left clean by
ACL-16's own teardown; `yarn test` (124/124) and `yarn typecheck` clean; `qa-agent --serve`
restarted and confirmed serving the new registration.

## Next plan

None — this closes the ACL suite automation work the user asked for ("add a Run button just like Core"). If the TLS section (`docs/qa-playbook.md`'s `## TLS`, immediately after ACL) should get the same treatment next, that's a new, separately-scoped chain of plans — do not fold it into this one.
