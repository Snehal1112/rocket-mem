# QA playbook — design system

The visual language of [`docs/qa-playbook.html`](../qa-playbook.html), the interactive view of
the 135 cases in [`docs/qa-playbook.md`](../qa-playbook.md). Every value below is taken from that
file's stylesheet, not from memory — if the two disagree, the stylesheet is right and this
document is stale.

Read this before changing the page's appearance, or before building another page that should
look like it.

## The one idea

The page is **operated, not read**. A tester scans for the next untested case, runs commands out
of it, and records a verdict — so the craft goes into information design rather than typography
for its own sake. Two rules follow from that, and everything else is downstream of them:

1. **Summary before detail.** Counts, progress and per-suite state are visible without scrolling;
   the case bodies are collapsed until wanted.
2. **State is encoded in form, not only colour.** A passed case carries a green border, a green
   ID chip *and* the word "Passed". Colour is never the only carrier of meaning.

## Colour

Neutrals are biased faintly cyan, toward the accent, so they read as chosen rather than as
default grey. The ground is not pure white.

### Light (bare `:root`)

| Token | Value | Role |
|---|---|---|
| `--ground` | `#F6F8F9` | Page background |
| `--surface` | `#FFFFFF` | Cards, inputs, rail boxes |
| `--surface-2` | `#EDF1F3` | Code blocks, hover fills |
| `--surface-3` | `#E3EAED` | ID chips, the meter's empty track |
| `--ink` | `#101619` | Primary text, section rules |
| `--ink-2` | `#48585F` | Secondary text, body copy |
| `--ink-3` | `#78898F` | Micro-labels, placeholders, footer |
| `--line` | `#D6DFE3` | Hairlines, card borders |
| `--line-2` | `#C2CFD4` | Stronger borders: buttons, inputs |
| `--accent` | `#0B6E75` | Identity: eyebrow, links, active filter |
| `--accent-soft` | `#DFEFF0` | Expected-output block fill |
| `--accent-line` | `#8FC4C7` | Expected-output border, hover borders |
| `--pass` | `#2C7A44` | Passed state |
| `--pass-soft` | `#E3F1E7` | Passed ID-chip fill |
| `--fail` | `#B02128` | Failed state |
| `--fail-soft` | `#FAE6E7` | Failed ID-chip fill |
| `--gap` | `#9A5A08` | Known-defect callout |
| `--gap-soft` | `#FAEEDC` | Known-defect callout fill |

### Dark

Redefined identically in two places — see "Theming". Same roles, retuned for a dark ground:

| Token | Dark value | | Token | Dark value |
|---|---|---|---|---|
| `--ground` | `#0E1418` | | `--accent` | `#41C0C8` |
| `--surface` | `#151D23` | | `--accent-soft` | `#0E2E31` |
| `--surface-2` | `#1B252C` | | `--accent-line` | `#1E5B60` |
| `--surface-3` | `#233038` | | `--pass` | `#63C384` |
| `--ink` | `#E4ECF0` | | `--pass-soft` | `#14291C` |
| `--ink-2` | `#9EB1BB` | | `--fail` | `#F2818A` |
| `--ink-3` | `#6C808A` | | `--fail-soft` | `#2E1517` |
| `--line` | `#25323A` | | `--gap` | `#DFA34A` |
| `--line-2` | `#33444E` | | `--gap-soft` | `#2B2011` |

### The rule that matters

**Semantic colour is separate from the accent.** `--pass`, `--fail` and `--gap` mean *result*;
`--accent` means *identity and interactivity*. Never express a verdict in the accent, and never
use a semantic colour for a link or an active filter — the moment those overlap, a teal chip
becomes ambiguous between "this is interactive" and "this is a state".

## Theming

The viewer has **three** states, not two: an explicit choice stamps `data-theme` on the root
element, and the default "system" setting stamps nothing. The stylesheet handles all three:

```
:root { … }                                                  /* complete light palette */
@media (prefers-color-scheme:dark){
  :root:not([data-theme="light"]) { … }                      /* dark OS, unless light is stamped */
}
:root[data-theme="dark"] { … }                               /* explicit dark wins either way */
```

Two constraints follow, and breaking either produces the classic unreadable page:

- **Every colour is defined on bare `:root` first.** A colour whose only definition sits inside
  the media query or a `[data-theme]` block never applies in the un-stamped state.
- **Components read tokens, never literals.** `body` sets `background:var(--ground)` explicitly;
  a transparent body borrows whatever ground the host paints behind it.

## Typography

Three roles from one family — IBM Plex, drawn for technical documentation, which is what this is.
Each has a real fallback stack, so the page degrades to system faces offline.

| Role | Stack | Used for |
|---|---|---|
| `--sans` | IBM Plex Sans → `ui-sans-serif`, `system-ui`, … | All UI and prose |
| `--mono` | IBM Plex Mono → `ui-monospace`, `SFMono-Regular`, … | Every command, output, case ID, count, chip |
| `--serif` | IBM Plex Serif *italic* → Georgia, … | The masthead lede only |

The serif appears exactly once, in the sub-headline. It is a deliberate single note of contrast
against an otherwise sans/mono page — using it anywhere else dilutes it to decoration.

**Scale**, in px: `10.5 · 11 · 11.5 · 12 · 12.5 · 13 · 13.5 · 14.5 · 15 · 19 · 23`. Body is 15px
at `line-height:1.5`; code is 12.5px at `1.62`, which keeps dense terminal output readable.

**The micro-label convention.** Field labels (`PRECONDITION`, `STEPS`, `EXPECTED OUTPUT`),
eyebrows and scoreboard captions are all: mono, 10.5–11px, uppercase, `--ink-3`, weight 600,
letter-spacing `.09em`–`.13em`. Wide tracking is what makes small uppercase legible; without it
they read as noise. Anything that labels rather than informs should look like this.

Numbers that stack in columns use `font-variant-numeric: tabular-nums` so digits align.

## Layout

A **two-pane operator console**, capped at `1240px`:

```
┌──────────────────────────────────────────────────────────┐
│ masthead: identity + lede + build tags │ scoreboard      │
│                                        │ progress meter  │  ← flex, ends aligned
├───────────────┬──────────────────────────────────────────┤
│ rail (236px)  │ main                                     │
│ sticky, 16px  │  ├ known-defects callout                 │
│  ├ suite nav  │  ├ search + filter chips + expand        │
│  └ run box    │  └ sections → case cards                 │
└───────────────┴──────────────────────────────────────────┘
```

- `.shell` — `grid-template-columns: 236px minmax(0,1fr)`, `gap:30px`, `align-items:start`.
  The `minmax(0,1fr)` matters: without the `0` floor, wide `<pre>` content forces the column
  wider than the viewport.
- `.rail` — `position:sticky; top:16px`, so counts stay visible through a long scroll.
- **One breakpoint, `max-width:900px`** — the grid collapses to a single column and the rail
  un-sticks. There is no tablet-specific tier; the design has one shape and one fallback.
- Wide content scrolls inside its own container (`pre { overflow-x:auto }`), never the body.

## Components

| Component | Spec | Notes |
|---|---|---|
| `.scoreboard` | `repeat(4, minmax(0,1fr))`, 1px gaps over `--line`, radius 5px | Fixed four columns on purpose. `auto-fit` wrapped 4 tiles into 3+1 and left an empty cell. |
| `.meter` | 5px tall, pass and fail segments over `--surface-3` | Proportional bar, no label — the scoreboard carries the numbers. |
| `.case` | `--surface`, 1px `--line`, radius 6px, `--shadow`, 9px gap | Border colour switches to `--pass` / `--fail` on verdict. |
| `.cid` | mono 11.5px, 600, radius 3px, `--surface-3` | Tinted `--pass-soft` / `--fail-soft` once judged. |
| `.sec-head` | `border-bottom:2px solid var(--ink)` | The only 2px ink rule on the page — it is what separates suites. |
| `pre` | mono 12.5px/1.62, `--surface-2`, radius 5px, `tab-size:2` | Expected output overrides to `--accent-soft` + `--accent-line`, so steps and expected never blur together. |
| `.chip` | mono 12px, radius 5px, `aria-pressed` | Filters. Active state fills with `--accent`. |
| `.vb` | mono 12px, 600, radius 5px | Pass / Fail / Clear. Fills with its semantic colour when pressed. |
| `.copy` | absolute, `opacity:0` until `.codewrap:hover` or `:focus-visible` | Hidden affordance that must stay keyboard-reachable — hence the focus condition. |
| `.alert` | `--gap` border with 3px left edge, `--gap-soft` fill | Known-defects callout. The only amber on the page. |

## Interaction

- **Verdicts persist per viewer** in `localStorage` under `rocketmem-qa-v1`. Every read and write
  is wrapped in `try`/`catch` — private windows and blocked site data throw on access, and the
  page must still render.
- **Failed cases auto-expand** on render. A failure is the thing you need to look at.
- **Search** filters on ID, title, precondition, steps, expected and notes, debounced 130ms.
- **Re-render preserves the open card.** Recording a verdict rebuilds the list; the handler
  re-opens the card that was just judged, so the page does not collapse under the cursor.

## Accessibility

- Focus is visible everywhere: `2px solid var(--accent)` at `2px` offset.
- `prefers-reduced-motion: reduce` disables all transitions and animations.
- Filters use `aria-pressed`; case headers use `aria-expanded`; the case list is real buttons,
  not click-handled `div`s.
- Verdicts are never colour-only — border, chip tint and a text label all change together.

## Extending it

1. Add a token before adding a colour. A literal hex in a component rule will break one theme.
2. Define the token on bare `:root` first, then in **both** dark blocks.
3. Decide whether the thing is *state* or *identity*, and take the colour from the matching
   family. Do not reach across.
4. Label-like text gets the micro-label treatment; data gets mono; prose gets sans.
5. Check both themes before committing. The fastest check:

   ```bash
   google-chrome --headless --disable-gpu --hide-scrollbars \
     --virtual-time-budget=4000 --window-size=1280,900 \
     --screenshot=/tmp/qa.png "file://$PWD/docs/qa-playbook.html"
   ```

   Force a theme by temporarily replacing the `localStorage.getItem('rm-theme') || 'system'`
   fallback — note the assignment on the line *above* it is overwritten, so patching that one
   does nothing.

## Constraints worth knowing

- **Webfonts are fetched from Google Fonts.** Offline, the page falls back to the system stacks
  and still works. `docs/manual.html` deliberately takes the opposite position and ships no
  webfont at all, so the two documents are inconsistent on this point by choice, not accident.
- **The page is generated, not hand-edited.** Cases are parsed out of `docs/qa-playbook.md` into
  an embedded JSON block. Editing case text in the HTML will be overwritten on the next
  regeneration — edit the markdown.
- **The repo copy carries its own document wrapper** (doctype, charset, viewport, `body{margin:0}`)
  because it is opened as a file. The published-artifact copy must *not* have those; the host
  supplies them.
