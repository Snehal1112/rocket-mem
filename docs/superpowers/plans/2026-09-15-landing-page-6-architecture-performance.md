# rocket-mem Landing Page — Architecture & Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the two remaining high-design-judgment sections — the three-layer architecture diagram and the recharts performance comparison against `redis-server`.

**Architecture:** Two new `src/components/landing/*.tsx` files. `architecture.tsx` rebuilds the README's ASCII three-layer diagram as real stacked, connected `div`s (no image). `performance.tsx` wraps the vendored `chart.tsx` primitive around a static data array hand-transcribed from the README's Performance table. Both get `id`s matching `SiteNav`'s anchors (`architecture`, `performance`) and are appended into `App.tsx` immediately after `<Features />`.

**Tech Stack:** Same as prior plans, plus `recharts`' `BarChart`/`Bar`/`XAxis`/`YAxis`/`CartesianGrid` (already a dependency; `chart.tsx` re-exports the shadcn wrapper around it, vendored in plan 3).

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first, shadcn `base-luma` style on `@base-ui/react`.
- Package manager: **Bun only**.
- No new vendored `ui/` primitives — `chart.tsx` was already vendored in plan 3.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks.
- All performance numbers are static, hand-transcribed from `README.md`'s Performance table (measured 2026-09-08) — not live-fetched, and the plan's copy must carry that date/caveat rather than presenting the numbers as always-current.
- Dev server binds `127.0.0.1:5174`.
- **Working directory:** `git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Build the architecture diagram

**Files:**
- Create: `web/src/components/landing/architecture.tsx`
- Modify: `web/src/App.tsx` — insert `<Architecture />` immediately after `<Features />`

**Interfaces:**
- Consumes: nothing beyond plain Tailwind classes (no shared UI primitive needed — the diagram is hand-built `div`s, not a `Card`).
- Produces: `Architecture` (no props) from `@/components/landing/architecture`, with `id="architecture"` on its root `<section>`.

**Suggested model:** opus (high design judgment)

- [ ] **Step 1: Invoke the `frontend-design` skill**

Before writing this component, invoke the `frontend-design` skill for guidance on making a stacked-layer diagram read clearly and feel intentional rather than like three generic boxes.

- [ ] **Step 2: Create `web/src/components/landing/architecture.tsx`**

The three layers and their contents are transcribed verbatim from the README's Architecture section diagram; the connecting lines and the 16-shard callout below replace the README's own prose paragraph with something skimmable.

```tsx
const LAYERS = [
  {
    name: "Protocol layer",
    detail: "RESP2 / RESP3, RMP",
  },
  {
    name: "Command dispatcher",
    detail: "routing, arg checks, auth, cluster, AOF",
  },
  {
    name: "Storage engine",
    detail: "data structures, expiry, persistence",
  },
] as const

export function Architecture() {
  return (
    <section
      id="architecture"
      className="mx-auto max-w-4xl px-4 py-20 sm:px-6"
    >
      <div className="mb-10 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Architecture</h2>
        <p className="text-muted-foreground">
          Three layers, with a strict rule: the storage engine knows nothing
          about any wire protocol.
        </p>
      </div>
      <div className="flex flex-col items-center">
        {LAYERS.map((layer, index) => (
          <div
            key={layer.name}
            className="flex w-full flex-col items-center"
          >
            <div className="w-full max-w-xl rounded-2xl border border-border bg-card px-6 py-4 text-center shadow-sm">
              <p className="font-heading text-sm font-semibold">
                {layer.name}
              </p>
              <p className="text-sm text-muted-foreground">{layer.detail}</p>
            </div>
            {index < LAYERS.length - 1 && (
              <div className="h-8 w-px bg-border" aria-hidden="true" />
            )}
          </div>
        ))}
      </div>
      <p className="mx-auto mt-10 max-w-2xl text-center text-sm text-muted-foreground">
        One Tokio task per connection; the keyspace is split into 16
        independently-locked shards, so requests against different keys
        execute on different CPU cores at the same instant.
      </p>
    </section>
  )
}
```

- [ ] **Step 3: Insert `<Architecture />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"

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

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Architecture" in the nav.
Confirm: three stacked bordered boxes (Protocol layer / Command dispatcher / Storage engine) connected by short vertical lines, each showing its detail text, followed by the 16-shard callout paragraph. Check both light and dark mode. Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/architecture.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the architecture diagram section to web/

Three-layer diagram (protocol / dispatcher / storage engine)
rebuilt as real connected elements rather than an image, plus a
16-shard concurrency callout.
EOF
)"
```

---

### Task 2: Build the performance chart

**Files:**
- Create: `web/src/components/landing/performance.tsx`
- Modify: `web/src/App.tsx` — insert `<Performance />` immediately after `<Architecture />`

**Interfaces:**
- Consumes: `ChartContainer`, `ChartTooltip`, `ChartTooltipContent`, `ChartLegend`, `ChartLegendContent`, `type ChartConfig` (`@/components/ui/chart`); `Bar`, `BarChart`, `CartesianGrid`, `XAxis`, `YAxis` (`recharts`).
- Produces: `Performance` (no props) from `@/components/landing/performance`, with `id="performance"` on its root `<section>`.

**Suggested model:** opus (high design judgment)

- [ ] **Step 1: Invoke the `frontend-design` skill**

Before writing this component, invoke the `frontend-design` skill for guidance on presenting a data-dense comparison chart clearly at both desktop and mobile widths.

- [ ] **Step 2: Create `web/src/components/landing/performance.tsx`**

The 8 rows and both req/s values per row are transcribed verbatim from the README's Performance table (`redis-benchmark -t set,get -n 100000 -c 50 -r 100000`, median of three sweeps, measured 2026-09-08). A horizontal bar chart (`layout="vertical"`) reads better than vertical bars given how long the workload labels are.

```tsx
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts"

import {
  ChartContainer,
  ChartLegend,
  ChartLegendContent,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart"

const PERFORMANCE_DATA = [
  {
    workload: "SET, 3B, no pipeline",
    "redis-server": 78247,
    "rocket-mem": 90662,
  },
  { workload: "SET, 1KB, P16", "redis-server": 438596, "rocket-mem": 500000 },
  { workload: "GET, 1KB, P16", "redis-server": 746269, "rocket-mem": 763359 },
  {
    workload: "GET, 1KB, no pipeline",
    "redis-server": 99010,
    "rocket-mem": 97943,
  },
  { workload: "SET, 3B, P16", "redis-server": 763359, "rocket-mem": 729927 },
  {
    workload: "GET, 3B, no pipeline",
    "redis-server": 105597,
    "rocket-mem": 98039,
  },
  {
    workload: "SET, 1KB, no pipeline",
    "redis-server": 96339,
    "rocket-mem": 86655,
  },
  {
    workload: "GET, 3B, P16",
    "redis-server": 1449275,
    "rocket-mem": 1176471,
  },
] as const

const chartConfig = {
  "redis-server": { label: "redis-server", color: "var(--chart-2)" },
  "rocket-mem": { label: "rocket-mem", color: "var(--chart-1)" },
} satisfies ChartConfig

export function Performance() {
  return (
    <section id="performance" className="mx-auto max-w-4xl px-4 py-20 sm:px-6">
      <div className="mb-6 flex flex-col gap-2 text-center">
        <h2 className="font-heading text-3xl font-semibold">Performance</h2>
        <p className="text-2xl font-semibold text-primary">
          0.86x–1.23x of Redis, faster on 3 of 8 measured workloads
        </p>
        <p className="text-muted-foreground">
          <code>redis-benchmark -t set,get -n 100000 -c 50 -r 100000</code>,
          median of three sweeps against redis-server 8.10.1 on the same
          host, matching durability (<code>appendonly yes</code>,{" "}
          <code>appendfsync everysec</code>) on both servers.
        </p>
      </div>
      <ChartContainer
        config={chartConfig}
        className="mx-auto aspect-auto h-96 w-full"
      >
        <BarChart
          data={[...PERFORMANCE_DATA]}
          layout="vertical"
          margin={{ left: 24 }}
        >
          <CartesianGrid horizontal={false} />
          <XAxis
            type="number"
            tickFormatter={(value: number) => value.toLocaleString()}
          />
          <YAxis
            type="category"
            dataKey="workload"
            width={140}
            tick={{ fontSize: 12 }}
          />
          <ChartTooltip content={<ChartTooltipContent />} />
          <ChartLegend content={<ChartLegendContent />} />
          <Bar
            dataKey="redis-server"
            fill="var(--color-redis-server)"
            radius={4}
          />
          <Bar
            dataKey="rocket-mem"
            fill="var(--color-rocket-mem)"
            radius={4}
          />
        </BarChart>
      </ChartContainer>
      <p className="mx-auto mt-6 max-w-2xl text-center text-xs text-muted-foreground">
        Medians from a single, noisy host, measured 2026-09-08 — run-to-run
        spread is wide, so no ratio here should be read to two significant
        figures.
      </p>
    </section>
  )
}
```

- [ ] **Step 3: Insert `<Performance />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"
import { Why } from "@/components/landing/why"
import { Features } from "@/components/landing/features"
import { Architecture } from "@/components/landing/architecture"
import { Performance } from "@/components/landing/performance"

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

Run: `bun run dev`, open `http://127.0.0.1:5174`, click "Performance" in the nav.
Confirm: the headline stat callout renders above a horizontal bar chart with 8 rows, two bars per row (redis-server and rocket-mem, visually distinct colors, a legend identifying which is which), hovering a bar shows a tooltip with the exact number, and the measurement-date caveat paragraph is present below the chart. Check both light and dark mode — the chart's colors come from `--chart-1`/`--chart-2` tokens, which differ between themes. Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/performance.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the performance chart section to web/

Headline stat callout plus a recharts horizontal bar chart
comparing rocket-mem vs redis-server across the 8 SET/GET
workloads from the README's Performance table (measured
2026-09-08), with a caveat noting the measurement date.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's `architecture` and `performance` bullets from Page structure in full, including the required headline stat callout and the "not always-current" dating caveat.
- **Placeholder scan:** none — all 8 performance rows carry real numbers transcribed from the README, not a subset or "…and more."
- **Type consistency:** `Architecture` and `Performance` both take no props; `Performance`'s `chartConfig` keys (`"redis-server"`, `"rocket-mem"`) match the `dataKey`s used in both the data array and the `<Bar>` elements exactly, which is what makes `chart.tsx`'s generated `--color-redis-server`/`--color-rocket-mem` CSS variables resolve.

## Next plan

[`2026-09-15-landing-page-7-security-commands-quickstart.md`](2026-09-15-landing-page-7-security-commands-quickstart.md) — security/observability, command coverage, and the quickstart tabs (including the shared copy-to-clipboard helper).
