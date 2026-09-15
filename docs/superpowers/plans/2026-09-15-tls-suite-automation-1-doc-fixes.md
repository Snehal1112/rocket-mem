# TLS Suite Automation — Doc Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `docs/qa-playbook.md`'s `TLS` suite (`TLS-01`..`TLS-11`) match the placeholder conventions every already-automated suite (ACL, Persistence, Observability, ...) follows, so it can be registered with `tools/qa-agent` in the next plan — the same first step the ACL suite automation chain took (`docs/superpowers/plans/2026-09-15-acl-suite-automation-1-doc-fixes.md`).

**Architecture:** `tools/qa-agent/src/matcher.ts` auto-wildcards any `<word>` bracketed token in an `Expected` block (`PLACEHOLDER_RE`) plus box-drawing borders/padding. Most of the TLS section already uses this convention (`TLS-02`/`TLS-08` were captured post-structured-logging and already use `<date>`/`<n>`). Two real gaps remain: `TLS-01`'s `ls -l` output has literal byte sizes/owner/timestamp, and `TLS-11` has one line starting with an informal `...` instead of `<date>`. `TLS-09` and `TLS-10` are a third, different kind of gap — their `Expected` blocks describe omitted log lines in prose ("... startup/config-summary logging, then...") instead of showing literal text — and this plan deliberately does **not** try to fill those in by guessing: per this project's standing rule (see the ACL suite automation Plan 1's own Global Constraints), never invent a value you haven't verified live. That expansion is left to the live-verification plan, which captures the real output first.

**Tech Stack:** Markdown editing only — no code in this plan.

**Spec:** none (bounded task; mirrors the ACL suite automation chain's Plan 1).

## Global Constraints

- Every dynamic value must use the `<word>` bracket format (letters/digits/underscore/space/hyphen only) — `PLACEHOLDER_RE` in `tools/qa-agent/src/matcher.ts:16` is `/<[a-zA-Z_][a-zA-Z0-9_ -]*>/g`. Reuse the project's existing token names (`<n>`, `<date>`, `<user>`, `<group>`) rather than inventing new ones.
- Do not touch any case's `Steps` block — only `Expected` blocks are in scope for this plan.
- Do not guess at `TLS-09`/`TLS-10`'s omitted log lines. Leave their `...`-prose `Expected` blocks exactly as they are; the live-verification plan replaces them with a real capture.
- Do not touch the ACL section, or any other suite's cases, while making these edits.

---

### Task 1: Placeholder-ize `TLS-01`'s `ls -l` output

**Files:**
- Modify: `docs/qa-playbook.md` (`TLS-01`'s `**Expected:**` block, `docs/qa-playbook.md:7538`-`7546`)

**Interfaces:** N/A (doc-only).

- [ ] **Step 1: Replace the literal size/owner/timestamp values**

In the `**Expected:**` block at `docs/qa-playbook.md:7539`-`7546`, replace:

```
Your certificate has been saved in cert.pem.
Your private key has been saved in key.pem.
exit=0
total 8
-rw------- 1 numericlabs numericlabs 599 Sep 13 08:36 cert.pem
-rw------- 1 numericlabs numericlabs 227 Sep 13 08:36 key.pem
```

with:

```
Your certificate has been saved in cert.pem.
Your private key has been saved in key.pem.
exit=0
total <n>
-rw------- 1 <user> <group> <n> <date> cert.pem
-rw------- 1 <user> <group> <n> <date> key.pem
```

The case's own Notes already say "Exact byte sizes vary slightly per key; owner and timestamp will be yours" — this makes that caveat machine-enforceable instead of just prose. `<user>`/`<group>` and `<n>` are both already-established tokens elsewhere in this doc (confirmed via `grep -o '<[a-zA-Z_][a-zA-Z0-9_ -]*>' docs/qa-playbook.md | sort -u`), not new ones invented for this case.

- [ ] **Step 2: Verify**

```bash
grep -n 'numericlabs numericlabs 599\|numericlabs numericlabs 227\|total 8$' docs/qa-playbook.md
```

Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add docs/qa-playbook.md
git commit -m "docs: placeholder-ize TLS-01's ls -l output"
```

---

### Task 2: Fix `TLS-11`'s informal ellipsis token

**Files:**
- Modify: `docs/qa-playbook.md` (`TLS-11`'s `**Expected:**` block, `docs/qa-playbook.md:7997`-`8001`)

**Interfaces:** N/A (doc-only).

- [ ] **Step 1: Replace the leading `...` with `<date>`**

At `docs/qa-playbook.md:7999`-`8001`, change:

```
...  WARN rocket_mem: replica_announce_addr is unset while a TLS listener is configured -- this node advertises its plaintext address to its leader announced=numericlabs.lxd:6379
```

to:

```
<date>  WARN rocket_mem: replica_announce_addr is unset while a TLS listener is configured -- this node advertises its plaintext address to its leader announced=numericlabs.lxd:6379
```

Every other structured-logging line in this suite (`TLS-02`, `TLS-08`) already uses `<date>` for its leading timestamp; `TLS-11` was the one case still using the old informal `...` convention (the same class of gap `ACL-19`'s `conn_id=N`/`:PORT`/`node_id=...` tokens had before the ACL suite automation's Plan 1 Task 2 Step 3 fixed them).

- [ ] **Step 2: Verify**

```bash
grep -n '^\.\.\.  WARN\|^\.\.\.  INFO' docs/qa-playbook.md | sed -n '1,20p'
```

Expected: no hits inside the `TLS` section (cross-check any remaining hits' line numbers against `grep -n '^## TLS$\|^## Known limits' docs/qa-playbook.md` to confirm they fall outside `TLS-01`..`TLS-11`).

- [ ] **Step 3: Commit**

```bash
git add docs/qa-playbook.md
git commit -m "docs: fix TLS-11's informal ellipsis placeholder"
```

---

### Task 3: Confirm `TLS-09`/`TLS-10` are correctly left alone, and record why

**Files:** none modified — this task only documents a decision already reflected in the Global Constraints above.

**Interfaces:** N/A.

- [ ] **Step 1: Re-read `TLS-09` and `TLS-10`'s `Expected` blocks**

`docs/qa-playbook.md:7899`-`7908` (`TLS-09`) and `:7963`-`7971` (`TLS-10`) both describe part of their output in prose (`... startup/config-summary logging, then ...`) rather than showing literal text. Unlike `TLS-01`/`TLS-11`, there is no mechanical substitution to make here — filling in the omitted lines requires knowing exactly which `listener bound` events appear, in what order, before each case's error/success line, and that can only come from a real run.

- [ ] **Step 2: Confirm neither block was edited**

```bash
git diff --stat docs/qa-playbook.md
```

Expected: only the `TLS-01` and `TLS-10` — no, `TLS-11` — hunks from Task 1/Task 2 appear (i.e. two commits already landed, nothing further pending). No diff should touch lines in the `TLS-09`/`TLS-10` ranges.

- [ ] **Step 3: No commit** — this task makes no file changes; it exists to record the decision in the plan so the next plan in this chain knows exactly what's still open.

## Next plan

`docs/superpowers/plans/2026-09-15-tls-suite-automation-2-wiring.md` — sync `docs/qa-playbook.html`'s embedded JSON to this plan's markdown changes, and register the `TLS` suite in `tools/qa-agent/src/suites.ts` (it isn't in `IN_SCOPE_SUITES` at all yet, unlike ACL was before its own wiring plan).
