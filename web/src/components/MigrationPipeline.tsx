const MIGRATION_PHASES: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
}> = [
  {
    num: "01",
    name: "snapshot",
    body: "source serializes daemon into a portable bundle.",
  },
  {
    num: "02",
    name: "transfer",
    body: "bundle moves source → target.",
  },
  {
    num: "03",
    name: "restore",
    body: "target copies program data from source; maps daemon environment and identity.",
  },
  {
    num: "04",
    name: "replay",
    body: "target plays back events in order. catches up to where the source left off.",
  },
  {
    num: "05",
    name: "cutover",
    body: "source stops accepting work. routing flips atomically. next event goes to target.",
  },
  {
    num: "06",
    name: "complete",
    body: "source daemon collapses. target becomes sole entity.",
  },
];

export function MigrationPipeline() {
  return (
    <div className="mt-24 mb-12">
      <div className="mb-6 border-b border-line pb-3">
        <h3 className="font-display font-bold text-[22px] md:text-[26px] leading-[1.05] text-ink tracking-[-0.015em]">
          Mikoshi migration · 6 phases
        </h3>
      </div>

      <div className="relative">
        {/* the rail every phase sits on, with the daemon travelling it */}
        <div
          aria-hidden
          className="hidden lg:block absolute left-0 right-0 top-0 h-px bg-line"
        >
          <span className="migration-marker absolute -top-[3px] h-[7px] w-[7px] bg-accent" />
        </div>
        <ol className="grid grid-cols-1 md:grid-cols-3 lg:grid-cols-6 gap-3 lg:gap-4 lg:pt-6 list-none">
          {MIGRATION_PHASES.map((p, i) => (
            <li
              key={p.num}
              className="migration-phase slab px-5 py-6 flex flex-col"
              style={{ "--i": i } as React.CSSProperties}
            >
              <div className="migration-num font-display font-bold text-[28px] tracking-[-0.02em] text-ink-dim leading-none mb-4 tabular-nums">
                {p.num}
              </div>
              <div className="text-[13px] text-ink uppercase tracking-[0.1em] mb-2.5 font-semibold">
                {p.name}
              </div>
              <div className="text-[15px] font-sans text-ink-dim leading-[1.5]">
                {p.body}
              </div>
            </li>
          ))}
        </ol>
      </div>
      <p className="mt-6 font-mono text-[12px] text-ink-dim tracking-[0.12em] uppercase">
        zero-downtime cutover · <b className="text-accent">~280ns total</b>
      </p>
    </div>
  );
}
