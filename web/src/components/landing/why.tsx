import { ArrowLeftRight, Cpu, HardDrive, Layers } from "lucide-react"

import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"

const VALUE_PROPS = [
  {
    icon: Layers,
    title: "Dual protocol: RESP + RMP",
    description:
      "Speaks RESP2 and RESP3 for every existing Redis client, plus RMP — its own binary protocol adding the one thing RESP structurally can't do: request multiplexing, many in-flight requests on one connection.",
  },
  {
    icon: Cpu,
    title: "Multi-threaded, unlike Redis",
    description:
      "Runs on Tokio's multi-threaded runtime across 16 independently-locked shards, so requests against different keys execute on different CPU cores at the same instant. Real Redis is deliberately single-threaded for command execution.",
  },
  {
    icon: HardDrive,
    title: "Durable by default",
    description:
      "Every write is appended to an AOF with a configurable fsync policy, plus point-in-time snapshots. Startup replays the snapshot and only the AOF tail written after it.",
  },
] as const

export function Why() {
  return (
    <section id="why" className="mx-auto max-w-6xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">
          Why rocket-mem
        </h2>
        <p className="text-muted-foreground">
          Compatibility first, with the durability and concurrency Redis
          leaves on the table.
        </p>
      </div>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <Card className="sm:col-span-3">
          <CardHeader>
            <ArrowLeftRight className="size-6 text-primary" />
            <CardTitle className="text-lg">
              Drop-in Redis compatibility
            </CardTitle>
            <CardDescription className="text-base text-foreground">
              rocket-mem speaks RESP2 and RESP3, with full <code>HELLO</code>{" "}
              version negotiation. Point your existing Redis client —{" "}
              <code>redis-cli</code>, <code>redis-py</code>,{" "}
              <code>ioredis</code>, <code>go-redis</code>, or any other — at it
              and it just works. No code changes.
            </CardDescription>
          </CardHeader>
        </Card>
        {VALUE_PROPS.map((prop) => (
          <Card key={prop.title}>
            <CardHeader>
              <prop.icon className="size-6 text-primary" />
              <CardTitle className="text-lg">{prop.title}</CardTitle>
              <CardDescription>{prop.description}</CardDescription>
            </CardHeader>
          </Card>
        ))}
      </div>
    </section>
  )
}
