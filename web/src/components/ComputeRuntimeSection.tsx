import { DaemonCaseBlock } from "./DaemonCaseBlock";
import { DisplayHeading } from "./DisplayHeading";
import { GroupCards } from "./GroupCards";
import { MigrationPipeline } from "./MigrationPipeline";
import { SectionLabel } from "./SectionLabel";
import { SpecStrip } from "./SpecStrip";

export function ComputeRuntimeSection() {
  return (
    <section
      id="runtime"
      className="compute-bg border-b border-line px-6 py-20"
    >
      <SectionLabel>§10 / daemon runtime // new</SectionLabel>
      <DisplayHeading>
        compute
        <br />
        <span className="text-accent">
          lives on
          <br />
          the wire.
        </span>
      </DisplayHeading>

      <p className="text-[16px] text-accent max-w-[740px] leading-[1.6] font-light mb-12">
        a daemon survives the host. identity travels. hosts don&apos;t.
      </p>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        A program on NET is called a{" "}
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
