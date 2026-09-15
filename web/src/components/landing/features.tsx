import {
  Activity,
  Boxes,
  Database,
  GitBranch,
  HardDrive,
  Network,
  ShieldCheck,
  Zap,
} from "lucide-react"

import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"

const FEATURES = [
  {
    icon: Network,
    title: "Redis wire compatibility",
    description: "RESP2 and RESP3, with full HELLO version negotiation.",
  },
  {
    icon: Zap,
    title: "A second protocol, RMP",
    description:
      "Hand-rolled binary framing with request multiplexing, reachable on its own port, covering almost the entire command set.",
  },
  {
    icon: Database,
    title: "Data types",
    description:
      "Strings, hashes, lists, sets, and sorted sets, with Redis's WRONGTYPE and missing-key semantics matched command for command.",
  },
  {
    icon: HardDrive,
    title: "Durability",
    description:
      "Every write is appended to an AOF with a configurable fsync policy, plus point-in-time snapshots.",
  },
  {
    icon: GitBranch,
    title: "Replication",
    description:
      "Leader/follower over the ordinary RESP port; followers reject writes with -READONLY until promoted.",
  },
  {
    icon: Boxes,
    title: "Clustering",
    description:
      "Redis-Cluster-compatible hash slots (CRC16(hash_tag(key)) % 16384), with -MOVED redirection and CROSSSLOT enforcement.",
  },
  {
    icon: ShieldCheck,
    title: "Security",
    description:
      "Argon2-hashed passwords, per-user ACL rules over commands and key patterns, and optional TLS listeners for both protocols.",
  },
  {
    icon: Activity,
    title: "Observability",
    description:
      "A Prometheus /metrics endpoint, INFO in Redis's own format across eight sections, and a bounded slow log.",
  },
] as const

export function Features() {
  return (
    <section id="features" className="mx-auto max-w-6xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Features</h2>
        <p className="text-muted-foreground">
          Everything a Redis-compatible store needs, built from scratch.
        </p>
      </div>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        {FEATURES.map((feature) => (
          <Card key={feature.title}>
            <CardHeader>
              <feature.icon className="size-6 text-primary" />
              <CardTitle className="text-base">{feature.title}</CardTitle>
              <CardDescription>{feature.description}</CardDescription>
            </CardHeader>
          </Card>
        ))}
      </div>
    </section>
  )
}
