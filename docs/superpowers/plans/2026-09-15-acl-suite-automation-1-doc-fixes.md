# ACL Suite Automation — Doc Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `docs/qa-playbook.md`'s "ACL and authentication" suite (ACL-01..ACL-19) match the placeholder/ordering conventions every already-automated suite (Persistence, Configuration layering, RMP protocol, Observability, ...) already follows, so it can be registered with `tools/qa-agent` in the next plan.

**Architecture:** `tools/qa-agent/src/matcher.ts` auto-wildcards any `<word>` bracketed token in an `Expected` block (see `PLACEHOLDER_RE`) plus box-drawing borders/padding. Onboarded suites replace every run-to-run-variable value (PIDs, timestamps, per-connection ids, argon2 hashes, counters) with such tokens instead of the literal value a human captured once. The ACL section currently has five cases with literal captured values, one case (ACL-16) whose precondition ordering conflicts with three later cases, and one case (ACL-19) using an informal (non-bracketed) placeholder convention the matcher won't recognize.

**Tech Stack:** Markdown editing only — no code in this plan.

**Spec:** none (bounded task; design agreed in chat — see conversation for the ACL-16 reordering decision).

## Global Constraints

- Every dynamic value must use the `<word>` bracket format (letters/digits/underscore/space/hyphen only) — `PLACEHOLDER_RE` in `tools/qa-agent/src/matcher.ts:16` is `/<[a-zA-Z_][a-zA-Z0-9_ -]*>/g`.
- Do not touch any case's `Steps` block unless explicitly instructed — only `Expected` blocks, one `Precondition`, and structural reordering are in scope for this plan.
- Never invent a "correct" value for something you haven't verified live — where a literal looks wrong (e.g. ACL-05's version string) flag it in a comment for the live-verification plan (Plan 3) rather than guessing a fix.

---

### Task 1: Placeholder-ize the server-lifecycle cases (ACL-01, ACL-05, ACL-11, ACL-12, ACL-16)

**Files:**
- Modify: `docs/qa-playbook.md` (ACL-01 ~line 6634, ACL-05 ~line 6827, ACL-11 ~line 7049, ACL-12 ~line 7095, ACL-16 ~line 7261)

**Interfaces:** N/A (doc-only).

- [x] **Step 1: ACL-01 — wildcard the PID and the six startup timestamps**

In the `**Expected:**` block starting at `docs/qa-playbook.md:6684`, replace:

```
PID=2373827
2026-09-12T05:36:10.429907Z  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
2026-09-12T05:36:10.429946Z  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=/tmp/acltls-qa/acl.aof snapshot_path=/tmp/acltls-qa/acl.snap log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=4 tls_enabled=false tls_replication_enabled=false
2026-09-12T05:36:10.554031Z  INFO rocket_mem::aof: aof recovery replay complete commands=0 bytes=0 elapsed_us=6
2026-09-12T05:36:10.554361Z  INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:9310/metrics
2026-09-12T05:36:10.554413Z  INFO rocket_mem: listener bound protocol=RMP addr=127.0.0.1:6511
2026-09-12T05:36:10.554437Z  INFO rocket_mem: listener bound protocol=RESP addr=127.0.0.1:6510
```

with:

```
PID=<pid>
<date>  INFO rocket_mem: rocket-mem starting version="0.1.4" node_id=127.0.0.1:6510
<date>  INFO rocket_mem: resolved config summary node_id=127.0.0.1:6510 addr=127.0.0.1:6510 rmp_addr=127.0.0.1:6511 metrics_addr=127.0.0.1:9310 aof_path=/tmp/acltls-qa/acl.aof snapshot_path=/tmp/acltls-qa/acl.snap log_filter=info log_value_max_bytes=128 slowlog_threshold_micros=10000 cluster_mode=false acl_enabled=true acl_user_count=4 tls_enabled=false tls_replication_enabled=false
<date>  INFO rocket_mem::aof: aof recovery replay complete commands=0 bytes=0 elapsed_us=<n>
<date>  INFO rocket_mem: listener bound protocol=metrics addr=http://127.0.0.1:9310/metrics
<date>  INFO rocket_mem: listener bound protocol=RMP addr=127.0.0.1:6511
<date>  INFO rocket_mem: listener bound protocol=RESP addr=127.0.0.1:6510
```

(The box banner directly below needs no edit — `BOX_BORDER_RUN_RE`/`BOX_PADDING_RUN_RE` in matcher.ts already wildcard border/padding runs, and every interior label line here is static text with no PID/timestamp in it.)

- [x] **Step 2: ACL-05 — wildcard the per-connection `id`**

In the `**Expected:**` block starting at `docs/qa-playbook.md:6838`, change the line `id 9` to `id <n>`.

- [x] **Step 3: ACL-11 — wildcard the five argon2 hashes**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7062`, replace each of the five `$argon2id$v=19$m=19456,t=2,p=1$...` hash strings (four inside the `ACL LIST` lines, one inside the `ACL GETUSER app` `passwords` section) with `<hash>`. Example — the `admin` line:

```
user admin on #$argon2id$v=19$m=19456,t=2,p=1$OPHgcsV/dVHnIOriFp6Ltw$eVHSkyW7ilQCuktH/RBgP8omeUclaeBiihrzy8xDrCQ +@all ~*
```

becomes:

```
user admin on #<hash> +@all ~*
```

Apply the same substitution to the `scoped`, `app`, and `retired` lines, and to the standalone hash line under `passwords`.

- [x] **Step 4: ACL-12 — wildcard its one argon2 hash**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7112`, replace the single `$argon2id$...` line with `<hash>`.

- [x] **Step 5: ACL-16 — wildcard the four startup-failure timestamps**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7292`, replace all four `2026-09-12T05:38:30....Z` timestamps with `<date>` (two per failed-start attempt — `rocket-mem starting` and `resolved config summary`, each appearing twice).

- [x] **Step 6: Verify no literal dynamic values remain in the cases touched this step**

Run:
```bash
grep -n '2026-09-1[0-9]T[0-9:.]\+Z\|PID=[0-9]\+\|\$argon2id\$' docs/qa-playbook.md | sed -n '1,40p'
```
Expected: no hits between the `ACL-01` and `ACL-16` headings (hits inside other suites like OBS/RMP that already use literal-free placeholders are fine — check none remain in the ACL section specifically by cross-checking line numbers against `grep -n '^### ACL' docs/qa-playbook.md`).

- [x] **Step 7: Commit**

```bash
git add docs/qa-playbook.md
git commit -m "docs: placeholder-ize ACL suite's server-lifecycle expected output"
```

---

### Task 2: Placeholder-ize the history-dependent cases (ACL-17, ACL-18, ACL-19)

**Files:**
- Modify: `docs/qa-playbook.md` (ACL-17 ~line 7323, ACL-18 ~line 7375, ACL-19 ~line 7410)

**Interfaces:** N/A (doc-only).

- [x] **Step 1: ACL-17 — wildcard the slowlog entry ids/timestamps/durations**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7335`, replace:

```
61
1788234467
25797
AUTH
... (2 more arguments)
60
1788234454
28587
AUTH
... (2 more arguments)
59
1788234453
23238
```

with:

```
<n>
<n>
<n>
AUTH
... (2 more arguments)
<n>
<n>
<n>
AUTH
... (2 more arguments)
<n>
<n>
<n>
```

Leave the second block (`grep -c` output, a literal `0`) untouched — that count is deterministic (ACL-17's own Notes already explain why: nothing ACL-related ever reaches the AOF).

- [x] **Step 2: ACL-18 — wildcard the seven metrics counters**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7386`, replace every count value with `<n>`:

```
# TYPE rocket_mem_command_errors_total counter
rocket_mem_command_errors_total{cmd="acl"} <n>
rocket_mem_command_errors_total{cmd="auth"} <n>
rocket_mem_command_errors_total{cmd="get"} <n>
rocket_mem_command_errors_total{cmd="mget"} <n>
rocket_mem_command_errors_total{cmd="hello"} <n>
rocket_mem_command_errors_total{cmd="set"} <n>
rocket_mem_command_errors_total{cmd="ping"} <n>
```

Leave the `# TYPE ...` line and every `cmd="..."` label exactly as-is — only the trailing count numbers change. Also add a one-line note directly under the block (before **Notes:**) flagging that the *set* of `cmd=` labels present, not just their order, must be confirmed during live verification (Plan 3) since it depends on exactly which earlier cases ran — reuse this exact sentence: `**Automation note:** the label set above must be reconfirmed against a real run before this case is registered — see the ACL suite automation live-verification plan.`

- [x] **Step 3: ACL-19 — convert the informal `N`/`PORT`/`...` tokens to bracket placeholders**

In the `**Expected:**` block starting at `docs/qa-playbook.md:7425`, replace:

```
...  INFO conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: auth success user=admin
...  WARN conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: auth failure user=admin
...  WARN conn{conn_id=N peer=127.0.0.1:PORT protocol=RESP tls=false node_id=...}: rocket_mem::dispatcher: permission denied user=app
```

with:

```
<date>  INFO conn{conn_id=<n> peer=127.0.0.1:<port> protocol=RESP tls=false node_id=127.0.0.1:6510}: rocket_mem::dispatcher: auth success user=admin
<date>  WARN conn{conn_id=<n> peer=127.0.0.1:<port> protocol=RESP tls=false node_id=127.0.0.1:6510}: rocket_mem::dispatcher: auth failure user=admin
<date>  WARN conn{conn_id=<n> peer=127.0.0.1:<port> protocol=RESP tls=false node_id=127.0.0.1:6510}: rocket_mem::dispatcher: permission denied user=app
```

`node_id` is set literally to `127.0.0.1:6510` (matches the ACL suite's fixed config, confirmed deterministic in ACL-01's own capture) rather than wildcarded — flag this for live-run confirmation in Plan 3 too, same as ACL-18's label set.

- [x] **Step 4: Verify**

```bash
grep -n 'conn_id=N\|:PORT \|node_id=\.\.\.' docs/qa-playbook.md
```
Expected: no output (the informal tokens are gone).

- [x] **Step 5: Commit**

```bash
git add docs/qa-playbook.md
git commit -m "docs: placeholder-ize ACL suite's history-dependent expected output"
```

---

### Task 3: Reorder ACL-16 to the end and fold teardown into it

**Files:**
- Modify: `docs/qa-playbook.md` (ACL-16 heading through the standalone "ACL teardown" section, `docs/qa-playbook.md:7261`-`7457`)

**Interfaces:** N/A (doc-only). This task changes case *position* in the file, not any case `id` — `ACL-16` keeps its id (the project already has non-sequential-by-position ids elsewhere, e.g. `CORE-28` sits between `CORE-25` and `CORE-26`), so nothing outside this file needs to change.

- [x] **Step 1: Cut the ACL-16 section (heading through its `---` separator) from its current position**

Currently ACL-16 sits between ACL-15 and ACL-17 (`docs/qa-playbook.md:7261`, right after ACL-15's closing `---`). Remove the whole `### ACL-16 ...` through its trailing `---` (ending right before `### ACL-17`).

- [x] **Step 2: Rewrite ACL-16's Precondition and Steps to own its server teardown, then paste it back in after ACL-19's `---` and before the "ACL teardown" heading**

Replace the old precondition:

```
**Precondition:** The server from ACL-01 is **stopped** (see the teardown block below), and ports
6510, 6511 and 9310 are free. These runs fail before binding anything, but starting from a clean
slate keeps the output unambiguous.
```

with:

```
**Precondition:** ACL-01 through ACL-19 completed; the server from ACL-01 is still running on port
6510.
```

Replace the old Steps (which start straight in with `cat > /tmp/acltls-qa/acl-bad.toml`) by prepending the teardown that used to live in the standalone "ACL teardown" section:

```bash
PID=$(cut -d= -f2 /tmp/acltls-qa/acl.pid)
kill $PID
sleep 1
ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ports free"

cat > /tmp/acltls-qa/acl-bad.toml <<'EOF'
addr = "127.0.0.1:6510"
rmp_addr = "127.0.0.1:6511"
metrics_addr = "127.0.0.1:9310"

[[acl.users]]
username = "admin"
enabled = true
rules = ["on", ">secret123", "allcommands", "allkeys"]
EOF

"$ROCKET_MEM_BIN" --config /tmp/acltls-qa/acl-bad.toml
echo "exit=$?"

# Same file with the `on` token removed, so the password token is the first failure.
sed 's/"on", //' /tmp/acltls-qa/acl-bad.toml > /tmp/acltls-qa/acl-bad2.toml
"$ROCKET_MEM_BIN" --config /tmp/acltls-qa/acl-bad2.toml
echo "exit=$?"

ss -lnt | grep -E ':(6510|6511|9310)\b' || echo "ports free"
```

Prepend the new Expected output for the added teardown lines (`ports free` from the first `ss` check, matching the pattern the old standalone teardown block already documented) to the existing Expected block — the rest of the Expected block (the two failed-start banners and final `ports free`) is unchanged from Task 1's edit.

Add one sentence to the Notes explaining the move, e.g.: "ACL-16 now also stops the ACL-01 server as its own first step (folded in from the old standalone 'ACL teardown' section) — it's the natural last case since, unlike ACL-17/18/19, it has no dependency on the server's accumulated history, only on its ports being free once it's done. Same pattern RMP-05/SMOKE-12/PUBSUB-12 use to end their own suites."

- [x] **Step 3: Delete the now-redundant standalone "ACL teardown" section**

Remove the `### ACL teardown` heading and its bash block entirely (previously `docs/qa-playbook.md:7444`-`7455`) — its content now lives inside ACL-16's own Steps from Step 2.

- [x] **Step 4: Read through the whole ACL section once, start to finish, checking case order and precondition chaining**

Confirm the resulting order is: ACL-01, ACL-02, ..., ACL-15, ACL-17, ACL-18, ACL-19, ACL-16 — and that every precondition correctly references the case immediately (or transitively) before it in this new order. Fix any precondition text that still says "ACL-16" or "teardown block below" elsewhere in the section (search with `grep -n "teardown block below\|ACL-16" docs/qa-playbook.md`).

- [x] **Step 5: Commit**

```bash
git add docs/qa-playbook.md
git commit -m "docs: reorder ACL-16 to run last, folding server teardown into it"
```

## Status: Done (2026-09-15)

Executed inline in the same session (not via subagent-driven-development). All three tasks'
content changes landed in one combined commit rather than three per-task commits — `46dda45
docs: prep ACL suite for qa-agent automation` — since the work was done in a single inline pass;
the resulting file content matches this plan's Task 1/2/3 targets exactly, verified against the
plan's own Step 6/Step 4/Step 4 checks (re-run 2026-09-15, all clean). Plan 2
(`2026-09-15-acl-suite-automation-2-wiring.md`) and Plan 3
(`2026-09-15-acl-suite-automation-3-live-verify.md`) are also both done — see their own Status
sections.

## Next plan

`docs/superpowers/plans/2026-09-15-acl-suite-automation-2-wiring.md` — sync `docs/qa-playbook.html`'s embedded JSON to this plan's markdown changes, add an `ACL-11` unordered-line-group entry to `tools/qa-agent/src/matcher.ts`, and register the suite in `tools/qa-agent/src/suites.ts`.
