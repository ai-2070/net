import { useEffect, useState, Fragment } from "react";

export interface AsciiPhase {
  rows: ReadonlyArray<React.ReactNode>;
  caption: React.ReactNode;
}

export function AsciiCycle({
  phases,
  intervalMs = 3500,
}: {
  phases: ReadonlyArray<AsciiPhase>;
  intervalMs?: number;
}) {
  const [phase, setPhase] = useState(0);

  useEffect(() => {
    const id = window.setInterval(() => {
      setPhase((p) => (p + 1) % phases.length);
    }, intervalMs);
    return () => window.clearInterval(id);
  }, [phases.length, intervalMs]);

  const current = phases[phase];
  if (!current) return null;

  return (
    <>
      {current.rows.map((row, i) => (
        <Fragment key={i}>
          {row}
          {"\n"}
        </Fragment>
      ))}
      {"\n"}
      {current.caption}
    </>
  );
}
