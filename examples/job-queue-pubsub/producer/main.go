// Command producer enqueues jobs against rocket-mem and wakes up idle workers.
//
// The queue itself lives in a Redis LIST (jobs:queue), not in pub/sub: LPUSH is
// durable, so a job survives even if no worker is currently running. PUBLISH is
// used only as a "something's here" doorbell on jobs:notify -- a worker that
// isn't subscribed at the moment of a PUBLISH will never see that message, since
// pub/sub carries no history. Combining a durable list with a pub/sub wake-up is
// a common real-world pattern: it avoids workers busy-polling the list while
// still reacting to new work immediately instead of on the next poll tick.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"time"

	"github.com/redis/go-redis/v9"
)

type Job struct {
	ID        int       `json:"id"`
	Task      string    `json:"task"`
	CreatedAt time.Time `json:"created_at"`
}

const (
	queueKey   = "jobs:queue"
	notifyChan = "jobs:notify"
)

func main() {
	addr := flag.String("addr", "127.0.0.1:6379", "rocket-mem address")
	count := flag.Int("count", 5, "number of jobs to enqueue")
	interval := flag.Duration("interval", time.Second, "delay between jobs")
	flag.Parse()

	rdb := redis.NewClient(&redis.Options{
		Addr: *addr,
		// rocket-mem's RESP3 support doesn't cover every handshake extra a
		// RESP3-negotiating client library sends by default; RESP2 is fully
		// supported and is all this example needs.
		Protocol: 2,
	})
	defer rdb.Close()
	ctx := context.Background()

	if err := rdb.Ping(ctx).Err(); err != nil {
		log.Fatalf("connect to rocket-mem at %s: %v", *addr, err)
	}

	for i := 1; i <= *count; i++ {
		job := Job{ID: i, Task: fmt.Sprintf("resize-image-%d", i), CreatedAt: time.Now()}
		payload, err := json.Marshal(job)
		if err != nil {
			log.Fatalf("marshal job: %v", err)
		}

		if err := rdb.LPush(ctx, queueKey, payload).Err(); err != nil {
			log.Fatalf("enqueue job %d: %v", job.ID, err)
		}

		delivered, err := rdb.Publish(ctx, notifyChan, "job").Result()
		if err != nil {
			log.Fatalf("notify for job %d: %v", job.ID, err)
		}

		fmt.Printf("enqueued job %d (%s) -- notified %d listening worker(s)\n", job.ID, job.Task, delivered)
		time.Sleep(*interval)
	}
}
