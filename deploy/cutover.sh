#!/usr/bin/env bash
# rocket-mem cutover -- run ONLY after deploy/install.sh has completed, TLS material is in
# place, and you've reviewed /etc/rocket-mem/{primary,replica}.toml.
#
# This STOPS any hand-started dev rocket-mem processes and starts the new systemd-managed
# primary+replica in their place. This is the disruptive step.
#
# Usage: sudo bash deploy/cutover.sh
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "Run with sudo: sudo bash deploy/cutover.sh" >&2
  exit 1
fi

echo "==> current rocket-mem processes (verify this is still what you expect before continuing):"
pgrep -af rocket-mem || echo "    (none running)"
echo
read -rp "Stop these dev processes and start the production primary+replica? [y/N] " confirm
if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
  echo "Aborted, nothing changed."
  exit 0
fi

echo "==> stopping hand-started dev processes"
# Matches the dev cluster's config file naming convention (rocket-mem --config rocket-mem*.toml
# at the repo root) -- does NOT match the production units' absolute /etc/rocket-mem/... paths.
pkill -TERM -f 'rocket-mem --config rocket-mem' || true
sleep 1
pgrep -af rocket-mem || echo "    (all dev processes stopped)"

echo "==> starting production services"
systemctl start rocket-mem-primary
sleep 1
systemctl start rocket-mem-replica
sleep 1
systemctl status --no-pager rocket-mem-primary rocket-mem-replica

echo
echo "==> verify manually before trusting this:"
echo "    redis-cli -p 6379 -a <app-password> --no-auth-warning ping           # primary, loopback plaintext"
echo "    redis-cli -p 6389 -a <app-password> --no-auth-warning info replication | grep role"
echo "    redis-cli --tls --cacert /etc/rocket-mem/tls/root_ca.crt -p 16379 -a <app-password> --no-auth-warning ping"
echo
echo "    If the primary ever dies, this setup has no automatic failover -- see"
echo "    .claude/runbook-failover.md for the manual promotion procedure."
