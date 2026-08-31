# Sprint 8 Close Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** version bump to `1.0.0`, full-workspace verification, the sprint plan's Sprint 8 status/DoD ticked, and a local (unpushed) `v1.0.0` tag — the last plan of the sprint, depending on every other Sprint 8 plan being merged first.

**Architecture:** no production code changes — this plan is entirely bookkeeping and verification.

**Tech Stack:** none.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), Definition of Done section.

## Global Constraints

- This plan runs **last**, only after plans 1–14 are merged — its own verification step (Task 1) is only meaningful against the fully-assembled sprint.
- **Tagging is local only.** `git tag -a v1.0.0` is created and left unpushed by this plan (Task 3) — it is easily reversible (`git tag -d v1.0.0`) unlike a push, which would trigger the real `release.yml` workflow (cross-platform binaries, a public GitHub release, and — after plan 14 — a `ghcr.io` image push). Pushing the tag is a separate, explicit action the person running this plan takes after reviewing the tag, not something this plan does on its own.

---

### Task 1: Version bump + full-workspace verification

**Files:**
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Consumes: nothing.
- Produces: `workspace.package.version = "1.0.0"`, propagating to every crate that uses `version.workspace = true` (all five).

- [ ] **Step 1: Bump the version**

In `Cargo.toml`'s `[workspace.package]`, change:

```toml
version = "0.1.2"
```

to:

```toml
version = "1.0.0"
```

- [ ] **Step 2: Full-workspace verification**

Run, in order, stopping and fixing anything that fails before proceeding to the next command:

```bash
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Expected: all four green. The release-profile build (last command) is included deliberately — every other plan in this sprint verified with `cargo build`/`cargo test` (dev profile); this is the first point the *release* profile (what `Dockerfile` and `release.yml` actually ship) is confirmed to build clean, catching anything that only breaks under release-mode optimization or `#[cfg(not(debug_assertions))]`-gated code, if any exists.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `Cargo.toml`.

---

### Task 2: Tick Sprint 8's status and Definition of Done

**Files:**
- Modify: `docs/rocket-mem-sprint-plan.md`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing new — a documentation-only status update, matching the exact pattern every prior sprint (1–7) already used in this file.

- [ ] **Step 1: Add the Status line**

In `docs/rocket-mem-sprint-plan.md`'s `## Sprint 8 — Auth, ACLs, TLS & release` section, immediately after the `**Maps to:** Weeks 15-16 | **Dates:** Day 99–112` line, add a Status line matching the exact shape every prior sprint uses (see Sprint 7's: `**Status:** ✅ Complete — full P0 scope shipped, plus the P1 client library. See ...`):

```markdown
**Status:** ✅ Complete — full P0/P1 scope shipped, plus the P2 Docker/release workflow. See
`docs/superpowers/specs/2026-08-31-sprint-8-spec.md` and
`docs/superpowers/plans/2026-08-31-sprint-8-plans/`.
```

(Adjust the "full P0/P1 scope... plus P2" wording if any item was actually cut or descoped during implementation — this line must honestly reflect what shipped, matching this file's own convention of noting scope changes rather than papering over them, e.g. Sprint 6's carryover notes.)

- [ ] **Step 2: Tick the Definition of Done checkboxes**

In the same section's `### Definition of done` list, change every `- [ ]` to `- [x]`:

```markdown
### Definition of done
- [x] Overnight chaos test log shows zero corruption incidents
- [x] ACL and TLS test suites pass
- [x] README, config reference, and command-compatibility matrix are complete
- [x] `v1.0.0` tagged; Docker image builds and runs via `docker run`
```

(The last line's "tagged" refers to Task 3 of this plan, which creates the local tag — tick it once that task is actually done, not before, per this project's own established convention of DoD checkboxes reflecting real completed work.)

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `docs/rocket-mem-sprint-plan.md`.

---

### Task 3: Local `v1.0.0` tag

**Files:**
- None (a git tag is not a tracked file).

**Interfaces:**
- Consumes: the fully-verified, committed state from Tasks 1–2.
- Produces: a local, unpushed annotated tag `v1.0.0`.

- [ ] **Step 1: Confirm the working tree is clean and every prior commit landed**

Run: `git status --short && git log --oneline -20`
Expected: clean working tree; the log shows every Sprint 8 plan's commits (1 through 14) present, most recent first, ending with Task 2's sprint-plan.md commit from this plan.

- [ ] **Step 2: Create the local annotated tag**

```bash
git tag -a v1.0.0 -m "v1.0.0 — Sprint 8: auth/ACLs, TLS, config layering, chaos-tested durability, Docker & release"
```

- [ ] **Step 3: Verify it, and stop — do not push**

Run: `git show v1.0.0 --stat | head -20` to confirm the tag points at the expected commit.

**This plan ends here.** Pushing the tag (`git push origin v1.0.0`) is what triggers `release.yml` for real — cross-platform binary builds, a public GitHub release, and (per plan 14) a `ghcr.io` image push. That is a separate, externally-visible action requiring the repo owner's own explicit go-ahead at the time they're ready to actually ship `v1.0.0`, not something this plan does automatically as its last step.
