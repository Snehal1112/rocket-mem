# rocket-mem Landing Page — Vendor UI & Theming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Vendor the subset of shadcn `base-luma` UI primitives the landing page needs, plus the mobile-breakpoint hook and the light/dark/system theme provider, and prove theming works end-to-end.

**Architecture:** Every `ui/*.tsx` file is a straight `cp` from `../../rocketvault/web/src/components/ui/` — no edits, since both projects share the same `@/lib/utils` and `@/components/ui` aliases (set up in plan 1). `theme-provider.tsx` is vendored the same way and wired into `main.tsx`; `App.tsx` gets a temporary sanity-check body (a theme-toggle button plus a `Card`) that proves the vendored primitives and the theme provider work together before any real landing-page content exists.

**Tech Stack:** Same as plan 1, plus `@base-ui/react` primitives (already a dependency), `lucide-react` icons.

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first (no `tailwind.config.js`), shadcn `base-luma` style on `@base-ui/react` (not Radix).
- Package manager: **Bun only** — `bun.lock` committed, no npm/yarn/pnpm lockfile.
- Component sourcing: vendor-copy from `../../rocketvault/web/src/components/...` via `cp`, byte-identical unless a task says otherwise — never fetch from a live shadcn registry.
- Fonts: `--font-sans` = Noto Sans Variable (body), `--font-heading` = JetBrains Mono Variable (headings) — already wired by `src/index.css`, do not override.
- Theming: light/dark/system via vendored `theme-provider.tsx`'s `useTheme()` — never hand-roll a separate theme mechanism.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated — do not touch `.github/workflows/ci.yml`.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks, never invented unit tests.
- All landing-page copy is static, hand-transcribed from `README.md` — no live data fetching.
- Dev server binds `127.0.0.1:5174`.
- **Working directory:** `cp`/`git` commands are relative to the repo root (`rocket-mem/`); every `bun run ...` command must be run from inside `web/` — `cd web` first, even where a step doesn't repeat that.

---

### Task 1: Vendor button, card, badge, separator, kbd

**Files:**
- Create: `web/src/components/ui/button.tsx`, `web/src/components/ui/card.tsx`, `web/src/components/ui/badge.tsx`, `web/src/components/ui/separator.tsx`, `web/src/components/ui/kbd.tsx` (all copied from `../../rocketvault/web/`)

**Interfaces:**
- Consumes: `cn()` from `@/lib/utils` (plan 1, Task 3).
- Produces: `Button` (`{ variant?: "default"|"outline"|"secondary"|"ghost"|"destructive"|"link", size?: "default"|"xs"|"sm"|"lg"|"icon"|"icon-xs"|"icon-sm"|"icon-lg", render?: React.ReactElement }`, plus standard button props — it's built on `@base-ui/react/button`, so to render it as a link use `render={<a href="..." />}`, not an `asChild` prop); `Card`/`CardHeader`/`CardTitle`/`CardDescription`/`CardAction`/`CardContent`/`CardFooter`; `Badge` (`variant?: "default"|"secondary"|"destructive"|"outline"|"ghost"|"link"`); `Separator`; `Kbd`/`KbdGroup`. Every landing-page section from plan 3 onward imports one or more of these.

**Suggested model:** haiku

- [ ] **Step 1: Copy the five files**

```bash
mkdir -p web/src/components/ui
cp ../../rocketvault/web/src/components/ui/button.tsx web/src/components/ui/button.tsx
cp ../../rocketvault/web/src/components/ui/card.tsx web/src/components/ui/card.tsx
cp ../../rocketvault/web/src/components/ui/badge.tsx web/src/components/ui/badge.tsx
cp ../../rocketvault/web/src/components/ui/separator.tsx web/src/components/ui/separator.tsx
cp ../../rocketvault/web/src/components/ui/kbd.tsx web/src/components/ui/kbd.tsx
```

No edits needed: each file imports `cn` from `@/lib/utils` and (for `button.tsx`, `separator.tsx`) primitives from `@base-ui/react/*`, both of which already resolve correctly in this project.

- [ ] **Step 2: Verify typecheck and lint**

Run: `bun run typecheck && bun run lint`
Expected: both clean. `biome.json`'s `src/components/ui/**` override (copied in plan 1, Task 2) exempts these files from the a11y/style rules that would otherwise fire on generated shadcn code — if lint fails here, check that override is present before debugging the component code itself.

- [ ] **Step 3: Commit**

```bash
git add web/src/components/ui/button.tsx web/src/components/ui/card.tsx web/src/components/ui/badge.tsx web/src/components/ui/separator.tsx web/src/components/ui/kbd.tsx
git commit -m "$(cat <<'EOF'
Vendor button, card, badge, separator, kbd into web/

Byte-identical copies from rocketvault/web's base-luma shadcn
registry; no edits needed, same @/lib/utils and @base-ui/react
aliases resolve in both projects.
EOF
)"
```

---

### Task 2: Vendor tabs, accordion, tooltip, sheet, and the mobile hook

**Files:**
- Create: `web/src/components/ui/tabs.tsx`, `web/src/components/ui/accordion.tsx`, `web/src/components/ui/tooltip.tsx`, `web/src/components/ui/sheet.tsx` (copied from `../../rocketvault/web/`)
- Create: `web/src/hooks/use-mobile.ts` (copied from `../../rocketvault/web/`)

**Interfaces:**
- Consumes: `cn()` from `@/lib/utils`; `sheet.tsx` also imports `Button` from `@/components/ui/button` (Task 1) and `XIcon` from `lucide-react`.
- Produces: `Tabs`/`TabsList`/`TabsTrigger`/`TabsContent` (plan 7's quickstart); `Accordion`/`AccordionItem`/`AccordionTrigger`/`AccordionContent` (plan 7's command-coverage); `Tooltip`/`TooltipTrigger`/`TooltipContent`/`TooltipProvider` (wired at the root in Task 3 below); `Sheet`/`SheetTrigger`/`SheetContent`/`SheetHeader`/`SheetTitle` (plan 3's mobile nav drawer — `SheetTrigger` and `SheetPrimitive.Close` accept a `render` prop the same way `Button` does, e.g. `<SheetTrigger render={<Button variant="outline" size="icon" />}>...</SheetTrigger>`); `useIsMobile(): boolean` from `use-mobile.ts` (available if a later plan needs a JS-side mobile check beyond CSS breakpoints — not required by any current plan, but vendored now since it's a `ui/`-adjacent shadcn dependency other primitives assume is present).

**Suggested model:** haiku

- [ ] **Step 1: Copy the four `ui/` files and the hook**

```bash
cp ../../rocketvault/web/src/components/ui/tabs.tsx web/src/components/ui/tabs.tsx
cp ../../rocketvault/web/src/components/ui/accordion.tsx web/src/components/ui/accordion.tsx
cp ../../rocketvault/web/src/components/ui/tooltip.tsx web/src/components/ui/tooltip.tsx
cp ../../rocketvault/web/src/components/ui/sheet.tsx web/src/components/ui/sheet.tsx
mkdir -p web/src/hooks
cp ../../rocketvault/web/src/hooks/use-mobile.ts web/src/hooks/use-mobile.ts
```

No edits needed.

- [ ] **Step 2: Verify typecheck and lint**

Run: `bun run typecheck && bun run lint`
Expected: both clean (the same `src/components/ui/**` and `src/hooks/use-mobile.ts` Biome overrides from plan 1 cover all of these).

- [ ] **Step 3: Commit**

```bash
git add web/src/components/ui/tabs.tsx web/src/components/ui/accordion.tsx web/src/components/ui/tooltip.tsx web/src/components/ui/sheet.tsx web/src/hooks/use-mobile.ts
git commit -m "$(cat <<'EOF'
Vendor tabs, accordion, tooltip, sheet, use-mobile into web/

Byte-identical copies from rocketvault/web; needed by the mobile
nav drawer (plan 3), command-coverage accordion and quickstart
tabs (plan 7).
EOF
)"
```

---

### Task 3: Vendor the theme provider and wire light/dark/system theming

**Files:**
- Create: `web/src/components/theme-provider.tsx` (copied from `../../rocketvault/web/`)
- Modify: `web/src/main.tsx` — wrap `<App />` in `<ThemeProvider><TooltipProvider>...</TooltipProvider></ThemeProvider>`
- Modify: `web/src/App.tsx` — replace the plan-1 placeholder body with a theme-toggle `Button` and a sanity-check `Card`

**Interfaces:**
- Consumes: `Button`, `Card`/`CardHeader`/`CardTitle`/`CardDescription`/`CardContent` (Task 1); `TooltipProvider` (Task 2).
- Produces: `ThemeProvider` (props: `{ children, defaultTheme?: "dark"|"light"|"system", storageKey?: string, disableTransitionOnChange?: boolean }`) and `useTheme(): { theme: "dark"|"light"|"system", setTheme: (theme) => void }` from `@/components/theme-provider` — plan 3's `site-nav.tsx` imports `useTheme` by this exact name for its theme-toggle control, and every later plan relies on `ThemeProvider` already being mounted above `<App />` in `main.tsx`.

**Suggested model:** sonnet

- [ ] **Step 1: Copy `theme-provider.tsx`**

```bash
cp ../../rocketvault/web/src/components/theme-provider.tsx web/src/components/theme-provider.tsx
```

No edits needed. It owns light/dark/system state, persists to `localStorage` under the key `"theme"`, syncs across browser tabs via the `storage` event, and binds a bare `d` keypress as a dark-mode toggle (ignored while an input/textarea/select/contenteditable element has focus).

- [ ] **Step 2: Wire `main.tsx`**

```tsx
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"

import "./index.css"
import App from "./App.tsx"
import { ThemeProvider } from "@/components/theme-provider.tsx"
import { TooltipProvider } from "@/components/ui/tooltip"

const rootElement = document.getElementById("root")
if (!rootElement) {
  throw new Error('Root element with id "root" was not found in index.html')
}

createRoot(rootElement).render(
  <StrictMode>
    <ThemeProvider>
      <TooltipProvider>
        <App />
      </TooltipProvider>
    </ThemeProvider>
  </StrictMode>
)
```

Same provider nesting rocketvault/web uses.

- [ ] **Step 3: Replace `App.tsx`'s body with a theme-toggle sanity check**

This is temporary — plan 3, Task 2 replaces it with the real `<SiteNav />`, and the theme-toggle logic moves there.

```tsx
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { useTheme } from "@/components/theme-provider"

const THEME_SEQUENCE = ["light", "dark", "system"] as const

function App() {
  const { theme, setTheme } = useTheme()

  const cycleTheme = () => {
    const currentIndex = THEME_SEQUENCE.indexOf(theme)
    const nextTheme = THEME_SEQUENCE[(currentIndex + 1) % THEME_SEQUENCE.length]
    setTheme(nextTheme)
  }

  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <Button variant="outline" onClick={cycleTheme}>
        Theme: {theme}
      </Button>
      <Card className="max-w-md">
        <CardHeader>
          <CardTitle>rocket-mem</CardTitle>
          <CardDescription>
            A Redis-compatible in-memory data store, written from scratch in
            Rust.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            Vendored UI primitives and theming are wired up. Later plans
            replace this sanity check with the real page sections.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}

export default App
```

- [ ] **Step 4: Verify typecheck and lint**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

- [ ] **Step 5: Verify theming manually**

Run: `bun run dev`, open `http://127.0.0.1:5174`.

Confirm all of the following, then stop the dev server:
- The button reads "Theme: system" (or "Theme: light"/"Theme: dark" if your OS/browser preference resolves differently) and the `Card` below it renders with visible border/shadow styling from the vendored `card.tsx`.
- Clicking the button cycles the label light → dark → system → light, and the page background/text colors repaint each time (dark mode is a visibly darker background with light text).
- With the page focused (not inside any input), pressing the bare `d` key also toggles dark mode.
- Reload the page after picking a non-default theme — the same theme is still active (confirms `localStorage` persistence under the `"theme"` key).

- [ ] **Step 6: Commit**

```bash
git add web/src/components/theme-provider.tsx web/src/main.tsx web/src/App.tsx
git commit -m "$(cat <<'EOF'
Wire light/dark/system theming into web/

theme-provider.tsx vendored unchanged from rocketvault/web;
main.tsx now wraps App in ThemeProvider + TooltipProvider.
App.tsx gets a temporary theme-toggle + Card sanity check,
replaced by the real site nav in the next plan.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's Theming subsection in full (vendored `theme-provider.tsx`, `main.tsx` provider nesting, light/dark/system) and the `ui/` half of Component sourcing for everything except `chart.tsx` and `scroll-area.tsx` (the former is vendored in plan 3 alongside the recharts dependency it needs; the latter is dropped from the vendored subset entirely — see the note below).
- **Placeholder scan:** none.
- **Type consistency:** `useTheme()`'s return shape (`{ theme, setTheme }`) and the `Theme` union (`"dark" | "light" | "system"`) are fixed by the vendored file and used identically here and in plan 3's `site-nav.tsx`. `Button`'s `render` prop (not `asChild`) is the correct Base UI polymorphism API — confirmed against `sheet.tsx`'s own use of `render={<Button .../>}` on `SheetPrimitive.Close` — and every later plan that renders a `Button` as a link uses this same pattern.
- **Judgment call:** the spec's suggested vendor subset includes `scroll-area`. No section in any plan needs a custom-styled scrollbar — the page just uses native document scroll — so `scroll-area.tsx` is dropped from the vendored set to avoid an unused file. If a future page revision needs a scrollable panel, vendor it then.

## Next plan

[`2026-09-15-landing-page-3-chart-nav-footer.md`](2026-09-15-landing-page-3-chart-nav-footer.md) — vendor the chart primitive, and build the site nav and footer that establish the page shell every later plan inserts sections into.
