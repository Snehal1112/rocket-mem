# TLS Suite Automation — Live Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the TLS suite actually passes end to end through `tools/qa-agent` against a real `rocket-mem` binary, fill in the two `Expected` blocks (`TLS-09`, `TLS-10`) the doc-fixes plan deliberately left as prose rather than guess at, fix whatever else a live run turns up, and confirm both the "Run" and "Clean environment" buttons genuinely work for TLS in `docs/qa-playbook.html` — the original ask.

**Architecture:** Same shape as the ACL suite automation's own live-verification plan (`docs/superpowers/plans/2026-09-15-acl-suite-automation-3-live-verify.md`): `yarn cli --suite "TLS"` runs every in-scope case against a real server, `reporter.ts` prints pass/fail per case, and real mismatches get chased down and fixed in both `docs/qa-playbook.md` and `docs/qa-playbook.html`'s JSON together. The one thing this suite adds beyond ACL's own playbook: TLS's ports (`6379`/`7379`/`9121`/`16379`/`17379`) are the **live hand-started cluster's real address**, so a live run is only possible with that cluster stopped, and it must come back up afterward.

**Tech Stack:** the TLS suite's own `rocket-mem` binary (release build), Smallstep's `step` CLI (cert generation), `tools/qa-agent`'s CLI/server (Node/TypeScript, Yarn).

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-tls-suite-automation-3-scratch-cleanup.md`).

## Global Constraints

- **Do not stop the live hand-started cluster without the user's explicit go-ahead first.** It is a shared system other work may depend on, per this project's own standing rules (never `pkill -f rocket-mem`; check what's actually live before touching it). Ask before Task 1 Step 1 proceeds past the "confirm what's live" check if the cluster turns out to be up.
- Any fix discovered here to a *dynamic* value goes into `docs/qa-playbook.md` **and** `docs/qa-playbook.html`'s embedded JSON together, in the same commit.
- Any fix discovered here to a value that's **wrong, not just variable** (a stale literal) is a real doc-accuracy bug — fix it to the real captured value, don't wildcard it away.
- `tools/qa-agent` is gitignored — no commits for anything under `tools/qa-agent` itself, only for `docs/qa-playbook.md`/`docs/qa-playbook.html`.
- Smallstep's `step` CLI is required for `TLS-01`. If it isn't installed, install it first (see `TLS-01`'s own Precondition for install options) rather than skip the case.
- **Known gap, found by Plan 2's final review, that this plan must close:** no TLS case's `Steps` ever kills the server `TLS-02`/`TLS-10` start — the doc's own "TLS teardown" section is prose outside any case, so `qa-agent` never runs it. Worse, `TLS-10`'s server requires auth (its config sets an `admin` ACL user), so `tools/qa-agent/src/index.ts`'s end-of-run `killIfRocketMem` safety net is *correctly refused* (`processTracker.ts`'s `probeIsRealClusterNode` fails closed on `NOAUTH`). Left unaddressed, a full `yarn cli --suite "TLS"` run ends with an unreapable server still bound to `6379/7379/9121/16379/17379` — the live cluster's own address bank. Task 2 Step 1 below folds in the fix: add a teardown to `TLS-11`'s `Steps`, mirroring `ACL-16`'s own fold-in (kill by PID from `/tmp/acltls-qa/tls.pid` and `acltls.pid`, then confirm `ss` shows the ports free) — same pattern already used elsewhere in this doc.

---

### Task 1: Confirm the live cluster is safely out of the way, then run the TLS suite live and record every mismatch

**Files:** none modified — this task only observes (after the cluster-stop confirmation, which is an operational action, not a file change).

- [ ] **Step 1: Confirm what's actually live before touching anything**

```bash
ss -tlnp | grep -E ':(6379|7379|9121|16379|17379)\b' || echo "TLS ports free"
pgrep -af rocket-mem || echo "no rocket-mem processes"
systemctl --user is-active rocket-mem-shard-a rocket-mem-shard-b rocket-mem-shard-c 2>&1 || true
```

If anything is listening on those ports, **stop and ask the user for explicit confirmation before stopping the live cluster** — do not assume it's safe to take down. Once confirmed, stop it via its own documented procedure (`.claude/runbook-failover.md` / however the six hand-started processes were started — see `project-cluster-runs-under-systemd-user` context if this session has it), noting each PID so the cluster can be restarted afterward in Task 3.

- [ ] **Step 2: Confirm the release binary and `step` CLI**

```bash
ls -la /home/numericlabs/data/rocket/rocket-mem/target/release/rocket-mem 2>&1 || echo "release binary missing"
step version 2>&1 || echo "step CLI missing"
```

Build the binary (`cargo build --release --bin rocket-mem`) or install `step` (see `TLS-01`'s Precondition) first if either is missing.

- [ ] **Step 3: Run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "TLS" 2>&1 | tee /tmp/claude-scratch-tls-live-run-1.log
```

(Use this session's actual scratchpad directory in place of `/tmp/claude-scratch-tls-live-run-1.log` if one is available.)

- [ ] **Step 4: Triage the output**

For every case reported `fail`, read `reporter.ts`'s diff output to see exactly which line(s) of `actual` didn't match `expected`. In particular:

1. **`TLS-09` and `TLS-10`**: their `Expected` blocks still contain `...`-prose from the doc-fixes plan. Take the real `actual` output this run captured, placeholder-ize the dynamic parts (`<date>`, `<n>`, same conventions as `TLS-02`/`TLS-08`), and that becomes the new literal `Expected` text — this is the fill-in the earlier plan deferred rather than guessed at.
2. Any other case that fails for a reason unrelated to #1 — bucket into "should have been wildcarded but wasn't" vs. "wrong, not just variable" vs. "a real product/doc bug" (do not paper over a real bug with a wildcard; flag it for a human decision instead), same triage the ACL live-verification plan used.

Write the findings as a short list (case id → fix needed) rather than fixing inline yet — Task 2 applies them.

---

### Task 2: Apply fixes, re-run until fully green, and confirm ports/files are clean afterward

**Files:**
- Modify: `docs/qa-playbook.md`, `docs/qa-playbook.html` (kept in sync, same discipline as the earlier plans in this chain)

**Interfaces:** none beyond what Plans 1/2/3 already established (including `SUITE_SCRATCH_PATHS["TLS"]` from Plan 3).

- [ ] **Step 1: Apply every fix from Task 1's findings list, plus the teardown fix from Global Constraints**

For each finding, edit both `docs/qa-playbook.md` (by hand) and `docs/qa-playbook.html`'s embedded JSON (via a short Node script, same style as the earlier wiring plan's sync script). Re-verify sync the same way Plan 2 Task 1 Step 2 did.

Additionally — this is required regardless of what Task 1's live run found, per the Global Constraints note above — add a teardown to `TLS-11`'s `Steps` (it's the last case in execution order, same reasoning `ACL-16` used to become ACL's own final-case teardown): after `TLS-11`'s existing `timeout 2` command, append:

```bash
for f in /tmp/acltls-qa/tls.pid /tmp/acltls-qa/acltls.pid; do
  [ -f "$f" ] && kill "$(cut -d= -f2 "$f")" 2>/dev/null
done
sleep 1
ss -lnt | grep -E ':(6379|7379|16379|17379|9121)\b' || echo "ports free"
```

Prepend the matching `ports free` line to `TLS-11`'s `Expected` block (same shape as `ACL-16`'s own folded-in teardown output). Delete the now-redundant standalone "TLS teardown" section once this lands (its content now lives inside `TLS-11`'s own Steps). Sync this into `docs/qa-playbook.html`'s JSON the same way as every other fix in this step.

- [ ] **Step 2: Re-run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "TLS" 2>&1 | tee /tmp/claude-scratch-tls-live-run-2.log
```

Expected: all 11 cases `pass`. If not, repeat Step 1/Step 2 until they do.

- [ ] **Step 3: Confirm ports are clean afterward**

```bash
ss -lnt | grep -E ':(6379|7379|9121|16379|17379)\b' || echo "TLS ports free"
```

Expected: `TLS ports free` — the suite's own `yarn cli` run ends with `killIfRocketMem`'s safety net (`tools/qa-agent/src/index.ts`) having nothing left to do, same as the ACL suite's own live-verify confirmed.

- [ ] **Step 4: Commit the doc fixes, if any were needed**

```bash
git add docs/qa-playbook.md docs/qa-playbook.html
git commit -m "docs: fix TLS suite expected output found live-verifying qa-agent automation"
```

Skip this step if Task 1 found zero mismatches (unlikely, given `TLS-09`/`TLS-10` need real capture regardless — but check).

---

### Task 3: Wire up the live UI, verify the Clean Environment button end to end, and restart the cluster

**Files:** none modified — verification only.

- [ ] **Step 1: Restart the qa-agent dev server so it picks up the new suite registration**

```bash
pkill -f "tsx src/index.ts --serve"
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn serve &
disown
sleep 1
```

- [ ] **Step 2: Confirm TLS cases are reported runnable and clearable**

```bash
curl -s http://127.0.0.1:4848/api/status | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => {
  const { runnable, clearable } = JSON.parse(data);
  console.log("TLS runnable count:", runnable.filter(id => id.startsWith("TLS-")).length, "(expect 11)");
  console.log("TLS clearable:", clearable.includes("TLS"));
});
'
```

- [ ] **Step 3: Confirm the Run button's own request path works**

```bash
curl -s -X POST http://127.0.0.1:4848/api/run -H "Content-Type: application/json" -d '{"caseId":"TLS-01"}' | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => console.log(JSON.parse(data).ran.map(r => [r.id, r.verdict])));
'
```

Expected: a genuine `pass` for `TLS-01` — this is the exact request the browser's "Run" button sends (`docs/qa-playbook.html`'s `triggerRun`), so a real pass here proves the button's full click-to-execution path works, not just its visibility condition.

- [ ] **Step 4: Confirm the Clean environment button's own request path removes TLS's scratch files**

```bash
ls /tmp/acltls-qa
curl -s -X POST http://127.0.0.1:4848/api/clear-env -H "Content-Type: application/json" -d '{"section":"TLS"}' | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => console.log(JSON.parse(data)));
'
ls /tmp/acltls-qa
```

Expected: the second `ls` shows TLS's files gone (matching Plan 3's `SUITE_SCRATCH_PATHS["TLS"]` list) — this closes the second half of the original ask ("remove/clean the entire test env along with ports and configuration files").

- [ ] **Step 5: Full regression**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn test && yarn typecheck
```

Expected: all green.

- [ ] **Step 6: Restart the live cluster, if this plan's Task 1 Step 1 stopped it**

Bring it back up via its own documented procedure — same one used to stop it. Confirm with the same `ss`/`pgrep` check from Task 1 Step 1 that it's back.

- [ ] **Step 7: Report back to the user**

Summarize: how many live-run mismatches were found and fixed (Task 1/2, including the `TLS-09`/`TLS-10` fill-in), confirmation both the Run and Clean environment buttons work end to end for TLS, whether the live cluster was stopped/restarted during this plan, and the final commit list (`git log --oneline -8`).

## Next plan

None — this closes the TLS suite automation chain (the first half of the original ask). The Environment setup suite automation chain (`docs/superpowers/plans/2026-09-15-env-suite-automation-1-wiring.md`) is a separate, independently-scoped chain — it was started in parallel, not as a continuation of this one.
