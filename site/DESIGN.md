---
version: alpha
name: girt
description: >-
  Visual identity for the girt documentation site: layered graphite surfaces, a dark rust red and a
  verdigris green, plain technical copy and a commit-graph navigation motif. Dark theme first, with
  an equal light theme.
colors:
  # Dark theme (default). Surfaces from deepest to highest.
  inset: "#0d0d0e"
  background: "#141415"
  surface: "#1b1c1e"
  surface-raised: "#222326"
  rule: "#2a2b2e"
  edge: "#000000"
  on-surface: "#d6d3cc"
  on-surface-strong: "#f3efe6"
  on-surface-secondary: "#b9b6ae"
  on-surface-muted: "#9c9a94"
  graph-line: "#4a4c52"
  graph-node: "#8a8780"
  # Rust ramp. rust-900 is the base the identity is built on.
  rust-950: "#24100c"
  rust-900: "#3a1410"
  rust-800: "#5a1a12"
  rust-700: "#7a2214"
  rust-600: "#a83222"
  rust-500: "#c4492f"
  # Verdigris: weathered copper green, the counterpart to rust.
  verdigris-900: "#1f3d34"
  verdigris-700: "#2f6b59"
  verdigris-500: "#4f9a80"
  # Syntax highlighting (dark).
  code-keyword: "#c98a7a"
  code-string: "#8fae9f"
  code-type: "#e9dcc4"
  code-comment: "#8a8780"
  # Spec-recommended aliases.
  primary: "{colors.rust-700}"
  secondary: "{colors.on-surface-secondary}"
  tertiary: "{colors.verdigris-500}"
  neutral: "{colors.background}"
  error: "{colors.rust-600}"
  # Light theme.
  light-inset: "#ffffff"
  light-background: "#f6f3ec"
  light-surface: "#efebe2"
  light-surface-raised: "#e8e3d7"
  light-rule: "#ddd7c9"
  light-edge: "#d3ccbc"
  light-on-surface: "#3a3832"
  light-on-surface-strong: "#1b1a17"
  light-on-surface-secondary: "#4a473f"
  light-on-surface-muted: "#625f55"
  light-graph-line: "#b9b3a2"
  light-graph-node: "#7a756a"
  light-rust-tint: "#f3e2dc"
  light-rust-wash: "#f8ece8"
  light-rust-edge: "#e9c9bf"
  light-verdigris-tint: "#dcebe4"
  light-code-keyword: "#8a2a1a"
  light-code-string: "#2f6b59"
  light-code-type: "#4a3a20"
  light-code-comment: "#6f6a5e"
typography:
  display:
    fontFamily: Source Serif 4
    fontSize: 64px
    fontWeight: 700
    lineHeight: 1
    letterSpacing: -0.02em
  headline-lg:
    fontFamily: Source Serif 4
    fontSize: 40px
    fontWeight: 700
    lineHeight: 1.1
    letterSpacing: -0.01em
  headline-md:
    fontFamily: Source Serif 4
    fontSize: 25px
    fontWeight: 700
    lineHeight: 1.2
  headline-sm:
    fontFamily: Source Serif 4
    fontSize: 19px
    fontWeight: 700
    lineHeight: 1.3
  body-lg:
    fontFamily: system-ui
    fontSize: 18px
    fontWeight: 400
    lineHeight: 1.65
  body-md:
    fontFamily: system-ui
    fontSize: 16px
    fontWeight: 400
    lineHeight: 1.75
  body-sm:
    fontFamily: system-ui
    fontSize: 14px
    fontWeight: 400
    lineHeight: 1.6
  label-caps:
    fontFamily: JetBrains Mono
    fontSize: 11px
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: 0.12em
  code-md:
    fontFamily: JetBrains Mono
    fontSize: 13.5px
    fontWeight: 400
    lineHeight: 1.7
  code-inline:
    fontFamily: JetBrains Mono
    fontSize: 0.875em
    fontWeight: 400
    lineHeight: 1.4
  numeric:
    fontFamily: JetBrains Mono
    fontSize: 13.5px
    fontWeight: 400
    lineHeight: 1.4
    fontFeature: '"tnum" 1'
rounded:
  none: 0px
  xs: 4px
  sm: 6px
  md: 8px
  lg: 12px
  full: 9999px
spacing:
  base: 4px
  xs: 4px
  sm: 8px
  md: 16px
  lg: 24px
  xl: 32px
  2xl: 48px
  3xl: 64px
  nav-row: 30px
  sidebar-width: 300px
  content-max: 820px
  header-height: 58px
components:
  button-primary:
    backgroundColor: "{colors.rust-700}"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.sm}"
    padding: 10px 16px
  button-primary-hover:
    backgroundColor: "{colors.rust-600}"
  button-secondary:
    backgroundColor: transparent
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.sm}"
    padding: 10px 16px
  button-secondary-hover:
    backgroundColor: "{colors.surface-raised}"
  link:
    textColor: "{colors.on-surface-strong}"
  link-light:
    textColor: "{colors.rust-700}"
  code-block:
    backgroundColor: "{colors.inset}"
    textColor: "{colors.on-surface}"
    typography: "{typography.code-md}"
    rounded: "{rounded.md}"
    padding: 14px 18px
  code-inline:
    backgroundColor: "{colors.inset}"
    textColor: "{colors.on-surface-strong}"
    typography: "{typography.code-inline}"
    rounded: "{rounded.xs}"
    padding: 1px 5px
  callout-note:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.md}"
    padding: 12px 16px
  callout-refused:
    backgroundColor: "{colors.rust-950}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.md}"
    padding: 12px 16px
  card:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.lg}"
    padding: 20px 22px
  nav-graph-node:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.on-surface-secondary}"
    size: 9px
  nav-graph-node-current:
    backgroundColor: "{colors.rust-600}"
    textColor: "{colors.on-surface-strong}"
    size: 11px
  perf-band-0:
    backgroundColor: "{colors.verdigris-900}"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.xs}"
  perf-band-1:
    backgroundColor: "#2a2b2e"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.xs}"
  perf-band-2:
    backgroundColor: "{colors.rust-900}"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.xs}"
  perf-band-3:
    backgroundColor: "{colors.rust-700}"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.xs}"
  perf-bar-faster:
    backgroundColor: "{colors.verdigris-700}"
  perf-bar-slower:
    backgroundColor: "{colors.rust-700}"
  status-chip-failed:
    backgroundColor: "{colors.rust-800}"
    textColor: "{colors.on-surface-strong}"
    rounded: "{rounded.xs}"
    padding: 2px 8px
---

# girt design

This file is the source of truth for how the girt site at <https://www.joshka.net/girt/> looks. The
YAML front matter holds the normative values, in the
[DESIGN.md format](https://github.com/google-labs-code/design.md/blob/main/docs/spec.md) (alpha).
The prose explains how to apply them. Tokens without a `light-` prefix are the dark theme; the light
theme swaps in its `light-` counterparts and keeps every other rule.

## Overview

girt is an idiomatic Rust library for Git repositories and the Git backend for an experimental build
of jj. Its readers are Rust developers who build version-control tools, forges and build systems.
They are technical and sceptical, and they want facts and code quickly.

The site should feel like a well-made tool: calm, precise and crafted, never loud. It explains; it
doesn't sell.

- **Plain copy.** No taglines or slogans. The landing page heading is the logo, followed by one
  sentence saying what girt is, written the way a crate README would put it.
- **Honest comparisons.** Performance is shown as girt against stock jj and Git, with wins and
  losses side by side, and always links to the full results.
- **Quiet colour.** Colour lives in fills, bars, rules and underlines. Running text stays neutral.
- **Dark first.** Dark is the default theme. The light theme is equally complete, not an
  afterthought.

## Colors

The palette has three families.

- **Graphite (`inset` to `surface-raised`):** four neutral greys that create depth by layering. The
  page sits in the middle, chrome rises above it, and code is set into it.
- **Rust (`rust-950` to `rust-500`):** a dark iron-oxide red. `rust-900` (`#3a1410`) is the base the
  identity is built on. Rust is used as a surface and an edge colour: the primary button, the
  current navigation node, link underlines, refusal callouts and "slower" bars.
- **Verdigris (`verdigris-900` to `verdigris-500`):** weathered copper green, the counterpart to
  rust. It marks "faster" results and string literals.

Rules for colour:

- In the dark theme, red is never used for text. Any red light enough to read on graphite looks
  orange or pastel. Use ivory or grey text on a red fill, or a red underline, instead.
- In the light theme, `rust-700` (`#7a2214`) is the text colour for links, labels and "slower"
  figures. It is 9.2:1 on `light-background`.
- Verdigris text is allowed only for string literals in code.
- Body text is `on-surface`. Headings are `on-surface-strong`. Secondary and muted text use
  `on-surface-secondary` and `on-surface-muted`.

Checked contrast ratios (WCAG 2.x):

| Pair                                                            | Ratio             |
| --------------------------------------------------------------- | ----------------- |
| `on-surface` on `background`                                    | 12.3:1            |
| `on-surface-muted` on `background`                              | 6.5:1             |
| `on-surface-secondary` on `surface` (navigation)                | 8.4:1             |
| `on-surface-strong` on `rust-700` (primary button)              | 8.9:1             |
| `on-surface-strong` on `rust-800` (failure chip)                | 11.5:1            |
| `on-surface-strong` on `rust-900` and `verdigris-900` (heatmap) | 14.2:1 and 10.3:1 |
| `code-keyword` and `code-string` on `inset`                     | 6.9:1 and 8.1:1   |
| `light-on-surface` on `light-background`                        | 10.6:1            |
| `light-on-surface-muted` on `light-background`                  | 5.8:1             |
| `rust-700` and `verdigris-700` on `light-background`            | 9.2:1 and 5.6:1   |

Every new pair must pass AA: 4.5:1 for text, 3:1 for large text and meaningful graphics. Bars may
fall below 3:1 against the page only when the same information is written next to them in text.

## Typography

- **Headings:** Source Serif 4 (SIL Open Font License 1.1), bold. A sturdy serif gives the site its
  character. Self-host the Latin subset.
- **Body:** the system UI stack (`system-ui`, `-apple-system`, `Segoe UI`, and so on). It costs no
  bytes, causes no layout shift and reads well at length.
- **Code, labels and numbers:** JetBrains Mono (SIL Open Font License 1.1). Labels are small
  uppercase mono with wide tracking (`label-caps`) and stay muted. Numbers in tables use tabular
  figures so they align.

Load the web fonts with `font-display: swap` and preload only the Latin subsets. Give each local
fallback font its own metric-adjusted face, measured separately for every weight the site uses
(Georgia and Times New Roman at 400 and 700 for the serif; Menlo and Courier New for the mono), so
text keeps its size and wrapping while the web fonts load. Check heading wrapping at phone width
with the web fonts blocked.

## Layout

- A 4px base unit; most spacing uses `sm`, `md`, `lg` and `xl`.
- Docs pages use three columns at desktop widths: the sidebar (`sidebar-width`), the content column
  (at most `content-max`) and the table of contents. The performance page drops the table of
  contents so the heatmap has room.
- The landing page has a two-column hero, with the logo, description and actions on the left and a
  facts column (install, status, licence, no subprocess) on the right. Below it come a code example,
  the comparison card and three plain link cards.
- At phone width everything stacks into one column. Wide tables scroll sideways inside their own
  frame, never the page.

## Elevation & Depth

Depth comes from tonal layers, not decoration.

- **Inset (`inset`):** code blocks, inline code and the search field. They are set into the page,
  with a soft inner shadow and a hairline border.
- **Page (`background`):** prose sits here.
- **Surface (`surface`):** the header, sidebar, callouts and cards. A 1px black edge plus a 1px
  `rule` highlight separates them from the page.
- **Raised (`surface-raised`):** hover states and the occasional raised card, with a soft drop
  shadow.

The landing hero may carry a very faint `rust-900` wash that fades into the page. Backgrounds stay
subtle: no textures, no photographic or noise images, no glows.

## Shapes

Corners are small and consistent: `xs` (4px) for inline code and chips, `sm` (6px) for buttons and
inputs, `md` (8px) for code blocks and callouts, and `lg` (12px) for cards. Lines are hairlines
(1px), with 1.5px for table header rules and 2px for link underlines.

## Components

### Header

The logo, the search field, then text links: Guides, Performance and API (docs.rs), and GitHub with
the theme switch. The current section's link gets a 2px rust underline.

### Navigation commit graph

The sidebar is drawn as a `git log --graph`. It must look like a real graph, not decorative
squiggles:

- **Lanes on a grid.** Every row is exactly `nav-row` (30px) tall. Lane 0, the trunk, is a straight
  vertical line at a fixed x position. Lane 1, for pages, sits one lane width (18px) to the right.
  Lines snap to whole or half pixels.
- **Sections are commits on the trunk.** Each section title (Start here, Guides, and so on) is an
  open node on lane 0, centred on its row, with the label beside it in `label-caps`.
- **Each page is a branch.** Each page forks from the trunk with one straight 45° edge, as in gitk.
  The edge leaves the trunk just above the page's row and meets the node at the row centre on lane
  1. Every fork has the same shape, so the rows read as evenly spaced commits. Curved edges were
  tried and rejected: they hook into the node from the side and look uneven.
- **Open dots.** Nodes are hollow circles: `surface` fill with a 1.5 to 2px `graph-node` stroke. The
  node for the current page is filled `rust-600` with a `rust-500` stroke, and its label is
  `on-surface-strong` at semibold weight. There is no background highlight and no accent bar.
- **Draw order.** Lines first, nodes on top, so no line ever crosses through a hollow node.
- **Semantics.** Each section is a list item holding the section label and a nested list of its
  pages, labelled by that section label (`aria-labelledby`). The graph SVG is decorative
  (`aria-hidden`), and the current page's link carries `aria-current="page"`.
- **One line per row.** The sidebar is wide enough (`sidebar-width`) that every label fits on one
  row at the default text size; rows may still grow when text is enlarged.
- The same graph is the phone navigation menu.

### Buttons

There are three tiers. Primary is ivory text on `rust-700`, with a `rust-600` border and hover.
Secondary is ivory text with a `graph-line` outline and a `surface-raised` hover. Plain text links
are ivory, or `rust-700` in light mode, with a 2px rust underline. Links that leave the site end in
a muted `↗`, outside the underline, with "(external)" for screen readers. Every interactive element
has a visible focus ring: a 2px `on-surface-strong` outline, offset 2px.

### Code blocks

An inset panel with a file-name header row (`label`-sized, muted) and syntax colours from the
`code-*` tokens. Keywords use a muted rust and strings verdigris; nothing in code is bright.

### Callouts

A hairline-bordered box with a small `label-caps` title (NOTE, REFUSED, and so on). Notes use a
`surface` background and a `rule` border. Refusals and other warnings use a `rust-950` background
and a `rust-800` border, with the title in muted rust. There is no left accent bar and no coloured
fill on the whole box beyond these.

### Command references

Each CLI section heading names the task, then the command in parentheses
(`` Move a reference safely (`girt ref set`) ``), with the command in plain mono without the
inline-code chip. Under the heading, a compact strip on a surface fill with a hairline border gives
the girt command (or the family of commands the section covers), then the corresponding Git command,
then the linked library APIs the example exercises, with muted uppercase mono labels. Long entries
wrap inside the strip so the page never scrolls horizontally, and pages keep their table of
contents, which becomes an index of commands.

### Tables

A 1.5px header rule in `on-surface-secondary`, hairline `rule` row dividers, and no zebra stripes.
Numbers are right-aligned in tabular mono figures. A small `label-caps` group row may sit above
related columns.

### Comparison table (landing page)

Columns: command, girt, stock jj and Git (median times), then the ratio and a diverging bar.

- **Times:** girt's time is bold. Git's time is muted, as the reference point.
- **Ratio:** neutral text in dark mode ("1.8× slower"). In light mode it is `rust-700` or
  `verdigris-700`.
- **Bar:** centred on parity on a log scale, verdigris to the left for faster and rust to the right
  for slower. A timeout is a hatched rust bar plus a `status-chip-failed` chip.
- **Selection:** the card states that its rows are hand-picked best and worst. Its summary covers
  all repository sizes: how many cells are less than 20% slower than stock jj (faster cells
  included), out of the cells where both finished, plus how many did not finish with girt. It also
  gives the machine and dates and links to the full results. All figures are computed at build time
  from `docs/jj-performance.json`.

### Performance heatmap

Each cell shows girt's median as a chip coloured by time band (`perf-band-0` to `perf-band-3`,
verdigris through graphite to dark rust), with stock jj's time and ratio and Git's time in muted
small text below. Responsibility tags ("girt", "jj") are hairline-outlined, with the girt tag
outlined in rust.

## Do's and Don'ts

- Do write plain, factual copy. Don't write taglines, slogans or marketing claims.
- Do show losses alongside wins, and link to the full benchmark.
- Do keep text neutral in the dark theme. Don't use bright, orange-leaning or pastel reds for text.
  Don't use neon accents of any kind.
- Don't use left accent bars ("fingernails") on selected items, callouts or cards.
- Don't put textures, noise, photos or strong gradients behind content. A faint `rust-900` wash in
  the landing hero is the most allowed.
- Do keep the commit graph on a strict lane grid, with open nodes and the current page filled.
- Don't commit raster images to git. Visual assets are CSS or inline SVG; any image that is truly
  needed goes through Git LFS.
- Do meet WCAG AA in both themes, show visible focus rings, respect `prefers-reduced-motion`, and
  keep text size and wrapping stable while fonts load.
- Don't add JavaScript frameworks. Fonts and npm packages must be compatible with MIT and
  Apache-2.0.
- Do use the logo from `src/assets/`: a solid cog with thirty-six triangular teeth holding a
  three-commit graph, beside the "girt" wordmark as outlined paths. The light-theme file has a
  `rust-700` cog and wordmark with an `on-surface-strong` graph; the dark-theme file has an
  `on-surface-strong` cog and wordmark with a `rust-600` graph. Use the same files in the header and
  the landing hero. The favicon is the cog and graph alone, generated from the same files at build
  time, with the dark-theme colours when the browser prefers a dark scheme. Don't reuse or modify
  the Rust logo, which is a Rust Foundation trademark.
