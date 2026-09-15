# rocket-mem landing page — design spec

**Date:** 2026-09-15
**Status:** approved, pending implementation plan

## Purpose

A standalone marketing/showcase landing page for rocket-mem, living in a new
`web/` folder at the repo root (sibling to `examples/` and `rocket-mem-mcp/`).
It showcases rocket-mem's feature set, architecture, and Redis-comparison
performance numbers to a developer audience evaluating it as a Redis
alternative. All content is static, hand-transcribed from `README.md` — no
backend, no live data fetching, no build-time README parsing.

## Scope boundary

`web/` is **not** part of the Cargo workspace and **not** CI-gated, matching
the existing relationship `examples/` and `rocket-mem-mcp/` already have to
the rest of the repo (see root `CLAUDE.md`). No changes to
`.github/workflows/ci.yml`. No test runner is wired up, matching the current
state of the component source project (rocketvault/web).

## Stack & tooling

Mirrors `../rocketvault/web` exactly, since that project is the shadcn
component source and this keeps components portable between the two with
minimal adaptation:

- **React 19 + TypeScript (strict)** on **Vite 8**.
- **Tailwind CSS v4, CSS-first** — design tokens in `src/index.css` under
  `:root` / `.dark`, exposed to Tailwind via `@theme inline`. No
  `tailwind.config.js`.
- **shadcn/ui, `base-luma` style, built on `@base-ui/react`** — not Radix.
  `cva` for variants, `cn()` from `@/lib/utils` to merge classes, a
  `data-slot` attribute on each root element. Icons from `lucide-react`.
- **Bun** as package manager — `bun.lock` committed, no npm/yarn/pnpm
  lockfile. (This overrides MY.md's default Yarn-for-frontend preference,
  deliberately, to stay consistent with the component source.)
- **Biome** for linting (`--error-on-warnings`), **Prettier** for formatting
  (`prettier-plugin-tailwindcss` sorts Tailwind classes including inside
  `cn()`/`cva()`). Same division of labor as rocketvault/web: Biome's
  formatter and organize-imports assist stay disabled.
- Config files copied and adapted from rocketvault/web: `tsconfig.json`,
  `tsconfig.app.json`, `tsconfig.node.json`, `vite.config.ts`, `biome.json`,
  `.prettierrc`, `.prettierignore`, `components.json` — with
  `components.json`'s `registries.@rocketvault` entry removed, since this
  project vendor-copies components rather than depending on rocketvault's
  dev server being live.

### Fonts

Same pairing as rocketvault/web:

- `@fontsource-variable/noto-sans` → `--font-sans`, body text.
- `@fontsource-variable/jetbrains-mono` → `--font-heading`, headings — fits
  rocket-mem particularly well given the product itself is a terminal/wire
  protocol tool.

### Theming

Vendor `components/theme-provider.tsx` from rocketvault/web as-is: owns
light/dark/system state, persists to `localStorage` under `theme`, syncs
across tabs via the `storage` event, binds a bare `d` keypress as a
dark-mode toggle (ignored inside inputs/textareas/selects/contenteditable).
`main.tsx` wraps `<App />` in `ThemeProvider` then `TooltipProvider`, same
nesting as the source project. The site nav includes a theme toggle control
that cycles light → dark → system.

## Component sourcing

Vendor-copy (not live shadcn-registry-fetch) only the subset of
`rocketvault/web/src/components/ui/*.tsx` the landing page actually uses,
plus `lib/utils.ts`, `hooks/use-mobile.ts`, and `components/theme-provider.tsx`.
This keeps `web/` fully self-contained with no runtime or setup-time
dependency on rocketvault's dev server (its `components.json` registry entry
points at `http://localhost:5173/app/r/{name}.json`, which won't generally be
running).

Initial subset (confirmed/adjusted during implementation as sections are
built): `button`, `badge`, `card`, `separator`, `tabs`, `accordion`, `sheet`
(mobile nav drawer), `tooltip`, `scroll-area`, `chart` (recharts wrapper),
`kbd`.

Copied components stay byte-identical to the registry versions unless a
change is required, same convention rocketvault/web itself follows for its
vendored copies.

## Page structure

One scrolling single-page site, section components under
`src/components/landing/`, mirroring rocketvault/web's own
`components/landing/*` naming pattern (that project already solved this
exact "vendored shadcn + Tailwind + section composition" problem for its own
landing page):

- **`site-nav`** — anchor links (Features / Architecture / Performance /
  Security / Quick Start), theme toggle, GitHub link button. Mobile: `sheet`
  drawer.
- **`hero`** — headline, subheadline that leads with the drop-in-compatible
  message (see below), primary CTAs (Docker one-liner in a code block,
  GitHub link).
- **`why`** (value props) — 3-4 cards: drop-in Redis compatibility, dual
  RESP+RMP protocol, multi-threaded unlike Redis, durability.
  - The **compatibility card is explicit and prominent**: "Point your
    existing Redis client at rocket-mem — no code changes." Names concrete
    client libraries from the README (`redis-cli`, `redis-py`, `ioredis`,
    `go-redis`) and states RESP2 **and** RESP3 support with full `HELLO`
    version negotiation.
- **`terminal-demo`** — static styled terminal showing a `redis-cli`
  session (`SET`/`GET`), reinforcing the compatibility message visually.
- **`features`** — grid from README's Features section (wire compatibility,
  RMP, data types, durability, replication, clustering, security,
  observability).
- **`architecture`** — the three-layer diagram (Protocol → Command
  Dispatcher → Storage Engine) rebuilt as a real component (not an image),
  plus the 16-shard concurrency note.
- **`performance`** — leads with a headline stat callout ("0.86x–1.23x of
  Redis, faster on 3 of 8 workloads") above a recharts bar chart comparing
  rocket-mem vs redis-server across the SET/GET workload table from
  README's Performance section. Data is a static array hand-transcribed
  from the README (dated 2026-09-08 there — carry that date/caveat here too
  rather than presenting it as always-current).
- **`security-observability`** — cards for Argon2 password hashing,
  per-user ACLs, optional TLS, Prometheus `/metrics`, bounded slow log.
- **`command-coverage`** — accordion over command categories (strings,
  hashes, lists, sets, sorted sets, keys) from README's Command coverage
  section.
- **`quickstart`** — Docker / binary-download / build-from-source in tabs,
  each with a copyable code block, sourced from README's Quick start
  section.
- **`site-footer`** — GitHub/docs/license links, and a short honest nod to
  README's Limitations section (no automated failover, no live
  resharding) — the README itself leads with this caveat, so the landing
  page shouldn't oversell past it.

## Folder layout

```
rocket-mem/web/
  src/
    components/
      landing/        # site-nav, hero, why, terminal-demo, features,
                       # architecture, performance, security-observability,
                       # command-coverage, quickstart, site-footer
      ui/              # vendored shadcn subset (see above)
      theme-provider.tsx
    lib/utils.ts
    hooks/use-mobile.ts
    assets/
    index.css
    App.tsx
    main.tsx
  index.html
  package.json
  bun.lock
  vite.config.ts
  tsconfig.json / tsconfig.app.json / tsconfig.node.json
  components.json
  biome.json
  .prettierrc / .prettierignore
  README.md           # explains this is a standalone, non-CI-gated scaffold
                       # like examples/ and rocket-mem-mcp/, and how to run it
```

## Non-goals

- No CI wiring, no test runner.
- No live dependency on rocketvault/web's dev server or shadcn registry.
- No backend, no data fetching, no CMS — all copy is static and hand-written
  once from `README.md`.
- No multi-page routing (single scrolling page with anchors).

## Verification

"Done" means, from `rocket-mem/web/`:

```bash
bun run typecheck   # tsc -b --noEmit
bun run lint         # biome lint --error-on-warnings
bun run build         # tsc -b && vite build
```

all clean, plus a manual visual check via `bun run dev` in a browser: light
mode, dark mode, system mode, and mobile width (~400px, nav collapses to the
`sheet` drawer).

## Implementation planning guidance

Per this project's standing plan convention, implementation proceeds as
chained plans of at most 3 tasks each (`docs/superpowers/plans/2026-09-15-landing-page-plans/`),
each referencing this spec via a relative `../../specs/...` path. Suggested
split, roughly in complexity order:

1. **Scaffold** — config files, tooling, dependencies, vendored `ui/`
   components + `lib/utils.ts` + `hooks/use-mobile.ts` +
   `theme-provider.tsx`, base `index.css` tokens/fonts, `main.tsx` provider
   wiring. Mechanical — low design judgment.
2. **Structural sections** — `site-nav`, `site-footer`, `quickstart`,
   `command-coverage`, `features`, `security-observability`, `why`. Mostly
   composition of already-vendored primitives.
3. **High-design-judgment sections** — `hero`, `architecture` diagram,
   `performance` chart, `terminal-demo`. Invoke the `frontend-design` skill
   for these; they carry the most visual/taste weight on the page.
4. **Integration** — assemble `App.tsx`, verify typecheck/lint/build,
   manual browser QA (themes + mobile width).
