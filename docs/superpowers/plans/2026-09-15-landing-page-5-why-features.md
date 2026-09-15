# rocket-mem Landing Page — Why & Features Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the "why" value-props section (compatibility-first) and the full features grid, both structural compositions of already-vendored `Card` primitives.

**Architecture:** Two new `src/components/landing/*.tsx` files, each with its own `id` matching one of `SiteNav`'s six anchors (`why`, `features`), appended into `App.tsx`'s `<main>` immediately after `<TerminalDemo />` (why first, then features — matching the page's fixed section order from the spec).

**Tech Stack:** Same as prior plans; `lucide-react` icons only, no new dependencies.

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first, shadcn `base-luma` style on `@base-ui/react`.
- Package manager: **Bun only**.
- No new vendored `ui/` primitives in this plan — both sections are built entirely from `Card`/`CardHeader`/`CardTitle`/`CardDescription` (vendored in plan 2).
- `web/` is **not** part of the Cargo workspace and **not** CI-gated.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks.
- All copy is static, hand-transcribed from `README.md`'s opening paragraphs and Features section — no live data fetching.
- Dev server binds `127.0.0.1:5174`.
- `tsconfig.app.json` has `noUnusedLocals`/`noUnusedParameters` on — only import the `Card` sub-components each file actually renders (neither section uses `CardContent`, `CardAction`, or `CardFooter`).
- **Working directory:** `git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Build the why (value props) section

**Files:**
- Create: `web/src/components/landing/why.tsx`
- Modify: `web/src/App.tsx` — insert `<Why />` immediately after `<TerminalDemo />`

**Interfaces:**
- Consumes: `Card`, `CardHeader`, `CardTitle`, `CardDescription` (`@/components/ui/card`); `ArrowLeftRight`, `Layers`, `Cpu`, `HardDrive` icons (`lucide-react`).
- Produces: `Why` (no props) from `@/components/landing/why`, with `id="why"` on its root `<section>` — this is the anchor `SiteNav`'s "Why" link targets.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/why.tsx`**

The compatibility card is the most prominent one (spans the full grid width, listed first), with the same "no code changes" wording and the same four named client libraries as the hero — deliberately consistent phrasing between the two.

```tsx
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
              <code>ioredis</code>, <code>go-redis</code>, or any other — at
              it and it just works. No code changes.
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
```

- [ ] **Step 2: Insert `<Why />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
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

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Why" in the nav.
Confirm: the page scrolls to a full-width compatibility card followed by three evenly-sized cards below it, each with an icon, title, and description; the compatibility card's wording matches the hero's in spirit (same four client libraries named). Stop the dev server before continuing.

- [ ] **Step 4: Commit**

```bash
git add web/src/components/landing/why.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the why (value props) section to web/

Four cards: drop-in Redis compatibility (full width, most
prominent, same "no code changes" wording as the hero), dual
RESP+RMP protocol, multi-threaded design, durability.
EOF
)"
```

---

### Task 2: Build the features grid

**Files:**
- Create: `web/src/components/landing/features.tsx`
- Modify: `web/src/App.tsx` — insert `<Features />` immediately after `<Why />`

**Interfaces:**
- Consumes: `Card`, `CardHeader`, `CardTitle`, `CardDescription` (`@/components/ui/card`); `Network`, `Zap`, `Database`, `HardDrive`, `GitBranch`, `Boxes`, `ShieldCheck`, `Activity` icons (`lucide-react`).
- Produces: `Features` (no props) from `@/components/landing/features`, with `id="features"` on its root `<section>` — the anchor `SiteNav`'s "Features" link targets.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/features.tsx`**

One card per bullet in the README's Features section, condensed to fit — the full-depth versions of the security and observability bullets get their own dedicated section later (plan 7's `security-observability.tsx`); this grid is the one-line overview of all eight.

```tsx
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
```

- [ ] **Step 2: Insert `<Features />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
        <Why />
        <Features />
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

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Features" in the nav.
Confirm: the page scrolls to a heading, subheading, and an 8-card grid — 4 columns wide on a desktop-width window, collapsing to 2 columns then 1 as the window narrows (resize to check). Stop the dev server before continuing.

- [ ] **Step 4: Commit**

```bash
git add web/src/components/landing/features.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the features grid section to web/

One card per README Features bullet (wire compatibility, RMP,
data types, durability, replication, clustering, security,
observability), each with a lucide icon and condensed copy.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's `why` and `features` bullets from Page structure in full, including the "explicit and prominent" compatibility-card requirement.
- **Placeholder scan:** none.
- **Type consistency:** `Why` and `Features` both take no props and each sets its own `id` matching the corresponding `SiteNav` anchor exactly (`why`, `features`) — verified against `site-nav.tsx`'s `NAV_LINKS` array from plan 3.

## Next plan

[`2026-09-15-landing-page-6-architecture-performance.md`](2026-09-15-landing-page-6-architecture-performance.md) — the architecture diagram and performance chart (high design judgment; invoke the `frontend-design` skill first).
