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
    fontSize: "clamp(50px, 8.3vw, 132px)"
    fontWeight: 700
    lineHeight: 0.86
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "clamp(30px, 5.2vw, 80px)"
    fontWeight: 700
    lineHeight: 0.95
    letterSpacing: "-0.03em"
  title:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "24px"
    fontWeight: 700
    lineHeight: 1.05
    letterSpacing: "-0.015em"
  lede:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "21px"
    fontWeight: 300
    lineHeight: 1.5
  body:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.6
  panel-body:
    fontFamily: "Archivo, Helvetica Neue, Arial, sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.55
  numeral:
    fontFamily: "TeX Gyre Adventor, Avant Garde, Century Gothic, sans-serif"
    fontSize: "26px"
    fontWeight: 700
    lineHeight: 1
    fontFeature: "tnum"
  label:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "0.28em"
  data:
    fontFamily: "JetBrains Mono, ui-monospace, Menlo, monospace"
    fontSize: "12px"
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
  panel-pad: "32px"
  control-pad: "15px 22px"
  grid-gap: "24px"
  frame-margin-sm: "24px"
  frame-margin: "48px"
  section-rhythm: "128px"
  prose-measure: "70ch"
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
    padding: "32px"
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

The mesh is a force field. The landing is a dark, raw-concrete brutalist hall, held up by nothing visible: concrete shards turn in one cold light shaft in the hero, Mikoshi's plinths and the Blackwall's pillars hang in it. Inside that hall the information sits on **terminal panels**: near-black phosphor glass with a hairline border, faint scanlines and green corner ticks, the project's cyberpunk HUD surface. Where the subject has a real world (the machines and places Net is meant to run in), it is shown with **real photographs** from open collections, held in the panels like evidence. Where the argument is a shape (1969's four hosts becoming a mesh with no centre), it is **drawn live** inside a terminal panel.

Float carries meaning. Panels sit flat and still on aligned grids by default; sequence is shown by a marker travelling a rail, not by height. A panel leaves the floor only when its height says something: altitude encodes speed (NET above real-time above best-effort), a travelling wave encodes propagation (the nine axioms, like a pingwave), a shared rhythm encodes one live system (the hero telemetry). Hovering a panel lifts it a few pixels and lights its seam: the field picks up what you attend to. Titles come into focus rather than appearing: each phrase is pulled from a soft blur into place, never hidden.

The world is cyber-led and keeps the project's terminal telemetry, mono data and acid green, and refuses the dark-neon dev-tool page where every card floats, glows and differs for no reason.

**Key Characteristics:**
- Three materials: procedural cast concrete for scenery, terminal glass for every text-bearing panel and live diagram, real pictures for the subject's world.
- Acid green carries one meaning: the field (live, held, attended, Net's side of a contrast, or the one primary action).
- Monumental geometric caps (TeX Gyre Adventor, Avant Garde lineage) for titles; Archivo for prose; JetBrains Mono for data, code, telemetry, labels and controls.
- Square corners, equal page margins, aligned equal grids; height above the floor is data, never ornament.

## Colors

A near-monochrome warm-black hall with greenish-black terminal panels and a single acid-green field signal. Pictures are the only full colour, and only under attention.

### Primary
- **Field Green** (`field-green`): the field. The primary control (Install), panel corner ticks, the 1969 seed nodes and links and the pingwave rings in the §01 mesh, the migration marker and the lit phase number, the seam under attended and held panels, focus outlines, link underlines and hover, live status dots, mesh threads and node glows on the canvas, Mikoshi transit cubes, the selection fill, and the frozen accent phrases in titles and copy.
- **Field Green, Lit** (`field-green-hover`): the primary button under the pointer.
- **Field Green, Dim** (`field-green-dim`): the field at low volume. The border of an attended panel or of the active migration phase, ASCII glyph diagrams, low-priority event types (`hb.ack`), the one remaining terminal window dot, scrollbar thumb hover.

### Neutral
- **Cast Black** (`cast-black`): the hall. Page ground, nav bar (86% with backdrop blur), hero scrims, text on the green button.
- **Hall Floor** (`hall-floor`): recessed bands, such as the Install band between top and bottom hairlines.
- **Panel Black** (`panel-black`): the face of every panel and the ground of the live diagrams. **Panel Black, Attended** (`panel-black-hover`) is the face under hover/focus.
- **Panel Border** (`panel-border`): the 1px edge of every panel at rest.
- **Ghost Border** (`ghost-border`): the 1px edge of the secondary (ghost) button.
- **Astral Ink** (`astral-ink`): titles, running prose, panel headings.
- **Ink Dim** (`ink-dim`): panel body text, labels, captions-of-data, the footer credit line.
- **Ink Faint** (`ink-faint`): tertiary marks on the ground (the hairline beside a stencil label).
- **Ink Faint on Panel** (`ink-faint-on-panel`): tertiary text inside a panel (timestamps, placeholder metrics). The `slab` utility re-declares `--color-ink-faint` to this value.
- **Form Seam** (`form-seam`): 1px hairlines: nav bottom edge, rules inside panels, band edges, the footer's top edge, the Topology floor line.
- **Shaft Light** (`shaft-light`): the light shaft and highlights on concrete scenery, always at low alpha.

### Tertiary (telemetry only)
- **Telemetry Amber** (`telemetry-amber`): drops, evictions, Blackwall rejections in event logs. Warnings only.
- **Telemetry Ice** (`telemetry-ice`): capability reads and peer joins in event logs.

### Named Rules
**The One Meaning Rule.** Field Green means the field: live, held, attended, Net's side of a contrast in the copy ("net assumes abundance.", "MOVES", "post-cloud."), or the one primary action. On a panel it appears only as the corner ticks (60% at rest, full when attended), the dim border when attended, and the seam. It is never a panel fill, a section background or a bright border at rest.

**The Panel Ink Rule.** Inside a panel, only the faint ink is raised (`ink-faint-on-panel`); dim and primary ink keep their ground values. Get it by putting text inside the `slab` utility, not by hand-picking a grey.

**The No Red Rule.** The palette has no red. Warnings are amber. Inherited terminal window dots are neutralised to grey and dim green inside the world.

## Typography

**Display Font:** TeX Gyre Adventor (self-hosted Latin subset, `web/src/fonts/adventor/`, GUST Font License; fallback Avant Garde, Century Gothic, sans-serif)
**Body Font:** Archivo (fallback Helvetica Neue, Arial, sans-serif)
**Label/Mono Font:** JetBrains Mono (fallback ui-monospace, Menlo, monospace)

**Character:** Monumental, tightly tracked geometric caps carry the architecture; a quiet Swiss-lineage grotesque carries the argument; mono is the instrument readout. Three voices, never swapped.

### Hierarchy
- **Display** (700, `clamp(50px, 8.3vw, 132px)`, 0.86, -0.04em, uppercase): the hero area title only, set on exactly two lines ("NET. MOVES" / "AT LIGHT.").
- **Headline** (700, `clamp(30px, 5.2vw, 80px)`, 0.95, -0.03em, uppercase, max 19em, balanced): every section title. The copy's hard line breaks become spaces, so a title flows onto at most two lines.
- **Title** (700, 23–24px, 1.05–1.08, -0.015em): headings inside panels.
- **Numeral** (700, 26px, 1.0, tabular): measured figures in stat panels, with a mono unit in ink-dim beside them.
- **Lede** (300, 19–22px, 1.45–1.5): the opening paragraph of the hero and of a section, in Astral Ink.
- **Body** (400, 17px, 1.6, Astral Ink): running prose.
- **Panel Body** (400, 16px, 1.55, ink-dim): prose inside panels.
- **Release notes** (Archivo prose at 16px, `prose-invert` base): release bodies.
- **Label / Stencil** (400, 12px, 0.28em, uppercase, mono, ink-dim): section markers, panel IDs, document refs, footer meta.
- **Data** (400, 12–13px, mono): event tails, code, ASCII diagrams, panel header bars. The smallest data steps are 12–13px; the remaining 9–11px sizes are controls, units and telemetry header bars.
- **Control** (600–700, 11px, 0.16em, uppercase, mono): buttons; nav items at 10.5px.

### Named Rules
**The Three Voices Rule.** Adventor for structure, Archivo for sentences, JetBrains Mono for anything a machine would print. Never set prose in mono, never set a number readout in Archivo.

**The Two Line Rule.** A section title runs on one or two lines, never a stack. The hero title is exactly two.

**The Focus Pull Rule.** A title is never invisible. Below the fold it waits slightly soft and low (0.35em down, 55% opacity, 6px blur) and is pulled into focus phrase by phrase as it enters view; without script it is simply sharp.

## Layout

The landing runs in a centered 1440px column under a fixed 28px status bar and a fixed 56px nav (main content offset 84px). Every band, the hero, nav, status bar, sections and footer, shares **equal outer margins**: 24px on phones, 48px at ≥1024px, left and right alike. Sections keep a vertical rhythm of 96px (128px at ≥1024px). Prose is capped at 70ch.

Section labels sit **in the right margin**, stencilled vertically, at ≥1024px; below that they lie flat at the section's foot (24px in from each side, 36px from the bottom), wrapping if needed. Never above the heading.

Panels sit on **aligned, equal grids** with 20–24px gaps: one column on phones, two at ≥760px, and at ≥1024px as many as the set has peers (axioms 3, applications 2, components 4, group cards 3, topology classes 3, migration phases 6 on one baseline). Every panel in a set has the same span and the same top edge unless its height encodes something (see Elevation & Depth). Nothing overlaps; nothing bleeds off the page edge.

§01 splits into two equal columns at ≥1024px: argument and statement panel on the left; on the right the Scarcity Mesh panel, sticky 112px from the top so it stays in view while the argument scrolls past it.

The hero is a split: text on the left (title, lede, actions, stencil ref line pinned to the bottom), a 400px column on the right with two telemetry panels moving as one, and a full-bleed canvas field behind both, darkened on the text side by a horizontal scrim. On phones the panels drop out and a heavier vertical scrim sits over the field.

Breakpoints are the shared Tailwind set: 640, 760, 1024, 1280, 1440px.

## Elevation & Depth

Panels are **flat** at rest: no shadow, no top gradient, scanlines only. A shadow appears only when a panel is actually off the floor: a floating set (the axiom wave, the shared-rhythm panels) or a panel lifted by attention. Concrete scenery carries the hall's atmosphere: the hero shards turning in the light shaft, Mikoshi's plinths swaying over their floor shadows, the Blackwall pillars, and a fixed 20%-opacity film grain over the page.

### Shadow Vocabulary
- **Floating** (`0 22px 28px -24px rgba(0,0,0,0.9)`): panels inside the axiom wave or on the shared rhythm.
- **Attended** (`inset 0 1px 0 rgba(196,255,61,0.08), 0 34px 34px -26px rgba(0,0,0,0.95)`): a panel lifted on hover/focus, paired with a 5px rise.
- **Seam glow** (`0 8px 18px 1px rgba(196,255,61,0.22)`): spill under the seam of an attended panel.
- **Held seam glow** (`0 10px 22px 1px rgba(196,255,61,0.2)`): spill under the seam of a held panel.
- **Blackwall wordmark** (`text-shadow: 0 2px 0 rgba(0,0,0,0.8), 0 0 40px rgba(11,12,11,0.9)`): keeps the BLACKWALL wordmark legible over its concrete pillars. That element only.

### Float and sequence encodings in use
- **Rank (altitude = speed):** the topology classes share a floor hairline; best-effort rests on it, real-time sits 40px up, NET sits 96px up, held, on the shared rhythm.
- **Progress (rail, not altitude):** the six migration phases stand as equal panels on one baseline under a single hairline rail (≥1024px). A 7px green marker crosses the rail in 6s; each phase lights its border dim green and its number green during its sixth of the loop. No shadow, no rise.
- **Propagation (wave):** the nine axioms carry one 9px wave in reading order, 0.28s apart over 7s.
- **One system (sync):** the hero telemetry panels rise and settle together on one 9s, 8px rhythm.
- **Attention (lift):** any interactive panel rises 5px while hovered or focused.

### Named Rules
**The Meaningful Float Rule.** A panel leaves the floor only when its height encodes something: rank, propagation, shared state, or the visitor's attention. Sequence and progress travel along a rail instead. If you can't say what the height means, the panel rests. No random bobs, no per-card offsets, no idle drift on panels. All of it stops under `prefers-reduced-motion`.

**The Shadow Follows Height Rule.** A panel on the floor casts no shadow. Shadow is the evidence of altitude, so it appears exactly where a panel floats and nowhere else; the primary button is flat.

**The Seam Rule.** The field shows itself on a panel as a single 1px green line along its bottom edge: grown from 30% width when attended, or always lit when held. Held is reserved for the statement panel of a section.

**The Held Not Hung Rule.** Panels are never wired to each other, railed or framed together; a panel's relationship to the page is its grid position. The only lines that join things are field threads between mesh nodes (the hero canvas, the Mikoshi route), and they are green because they are the field.

## Shapes

Square-cornered throughout (0px). Panels, buttons, nav, terminals, code blocks and picture frames are rectangles; the only round forms are 6–7px status dots. A panel is defined by its 1px border plus eight 10px green corner ticks drawn just outside the border, and faint 3px scanlines. Pictures are cropped to 16:7 panel heads and run edge to edge across the top of their panel. Concrete scenery is the other form family: CSS 3D cuboids and pillars with grain, stain and pit textures.

### Named Rules
**The Three Materials Rule.** Concrete is for scenery (canvas shards, Mikoshi plinths, Blackwall pillars). Terminal glass is for anything that carries words or numbers. Real pictures are for the subject's world (places, machines). Live diagrams are drawn on canvas inside a terminal panel. Never put text on concrete, never make scenery out of panels.

## Components

### Buttons
- **Shape:** square (0px), flat: no shadow, no inset highlight.
- **Primary, "field":** solid Field Green with Cast Black mono caps, 15px × 22px padding, Phosphor arrow icons (bold, 13–14px) either side. It does not move: on hover it lightens to Field Green, Lit and a green line lights 12px below it.
- **Secondary, "ghost field":** a transparent terminal ghost (panel black at 60%) with a Ghost Border and Astral Ink caps; on hover the border and label turn Field Green.
- **Tertiary:** bare mono caps in ink-dim (`// view spec` style) that turn green on hover.
- **Focus:** 1px Field Green outline, 4px offset, on every link and button.
- **Compact:** in the nav, the field button runs at 10px × 16px with 10.5px type.

### Panels (cards / containers)
The terminal surface held in the field.
- **Corner Style:** square, with eight 10px Field Green corner ticks (60% at rest, 100% attended).
- **Background:** Panel Black with 3px scanlines.
- **Border:** 1px Panel Border; Field Green, Dim when attended. Inside, 1px form-seam rules separate a mono header bar and footer from the content.
- **Shadow Strategy:** none at rest; Floating inside a floating set; Attended on hover/focus.
- **Internal Padding:** 20px for telemetry panels, 24–32px for content panels.
- **Variants:** *lift* (interactive: rises 5px, face and border brighten, seam grows), *held* (seam always lit; one statement panel per section).
- **Interface fragments:** panels that reproduce Net's real interface (hero `mesh.proximity` and `event.tail`, the §01 Scarcity Mesh, the Dataforts console, the MeshOS stage, the daemon code block) carry a `data-ui` attribute. They are product UI, so they keep their terminal internals; simulated feeds say SIMULATED.
- **Motion:** 0.6s on the field ease (`cubic-bezier(0.16, 1, 0.3, 1)`) for lift; 0.35s for border, face and ticks.

### Pictures
Real images of the subject's world, sourced from open collections (Wikimedia Commons, rawpixel and similar), stored in `web/public/field/`.
- **Provenance:** every raster carries its source and licence (embedded, or a `.json` sidecar for WebP), and is listed in `web/src/lib/field-credits.json`, which renders as the footer's "Pictures:" credit line (13px, ink-dim, each title linked to its source page).
- **Application panel head:** a 16:7 photo running full-bleed across the top of the panel, grayscale, contrast 110%, brightness 70% at rest; full colour (brightness 90%) when the panel is hovered, over 0.5s. On the one focusable card (Multiplayer Games) keyboard focus restores colour too.
- **Games still:** the Multiplayer Games card is a link to /games and its head is a still frame from the project's own Rose & Blade gameplay reel (`/field/games-reel-poster.jpg`, an ffmpeg frame with embedded provenance), treated like every other application picture: grayscale at rest, colour on hover and focus. All eight application heads are stills; the page carries no video.
- **Text:** no captions on the page; the alt text says what each picture is.

### Named Rules
**The Witness Rule.** Sourced pictures show the world Net lives in, never Net itself. A sourced photo is never presented as Net's product, customer, deployment or team, and it never appears without its credit and licence. The only image of Net's own product is the Rose & Blade frame on the Multiplayer Games card, which is the project's own footage and links to /games.

### Scarcity Mesh (signature)
The §01 diagram, a `data-ui` terminal panel with a header reading `arpanet → net`, a year readout that flips from 1969 to now, and a live node counter. Inside, a scroll-linked canvas starts from the four real December 1969 ARPANET nodes (UCLA, SRI, UCSB, Utah) and their real links (UCLA–SRI, UCLA–UCSB, SRI–UCSB, SRI–Utah), drawn in Field Green with mono labels. As the section scrolls it grows outward into a dense, flat lattice where each new node links to its two or three nearest earlier peers, and the four seeds fade to ordinary size: no centre. When the mesh is full, green pingwave rings ripple through it. It pauses off-screen. Under reduced motion it shows one still of the full mesh with the four seeds still marked.

### Migration Rail
An ordered list of six equal phase panels on one baseline. At ≥1024px a single form-seam hairline runs above them and a 7px Field Green marker (with a faint 3px green halo) crosses it in 6s, linear. Each phase lights its border Field Green, Dim and its 28px Adventor number Field Green during its sixth of the loop; at rest the number is ink-dim. Flat: no shadow, no rise. Static under reduced motion.

### Navigation
- **Style:** fixed 56px bar under the status bar, on the shared 24/48px margins, Cast Black at 86% with backdrop blur and a form-seam bottom edge. Wordmark "NET" in Adventor bold 24px with a mono `// AI 2070` tag in ink-dim.
- **Items:** mono caps 10.5px, 0.16em, ink-dim; hover to Astral Ink. Active item is Astral Ink with a 1px Field Green underline grown from the center.
- **Action:** compact field button (Install) at the right; on phones it is the only nav item shown.

### Status Bar
A 28px mono strip (0.12em tracking) above the nav on the shared margins, carrying live mesh telemetry, codename, version, build and SHA, with a green live dot.

### Stencil Label
Section markers in 12px mono at 0.28em, ink-dim, led by a 24px ink-faint hairline: vertical in the right margin at ≥1024px, flat at the section's foot below that. The same stencil style marks panel IDs, the hero's document reference line and the footer meta row.

### Section Title
The headline, flowing on one or two lines; each phrase of the copy is pulled into focus on its own 110ms beat as the section scrolls into view (see the Focus Pull Rule).

### Footer
A plain form-seam hairline across the full width, then the wordmark (Adventor 40px), a one-line description, three link columns, the Pictures credit line and a stencil meta row.

### Hero Field (signature)
A full-bleed canvas of about 160 slowly turning concrete shards inside a diagonal light shaft. Twelve are mesh nodes, labelled in mono, linked by faint green threads with a packet travelling the path. The cursor pushes shards; a click launches them. Paired with a two-line area title that rises in phrase by phrase (1.6s, staggered).

## Do's and Don'ts

### Do:
- **Do** scope new landing-world work under `.world-field` and build every text-bearing container from the `slab` utility, so the terminal face, corner ticks and raised faint ink (`#7f8479`) come with it.
- **Do** keep every band on the shared margins (24px / 48px at ≥1024px), prose at or under 70ch, and panels on aligned, equal grids.
- **Do** give a panel altitude, and its shadow, only to encode rank, propagation, shared state or attention; show sequence with a marker on a rail.
- **Do** show the subject's real world with sourced, licensed pictures, credit each one in `field-credits.json`, and give each an alt text that says what it is.
- **Do** move on the field ease (`cubic-bezier(0.16, 1, 0.3, 1)`), and keep every float, wave, sync, focus pull and lift behind `prefers-reduced-motion`.
- **Do** label simulated telemetry as SIMULATED and mark real interface fragments with `data-ui`.
- **Do** keep the brand's deliberate terminal vocabulary, even where generic taste rules advise against it: mono telemetry, tracked uppercase labels, hairline borders and outlined panels with corner ticks, scanline texture, the frozen 01–06 / P.0n / § labels and panel IDs that the copy already carries, and green accent phrases in titles.

### Don't:
- **Don't** extend the legacy root theme (Major Mono Display, Space Grotesk, CRT scanlines, `#3df0ff` cyan, `#ff5e3d` warn) on any new surface; it survives only until /docs and /games migrate.
- **Don't** float, offset, stagger or shadow a panel for variety; a panel on the floor is flat.
- **Don't** present a picture as Net's product, customer or deployment, caption it with invented claims, or ship one without its source and licence.
- **Don't** stack a title over three or more lines, or hide a title entirely before it lifts.
- **Don't** put text on concrete or build panels out of concrete.
- **Don't** use Field Green as a panel fill, section background or bright resting border.
- **Don't** invent new numbered labels or kickers; the § / 01–06 / P.0n markers are frozen copy, carried, not a pattern to extend.
- **Don't** round corners, use red, redaction bars, or `<<text>>` Board-style treatments.
- **Don't** set prose in mono or titles in Archivo.
