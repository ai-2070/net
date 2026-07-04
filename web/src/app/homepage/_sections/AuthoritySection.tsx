import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

interface Boundary {
  owner: string;
  enforces: string;
}

const BOUNDARIES: ReadonlyArray<Boundary> = [
  { owner: "file service", enforces: "file policy" },
  { owner: "gpu worker", enforces: "compute policy" },
  { owner: "browser bridge", enforces: "browser policy" },
  { owner: "desktop agent", enforces: "desktop policy" },
];

export function AuthoritySection() {
  return (
    <section id="authority" className="border-b border-line px-6 py-20">
      <SectionLabel>§07 / authority model</SectionLabel>
      <DisplayHeading>
        policy lives where the
        <br />
        <span className="text-accent">consequences happen.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div>
          <p className="text-[15px] text-ink leading-[1.6] mb-5">
            Net separates transport from authority.
          </p>
          <p className="text-[13px] text-ink-dim leading-[1.7] mb-4">
            The network routes, authenticates, encrypts, streams, and replicates
            state. The resource owner decides what is allowed.
          </p>
          <p className="text-[13px] text-ink-dim leading-[1.7]">
            The protocol carries identity, capability, invocation, and state.
            The tool decides whether the request crosses the boundary.{" "}
            <span className="text-accent">That is how authority composes</span>{" "}
            — bounded capability and local policy, not a central allow-list.
          </p>
        </div>

        <div className="border border-line">
          <div className="border-b border-line px-5 py-3 text-[10px] tracking-[0.14em] uppercase text-ink-dim bg-bg-2">
            <span className="text-accent">▸</span> resource-bound policy
          </div>
          {BOUNDARIES.map((b, i) => (
            <div
              key={b.owner}
              className={`flex items-center justify-between px-5 py-4 ${
                i > 0 ? "border-t border-line" : ""
              }`}
            >
              <span className="text-[13px] text-ink lowercase">{b.owner}</span>
              <span className="text-ink-faint">▸</span>
              <span className="text-[13px] text-accent lowercase">
                {b.enforces}
              </span>
            </div>
          ))}
          <div className="border-t border-dashed border-line px-5 py-4 text-[11px] text-ink-dim leading-[1.6]">
            The mesh carries the request. The owner of the consequence accepts
            or rejects it.
          </div>
        </div>
      </div>
    </section>
  );
}
