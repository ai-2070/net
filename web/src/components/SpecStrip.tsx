const SPEC_STRIP: ReadonlyArray<{
  label: string;
  value: string;
  unit: string;
  body: React.ReactNode;
}> = [
  {
    label: "// trait surface",
    value: "5",
    unit: "methods",
    body: "name · requirements · process · snapshot · restore",
  },
  {
    label: "// migration phases",
    value: "6",
    unit: "strict",
    body: "snapshot → transfer → restore → replay → cutover → complete",
  },
  {
    label: "// wire messages",
    value: "10",
    unit: "types",
    body: (
      <>
        orchestrator + source + target over{" "}
        <code className="text-accent">0x0500</code>
      </>
    ),
  },
  {
    label: "// cycle time",
    value: "~280",
    unit: "ns",
    body: "full snapshot → activate, faster than a context switch",
  },
];

function SpecStrip() {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border border-line mt-10">
      {SPEC_STRIP.map((s, i) => (
        <div
          key={s.label}
          className={`px-6 py-5 bg-bg-2 ${i < SPEC_STRIP.length - 1 ? "border-b lg:border-b-0 lg:border-r border-line" : ""}`}
        >
          <div className="text-[10px] text-ink-dim tracking-[0.12em] uppercase mb-2">
            {s.label}
          </div>
          <div className="font-display text-[22px] text-accent leading-[1.1] mb-1">
            {s.value}
            <span className="text-[13px] text-ink-dim ml-1">{s.unit}</span>
          </div>
          <div className="text-[10px] text-ink-dim leading-[1.4]">{s.body}</div>
        </div>
      ))}
    </div>
  );
}
