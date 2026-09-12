#!/usr/bin/env bash
# rocket-mem production install -- safe to run any time.
#
# Builds and installs the primary+replica bundle described in deploy/README.md. Does NOT touch
# any currently-running rocket-mem process (dev cluster or otherwise) and does NOT start the new
# services -- run deploy/cutover.sh separately, when ready, to actually switch traffic over.
#
# Usage: sudo bash deploy/install.sh [cert-file] [key-file] [ca-file]
#   All three TLS arguments are optional -- see the "TLS material" step below.
set -euo pipefail

STAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$STAGE_DIR/.." && pwd)"

if [[ $EUID -ne 0 ]]; then
  echo "Run with sudo: sudo bash deploy/install.sh" >&2
  exit 1
fi

echo "==> building release binary"
( cd "$REPO_ROOT" && cargo build --release --bin rocket-mem )

echo "==> installing release binary"
install -o root -g root -m 755 "$REPO_ROOT/target/release/rocket-mem" \
  /usr/local/bin/rocket-mem

echo "==> creating service user"
if ! id rocket-mem &>/dev/null; then
  useradd --system --no-create-home --shell /usr/sbin/nologin rocket-mem
else
  echo "    rocket-mem user already exists, skipping"
fi

echo "==> creating directory layout"
mkdir -p /etc/rocket-mem/tls /var/lib/rocket-mem/primary /var/lib/rocket-mem/replica
chown -R rocket-mem:rocket-mem /var/lib/rocket-mem
chown -R root:rocket-mem /etc/rocket-mem
chmod 750 /etc/rocket-mem /var/lib/rocket-mem/primary /var/lib/rocket-mem/replica

echo "==> generating ACL passwords"
APP_PASSWORD="$(openssl rand -base64 32)"
REPLICA_PASSWORD="$(openssl rand -base64 32)"

echo "==> installing config (contains plaintext ACL/replication passwords once substituted -- mode 640)"
TMP_PRIMARY="$(mktemp)"
TMP_REPLICA="$(mktemp)"
trap 'rm -f "$TMP_PRIMARY" "$TMP_REPLICA"' EXIT

# '|' as the sed delimiter, since base64 output can contain '/'.
sed -e "s|__APP_PASSWORD__|$APP_PASSWORD|g" -e "s|__REPLICA_PASSWORD__|$REPLICA_PASSWORD|g" \
  "$STAGE_DIR/primary.toml.example" > "$TMP_PRIMARY"
sed -e "s|__APP_PASSWORD__|$APP_PASSWORD|g" -e "s|__REPLICA_PASSWORD__|$REPLICA_PASSWORD|g" \
  "$STAGE_DIR/replica.toml.example" > "$TMP_REPLICA"

install -o root -g rocket-mem -m 640 "$TMP_PRIMARY" /etc/rocket-mem/primary.toml
install -o root -g rocket-mem -m 640 "$TMP_REPLICA" /etc/rocket-mem/replica.toml

echo "==> TLS material"
CERT_FILE="${1:-${ROCKET_MEM_TLS_CERT:-}}"
KEY_FILE="${2:-${ROCKET_MEM_TLS_KEY:-}}"
CA_FILE="${3:-${ROCKET_MEM_TLS_CA:-}}"

if [[ -n "$CERT_FILE" && -n "$KEY_FILE" && -n "$CA_FILE" ]]; then
  install -o rocket-mem -g rocket-mem -m 644 "$CERT_FILE" /etc/rocket-mem/tls/server.crt
  install -o rocket-mem -g rocket-mem -m 600 "$KEY_FILE" /etc/rocket-mem/tls/server.key
  install -o rocket-mem -g rocket-mem -m 644 "$CA_FILE" /etc/rocket-mem/tls/root_ca.crt
  echo "    installed cert/key/CA from arguments"
else
  echo "    NOT installing TLS material automatically -- no cert/key/CA given."
  echo "    Place your own files at:"
  echo "      /etc/rocket-mem/tls/server.crt   (mode 644, owner rocket-mem:rocket-mem)"
  echo "      /etc/rocket-mem/tls/server.key   (mode 600, owner rocket-mem:rocket-mem)"
  echo "      /etc/rocket-mem/tls/root_ca.crt  (mode 644, owner rocket-mem:rocket-mem)"
  echo "    before starting either service -- see deploy/README.md's TLS note. This script"
  echo "    deliberately does not generate a throwaway self-signed cert for you."
fi

echo "==> installing systemd units"
install -o root -g root -m 644 "$STAGE_DIR/rocket-mem-primary.service" \
  /etc/systemd/system/rocket-mem-primary.service
install -o root -g root -m 644 "$STAGE_DIR/rocket-mem-replica.service" \
  /etc/systemd/system/rocket-mem-replica.service
systemctl daemon-reload
systemctl enable rocket-mem-primary rocket-mem-replica

echo "==> firewall: opening only the primary's TLS RESP port (16379)"
echo "    RMP (17379) and both replica ports stay loopback-only / unopened -- see deploy/README.md"
if command -v ufw &>/dev/null; then
  ufw allow 16379/tcp comment 'rocket-mem TLS RESP'
  ufw status verbose
else
  echo "    ufw not installed -- add an equivalent rule with your firewall of choice"
fi

echo
echo "==> install complete. Services are enabled but NOT started yet."
echo "    Review /etc/rocket-mem/*.toml, install TLS material if you haven't, then run"
echo "    deploy/cutover.sh when ready to go live."
echo
echo "==> GENERATED PASSWORDS -- save these to a password manager now, they are not written"
echo "    to any file on disk by this script:"
echo "      app     : $APP_PASSWORD"
echo "      replica : $REPLICA_PASSWORD"
