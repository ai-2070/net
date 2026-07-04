import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";

const NODE_TYPES: ReadonlyArray<string> = [
  "agent",
  "laptop",
  "server",
  "gpu worker",
  "browser bridge",
  "file store",
  "robot",
  "sensor",
  "daemon",
  "app service",
];

const CAN_DO: ReadonlyArray<string> = [
  "expose capabilities",
  "publish state",
  "stream observations",
  "move artifacts",
  "accept work",
  "reject unsafe requests",
  "coordinate with nearby participants",
];

export function WhatNetIsSection() {
  return (
    <section id="what" className="border-b border-line px-6 py-20">
      <SectionLabel>§03 / what net is</SectionLabel>
      <DisplayHeading>
        a mesh for
        <br />
        <span className="text-accent">autonomous participants.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-10 mt-6">
        <div>
          <p className="text-[13px] text-ink-dim tracking-[0.12em] uppercase mb-3">
            a net node can be
          </p>
          <div className="flex flex-wrap gap-2 mb-8">
            {NODE_TYPES.map((n) => (
              <span
                key={n}
                className="border border-line bg-bg-2 px-3 py-1.5 text-[12px] text-ink lowercase tracking-[0.03em]"
              >
                {n}
              </span>
            ))}
          </div>

          <p className="text-[13px] text-ink-dim tracking-[0.12em] uppercase mb-3">
            each node can
          </p>
          <ul className="daemon-list flex flex-col gap-2">
            {CAN_DO.map((c) => (
              <li
                key={c}
                className="relative pl-6 text-[13px] text-ink leading-[1.5]"
              >
                {c}
              </li>
            ))}
          </ul>
        </div>

        <div className="flex flex-col gap-5">
          <div className="border border-line bg-bg-2 p-7">
            <p className="text-[15px] text-ink leading-[1.6] mb-4">
              Net does not assume one central commander.
            </p>
            <p className="text-[15px] text-accent leading-[1.5] font-medium">
              Participants own their lane. The mesh coordinates, but it does not
              override.
            </p>
          </div>
          <div className="border border-line bg-bg-2 p-7 flex-1">
            <p className="text-[13px] text-ink-dim leading-[1.7]">
              The result is a system where autonomous participants can discover
              each other, understand what is possible, exchange live
              information, move durable artifacts, and complete work across
              unreliable networks.
            </p>
            <div className="border-t border-dashed border-line mt-5 pt-4 font-mono text-[11px] text-ink-dim tracking-[0.04em]">
              discover · understand · exchange · move · complete · recover
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
