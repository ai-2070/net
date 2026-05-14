export function BlackwallViz() {
  return (
    <div
      className="relative w-full h-[220px] md:h-[280px] border border-line bg-black overflow-hidden mb-12"
      aria-hidden
    >
      <div className="absolute inset-0 blackwall-stripes-thick" />
      <div className="absolute inset-0 blackwall-stripes-thin" />
      <div className="absolute inset-0 blackwall-stripes-cyan pointer-events-none" />
      <div className="absolute inset-y-0 right-0 w-1/2 blackwall-burst pointer-events-none" />
      <div className="absolute inset-0 blackwall-scan pointer-events-none" />
      <div className="absolute inset-0 grid place-items-center pointer-events-none">
        <div
          className="font-display text-accent text-[clamp(20px,4vw,42px)] tracking-[0.32em] opacity-70"
          style={{ textShadow: "0 0 18px rgba(196,255,61,0.55)" }}
        >
          BLACKWALL
        </div>
      </div>
    </div>
  );
}
