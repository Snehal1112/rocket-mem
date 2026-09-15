# Environment Setup Suite Automation — Live Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove `ENV-01`/`02`/`03`/`05` actually pass end to end through `tools/qa-agent` on this machine, fix any real mismatch between the doc's literal `Expected` text and what's actually installed here, and confirm the "Run" button genuinely works for `Environment setup` in `docs/qa-playbook.html` — closing the second half of the original ask.

**Architecture:** Unlike the TLS chain, this suite starts no process and touches no port or file, so there's no live-cluster coordination needed and no `Clean environment` button to verify (see the wiring plan's Global Constraints for why that button doesn't apply here). The only real risk is version drift: `ENV-01` (`redis-cli`), `ENV-02` (`openssl`), `ENV-03` (`curl`), `ENV-05` (`rustc`/`cargo`) all have literal captured version strings in the doc, and this specific machine's installed versions may not match them exactly.

**Tech Stack:** `tools/qa-agent`'s CLI/server (Node/TypeScript, Yarn) — no `rocket-mem` binary needed for these four cases.

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-env-suite-automation-1-wiring.md`).

## Global Constraints

- Any fix discovered here to a literal version string is a doc-accuracy fix (this machine's real installed version), not a placeholder — `ENV-01`'s own Notes already say the version legitimately differs *machine to machine*, but on this one machine it's stable run to run, so it stays a literal, correctly captured value, same as `ENV-03`'s already-literal `curl` version string.
- `tools/qa-agent` is gitignored — no commits for anything under `tools/qa-agent` itself, only for `docs/qa-playbook.md`/`docs/qa-playbook.html` if a real fix is needed there.
- Do not touch `ENV-04`/`ENV-06`..`ENV-11` — out of scope for this plan (`ENV-04` was never in `IN_SCOPE_SUITES`; `ENV-06` was deliberately excluded by the wiring plan).

---

### Task 1: Run the suite live and record every mismatch

**Files:** none modified — this task only observes.

- [ ] **Step 1: Confirm the tools are actually installed**

```bash
redis-cli --version
openssl version
curl --version | head -1
rustc --version
cargo --version
```

- [ ] **Step 2: Run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "Environment setup" 2>&1 | tee /tmp/claude-scratch-env-live-run-1.log
```

(Use this session's actual scratchpad directory in place of `/tmp/claude-scratch-env-live-run-1.log` if one is available.)

- [ ] **Step 3: Triage the output**

For every case reported `fail`, compare the exact installed version (Step 1) against the doc's literal `Expected` text (`docs/qa-playbook.md:174`-`177` for `ENV-01`, `:195`-`198` for `ENV-02`, `:212`-`215` for `ENV-03`, `:251`-`255` for `ENV-05`). Write the findings as a short list (case id → real value to substitute) — this is expected to be "the doc's captured value is stale for this machine," not a product bug; there is no product code involved in any of these four cases.

---

### Task 2: Apply fixes, re-run until green, and wire up the live UI

**Files:**
- Modify (only if Task 1 found a mismatch): `docs/qa-playbook.md`, `docs/qa-playbook.html`

**Interfaces:** none beyond what the wiring plan already established.

- [ ] **Step 1: Apply every fix from Task 1's findings list, if any**

Edit `docs/qa-playbook.md`'s literal `Expected` text to the real installed version, and sync `docs/qa-playbook.html`'s embedded JSON the same way the TLS chain's wiring plan did (a short Node script substituting the exact old string for the new one in the `"Environment setup"` section's matching case id).

- [ ] **Step 2: Re-run the suite**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn cli --suite "Environment setup" 2>&1 | tee /tmp/claude-scratch-env-live-run-2.log
```

Expected: all 4 cases (`ENV-01`, `ENV-02`, `ENV-03`, `ENV-05`) `pass`.

- [ ] **Step 3: Commit the doc fixes, if any were needed**

```bash
git add docs/qa-playbook.md docs/qa-playbook.html
git commit -m "docs: fix Environment setup's expected tool versions found live-verifying qa-agent automation"
```

Skip if Task 1 found zero mismatches.

- [ ] **Step 4: Restart the qa-agent dev server and confirm the Run button's request path**

```bash
pkill -f "tsx src/index.ts --serve"
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn serve &
disown
sleep 1

curl -s http://127.0.0.1:4848/api/status | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => {
  const { runnable, clearable } = JSON.parse(data);
  console.log("ENV runnable:", runnable.filter(id => id.startsWith("ENV-")));
  console.log("Environment setup clearable:", clearable.includes("Environment setup"));
});
'

curl -s -X POST http://127.0.0.1:4848/api/run -H "Content-Type: application/json" -d '{"caseId":"ENV-01"}' | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => console.log(JSON.parse(data).ran.map(r => [r.id, r.verdict])));
'
```

Expected: `ENV runnable:` is exactly `["ENV-01","ENV-02","ENV-03","ENV-05"]`; `Environment setup clearable:` is `false` (by design — see the wiring plan); the `/api/run` call reports a real `pass` for `ENV-01` — the exact request the browser's "Run" button sends.

- [ ] **Step 5: Full regression**

```bash
cd /home/numericlabs/data/rocket/rocket-mem/tools/qa-agent
yarn test && yarn typecheck
```

Expected: all green.

- [ ] **Step 6: Report back to the user**

Summarize: how many live-run mismatches were found and fixed (Task 1/2), confirmation the Run button works end to end for `Environment setup`, the explicit note that no Clean Environment button was added (nothing for this suite to clean) and `ENV-06` stays manual-only, and the final commit list (`git log --oneline -8`).

## Next plan

None — this closes the Environment setup suite automation chain (the second half of the original ask). Both chains (TLS's four plans, this one's two) are independent; see the TLS chain's own final plan (`docs/superpowers/plans/2026-09-15-tls-suite-automation-4-live-verify.md`) for that half's closing status.
