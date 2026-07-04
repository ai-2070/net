import Link from "next/link";
import { PacketRain } from "@/components/PacketRain";

interface Participant {
  node: string;
  kind: string;
  caps: string;
  status: string;
  statusColor: string;
}

const PARTICIPANTS: ReadonlyArray<Participant> = [
  {
    node: "node.0x4e·agent",
    kind: "operator",
    caps: "discover · claim · stream",
    status: "ACTIVE",
    statusColor: "text-accent",
  },
  {
    node: "node.0xa1·gpu",
    kind: "gpu.worker",
    caps: "rtx-4090 · 24gb · $/tok",
    status: "CLAIMABLE",
    statusColor: "text-cyan",
  },
  {
    node: "node.0x7f·browser",
    kind: "browser.bridge",
    caps: "dom · nav · capture",
    status: "BOUNDED",
    statusColor: "text-ink-dim",
  },
  {
    node: "node.0x33·files",
    kind: "cas / artifacts",
    caps: "blake3 · move · pin",
    status: "DURABLE",
    statusColor: "text-ink-dim",
  },
  {
    node: "node.0x91·sensor",
    kind: "observation",
    caps: "stream · subnet-local",
    status: "LIVE",
    statusColor: "text-accent",
  },
  {
    node: "node.0x0c·task",
    kind: "durable.task",
    caps: "launch · resume · replay",
    status: "RUNNING",
    statusColor: "text-accent",
  },
];

function ParticipantPanel() {
  return (
    <div className="border border-line bg-bg-2 p-4">
      <div className="flex justify-between items-center border-b border-line pb-2 mb-3.5 text-[10px] tracking-[0.12em] text-ink-dim uppercase">
        <span>
          <span className="text-accent">▸</span> mesh.participants
        </span>
        <span className="flex items-center gap-1.5">
          <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block animate-pulse-dot" />
          DISCOVERED
        </span>
      </div>

      <div className="font-mono text-[10px] leading-[1.55] flex flex-col gap-2.5">
        {PARTICIPANTS.map((p) => (
          <div key={p.node} className="flex flex-col gap-0.5">
            <div className="flex items-baseline justify-between gap-2">
              <span className="text-ink">{p.node}</span>
              <span className={`${p.statusColor} shrink-0`}>{p.status}</span>
            </div>
            <div className="flex items-baseline justify-between gap-2 text-ink-faint">
              <span className="text-ink-dim">{p.kind}</span>
              <span className="text-ink-dim truncate">{p.caps}</span>
            </div>
          </div>
        ))}
      </div>

      <div className="border-t border-line mt-3.5 pt-2.5 flex justify-between text-[9px] text-ink-faint tracking-[0.1em] uppercase font-mono">
        <span>typed mesh · 6 nodes</span>
        <span>
          <span className="text-accent">▸</span> one coordination model
        </span>
      </div>
    </div>
  );
}

export function VcHero() {
  return (
    <section
      id="hero"
      className="hero relative overflow-hidden border-b border-line px-6 pt-[60px] pb-20"
    >
      <PacketRain />
      <div className="relative grid grid-cols-1 lg:grid-cols-[1fr_440px] gap-12 items-start">
        <div>
          <div className="text-[10px] text-ink-dim tracking-[0.15em] mb-7 flex flex-wrap gap-[18px] items-center">
            <span className="text-accent border border-accent-dim px-2 py-[3px]">
              SUBSTRATE / NOT A HARNESS
            </span>
            <span className="text-ink-faint font-mono">PROTOCOL.0x4E45·54</span>
          </div>

          <h1
            className="font-display leading-[0.9] tracking-[-0.02em] text-ink mb-7"
            style={{ fontSize: "clamp(46px, 7.6vw, 100px)" }}
          >
            agents need
            <br />
            a network
            <br />
            they can <span className="text-accent">operate.</span>
          </h1>

          <p className="text-[19px] md:text-[21px] text-ink mt-7 max-w-[640px] leading-[1.4]">
            The first AI platform wave was{" "}
            <span className="text-ink-dim">model access.</span> The next is{" "}
            <span className="text-accent">operation</span>: machines, resources,
            streams, work, and authority.
          </p>

          <p className="text-[18px] text-ink mt-6 max-w-[620px] leading-[1.5] font-light">
            Agents are leaving chat. They are starting to{" "}
            <em className="not-italic text-accent bg-accent/10 px-1">
              operate
            </em>{" "}
            — running tools, watching screens, moving files, coordinating
            devices, launching jobs, using GPUs, reacting to live streams.
          </p>

          <p className="text-[13px] text-ink-dim mt-[18px] max-w-[620px] leading-[1.65]">
            The current stack was not built for that world. Tool protocols
            handle calls. Service meshes handle cloud services. Job queues
            handle workers. None of them model autonomous participants with
            local authority, live state, durable tasks, artifacts, and
            unreliable links as one system.
          </p>

          <p className="text-[14px] text-ink mt-6 max-w-[620px] leading-[1.6] border-l-2 border-accent-dim pl-4">
            <strong className="text-ink font-medium">
              Net is the coordination layer for agents, devices, services, and
              artifacts that need to keep moving when the link goes dark.
            </strong>
          </p>

          <div className="mt-11 flex gap-3 flex-wrap items-center">
            <Link
              href="/docs/concepts/architecture"
              className="btn-primary inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-accent bg-accent text-bg transition-all"
            >
              Read the architecture <span className="text-sm">→</span>
            </Link>
            <a
              href="#capabilities"
              className="btn-ghost inline-flex items-center gap-2.5 px-5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline border border-ink-faint text-ink transition-all"
            >
              View the primitives
            </a>
            <a
              href="#venture"
              className="btn-ghost inline-flex items-center gap-2.5 py-3 text-[11px] tracking-[0.12em] uppercase font-semibold no-underline text-ink transition-all"
            >
              // why venture-scale ↘
            </a>
          </div>

          <p className="text-[11px] text-ink-dim mt-9 max-w-[620px] leading-[1.7] font-mono">
            Autonomy at the edge.{" "}
            <span className="text-ink">Coordination through state.</span>{" "}
            Authority at the resource boundary.
          </p>
        </div>

        <div className="hidden lg:block self-start">
          <ParticipantPanel />
        </div>
      </div>
    </section>
  );
}
