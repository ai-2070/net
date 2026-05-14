import { useRepoInfo } from "./RepoInfoProvider";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";
import { formatReleaseDate } from "@/lib/utils";

export function ReleasesSection() {
  const { releases } = useRepoInfo();
  if (releases.length === 0) return null;

  return (
    <section id="releases" className="border-b border-line px-6 py-20">
      <SectionLabel>§13 / releases</SectionLabel>
      <DisplayHeading>net releases.</DisplayHeading>

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
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

      <div className="border border-line bg-bg-2 max-h-[640px] overflow-y-auto">
        {releases.map((r, i) => (
          <article
            key={r.tag}
            className={
              i % 2 ? "px-6 py-6 border-t border-line bg-black" : "px-6 py-6"
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
                  <span className="text-[10px] text-warn uppercase tracking-[0.15em] border border-warn px-1.5 py-0.5">
                    pre-release
                  </span>
                ) : null}
              </div>
              <a
                href={r.htmlUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="text-[10px] text-ink-dim font-mono tracking-[0.05em] hover:text-accent transition-colors"
              >
                {formatReleaseDate(r.publishedAt)} ↗
              </a>
            </header>
            {r.bodyHtml ? (
              <div
                className="prose prose-invert prose-sm max-w-none prose-headings:text-ink prose-headings:font-semibold prose-headings:tracking-tight prose-h1:text-[18px] prose-h2:text-[15px] prose-h3:text-[13px] prose-p:text-ink-dim prose-strong:text-ink prose-strong:font-medium prose-a:text-accent prose-a:no-underline hover:prose-a:underline prose-code:text-accent prose-code:font-mono prose-code:before:content-none prose-code:after:content-none prose-code:bg-bg prose-code:px-1 prose-code:py-0.5 prose-pre:bg-bg prose-pre:border prose-pre:border-line prose-ul:list-[square] prose-li:text-ink-dim prose-ul:marker:text-line prose-ol:text-ink-dim prose-hr:border-line prose-code:rounded-none"
                // Trusted: bodies come from repo maintainers' release notes via GitHub API.
                dangerouslySetInnerHTML={{ __html: r.bodyHtml }}
              />
            ) : (
              <p className="font-mono text-[12px] text-ink-faint italic">
                no notes
              </p>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
