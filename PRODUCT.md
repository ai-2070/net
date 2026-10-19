# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Inferred from the repository (not interviewed): systems and infrastructure engineers, and builders of agentic / edge / robotics / vehicular software, evaluating whether to adopt Net as the transport and capability layer under their workloads. They arrive from GitHub, crates.io / npm / PyPI, or a link, read the landing page to decide whether Net is credible and different, then go to install or the docs.

## Product Purpose

Net (Network Event Transport, published as `net-mesh`) is a latency-first, end-to-end encrypted mesh where every computer, device, app, and agent is an equal node: no clients, no servers, no coordinators. The mesh propagates state, not connections. Flagship use: capability federation, meaning discovery, typed RPC, durable logs, and artifact transfer across a trusted mesh. Success for the site: a qualified engineer understands the claim and reaches install or the docs.

## Positioning

Nanosecond-scale scheduling on one Rust core with SDKs in Rust, TypeScript, Python, Go and a C ABI. Capability-addressed rather than host-addressed. Relays forward ciphertext they cannot read. Drop-instead-of-queue backpressure. Programs (daemons) migrate between machines with identity and state intact (Mikoshi). "Not anti-cloud. Post-cloud."

## Capabilities and Constraints

- Site lives in `web/` (Next.js 16, React 19, Tailwind 4, motion). Landing page is `src/app/page.tsx`; docs under `/docs`; a `/games` page also exists.
- Benchmark numbers, release notes and codename on the page come from the repo (`BENCHMARKS.md`, release notes, `getRepoInfo`); never invent new figures.
- Feature names are product vocabulary and stay: Mikoshi, Dataforts, MeshOS, Blackwall, NRPC, RedEX, CortEX, NetDB, daemons, pingwaves.

## Brand Commitments

- Name: NET / Net, tagline "// AI 2070". Release codenames are rock-song titles. The project's identity is Cyberpunk-universe inspired (Blackwall, Mikoshi, Dataforts names).
- Copy on the landing page is binding: the 2026-10 redesign changes visuals only, every word stays (user instruction, 2026-10-19).
- Redesign direction pinned by the user (2026-10-19): fuse the current cyber-led terminal look with the mysterious atmosphere and effects of Remedy's *Control* and *Control Resonant* (the games, not their website). Explicitly excluded: the Hiss and its red, redacted-document styling, and the Board's text treatment.

## Evidence on Hand

Real benchmark tables (M1 Max and i9-14900K), release notes per version, SDK install commands, protocol property list, code samples. No customer logos, testimonials, or adoption numbers exist; do not fabricate them. Live-looking telemetry on the page (EVT/SEC, P50, event tails) is simulated and labeled as such.

## Product Principles

1. Prove with mechanism and measured numbers, never adjectives.
2. Physics is the only acceptable latency; the design should feel fast.
3. Peers, not platforms: no central authority in the story or the visuals.
4. Honesty about simulated vs measured data.
