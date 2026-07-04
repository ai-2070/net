import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const NEEDS: ReadonlyArray<string> = [
  "use a browser",
  "read and move files",
  "run code on another machine",
  "use a local or remote GPU",
  "watch a live stream of information",
  "start work that takes minutes or hours",
  "recover when a laptop sleeps or a connection drops",
  "know what they are allowed to do",
];

const BRIDGES: ReadonlyArray<string> = [
  "browser bridge",
  "file bridge",
  "GPU bridge",
  "device bridge",
  "stream bridge",
  "job bridge",
];

export function ProblemSection() {
  return (
    <section id="problem" className="border-b border-line px-6 py-20">
      <SectionLabel>§01 / the problem</SectionLabel>

      <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_0.9fr] gap-10 lg:gap-14 mt-3 lg:items-stretch">
        <div className="flex flex-col">
          <DisplayHeading>
            agents are ready to work.
            <br />
            the world around them <span className="text-accent">is not.</span>
          </DisplayHeading>
          <p className="text-[16px] text-ink max-w-[620px] leading-[1.6] font-light mb-6">
            AI is moving from answering questions to taking action. That means
            agents need to do more than call a tool once and return a message.
          </p>
          <p className="text-[13px] text-ink-dim tracking-[0.12em] uppercase mb-3">
            to do real work, an agent needs to
          </p>
          <ul className="daemon-list grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-2.5">
            {NEEDS.map((n) => (
              <li
                key={n}
                className="relative pl-6 text-[13px] text-ink leading-[1.5]"
              >
                {n}
              </li>
            ))}
          </ul>

          <div className="mt-10 lg:mt-auto lg:pt-10 max-w-[560px]">
            <p className="text-[14px] text-ink leading-[1.65] border-l-2 border-accent-dim pl-5">
              Custom bridges work for demos.{" "}
              <span className="text-accent">
                They do not scale into a real operating environment for
                autonomous work.
              </span>
            </p>
          </div>
        </div>

        <div className="flex flex-col">
          <div className="border border-line bg-bg-2 p-6">
            <div className="text-[10px] text-warn tracking-[0.14em] uppercase mb-4">
              today — before net
            </div>
            <div className="flex flex-col items-center gap-3">
              <span className="border border-line px-4 py-2 text-[12px] text-ink">
                agent
              </span>
              <span className="text-ink-faint text-[18px] leading-none">⌄</span>
              <div className="flex flex-wrap justify-center gap-2">
                {BRIDGES.map((b) => (
                  <span
                    key={b}
                    className="border border-dashed border-warn/40 text-ink-dim px-2.5 py-1.5 text-[11px] lowercase"
                  >
                    {b}
                  </span>
                ))}
              </div>
              <p className="text-[11px] text-ink-dim leading-[1.5] text-center mt-1">
                each product wires its own private bridge between agents, tools,
                files, devices, and services.
              </p>
            </div>
          </div>

          <div className="flex items-center justify-center gap-2.5 py-4 text-[10px] text-ink-dim tracking-[0.16em] uppercase">
            <span className="h-px w-8 bg-line" />
            <span className="text-accent">↓</span> the fix
            <span className="h-px w-8 bg-line" />
          </div>

          <div className="border border-accent-dim bg-accent/[0.03] p-6 flex-1">
            <div className="text-[10px] text-accent tracking-[0.14em] uppercase mb-4">
              with net
            </div>
            <div className="flex flex-col items-center gap-3">
              <span className="border border-line px-4 py-2 text-[12px] text-ink">
                agent
              </span>
              <span className="text-accent text-[18px] leading-none">⌄</span>
              <span className="border border-accent text-accent px-5 py-2 text-[13px] lowercase tracking-[0.03em]">
                one shared operating layer
              </span>
              <p className="text-[11px] text-ink-dim leading-[1.5] text-center mt-1">
                discovery, access, work, files, streams, and control — in one
                place, instead of six fragile integrations.
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
