# Contributing to rocket-mem

rocket-mem is developed sprint-by-sprint against a fixed roadmap — see [`docs/rocket-mem-production-plan.md`](docs/rocket-mem-production-plan.md) (16-week phase plan) and [`docs/rocket-mem-sprint-plan.md`](docs/rocket-mem-sprint-plan.md) (2-week sprint breakdown). Read the current sprint's spec under `docs/superpowers/specs/` before picking up work — it fixes the design decisions (wire formats, scope cuts) every plan in that sprint assumes as ground truth.

## Before you start

- Check the current sprint's plan files under `docs/superpowers/plans/<date>-sprint-N-plans/` for the backlog item you're picking up. Each is a numbered, TDD-oriented implementation plan (`01-*.md`, `02-*.md`, ...) referencing that sprint's spec.
- If what you want to do isn't covered by an existing plan, open an issue to discuss scope first — this project follows an explicit, honest-about-scope roadmap (see the production plan's "Scope honesty" note), and features land in the sprint they're planned for rather than opportunistically.

## Development workflow

1. **Write the test first.** Every command and every code path (success, `WRONGTYPE`, missing-key) gets a test before or alongside the implementation. `commands/wrongtype_matrix_tests.rs` and `commands/missing_key_semantics_tests.rs` are the pattern to follow for new commands.
2. **Build and test as you go:**
   ```bash
   cargo build --workspace
   cargo test --workspace
   ```
3. **Format and lint before committing — CI enforces both, with zero tolerance for warnings:**
   ```bash
   cargo fmt --all
   cargo clippy --workspace -- -D warnings
   ```
   All three (`fmt --check`, `clippy -D warnings`, `test`) must be clean locally before you open a PR; CI runs exactly these on every push.

## Code conventions

- **Correctness rules that apply to every command** (see `CLAUDE.md` for the full list):
  - A type mismatch returns `Err(EngineError::WrongType)` — never silently coerce or ignore it.
  - A read on a missing key returns `None`/empty, not an error. A mutation that finds nothing to act on must not write back a phantom empty collection (this has caused real bugs before — see `missing_key_semantics_tests.rs`).
  - Don't implement functionality ahead of the sprint that needs it (e.g. `SET`'s `EX`/`PX` flags are deferred until the expiry reaper exists in Sprint 4) — half-wired features are harder to review and test than an explicit "not yet."
- **Comments** explain *why*, not *what* — skip comments that just restate what well-named code already says. Only add one for a non-obvious constraint, invariant, or workaround.
- **New data types** go in `crates/engine/src/value.rs`; **new commands** are one free function per command under `crates/engine/src/commands/<type>.rs`, signature `fn(&Engine, ...) -> Result<T, common::EngineError>`.
- Keep the engine protocol-agnostic — it must not know about RESP, `Frame`, or sockets. Protocol concerns belong in `crates/protocol` and `crates/server`.

## Commit messages

Recent history follows a `type(scope): summary` convention (`feat`, `fix`, `test`, `chore`, `docs`), scoped to the crate touched, e.g.:

```
feat(protocol): add RespCodec::decode (RESP2 bytes -> Frame)
test(server): dispatch_wrongtype_is_mapped_to_a_resp_error_frame
docs: update sprint 2 status in README
```

Keep commits scoped to one logical change.

## Pull requests

- Ensure `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` all pass before opening a PR — CI will otherwise fail on the same checks.
- Update the command coverage table in `README.md` if your change adds or removes a command.
- Reference the plan file (`docs/superpowers/plans/.../NN-*.md`) your PR implements, if any.

## Releasing

`scripts/release.sh [major|minor|patch|<version>]` cuts a release: it bumps the workspace version, tags it, and pushes both. It's maintainer tooling, not something a contributor PR needs to touch.

Prerequisites:
- A clean working tree — commit or stash first.
- A GPG signing key configured: `git config user.signingkey <KEY_ID>`.

What it does, in order:
1. Runs the CI gate locally (`cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`) — aborts if any step fails, so a release never tags a commit CI would reject.
2. Resolves the new version from `Cargo.toml`'s current `[workspace.package] version` (not git tag history — the workspace version is the source of truth, and can be ahead of the latest tag) plus your `major`/`minor`/`patch` choice, or an explicit version like `v4.0.0` for a non-relative jump.
3. After you confirm, bumps `[workspace.package] version` in the root `Cargo.toml`, refreshes `Cargo.lock`, and commits both as `chore: bump version to vX.Y.Z`.
4. Creates a signed tag on that commit and verifies the signature before pushing.
5. Pushes the current branch and the tag to `origin`.

The pushed tag triggers [`.github/workflows/release.yml`](.github/workflows/release.yml), which:
1. Generates a changelog from conventional-commit history since the last tag, via [`git-cliff`](https://git-cliff.org/) (config: [`cliff.toml`](cliff.toml)).
2. Builds a release binary natively on Linux, macOS, and Windows runners (no cross-compilation), packaged as `.tar.gz` (Linux/macOS) or `.zip` (Windows) with a `.sha256` checksum alongside each archive.
3. Signs each archive with [minisign](https://jedisct1.github.io/minisign/) (see "Release artifact signing" below), producing a `.sig` file alongside it.
4. Opens a **draft** GitHub Release named after the tag, with the changelog as its body and every archive/checksum/signature attached — review and publish it manually.

### Release artifact signing

Two layers of trust cover a release: the git tag is GPG-signed (proves *who* cut it — see above), and each downloadable archive is minisign-signed (proves the archive matches what that person's CI actually built, independent of the sha256 checksum which only guards against transit corruption). This is the same mechanism (and the same underlying tool) as Tauri's updater signing — `cargo tauri signer` just wraps [minisign](https://jedisct1.github.io/minisign/) — but rocket-mem uses its **own dedicated keypair**, generated once and unrelated to any other project's signing key, so a compromise of one never implicates the other.

**One-time setup (maintainer only, done locally — not by CI):**

1. Generate a dedicated keypair. This prompts for a password interactively — pick one and store it in a password manager; there's no recovery if it's lost.
   ```bash
   cargo tauri signer generate -w ~/.minisign/rocket-mem.key
   ```
2. Register the two secrets the workflow reads, via the GitHub web UI: **Settings → Secrets and variables → Actions → New repository secret** on the `rocket-mem` repo.
   - `RELEASE_SIGNING_KEY` — paste the full contents of `~/.minisign/rocket-mem.key` (e.g. `cat ~/.minisign/rocket-mem.key` and copy its output, newlines included).
   - `RELEASE_SIGNING_KEY_PASSWORD` — the password you chose in step 1.

Then commit the printed public key (the `.pub` file's contents, safe to publish) somewhere durable in the repo — e.g. as `RELEASE_SIGNING_KEY.pub` at the root — so consumers have something to verify against.

**Verifying a downloaded release** (consumer side, needs the real `minisign` CLI — `brew install minisign` / `apt install minisign` — not `cargo tauri`, which only signs):

```bash
minisign -Vm rocket-mem-vX.Y.Z-linux-amd64.tar.gz -P <contents of RELEASE_SIGNING_KEY.pub>
```

## License

By contributing, you agree your contributions are licensed under the project's [MIT License](LICENSE).
