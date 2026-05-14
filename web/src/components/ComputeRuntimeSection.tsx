import { DaemonCaseBlock } from "./DaemonCaseBlock";
import { DisplayHeading } from "./DisplayHeading";
import { GroupCards } from "./GroupCards";
import { MigrationPipeline } from "./MigrationPipeline";
import { SectionLabel } from "./SectionHeadings";
import { SpecStrip } from "./SpecStip";

export function ComputeRuntimeSection() {
  return (
    <section
      id="runtime"
      className="compute-bg border-b border-line px-6 py-20"
    >
      <SectionLabel>§06 / daemon runtime // new</SectionLabel>
      <DisplayHeading>
        compute
        <br />
        <span className="text-accent">
          lives on
          <br />
          the wire.
        </span>
      </DisplayHeading>

      <div className="border border-accent-dim bg-accent/[0.03] px-5 py-4 mb-10 flex items-center gap-[18px] text-[11px] text-ink-dim tracking-[0.05em] flex-wrap">
        <span className="bg-accent text-bg px-2.5 py-1 font-bold tracking-[0.18em] text-[10px]">
          NEW
        </span>
        <span>
          <b className="text-ink font-medium">
            Stateful programs that live on the mesh, not on a machine.
          </b>{" "}
          They have cryptographic identity, a verifiable history, and they move
          between nodes mid-execution without anyone noticing.
        </span>
        <span className="ml-auto">
          subprotocol{" "}
          <code className="text-accent bg-accent/[0.06] px-1.5 py-0.5 font-mono">
            0x0500
          </code>
        </span>
      </div>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        A program on Net is called a{" "}
        <em className="not-italic text-accent bg-accent/[0.08] px-1">daemon</em>
        . Its identity is a public key — an{" "}
        <code className="text-accent bg-accent/[0.06] px-1.5 py-0.5 font-mono">
          origin_hash
        </code>{" "}
        derived from ed25519, which doesn&apos;t change when the daemon moves.
        Its history is a causal chain — every event it produces is signed and
        links to the previous one, verifiable by any node. Its location is
        wherever in the mesh has the capabilities it asked for.{" "}
        <strong className="text-ink font-medium">
          When that location goes away, the daemon doesn&apos;t.
        </strong>
      </p>

      <DaemonCaseBlock />
      <MigrationPipeline />
      {/*<SuperpositionViz />*/}
      <GroupCards />
      <SpecStrip />

      {/*<p className="mt-8 text-[11px] text-ink-dim text-center tracking-[0.05em]">
        // see <span className="text-accent">compute/daemon.rs</span> ·{" "}
        <span className="text-accent">compute/orchestrator.rs</span> ·{" "}
        <span className="text-accent">
          compute/{"{replica,fork,standby}"}_group.rs
        </span>
      </p>*/}
    </section>
  );
}
