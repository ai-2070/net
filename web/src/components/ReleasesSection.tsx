import { useRepoInfo } from "./RepoInfoProvider";
import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { formatReleaseDate } from "@/lib/utils";

export function ReleasesSection() {
  const { releases } = useRepoInfo();
  if (releases.length === 0) return null;

  return (
    <section id="releases" className="relative px-6 lg:px-12 py-24 lg:py-32">
      <SectionLabel>§13 / releases</SectionLabel>
      <DisplayHeading>net releases.</DisplayHeading>

      <p className="text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14">
        Every tagged release pulled directly from{" "}
        <a
          href="https://github.com/ai-2070/net/releases"
          target="_blank"
          rel="noopener noreferrer"
          className="text-accent hover:text-ink transition-colors"
        >
          ai-2070/net
        </a>
        .
      </p>

      <div className="slab max-h-[640px] overflow-y-auto [mask-image:linear-gradient(180deg,#000_0,#000_calc(100%-88px),transparent_100%)]">
        {releases.map((r, i) => (
          <article
            key={r.tag}
            className={
              i % 2 ? "px-7 py-7 border-t border-line bg-black/25" : "px-7 py-7"
            }
          >
            <header className="flex items-baseline justify-between gap-4 mb-3 flex-wrap">
              <div className="flex items-baseline gap-3 flex-wrap">
                <span className="font-mono text-[16px] text-accent font-semibold">
                  {r.tag}
                </span>
                {r.codename ? (
                  <span className="font-mono text-[14px] text-ink uppercase tracking-[0.12em]">
                    <span className="text-ink-faint">Codename:</span> &ldquo;
                    <span className="font-semibold">{r.codename}</span>&rdquo;
                  </span>
                ) : null}
                {r.prerelease ? (
                  <span className="text-[12px] text-warn uppercase tracking-[0.15em] border border-warn px-1.5 py-0.5">
                    pre-release
                  </span>
                ) : null}
              </div>
              <a
                href={r.htmlUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[12px] text-ink-dim font-mono tracking-[0.05em] hover:text-accent transition-colors"
              >
                {formatReleaseDate(r.publishedAt)} ↗
              </a>
            </header>
            {r.bodyHtml ? (
              <div
                className="font-sans prose prose-invert max-w-none break-words [&_code]:break-all [&_pre]:whitespace-pre-wrap [&_li]:max-w-[70ch] prose-pre:font-mono prose-li:font-sans prose-headings:text-ink prose-headings:font-semibold prose-headings:tracking-tight prose-h1:text-[18px] prose-h2:text-[15px] prose-h3:text-[13px] prose-p:text-ink-dim prose-strong:text-ink prose-strong:font-medium prose-a:text-accent prose-a:no-underline hover:prose-a:underline prose-code:text-accent prose-code:font-mono prose-code:before:content-none prose-code:after:content-none prose-code:bg-bg prose-code:px-1 prose-code:py-0.5 prose-pre:bg-bg prose-pre:border prose-pre:border-line prose-ul:list-[square] prose-li:text-ink-dim prose-ul:marker:text-line prose-ol:text-ink-dim prose-hr:border-line prose-code:rounded-none"
                // Trusted: bodies come from repo maintainers' release notes via GitHub API.
                dangerouslySetInnerHTML={{ __html: r.bodyHtml }}
              />
            ) : (
              <p className="font-mono text-[13px] text-ink-faint italic">
                no notes
              </p>
            )}
          </article>
        ))}
        <div aria-hidden className="h-20" />
      </div>
    </section>
  );
}
