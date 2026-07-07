import type { Metadata } from "next";
import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";
import { FooterDivider } from "@/components/FooterDivider";
import { SectionLabel } from "@/components/SectionLabel";
import { DisplayHeading } from "@/components/DisplayHeading";
import globals from "@/lib/globals";

export const metadata: Metadata = {
  title: "why net exists.",
};

interface Thesis {
  h2: string;
  body: string;
}

const THESES: readonly Thesis[] = [
  {
    h2: "Agents are network peers running on human infrastructure.",
    body: "We handed agents the web's plumbing — HTTP, OAuth, bearer tokens, CORS. All of it designed for a human with a browser and one identity. Agents are processes with delegated authority, budgets, lifespans, and parents. The web's trust model has no nouns for any of that. The mismatch isn't a bug to patch; it's a missing layer.",
  },
  {
    h2: "MCP standardized the noun. The geography is still missing.",
    body: "What a tool is — solved, and solved well. Where tools live, what's alive right now, who's calling, who's serving, what happens when the box dies mid-call — structurally out of scope for stateless HTTP, by deliberate and defensible choice. That's not a criticism. It's a boundary, and we build on the far side of it. MCP is the vocabulary; Net is the geography.",
  },
  {
    h2: "Identity is the actual primitive. Everything else is downstream.",
    body: "A bearer token proves what you hold, not who you are — and the server can't tell the user from the agent from the subagent from the attacker who read an env var. Meanwhile the client can't prove the server is who the catalog claims. Policy, pricing, credit, audit, coordination: every one requires both parties knowing who they're talking to, cryptographically, per message.",
  },
  {
    h2: "The model must never hold authority.",
    body: "The context window is a public space — anything in it can be injected, and anything the model can read can be exfiltrated. So authority lives outside: agents request typed operations, a policy daemon decides, keys are unrepresentable in any agent-visible surface, every grant is logged and revocable. The blast radius of a fully compromised agent collapses to: it asked the policy engine for permission, on the record.",
  },
  {
    h2: "The context window is scarce. The network should respect it.",
    body: "Two hundred tool definitions in every prompt is the network failing the model. Streams firehosed into context is the network failing the model. So: progressive discovery, pinned tools with live signed schemas — stale types are unrepresentable, the announcement is the schema — and streams that fold into local state the model queries. The network's job is to arrive in the context window small, current, and true.",
  },
  {
    h2: "Delegation is the org chart of the agent era.",
    body: "root → machine → agent → subagent. Each link individually revocable, budgeted, attributable. Subagent sprawl is a delegation problem, and OAuth's on-behalf-of machinery was never built for principals that fork. Key chains were. “Which subagent spent that, and can I revoke just it” should be a query, not an investigation.",
  },
  {
    h2: "Presence beats registry.",
    body: "A catalog tells you what existed at crawl time. An agent needs what's alive right now — announcements, liveness, capability-based routing, failover when the host dies mid-call. It's the difference between a phonebook and a dial tone.",
  },
  {
    h2: "Federation before economy.",
    body: "The network is useful at n = your own two machines. No marketplace cold start, no strangers required. Trust then widens outward — your machines, your team, your org, and eventually paid capabilities from attested strangers — on the same substrate, with the same objects. The economy is the last ring, not the premise.",
  },
];

const PULLQUOTES: Record<number, string> = {
  3: "A bearer token says what you hold. It doesn't say who you are.",
  4: "The model can ask. It cannot take.",
  5: "Streams feed state; models query folds.",
  7: "A phonebook is not a dial tone.",
};

function PullQuote({ text }: { text: string }) {
  return (
    <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] my-14 max-w-[760px]">
      <p className="text-[20px] text-accent leading-[1.4] font-light">{text}</p>
    </div>
  );
}

export default function WhyPage() {
  return (
    <>
      <NavBar />
      <main className="pt-20 max-w-[1440px] mx-auto">
        <section className="border-b border-line px-6 py-20">
          <SectionLabel>// why net exists</SectionLabel>
          <DisplayHeading>
            why <span className="text-accent">net</span> exists.
          </DisplayHeading>

          <p className="text-[16px] text-ink-dim max-w-[760px] leading-[1.7] font-light mb-16">
            Software is becoming a population. Here is the argument, one thesis
            at a time — each heading is the claim, each paragraph is the reason.
          </p>

          {THESES.map((t, i) => (
            <div key={t.h2}>
              <div className="max-w-[760px] mb-14">
                <h2 className="font-head text-[22px] leading-tight text-ink tracking-[0.02em] mb-4">
                  {t.h2}
                </h2>
                <p className="text-[15px] text-ink-dim leading-[1.75]">
                  {t.body}
                </p>
              </div>
              {PULLQUOTES[i] ? <PullQuote text={PULLQUOTES[i] as string} /> : null}
            </div>
          ))}
        </section>

        <section className="border-b border-line px-6 py-20">
          <SectionLabel>// the population</SectionLabel>
          <DisplayHeading>
            nobody assumes <span className="text-accent">a human.</span>
          </DisplayHeading>
          <p className="text-[16px] text-ink max-w-[760px] leading-[1.7] font-light mb-6">
            Software is becoming a population — autonomous programs that discover
            each other, trust each other, hire each other, and pay each other, on
            every device, at machine speed. A population needs a commons,
            identity, law, and an economy. The internet has none of them for this
            population, because every layer of it assumes a human on one end.
            Net is the internet where nobody assumes a human.
          </p>
          <p className="text-[13px] text-ink-dim max-w-[760px] leading-[1.75] mb-8">
            We ship Hermes as a native plugin — a full citizen: identity,
            streams, agent-to-agent, migration. Anything that speaks MCP is
            already a tourist through one config line. OpenClaw is next. Your
            runtime becoming a native is a plugin, not a partnership. The daemon
            does policy, consent, identity, and audit; your harness keeps doing
            what it's best at — being the brain.
          </p>
          <a
            href={`mailto:${globals.email}`}
            className="inline-flex items-center gap-2 text-[13px] text-accent hover:text-ink transition-colors"
          >
            talk to us →
          </a>
        </section>

        <FooterDivider />
        <Footer />
      </main>
    </>
  );
}
