# rocket-mem production deployment bundle

This directory is the hardened, opinionated "go to production" bundle for a single-primary +
single-replica rocket-mem deployment (same box, not a cluster). It is distinct from the
general-purpose example configs at the repo root (`rocket-mem.toml.example`,
`rocket-mem-replica.toml.example`), which document every field the server understands, all
commented out, for you to pick and choose from. The files here are close-to-as-is: the fields
that matter for this specific topology are already active, and `install.sh` installs them with
minimal editing.

No real secrets are committed here or anywhere in this bundle -- `primary.toml.example` and
`replica.toml.example` use `__APP_PASSWORD__`/`__REPLICA_PASSWORD__` placeholders that
`install.sh` fills in with freshly generated values at install time.

## Topology

One primary + one replica, on the same box for now, not the 3-shard cluster this repo's dev
setup (`rocket-mem.toml`, `rocket-mem-shard-{a,b,c}*.toml`) runs elsewhere.

| Node | Plaintext RESP | Plaintext RMP | Metrics | TLS RESP | TLS RMP |
|---|---|---|---|---|---|
| primary | `127.0.0.1:6379` | `127.0.0.1:6380` | `127.0.0.1:9121` | `0.0.0.0:16379` (**the only externally-facing listener**) | `127.0.0.1:17379` |
| replica | `127.0.0.1:6389` | `127.0.0.1:6390` | `127.0.0.1:9122` | `127.0.0.1:16389` | (none) |

The primary's plaintext listeners are mandatory (rocket-mem always binds `addr`/`rmp_addr`, they
cannot be disabled) but stay loopback-only and unreachable externally. The replica's
`replicaof` points at the primary's **TLS** port (`127.0.0.1:16379`), not its plaintext one, so
the replication link itself runs over TLS -- see `.claude/manual-testing.md`'s
"`replica_announce_addr`: what a TLS follower tells its leader" section for the pattern this
follows.

ACL: the primary defines two users -- `app` (`allcommands`, `allkeys`, for real client traffic)
and `replica` (`+psync`, `+replconf`, `allkeys`, used only for the replication link, so a leaked
replication credential can't be used as a general client). The replica only needs its own `app`
user (for its own client connections) plus `replicaof_auth_username`/`replicaof_auth_password`
set to the primary's `replica` account.

Filesystem layout: binary at `/usr/local/bin/rocket-mem`; configs at
`/etc/rocket-mem/{primary,replica}.toml` (root-owned, group `rocket-mem`, mode 640 -- they hold
plaintext passwords once installed); TLS material at `/etc/rocket-mem/tls/`; data at
`/var/lib/rocket-mem/{primary,replica}/`; a dedicated `rocket-mem` system user (no login shell,
no home dir) owns the data dirs and TLS material.

## Workflow

```bash
sudo bash deploy/install.sh      # build, install binary/configs/units, generate passwords
                                  # (does NOT touch any running process, does NOT start services)
# ... place TLS material, review /etc/rocket-mem/*.toml, save the printed passwords ...
sudo bash deploy/cutover.sh      # the disruptive step: stops dev processes, starts the new services
```

`install.sh` prints the two generated ACL passwords once, at the end, with a reminder to save
them to a password manager -- it does not write them to any file on disk itself.

## What's NOT handled

- **A real TLS certificate.** `install.sh` does not generate one -- if you don't pass cert/key/CA
  files (as arguments or via `ROCKET_MEM_TLS_CERT`/`_TLS_KEY`/`_TLS_CA` env vars), it prints
  instructions to place `server.crt`/`server.key`/`root_ca.crt` under `/etc/rocket-mem/tls/`
  yourself before starting either service. Getting a real cert needs a domain pointed at this
  box; see `docs/getting-started.md`'s "Enabling TLS" section and `docs/config-reference.md`'s
  TLS notes for the underlying mechanics this bundle assumes.
- **Migrating existing data.** Both services start from an empty AOF/snapshot. If you need the
  current dev cluster's data carried over, copy the relevant AOF/snapshot files into
  `/var/lib/rocket-mem/{primary,replica}/` before first start -- this bundle doesn't automate
  that.
- **Enabling `min_replicas_to_write`.** Both configs ship with it at `0` (writes never blocked by
  a slow/dead replica). Raising it to `1` makes the primary refuse writes once the replica falls
  more than `min_replicas_max_lag_secs` behind -- worth revisiting once you've observed real
  replication lag under load, per `docs/config-reference.md`'s "Replica fencing" note.

## If the primary dies

This setup has no automatic failover, no quorum, and no live cluster-topology reload -- promoting
the replica is a manual, documented procedure. See `.claude/runbook-failover.md`.
