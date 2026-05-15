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
    <div className="my-12">
      <div className="flex justify-between items-baseline mb-6 border-b border-line pb-3">
        <h3 className="font-head text-[22px] leading-tight text-ink tracking-[0.04em] lowercase">
          Mikoshi migration · 6 phases
        </h3>
        <span className="text-[10px] text-ink-dim tracking-[0.12em] uppercase">
          zero-downtime cutover · <b className="text-accent">~280ns total</b>
        </span>
      </div>

      <div className="phase-track-md grid grid-cols-1 md:grid-cols-3 lg:grid-cols-6 gap-0 border border-line bg-bg-2">
        {MIGRATION_PHASES.map((p, i) => (
          <div
            key={p.num}
            className={`phase-arrow relative px-4 py-5 ${i < 5 ? "border-r border-line" : ""} transition-colors hover:bg-accent/[0.04]`}
          >
            <div className="font-display text-[28px] text-accent leading-none mb-2">
              {p.num}
            </div>
            <div className="text-[11px] text-ink uppercase tracking-[0.1em] mb-2.5 font-semibold">
              {p.name}
            </div>
            <div className="text-[10px] text-ink-dim leading-[1.55]">
              {p.body}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
