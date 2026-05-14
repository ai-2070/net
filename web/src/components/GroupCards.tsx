import { AsciiCycle } from "./AsciiCycle";

interface GroupCard {
  id: string;
  name: string;
  meta: string;
  ascii: React.ReactNode;
  body: React.ReactNode;
  spec: ReadonlyArray<readonly [string, string]>;
}

interface AsciiPhase {
  rows: ReadonlyArray<React.ReactNode>;
  caption: React.ReactNode;
}

const STANDBY_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>
        active{"   "}
        <span className="text-accent">●</span> processing seq=102
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=98
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=101
      </>,
    ],
    caption: <>all healthy · 3 nodes online</>,
  },
  {
    rows: [
      <>
        active{"   "}
        <span className="text-warn">×</span> failed @ seq=102
      </>,
      <>
        standby{"  "}
        <span className="text-accent">↑</span> promoting (sync=101)
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=101
      </>,
    ],
    caption: <>replaying buffered events ▸ seq=103</>,
  },
  {
    rows: [
      <>
        active{"   "}
        <span className="text-accent">●</span> processing seq=104{"  "}
        <span className="text-ink-faint">(was member 1)</span>
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=104
      </>,
      <>
        standby{"  "}
        <span className="text-ink-faint">○</span> synced_through=104
      </>,
    ],
    caption: <>continuity preserved · 0 events lost</>,
  },
];

const REPLICA_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-accent">●</span> event #58 → result
      </>,
      <>
        member 1{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 2{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
    ],
    caption: <>round-robin · seq=58</>,
  },
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 1{"  "}
        <span className="text-accent">●</span> event #59 → result
      </>,
      <>
        member 2{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
    ],
    caption: <>round-robin · seq=59</>,
  },
  {
    rows: [
      <>
        member 0{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 1{"  "}
        <span className="text-ink-faint">○</span> idle
      </>,
      <>
        member 2{"  "}
        <span className="text-accent">●</span> event #60 → result
      </>,
    ],
    caption: <>round-robin · seq=60</>,
  },
];

const FORK_PHASES: ReadonlyArray<AsciiPhase> = [
  {
    rows: [
      <>parent @ seq=42</>,
      <>
        {"   "}
        <span className="text-ink-faint">·</span> single chain, no divergence
      </>,
      <>
        {"   "}
        <span className="text-ink-faint">·</span> awaiting fork directive
      </>,
    ],
    caption: <>pre-fork · monitoring</>,
  },
  {
    rows: [
      <>
        {"   ├─▶ "}fork.A <span className="text-accent">(sentinel)</span>
      </>,
      <>
        {"   ├─▶ "}fork.B <span className="text-accent">(sentinel)</span>
      </>,
      <>
        {"   └─▶ "}fork.C <span className="text-accent">(sentinel)</span>
      </>,
    ],
    caption: <>forking · sentinels written from seq=42</>,
  },
  {
    rows: [
      <>
        fork.A @ seq=58 <span className="text-ink-faint">─ diverged</span>
      </>,
      <>
        fork.B @ seq=53 <span className="text-ink-faint">─ diverged</span>
      </>,
      <>
        fork.C @ seq=61 <span className="text-ink-faint">─ diverged</span>
      </>,
    ],
    caption: <>verifiable lineage → parent @ 42</>,
  },
];

const GROUP_CARDS: readonly GroupCard[] = [
  {
    id: "▸ GRP.01",
    name: "replica",
    meta: "N interchangeable copies · load-balanced",
    ascii: <AsciiCycle phases={REPLICA_PHASES} intervalMs={2200} />,
    body: (
      <>
        For horizontal scale on stateless workloads.{" "}
        <b className="text-ink font-medium">
          Each replica has its own causal chain
        </b>{" "}
        derived from a deterministic seed — fail one, spawn another with the
        same identity. No state to transfer.
      </>
    ),
    spec: [
      ["identity", "deterministic from seed"],
      ["routing", "round-robin"],
      ["state", "stateless"],
      ["recovery", "respawn"],
    ],
  },
  {
    id: "▸ GRP.02",
    name: "fork",
    meta: "independent siblings · documented lineage",
    ascii: <AsciiCycle phases={FORK_PHASES} intervalMs={3500} />,
    body: (
      <>
        For experiments, A/B testing, scenario branching.{" "}
        <b className="text-ink font-medium">
          Each fork carries a cryptographic sentinel
        </b>{" "}
        linking back to the parent at the fork point. Forks share a past but not
        a future.
      </>
    ),
    spec: [
      ["identity", "divergent from sentinel"],
      ["routing", "per-fork"],
      ["state", "independent"],
      ["recovery", "resnapshot from origin"],
    ],
  },
  {
    id: "▸ GRP.03",
    name: "standby",
    meta: "1 active · N-1 warm · zero duplicate compute",
    ascii: <AsciiCycle phases={STANDBY_PHASES} intervalMs={3500} />,
    body: (
      <>
        For stateful daemons that need fault tolerance without paying for
        duplicate compute.{" "}
        <b className="text-ink font-medium">Only the active processes events</b>{" "}
        — standbys are warm, not hot. Periodic snapshots track{" "}
        <code className="font-mono">synced_through</code> for each standby. On
        active failure, the standby with the highest sync point promotes and
        replays the gap using{" "}
        <b className="text-ink font-medium">
          the same replay machinery migration uses
        </b>
        .
      </>
    ),
    spec: [
      ["identity", "deterministic from seed"],
      ["routing", "active only"],
      ["state", "stateful, synced"],
      ["recovery", "promote + replay gap"],
    ],
  },
];

export function GroupCards() {
  return (
    <div className="grid grid-cols-1 md:grid-cols-3 gap-px bg-accent-dim mt-8 border border-accent-dim">
      {GROUP_CARDS.map((g) => (
        <div
          key={g.id}
          className="bg-bg p-7 transition-colors hover:bg-bg-2 relative"
        >
          <div className="text-[10px] text-accent tracking-[0.18em] mb-1.5">
            {g.id}
          </div>
          <h3 className="font-head text-[22px] leading-tight text-ink mb-1.5 tracking-[0.04em] lowercase">
            {g.name}
          </h3>
          <div className="text-[10px] text-ink-dim mb-4 tracking-[0.05em]">
            {g.meta}
          </div>
          <pre className="font-mono text-[10px] text-ink-dim leading-[1.5] bg-bg-2 p-3.5 border-l-2 border-accent mb-4 whitespace-pre overflow-x-auto">
            {g.ascii}
          </pre>
          <p className="text-[12px] text-ink-dim leading-[1.6] mb-4">
            {g.body}
          </p>
          <div className="border-t border-line pt-3.5 grid grid-cols-2 gap-2.5 text-[10px]">
            {g.spec.map(([k, v]) => (
              <span key={k} className="contents">
                <span className="text-ink-dim uppercase tracking-[0.1em]">
                  {k}
                </span>
                <span className="text-ink">{v}</span>
              </span>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
