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
were clicking Pass/Fail.

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
    server.ts                 # local HTTP server: serves docs/qa-playbook.html, POST
                              #   /api/result (from caseRunner), GET /events (SSE fan-out),
                              #   GET /api/status (snapshot for a freshly opened tab)
    reporter.ts                # console summary + writes a JSON run snapshot to
                              #   tools/qa-agent/.run/results/<timestamp>.json (gitignored)
  .run/                       # gitignored: scratch server data dirs, PID tracking, results
```

### Data flow

1. `yarn serve` starts `server.ts`, which serves `docs/qa-playbook.html` at
   `http://localhost:<port>/qa-playbook.html` (same-origin — no CORS needed) and opens the SSE
   endpoint. The tester opens that URL in a browser and leaves it open.
2. `yarn run --suite <name>` (or `--all`) starts `caseRunner`, which:
   a. picks the next in-scope suite section, in the JSON's own order;
   b. for each case in that section, in order: substitutes `<pid>` (see below), runs `steps`
      verbatim as `bash -c` in a suite-scoped scratch working directory (so a stray
      `rocket-mem.toml`/AOF/snapshot never leaks between suites — same reasoning the playbook
      itself gives for per-section data paths), captures combined stdout;
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
- If a suite's server fails to start, the remaining cases in that suite are reported as
  `not run` (a fourth state, distinct from pass/fail/todo) rather than silently skipped — pushed
  to the browser too, but the browser-side script only special-cases `pass`/`fail` (matching the
  page's existing three-state model); `not run` for now just leaves the case as `todo`, since the
  page doesn't currently have a fourth visual state and adding one is out of scope for this pass.

## Testing plan

- Unit tests (in `tools/qa-agent`, run via the project's normal `yarn test` once added) for
  `matcher.ts`'s wildcard/tolerance logic against hand-picked real case data (exact match, `<...>`
  wildcard match, TTL-drift tolerance, a deliberate mismatch that must fail).
- One integration-style test that runs the full Smoke suite end-to-end against a real built
  binary in a scratch directory and asserts all 12 cases pass — this is the walking-skeleton
  proof that `caseRunner`/`processTracker`/`matcher` work together against the real server, not
  just mocks.
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
