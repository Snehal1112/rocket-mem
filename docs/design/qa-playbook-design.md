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
for its own sake. Three rules follow from that, and everything else is downstream of them:

1. **The run is the hero.** Where the run stands — how far through, and where the failures sit —
   is the first thing on the page, at full width, before any prose.
2. **Summary before detail.** Counts, progress and per-suite state are visible without scrolling;
   the case bodies are collapsed until wanted.
3. **State is encoded in form, not only colour.** A failed case carries a red gutter, a red ID
   chip, a taller tick in the run strip *and* the word "Failed". Colour is never the only carrier.

The default state is the exception to rule 3: an untested case says nothing at all in the row.
Repeating "Untested" 135 times is noise, and the empty verdict is self-evident — the word is
kept in a visually-hidden span so assistive tech still hears it.

## The run strip

The masthead's centrepiece, and the one place the page spends any boldness.

**One tick per case**, 135 of them, in source order, grouped into 11 suite blocks. Each block's
`flex-grow` is its case count, so block widths are proportional to how much of the run each suite
represents. A tick is `--tick` when untested, `--pass` or `--fail` once judged.

Two details carry information rather than decorate:

- **Failed ticks are 29px tall against 22px.** The strip is bottom-aligned, so a failure sticks
  up out of the band and is findable at a glance without relying on red.
- **The strip sits on a `--line-2` baseline**, which is what makes it read as a measurement rather
  than a skeleton loader when nothing has been recorded yet.

Each suite block is a real `<a>` to that suite's section, with an `aria-label` carrying its
counts; the ticks inside are decorative children with `title` tooltips. Hovering or focusing a
block reveals a `--accent` underline, which also shows where one suite ends and the next begins.

The same component appears at 5×12px in every section head (`.secstrip`), so a suite's own state
reads in the same visual language as the whole run.

## Colour

Neutrals are biased faintly cyan, toward the accent, so they read as chosen rather than as
default grey. The ground is not pure white.

### Light (bare `:root`)

| Token | Value | Role |
|---|---|---|
| `--ground` | `#F6F8F9` | Page background, and the sticky control bar's fill |
| `--surface` | `#FFFFFF` | Masthead, case list, rail boxes, inputs |
| `--surface-2` | `#EDF1F3` | Code blocks, hover fills, keycaps, tags |
| `--surface-3` | `#E3EAED` | ID chips |
| `--ink` | `#101619` | Primary text, section rules |
| `--ink-2` | `#48585F` | Secondary text, body copy |
| `--ink-3` | `#78898F` | Micro-labels, placeholders, counts, footer |
| `--line` | `#D6DFE3` | Hairlines, card and list borders |
| `--line-2` | `#C2CFD4` | Stronger borders: buttons, inputs, the strip baseline |
| `--accent` | `#0B6E75` | Identity: links, active filter, focus ring |
| `--accent-soft` | `#DFEFF0` | Expected-output block fill |
| `--accent-line` | `#8FC4C7` | Expected-output border, hover borders |
| `--pass` | `#2C7A44` | Passed state |
| `--pass-soft` | `#E3F1E7` | Passed ID-chip fill |
| `--fail` | `#B02128` | Failed state |
| `--fail-soft` | `#FAE6E7` | Failed ID-chip fill |
| `--gap` | `#9A5A08` | Known-defect edge and heading |
| `--tick` | `#CAD7DC` | An unjudged tick in the run strip |

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
| `--line-2` | `#33444E` | | `--tick` | `#2E3D46` |

### The rule that matters

**Semantic colour is separate from the accent.** `--pass`, `--fail` and `--gap` mean *result*;
`--accent` means *identity and interactivity*. Never express a verdict in the accent, and never
use a semantic colour for a link or an active filter — the moment those overlap, a teal chip
becomes ambiguous between "this is interactive" and "this is a state".

There is no `--shadow` and no `--gap-soft`. Both were dropped when the case list stopped being a
stack of shadowed cards and the known-defect callout lost its amber fill; a token with no
consumer only misleads whoever edits next. Do not reintroduce either without a use.

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

**Two families, split along who is speaking.** JetBrains Mono — drawn for reading code on screen —
carries everything the *machine* says: commands, output, case IDs, counts, chips, tags, keycaps.
IBM Plex carries everything the *page* says: prose, headings, labels, buttons. On a page this
mono-heavy that split does real work; it is not decoration.

Each has a real fallback stack, so the page degrades to system faces offline.

| Role | Stack | Used for |
|---|---|---|
| `--sans` | IBM Plex Sans → `ui-sans-serif`, `system-ui`, … | All UI and prose |
| `--mono` | JetBrains Mono → `ui-monospace`, `SFMono-Regular`, … | Every command, output, case ID, count, chip, keycap |
| `--serif` | IBM Plex Serif *italic* → Georgia, … | The masthead lede only |

The serif appears exactly once, in the sub-headline. It is a deliberate single note of contrast
against an otherwise sans/mono page — using it anywhere else dilutes it to decoration.

**Code ligatures are switched off**, and this is not a stylistic preference:

```
pre,code{font-variant-ligatures:none;font-feature-settings:"liga" 0,"calt" 0}
```

JetBrains Mono ligates by default, and the case data contains 330 occurrences of `--`, 29 of
`->`, 37 of `//` and 35 of `::`. With ligatures on, `redis-cli --version` renders the double
hyphen as one long dash — a tester could reasonably retype it as a single hyphen or an em dash.
The whole page exists so that expected output can be compared against actual character by
character, so every glyph stays literal. Any new element that shows a command or a reply must
inherit this rule or restate it.

Swapping the mono face was safe because **JetBrains Mono and IBM Plex Mono share a 0.6em advance
width** — measured, not assumed — so nothing reflowed. Check that before changing it again; a
mono face with different metrics would resize every `<pre>`, chip, tag and count on the page.

**Scale**, in px: `10.5 · 11 · 11.5 · 12 · 12.5 · 13 · 13.5 · 14.5 · 15 · 18 · 22`. Body is 15px
at `line-height:1.5`; code is 12.5px at `1.62`, which keeps dense terminal output readable.

**The micro-label convention.** Field labels (`PRECONDITION`, `STEPS`, `EXPECTED OUTPUT`) and
rail headings are all: mono, 10.5–11px, uppercase, `--ink-3`, weight 600, letter-spacing
`.09em`–`.13em`. Wide tracking is what makes small uppercase legible; without it they read as
noise. Anything that *labels* rather than informs looks like this — and nothing else does. In
particular the masthead has no eyebrow, and a case's verdict (`Passed` / `Failed`) is mono
sentence case, not tracked caps: it is data, not a label, and 135 rows of tracked caps shout.

Numbers that stack in columns use `font-variant-numeric: tabular-nums` so digits align.

## Layout

A **two-pane operator console**, capped at `1240px`:

```
┌──────────────────────────────────────────────────────────┐
│ masthead: rocket-mem QA          │ build tags            │
│           serif lede                                     │
│ ▐▐▐▐▐▐▐ ▐▐▐▐ ▐▐▐▐▐▐▐▐▐▐▐▐ ▐▐▐ …  run strip, full width  │
│ 55 passed  3 failed  77 untested │ plain-language status │
├───────────────┬──────────────────────────────────────────┤
│ rail (236px)  │ main                                     │
│ sticky, 16px  │  ├ known defects (collapsible)           │
│  ├ suite nav  │  ├ search + filters + expand  ← sticky   │
│  ├ run box    │  └ sections → grouped case lists         │
│  └ key hints  │                                          │
├──────────────────────────────────────────────────────────┤
│ footer (--surface, bookends the masthead)                │
│  env exports │ provenance │ links                        │
│  rocket-mem QA          [System|Light|Dark]  Back to top │
└──────────────────────────────────────────────────────────┘
```

`body` is a `min-height:100vh` column flex with `.shell{flex:1 0 auto}`, so the footer sits at the
viewport bottom instead of floating when a filter empties the case list. Sticky positioning is
unaffected — the rail still pins at `top:16px` and the control bar at `top:0`.

- `.shell` — `grid-template-columns: 236px minmax(0,1fr)`, `gap:30px`, `align-items:start`.
  The `minmax(0,1fr)` matters: without the `0` floor, wide `<pre>` content forces the column
  wider than the viewport. **Every grid on the page needs it**, not just this one — a plain
  `1fr` has an `auto` min floor, and it has now caused the same overflow bug twice: once in the
  `900px` one-column rule, and again in `.foot-grid`, whose env-export `<pre>` widened its track
  at 500px. If you add a grid that can contain a `<pre>`, write `minmax(0,…)`.
- `.rail` — `position:sticky; top:16px`, so counts stay visible through a long scroll.
- `.controls` — `position:sticky; top:0` on an opaque `--ground` fill, 59px tall when stuck.
  Search and filters have to survive a 135-case scroll. Everything that can be jumped to
  therefore carries `scroll-margin-top:76px` to clear it.
- **Breakpoints: `900px` and `560px`.** At 900px the grid collapses to one column, the rail
  un-sticks and **moves below `main`** (`order`) — on a phone the cases matter more than the
  navigation, and the run strip still navigates by suite. The keycap box hides entirely. 560px
  only tightens page padding and unwraps the callout summary.
- Wide content scrolls inside its own container (`pre { overflow-x:auto }`), never the body.
  Verified at 390/768/1400px: `scrollWidth` never exceeds `innerWidth`.

## Components

| Component | Spec | Notes |
|---|---|---|
| `.strip` / `.grp` / `.t` | 22px ticks (29px failed), 2px gaps, 7px between suites, `--line-2` baseline | See "The run strip". Blocks grow by case count. |
| `.readout` / `.fig` | mono 22px tabular figure + 12.5px sans label | Boxless. Lowercase labels; the count is the loud part. |
| `.runnote` | 12.5px `--ink-3`, right-aligned above 760px | A sentence, generated: "58 of 135 recorded. Failures in Smoke suite and Core data types and keys." Names up to three suites, then collapses to a count. |
| `.cases` | `--surface`, 1px `--line`, radius 7px, `overflow:hidden` | One grouped list, not a stack of cards. |
| `.case` | `border-top` hairline, `border-left:3px` verdict gutter | The gutter is the scan line. No shadow, no gap. |
| `.cid` | mono 11.5px, 600, radius 3px, `--surface-3` | Tinted `--pass-soft` / `--fail-soft` once judged. |
| `.sec-head` | `border-bottom:2px solid var(--ink)` | The only 2px ink rule on the page — it is what separates suites. |
| `pre` | mono 12.5px/1.62, `--surface-2`, radius 5px, `tab-size:2` | Expected output overrides to `--accent-soft` + `--accent-line`, so steps and expected never blur together. |
| `.chip` | mono 12px, radius 5px, `aria-pressed` | Filters. Active state fills with `--accent`. |
| `.vb` | mono 12px, 600, radius 5px | Pass / Fail / Clear. Fills with its semantic colour when pressed. |
| `.copy` | absolute, `opacity:0` until `.codewrap:hover` or `:focus-visible` | Hidden affordance that must stay keyboard-reachable — hence the focus condition. |
| `.alert` | `<details>`, 3px `--gap` left edge on a plain `--surface` | Known defects. The only amber on the page, and it no longer fills — it is reference material, not the hero. |
| `.keyrow kbd` | mono 10.5px, `border-bottom-width:2px` | The doubled bottom border is the whole keycap illusion. |
| `.foot` | `--surface` with a `--line` top rule, full-bleed | Bookends the masthead, so the page opens and closes on `--surface` with the working area on `--ground` between them. |
| `.foot-grid` | `minmax(0,1.5fr) minmax(0,1fr) minmax(0,1fr)`, one column below 820px | Three columns of real content, not link soup. The wide one leads because it holds a working code block. |
| `.foot-bar` | flex, `--line` top rule; `.foot-id` takes `margin-right:auto` | Identity left, controls right. Wraps to two rows on a phone. |
| `.themectl` / `.tbtn` | inline-flex, hairline-divided segments, radius 5px | Active segment fills with `--accent`, matching `.chip`. `outline-offset:-2px` because the wrapper clips. |

The footer's first column is a live `.codewrap`/`pre`/`.copy` block holding the two env exports
every case depends on. That is the point of it: the footer ends the page with the thing a tester
actually needs to paste, rather than a disclaimer. It reuses the case components verbatim, so
`copyFrom()` is shared between the per-case buttons and this one.

## Interaction

- **Verdicts persist per viewer** in `localStorage` under `rocketmem-qa-v1`; whether the
  known-defect callout is folded away persists under `rocketmem-qa-v1-known`. Every read and
  write is wrapped in `try`/`catch` — private windows and blocked site data throw on access, and
  the page must still render.
- **Cases are collapsed by default**, except failures, which auto-expand. A failure is the thing
  you need to look at. This depends on `[hidden]{display:none!important}` near the top of the
  stylesheet: `.case-body{display:flex}` is a class selector and outranks the UA's `[hidden]`
  rule, so without the restatement every case renders expanded.
- **The rail marks the suite you are scrolled into** via `aria-current`, recomputed on a
  rAF-throttled scroll listener against each section's `getBoundingClientRect().top - 90`.
- **Keyboard operation**, since the page is worked alongside a terminal: `j`/`k` move between
  cases, `n` jumps to the next untested one, `p`/`f`/`x` record or clear a verdict on the focused
  case, `e` expands everything, `/` focuses search and `Escape` clears it. Verdict keys act on
  `document.activeElement.closest('.case')` — focus *is* the cursor, so there is no second
  selection state to keep in sync. Modifier chords and typing in a field are ignored.
- **Theme is chosen, not only inherited.** A three-state `System / Light / Dark` control sits in
  the footer bar. It stores under **`rm-theme`** — the same key and the same three values
  `docs/manual.html` uses — so a choice made in either document carries to the other. `light` and
  `dark` stamp `data-theme` on `<html>`; `system` *removes* the attribute, which is what hands
  control back to `prefers-color-scheme`.

  A tiny inline script in `<head>` applies the stored value **before first paint**, so the page
  never flashes the wrong theme. `manual.html` deliberately is not a model here — it applies the
  theme only after its main script runs, and does flash. Keep the head script first if you
  reorder anything in `<head>`.

  It lives in the footer rather than the masthead because the top of the page belongs to the run
  status, and a theme is a preference rather than an operation. Moving it is a markup move only —
  the CSS and JS bind by class and `data-theme-set`, not by location.
- **Search** filters on ID, title, precondition, steps, expected and notes, debounced 130ms.
- **Re-render preserves the open card and the focus.** Recording a verdict rebuilds the list; the
  handler re-opens and re-focuses the case that was just judged, so the page does not collapse
  under the cursor and `j`/`k` keep working after a verdict.

## Accessibility

- Focus is visible everywhere: `2px solid var(--accent)` at `2px` offset.
- `prefers-reduced-motion: reduce` disables all transitions and animations.
- Filters use `aria-pressed`; case headers use `aria-expanded`; the case list is real buttons,
  not click-handled `div`s; suite blocks in the run strip are real links with counts in their
  `aria-label`.
- Verdicts are never colour-only — gutter, chip tint, tick height and a text label change
  together, and the default "Untested" survives in a `.vh` span for screen readers.

## Extending it

1. Add a token before adding a colour. A literal hex in a component rule will break one theme.
2. Define the token on bare `:root` first, then in **both** dark blocks.
3. Decide whether the thing is *state* or *identity*, and take the colour from the matching
   family. Do not reach across.
4. Label-like text gets the micro-label treatment; data gets mono; prose gets sans.
5. Check both themes before committing. The fastest check:

   ```bash
   google-chrome --headless --disable-gpu --hide-scrollbars \
     --virtual-time-budget=5000 --window-size=1400,1150 \
     --screenshot=/tmp/qa.png "file://$PWD/docs/qa-playbook.html"
   ```

   Headless defaults to the **dark** palette. Do *not* force light by stamping
   `data-theme="light"` on `<html>` — the theme control's `applyTheme('system')` runs on load and
   strips the attribute straight back off. Click the control instead:

   ```js
   addEventListener('load', function(){ setTimeout(function(){
     document.querySelector('[data-theme-set="light"]').click(); }, 200); });
   ```

   Review the **mid-run** state, not the empty one — the strip, the gutters and the status line
   all only do their job once verdicts exist. Seed one by replacing the `localStorage` read in the
   script with a loop over `DATA`. To review the footer on its own, inject
   `.rail,.shell main{display:none}` rather than trying to scroll to it.

   Headless `--screenshot` cannot capture a scrolled viewport; to check sticky geometry, inject a
   script that scrolls and writes the measurements into `document.title`, then read it back with
   `--dump-dom`.

## Constraints worth knowing

- **Webfonts are fetched from Google Fonts** — IBM Plex Sans, IBM Plex Serif and JetBrains Mono,
  in one request. Offline, the page falls back to the system stacks and still works.
  `docs/manual.html` deliberately takes the opposite position and ships no webfont at all, so the
  two documents are inconsistent on this point by choice, not accident.
- **There is no JetBrains sans on Google Fonts.** JetBrains Mono is the only family of theirs the
  CDN serves; JetBrains Sans is distributed from jetbrains.com and would have to be self-hosted,
  which is why the sans and serif roles are still IBM Plex. If someone asks for "JetBrains fonts"
  across the whole page, that is the trade to put to them — self-hosting font files in the repo
  versus one CDN request.
- **The page is generated, not hand-edited.** Cases are parsed out of `docs/qa-playbook.md` into
  an embedded JSON block on a single long line. Editing case text in the HTML will be overwritten
  on the next regeneration — edit the markdown. There is no generator script in the repo; when
  regenerating, keep that `<script type="application/json" id="data">` line byte-identical unless
  the markdown itself changed.
- **The repo copy carries its own document wrapper** (doctype, charset, viewport, `body{margin:0}`)
  because it is opened as a file. The published-artifact copy must *not* have those; the host
  supplies them.
