# CI Skeleton Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every push and PR runs `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` automatically.

**Architecture:** a single GitHub Actions workflow, no matrix build needed yet (single OS/toolchain is fine for this sprint — a build matrix is worth adding once the release workflow shows up in Sprint 8/Week 16).

**Depends on:** `01-workspace-scaffold-and-value-enum.md` must be complete (needs a workspace to run against).

---

### Task 1: GitHub Actions workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

```yaml
# .github/workflows/ci.yml
name: CI
on:
  push:
  pull_request:

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - name: Format check
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace -- -D warnings
      - name: Test
        run: cargo test --workspace
```

- [ ] **Step 2: Verify locally before pushing**

Run: `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: all three pass locally — if `fmt --check` fails, run `cargo fmt --all` first and commit the formatting fix separately

- [ ] **Step 3: Commit and push, then verify the Actions run**

```bash
git add .github/workflows/ci.yml
git commit -m "chore: add CI workflow (fmt, clippy, test)"
git push
```

Expected: the workflow appears under the repo's Actions tab and passes

---

### Task 2: CI status badge

**Files:**
- Modify: `README.md` (create it first if it doesn't exist yet)

- [ ] **Step 1: Add the badge**

```markdown
![CI](https://github.com/<your-username>/rocket-mem/actions/workflows/ci.yml/badge.svg)
```

- [ ] **Step 2: Verify it renders**

Push and check the README on GitHub — the badge should show a green "passing" once the Task 1 workflow has run at least once on the default branch.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add CI status badge"
```
