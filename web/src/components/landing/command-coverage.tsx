import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion"

const COMMAND_CATEGORIES = [
  {
    category: "String/Key",
    commands:
      "GET, SET (NX/XX/EX/PX), GETSET, GETRANGE, SETRANGE, APPEND, STRLEN, INCR/DECR/INCRBY, MSET, MGET, MSETNX, RENAME, RENAMENX, TYPE, RANDOMKEY, KEYS, SCAN, DEL/EXISTS (variadic), EXPIRE, PEXPIRE, EXPIREAT, PEXPIREAT, TTL, PTTL, PERSIST, MEMORY USAGE, OBJECT ENCODING",
  },
  {
    category: "Hash",
    commands:
      "HSET, HGET, HDEL, HEXISTS, HGETALL, HLEN, HINCRBY, HKEYS, HVALS, HMGET, HSETNX, HSCAN",
  },
  {
    category: "List",
    commands:
      "LPUSH, RPUSH (variadic), LPOP, RPOP, LRANGE, LLEN, LINDEX, LSET, LTRIM, LREM, LINSERT",
  },
  {
    category: "Set",
    commands:
      "SADD, SREM, SMEMBERS, SISMEMBER, SCARD, SINTER, SUNION, SDIFF, SINTERSTORE, SUNIONSTORE, SDIFFSTORE, SPOP, SRANDMEMBER, SSCAN",
  },
  {
    category: "Sorted Set",
    commands:
      "ZADD (single pair only, no NX/XX/GT/LT/CH/INCR), ZSCORE, ZREM, ZCARD, ZINCRBY, ZRANGE, ZRANK",
  },
  {
    category: "Server/Cluster",
    commands:
      "PING, ECHO, SELECT, COMMAND, HELLO, INFO [section], SAVE, BGREWRITEAOF, REPLICAOF, PSYNC, DEBUG SLEEP, CLUSTER KEYSLOT/SHARDS/NODES/INFO/MYID, SLOWLOG GET/LEN/RESET",
  },
  {
    category: "Auth/ACL",
    commands:
      "AUTH (single-arg and <user> <pass>), ACL SETUSER/DELUSER/WHOAMI/LIST/GETUSER",
  },
  {
    category: "Transactions",
    commands:
      "MULTI, EXEC, DISCARD (writers-only isolation; no WATCH/UNWATCH yet)",
  },
  {
    category: "Pub/Sub",
    commands:
      "SUBSCRIBE, UNSUBSCRIBE, PSUBSCRIBE, PUNSUBSCRIBE, PUBLISH, PUBSUB (CHANNELS/NUMSUB/NUMPAT) — single-node delivery only, no cluster-wide fanout",
  },
] as const

export function CommandCoverage() {
  return (
    <section
      id="command-coverage"
      className="mx-auto max-w-3xl px-4 py-20 sm:px-6"
    >
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">
          Command coverage
        </h2>
        <p className="text-muted-foreground">
          Nine command families, matched to Redis command for command.
        </p>
      </div>
      <Accordion>
        {COMMAND_CATEGORIES.map((entry) => (
          <AccordionItem key={entry.category} value={entry.category}>
            <AccordionTrigger>{entry.category}</AccordionTrigger>
            <AccordionContent>
              <p>{entry.commands}</p>
            </AccordionContent>
          </AccordionItem>
        ))}
      </Accordion>
    </section>
  )
}
