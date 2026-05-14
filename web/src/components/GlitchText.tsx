import { useEffect, useState } from "react";

const GLITCH_CHARS = "█▓▒░@#$%&*+=<>{}[]|/\\01";

function scrambleText(text: string, intensity: number): string {
  return text
    .split("")
    .map((c) => {
      if (c === " ") return " ";
      if (Math.random() > intensity) return c;
      const i = Math.floor(Math.random() * GLITCH_CHARS.length);
      return GLITCH_CHARS[i] ?? c;
    })
    .join("");
}

export function GlitchText({
  text,
  intervalMs = 4500,
  offsetMs = 0,
}: {
  text: string;
  intervalMs?: number;
  offsetMs?: number;
}) {
  const [chars, setChars] = useState(text);

  useEffect(() => {
    const timeouts: number[] = [];

    const cycle = (): void => {
      setChars(scrambleText(text, 0.7));
      timeouts.push(
        window.setTimeout(() => {
          setChars(scrambleText(text, 0.4));
        }, 70),
      );
      timeouts.push(
        window.setTimeout(() => {
          setChars(text);
        }, 160),
      );
    };

    let intervalId = 0;
    const startId = window.setTimeout(() => {
      cycle();
      intervalId = window.setInterval(cycle, intervalMs);
    }, offsetMs);

    return () => {
      window.clearTimeout(startId);
      if (intervalId) window.clearInterval(intervalId);
      for (const t of timeouts) window.clearTimeout(t);
    };
  }, [text, intervalMs, offsetMs]);

  return <>{chars}</>;
}
