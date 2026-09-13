# QA agent: automated runner + live status for `docs/qa-playbook.html`

## Problem

`docs/qa-playbook.html` is a single-page checklist: 184 manual test cases embedded as JSON
(`<script type="application/json" id="data">`), each with `id`/`area`/`title`/`precondition`/
`steps`/`expected`/`notes`. A human runs each case's shell commands by hand, compares output to
`expected`, and clicks Pass/Fail, which is stored in `localStorage` under key `rocketmem-qa-v1`
(`{caseId: 'pass'|'fail'|'todo'}`) and re-rendered into the page's ticks/strip/counts.

We want an agent that executes the automatable subset of these cases against a real `rocket-mem`
instance, judges pass/fail itself, and drives the *same* page live — so a tester watching the
open tab sees ticks flip in real time as the agent works through a suite, exactly as if a human
were clicking Pass/Fail. Beyond bulk suite runs, each in-scope case in the page also gets a
**Run** button: clicking it executes that one scenario against a live server right then, the way
a human tester would, rather than requiring a whole-suite batch run.

## Scope (v1)

In scope — all single-instance, no Docker/multi-node/TLS-cert setup, matching the section headers
in the playbook's own JSON exactly:

| Section (JSON `title`) | Cases | Notes |
|---|---|---|
| Environment setup | 5 of 11 | Only `ENV-01`,`ENV-02`,`ENV-03`,`ENV-05`,`ENV-06` (tool-presence checks + build from source). `ENV-04`,`ENV-07`..`ENV-11` (Docker build/run, release-archive download/pull) excluded — Docker permission traps and network-dependent downloads the doc itself flags as flaky, and not needed once ENV-06's source build succeeds. |
| Smoke suite | 12 | ports 6540/6541/9340 |
| Core data types and keys | 54 | ports 6550/6551 |
| Transactions | 10 | ports 6620/6621 |
| Persistence | 5 | ports 6560/6561 |
| Configuration layering | 9 | ports 6570/6571/6572/6573/9370 (shared block with RMP/OBS) |
| RMP protocol | 5 | same shared block |
| Observability | 14 | same shared block |

Total: **114 of 184 cases**.

Out of scope for v1, left fully manual (unchanged in the UI — just never touched by the agent):
Replication, Pub/sub, Cluster, ACL and authentication, TLS, and the 6 excluded ENV cases above.
These need multi-node orchestration, cert generation, or carry documented host-specific traps
(Docker signal-mediation bug) that make them a materially heavier and riskier build. Nothing
about the design below blocks adding them later as a follow-on.

## Non-goals

- No LLM judging any case. All comparison is deterministic (string diff + wildcard + a small
  explicit tolerance table for the handful of cases with documented numeric drift, e.g. TTL
  counting down between `EXPIRE`/`SET EX` and the following `TTL` call).
- No CI wiring in this pass (mirrors `tools/review-agent`'s own "not built here" stance on
  its own follow-on ideas).
- No new distinction between "agent-verified" and "human-verified" in the stored state — an
  automated result and a manual click both just set `state[id] = 'pass'|'fail'`, sharing
  `rocketmem-qa-v1` exactly as today. A rerun overwrites a prior result, same as a human
  re-clicking would.
- Does not modify `docs/qa-playbook.md` (the markdown source) — only the already-generated
  `docs/qa-playbook.html`, plus new files under `tools/qa-agent/`.

## Why this shape

`tools/review-agent/` already establishes this repo's convention for a small custom "agent":
a standalone Node/TypeScript project under `tools/<name>/`, Yarn-managed, not part of the Cargo
workspace. `qa-agent` follows that same convention (own `package.json`, `tsx` for running
TypeScript directly, no build step) — it just doesn't pull in `@anthropic-ai/claude-agent-sdk`,
since the runner here is deterministic rather than LLM-driven.

## Architecture

```
tools/qa-agent/
  package.json            # yarn, tsx, typescript, @types/node — no LLM SDK
  tsconfig.json
  README.md
  src/
    index.ts              # CLI entry: `yarn run --suite smoke`, `yarn run --all`, `yarn serve`
    playbook.ts            # loads docs/qa-playbook.html, extracts+parses the #data JSON
    suites.ts               # in-scope suite list: title, known ports to scan for cleanup
    processTracker.ts        # resolve/kill rocket-mem PIDs via `ss -tlnp` on known ports —
                              # never `pkill -f`; only kills a PID this run itself observed
    caseRunner.ts             # executes one case's `steps` as `bash -c`, substitutes `<pid>`,
                              # captures stdout+exit code
    matcher.ts                 # actual vs `expected`: literal diff, `<...>` wildcard tokens
                              #   (already used throughout `expected`, e.g. `<pid>`, `<n>`,
                              #   `<date>`), plus a small per-case-id tolerance table
    sessionRunner.ts            # runSuite(suite, {stopAfterCaseId?, alreadyRun}): the one
                              #   sequential-execution path both the CLI's bulk suite runs and
                              #   the browser's on-demand Run-button clicks call into — see
                              #   "On-demand single-case runs" below
    server.ts                 # local HTTP server: serves docs/qa-playbook.html, POST
                              #   /api/result (from sessionRunner), POST /api/run {caseId}
                              #   (triggers an on-demand chained run), GET /events (SSE
                              #   fan-out), GET /api/status (snapshot for a freshly opened tab)
    reporter.ts                # console summary + writes a JSON run snapshot to
                              #   tools/qa-agent/.run/results/<timestamp>.json (gitignored)
  .run/                       # gitignored: scratch server data dirs, PID tracking, results
```

### Data flow

1. `yarn serve` starts `server.ts`, which serves `docs/qa-playbook.html` at
   `http://localhost:<port>/qa-playbook.html` (same-origin — no CORS needed) and opens the SSE
   endpoint. The tester opens that URL in a browser and leaves it open.
2. `yarn run --suite <name>` (or `--all`) starts `sessionRunner`, which:
   a. picks the next in-scope suite section, in the JSON's own order;
   b. for each case in that section, in order, calls `caseRunner` to: substitute `<pid>` (see
      below), run `steps` verbatim as `bash -c` in a suite-scoped scratch working directory (so a
      stray `rocket-mem.toml`/AOF/snapshot never leaks between suites — same reasoning the
      playbook itself gives for per-section data paths), and capture combined stdout;
   c. diffs actual output against `expected` via `matcher.ts`;
   d. POSTs `{id, verdict, actual, expected}` to the running `server.ts` (if reachable — the
      runner works standalone without a browser open, it just has nothing to push results to);
   e. `server.ts` fans the result out over SSE to every connected tab.
3. The browser-side addition to `qa-playbook.html` (a ~10-line script block) opens an
   `EventSource('/events')` and on each message does exactly what a manual Pass/Fail click does:
   `state[id] = verdict; save(); render();` — so every existing visual (per-case tick, section
   strip, run strip, nav rail, pass/fail/todo counts) updates live with zero other page changes.
4. At the end of a suite run, `processTracker` kills only the PID(s) it resolved via `ss -tlnp`
   on that suite's known ports and confirmed as `rocket-mem` — never a broad pattern kill.
5. `reporter.ts` prints a final pass/fail/error summary and writes the run snapshot.
6. Alternatively, clicking a case's **Run** button in the browser POSTs `{caseId}` to
   `/api/run`; `server.ts` resolves the case's suite and calls the same `sessionRunner.runSuite`
   used by step 2, with `stopAfterCaseId` set to the clicked case — see "On-demand single-case
   runs" below for exactly what that runs.

### Executing `steps` faithfully, including server lifecycle

Each case's `steps` field is already self-contained, literal bash (confirmed by inspection,
including the `MULTI...EXEC` cases which use a heredoc to keep one `redis-cli` connection open
for the whole transaction). The runner does not need a bespoke "start suite's server" step
separate from the cases themselves: a suite's server is started by whichever case's own steps
first launches it (e.g. `SMOKE-01`, `CFG-01`), same as a human tester would, and stays running
(a background `&` process outlives the short-lived `bash -c` that spawned it) for however many
subsequent cases the playbook's own preconditions say it should — e.g. Observability's setup
note: "start it the same way and keep it running through OBS-05." The runner just runs each
case's steps in order, in the same working directory, and trusts the doc's own sequencing.

The one exception: `SMOKE-12` is the only in-scope case whose `steps` contains a literal `<pid>`
placeholder (`kill -TERM <pid>` / `ps -p <pid>`), meant for a human to fill in by hand. The
runner substitutes it with the PID it resolves via `ss -tlnp` for that suite's known port,
immediately before running that case.

### On-demand single-case runs (Run button)

Most cases' `precondition` assumes earlier cases in the same suite already ran against the same
still-running server (shared instance, accumulated state — e.g. `CORE-09` assumes `CORE-08` left
behind a hash key). A bare "run just this one case" click can't satisfy that on its own, so a
click auto-chains rather than either failing outright or silently running the case against
unsatisfied state:

- `server.ts` keeps per-suite session state in memory: whether that suite's server is currently
  running (and its PID, once resolved) and the ordered set of case ids that have successfully
  run against it *this session* (i.e. since `yarn serve` started, or since the suite's server was
  last (re)started).
- On a Run-button click for case `X` in suite `S`: `sessionRunner.runSuite(S, { stopAfterCaseId:
  X, alreadyRun: <S's already-run set> })` walks `S`'s cases in playbook order, skips any already
  in `alreadyRun`, and executes (starting `S`'s server first if it isn't up yet) every case up to
  and including `X` that hasn't run yet this session — reusing the same per-case execution,
  matching, and result-push path bulk suite runs use, so every case that actually ran in the
  chain (not just `X`) streams a result to the browser, not a single result for `X` alone.
- If any case *before* `X` in the chain fails, the chain stops there: `X` itself is never
  attempted (its precondition is not actually satisfied), and is reported with the same `not run`
  state the Safety section already defines for a suite whose server failed to start — extended
  here to also mean "blocked by an earlier failed precondition in this on-demand chain," not only
  "the suite's server never started."
- A suite's already-run set is cleared when its server is (re)started from scratch (a fresh
  `yarn serve` process, or after `processTracker` has torn it down) — so a stale in-memory record
  never claims a precondition is satisfied against a server that no longer holds that state.

### Matching `expected`

- Exact line-for-line match after normalizing `<...>` bracketed tokens (e.g. `<pid>`, `<n>`,
  `<date>`, `<user>`, `<group>`) to wildcards — this convention is already used throughout the
  playbook's own `expected` blocks for exactly this purpose.
- A small explicit tolerance table, keyed by case id, for the documented-but-not-placeholder
  numeric drift cases (e.g. `CORE-02`/`CORE-03`'s TTL/PTTL countdown, `SMOKE-07`'s `TTL` value) —
  allow actual ≤ expected and within a small delta, not a generic fuzzy-diff heuristic. Seed this
  table from the cases already known (from reading the playbook) to have this pattern; expect to
  add entries during implementation as the runner is actually run against a live server and
  produces real mismatches.
- Anything else that doesn't match is a `fail`, with the actual output captured for the report
  and pushed to the browser (so the case detail view, which already renders `expected`, can show
  what was actually seen — the live push includes the actual text, not just the verdict, so a
  human can inspect a live failure without re-running it by hand).

### Safety

- Ports for all in-scope suites (6540/41, 6550/51, 6560/61, 6570-73, 6620/21, plus their
  9340/9350/9370 metrics ports) are already distinct from both the live hand-started cluster's
  ports (6379-6381/7379-7381/9121-9123, confirmed against the checked-in `rocket-mem*.toml`
  files) and the out-of-scope suites' ports (7101-7103 cluster, 6600s/6610s pubsub, 6630/6640
  replication-adjacent, 65xx TLS) — the runner never needs to invent new ports, it reuses exactly
  what the playbook already documents per suite.
- PID resolution is always via `ss -tlnp` on a suite's known port, confirmed to be a
  `rocket-mem` process, before any kill — never `pkill -f rocket-mem` (this has caused real
  confusion before, per the playbook's own warning and this project's own operating history).
- Each suite runs in its own scratch working directory under `tools/qa-agent/.run/`, gitignored,
  so AOF/snapshot files never collide across suites or with the tracked `rocket-mem*.toml`
  configs at the repo root.
- Every case gets a timeout (a case whose server never started, or whose command hangs — e.g. a
  `SUBSCRIBE` accidentally run blocking — must not stall the whole run); a timed-out case reports
  as `fail` with a distinct "timed out" reason and the runner moves on.
- If a suite's server fails to start, or an on-demand chain's earlier case fails its own
  precondition-satisfying run (see "On-demand single-case runs" above), the affected case(s) are
  reported as `not run` (a fourth state, distinct from pass/fail/todo) rather than silently
  skipped — pushed to the browser too, but the browser-side script only special-cases
  `pass`/`fail` (matching the page's existing three-state model); `not run` for now just leaves
  the case as `todo`, since the page doesn't currently have a fourth visual state and adding one
  is out of scope for this pass.

## Testing plan

- Unit tests (in `tools/qa-agent`, run via the project's normal `yarn test` once added) for
  `matcher.ts`'s wildcard/tolerance logic against hand-picked real case data (exact match, `<...>`
  wildcard match, TTL-drift tolerance, a deliberate mismatch that must fail).
- One integration-style test that runs the full Smoke suite end-to-end against a real built
  binary in a scratch directory and asserts all 12 cases pass — this is the walking-skeleton
  proof that `sessionRunner`/`caseRunner`/`processTracker`/`matcher` work together against the
  real server, not just mocks.
- A test exercising the on-demand chain specifically: clicking a case partway through a suite
  (e.g. `CORE-05`) with nothing yet run this session executes every preceding not-yet-run case
  first, in order, and reports a result for each — not just the clicked case.
- Manual verification: run `yarn serve`, open the page, run `yarn run --suite smoke`, watch the
  ticks flip live; confirm the live cluster (`ss -tlnp | grep -E ':(6379|6380|6381)\b'`) is
  untouched before and after.

## Open items deferred to the implementation plan

- Exact heuristic for "does this case's `steps` start a new background server" (needed so
  `processTracker` knows when to re-resolve the tracked PID) — likely: contains
  `ROCKET_MEM_ADDR=` and ends with `&`, but confirm against all 114 in-scope cases while
  implementing rather than enumerating them all in this spec.
- Full seed list for the matcher's numeric-tolerance table — start with the cases already
  identified while writing this spec (`CORE-02`, `CORE-03`, `SMOKE-07`) and extend as real runs
  surface more.
- **A required Plan 2 design input, found during Plan 1's final review, not a Plan 1 defect:**
  roughly 12 of the 114 in-scope cases — concentrated in Observability (9 of its 14) — encode
  variable values in conventions the tolerance table can't express: literal `...` elision
  (`aof_path=...`, `WARN ...:`), `NNNNN`/`NNN` numeric masks (`peer=127.0.0.1:NNNNN`), values
  baked in from one specific run (`process_id:2389374`, Prometheus counter values), and at least
  one case (`OBS-01`) whose `expected` block is a transcribed excerpt of a longer real capture,
  not the whole thing (its `steps` run a bare `redis-cli info server` with no `grep`, so strict
  line-count equality rejects the real output outright). This needs a genuinely new matching
  mode — a `...`/`NNNNN` normalization pass, plus an opt-in "expected is a subset of actual"
  containment check for excerpted cases — not more tolerance-table entries. Budget for
  Observability being the hard section when planning `matcher.ts`'s extension in Plan 2.
