# rocket-mem Landing Page — Chart, Nav & Footer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Vendor the chart primitive the performance section (plan 6) will need, and build the site nav and footer — the fixed page shell every later plan inserts its section into.

**Architecture:** `chart.tsx` is a straight `cp`, same as every other vendored primitive. `site-nav.tsx` and `site-footer.tsx` are new, rocket-mem-specific components under `src/components/landing/` — this is the first plan to write files in that folder, mirroring rocketvault/web's own `components/landing/*` pattern (see the spec's Page structure section). `App.tsx` becomes the permanent page shell — `<SiteNav />`, an empty `<main>`, `<SiteFooter />` — that plans 4 through 7 append sections into, always immediately before `<SiteFooter />`.

**Tech Stack:** Same as plans 1–2, plus `recharts` (already a `package.json` dependency from plan 1) and `lucide-react` icons (`Github`, `Menu`).

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first (no `tailwind.config.js`), shadcn `base-luma` style on `@base-ui/react` (not Radix).
- Package manager: **Bun only** — `bun.lock` committed, no npm/yarn/pnpm lockfile.
- Component sourcing: vendor-copy from `../../rocketvault/web/src/components/...` via `cp`, byte-identical unless a task says otherwise — never fetch from a live shadcn registry.
- Fonts: `--font-sans` = Noto Sans Variable (body), `--font-heading` = JetBrains Mono Variable (headings) — already wired.
- Theming: light/dark/system via vendored `theme-provider.tsx`'s `useTheme()` — never hand-roll a separate theme mechanism.
- Base UI components are polymorphic via a `render` prop, **not** an `asChild` prop — e.g. `<Button render={<a href="..." />}>text</Button>`.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated — do not touch `.github/workflows/ci.yml`.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks, never invented unit tests.
- All landing-page copy is static, hand-transcribed from `README.md` — no live data fetching.
- Dev server binds `127.0.0.1:5174`.
- **Working directory:** `cp`/`git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Vendor the chart primitive

**Files:**
- Create: `web/src/components/ui/chart.tsx` (copied from `../../rocketvault/web/`)

**Interfaces:**
- Consumes: `cn()` from `@/lib/utils`; `recharts` (already installed — see plan 1, Task 1).
- Produces: `ChartContainer` (props: `{ config: ChartConfig, children, className?, id? }`), `ChartTooltip` (re-exported `recharts` `Tooltip`), `ChartTooltipContent`, `ChartLegend`, `ChartLegendContent`, and the `ChartConfig` type (`Record<string, { label?: ReactNode, icon?: ComponentType } & ({ color?: string } | { theme: { light: string, dark: string } })>`) from `@/components/ui/chart` — plan 6's `performance.tsx` builds its bar chart on top of these.

**Suggested model:** haiku

- [ ] **Step 1: Copy the file**

```bash
cp ../../rocketvault/web/src/components/ui/chart.tsx web/src/components/ui/chart.tsx
```

No edits needed — it imports `recharts` and `cn` from `@/lib/utils`, both already available in this project (`recharts` was added to `package.json` in plan 1, Task 1, specifically so this step needs no separate `bun add`).

- [ ] **Step 2: Verify typecheck and lint**

Run: `bun run typecheck && bun run lint`
Expected: both clean. If `recharts`' types aren't found, re-run `bun install` from `web/` first — `package.json` already lists it, but the vendored file is the first thing in this project to actually import it.

- [ ] **Step 3: Commit**

```bash
git add web/src/components/ui/chart.tsx
git commit -m "$(cat <<'EOF'
Vendor the chart primitive into web/

Byte-identical copy from rocketvault/web; recharts was already a
package.json dependency (added in plan 1 for this purpose). Used
by the performance section in a later plan.
EOF
)"
```

---

### Task 2: Build the site nav

**Files:**
- Create: `web/src/components/landing/site-nav.tsx`
- Modify: `web/src/App.tsx` — replace the plan-2 theme-toggle/Card sanity check with `<SiteNav />` at the top, followed by an empty `<main>`

**Interfaces:**
- Consumes: `Button` (`@/components/ui/button`), `Sheet`/`SheetContent`/`SheetHeader`/`SheetTitle`/`SheetTrigger` (`@/components/ui/sheet`), `useTheme` (`@/components/theme-provider`), `Github`/`Menu` icons (`lucide-react`).
- Produces: `SiteNav` (no props) from `@/components/landing/site-nav` — the six anchor `id`s it links to (`#why`, `#features`, `#architecture`, `#performance`, `#security`, `#quickstart`) are the exact `id` values every section component from plan 4 onward must set on its own root element, or the nav links silently go nowhere.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/site-nav.tsx`**

```tsx
import { useState } from "react"
import { Github, Menu } from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet"
import { useTheme } from "@/components/theme-provider"

const NAV_LINKS = [
  { href: "#why", label: "Why" },
  { href: "#features", label: "Features" },
  { href: "#architecture", label: "Architecture" },
  { href: "#performance", label: "Performance" },
  { href: "#security", label: "Security" },
  { href: "#quickstart", label: "Quick Start" },
]

const THEME_SEQUENCE = ["light", "dark", "system"] as const

const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"

function ThemeToggle() {
  const { theme, setTheme } = useTheme()

  const cycleTheme = () => {
    const currentIndex = THEME_SEQUENCE.indexOf(theme)
    const nextTheme =
      THEME_SEQUENCE[(currentIndex + 1) % THEME_SEQUENCE.length]
    setTheme(nextTheme)
  }

  return (
    <Button variant="outline" size="sm" onClick={cycleTheme}>
      Theme: {theme}
    </Button>
  )
}

function GitHubButton() {
  return (
    <Button
      variant="outline"
      size="sm"
      render={<a href={GITHUB_URL} target="_blank" rel="noreferrer" />}
    >
      <Github />
      GitHub
    </Button>
  )
}

function NavLinks({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <>
      {NAV_LINKS.map((link) => (
        <a
          key={link.href}
          href={link.href}
          onClick={onNavigate}
          className="text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          {link.label}
        </a>
      ))}
    </>
  )
}

export function SiteNav() {
  const [mobileOpen, setMobileOpen] = useState(false)

  return (
    <header className="sticky top-0 z-40 border-b border-border bg-background/80 backdrop-blur-sm">
      <div className="mx-auto flex h-16 max-w-6xl items-center justify-between gap-4 px-4 sm:px-6">
        <a href="#" className="font-heading text-lg font-semibold">
          rocket-mem
        </a>

        <nav className="hidden items-center gap-6 md:flex">
          <NavLinks />
        </nav>

        <div className="hidden items-center gap-2 md:flex">
          <ThemeToggle />
          <GitHubButton />
        </div>

        <div className="flex items-center gap-2 md:hidden">
          <ThemeToggle />
          <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
            <SheetTrigger render={<Button variant="outline" size="icon" />}>
              <Menu />
              <span className="sr-only">Open menu</span>
            </SheetTrigger>
            <SheetContent side="right">
              <SheetHeader>
                <SheetTitle>rocket-mem</SheetTitle>
              </SheetHeader>
              <nav className="flex flex-col gap-4 px-6">
                <NavLinks onNavigate={() => setMobileOpen(false)} />
                <GitHubButton />
              </nav>
            </SheetContent>
          </Sheet>
        </div>
      </div>
    </header>
  )
}
```

- [ ] **Step 2: Replace `App.tsx`'s body with `<SiteNav />` and a shell `<main>`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"

function App() {
  return (
    <>
      <SiteNav />
      <main>{/* sections are appended here by later plans */}</main>
    </>
  )
}

export default App
```

- [ ] **Step 3: Verify typecheck and lint**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

- [ ] **Step 4: Verify manually — desktop and mobile widths**

Run: `bun run dev`, open `http://127.0.0.1:5174`.

At full browser width (desktop): confirm the "rocket-mem" brand mark, the six nav links (Why / Features / Architecture / Performance / Security / Quick Start), the theme toggle button, and the GitHub button are all visible inline in the header, and the theme toggle still cycles light → dark → system as it did in plan 2.

Resize the browser to roughly 400px wide (or use devtools' responsive mode): confirm the inline nav links and GitHub button disappear, replaced by a hamburger (`Menu`) icon button next to the theme toggle. Click it: a sheet drawer slides in from the right showing "rocket-mem" as a title, the same six links stacked vertically, and the GitHub button. Click a link: the drawer closes (confirms `onNavigate` fires `setMobileOpen(false)`). Click the hamburger again and use the drawer's own close (X) button to confirm it closes that way too.

Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/landing/site-nav.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add SiteNav to web/, replacing the theming sanity check

Sticky header with anchor links to the six main sections, the
theme toggle (moved here from App.tsx), a GitHub link button, and
a Sheet-based mobile drawer below the md breakpoint. App.tsx is
now the permanent page shell later plans append sections into.
EOF
)"
```

---

### Task 3: Build the site footer

**Files:**
- Create: `web/src/components/landing/site-footer.tsx`
- Modify: `web/src/App.tsx` — add `<SiteFooter />` after `<main>`

**Interfaces:**
- Consumes: nothing beyond plain HTML and Tailwind classes (no shared component state).
- Produces: `SiteFooter` (no props) from `@/components/landing/site-footer` — rendered once, at the very end of `App.tsx`; no later plan touches this file again.

**Suggested model:** sonnet

- [ ] **Step 1: Create `web/src/components/landing/site-footer.tsx`**

```tsx
const GITHUB_URL = "https://github.com/Snehal1112/rocket-mem"
const GETTING_STARTED_URL = `${GITHUB_URL}/blob/main/docs/getting-started.md`
const LICENSE_URL = `${GITHUB_URL}/blob/main/LICENSE`

const FOOTER_LINKS = [
  { href: GITHUB_URL, label: "GitHub" },
  { href: GETTING_STARTED_URL, label: "Getting started" },
  { href: LICENSE_URL, label: "License (MIT)" },
]

export function SiteFooter() {
  return (
    <footer className="border-t border-border">
      <div className="mx-auto flex max-w-6xl flex-col gap-4 px-4 py-10 sm:px-6">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <span className="font-heading text-sm font-medium">
            rocket-mem
          </span>
          <nav className="flex flex-wrap gap-6">
            {FOOTER_LINKS.map((link) => (
              <a
                key={link.href}
                href={link.href}
                target="_blank"
                rel="noreferrer"
                className="text-sm text-muted-foreground transition-colors hover:text-foreground"
              >
                {link.label}
              </a>
            ))}
          </nav>
        </div>
        <p className="text-xs text-muted-foreground">
          No automated failover, no live resharding — read the README's
          Limitations section before deploying.
        </p>
      </div>
    </footer>
  )
}
```

- [ ] **Step 2: Add `<SiteFooter />` to `App.tsx`**

```tsx
import { SiteNav } from "@/components/landing/site-nav"
import { SiteFooter } from "@/components/landing/site-footer"

function App() {
  return (
    <>
      <SiteNav />
      <main>{/* sections are appended here by later plans */}</main>
      <SiteFooter />
    </>
  )
}

export default App
```

- [ ] **Step 3: Verify typecheck, lint, and the visual result**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

Run: `bun run dev`, open `http://127.0.0.1:5174`.
Expected: below the (currently empty) main area, a footer with the "rocket-mem" mark, three links (GitHub, Getting started, License (MIT)) that open in a new tab, and the one-line Limitations caveat. Stop the dev server before continuing.

- [ ] **Step 4: Commit**

```bash
git add web/src/components/landing/site-footer.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Add SiteFooter to web/

GitHub/docs/license links plus a one-line honest nod to the
README's Limitations section (no automated failover, no live
resharding), so the landing page doesn't oversell past what the
README itself already discloses.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's `site-nav` and `site-footer` bullets in the Page structure section in full, including the mobile `Sheet` drawer requirement, and vendors `chart.tsx` (the last remaining item from Component sourcing's initial subset besides the section components themselves).
- **Placeholder scan:** none.
- **Type consistency:** `SiteNav`'s six anchor `href`s (`#why`, `#features`, `#architecture`, `#performance`, `#security`, `#quickstart`) are the fixed contract every later plan's section component must honor via its own `id` prop — restated in each of plans 4–7's task descriptions so no section drifts from this list. `SiteFooter` has no exported props or state, so nothing downstream can break by consuming it wrong.

## Next plan

[`2026-09-15-landing-page-4-hero-terminal.md`](2026-09-15-landing-page-4-hero-terminal.md) — the hero and terminal-demo sections (high design judgment; invoke the `frontend-design` skill first).
