// Command worker subscribes to job notifications and drains the shared job
// queue. Run several of these at once (different -name values) against the
// same rocket-mem instance to see them race for work: each job is delivered to
// exactly one worker, since RPOP is atomic, no matter how many workers were
// notified by the same PUBLISH.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
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
	name := flag.String("name", "worker-1", "worker name, for logging")
	flag.Parse()

	rdb := redis.NewClient(&redis.Options{Addr: *addr, Protocol: 2})
	defer rdb.Close()
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	if err := rdb.Ping(ctx).Err(); err != nil {
		log.Fatalf("connect to rocket-mem at %s: %v", *addr, err)
	}

	// Subscribe FIRST, before draining the backlog. A PUBLISH sent before this
	// call completes is gone forever -- pub/sub has no history or replay, unlike
	// the list. Subscribing before draining closes the gap where a notification
	// could arrive between "check the queue" and "start listening" and be missed.
	sub := rdb.Subscribe(ctx, notifyChan)
	defer sub.Close()
	if _, err := sub.Receive(ctx); err != nil {
		log.Fatalf("subscribe: %v", err)
	}
	fmt.Printf("[%s] subscribed to %s, draining any existing backlog...\n", *name, notifyChan)

	drain(ctx, rdb, *name)

	msgs := sub.Channel()
	fmt.Printf("[%s] waiting for work (Ctrl+C to stop)\n", *name)
	for {
		select {
		case <-ctx.Done():
			fmt.Printf("[%s] shutting down\n", *name)
			return
		case _, ok := <-msgs:
			if !ok {
				return
			}
			// The notification payload itself doesn't matter -- it's a doorbell,
			// not the job. Every notified worker races to RPOP; only one of them
			// gets each job.
			drain(ctx, rdb, *name)
		}
	}
}

// drain pops and "processes" jobs until the queue is empty -- whether this
// worker just started (catching up on a backlog) or was just woken by PUBLISH.
func drain(ctx context.Context, rdb *redis.Client, name string) {
	for {
		result, err := rdb.RPop(ctx, queueKey).Result()
		if err == redis.Nil {
			return // queue empty
		}
		if err != nil {
			log.Printf("[%s] rpop error: %v", name, err)
			return
		}
		var job Job
		if err := json.Unmarshal([]byte(result), &job); err != nil {
			log.Printf("[%s] bad job payload, skipping: %v", name, err)
			continue
		}
		fmt.Printf("[%s] processing job %d: %s\n", name, job.ID, job.Task)
		time.Sleep(300 * time.Millisecond) // simulate work
		fmt.Printf("[%s] done with job %d\n", name, job.ID)
	}
}
