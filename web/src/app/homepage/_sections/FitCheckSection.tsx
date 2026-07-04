import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const USE_WHEN: ReadonlyArray<string> = [
  "the system spans multiple machines, agents, devices, services, or sensors",
  "participants join, leave, sleep, partition, or reconnect",
  "capabilities need to be discovered dynamically",
  "files and artifacts move across nodes",
  "live streams matter",
  "work runs longer than a single request",
  "scarce resources need claims before use",
  "policy belongs at the tool, daemon, or resource boundary",
  "one central controller is brittle, expensive, or wrong",
];

const DONT_WHEN: ReadonlyArray<string> = [
  "one server and one database solve the problem",
  "HTTP or gRPC request/response is enough",
  "Redis, NATS, Kafka, Postgres, or Kubernetes already compose cleanly",
  "you only need a simple queue",
  "you don't need discovery, streams, artifacts, claims, durable tasks, subnets, or device federation",
];

export function FitCheckSection() {
  return (
    <section id="fit" className="border-b border-line px-6 py-20">
      <SectionLabel>§10 / fit check</SectionLabel>
      <DisplayHeading>
        use net when the network
        <br />
        becomes <span className="text-accent">part of the product.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-px bg-line border border-line mt-6">
        <div className="bg-bg p-8">
          <div className="text-[11px] text-accent tracking-[0.14em] uppercase mb-5 flex items-center gap-2">
            <span className="w-1.5 h-1.5 rounded-full bg-accent inline-block" />
            use net when
          </div>
          <ul className="flex flex-col gap-3">
            {USE_WHEN.map((u) => (
              <li
                key={u}
                className="flex gap-3 text-[13px] text-ink-dim leading-[1.5]"
              >
                <span className="text-accent shrink-0">+</span>
                <span>{u}</span>
              </li>
            ))}
          </ul>
        </div>

        <div className="bg-bg p-8">
          <div className="text-[11px] text-ink-dim tracking-[0.14em] uppercase mb-5 flex items-center gap-2">
            <span className="w-1.5 h-1.5 rounded-full bg-ink-faint inline-block" />
            do not use net when
          </div>
          <ul className="flex flex-col gap-3">
            {DONT_WHEN.map((d) => (
              <li
                key={d}
                className="flex gap-3 text-[13px] text-ink-dim leading-[1.5]"
              >
                <span className="text-ink-faint shrink-0">−</span>
                <span>{d}</span>
              </li>
            ))}
          </ul>
        </div>
      </div>

      <div className="mt-10 border-l-2 border-accent-dim pl-5 max-w-[760px]">
        <p className="text-[15px] text-ink leading-[1.6]">
          Net is not here to replace simple systems.{" "}
          <span className="text-accent">
            It is here for the systems that stop being simple.
          </span>
        </p>
      </div>
    </section>
  );
}
