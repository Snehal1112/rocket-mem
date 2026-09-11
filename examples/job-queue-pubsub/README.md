# Job queue + pub/sub notification (Go)

A real-world pattern for rocket-mem's pub/sub feature: a durable job queue (a
Redis `LIST`) combined with pub/sub as a wake-up signal, so worker processes
react to new work immediately instead of polling the list in a loop.

**Why not pub/sub alone?** `PUBLISH` never persists anything — a message sent
while no one is subscribed is gone forever, and there's no acknowledgment or
redelivery. That's fine for a notification ("something changed"), but wrong
for the job data itself, which must survive a worker being briefly offline.
This example keeps the actual job payloads in `jobs:queue` (a list, durable)
and uses `jobs:notify` (pub/sub) purely as the doorbell.

## Files

- `producer/main.go` — enqueues jobs (`LPUSH jobs:queue`) and notifies
  (`PUBLISH jobs:notify`).
- `worker/main.go` — subscribes to `jobs:notify`, drains `jobs:queue`
  (`RPOP`) on every notification, and also drains once at startup to pick up
  any backlog that was queued before it connected.

## Run it

Point both at any running rocket-mem instance (adjust `-addr` to match your
config's `addr`; defaults to `127.0.0.1:6379`).

```bash
go mod tidy   # first time only, fetches go-redis

# Terminal 1 & 2 — start a couple of workers
go run ./worker -addr 127.0.0.1:6379 -name worker-A
go run ./worker -addr 127.0.0.1:6379 -name worker-B

# Terminal 3 — enqueue some jobs
go run ./producer -addr 127.0.0.1:6379 -count 6 -interval 400ms
```

Watch the two workers split the jobs between them — each job is picked up by
exactly one worker, since `RPOP` is atomic, no matter how many workers were
woken by the same `PUBLISH`.

**Try this too:** stop both workers, run the producer again (jobs pile up in
the list with nobody listening), then start a fresh worker — it drains the
whole backlog on startup, proving the list carries jobs across gaps that
pub/sub itself cannot.

## What this exercises in rocket-mem specifically

- `PUBLISH`'s reply (`delivered_count`) — the producer logs how many workers
  were listening at the moment of each publish.
- Real concurrent subscribers on one channel, each with its own independent
  connection and message stream.
- `LPUSH`/`RPOP` for the durable side — ordinary commands, included here to
  show why pub/sub is usually paired with real storage rather than used as
  the only mechanism.

Any standard Redis client library works the same way against rocket-mem —
this example happens to use Go's `go-redis`, but `redis-py`, `ioredis`, or
the Rust `redis` crate would look nearly identical.
