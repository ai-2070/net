import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface IdItem {
  label: string;
  body: string;
}

const ID_ITEMS: readonly IdItem[] = [
  { label: "SIGNED", body: "Origin verified, per message" },
  { label: "UNREPRESENTABLE", body: "Keys invisible to agents" },
  { label: "TYPED", body: "Operations, never raw access" },
  { label: "REVOCABLE", body: "Every grant killable alone" },
];

export function IdentitySection() {
  return (
    <section id="identity" className="border-b border-line px-6 py-20">
      <SectionLabel>§04 / identity</SectionLabel>
      <DisplayHeading>
        no API keys.
        <br />
        <span className="text-accent">cryptographic identity.</span>
      </DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-6">
        A bearer token says what you hold — not who you are. The server
        can&apos;t tell the user from the agent from the attacker who read an env
        var. On Net, every message carries a{" "}
        <em className="not-italic text-accent bg-accent/10 px-1">
          signed origin
        </em>
        . Both parties. Every time.
      </p>

      <p className="text-[13px] text-ink-dim max-w-[740px] leading-[1.7] mb-10">
        And the model never touches a key. It requests;{" "}
        <em className="not-italic text-accent bg-accent/10 px-1">
          a policy daemon decides
        </em>
        ; every grant is logged and revocable. A fully compromised agent
        collapses to:{" "}
        <em className="not-italic text-accent bg-accent/10 px-1">
          it asked permission, on the record.
        </em>
      </p>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-8 mb-6">
        {ID_ITEMS.map((it) => (
          <div key={it.label} className="border-t border-accent-dim pt-4">
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2">
              {it.label}
            </div>
            <div className="text-ink-dim text-[12px] leading-[1.6]">
              {it.body}
            </div>
          </div>
        ))}
      </div>

      <p className="text-[12px] text-ink-dim max-w-[740px] leading-[1.6] mb-12">
        Delegation chains: root → machine → agent → subagent. Each link
        budgeted, attributable, individually revocable.
      </p>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          The model can ask.{" "}
          <span className="text-accent">It cannot take.</span>
        </p>
      </div>
    </section>
  );
}
