# rocket-mem Landing Page — Scaffold & Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a bare, correctly-configured Vite + React 19 + TypeScript + Tailwind v4 project at `rocket-mem/web/`, with a minimal page that proves the toolchain (dev server, lint, typecheck) and the font pairing all work.

**Architecture:** A fresh Bun-managed Vite project, config mirrored from `../rocketvault/web` (the shadcn component source project) with rocket-mem-specific edits where the two projects genuinely differ (package name, dev server port, page metadata — never the shared toolchain conventions). No landing-page content yet; that starts in plan 3.

**Tech Stack:** React 19 + TypeScript (strict) on Vite 8, Tailwind CSS v4 (CSS-first, no `tailwind.config.js`), Bun package manager, Biome (lint) + Prettier (format).

**Spec:** [`../specs/2026-09-15-landing-page-design.md`](../specs/2026-09-15-landing-page-design.md)

**Continuation:** Pre-authorized — on completing this plan's tasks and verification, proceed directly into the next plan below without waiting for user confirmation (per user instruction, 2026-09-15).

## Global Constraints

- Stack: React 19 + TypeScript strict on Vite 8, Tailwind v4 CSS-first (no `tailwind.config.js`), shadcn `base-luma` style on `@base-ui/react` (not Radix).
- Package manager: **Bun only** — `bun.lock` committed, no npm/yarn/pnpm lockfile.
- Component sourcing (from plan 2 onward): vendor-copy from `../../rocketvault/web/src/components/...` via `cp`, byte-identical unless a task says otherwise — never fetch from a live shadcn registry.
- Fonts: `--font-sans` = Noto Sans Variable (body), `--font-heading` = JetBrains Mono Variable (headings) — wired by `src/index.css` in this plan; never override with a different font stack.
- Theming (from plan 2 onward): light/dark/system via vendored `theme-provider.tsx`'s `useTheme()` — never hand-roll a separate theme mechanism.
- `web/` is **not** part of the Cargo workspace and **not** CI-gated — do not touch `.github/workflows/ci.yml`.
- No test runner — verification is `bun run typecheck` / `bun run lint` / `bun run build` plus manual `bun run dev` checks, never invented unit tests.
- All landing-page copy is static, hand-transcribed from `README.md` — no live data fetching.
- Dev server binds `127.0.0.1:5174` (not rocketvault/web's `5173`, so both dev servers can run at once without colliding).
- **Working directory:** every `cp` and `git` command in this plan is written relative to the repo root (`rocket-mem/`, which is also where `../../rocketvault/web/...` source paths resolve from). Every `bun run ...` / `bun install` / `bun add` command must be run from inside `web/` — `cd web` first — even where a step just says "Run: `bun run ...`" without repeating that.

---

### Task 1: Package manifest, TS config, Vite config, and a minimal entry point

**Files:**
- Create: `web/package.json`
- Create: `web/.gitignore`
- Create: `web/tsconfig.json`, `web/tsconfig.app.json`, `web/tsconfig.node.json` (copied from `../../rocketvault/web/`)
- Create: `web/vite.config.ts`
- Create: `web/index.html`
- Create: `web/src/main.tsx`

**Interfaces:**
- Consumes: nothing (first task in the chain).
- Produces: a running Vite dev server at `http://127.0.0.1:5174`; the `@/*` → `web/src/*` path alias (used by every later task's imports); `bun run dev|build|typecheck|lint|lint:fix|format|preview` scripts every later task's verification steps rely on.

**Suggested model:** haiku

- [ ] **Step 1: Create `web/package.json`**

```json
{
  "name": "rocket-mem-web",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "lint": "biome lint --error-on-warnings",
    "lint:fix": "biome lint --write --error-on-warnings",
    "format": "prettier --write \"**/*.{ts,tsx}\"",
    "typecheck": "tsc -b --noEmit",
    "preview": "vite preview"
  },
  "dependencies": {
    "@base-ui/react": "^1.7.0",
    "@fontsource-variable/jetbrains-mono": "^5.3.0",
    "@fontsource-variable/noto-sans": "^5.3.0",
    "@tailwindcss/vite": "^4.3.3",
    "class-variance-authority": "^0.7.1",
    "clsx": "^2.1.1",
    "lucide-react": "^1.39.0",
    "react": "^19.2.6",
    "react-dom": "^19.2.6",
    "recharts": "3.8.0",
    "tailwind-merge": "^3.6.0",
    "tailwindcss": "^4.3.3",
    "tw-animate-css": "^1.4.0"
  },
  "devDependencies": {
    "@biomejs/biome": "^2.5.11",
    "@types/node": "^24",
    "@types/react": "^19",
    "@types/react-dom": "^19",
    "@vitejs/plugin-react": "^6",
    "prettier": "^3.8.3",
    "prettier-plugin-tailwindcss": "^0.8.0",
    "typescript": "~6",
    "vite": "^8"
  }
}
```

This dependency list is deliberately smaller than `rocketvault/web`'s: it drops `@shadcn/helpers`, `shadcn`, and `styled-components` (this project doesn't do live shadcn-registry builds or styled-components), and drops `cmdk`, `date-fns`, `embla-carousel-react`, `input-otp`, `react-day-picker`, `react-resizable-panels` (those are dependencies of shadcn components — `command`, `calendar`, `carousel`, `input-otp`, `resizable` — that this landing page never vendors; see the spec's Component sourcing section for the actual subset used). `recharts` is included now, up front, rather than added later in plan 3, since we already know the performance chart (plan 6) needs it — one `bun install` instead of a redundant `bun add` mid-project.

- [ ] **Step 2: Create `web/.gitignore`**

```
node_modules/
node_modules/.tmp/
dist/
```

- [ ] **Step 3: `bun install`**

Run from `web/`: `bun install`

Expected: creates `bun.lock` and `node_modules/`, no errors. This is the package manager lockfile this project commits (per Global Constraints — no npm/yarn/pnpm lockfile).

- [ ] **Step 4: Copy the three TypeScript project-reference configs verbatim**

```bash
cp ../../rocketvault/web/tsconfig.json web/tsconfig.json
cp ../../rocketvault/web/tsconfig.app.json web/tsconfig.app.json
cp ../../rocketvault/web/tsconfig.node.json web/tsconfig.node.json
```

These need zero edits: same `@/*` → `./src/*` path alias, same `es2023`/strict/bundler-mode settings, same `noUnusedLocals`/`noUnusedParameters`/`verbatimModuleSyntax`/`erasableSyntaxOnly` flags this project also wants.

- [ ] **Step 5: Create `web/vite.config.ts`**

rocketvault/web's `vite.config.ts` has two things this project must NOT copy: a Caddy-specific `base: "/app/"` (rocket-mem's landing page isn't served behind that reverse proxy) and a `wss`/`numericlabs.lxd` HMR override (same reason). Everything else — the plugin list and the `@` alias — carries over unchanged, with the port set to `5174`:

```typescript
import path from "node:path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
})
```

- [ ] **Step 6: Create `web/index.html`**

New title/description for rocket-mem (not RocketVault's), and no favicon link — this project has no icon asset yet and a design-judgment favicon isn't worth a task of its own:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>rocket-mem — a Redis-compatible store, from scratch in Rust</title>
    <meta
      name="description"
      content="rocket-mem speaks RESP2, RESP3, and its own multiplexing protocol RMP. Point redis-cli, redis-py, ioredis, or go-redis at it — no code changes."
    />
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 7: Create a minimal `web/src/main.tsx`**

No `App.tsx`, no CSS yet — this step only proves Vite + React + the TS config actually run. Later tasks replace this.

```tsx
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"

const rootElement = document.getElementById("root")
if (!rootElement) {
  throw new Error('Root element with id "root" was not found in index.html')
}

createRoot(rootElement).render(
  <StrictMode>
    <div>rocket-mem</div>
  </StrictMode>
)
```

- [ ] **Step 8: Verify the dev server**

Run: `bun run dev`
Expected: Vite starts and serves on `http://127.0.0.1:5174`; opening it in a browser shows unstyled text "rocket-mem". Stop the dev server (Ctrl-C) before continuing.

- [ ] **Step 9: Commit**

```bash
git add web/package.json web/bun.lock web/.gitignore web/tsconfig.json web/tsconfig.app.json web/tsconfig.node.json web/vite.config.ts web/index.html web/src/main.tsx
git commit -m "$(cat <<'EOF'
Scaffold web/: Vite + React 19 + TS project skeleton

Bare Bun-managed Vite project for the rocket-mem landing page,
config adapted from rocketvault/web (the shadcn component source)
minus its Caddy-specific base path and HMR override. No page
content yet — this proves the toolchain runs.
EOF
)"
```

---

### Task 2: Lint, format, and shadcn tooling config

**Files:**
- Create: `web/biome.json`, `web/.prettierrc`, `web/.prettierignore` (copied from `../../rocketvault/web/`)
- Create: `web/components.json`

**Interfaces:**
- Consumes: `web/src/main.tsx` and the TS configs from Task 1 (what gets linted/typechecked).
- Produces: `bun run lint` / `bun run format` working config every later task's verification steps run; `components.json`'s `aliases` (`@/components`, `@/components/ui`, `@/lib`, `@/hooks`) that plan 2's vendored shadcn components assume.

**Suggested model:** haiku

- [ ] **Step 1: Copy Biome and Prettier config verbatim**

```bash
cp ../../rocketvault/web/biome.json web/biome.json
cp ../../rocketvault/web/.prettierrc web/.prettierrc
cp ../../rocketvault/web/.prettierignore web/.prettierignore
```

These need zero edits: same `src/components/ui/**` and `src/hooks/use-mobile.ts` linter-exemption overrides this project's vendored components also need (see the spec's Component sourcing section — this project vendors the exact same kind of generated shadcn code rocketvault/web does), same Prettier settings (no semicolons, double quotes, `prettier-plugin-tailwindcss` sorting inside `cn()`/`cva()`), same `src/components/ui/` Prettier-ignore so vendored files stay byte-identical to their source.

- [ ] **Step 2: Create `web/components.json`**

Same as rocketvault/web's, minus the `registries` key — this project vendor-copies components via `cp` (Task 4 of plan 2 onward) rather than depending on rocketvault/web's dev server being live at `http://localhost:5173/app/r/{name}.json`:

```json
{
  "$schema": "https://ui.shadcn.com/schema.json",
  "style": "base-luma",
  "rsc": false,
  "tsx": true,
  "tailwind": {
    "config": "",
    "css": "src/index.css",
    "baseColor": "mist",
    "cssVariables": true,
    "prefix": ""
  },
  "iconLibrary": "lucide",
  "rtl": false,
  "aliases": {
    "components": "@/components",
    "utils": "@/lib/utils",
    "ui": "@/components/ui",
    "lib": "@/lib",
    "hooks": "@/hooks"
  },
  "menuColor": "default-translucent",
  "menuAccent": "subtle"
}
```

- [ ] **Step 3: Verify lint and typecheck**

Run: `bun run lint`
Expected: clean, 0 errors (only `src/main.tsx` exists so far, and it's plain, valid TypeScript/JSX).

Run: `bun run typecheck`
Expected: clean, 0 errors.

- [ ] **Step 4: Commit**

```bash
git add web/biome.json web/.prettierrc web/.prettierignore web/components.json
git commit -m "$(cat <<'EOF'
Add lint, format, and shadcn tooling config to web/

Biome/Prettier config copied from rocketvault/web unchanged (same
vendored-component lint exemptions this project will also need);
components.json drops the registries key since this project
vendor-copies shadcn components rather than fetching them live.
EOF
)"
```

---

### Task 3: Design tokens, fonts, utils, and a real `App.tsx`

**Files:**
- Create: `web/src/index.css` (copied from `../../rocketvault/web/`)
- Create: `web/src/lib/utils.ts` (copied from `../../rocketvault/web/`)
- Create: `web/src/App.tsx`
- Modify: `web/src/main.tsx` — import `./index.css`, render `<App />` instead of the placeholder `<div>`

**Interfaces:**
- Consumes: `cn()` is imported by name from `@/lib/utils` — every vendored `ui/*` component in plan 2 imports it the same way, so this file's export shape (`export function cn(...inputs: ClassValue[])`) must not change.
- Produces: the `font-heading` (JetBrains Mono Variable) / `font-sans` (Noto Sans Variable, the `html` default) Tailwind utility classes and the full `base-luma` color token set (`--background`, `--foreground`, `--primary`, `--card`, etc., light and dark) every later component class name in this project assumes exist. `App.tsx` as the component `main.tsx` renders — every plan from 2 onward edits this same file.

**Suggested model:** sonnet (first real visual composition)

- [ ] **Step 1: Copy `index.css` and `lib/utils.ts` verbatim**

```bash
cp ../../rocketvault/web/src/index.css web/src/index.css
mkdir -p web/src/lib
cp ../../rocketvault/web/src/lib/utils.ts web/src/lib/utils.ts
```

`index.css` already imports both fonts (`@import "@fontsource-variable/noto-sans"` and `@import "@fontsource-variable/jetbrains-mono"`) and defines `--font-sans`/`--font-heading` under `@theme inline` — this one file satisfies the whole font-pairing requirement with no edits. It also carries a `.terminal-line` fade-in keyframe rocketvault/web's own hero uses; this project's `terminal-demo` (plan 4) is static per the spec, so that class is unused here and that's fine — leaving an unused CSS class in a copied stylesheet is not worth trimming by hand and risking a token diff.

- [ ] **Step 2: Create `web/src/App.tsx`**

A real (not throwaway) component: headline in the heading font, body text in the default sans font, so the pairing is visually checkable. Later plans replace this body with the actual page sections.

```tsx
function App() {
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-8">
      <h1 className="font-heading text-3xl font-semibold">rocket-mem</h1>
      <p className="max-w-prose text-center text-muted-foreground">
        A Redis-compatible in-memory data store, written from scratch in Rust.
      </p>
    </div>
  )
}

export default App
```

- [ ] **Step 3: Rewrite `web/src/main.tsx`**

```tsx
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"

import "./index.css"
import App from "./App.tsx"

const rootElement = document.getElementById("root")
if (!rootElement) {
  throw new Error('Root element with id "root" was not found in index.html')
}

createRoot(rootElement).render(
  <StrictMode>
    <App />
  </StrictMode>
)
```

- [ ] **Step 4: Verify typecheck, lint, and the visual result**

Run: `bun run typecheck && bun run lint`
Expected: both clean.

Run: `bun run dev`, open `http://127.0.0.1:5174`.
Expected: "rocket-mem" heading rendered in a monospace face (JetBrains Mono Variable) visually distinct from the paragraph below it, which renders in a humanist sans face (Noto Sans Variable) — compare the two side by side to confirm they're not the same font. Background is a plain light background (no dark mode wiring yet — that's plan 2). Stop the dev server before continuing.

- [ ] **Step 5: Commit**

```bash
git add web/src/index.css web/src/lib/utils.ts web/src/App.tsx web/src/main.tsx
git commit -m "$(cat <<'EOF'
Wire up design tokens, fonts, and a real App.tsx in web/

index.css and lib/utils.ts vendored unchanged from rocketvault/web
(base-luma tokens, Noto Sans / JetBrains Mono font imports already
wired). App.tsx now renders real heading/body text so the font
pairing is visually verifiable.
EOF
)"
```

## Self-Review

- **Spec coverage:** implements the spec's Stack & tooling section (React 19 + TS + Vite 8 + Tailwind v4 + Bun + Biome/Prettier), the Fonts subsection (Noto Sans / JetBrains Mono, via the copied `index.css`), and the `package.json`/`tsconfig*`/`vite.config.ts`/`components.json`/`biome.json`/`.prettierrc`/`.prettierignore` half of the Folder layout section. Theming, vendored `ui/` components, and all landing-page content are out of scope for this plan — they're plans 2 onward.
- **Placeholder scan:** none — every step has real, complete file content or a real command.
- **Type consistency:** `cn()`'s signature (`(...inputs: ClassValue[]) => string`) and the `@/*` alias are the only interfaces this plan produces for later plans to consume; both are copied verbatim from rocketvault/web, so they match every vendored component in plan 2 without edits.

## Next plan

[`2026-09-15-landing-page-2-vendor-ui-theming.md`](2026-09-15-landing-page-2-vendor-ui-theming.md) — vendor the shadcn `ui/` component subset and wire up light/dark/system theming.
