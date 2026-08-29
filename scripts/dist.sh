#!/usr/bin/env bash
# Packages the release binary into dist/, mirroring the packaging steps in
# .github/workflows/release.yml (archive naming, tar.gz/zip choice, sha256
# checksum) so a local dist/ matches what CI would produce for this host's
# platform. Does not sign artifacts -- minisign signing needs
# RELEASE_SIGNING_KEY, which only exists as a CI secret.
#
# Usage: ./scripts/build.sh --release && ./scripts/dist.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

# Same source of truth as scripts/release.sh: the workspace version, not tag
# history -- falls back to it when HEAD isn't an exact tag match.
version_from_cargo() {
    grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/v\1/'
}
VERSION="$(git describe --tags --exact-match 2>/dev/null || version_from_cargo)"

case "$(uname -s)" in
    Linux) os_name="linux"; archive="tar.gz" ;;
    Darwin) os_name="darwin"; archive="tar.gz" ;;
    MINGW*|MSYS*|CYGWIN*) os_name="windows"; archive="zip" ;;
    *)
        echo -e "${RED}Unsupported OS: $(uname -s)${NC}"
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64|amd64) arch="amd64" ;;
    arm64|aarch64) arch="arm64" ;;
    *)
        echo -e "${RED}Unsupported architecture: $(uname -m)${NC}"
        exit 1
        ;;
esac

ext=""
[ "$os_name" = "windows" ] && ext=".exe"

binary_src="target/release/rocket-mem${ext}"
if [ ! -f "$binary_src" ]; then
    echo -e "${RED}${binary_src} not found. Build it first:${NC}"
    echo "  ./scripts/build.sh --release"
    exit 1
fi

BINARY="rocket-mem-${VERSION}-${os_name}-${arch}${ext}"
mkdir -p dist
cp "$binary_src" "dist/${BINARY}"

echo -e "${BLUE}Packaging ${BINARY}...${NC}"
if [ "$archive" = "tar.gz" ]; then
    ARCHIVE="rocket-mem-${VERSION}-${os_name}-${arch}.tar.gz"
    tar -czf "dist/${ARCHIVE}" -C dist "${BINARY}"
else
    ARCHIVE="rocket-mem-${VERSION}-${os_name}-${arch}.zip"
    (cd dist && zip -q "${ARCHIVE}" "${BINARY}")
fi

# Hash from inside dist so the recorded filename has no path prefix, matching
# release.yml -- lets consumers verify against a bare downloaded archive.
(
    cd dist
    command -v sha256sum >/dev/null 2>&1 \
        && sha256sum "${ARCHIVE}" > "${ARCHIVE}.sha256" \
        || shasum -a 256 "${ARCHIVE}" > "${ARCHIVE}.sha256"
)

echo -e "${GREEN}Done.${NC}"
echo "  dist/${ARCHIVE}"
echo "  dist/${ARCHIVE}.sha256"
