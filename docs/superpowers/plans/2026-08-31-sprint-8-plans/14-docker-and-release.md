# Docker & Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a multi-stage `Dockerfile` that `docker run`s successfully out of the box, plus a `ghcr.io` publish job added to the *existing* release workflow (not a new one) so a `v*.*.*` tag push builds and ships the image alongside the binaries it already builds.

**Architecture:** `Dockerfile` at the repo root — a `rust:1-bookworm` build stage producing the release binary, copied into a minimal `debian:bookworm-slim` runtime stage (same Debian base family in both stages, avoiding a glibc version mismatch), running as a non-root user. `.github/workflows/release.yml` gains one more job, gated on the same tag trigger the `build`/`release` jobs already use.

**Tech Stack:** Docker, `docker/build-push-action` (GitHub Action) — no new Rust dependency.

**Spec:** [`../../specs/2026-08-31-sprint-8-spec.md`](../../specs/2026-08-31-sprint-8-spec.md), "Decision: Docker image + release pipeline extension" section.

## Global Constraints

- No new external account/secret — `docker/login-action` authenticates to `ghcr.io` with the workflow's own `GITHUB_TOKEN`, matching the existing `release` job's GitHub-native signing setup.
- `ghcr.io` image names must be lowercase; `github.repository` is not guaranteed to be (this repo's owner segment is `Snehal1112`, mixed case) — the workflow must lowercase it before using it in a tag, not assume it's already safe.
- `EXPOSE`d ports match `Config`'s defaults (`6379`, `6380`, `9121`) from plan 01 — a reader should be able to map the Dockerfile's `EXPOSE` line onto the config reference without cross-checking anything else.

---

### Task 1: `Dockerfile`

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`

**Interfaces:**
- Consumes: nothing — builds `crates/server`'s `rocket-mem` binary from source.
- Produces: a Docker image that runs `rocket-mem` as its entrypoint.

- [ ] **Step 1: Write `.dockerignore`**

```
target/
.git/
.claude/
docs/chaos/
*.aof
*.snapshot
```

- [ ] **Step 2: Write `Dockerfile`**

```dockerfile
# syntax=docker/dockerfile:1

# --- Build stage ---
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin rocket-mem

# --- Runtime stage ---
# Same Debian base family as the builder (bookworm), so the glibc the binary was linked
# against matches what's actually present here.
FROM debian:bookworm-slim
RUN useradd --system --create-home --shell /usr/sbin/nologin rocket-mem
COPY --from=builder /build/target/release/rocket-mem /usr/local/bin/rocket-mem
USER rocket-mem
WORKDIR /home/rocket-mem

# RESP, RMP, and Prometheus metrics -- matching Config's defaults
# (see docs/config-reference.md).
EXPOSE 6379 6380 9121

# Binds to 0.0.0.0 inside the container by default -- the image's whole point is to be reached
# from outside its own network namespace, unlike the loopback-only defaults a bare `cargo run`
# uses on a host. An operator overriding ROCKET_MEM_ADDR etc. still works normally.
ENV ROCKET_MEM_ADDR=0.0.0.0:6379
ENV ROCKET_MEM_RMP_ADDR=0.0.0.0:6380
ENV ROCKET_MEM_METRICS_ADDR=0.0.0.0:9121

ENTRYPOINT ["/usr/local/bin/rocket-mem"]
```

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `Dockerfile` and `.dockerignore`.

---

### Task 2: Local build + `redis-cli` smoke test

**Files:**
- None (verification only).

**Interfaces:**
- Consumes: `Dockerfile` (Task 1).
- Produces: nothing new — confirms "just works" per the production plan's own DoD wording.

- [ ] **Step 1: Build and run the image**

```bash
docker build -t rocket-mem:local .
docker run -d --name rocket-mem-smoke -p 16379:6379 -p 16380:6380 -p 19121:9121 rocket-mem:local
sleep 2
docker logs rocket-mem-smoke
```
Expected: the container's logs show the same `Listening on 0.0.0.0:6379` / `RMP listening on 0.0.0.0:6380` / `Metrics on http://0.0.0.0:9121/metrics` lines the binary prints when run bare, with no crash.

- [ ] **Step 2: Round-trip against the running container**

```bash
redis-cli -p 16379 PING
redis-cli -p 16379 SET foo bar
redis-cli -p 16379 GET foo
curl -s http://127.0.0.1:19121/metrics | head -5
```
Expected: `PONG`, `OK`, `"bar"`, and real Prometheus text output. If any of these fail, fix the `Dockerfile` (most likely cause: a port not actually bound to `0.0.0.0`, or a missing `EXPOSE`/`-p` mismatch) and re-run this step from the top.

- [ ] **Step 3: Clean up and confirm non-root**

```bash
docker exec rocket-mem-smoke whoami   # expect: rocket-mem, not root
docker rm -f rocket-mem-smoke
```

No commit for this task — it's verification of Task 1's artifact, not new source.

---

### Task 3: Extend `release.yml` with a `ghcr.io` publish job

**Files:**
- Modify: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `Dockerfile` (Task 1).
- Produces: a new `docker` job in the existing release workflow, triggered by the same `v*.*.*` tag push the `build`/`release` jobs already respond to.

- [ ] **Step 1: Add the job**

In `.github/workflows/release.yml`, add a new top-level job (alongside `changelog`/`build`/`release`):

```yaml
  docker:
    name: Build & Push Docker Image
    needs: [changelog]
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write

    steps:
      - uses: actions/checkout@v4

      - name: Lowercase repository name
        id: repo
        run: echo "name=$(echo '${{ github.repository }}' | tr '[:upper:]' '[:lower:]')" >> "$GITHUB_OUTPUT"

      - uses: docker/setup-buildx-action@v3

      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - uses: docker/build-push-action@v6
        with:
          context: .
          push: true
          tags: |
            ghcr.io/${{ steps.repo.outputs.name }}:${{ github.ref_name }}
            ghcr.io/${{ steps.repo.outputs.name }}:latest
```

(`needs: [changelog]` only, not `[build, release]` — the Docker image builds from source independently of the cross-platform binary archives the `build` job produces, so it doesn't need to wait for them; it does wait for `changelog` purely to keep the workflow's job graph simple to read, not because it uses the changelog's output.)

- [ ] **Step 2: Validate the YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"` (or any available YAML linter)
Expected: no parse error.

This job cannot be fully exercised without pushing a real `v*.*.*` tag (out of scope for this plan to do unprompted — see plan 15's sprint-close task, which is where a real tag push happens under the user's explicit direction). Validating the YAML parses and the job's steps mirror the already-proven pattern in the same file (`docker/login-action` + `docker/build-push-action` against `GITHUB_TOKEN` is a standard, widely-used pattern) is this task's achievable verification.

- [ ] **Step 3: Commit**

Use the `1-git-commit` skill/command to commit `.github/workflows/release.yml`.
