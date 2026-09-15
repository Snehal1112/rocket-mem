# rocket-mem Landing Page — Integration & Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out the landing page: a clean full verification pass, a `web/README.md` explaining what this folder is and how to run it, and a manual QA pass across themes and viewport widths.

**Architecture:** No new landing sections — by the end of plan 7, `App.tsx` already renders every section the spec calls for, in order. This plan only verifies and documents. Task 1's steps are written as "run, and if it fails, fix and re-run" rather than assuming zero issues, since seven prior plans of hand-written TSX may have accumulated small typecheck/lint issues that only surface once every file exists together.

**Tech Stack:** Same as all prior plans — no new dependencies.

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** This is the final plan in the chain — the landing page is complete after this plan's verification passes. No further plan follows.

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first, shadcn `base-luma` style on `@base-ui/react`.
- Package manager: **Bun only**.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated — do not touch `.github/workflows/ci.yml`, and do not add a test runner.
- "Done," per the spec's Verification section, means `bun run typecheck`, `bun run lint`, and `bun run build` all clean, plus a manual visual check across light/dark/system themes and mobile width (~400px).
- Dev server binds `127.0.0.1:5174`.
- **Working directory:** `git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Full verification pass

**Files:**
- Modify: any `web/src/**/*.tsx` file, only as needed to fix a typecheck or lint failure surfaced by this task (no new files expected).

**Interfaces:**
- Consumes: every file created across plans 1–7.
- Produces: a `web/` tree that passes `bun run typecheck`, `bun run lint`, and `bun run build` cleanly — the baseline Task 3's manual QA runs against.

**Suggested model:** sonnet

- [ ] **Step 1: Run typecheck**

Run: `bun run typecheck` from `web/`.
If it fails: read the error, fix the offending file (common causes at this scale: a missed import after copy-pasting a section's `App.tsx` snippet, a stale prop name), and re-run until clean.

- [ ] **Step 2: Run lint**

Run: `bun run lint` from `web/`.
If it fails: read the error. If it's a real issue in a hand-written `landing/*.tsx` file, fix it. If it fires on a vendored `ui/*.tsx` or `hooks/use-mobile.ts` file, that means the `src/components/ui/**` / `src/hooks/use-mobile.ts` override block is missing or wrong in `biome.json` (copied verbatim in plan 1) — diff it against `../../rocketvault/web/biome.json` rather than editing the vendored component to satisfy the rule. Re-run until clean.

- [ ] **Step 3: Run the build**

Run: `bun run build` from `web/`.
If it fails: read the error (this catches anything `typecheck`'s `--noEmit` mode or `lint` wouldn't, such as an actual Vite/Rollup bundling issue) and fix it. Re-run until clean. Expected on success: a `web/dist/` directory containing the built static site.

- [ ] **Step 4: Commit any fixes**

If Steps 1–3 required no changes, skip this step entirely — there's nothing to commit. Otherwise:

```bash
git add web/
git commit -m "$(cat <<'EOF'
Fix typecheck/lint/build issues found in web/'s full verification

Issues surfaced only once every section from plans 1-7 existed
together; see the diff for what changed.
EOF
)"
```

---

### Task 2: Write `web/README.md`

**Files:**
- Create: `web/README.md`

**Interfaces:**
- Consumes: nothing (documentation only).
- Produces: the standard entry point for anyone opening `web/` for the first time.

**Suggested model:** haiku

- [ ] **Step 1: Create `web/README.md`**

```markdown
# rocket-mem landing page

A standalone marketing/showcase landing page for
[rocket-mem](https://github.com/Snehal1112/rocket-mem), built with React 19,
Vite, Tailwind CSS v4, and a vendored subset of
[rocketvault/web](../../rocketvault/web)'s `base-luma` shadcn/ui components.

This folder is **not** part of the Cargo workspace and is **not** CI-gated —
the same relationship `examples/` and `rocket-mem-mcp/` have to the rest of
the repository. See the root [`CLAUDE.md`](../CLAUDE.md) for that convention.

## Running it

```bash
bun install
bun run dev       # dev server at http://127.0.0.1:5174
```

## Building it

```bash
bun run build      # tsc -b && vite build, output in dist/
bun run preview      # serve the production build locally
```

## Other scripts

```bash
bun run typecheck   # tsc -b --noEmit
bun run lint         # biome lint --error-on-warnings
bun run lint:fix      # biome lint --write --error-on-warnings
bun run format         # prettier --write "**/*.{ts,tsx}"
```

## Design doc

The full design spec this page implements lives at
[`../docs/superpowers/specs/2026-09-15-landing-page-design.md`](../docs/superpowers/specs/2026-09-15-landing-page-design.md).
```

- [ ] **Step 2: Verify the file reads correctly**

Open `web/README.md` and confirm every relative link (`../../rocketvault/web`, `../CLAUDE.md`, `../docs/superpowers/specs/2026-09-15-landing-page-design.md`) actually resolves to an existing path from `web/`'s location.

- [ ] **Step 3: Commit**

```bash
git add web/README.md
git commit -m "$(cat <<'EOF'
Add web/README.md

What this folder is, how to run and build it, and where its
design spec lives — the standard entry point for anyone opening
web/ for the first time.
EOF
)"
```

---

### Task 3: Manual QA pass

**Files:** none (verification only — no files are expected to change unless this step uncovers a bug, in which case fix it in the relevant `landing/*.tsx` file and note it in the commit).

**Interfaces:**
- Consumes: the fully assembled page from Task 1.
- Produces: confirmation the page meets the spec's Verification section in a real browser, and a final repository sanity check that everything from all 8 plans is committed.

**Suggested model:** sonnet

- [ ] **Step 1: Start the dev server**

Run: `bun run dev` from `web/`, open `http://127.0.0.1:5174`.

- [ ] **Step 2: Light mode full-page scroll**

If the page isn't already in light mode, use the nav's theme toggle to set it. Scroll from the top (hero) to the bottom (footer). Confirm every section renders without visual glitches: Hero, Terminal demo, Why, Features, Architecture, Performance, Security & observability, Command coverage, Quickstart, footer — in that order, no missing or duplicated sections, no unstyled flash-of-default-browser-font text anywhere.

- [ ] **Step 3: Dark mode full-page scroll**

Toggle to dark mode. Repeat the same full-page scroll. Confirm text stays legible against its background in every section (particularly the terminal demo card, which stays internally dark regardless of page theme — check it doesn't lose contrast against a dark page background around it) and the performance chart's two bar colors are still visually distinct from each other.

- [ ] **Step 4: System mode**

Toggle to system mode. Confirm the page matches your OS/browser's current light/dark preference (toggle your OS setting if you want to confirm it reacts live — the vendored `theme-provider.tsx` listens for `prefers-color-scheme` changes while in system mode).

- [ ] **Step 5: Mobile width**

Resize the browser (or use devtools' responsive mode) to ~400px wide. Confirm:
- The nav's inline links and GitHub button are replaced by the hamburger menu; the sheet drawer opens/closes and its links work.
- No section causes horizontal page scroll (check the performance chart and the quickstart code blocks specifically — both have historically been the most likely to overflow on narrow viewports).
- The features and security/observability card grids collapse to a single column.

- [ ] **Step 6: Interactive elements**

Click every one of `SiteNav`'s six anchor links (Why, Features, Architecture, Performance, Security, Quick Start) and confirm each scrolls to its matching section. The terminal demo itself is static (no copy button, per the spec's design decision) — instead, click the copy button in the hero's Docker command block, and each of the three quickstart tabs' code blocks, confirming the checkmark-swap animation and that the clipboard content is correct each time (paste into an address bar or text field to check).

- [ ] **Step 7: Stop the dev server, then a final repository sanity check**

Stop `bun run dev` (Ctrl-C).

```bash
git status
git log --oneline -30
```

Confirm `git status` is clean (no uncommitted changes) and `git log` shows a commit for every task across all 8 plans, in order, ending with this plan's own commits.

- [ ] **Step 8: Commit** (only if Steps 2–6 uncovered and required fixing a bug)

```bash
git add web/
git commit -m "$(cat <<'EOF'
Fix issue found during manual QA

<describe what was found and fixed>
EOF
)"
```

If no bug was found, skip this step — Step 7's `git status` should already be clean.

## Self-Review

- **Spec coverage:** this plan implements the spec's entire Verification section (`bun run typecheck` / `lint` / `build` all clean, plus manual light/dark/system/mobile checks) and the `web/README.md` half of the Folder layout section — the last two items the spec calls for that no earlier plan covered.
- **Placeholder scan:** none — the manual QA task lists concrete pass/fail criteria for every check rather than a vague "make sure it looks good."
- **Type consistency:** no new interfaces are produced by this plan; nothing downstream depends on it, since it's the last plan in the chain.

## Next plan

None — this is the final plan in the chain. The rocket-mem landing page is complete once this plan's verification passes.
