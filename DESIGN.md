---
name: NET
description: Network Event Transport. A latency-first encrypted mesh, staged as a suspended field.
colors:
  field-green: "#c4ff3d"
  field-green-hover: "#d4ff6e"
  field-green-dim: "#7a9a2c"
  cast-black: "#0b0c0b"
  hall-floor: "#121311"
  panel-black: "#0d100d"
  panel-black-hover: "#111611"
  panel-border: "#222a21"
  ghost-border: "#3a4537"
  astral-ink: "#e6e8e0"
  ink-dim: "#969a91"
  ink-faint: "#555850"
  ink-faint-on-panel: "#7f8479"
  form-seam: "#252723"
  shaft-light: "#eef1e8"
  telemetry-amber: "#f0b04a"
  telemetry-ice: "#a9d3e2"
typography:
  display:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "clamp(60px, 8.3vw, 132px)"
    fontWeight: 700
    lineHeight: 0.86
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "clamp(40px, 5.6vw, 88px)"
    fontWeight: 700
    lineHeight: 0.92
    letterSpacing: "-0.03em"
  title:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "23px"
    fontWeight: 700
    lineHeight: 1.08
    letterSpacing: "-0.015em"
  lede:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "22px"
    fontWeight: 300
    lineHeight: 1.45
  body:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.6
  numeral:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "26px"
    fontWeight: 700
    lineHeight: 1
    fontFeature: "tnum"
  label:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "10px"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "0.32em"
  data:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "10.5px"
    fontWeight: 400
    lineHeight: 1.6
  control:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "11px"
    fontWeight: 700
    letterSpacing: "0.16em"
rounded:
  none: "0px"
  dot: "9999px"
spacing:
  panel-pad-sm: "20px"
  panel-pad: "28px"
  control-pad: "15px 22px"
  grid-gap: "24px"
  section-inset: "40px"
  section-pillar: "96px"
  section-rhythm: "128px"
components:
  button-field:
    backgroundColor: "{colors.field-green}"
    textColor: "{colors.cast-black}"
    typography: "{typography.control}"
    rounded: "{rounded.none}"
    padding: "15px 22px"
  button-field-hover:
    backgroundColor: "{colors.field-green-hover}"
    textColor: "{colors.cast-black}"
  button-ghost-field:
    textColor: "{colors.astral-ink}"
    typography: "{typography.control}"
    rounded: "{rounded.none}"
    padding: "14px 22px"
  button-ghost-field-hover:
    textColor: "{colors.field-green}"
  panel:
    backgroundColor: "{colors.panel-black}"
    textColor: "{colors.astral-ink}"
    rounded: "{rounded.none}"
    padding: "28px"
  panel-attended:
    backgroundColor: "{colors.panel-black-hover}"
  nav-field:
    backgroundColor: "{colors.cast-black}"
    textColor: "{colors.ink-dim}"
    typography: "{typography.control}"
    height: "56px"
  stencil-label:
    textColor: "{colors.ink-dim}"
    typography: "{typography.label}"
---

# Design System: NET

> **Scope, read first.** This system ("Suspended Field") is the **target** world for the whole site, but as of 2026-10 it is applied **only to the landing page `/`**. It lives in `web/src/app/world-field.css`, scoped under the `.world-field` class (set on the landing page container, and on the top status bar when the path is `/`). `/docs` and `/games` still run the **legacy** world defined by the root `@theme` in `web/src/app/globals.css`: acid green on near-black, Major Mono Display titles, Space Grotesk prose, CRT scanlines. That root theme is legacy and is not to be extended; new surfaces and the planned second pass migrate onto `.world-field`. Where the two share a token name (`--color-ink-dim`, `--color-slab`, `--font-display`, …), the `.world-field` value is the one this document records.

## Overview

**Creative North Star: "The Suspended Field"**

The mesh is a force field. The landing is a dark, raw-concrete brutalist hall, held up by nothing visible: shards, monoliths and pillars of cast concrete hang in it, lit by one cold light shaft through the dust. Inside that hall the information sits on **terminal panels**: near-black phosphor glass with a hairline border, faint scanlines and green corner ticks, the project's cyberpunk HUD surface. The concrete is the world; the terminal is the instrument.

Float carries meaning. The panels sit still on aligned grids by default, resting on the floor. A panel leaves the floor only when its height says something: altitude encodes speed (NET above real-time above best-effort), a staircase encodes progress (migration phases climbing to complete), a travelling wave encodes propagation (the nine axioms, like a pingwave), a shared rhythm encodes a single live system (the hero telemetry). Hovering a panel lifts it a few pixels and lights its seam: the field picks up what you attend to. The motion grammar is displacement and hold: **resting**, **attended** (lifted on hover/focus), **held** (seam always lit), **launched** (the hero canvas, where the cursor pushes shards and a click throws them).

The world is cyber-led and keeps the project's terminal telemetry, mono data and acid green, and refuses the dark-neon dev-tool page where every card floats, glows and differs for no reason.

**Key Characteristics:**
- Two materials: procedural cast concrete for scenery, terminal glass for every panel.
- Acid green carries one meaning: the field (live, held, attended, or the one primary action).
- Monumental geometric caps (TeX Gyre Adventor, Avant Garde lineage) for titles; Archivo for prose; JetBrains Mono only for data, code, telemetry and controls.
- Square corners. Panels on aligned, equal grids; height above the floor is data, never ornament.

## Colors

A near-monochrome warm-black hall with greenish-black terminal panels and a single acid-green field signal.

### Primary
- **Field Green** (`field-green`): the field. The primary control (Install), panel corner ticks, the seam under attended and held panels, focus outlines, link underlines and hover, live status dots, mesh threads and node glows on the canvas, Mikoshi transit cubes, the selection fill.
- **Field Green, Lit** (`field-green-hover`): the primary button under the pointer.
- **Field Green, Dim** (`field-green-dim`): the field at rest or attended at low volume. The border of an attended panel, ASCII glyph diagrams, low-priority event types (`hb.ack`), the one remaining terminal window dot, scrollbar thumb hover.

### Neutral
- **Cast Black** (`cast-black`): the hall. Page ground, nav bar (86% with backdrop blur), hero scrims, text on the green button.
- **Hall Floor** (`hall-floor`): recessed bands, such as the Install band between top and bottom hairlines.
- **Panel Black** (`panel-black`): the face of every panel. **Panel Black, Attended** (`panel-black-hover`) is the face under hover/focus.
- **Panel Border** (`panel-border`): the 1px edge of every panel at rest.
- **Ghost Border** (`ghost-border`): the 1px edge of the secondary (ghost) button.
- **Astral Ink** (`astral-ink`): titles, primary text, panel headings.
- **Ink Dim** (`ink-dim`): secondary text, on the ground and inside panels alike.
- **Ink Faint** (`ink-faint`): tertiary marks on the ground (hairlines beside stencils).
- **Ink Faint on Panel** (`ink-faint-on-panel`): tertiary text inside a panel (timestamps, placeholder metrics). The `slab` utility re-declares `--color-ink-faint` to this value so faint text stays legible on the panel face.
- **Form Seam** (`form-seam`): 1px hairlines: nav bottom edge, rules inside panels, band edges, the Topology floor line.
- **Shaft Light** (`shaft-light`): the light shaft and highlights on concrete scenery, always at low alpha.

### Tertiary (telemetry only)
- **Telemetry Amber** (`telemetry-amber`): drops, evictions, Blackwall rejections in event logs. Warnings only.
- **Telemetry Ice** (`telemetry-ice`): capability reads and peer joins in event logs.

### Named Rules
**The One Meaning Rule.** Field Green means the field: live, held, attended, or the one primary action. On a panel it appears only as the corner ticks (where the field grips it, 60% at rest, full when attended), the dim border when attended, and the seam. It is never a panel fill, a section background or a full bright border at rest.

**The Panel Ink Rule.** Inside a panel, only the faint ink is raised (`ink-faint-on-panel`); dim and primary ink keep their ground values. Get it by putting text inside the `slab` utility, not by hand-picking a grey.

**The No Red Rule.** The palette has no red. Warnings are amber. Inherited terminal window dots are neutralised to grey and dim green inside the world.

## Typography

**Display Font:** TeX Gyre Adventor (self-hosted Latin subset, `web/src/fonts/adventor/`, GUST Font License; fallback Avant Garde, Century Gothic, sans-serif)
**Body Font:** Archivo (fallback Helvetica Neue, Arial, sans-serif)
**Label/Mono Font:** JetBrains Mono (fallback ui-monospace, Menlo, monospace)

**Character:** Monumental, tightly tracked geometric caps carry the architecture; a quiet Swiss-lineage grotesque carries the argument; mono is the instrument readout. Three voices, never swapped.

### Hierarchy
- **Display** (700, `clamp(60px, 8.3vw, 132px)`, 0.86, -0.04em, uppercase): the hero area title only. One per page.
- **Headline** (700, `clamp(40px, 5.6vw, 88px)`, 0.92, -0.03em, uppercase, max 15em): every section title. Each line is lifted into place separately as the section enters view.
- **Title** (700, 23–24px, 1.08, -0.015em): headings inside panels.
- **Numeral** (700, 26px, 1.0, tabular): measured figures in stat panels, with a mono unit in ink-dim at 10px beside them.
- **Lede** (300, 19–22px, 1.45–1.5, max 30–34em): opening paragraphs.
- **Body** (400, 14–15px, 1.6, max 34–36em): prose, in ink-dim. All `<p>` in the world default to Archivo.
- **Label / Stencil** (400, 10px, 0.32em, uppercase, mono, ink-dim): section markers, panel IDs, document refs.
- **Data** (400, 10.5–11px, 1.35–1.6, mono): event tails, code, ASCII diagrams, panel header bars (10px, 0.16em, uppercase).
- **Control** (600–700, 11px, 0.16em, uppercase, mono): buttons and nav items (10.5px).

### Named Rules
**The Three Voices Rule.** Adventor for structure, Archivo for sentences, JetBrains Mono for anything a machine would print. Never set prose in mono, never set a number readout in Archivo.

**The Monument Rule.** Titles are bold, uppercase, negatively tracked and nearly solid (line-height under 1). A title that needs a period to land gets it ("NET. MOVES AT LIGHT.").

## Layout

The landing runs in a centered 1440px column under a fixed 28px status bar and a fixed 56px nav (main content offset 84px). Sections use one shell: left inset 24px (40px at ≥1024px), a wide right **pillar** of 48px (96px at ≥1024px) kept clear for the vertical stencil label, and a vertical rhythm of 96px (128px at ≥1024px).

Panels sit on **aligned, equal grids** with 20–24px gaps: one column on phones, two at ≥760px, and at ≥1024px as many as the set has peers (axioms 3, applications 2, components 4, group cards 3, topology classes 3, migration phases 6). Every panel in a set has the same span and the same top edge unless its height encodes something (see Elevation & Depth). Nothing overlaps; nothing bleeds off the page edge.

The hero is a split: text on the left (title, lede, actions, stencil ref line pinned to the bottom), a 400px column on the right with two telemetry panels moving as one, and a full-bleed canvas field behind both, darkened on the text side by a horizontal scrim. On phones the panels drop out and a heavier vertical scrim sits over the field.

Breakpoints are the shared Tailwind set: 640, 760, 1024, 1280, 1440px.

## Elevation & Depth

Panels are flat terminal glass with one soft cast shadow below them; depth is not varied between panels. What varies is **altitude**, and only where it encodes data. Concrete scenery carries the hall's atmosphere: the hero shards turning in the light shaft, Monolith cuboids swaying over their floor shadows, Blackwall pillars, Mikoshi plinths, and a fixed 20%-opacity film grain over the page.

### Shadow Vocabulary
- **Panel rest** (`inset 0 1px 0 rgba(196,255,61,0.05), 0 22px 28px -24px rgba(0,0,0,0.9)`): every panel.
- **Panel attended** (`inset 0 1px 0 rgba(196,255,61,0.08), 0 34px 34px -26px rgba(0,0,0,0.95)`): a panel lifted on hover/focus, paired with a 5px rise.
- **Seam glow** (`0 8px 18px 1px rgba(196,255,61,0.22)`): spill under the seam of an attended panel.
- **Held seam glow** (`0 10px 22px 1px rgba(196,255,61,0.2)`): spill under the seam of a held panel.
- **Primary control** (`inset 0 1px 0 rgba(255,255,255,0.45), 0 22px 30px -22px rgba(0,0,0,0.9)`): the field button.

### Altitude encodings in use
- **Rank (altitude = speed):** the topology classes share a floor hairline; best-effort rests on it, real-time sits 40px up, NET sits 96px up, held, on the shared rhythm.
- **Progress (staircase):** migration phases climb 14px per step from snapshot to complete.
- **Propagation (wave):** the nine axioms carry one 9px wave in reading order, 0.28s apart over 7s.
- **One system (sync):** the hero telemetry panels rise and settle together on one 9s, 8px rhythm.
- **Attention (lift):** any interactive panel rises 5px while hovered or focused.

### Named Rules
**The Meaningful Float Rule.** A panel leaves the floor only when its height encodes something: rank, progress, propagation, shared state, or the visitor's attention. If you can't say what the height means, the panel rests. No random bobs, no per-card offsets, no idle drift on panels. All of it stops under `prefers-reduced-motion`.

**The Seam Rule.** The field shows itself on a panel as a single 1px green line along its bottom edge: grown from 30% width when attended, or always lit when held. Held is reserved for the statement panel of a section.

**The Held Not Hung Rule.** Panels are never wired to each other, railed or framed together; a panel's relationship to the page is its grid position and its shadow on the floor. The only lines that join things are field threads between mesh nodes (the hero canvas, the Mikoshi route), and they are green because they are the field.

## Shapes

Square-cornered throughout (0px). Panels, buttons, nav, terminals and code blocks are rectangles; the only round forms are 6–7px status dots. A panel is defined by its 1px border plus eight 10px green corner ticks drawn just outside the border, faint 3px scanlines and a faint green top gradient. Concrete scenery is the other form family: true CSS 3D cuboids and pillars with grain, stain and pit textures, cropping hard off page edges.

### Named Rules
**The Two Materials Rule.** Concrete is for scenery: canvas shards, Monoliths, Mikoshi plinths, Blackwall pillars. Terminal glass is for anything that carries words or numbers. Never put text on concrete, never make scenery out of panels.

## Components

### Buttons
- **Shape:** square (0px).
- **Primary, "field":** solid Field Green with Cast Black mono caps, 15px × 22px padding, Phosphor arrow icons (bold, 13–14px) either side. It does not move: on hover it lightens to Field Green, Lit and a green line lights 12px below it.
- **Secondary, "ghost field":** a transparent terminal ghost (panel black at 60%) with a Ghost Border and Astral Ink caps; on hover the border and label turn Field Green.
- **Tertiary:** bare mono caps in ink-dim (`// view spec` style) that turn green on hover.
- **Focus:** 1px Field Green outline, 4px offset, on every link and button.
- **Compact:** in the nav, the field button runs at 10px × 16px with 10.5px type.

### Panels (cards / containers)
The terminal surface held in the field.
- **Corner Style:** square, with eight 10px Field Green corner ticks (60% at rest, 100% attended).
- **Background:** Panel Black with a faint green top gradient and 3px scanlines.
- **Border:** 1px Panel Border; Field Green, Dim when attended. Inside, 1px form-seam rules separate a mono header bar and footer from the content.
- **Shadow Strategy:** Panel rest; Panel attended on hover/focus.
- **Internal Padding:** 20px for telemetry panels, 24–32px for content panels.
- **Variants:** *lift* (interactive: rises 5px, face and border brighten, seam grows), *held* (seam always lit; one statement panel per section).
- **Motion:** 0.6s on the field ease (`cubic-bezier(0.16, 1, 0.3, 1)`) for lift; 0.35s for border, face and ticks.

### Navigation
- **Style:** fixed 56px bar under the status bar, Cast Black at 86% with backdrop blur and a form-seam bottom edge. Wordmark "NET" in Adventor bold 24px with a mono `// AI 2070` tag in ink-dim.
- **Items:** mono caps 10.5px, 0.16em, ink-dim; hover to Astral Ink. Active item is Astral Ink with a 1px Field Green underline grown from the center.
- **Action:** compact field button (Install) at the right; on phones it is the only nav item shown.

### Status Bar
A 28px mono strip (0.12em tracking) above the nav carrying live mesh telemetry, codename, version, build and SHA, with a green live dot.

### Stencil Label
Section markers are stencilled **vertically** down the section's right pillar (`writing-mode: vertical-rl`), 10px mono, 0.32em, ink-dim, led by a 24px ink-faint hairline. They never sit above the heading. The same stencil style marks panel IDs and the hero's document reference line.

### Section Title
The headline, split at its line breaks; each line rises 0.5em out of an 8px blur into place as the section scrolls into view, 110ms apart. Visible by default, and only lowered once script has confirmed it is below the fold.

### Hero Field (signature)
A full-bleed canvas of about 160 slowly turning concrete shards inside a diagonal light shaft. Twelve are mesh nodes, labelled in mono, linked by faint green threads with a packet travelling the path. The cursor pushes shards; a click launches them. Paired with an area title that rises in line by line (1.6s, staggered 140ms).

### Monolith
Decorative CSS 3D concrete cuboids that sway a few degrees and bob 14px over their own blurred floor shadow. Scenery, not panels: always `aria-hidden`, never interactive, and exempt from the float rule because they carry no data.

## Do's and Don'ts

### Do:
- **Do** scope new landing-world work under `.world-field` and build every text-bearing container from the `slab` utility, so the terminal face, corner ticks and raised faint ink (`#7f8479`) come with it.
- **Do** lay panels on aligned, equal grids, one column per peer, same span and same top edge.
- **Do** give a panel altitude only to encode rank, progress, propagation, shared state or attention.
- **Do** move on the field ease (`cubic-bezier(0.16, 1, 0.3, 1)`), and keep every float, wave, sync, rise and lift behind `prefers-reduced-motion`.
- **Do** label simulated telemetry as SIMULATED and measured figures as observed.
- **Do** make concrete scenery with the procedural SVG textures (`--tex-grain`, `--tex-stain`, `--tex-pits`), not raster images.

### Don't:
- **Don't** extend the legacy root theme (Major Mono Display, Space Grotesk, CRT scanlines, `#3df0ff` cyan, `#ff5e3d` warn) on any new surface; it survives only until /docs and /games migrate.
- **Don't** float, bob, offset or stagger a panel for variety; staggered spans, overlaps and edge bleeds are retired.
- **Don't** put text on concrete or build panels out of concrete.
- **Don't** use Field Green as a panel fill, section background or bright resting border.
- **Don't** round corners on panels, buttons or terminals.
- **Don't** use red, redaction bars, or `<<text>>` Board-style treatments.
- **Don't** set prose in mono or titles in Archivo.
- **Don't** wire, rail or frame panels together; only mesh nodes are joined, by green field threads.
