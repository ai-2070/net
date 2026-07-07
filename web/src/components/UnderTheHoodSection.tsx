import Link from "next/link";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";
import { DaemonCasePanel } from "./DaemonCasePanel";

interface HoodCard {
  label: string;
  body: string;
  href: string;
}

const HOOD_CARDS: readonly HoodCard[] = [
  {
    label: "RUNTIME",
    body: "Mikoshi & daemons. Programs that survive their host.",
    href: "/runtime",
  },
  {
    label: "STORAGE",
    body: "Dataforts. Data became a fluid.",
    href: "/dataforts",
  },
  {
    label: "CLUSTER",
    body: "MeshOS. Programs move. Clusters think.",
    href: "/meshos",
  },
  {
    label: "PROTOCOL",
    body: "Nine axioms, four components, the spec.",
    href: "/protocol",
  },
];

export function UnderTheHoodSection() {
  return (
    <section id="under-the-hood" className="border-b border-line px-6 py-20">
      <SectionLabel>§07 / under the hood</SectionLabel>
      <DisplayHeading>
        under the <span className="text-accent">hood.</span>
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 border-t border-l border-line mt-10">
        {HOOD_CARDS.map((c) => (
          <Link
            key={c.label}
            href={c.href}
            className="group border-r border-b border-line p-7 transition-colors hover:bg-bg-2 relative no-underline"
          >
            <div className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2.5 flex items-center justify-between">
              {c.label}
              <span className="text-ink-faint transition-transform group-hover:translate-x-0.5">
                →
              </span>
            </div>
            <p className="text-ink-dim text-[12px] leading-[1.6]">{c.body}</p>
          </Link>
        ))}
      </div>

      <div className="max-w-[740px] mt-12">
        <DaemonCasePanel defaultCaseIndex={3} />
        <p className="text-[11px] text-ink-dim tracking-[0.05em] mt-3">
          A program that survives its host —{" "}
          <a
            href="/runtime"
            className="text-accent hover:text-ink transition-colors"
          >
            read the runtime →
          </a>
        </p>
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-12 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          Small pieces. <span className="text-accent">One mesh.</span>
        </p>
      </div>
    </section>
  );
}
