import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const SURVIVES: ReadonlyArray<string> = [
  "laptops sleeping",
  "browsers crashing",
  "GPUs becoming busy",
  "devices reconnecting",
  "networks partitioning",
];

export function WhyNowSection() {
  return (
    <section id="why-now" className="border-b border-line px-6 py-20">
      <SectionLabel>§01 / why now</SectionLabel>
      <DisplayHeading>
        agents are
        <br />
        becoming <span className="text-accent">operators.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-10 mt-6">
        <div>
          <p className="text-[16px] text-ink max-w-[620px] leading-[1.6] font-light mb-5">
            The first wave of AI infrastructure was built around prompts, chats,
            and tool calls. That was enough when agents were mostly assistants.
          </p>
          <p className="text-[16px] text-ink max-w-[620px] leading-[1.6] font-light mb-5">
            It is not enough when they become operators.
          </p>
          <p className="text-[13px] text-ink-dim leading-[1.7] max-w-[620px]">
            An operating agent needs to know what machines exist, what each one
            can do, which resources are available, where files live, what
            streams are active, which jobs are running, and what authority it
            has at each boundary.
          </p>
        </div>

        <div className="border border-line bg-bg-2 p-7">
          <div className="text-[10px] text-ink-dim tracking-[0.15em] uppercase mb-4">
            an operating agent must survive
          </div>
          <ul className="daemon-list flex flex-col gap-2.5">
            {SURVIVES.map((s) => (
              <li
                key={s}
                className="relative pl-6 text-[13px] text-ink leading-[1.5]"
              >
                {s}
              </li>
            ))}
          </ul>
          <div className="border-t border-dashed border-line mt-6 pt-4">
            <p className="text-[13px] text-ink-dim leading-[1.6]">
              That is not a prompt-engineering problem.
            </p>
            <p className="text-[15px] text-accent leading-[1.5] mt-1 font-medium">
              It is a substrate problem.
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}
