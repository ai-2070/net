"use client";

import { useState } from "react";

const NAV_LINKS = [
  { href: "#why", label: "Why" },
  { href: "#stack", label: "Stack" },
  { href: "#benchmarks", label: "Benchmarks" },
  { href: "#daemons", label: "Daemons" },
  { href: "#data", label: "Local data" },
  { href: "#applications", label: "Applications" },
  { href: "#install", label: "Install" },
];

const HERO_STATS = [
  { num: "1.98 ns", label: "Header serialize" },
  { num: "53 ns", label: "Per-hop forward" },
  { num: "288 ns", label: "Fail + recover cycle" },
  { num: "1.92 MB", label: "Deployed binary" },
];

const INVERT_LEFT = [
  "Queues at every hop absorb bursts",
  "Connections as the primary object",
  "Brokers (Kafka, Pulsar) hold plaintext at fixed addresses",
  "Cooperative — assumes goodwill",
  "TCP backpressure measured in round trips",
  "Millisecond budgets at the software layer",
];

const INVERT_RIGHT: React.ReactNode[] = [
  <>
    Bounded ring buffers; queues are <em>failure</em>
  </>,
  <>State propagates; connections are ephemeral</>,
  <>
    Bus has no location — it <em>is</em> the mesh
  </>,
  <>Trust derived from observed behavior</>,
  <>Backpressure is silence; reroute is the response</>,
  <>Nanosecond scheduling on commodity HW</>,
];

const PROPS = [
  {
    title: "Latency-first",
    body: "Sub-2 ns header serialize. Per-hop forwarding in nanoseconds. The floor is low enough that scheduling operates at function-call timescales.",
  },
  {
    title: "Streaming-first",
    body: "Data is a continuous flow, not documents. Sharded ring buffers and adaptive batching assume incremental production and consumption. No requests, no responses — streams.",
  },
  {
    title: "Zero-copy",
    body: "Ring buffers, no GC, native Rust. Forwarding doesn't allocate or copy payload data. This is what makes per-hop numbers possible.",
  },
  {
    title: "Encrypted end-to-end",
    body: "Noise NK handshakes for key exchange. ChaCha20-Poly1305 with counter nonces. Every packet is encrypted between source and destination. Intermediate nodes never see plaintext.",
  },
  {
    title: "Untrusted relay",
    body: "Nodes forward packets without decrypting payloads. The mesh can route through infrastructure you don't trust — the network grows through adversarial nodes.",
  },
  {
    title: "Schema-agnostic",
    body: "The transport moves bytes, not structures. The protocol never inspects content. Two nodes can negotiate typed streams while the rest of the mesh passes opaque bytes.",
  },
  {
    title: "Optionally ordered",
    body: "Ordering is per-stream, per-entity — not global. The unordered path is the fast path. Causal ordering is available when streams need it.",
  },
  {
    title: "Native backpressure",
    body: "Nodes drop packets without reply. The proximity graph makes silence a signal, not an error. Neighbors notice within a heartbeat interval.",
  },
];

const ARCH_ROWS = [
  {
    name: "Transport",
    desc: "Encrypted UDP, zero-alloc packet pools, multi-hop forwarding, adaptive batching, fair scheduling, failure detection, pingwave swarm discovery",
    mech: "ChaCha20-Poly1305 / Noise NK",
  },
  {
    name: "Identity",
    desc: "ed25519 entity identity, origin binding on every packet, permission tokens with delegation chains",
    mech: "ed25519 + BLAKE2s origin hashes",
  },
  {
    name: "Channels",
    desc: "Named hierarchical channels, capability-based access control, bloom-filter authorization at ~20 ns per packet",
    mech: "AuthGuard bloom + verified cache",
  },
  {
    name: "Behavior plane",
    desc: "Capability announcements, capability diffs, API schema registry, device autonomy rules, distributed tracing, load balancing, proximity graph",
    mech: "CapabilityIndex + RuleEngine",
  },
  {
    name: "Subnets",
    desc: "4-level (region/fleet/vehicle/subsystem) hierarchy, label-based assignment, gateway visibility enforcement",
    mech: "8/8/8/8 SubnetId encoding",
  },
  {
    name: "Distributed state",
    desc: "24-byte causal links, compressed observed horizons, append-only entity logs, state snapshots for migration",
    mech: "Per-entity causal chain",
  },
  {
    name: "Compute runtime",
    desc: "MeshDaemon trait, capability-based placement, 6-phase migration with snapshot chunking, replica + fork + standby groups",
    mech: "DaemonHost + PlacementScheduler",
  },
  {
    name: "Subprotocols",
    desc: "Formal protocol registry, version negotiation, capability-aware routing via tags, opaque forwarding guarantee",
    mech: "16-bit subprotocol_id space",
  },
  {
    name: "Continuity",
    desc: "Causal cones, 36-byte continuity proofs, honest discontinuity with deterministic forking, superposition during migration",
    mech: "Fork records, lineage sentinels",
  },
  {
    name: "Contested envs",
    desc: "Correlated failure detection, subnet-aware partition classification, partition healing with log reconciliation",
    mech: "Longest chain + deterministic tiebreak",
  },
  {
    name: "Local data",
    desc: "RedEX append-only log, CortEX folded state, NetDB query façade. Local, per-node, and entirely optional",
    mech: "redex / cortex / netdb features",
  },
];

const WIRE_ROWS = [
  { off: "0x00", name: "MAGIC (0x4E45), VERSION, FLAGS", size: "4 B" },
  { off: "0x04", name: "PRIORITY, HOP_TTL, HOP_COUNT, FRAG", size: "4 B" },
  { off: "0x08", name: "SUBPROTOCOL_ID, CHANNEL_HASH", size: "4 B" },
  { off: "0x0C", name: "NONCE", size: "12 B" },
  { off: "0x18", name: "SESSION_ID", size: "8 B" },
  { off: "0x20", name: "STREAM_ID", size: "8 B" },
  { off: "0x28", name: "SEQUENCE", size: "8 B" },
  { off: "0x30", name: "SUBNET_ID, ORIGIN_HASH", size: "8 B" },
  { off: "0x38", name: "FRAGMENT_ID, FRAGMENT_OFFSET", size: "4 B" },
  { off: "0x3C", name: "PAYLOAD_LEN, EVENT_COUNT", size: "4 B" },
];

const BENCH = [
  {
    num: "505",
    unit: "M ops/s",
    label: "Header serialize",
    meta: "1.98 ns · M1 Max — 762 M/s on i9",
  },
  {
    num: "5.06",
    unit: "G ops/s",
    label: "Routing header forward",
    meta: "0.20 ns on i9-14900K",
  },
  {
    num: "3.21",
    unit: "G ops/s",
    label: "GPU capability check",
    meta: "0.31 ns — inline per-packet",
  },
  {
    num: "53",
    unit: "ns",
    label: "Per-hop forwarding",
    meta: "i9 1-hop — linear to 5+ hops",
  },
  {
    num: "288",
    unit: "ns",
    label: "Full fail + recover cycle",
    meta: "Mark, evaluate, reroute — M1 Max",
  },
  {
    num: "10",
    unit: "M+ evt/s",
    label: "Sustained event ingestion",
    meta: "< 1 μs p99 through the EventBus",
  },
  {
    num: "6.97",
    unit: "M evt/s",
    label: "Python batch ingestion",
    meta: "PyO3 binding releases the GIL on FFI",
  },
  {
    num: "6.31",
    unit: "M evt/s",
    label: "Go raw ingestion",
    meta: "158 ns/event · zero allocations",
  },
  {
    num: "1.07",
    unit: "G ops/s",
    label: "Pingwave roundtrip",
    meta: "0.93 ns swarm-discovery primitive",
  },
];

const APPS = [
  {
    icon: "AI",
    title: "AI runtime",
    body: "Token streams, tool-call results, guardrail decisions, and consensus votes flowing across heterogeneous GPU nodes. Compute-heavy inference routes to whichever node has capacity. The mesh is the runtime.",
  },
  {
    icon: "VS",
    title: "Vehicular sensor mesh",
    body: "Cars sharing LIDAR, radar, camera. Vehicles sync intent — braking, turning, route changes — nanoseconds before the brake pads touch the rotor.",
  },
  {
    icon: "RB",
    title: "Robotics & factory floors",
    body: "Mesh routes through whatever's reachable — a robot behind a steel column relays through one that isn't. Sub-microsecond reroute on node failure. No central controller, no SPOF.",
  },
  {
    icon: "DR",
    title: "Disaster response",
    body: "Phones, drones, portable radios forming a mesh with no surviving infrastructure. Each device contributes what it has. The mesh forms from whatever is present.",
  },
  {
    icon: "RS",
    title: "Remote surgery",
    body: "Control signals and haptic feedback routed across the mesh. If the primary compute node lags, the mesh reroutes mid-operation. The surgeon doesn't notice. The scalpel doesn't stop.",
  },
  {
    icon: "DS",
    title: "Drone swarms",
    body: "Coordinated flight without a ground controller. A drone that loses a motor broadcasts the failure; the swarm adjusts formation before the drone has begun to fall.",
  },
  {
    icon: "LP",
    title: "Live performance",
    body: "Lighting, audio, video, pyrotechnics synced across hundreds of nodes on a stage rig. A DMX controller dies, another node picks up the cue list. No show stop.",
  },
  {
    icon: "AG",
    title: "Precision agriculture",
    body: "Tractors, drones, soil sensors, weather stations forming a field mesh. A tractor that detects a soil condition shares it; every other tractor adjusts seeding without round-tripping to a cloud.",
  },
  {
    icon: "MP",
    title: "Multiplayer gaming",
    body: "State propagates peer-to-peer with causal ordering. Capability-aware routing means physics and world state route toward the gaming PC, not the phone. Ping is meaningless — there's no fixed server.",
  },
];

const STATUS_CARDS = [
  { v: "~1,573", l: "Rust unit tests" },
  { v: "~469", l: "Integration + SDK tests" },
  { v: "62 + 190", l: "Node + Python SDK smoke" },
  { v: "1.92 MB", l: "Core cdylib (release LTO)" },
];

type TabKey = "rust" | "ts" | "py" | "go" | "c";

const INSTALL_TABS: { key: TabKey; label: string }[] = [
  { key: "rust", label: "Rust" },
  { key: "ts", label: "TypeScript" },
  { key: "py", label: "Python" },
  { key: "go", label: "Go" },
  { key: "c", label: "C" },
];

const INSTALL_PANES: Record<TabKey, React.ReactNode> = {
  rust: (
    <>
      <span className="text-dim"># Rust SDK</span>
      {"\n"}cargo add <span className="text-accent2">ai2070-net-sdk</span>
      {"\n\n"}
      <span className="text-dim"># Lower-level core (skip SDK ergonomics)</span>
      {"\n"}cargo add <span className="text-accent2">ai2070-net</span>
    </>
  ),
  ts: (
    <>
      <span className="text-dim"># TypeScript / Node SDK</span>
      {"\n"}npm install <span className="text-accent2">@ai2070/net-sdk</span>{" "}
      <span className="text-accent2">@ai2070/net</span>
      {"\n\n"}
      <span className="text-dim"># Lower-level NAPI binding</span>
      {"\n"}npm install <span className="text-accent2">@ai2070/net</span>
    </>
  ),
  py: (
    <>
      <span className="text-dim"># Python SDK</span>
      {"\n"}pip install <span className="text-accent2">ai2070-net-sdk</span>
      {"\n\n"}
      <span className="text-dim"># Lower-level PyO3 binding</span>
      {"\n"}pip install <span className="text-accent2">ai2070-net</span>
    </>
  ),
  go: (
    <>
      <span className="text-dim"># Go binding</span>
      {"\n"}go get{" "}
      <span className="text-accent2">github.com/ai-2070/net/go</span>
    </>
  ),
  c: (
    <>
      <span className="text-dim"># C SDK — build cdylib + bundled header</span>
      {"\n"}cargo build --release --features ffi,net
      {"\n\n"}
      <span className="text-dim"># Result: libnet.dylib + include/net.h</span>
    </>
  ),
};

const BLACKWALL = [
  {
    tag: "No plaintext on relays",
    title: "Nothing to sniff.",
    body: "Zero-copy forwarding means relay nodes pass encrypted bytes through without decrypting. There is no moment where the payload is readable in memory on an untrusted node. Compromise of a relay leaks encrypted bytes with no key material — session keys are between source and destination.",
  },
  {
    tag: "No clock dependency",
    title: "Causal, not temporal.",
    body: "Net has no dependency on wall clocks, NTP, or synchronized time. Event ordering is causal — parent hashes, sequence numbers, vector clocks. An attacker who poisons NTP, spoofs GPS, or skews clocks across a subnet cannot disrupt ordering. A captured tower broadcasting adversarial timestamps disrupts clock-dependent protocols. Net is unaffected.",
  },
  {
    tag: "No connection state to hijack",
    title: "Defense in depth at the protocol.",
    body: "No TCP session to take over, no cookie to steal, no sequence number to predict. Backpressure, bounded queues, fanout limits, deduplication, TTL, and rate limiting compose. Any single mechanism can be overwhelmed; their composition forms the wall. There is no single point to breach because the wall is the mesh.",
  },
];

function Eyebrow({ children }: { children: React.ReactNode }) {
  return (
    <span className="font-mono text-xs tracking-[0.14em] uppercase text-accent inline-flex items-center gap-2 before:content-[''] before:w-1.5 before:h-1.5 before:rounded-full before:bg-accent before:shadow-[0_0_12px_var(--color-accent)]">
      {children}
    </span>
  );
}

function SectionHead({
  eyebrow,
  title,
  right,
}: {
  eyebrow: React.ReactNode;
  title: React.ReactNode;
  right: React.ReactNode;
}) {
  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 lg:gap-16 lg:items-end">
      <div>
        <Eyebrow>{eyebrow}</Eyebrow>
        <h2 className="text-[clamp(30px,3.6vw,46px)] leading-[1.08] tracking-[-0.02em] font-semibold mt-5">
          {title}
        </h2>
      </div>
      <div className="text-muted text-base">{right}</div>
    </div>
  );
}

export default function Home() {
  const [tab, setTab] = useState<TabKey>("rust");

  return (
    <div className="bg-deep text-ink">
      {/* NAV */}
      <nav className="sticky top-0 z-50 nav-glass border-b border-line">
        <div className="max-w-[1200px] mx-auto px-7 flex items-center justify-between h-16">
          <a className="inline-flex items-center gap-2.5 font-semibold tracking-[-0.01em]" href="#">
            <span className="logo-mark" />
            <span>Net</span>
          </a>
          <div className="hidden lg:flex gap-7 text-sm text-muted">
            {NAV_LINKS.map((l) => (
              <a key={l.href} href={l.href} className="hover:text-ink transition-colors">
                {l.label}
              </a>
            ))}
          </div>
          <a
            href="#install"
            className="font-mono text-xs px-3.5 py-2 border border-line2 rounded-full text-ink transition-all hover:border-accent hover:text-accent"
          >
            Get started →
          </a>
        </div>
      </nav>

      {/* HERO */}
      <header className="relative overflow-hidden hero-bg pt-[140px] pb-[120px]">
        <div className="hero-grid-bg" />
        <div className="relative max-w-[1200px] mx-auto px-7">
          <div className="inline-flex gap-2 items-center mb-5 font-mono text-[11px] text-dim tracking-[0.06em]">
            <span className="w-1.5 h-1.5 rounded-full bg-accent2 shadow-[0_0_8px_var(--color-accent2)] animate-pulse-soft" />
            <span>NETWORK EVENT TRANSPORT</span>
            <span className="text-dim">·</span>
            <span>v0 · working protocol</span>
          </div>
          <Eyebrow>
            Existing networks operate in 10⁻³.{" "}
            <span className="text-muted">Net operates in 10⁻⁹.</span>
          </Eyebrow>
          <h1 className="text-[clamp(40px,6.4vw,84px)] leading-[1.02] tracking-[-0.03em] font-semibold mt-6">
            The internet was built
            <br />
            for scarcity. <span className="grad-text">Net</span>
            <br />
            is built for abundance.
          </h1>
          <p className="text-[clamp(17px,1.5vw,20px)] leading-[1.55] text-muted max-w-[64ch] mt-[22px]">
            A latency-first encrypted mesh. Every computer, device, and application is an
            equal node on a flat topology — no clients, no servers, no coordinators. The
            mesh propagates state, not connections. Real-time scheduling latencies on
            commodity hardware over commodity networks.
          </p>
          <div className="mt-10 inline-flex gap-3 flex-wrap">
            <a
              href="#install"
              className="group inline-flex items-center gap-2 px-5 py-3 rounded-[10px] font-mono text-[13px] font-medium border bg-ink text-deep border-ink transition-all hover:bg-accent hover:border-accent hover:-translate-y-px"
            >
              Install{" "}
              <span className="transition-transform group-hover:translate-x-[3px]">→</span>
            </a>
            <a
              href="#benchmarks"
              className="inline-flex items-center gap-2 px-5 py-3 rounded-[10px] font-mono text-[13px] font-medium border border-line2 transition-all hover:border-ink"
            >
              See the benchmarks
            </a>
          </div>

          <div className="mt-20 grid grid-cols-2 lg:grid-cols-4 border-t border-b border-line">
            {HERO_STATS.map((s, i) => (
              <div
                key={s.label}
                className={`py-7 ${
                  i % 2 === 0
                    ? "pr-6 border-r border-line lg:border-r"
                    : "pl-6 lg:pl-0 lg:pr-6 lg:border-r lg:border-line"
                } ${i === 1 ? "lg:pl-0" : ""} ${
                  i === HERO_STATS.length - 1 ? "lg:border-r-0" : ""
                } ${i === 0 ? "lg:pl-0" : ""}`}
              >
                <div className="font-mono text-[28px] tracking-[-0.02em] text-ink font-medium">
                  {s.num}
                </div>
                <div className="text-xs text-dim mt-1.5 font-mono tracking-[0.04em]">
                  {s.label}
                </div>
              </div>
            ))}
          </div>
        </div>
      </header>

      {/* WHY */}
      <section id="why" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Why not best-effort"
            title={
              <>
                ARPANET assumed scarcity.
                <br />
                Net assumes abundance.
              </>
            }
            right={
              <>
                TCP guarantees delivery to a buffer. It does not guarantee the receiver can
                act on it in time. In a world of abundant data, abundant nodes, abundant
                bandwidth, and continuous external pressure, a delivery guarantee becomes a
                liability — you're promising to deliver data that will bury the receiver.
                The bottleneck isn't delivery. It's processing.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 md:grid-cols-2 border border-line rounded-[14px] overflow-hidden bg-panel">
            <div className="p-8 md:p-9 border-b md:border-b-0 md:border-r border-line">
              <h3 className="text-dim font-mono text-xs tracking-[0.14em] uppercase font-medium mb-5">
                Best-effort networks (TCP/IP, HTTP, gRPC)
              </h3>
              <ul className="list-none m-0 p-0">
                {INVERT_LEFT.map((line, i) => (
                  <li
                    key={i}
                    className={`py-3 flex items-start gap-2.5 text-[15px] text-dim ${
                      i === 0 ? "pt-0" : "border-t border-dashed border-line"
                    }`}
                  >
                    <span className="text-[#ff6b7a]">×</span> {line}
                  </li>
                ))}
              </ul>
            </div>
            <div className="p-8 md:p-9">
              <h3 className="text-dim font-mono text-xs tracking-[0.14em] uppercase font-medium mb-5">
                Net
              </h3>
              <ul className="list-none m-0 p-0">
                {INVERT_RIGHT.map((row, i) => (
                  <li
                    key={i}
                    className={`py-3 flex items-start gap-2.5 text-[15px] ${
                      i === 0 ? "pt-0" : "border-t border-dashed border-line"
                    }`}
                  >
                    <span className="text-accent">→</span> <span>{row}</span>
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </div>
      </section>

      {/* PROPERTIES */}
      <section className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Properties"
            title={
              <>
                Latency-first.
                <br />
                Streaming-first.
                <br />
                Zero-copy.
              </>
            }
            right={
              <>
                Net composes pieces that exist as solved problems — event sourcing, process
                migration, causal ordering, capability scheduling, self-healing mesh — into
                one runtime at nanosecond speeds. Nobody composed them before because
                nobody had a transport fast enough.
              </>
            }
          />

          <div className="mt-16 grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-4 gap-px bg-line border border-line rounded-[14px] overflow-hidden">
            {PROPS.map((p) => (
              <div key={p.title} className="bg-panel p-7">
                <div className="text-base font-semibold mb-2 tracking-[-0.01em]">
                  {p.title}
                </div>
                <div className="text-muted text-sm leading-[1.55]">{p.body}</div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* STACK */}
      <section id="stack" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Stack"
            title={
              <>
                One process. One header.
                <br />
                Eleven concerns.
              </>
            }
            right={
              <>
                A 64-byte cache-line-aligned wire format. Forwarding nodes read one cache
                line, decide a route, and forward without touching the payload. Every layer
                above transport adds a distinct concern at sub-microsecond cost.
              </>
            }
          />

          <div className="mt-14 border border-line rounded-[14px] arch-fade overflow-hidden">
            <div className="hidden xl:grid grid-cols-[220px_1fr_280px] bg-raised font-mono text-[11px] tracking-[0.12em] uppercase text-dim py-3.5">
              <div className="px-6">Layer</div>
              <div className="px-6">What it does</div>
              <div className="px-6">Key mechanism</div>
            </div>
            {ARCH_ROWS.map((r, i) => (
              <div
                key={r.name}
                className={`grid grid-cols-1 xl:grid-cols-[220px_1fr_280px] xl:items-center ${
                  i === 0 ? "border-t-0 xl:border-t border-line" : "border-t border-line"
                } hover:bg-[rgba(124,245,192,0.03)] transition-colors`}
              >
                <div className="px-6 pt-4 xl:py-4 font-mono text-[13px] text-accent font-medium tracking-[-0.01em]">
                  {r.name}
                </div>
                <div className="px-6 py-1.5 xl:py-4 text-muted text-sm">{r.desc}</div>
                <div className="px-6 pb-4 xl:py-4 font-mono text-xs text-dim">{r.mech}</div>
              </div>
            ))}
          </div>

          <div className="mt-14 grid grid-cols-1 lg:grid-cols-2 gap-8 items-start">
            <div className="text-muted text-[15px]">
              <h3 className="text-[18px] tracking-[-0.01em] font-semibold mb-4 text-ink">
                A 64-byte header you can read in one cache line.
              </h3>
              <p>
                Every Net packet begins with a header aligned to a single CPU cache line. A
                forwarding node reads one cache line, decides a route, and forwards without
                touching the payload. Routed (multi-hop) packets prepend an 18-byte routing
                header; direct packets use the Net header alone.
              </p>
              <p className="mt-3.5">
                Headers are never encrypted — only payloads. ChaCha20-Poly1305 with counter
                nonces eliminates nonce-reuse risk. Every field is read by at least one
                layer.
              </p>
            </div>
            <div className="border border-line rounded-xl bg-panel overflow-hidden font-mono text-xs">
              <div className="bg-raised px-4 py-3 text-dim tracking-[0.08em] border-b border-line flex justify-between">
                <span>NET HEADER</span>
                <span>64 BYTES · 1 CACHE LINE</span>
              </div>
              {WIRE_ROWS.map((r, i) => (
                <div
                  key={i}
                  className={`grid grid-cols-[60px_1fr_60px] px-4 py-2.5 text-muted ${
                    i === 0 ? "" : "border-t border-dashed border-line"
                  }`}
                >
                  <span>{r.off}</span>
                  <span className="text-ink">{r.name}</span>
                  <span className="text-accent text-right">{r.size}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>

      {/* BENCHMARKS */}
      <section id="benchmarks" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Benchmarks · M1 Max / i9-14900K @5GHz · 2026-04-27"
            title={
              <>
                The software is no longer
                <br />
                the bottleneck.
              </>
            }
            right={
              <>
                Every number measures packet scheduling: process, route, encrypt, queue.
                Not NIC transfer, not wire latency, not the speed of light. At 5 hops,
                total scheduling overhead is under 300 ns — well inside the budget for
                edge-to-edge coordination on a campus network where the physics floor is
                ~33 μs.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 lg:grid-cols-3 gap-4">
            {BENCH.map((b) => (
              <div
                key={b.label}
                className="bg-panel border border-line rounded-xl p-7 transition-all hover:border-line2 hover:-translate-y-0.5"
              >
                <div className="font-mono text-[36px] tracking-[-0.02em] grad-text font-medium leading-[1.1]">
                  {b.num}
                  <span className="font-mono text-dim text-sm ml-0.5">{b.unit}</span>
                </div>
                <div className="text-ink mt-3.5 font-medium">{b.label}</div>
                <div className="text-dim font-mono text-xs mt-2">{b.meta}</div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* DAEMONS / MIKOSHI / CHANNELS */}
      <section id="daemons" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Capabilities · Daemons · Channels"
            title={<>The mesh is the runtime.</>}
            right={
              <>
                The deployment topology is a runtime decision, not a code change. Code that
                runs on a single node runs unmodified across a multi-hop encrypted mesh.
                The mesh resolves routing, decryption, and chain validation before your
                daemon sees the event.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 xl:grid-cols-3 gap-4">
            <FeatureCard tag="Capabilities" title="No registry. The nodes are the control plane.">
              <p>
                A node announces what it is — cores, memory, GPU, loaded models, installed
                tools, operator tags — and every peer indexes that announcement locally.
                Capability diffs propagate multi-hop. A node four subnets away learns the
                same fingerprint as a direct peer, without anyone in the middle being a
                directory.
              </p>
              <p className="mt-4 pl-3.5 border-l-2 border-accent text-ink italic text-sm">
                You ask <em>any peer with an NVIDIA GPU and 40 GB of VRAM, advertising the
                prod tag</em> — you get an answer in microseconds.
              </p>
            </FeatureCard>

            <FeatureCard tag="Mikoshi · daemon migration" title="The thing itself, carried across.">
              <p>
                A daemon's identity is cryptographic; its location is the mesh. Address it
                by{" "}
                <code className="font-mono text-[13px] text-accent">origin_hash</code>, a
                fingerprint of an ed25519 public key that doesn't change when the daemon
                moves.
              </p>
              <p className="mt-3">
                Six-phase migration: snapshot → transfer → restore → replay → cutover →
                cleanup. Subscribers don't notice it moved. The same machinery composes
                into replica groups, fork groups with verifiable lineage, and
                active-passive standby.
              </p>
            </FeatureCard>

            <FeatureCard tag="Channels" title="A name you match on, not a thing you connect to.">
              <p>
                A publisher registers{" "}
                <code className="font-mono text-[13px] text-accent">
                  sensors/temperature
                </code>{" "}
                with a policy; subscribers ask to join by name; the mesh routes the
                semantic. There is no broker. Publish-without-subscribers is literally a
                no-op — the roster is empty, the fan-out loop runs zero times.
              </p>
              <p className="mt-3">
                A subscriber that loses their token stops receiving events on the
                publisher's next packet — not on the next cluster reconciliation, not when
                the service mesh pushes an ACL.
              </p>
            </FeatureCard>
          </div>
        </div>
      </section>

      {/* LATENCY GAP */}
      <section className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="The industrial latency gap"
            title={
              <>
                33 μs is the floor.
                <br />
                Cloud is 1500× above it.
              </>
            }
            right={
              <>
                Most industrial coordination is hyper-local: factory floor, facility
                campus, vehicle fleet. Light in fiber across 5 km is around 33 μs round
                trip. That's the hard limit. No protocol can beat it. Cloud-routed PLCs,
                centralized SCADA, remote monitoring — they add 10–50 ms on top of physics.
              </>
            }
          />

          <div className="mt-14 border border-line rounded-[14px] gap-fade p-8 lg:p-12 grid grid-cols-1 lg:grid-cols-[1.1fr_1fr] gap-8 lg:gap-12 items-center">
            <div className="flex flex-col gap-4">
              <BarRow label="Cloud round trip" value="~50 ms" valueColor="text-dim">
                <span className="block h-full rounded bar-track" style={{ width: "100%" }} />
              </BarRow>
              <BarRow label="Local-area TCP" value="~5 ms" valueColor="text-dim">
                <span className="block h-full rounded bar-track" style={{ width: "10%" }} />
              </BarRow>
              <BarRow label="Physics floor (5 km fiber RTT)" value="~33 μs" valueColor="text-warn">
                <span
                  className="block h-full rounded bar-physics"
                  style={{ width: "0.066%", minWidth: 4 }}
                />
              </BarRow>
              <BarRow label="Net — 5-hop scheduling overhead" value="274 ns" valueColor="text-accent">
                <span
                  className="block h-full rounded bar-net"
                  style={{ width: "0.0005%", minWidth: 4 }}
                />
              </BarRow>
            </div>
            <div className="text-muted text-[15px]">
              <h3 className="text-2xl tracking-[-0.02em] mb-4 text-ink font-semibold">
                A category change, not a marginal improvement.
              </h3>
              <p>
                When coordination latency drops from 50 ms to 33 μs, things that were
                impossible become trivial. Closed-loop control across a mesh of autonomous
                devices. Real-time consensus between robots on an assembly line. Swarm
                coordination where the mesh reacts faster than any individual node's
                control loop.
              </p>
              <p className="mt-3.5">
                Net doesn't replace the data center. It separates what must be fast from
                what must be smart. The 10 kHz torque feedback loop stays on the floor
                between sensor and actuator. The vibration pattern that predicts bearing
                failure next week travels to a model with 100 GB of training data.
              </p>
            </div>
          </div>
        </div>
      </section>

      {/* LOCAL DATA */}
      <section id="data" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Optional · per-node · local-first"
            title={
              <>
                RedEX. CortEX. NetDB.
                <br />
                The stream is the state.
              </>
            }
            right={
              <>
                The hot path never touches disk. Storage is a choice, not an architectural
                requirement. When a node needs durability, queries, or replay, three
                feature-flagged layers stack on top of Net — all local, all per-node, all
                unbundled from any cluster.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 lg:grid-cols-3 border border-line rounded-[14px] overflow-hidden">
            <StackCard
              title="RedEX"
              name="Append-only event log"
              meta="21.3 M append/s inline · 138 ns tail latency"
              border
            >
              The append-only log unbundled and local. 20-byte index records, optional
              disk persistence per channel, atomic backfill-then-live tailing. A Pi keeps
              a tiny log of its own readings; a server keeps a huge log of whatever it
              cares about. No cluster consensus protocol — the log is local, replay is
              local, retention is local.
            </StackCard>
            <StackCard
              title="CortEX"
              name="RedEX, folded"
              meta="8.98 ns find_unique · 8.87 M ingest/s"
              border
            >
              A reactive, queryable projection of the log, updated event-by-event. The
              "database" isn't a process you connect to — it's a{" "}
              <code className="font-mono text-[13px] text-accent">
                Vec&lt;Task&gt;
              </code>{" "}
              or{" "}
              <code className="font-mono text-[13px] text-accent">
                HashMap&lt;Uuid, Memory&gt;
              </code>{" "}
              in your Rust, TypeScript, or Python code, updating as events fold in. Queries
              are direct memory access.
            </StackCard>
            <StackCard
              title="NetDB"
              name="Unified query façade"
              meta="6.30 μs open · 1 K-row bundle 48 KB"
            >
              One handle bundling tasks + memories under{" "}
              <code className="font-mono text-[13px] text-accent">db.tasks</code> and{" "}
              <code className="font-mono text-[13px] text-accent">db.memories</code>.
              Prisma-style{" "}
              <code className="font-mono text-[13px] text-accent">find_unique</code> /{" "}
              <code className="font-mono text-[13px] text-accent">find_many</code> across
              Rust, TypeScript, and Python — whole-database snapshots round-trip between
              languages.
            </StackCard>
          </div>
        </div>
      </section>

      {/* APPLICATIONS */}
      <section id="applications" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Applications"
            title={<>Coordination at hardware timescales.</>}
            right={
              <>
                Anywhere the bottleneck is processing time, not delivery. Anywhere the
                topology is dynamic, heterogeneous, or partially adversarial. Net sits
                underneath; the applications above don't change.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 lg:grid-cols-3 gap-4">
            {APPS.map((a) => (
              <div
                key={a.title}
                className="bg-panel border border-line rounded-xl p-7 min-h-[200px] relative overflow-hidden transition-all hover:border-accent"
              >
                <div className="w-9 h-9 rounded-lg bg-accent/10 border border-accent/25 grid place-items-center text-accent font-mono text-sm mb-4">
                  {a.icon}
                </div>
                <h3 className="text-[17px] font-semibold tracking-[-0.01em]">{a.title}</h3>
                <p className="text-muted text-sm mt-2">{a.body}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* INSTALL */}
      <section id="install" className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Install"
            title={
              <>
                One core. Five SDKs.
                <br />
                Same engine.
              </>
            }
            right={
              <>
                All SDKs wrap the same Rust core. The SDK is the developer experience; the
                engine is Rust. Lower-level bindings skip the SDK ergonomics and talk
                directly to the engine.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 md:grid-cols-[240px_1fr] border border-line rounded-[14px] overflow-hidden bg-panel">
            <div
              role="tablist"
              className="flex flex-row md:flex-col flex-wrap p-4 bg-raised border-b md:border-b-0 md:border-r border-line"
            >
              {INSTALL_TABS.map((t) => (
                <button
                  key={t.key}
                  role="tab"
                  onClick={() => setTab(t.key)}
                  className={`text-left bg-transparent border-0 px-3.5 py-3 rounded-lg font-mono text-[13px] cursor-pointer transition-all ${
                    tab === t.key
                      ? "bg-accent/10 text-accent"
                      : "text-muted hover:text-ink"
                  }`}
                >
                  {t.label}
                </button>
              ))}
            </div>
            <div className="px-8 py-7 font-mono text-sm overflow-x-auto">
              <pre className="m-0 whitespace-pre text-ink">{INSTALL_PANES[tab]}</pre>
            </div>
          </div>

          <div className="mt-6 border border-line rounded-xl bg-panel overflow-hidden">
            <div className="px-4 py-2.5 bg-raised font-mono text-[11px] text-dim tracking-[0.1em] border-b border-line flex justify-between">
              <span>main.rs · minimum-viable mesh node</span>
              <span>RUST</span>
            </div>
            <pre className="m-0 px-5 py-5 font-mono text-[13px] text-ink overflow-x-auto">
              <code>
                <span className="text-accent">use</span>{" "}
                {`net_sdk::{MeshNode, MeshNodeConfig, ChannelName};`}
                {"\n\n"}
                <span className="text-dim">
                  {`// One key, one policy surface, one revocation primitive.`}
                </span>
                {"\n"}
                <span className="text-dim">
                  {`// The same ed25519 seed signs the node id, capability ads, and tokens.`}
                </span>
                {"\n"}
                {`#[tokio::main]`}
                {"\n"}
                <span className="text-accent">async fn</span>{" "}
                <span className="text-[#c4a4f5]">main</span>
                {`() -> `}
                <span className="text-accent">anyhow</span>
                {`::Result<()> {`}
                {"\n    "}
                <span className="text-accent">let</span>
                {` node = MeshNode::`}
                <span className="text-[#c4a4f5]">spawn</span>
                {`(`}
                {"\n        "}
                {`MeshNodeConfig::`}
                <span className="text-[#c4a4f5]">builder</span>
                {`()`}
                {"\n            "}
                {`.`}
                <span className="text-[#c4a4f5]">bind</span>
                {`(`}
                <span className="text-accent2">{`"0.0.0.0:0"`}</span>
                {`)`}
                {"\n            "}
                {`.`}
                <span className="text-[#c4a4f5]">capabilities</span>
                {`([`}
                <span className="text-accent2">{`"gpu:rtx-4090"`}</span>
                {`, `}
                <span className="text-accent2">{`"vram:24gb"`}</span>
                {`, `}
                <span className="text-accent2">{`"region:eu-west"`}</span>
                {`])`}
                {"\n            "}
                {`.`}
                <span className="text-[#c4a4f5]">with_try_port_mapping</span>
                {`(`}
                <span className="text-accent">true</span>
                {`)`}
                {"\n            "}
                {`.`}
                <span className="text-[#c4a4f5]">build</span>
                {`(),`}
                {"\n    "}
                {`).`}
                <span className="text-accent">await</span>
                {`?;`}
                {"\n\n    "}
                <span className="text-accent">let</span>
                {` sensors = ChannelName::`}
                <span className="text-[#c4a4f5]">parse</span>
                {`(`}
                <span className="text-accent2">{`"sensors/temperature"`}</span>
                {`)?;`}
                {"\n    "}
                <span className="text-accent">let mut</span>
                {` stream = node.`}
                <span className="text-[#c4a4f5]">subscribe</span>
                {`(&sensors).`}
                <span className="text-accent">await</span>
                {`?;`}
                {"\n\n    "}
                <span className="text-accent">while let</span>{" "}
                <span className="text-accent">Some</span>
                {`(event) = stream.`}
                <span className="text-[#c4a4f5]">next</span>
                {`().`}
                <span className="text-accent">await</span>
                {` {`}
                {"\n        "}
                <span className="text-dim">{`// Same call signature as a local function.`}</span>
                {"\n        "}
                <span className="text-dim">
                  {`// Mesh resolved routing, decryption, and chain validation.`}
                </span>
                {"\n        "}
                <span className="text-[#c4a4f5]">handle</span>
                {`(event).`}
                <span className="text-accent">await</span>
                {`;`}
                {"\n    "}
                {`}`}
                {"\n    "}
                <span className="text-accent">Ok</span>
                {`(())`}
                {"\n"}
                {`}`}
              </code>
            </pre>
          </div>
        </div>
      </section>

      {/* BLACKWALL */}
      <section className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="The Blackwall"
            title={<>The wall is the mesh itself.</>}
            right={
              <>
                Encryption isn't a layer on top of Net — it's a consequence of how
                forwarding works. Containment isn't one mechanism — it's the emergent
                effect of every constraint working together.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-1 xl:grid-cols-3 gap-4">
            {BLACKWALL.map((b) => (
              <FeatureCard key={b.tag} tag={b.tag} title={b.title}>
                <p>{b.body}</p>
              </FeatureCard>
            ))}
          </div>
        </div>
      </section>

      {/* STATUS */}
      <section className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <SectionHead
            eyebrow="Status"
            title={
              <>
                A working protocol,
                <br />
                not a paper design.
              </>
            }
            right={
              <>
                Encrypted point-to-point transport, multi-peer mesh runtime, relay
                forwarding without decryption, pingwave-driven distance-vector routing,
                stream multiplexing with byte-credit backpressure, full 6-phase migration,
                partition simulation and healing, RedEX disk durability, and CortEX +
                NetDB across Rust, Node, and Python — all shipping today.
              </>
            }
          />

          <div className="mt-14 grid grid-cols-2 lg:grid-cols-4 border border-line rounded-[14px] overflow-hidden bg-panel">
            {STATUS_CARDS.map((s, i) => (
              <div
                key={s.l}
                className={`px-7 py-6 ${
                  i % 2 === 0 ? "border-r border-line" : "lg:border-r lg:border-line"
                } ${i < 2 ? "border-b border-line lg:border-b-0" : ""} ${
                  i === STATUS_CARDS.length - 1 ? "lg:border-r-0" : ""
                }`}
              >
                <div className="font-mono text-[22px] text-ink font-medium">{s.v}</div>
                <div className="text-xs text-dim mt-1 font-mono tracking-[0.06em]">
                  {s.l}
                </div>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="py-[120px] border-t border-line">
        <div className="max-w-[1200px] mx-auto px-7">
          <div className="border border-line rounded-[18px] cta-fade p-10 lg:p-16 text-center relative overflow-hidden">
            <div className="absolute inset-0 cta-grid-bg pointer-events-none" />
            <div className="relative">
              <span className="font-mono text-xs tracking-[0.14em] uppercase text-accent inline-flex items-center gap-2 before:content-[''] before:w-1.5 before:h-1.5 before:rounded-full before:bg-accent before:shadow-[0_0_12px_var(--color-accent)] animate-pulse-soft">
                v0 · working protocol
              </span>
              <h2 className="text-[clamp(30px,3.6vw,46px)] leading-[1.08] tracking-[-0.02em] font-semibold mx-auto mt-[18px] max-w-[22ch]">
                Build on a network
                <br />
                that doesn't flinch.
              </h2>
              <p className="text-[clamp(17px,1.5vw,20px)] leading-[1.55] text-muted mx-auto mt-[22px] max-w-[56ch]">
                The transport, encryption, and routing are fixed. Everything above them —
                subprotocols, channels, daemons, models — is a feature space, not a
                feature list. Deploy incrementally. The mesh already supports your
                protocol. It just doesn't know what it means yet.
              </p>
              <div className="mt-10 inline-flex gap-3 flex-wrap justify-center">
                <a
                  href="#install"
                  className="group inline-flex items-center gap-2 px-5 py-3 rounded-[10px] font-mono text-[13px] font-medium border bg-ink text-deep border-ink transition-all hover:bg-accent hover:border-accent hover:-translate-y-px"
                >
                  Install Net{" "}
                  <span className="transition-transform group-hover:translate-x-[3px]">
                    →
                  </span>
                </a>
                <a
                  href="PAPER.md"
                  className="inline-flex items-center gap-2 px-5 py-3 rounded-[10px] font-mono text-[13px] font-medium border border-line2 transition-all hover:border-ink"
                >
                  Read the paper
                </a>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* FOOTER */}
      <footer className="border-t border-line py-12 text-dim text-[13px]">
        <div className="max-w-[1200px] mx-auto px-7 flex justify-between items-start gap-6 flex-wrap">
          <div>
            <div className="flex items-center gap-2.5 mb-3.5">
              <span className="inline-flex items-center gap-2.5 font-semibold tracking-[-0.01em]">
                <span className="logo-mark" />
                <span>Net</span>
              </span>
            </div>
            <div className="max-w-[60ch] leading-[1.6]">
              Loosely inspired by the Net from Cyberpunk 2077. Not affiliated with CD
              Projekt Red or R. Talsorian Games. This is an engineering take on the
              concept, not a licensed adaptation. © 2026.
            </div>
          </div>
          <div className="flex gap-6 flex-wrap">
            <a href="#stack" className="hover:text-ink transition-colors">
              Stack
            </a>
            <a href="#benchmarks" className="hover:text-ink transition-colors">
              Benchmarks
            </a>
            <a href="#daemons" className="hover:text-ink transition-colors">
              Daemons
            </a>
            <a href="#data" className="hover:text-ink transition-colors">
              Local data
            </a>
            <a href="#install" className="hover:text-ink transition-colors">
              Install
            </a>
            <a href="PAPER.md" className="hover:text-ink transition-colors">
              Paper
            </a>
          </div>
        </div>
      </footer>
    </div>
  );
}

function FeatureCard({
  tag,
  title,
  children,
}: {
  tag: string;
  title: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="bg-panel border border-line rounded-[14px] p-8 transition-colors hover:border-line2 text-muted text-[14.5px] leading-[1.55]">
      <span className="font-mono text-[11px] tracking-[0.14em] uppercase text-dim">
        {tag}
      </span>
      <h3 className="text-[22px] tracking-[-0.02em] font-semibold mt-3 mb-3 text-ink">
        {title}
      </h3>
      {children}
    </div>
  );
}

function StackCard({
  title,
  name,
  meta,
  border,
  children,
}: {
  title: string;
  name: string;
  meta: string;
  border?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      className={`bg-panel p-9 relative ${
        border ? "border-b lg:border-b-0 lg:border-r border-line" : ""
      }`}
    >
      <h3 className="font-mono text-sm text-accent tracking-[0.04em] mb-3.5 font-semibold">
        {title}
      </h3>
      <div className="text-2xl tracking-[-0.02em] font-semibold mb-3 text-ink">
        {name}
      </div>
      <p className="text-muted text-sm">{children}</p>
      <div className="mt-4 font-mono text-[11px] text-dim tracking-[0.04em] pt-3.5 border-t border-dashed border-line">
        {meta}
      </div>
    </div>
  );
}

function BarRow({
  label,
  value,
  valueColor,
  children,
}: {
  label: string;
  value: string;
  valueColor: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="flex justify-between font-mono text-xs text-muted mb-2 tracking-[0.04em]">
        <span>{label}</span>
        <span className={valueColor}>{value}</span>
      </div>
      <div className="h-3.5 rounded bg-raised overflow-hidden relative">{children}</div>
    </div>
  );
}
