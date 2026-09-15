# rocket-mem Landing Page — Security, Commands & Quickstart Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the three remaining structural sections — security/observability cards, a command-coverage accordion, and the quickstart tabs — completing the page's content.

**Architecture:** Three new `src/components/landing/*.tsx` files, appended into `App.tsx`'s `<main>` immediately after `<Performance />`, in this order: security-observability, command-coverage, quickstart. This plan also introduces `src/components/landing/copy-code-block.tsx`, a small shared copy-to-clipboard helper `quickstart.tsx`'s three tabs use — defined here (not in plan 4) because plan 4 runs first and must stay self-contained; plan 4's hero keeps its own local, near-identical clipboard logic rather than depending on a file this plan hasn't created yet when plan 4 executes.

**Tech Stack:** Same as prior plans; `Tabs`/`Accordion` (vendored in plan 2), `lucide-react` icons, no new dependencies.

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first, shadcn `base-luma` style on `@base-ui/react`.
- Package manager: **Bun only**.
- No new vendored `ui/` primitives — `Tabs` and `Accordion` were vendored in plan 2.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks.
- All copy is static, hand-transcribed from `README.md`'s Security/Observability feature bullets, Logging section, Command coverage table, and Quick start section — no live data fetching.
- Dev server binds `127.0.0.1:5174`.
- `command-coverage`'s section has an `id` (for deep-linking) but is **not** one of `SiteNav`'s six anchor links — it's supplementary depth, per the spec.
- **Working directory:** `git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Build the security & observability section

**Files:**
- Create: `web/src/components/landing/security-observability.tsx`
- Modify: `web/src/App.tsx` — insert `<SecurityObservability />` immediately after `<Performance />`

**Interfaces:**
- Consumes: `Card`, `CardHeader`, `CardTitle`, `CardDescription` (`@/components/ui/card`); `KeyRound`, `ShieldCheck`, `Lock`, `Activity`, `FileClock` icons (`lucide-react`).
- Produces: `SecurityObservability` (no props) from `@/components/landing/security-observability`, with `id="security"` on its root `<section>` — the anchor `SiteNav`'s "Security" link targets.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/security-observability.tsx`**

```tsx
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
```

- [ ] **Step 2: Insert `<SecurityObservability />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"
import { Performance } from "@/components/landing/performance"
import { SecurityObservability } from "@/components/landing/security-observability"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
        <Features />
        <Architecture />
        <Performance />
        <SecurityObservability />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
```

- [ ] **Step 3: Verify typecheck, lint, and the visual result**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Security" in the nav.
Confirm: a 5-card grid (3 columns wide on desktop) covering Argon2, ACLs, TLS, Prometheus, and the slow log. Stop the dev server before continuing.

- [ ] **Step 4: Commit**

```bash
git add web/src/components/landing/security-observability.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the security & observability section to web/

Cards for Argon2 password hashing, per-user ACLs, optional TLS,
Prometheus /metrics, and the bounded slow log.
EOF
)"
```

---

### Task 2: Build the command coverage accordion

**Files:**
- Create: `web/src/components/landing/command-coverage.tsx`
- Modify: `web/src/App.tsx` — insert `<CommandCoverage />` immediately after `<SecurityObservability />`

**Interfaces:**
- Consumes: `Accordion`, `AccordionItem`, `AccordionTrigger`, `AccordionContent` (`@/components/ui/accordion`).
- Produces: `CommandCoverage` (no props) from `@/components/landing/command-coverage`, with `id="command-coverage"` on its root `<section>` (deep-linkable, but deliberately not one of `SiteNav`'s six links — see the spec).

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/command-coverage.tsx`**

All nine rows and their full command lists are transcribed verbatim from the README's Command coverage table — no row is abbreviated to "…and more."

```tsx
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
```

- [ ] **Step 2: Insert `<CommandCoverage />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"
import { Performance } from "@/components/landing/performance"
import { SecurityObservability } from "@/components/landing/security-observability"
import { CommandCoverage } from "@/components/landing/command-coverage"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
        <Features />
        <Architecture />
        <Performance />
        <SecurityObservability />
        <CommandCoverage />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
```

- [ ] **Step 3: Verify typecheck, lint, and the visual result**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

Run: `bun run dev`, open `http://127.0.0.1:5174`, scroll to Command coverage (no nav link — scroll manually or navigate directly to `http://127.0.0.1:5174/#command-coverage`).
Confirm: nine collapsed accordion rows (String/Key, Hash, List, Set, Sorted Set, Server/Cluster, Auth/ACL, Transactions, Pub/Sub); clicking one expands it to show its full command list and collapses when clicked again. Stop the dev server before continuing.

- [ ] **Step 4: Commit**

```bash
git add web/src/components/landing/command-coverage.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the command coverage accordion to web/

Nine command-family rows transcribed from the README's Command
coverage table, each expandable to its full command list.
EOF
)"
```

---

### Task 3: Build the quickstart tabs and the shared copy-to-clipboard helper

**Files:**
- Create: `web/src/components/landing/copy-code-block.tsx`
- Create: `web/src/components/landing/quickstart.tsx`
- Modify: `web/src/App.tsx` — insert `<Quickstart />` immediately after `<CommandCoverage />`

**Interfaces:**
- Consumes: `Button` (`@/components/ui/button`); `Tabs`, `TabsList`, `TabsTrigger`, `TabsContent` (`@/components/ui/tabs`); `Copy`/`Check` icons (`lucide-react`).
- Produces: `CopyCodeBlock` (props: `{ code: string }`) from `@/components/landing/copy-code-block` — a reusable copy-to-clipboard block any future section can use (plan 4's `hero.tsx` deliberately keeps its own local, separate copy of this logic rather than importing this file — see that plan's self-review for why). `Quickstart` (no props) from `@/components/landing/quickstart`, with `id="quickstart"` on its root `<section>` — the anchor `SiteNav`'s "Quick Start" link targets, and the last section before `<SiteFooter />`.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/copy-code-block.tsx`**

```tsx
import { useState } from "react"
import { Check, Copy } from "lucide-react"

import { Button } from "@/components/ui/button"

export function CopyCodeBlock({ code }: { code: string }) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    await navigator.clipboard.writeText(code)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div className="flex items-start gap-2 rounded-2xl border border-border bg-muted/50 px-4 py-3">
      <pre className="flex-1 overflow-x-auto whitespace-pre-wrap font-heading text-xs text-foreground sm:text-sm">
        {code}
      </pre>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={copy}
        aria-label="Copy command"
        className="shrink-0"
      >
        {copied ? <Check className="text-primary" /> : <Copy />}
      </Button>
    </div>
  )
}
```

- [ ] **Step 2: Create `web/src/components/landing/quickstart.tsx`**

All three commands are transcribed verbatim from the README's Quick start section, including the exact `VERSION=v0.1.4` pin shown there.

```tsx
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui/tabs"
import { CopyCodeBlock } from "@/components/landing/copy-code-block"

const DOCKER_COMMAND =
  "docker run --rm -p 6379:6379 -p 6380:6380 ghcr.io/snehal1112/rocket-mem:latest"

const BINARY_COMMAND = `VERSION=v0.1.4
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz
curl -LO https://github.com/Snehal1112/rocket-mem/releases/download/$VERSION/rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
sha256sum -c rocket-mem-$VERSION-linux-amd64.tar.gz.sha256
tar -xzf rocket-mem-$VERSION-linux-amd64.tar.gz
./rocket-mem-$VERSION-linux-amd64`

const SOURCE_COMMAND = `git clone https://github.com/Snehal1112/rocket-mem.git
cd rocket-mem
cargo build --release --bin rocket-mem
./target/release/rocket-mem`

export function Quickstart() {
  return (
    <section id="quickstart" className="mx-auto max-w-3xl px-4 py-20 sm:px-6">
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Quick start</h2>
        <p className="text-muted-foreground">
          No configuration file and no environment variables required.
        </p>
      </div>
      <Tabs defaultValue="docker">
        <TabsList className="mx-auto">
          <TabsTrigger value="docker">Docker</TabsTrigger>
          <TabsTrigger value="binary">Binary download</TabsTrigger>
          <TabsTrigger value="source">Build from source</TabsTrigger>
        </TabsList>
        <TabsContent value="docker">
          <CopyCodeBlock code={DOCKER_COMMAND} />
        </TabsContent>
        <TabsContent value="binary">
          <CopyCodeBlock code={BINARY_COMMAND} />
        </TabsContent>
        <TabsContent value="source">
          <CopyCodeBlock code={SOURCE_COMMAND} />
        </TabsContent>
      </Tabs>
    </section>
  )
}
```

- [ ] **Step 3: Insert `<Quickstart />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"
import { Performance } from "@/components/landing/performance"
import { SecurityObservability } from "@/components/landing/security-observability"
import { CommandCoverage } from "@/components/landing/command-coverage"
import { Quickstart } from "@/components/landing/quickstart"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
        <Features />
        <Architecture />
        <Performance />
        <SecurityObservability />
        <CommandCoverage />
        <Quickstart />
      </main>
      <SiteFooter />
    </>
  )
}

export default App
```

- [ ] **Step 4: Verify typecheck, lint, and the visual result**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Quick Start" in the nav.
Confirm: three tabs (Docker / Binary download / Build from source), Docker selected by default; each tab shows its exact command block with a working copy button (click it, confirm the checkmark swap, paste somewhere to confirm the clipboard content matches). This is the last section before the footer — confirm `<SiteFooter />` immediately follows it with no gap or overlap. Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/copy-code-block.tsx web/src/components/landing/quickstart.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the quickstart tabs and shared copy-code-block to web/

Docker / binary-download / build-from-source tabs, each with a
copy-to-clipboard code block via a new shared CopyCodeBlock
helper. All three commands transcribed verbatim from the README's
Quick start section. This completes the page's content — every
section from the spec is now in App.tsx.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's `security-observability`, `command-coverage`, and `quickstart` bullets from Page structure in full. After this plan, every section listed in the spec's Page structure is present in `App.tsx` in the documented order.
- **Placeholder scan:** none — all nine command-coverage rows are complete, not abbreviated.
- **Type consistency:** `CopyCodeBlock`'s `{ code: string }` prop is used identically by all three `quickstart.tsx` tabs. `AccordionItem`'s `value` prop uses each category name directly (unique per item, matching the vendored `accordion.tsx`'s expected shape). Every section component in this plan sets its `id` to match its corresponding `SiteNav` entry exactly (`security`, `quickstart`) or, for `command-coverage`, an `id` that exists for deep-linking without a nav entry — consistent with plans 3 and 5's same pattern.

## Next plan

[`2026-09-15-landing-page-8-integration-verification.md`](2026-09-15-landing-page-8-integration-verification.md) — final verification pass, `web/README.md`, and manual QA. This is the final plan in the chain.
