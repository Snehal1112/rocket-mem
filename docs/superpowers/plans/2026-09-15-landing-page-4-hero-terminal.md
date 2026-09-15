# rocket-mem Landing Page — Hero & Terminal Demo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the two highest-design-judgment sections of the page — the hero (headline, drop-in-compatibility subheadline, CTAs) and a static terminal demo reinforcing that message visually.

**Architecture:** Both are new `src/components/landing/*.tsx` files, appended into `App.tsx`'s `<main>` in page order (hero first, terminal demo second — both above the fold). Both are self-contained: the copy-to-clipboard logic for the hero's Docker command is written inline here rather than factored into a shared helper, since the shared `copy-code-block` helper this pattern also needs for `quickstart.tsx` is deliberately defined later, in plan 7 (which runs after this one) — see that plan's self-review for why the direction is one-way.

**Tech Stack:** Same as prior plans, plus `lucide-react`'s `Github`, `Copy`, `Check` icons and the browser `navigator.clipboard` API.

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first (no `tailwind.config.js`), shadcn `base-luma` style on `@base-ui/react` (not Radix).
- Package manager: **Bun only**.
- Component sourcing: only vendor-copy new `ui/` primitives via `cp` from `../../rocketvault/web/` — this plan adds no new vendored primitives, only new `landing/` components built from ones already vendored.
- Base UI components are polymorphic via a `render` prop, **not** `asChild` — e.g. `<Button render={<a href="..." />}>text</Button>`.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks.
- All landing-page copy is static, hand-transcribed from `README.md` — no live data fetching, no typing/scroll animation (this plan's terminal demo is a static snapshot).
- Dev server binds `127.0.0.1:5174`.
- The six section anchor `id`s `SiteNav` links to are fixed: `#why`, `#features`, `#architecture`, `#performance`, `#security`, `#quickstart`. Neither section in this plan is one of those six (hero and the terminal demo sit above `#why`, unlinked from the nav), so neither needs an `id`.
- **Working directory:** `cp`/`git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Build the hero

**Files:**
- Create: `web/src/components/landing/hero.tsx`
- Modify: `web/src/App.tsx` — insert `<Hero />` as the first child of `<main>`

**Interfaces:**
- Consumes: `Button` (`@/components/ui/button`, with the `render` prop for its GitHub link), `Github`/`Copy`/`Check` icons (`lucide-react`).
- Produces: `Hero` (no props) from `@/components/landing/hero`, rendered first inside `<main>`.

**Suggested model:** opus (high design judgment)

- [ ] **Step 1: Invoke the `frontend-design` skill**

Before writing this component, invoke the `frontend-design` skill for guidance on typography scale, spacing, and making the hero feel distinctive rather than templated — this is the first thing a visitor sees, and it carries the most visual weight on the page.

- [ ] **Step 2: Create `web/src/components/landing/hero.tsx`**

The subheadline is the most important sentence on the page per the user's explicit request during design review: it must lead with drop-in RESP2/RESP3 compatibility and name the same concrete client libraries the README's opening paragraph does (`redis-cli`, `redis-py`, `ioredis`, `go-redis`), framed as "no code changes."

```tsx
import { useState } from "react"
import { Check, Copy, Github } from "lucide-react"

import { Button } from "@/components/ui/button"

const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"
const DOCKER_COMMAND =
  "docker run --rm -p 6379:6379 -p 6380:6380 ghcr.io/snehal1112/rocket-mem:latest"

function DockerCommand() {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    await navigator.clipboard.writeText(DOCKER_COMMAND)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div className="flex w-full max-w-xl items-center gap-2 rounded-2xl border border-border bg-muted/50 px-4 py-3">
      <code className="flex-1 overflow-x-auto whitespace-pre font-heading text-xs text-foreground sm:text-sm">
        $ {DOCKER_COMMAND}
      </code>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={copy}
        aria-label="Copy Docker command"
      >
        {copied ? <Check className="text-primary" /> : <Copy />}
      </Button>
    </div>
  )
}

export function Hero() {
  return (
    <section className="mx-auto flex max-w-4xl flex-col items-center gap-6 px-4 py-20 text-center sm:px-6 sm:py-28">
      <h1 className="text-balance font-heading text-4xl font-semibold tracking-tight sm:text-5xl">
        A Redis-compatible store, built from scratch in Rust
      </h1>
      <p className="max-w-2xl text-balance text-lg text-muted-foreground">
        rocket-mem speaks RESP2 and RESP3. Point <code>redis-cli</code>,{" "}
        <code>redis-py</code>, <code>ioredis</code>, or <code>go-redis</code>{" "}
        at it and it just works — no code changes.
      </p>
      <div className="flex flex-wrap items-center justify-center gap-3">
        <Button
          size="lg"
          render={<a href={GITHUB_URL} target="_blank" rel="noreferrer" />}
        >
          <Github />
          View on GitHub
        </Button>
      </div>
      <DockerCommand />
    </section>
  )
}
```

- [ ] **Step 3: Insert `<Hero />` into `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
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

Run: `bun run dev`, open `http://127.0.0.1:5174`.
Confirm: the headline renders in the heading (monospace) font, the subheadline names all four client libraries and reads naturally, the "View on GitHub" button opens the repo in a new tab, and clicking the copy icon next to the Docker command swaps it to a checkmark for about 1.5 seconds (confirm the clipboard actually received the text by pasting it somewhere). Check both light and dark mode. Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/hero.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the hero section to web/

Headline plus a compatibility-first subheadline naming redis-cli,
redis-py, ioredis, and go-redis explicitly ("no code changes"),
GitHub CTA, and a copyable Docker one-liner.
EOF
)"
```

---

### Task 2: Build the terminal demo

**Files:**
- Create: `web/src/components/landing/terminal-demo.tsx`
- Modify: `web/src/App.tsx` — insert `<TerminalDemo />` immediately after `<Hero />`

**Interfaces:**
- Consumes: `Card` (`@/components/ui/card`).
- Produces: `TerminalDemo` (no props) from `@/components/landing/terminal-demo`, rendered second inside `<main>`.

**Suggested model:** opus (high design judgment)

- [ ] **Step 1: Invoke the `frontend-design` skill**

Before writing this component, invoke the `frontend-design` skill for guidance on making a static "fake terminal" read as tasteful and consistent with the `base-luma` palette rather than a generic dark-box cliché, in both light and dark mode.

- [ ] **Step 2: Create `web/src/components/landing/terminal-demo.tsx`**

Static only — no typing animation, no scroll-triggered replay (per the spec's decision that richness on this page comes from the terminal snapshot and the performance chart, not motion). The exact commands and output are transcribed from the README's Quick start section.

```tsx
import { Card } from "@/components/ui/card"

const TERMINAL_LINES = [
  { prompt: true, text: "redis-cli -p 6379 SET user:1 alice" },
  { prompt: false, text: "OK" },
  { prompt: true, text: "redis-cli -p 6379 GET user:1" },
  { prompt: false, text: '"alice"' },
] as const

export function TerminalDemo() {
  return (
    <section className="mx-auto max-w-2xl px-4 pb-20 sm:px-6">
      <Card className="overflow-hidden bg-foreground p-0 text-background">
        <div className="flex items-center gap-1.5 border-b border-background/10 px-4 py-3">
          <span className="size-3 rounded-full bg-background/20" />
          <span className="size-3 rounded-full bg-background/20" />
          <span className="size-3 rounded-full bg-background/20" />
        </div>
        <div className="space-y-1 px-4 py-4 font-heading text-sm">
          {TERMINAL_LINES.map((line) => (
            <div key={line.text}>
              {line.prompt ? (
                <span>
                  <span className="text-background/50">$ </span>
                  {line.text}
                </span>
              ) : (
                <span className="text-background/70">{line.text}</span>
              )}
            </div>
          ))}
        </div>
      </Card>
    </section>
  )
}
```

- [ ] **Step 3: Insert `<TerminalDemo />` into `App.tsx`, after `<Hero />`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"
import { Hero } from "@/components/landing/hero"
import { TerminalDemo } from "@/components/landing/terminal-demo"

function App() {
  return (
    <>
      <SiteNav />
      <main>
        <Hero />
        <TerminalDemo />
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

Run: `bun run dev`, open `http://127.0.0.1:5174`.
Confirm: below the hero, a dark terminal-styled card shows three traffic-light dots, then the `SET`/`OK`/`GET`/`"alice"` transcript in the heading (monospace) font, with the `$ ` prompt prefix dimmed relative to the command text. Check both light and dark page mode — the card should stay legible (readable contrast) in both, since it deliberately stays dark internally regardless of page theme. Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/terminal-demo.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add the terminal demo section to web/

Static (no typing animation) redis-cli SET/GET transcript from
the README's Quick start section, reinforcing the hero's
compatibility message visually.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's `hero` and `terminal-demo` bullets from Page structure in full, including the explicit compatibility-led subheadline wording the user requested during design review.
- **Placeholder scan:** none.
- **Type consistency:** `Hero` and `TerminalDemo` both take no props, so nothing downstream can misuse them; `Button`'s `render` prop usage matches the pattern established in plan 3's `site-nav.tsx`.
- **Judgment call (recorded, not re-decided here):** the copy-to-clipboard logic in `DockerCommand` is intentionally local to this file rather than shared with `quickstart.tsx`'s three copy buttons (plan 7) — plan 7 defines the shared `copy-code-block.tsx` helper, since it runs after this plan and this plan must stay self-contained. If a future cleanup pass wants to de-duplicate, `hero.tsx`'s `DockerCommand` is the one to migrate onto that helper.

## Next plan

[`2026-09-15-landing-page-5-why-features.md`](2026-09-15-landing-page-5-why-features.md) — the why (value props) and features sections.
