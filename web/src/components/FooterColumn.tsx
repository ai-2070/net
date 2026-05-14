import { cn } from "@/lib/cn";

export function FooterColumn({
  title,
  items,
}: {
  title: string;
  items: ReadonlyArray<{ href: string; label: string; class?: string }>;
}) {
  return (
    <div>
      <h5 className="text-[10px] tracking-[0.18em] text-ink-dim uppercase mb-4 font-medium">
        {title}
      </h5>
      <ul className="list-none space-y-2">
        {items.map((it) => {
          const external = /^https?:\/\//i.test(it.href);
          return (
            <li key={it.label}>
              <a
                href={it.href}
                {...(external
                  ? { target: "_blank", rel: "noopener noreferrer" }
                  : {})}
                className={cn(
                  "text-ink no-underline text-[12px] hover:text-accent transition-colors",
                  it.class,
                )}
              >
                {it.label}
              </a>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
