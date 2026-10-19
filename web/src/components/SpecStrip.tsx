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
    unit: "phases",
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
    body: "full snapshot → activate, faster than a kernel context switch",
  },
];

export function SpecStrip() {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-x-10 gap-y-10 mt-20">
      {SPEC_STRIP.map((s) => (
        <div key={s.label} className="border-t border-line pt-5">
          <div className="font-display font-bold text-[48px] tracking-[-0.035em] text-ink leading-none mb-3 tabular-nums">
            {s.value}
            <span className="font-mono font-normal text-[13px] tracking-[0.04em] text-ink-dim ml-1.5">
              {s.unit}
            </span>
          </div>
          <div className="text-[15px] text-ink-dim leading-[1.5] mb-3">
            {s.body}
          </div>
          <div className="stencil normal-case">{s.label}</div>
        </div>
      ))}
    </div>
  );
}
