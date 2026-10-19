import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import globals from "@/lib/globals";
import Link from "next/link";
import { ArrowDown, ArrowRight } from "@phosphor-icons/react";
import { FieldCanvas } from "./field/FieldCanvas";

export function ClosingSection() {
  return (
    <section
      id="post-cloud"
      className="relative px-6 lg:px-12 pt-24 lg:pt-32 pb-0"
    >
      <SectionLabel>§14 / post-cloud</SectionLabel>
      <DisplayHeading>
        not anti-cloud.
        <br />
        <span className="text-accent">post-cloud.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-x-14 gap-y-1 mt-8">
        <div>
          <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
            Cloud infrastructure solves the wrong problem. It moves compute
            closer to a central provider.{" "}
            <strong className="text-ink font-medium">
              NET decouples storage and compute from hardware and location.
            </strong>
          </p>
          <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
            Cloud adds a trusted intermediary by definition.{" "}
            <strong className="text-ink font-medium">
              NET has no intermediaries.
            </strong>{" "}
            Relay nodes forward encrypted bytes they cannot read. There is no
            Cloudflare, no AWS, no Azure in the path because the path is yours.
          </p>
        </div>
        <div>
          <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
            <strong className="text-ink font-medium">
              Cloud was the right answer when compute was scarce and hardware
              was expensive.
            </strong>{" "}
            Compute is abundant. Hardware is cheap. The coordination layer
            should reflect that.
          </p>
          <p className="text-ink text-[17px] leading-[1.6] mb-5 max-w-[38em]">
            A manufacturing plant running on NET doesn&apos;t route sensor data
            to AWS us-east-1 and back. The sensor talks directly to the decision
            system on the factory floor.{" "}
            <strong className="text-ink font-medium">
              The latency is physics, not geography plus cloud overhead.
            </strong>
          </p>
        </div>
      </div>

      <div className="relative mt-28 -mx-6 lg:-mx-12 text-center py-28 md:py-36 overflow-hidden">
        <div className="absolute inset-0">
          <FieldCanvas variant="closing" />
        </div>
        <div
          aria-hidden
          className="absolute inset-0 pointer-events-none"
          style={{
            background:
              "radial-gradient(ellipse 46% 52% at 50% 50%, rgba(11,12,11,0.78), rgba(11,12,11,0) 100%), linear-gradient(180deg, rgba(11,12,11,1) 0%, rgba(11,12,11,0) 18%, rgba(11,12,11,0) 82%, rgba(11,12,11,1) 100%)",
          }}
        />
        <div
          className="relative font-display font-bold uppercase tracking-[-0.035em] text-ink leading-[0.92] mb-10"
          style={{ fontSize: "clamp(44px, 7vw, 112px)" }}
        >
          the mesh is <span className="text-accent">already</span>
          <br />
          running.
        </div>
        <Link href={globals.links.install} className="relative btn-field">
          <ArrowDown size={14} weight="bold" aria-hidden />
          Install NET
          <ArrowRight size={14} weight="bold" aria-hidden />
        </Link>
      </div>
    </section>
  );
}
