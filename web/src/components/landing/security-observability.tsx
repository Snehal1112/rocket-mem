import { Activity, FileClock, KeyRound, Lock, ShieldCheck } from "lucide-react"

import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"

const ITEMS = [
  {
    icon: KeyRound,
    title: "Argon2-hashed passwords",
    description: "Credentials are never stored or logged in plaintext.",
  },
  {
    icon: ShieldCheck,
    title: "Per-user ACLs",
    description:
      "Rules scoped over both commands and key patterns, per authenticated user.",
  },
  {
    icon: Lock,
    title: "Optional TLS",
    description:
      "TLS listeners run alongside the plaintext ones, for both RESP and RMP.",
  },
  {
    icon: Activity,
    title: "Prometheus /metrics",
    description:
      "A dedicated HTTP endpoint, plus INFO in Redis's own format across eight sections.",
  },
  {
    icon: FileClock,
    title: "Bounded slow log",
    description:
      "Commands at or over a configurable threshold are recorded for later inspection.",
  },
] as const

export function SecurityObservability() {
  return (
    <section id="security" className="mx-auto max-w-6xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">
          Security &amp; observability
        </h2>
      </div>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {ITEMS.map((item) => (
          <Card key={item.title}>
            <CardHeader>
              <item.icon className="size-6 text-primary" />
              <CardTitle className="text-base">{item.title}</CardTitle>
              <CardDescription>{item.description}</CardDescription>
            </CardHeader>
          </Card>
        ))}
      </div>
    </section>
  )
}
