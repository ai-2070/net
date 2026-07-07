import { useRepoInfo } from "./RepoInfoProvider";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

export function ReleasesSection() {
  const { version, codename, buildDate } = useRepoInfo();

  return (
    <section id="releases" className="border-b border-line px-6 py-20">
      <SectionLabel>// releases</SectionLabel>
      <DisplayHeading>net releases.</DisplayHeading>

      <div className="mt-6 border border-line bg-bg-2 px-6 py-5 flex items-center justify-between gap-4 flex-wrap font-mono">
        <span className="flex items-center gap-2.5 flex-wrap text-[13px]">
          <span className="text-accent font-semibold text-[16px]">
            {version}
          </span>
          <span className="text-ink-faint">·</span>
          <span className="text-ink uppercase tracking-[0.12em]">
            {codename}
          </span>
          <span className="text-ink-faint">·</span>
          <span className="text-ink-dim">{buildDate}</span>
        </span>
        <a
          href="https://github.com/ai-2070/net/releases"
          target="_blank"
          rel="noopener noreferrer"
          className="text-accent hover:text-ink transition-colors text-[12px] tracking-[0.05em]"
        >
          full release notes on GitHub →
        </a>
      </div>
    </section>
  );
}
