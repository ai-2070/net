import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

export function AgentEconomySection() {
  return (
    <section id="economy" className="border-b border-line px-6 py-20">
      <SectionLabel>§01 / the missing layer</SectionLabel>
      <DisplayHeading>
        the internet sees one user.
        <br />
        <span className="text-accent">behind him, a million agents.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-6">
        Agents are processes with delegated authority, budgets, lifespans, and
        parents. The web&apos;s trust model has no nouns for any of that.
      </p>

      <p className="text-[13px] text-ink-dim max-w-[740px] leading-[1.7] mb-6">
        We handed agents the web&apos;s plumbing — HTTP, OAuth, bearer tokens,
        registries. All of it designed for a human with a browser and one
        identity.
      </p>

      <p className="text-[13px] text-ink-dim max-w-[740px] leading-[1.7] mb-10">
        Tool-definition bloat in every prompt. Token sprawl. Injection blast
        radius. Subagents with no identity story. Stale schemas. Multi-agent
        duct tape. Different costumes, same absence — the network layer agents
        were supposed to have. We built minds without hands — and intelligence
        means nothing if it can&apos;t act on the real world.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          Every device becomes a tool. Every agent becomes a peer.{" "}
          <span className="text-accent">
            The same sentence, read from both ends.
          </span>
        </p>
      </div>
    </section>
  );
}
