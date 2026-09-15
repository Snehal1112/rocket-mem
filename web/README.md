# rocket-mem landing page

A standalone marketing/showcase landing page for
[rocket-mem](https://github.com/Snehal1112/rocket-mem), built with React 19,
Vite, Tailwind CSS v4, and a vendored subset of
[rocketvault/web](../../rocketvault/web)'s `base-luma` shadcn/ui components.

This folder is **not** part of the Cargo workspace and is **not** CI-gated —
the same relationship `examples/` and `rocket-mem-mcp/` have to the rest of
the repository. See the root [`CLAUDE.md`](../CLAUDE.md) for that convention.

## Running it

```bash
bun install
bun run dev       # dev server at http://127.0.0.1:5174
```

## Building it

```bash
bun run build      # tsc -b && vite build, output in dist/
bun run preview      # serve the production build locally
```

## Other scripts

```bash
bun run typecheck   # tsc -b --noEmit
bun run lint         # biome lint --error-on-warnings
bun run lint:fix      # biome lint --write --error-on-warnings
bun run format         # prettier --write "**/*.{ts,tsx}"
```

## Design doc

The full design spec this page implements lives at
[`../docs/superpowers/specs/2026-09-15-landing-page-design.md`](../docs/superpowers/specs/2026-09-15-landing-page-design.md).
