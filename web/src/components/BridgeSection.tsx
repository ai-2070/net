import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface BridgeQuestion {
  label: string;
  body: string;
}

const BRIDGE_QUESTIONS: readonly BridgeQuestion[] = [
  { label: "WHERE", body: "does the tool live?" },
  { label: "WHAT", body: "is online right now?" },
  { label: "WHO", body: "is calling, who is serving?" },
  { label: "WHEN", body: "the machine dies mid-call?" },
];

export function BridgeSection() {
  return (
    <section id="bridge" className="border-b border-line px-6 py-20">
      <SectionLabel>§02 / the bridge</SectionLabel>
      <DisplayHeading>
        MCP is the vocabulary.
        <br />
        <span className="text-accent">Net is the geography.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-10">
        MCP answered one question perfectly: what is a tool? It never tried to
        answer the rest —
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-8 mb-12">
        {BRIDGE_QUESTIONS.map((q) => (
          <div key={q.label} className="border-t border-accent-dim pt-4">
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2">
              {q.label}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6]">
              {q.body}
            </div>
          </div>
        ))}
      </div>

      <p className="text-[13px] text-ink-dim max-w-[740px] leading-[1.7] mb-10">
        HTTP forgets you between requests — by design. Not a flaw. A boundary.
        We build on the far side of it.
      </p>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-10">
        One line of config: any MCP server joins the mesh — findable, watchable,
        callable across machines, owner&apos;s rules, no API keys copied.
        Millions already running. It works both ways. And the network&apos;s job
        is to arrive in the model&apos;s context window small, current, and true
        — the announcement is the schema, so a stale type is unrepresentable.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          A phonebook is not <span className="text-accent">a dial tone.</span>
        </p>
      </div>
    </section>
  );
}
