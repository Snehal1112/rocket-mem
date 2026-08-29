#!/usr/bin/env bash
# Bumps the workspace version, creates a signed release tag, and pushes both.
# Usage: ./scripts/release.sh [major|minor|patch|<version>]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

usage() {
    echo "Usage: $0 [major|minor|patch|<version>]"
    echo ""
    echo "  major     v1.2.3 -> v2.0.0"
    echo "  minor     v1.2.3 -> v1.3.0"
    echo "  patch     v1.2.3 -> v1.2.4"
    echo "  <version> explicit target, e.g. v4.0.0 or 4.0.0 -- use this when the"
    echo "            next release isn't a relative bump from the latest tag"
    exit 1
}

# Working tree must be clean before we tag -- a signed release tag should
# capture exactly what's on origin, not local uncommitted state. (The version
# bump below is the one commit we make on top of it, deliberately.)
if [[ -n "$(git status --porcelain)" ]]; then
    echo -e "${RED}Working tree has uncommitted changes. Commit or stash before releasing.${NC}"
    git status --short
    exit 1
fi

# CI gate: mirrors .github/workflows/ci.yml exactly, so a release never tags
# a commit that would fail CI. Runs before anything else since it's the
# slowest step and shouldn't run at all if the tree isn't even clean.
echo -e "${BLUE}Running CI gate (fmt, clippy, test)...${NC}"
if ! cargo fmt --all -- --check; then
    echo -e "${RED}cargo fmt found unformatted code. Run 'cargo fmt --all' and commit.${NC}"
    exit 1
fi
if ! cargo clippy --workspace -- -D warnings; then
    echo -e "${RED}cargo clippy found warnings. Fix them before releasing.${NC}"
    exit 1
fi
if ! cargo test --workspace; then
    echo -e "${RED}Tests failed. Fix them before releasing.${NC}"
    exit 1
fi
echo -e "${GREEN}CI gate passed.${NC}"
echo ""

# Resolve bump type from argument or prompt.
bump="${1:-}"
if [[ -z "$bump" ]]; then
    echo -e "${BLUE}Bump type:${NC} major / minor / patch, or an explicit version (e.g. v4.0.0)"
    read -rp "> " bump
fi

# The source of truth for "current version" is Cargo.toml, not the latest git
# tag -- unlike a bare tag-versioned project, this workspace's version.workspace
# field is what `cargo pkg version`/INFO actually report, and it can drift
# ahead of tags (e.g. it already sits at 0.1.0 with no tags cut yet). Basing
# the bump on tag history alone would silently go backwards in that case.
current_version=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
latest="v${current_version}"

if [[ "$bump" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    # Explicit target version. Needed because the next release isn't always
    # a relative bump from the current version.
    new_tag="v${bump#v}"
else
    case "$bump" in
        major|minor|patch) ;;
        *) echo -e "${RED}Invalid bump type or version: ${bump}${NC}"; usage ;;
    esac

    # Strip leading 'v' and split.
    major="${current_version%%.*}"; rest="${current_version#*.}"
    minor="${rest%%.*}"; patch="${rest#*.}"

    # Bump.
    case "$bump" in
        major) major=$((major + 1)); minor=0; patch=0 ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        patch) patch=$((patch + 1)) ;;
    esac

    new_tag="v${major}.${minor}.${patch}"
fi

if git tag --list "$new_tag" | grep -qx "$new_tag"; then
    echo -e "${RED}Tag ${new_tag} already exists.${NC}"
    exit 1
fi

new_version="${new_tag#v}"

echo ""
echo -e "  Current tag : ${YELLOW}${latest}${NC}"
echo -e "  New tag     : ${GREEN}${new_tag}${NC}"
echo -e "  Cargo.toml  : ${YELLOW}$(grep -m1 '^version = ' Cargo.toml)${NC} -> ${GREEN}version = \"${new_version}\"${NC}"
echo ""
read -rp "Bump version, create and push signed tag ${new_tag}? [y/N] " confirm
if [[ "${confirm,,}" != "y" ]]; then
    echo "Aborted."
    exit 0
fi

# Verify GPG signing is configured before doing anything that would need to
# be unwound (version bump commit, tag).
signing_key=$(git config --get user.signingkey 2>/dev/null || true)
if [[ -z "$signing_key" ]]; then
    echo -e "${RED}No user.signingkey configured. Run:${NC}"
    echo "  git config user.signingkey <KEY_ID>"
    exit 1
fi

# Bump [workspace.package] version in the root Cargo.toml. Scoped to that
# section (not a blind sed on every "version = " line) so this stays correct
# if a [package] section is ever added to the root manifest.
awk -v new="$new_version" '
    /^\[workspace\.package\]/ { in_section = 1 }
    /^\[/ && !/^\[workspace\.package\]/ { in_section = 0 }
    in_section && /^version = / { print "version = \"" new "\""; next }
    { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

# Refresh Cargo.lock's recorded versions for the workspace members (all of
# which inherit version.workspace = true) to match.
cargo check --workspace --quiet

git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to ${new_tag}"

# Create signed tag on the version-bump commit.
git tag -s "$new_tag" -m "Release ${new_tag}"

# Verify before pushing.
if ! git tag -v "$new_tag" 2>&1 | grep -q "Good signature"; then
    echo -e "${RED}Tag signature verification failed. Not pushing.${NC}"
    git tag -d "$new_tag"
    echo -e "${YELLOW}Version bump commit is still local -- 'git reset --hard HEAD~1' to undo it.${NC}"
    exit 1
fi

branch="$(git symbolic-ref --short HEAD)"
git push origin "$branch" "$new_tag"

echo ""
echo -e "${GREEN}Bumped, tagged, and pushed: ${new_tag}${NC}"
echo "GitHub Actions will build release binaries and open a draft release shortly."
echo "Monitor: https://github.com/$(git remote get-url origin | sed 's/.*github.com[:/]\(.*\)\.git/\1/')/actions"
