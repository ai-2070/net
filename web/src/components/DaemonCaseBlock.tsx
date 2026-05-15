"use client";

import { useRef, useEffect, useState, useMemo, Fragment } from "react";

interface DaemonCase {
  subtitle: string;
  code: React.ReactNode;
}

const DAEMON_CASES: readonly DaemonCase[] = [
  {
    subtitle: "trading agent · NYSE colo",
    code: (
      <>
        <span className="cm">
          // node A is failing — daemon migrates to node B
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">TraderConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;200μs to NYSE&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">100</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Order</span>(o) =&gt; bus.
        <span className="fn">publish</span>(o).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// state moves with us</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // origin_hash unchanged. subscribers don&apos;t notice.
        </span>
      </>
    ),
  },
  {
    subtitle: "inference daemon · follows user",
    code: (
      <>
        <span className="cm">
          // user moves laptop → desktop. session continues.
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(
        <span className="ty">InferenceConfig</span> {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Gpu</span>(
        <span className="st">&quot;vram&gt;=24gb&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;user:7af3&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">250</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Token</span>(t) =&gt; stream.
        <span className="fn">push</span>(t).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// kv cache moves with us</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // conversation context preserved across hardware change.
        </span>
      </>
    ),
  },
  {
    subtitle: "factory controller · plant-04",
    code: (
      <>
        <span className="cm">
          // edge box thermal alarm → migrate to standby
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">PlcConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;5ms to actuator&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;floor-A&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">50</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Command</span>(c) =&gt; actuator.
        <span className="fn">send</span>(c).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// control loop unaffected</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // torque feedback never breaks. assembly line keeps moving.
        </span>
      </>
    ),
  },
  {
    subtitle: "sensor fusion · vehicle-07",
    code: (
      <>
        <span className="cm">
          // LIDAR + radar + camera, mesh-routed perception
        </span>
        {"\n"}
        <span className="kw">let</span> daemon = Daemon::
        <span className="fn">new</span>(<span className="ty">FusionConfig</span>{" "}
        {"{"}
        {"\n    "}
        <span className="fn">requirements</span>:{" "}
        <span className="kw">vec</span>![ <span className="ty">Cap</span>::
        <span className="fn">Latency</span>(
        <span className="st">&quot;&lt;1ms&quot;</span>),{" "}
        <span className="ty">Cap</span>::<span className="fn">Tag</span>(
        <span className="st">&quot;vehicle-07&quot;</span>) ],
        {"\n    "}
        <span className="fn">snapshot_interval</span>:{" "}
        <span className="ty">Duration</span>::
        <span className="fn">millis</span>(<span className="st">20</span>),
        {"\n"}
        {"});"}
        {"\n\n"}
        <span className="kw">match</span> daemon.
        <span className="fn">tick</span>
        (event).<span className="kw">await</span>? {"{"}
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Detection</span>(d) =&gt; bus.
        <span className="fn">publish</span>(d).
        <span className="kw">await</span>?,
        {"\n    "}
        <span className="ty">Outcome</span>::
        <span className="fn">Migrate</span>(target) =&gt;{" "}
        <span className="cm">// perception state moves</span>
        {"\n"}
        {"}"}
        {"\n\n"}
        <span className="cm">
          // neighboring vehicles see continuous track.
        </span>
      </>
    ),
  },
];

type CodeTokenCls = "kw" | "ty" | "fn" | "cm" | "st";

interface CodeToken {
  text: string;
  cls?: CodeTokenCls;
}

const CODE_TOKEN_CLASSES = new Set<string>(["kw", "ty", "fn", "cm", "st"]);

function isCodeTokenCls(s: unknown): s is CodeTokenCls {
  return typeof s === "string" && CODE_TOKEN_CLASSES.has(s);
}

function flattenCodeJsx(node: React.ReactNode): CodeToken[] {
  const out: CodeToken[] = [];
  const walk = (n: React.ReactNode, cls?: CodeTokenCls): void => {
    if (n == null || typeof n === "boolean") return;
    if (typeof n === "string" || typeof n === "number") {
      out.push({ text: String(n), cls });
      return;
    }
    if (Array.isArray(n)) {
      for (const child of n) walk(child, cls);
      return;
    }
    if (typeof n === "object" && "props" in n) {
      const elem = n as React.ReactElement<{
        children?: React.ReactNode;
        className?: string;
      }>;
      const cn = elem.props.className;
      const newCls = isCodeTokenCls(cn) ? cn : cls;
      walk(elem.props.children, newCls);
    }
  };
  walk(node);
  return out;
}

function totalChars(tokens: readonly CodeToken[]): number {
  let n = 0;
  for (const t of tokens) n += t.text.length;
  return n;
}

function renderTypedTokens(
  tokens: readonly CodeToken[],
  charLimit: number,
): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let remaining = charLimit;
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (!t || remaining <= 0) break;
    const slice =
      t.text.length <= remaining ? t.text : t.text.slice(0, remaining);
    if (t.cls) {
      out.push(
        <span key={i} className={t.cls}>
          {slice}
        </span>,
      );
    } else {
      out.push(<Fragment key={i}>{slice}</Fragment>);
    }
    remaining -= slice.length;
  }
  return out;
}

const TYPING_CPS = 95;
const DWELL_SECONDS = 1.4;

export function DaemonCaseBlock() {
  const caseIdxRef = useRef(0);
  const charIdxRef = useRef(0);
  const dwellRef = useRef(0);
  const [, forceUpdate] = useState(0);

  const caseTokens = useMemo(
    () => DAEMON_CASES.map((c) => flattenCodeJsx(c.code)),
    [],
  );

  useEffect(() => {
    let rafId = 0;
    let last = performance.now();
    const loop = (now: number): void => {
      const dt = (now - last) / 1000;
      last = now;
      const tokens = caseTokens[caseIdxRef.current] ?? [];
      const total = totalChars(tokens);
      if (charIdxRef.current < total) {
        charIdxRef.current = Math.min(
          total,
          charIdxRef.current + dt * TYPING_CPS,
        );
      } else {
        dwellRef.current += dt;
        if (dwellRef.current >= DWELL_SECONDS) {
          dwellRef.current = 0;
          caseIdxRef.current = (caseIdxRef.current + 1) % DAEMON_CASES.length;
          charIdxRef.current = 0;
        }
      }
      forceUpdate((n) => n + 1);
      rafId = requestAnimationFrame(loop);
    };
    rafId = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(rafId);
  }, [caseTokens]);

  const idx = caseIdxRef.current;
  const current = DAEMON_CASES[idx];
  const tokens = caseTokens[idx] ?? [];
  if (!current) return null;
  const limit = Math.floor(charIdxRef.current);
  const isTyping = limit < totalChars(tokens);

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_0.9fr] gap-8 my-12 items-start">
      <div className="border border-line bg-bg-2 overflow-hidden">
        <div className="bg-bg border-b border-line px-3.5 py-2 text-[10px] text-ink-dim tracking-[0.12em] uppercase flex justify-between items-center">
          <span key={`title-${idx}`} className="daemon-fade">
            <span className="text-accent font-semibold">CASE</span> ·{" "}
            {current.subtitle}
          </span>
          <span className="inline-flex gap-1.5 items-center">
            {DAEMON_CASES.map((_, i) => (
              <button
                key={i}
                type="button"
                aria-label={`Show case ${i + 1}`}
                onClick={() => {
                  caseIdxRef.current = i;
                  charIdxRef.current = 0;
                  dwellRef.current = 0;
                  forceUpdate((n) => n + 1);
                }}
                className={`w-1.5 h-1.5 rounded-full transition-colors cursor-pointer ${
                  i === idx ? "bg-accent" : "bg-ink-faint hover:bg-ink-dim"
                }`}
              />
            ))}
          </span>
        </div>
        <pre className="px-5 py-4 text-[12px] leading-[1.7] text-ink overflow-x-auto font-mono min-h-[260px]">
          {renderTypedTokens(tokens, limit)}
          <span
            className={isTyping ? "text-accent" : "cursor-blink"}
            aria-hidden
          >
            ▋
          </span>
        </pre>
      </div>

      <div>
        <h3 className="text-accent font-mono text-[14px] font-semibold tracking-[0.05em] uppercase mb-3.5">
          // what is a daemon
        </h3>
        <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
          Stateful programs that live on the mesh, not on a machine. It
          holds working state, snapshots periodically, and exposes five trait
          methods. Everything else — placement, migration, durability — is the
          runtime.
        </p>
        <ul className="daemon-list list-none mt-4">
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">cryptographic identity</b> —
            origin_hash from ed25519. survives moves.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">causal chain</b> — every event
            signed, links to parent. self-authenticating.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">capability requirements</b> —
            daemon declares needs. mesh finds matching node.
          </li>
          <li className="py-2.5 pl-5 border-b border-line text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">snapshot + replay</b> — state
            captured periodically. gap replayed on restore.
          </li>
          <li className="py-2.5 pl-5 text-ink-dim text-[12px] leading-[1.5] relative">
            <b className="text-ink font-medium">opaque to mesh</b> — what the
            daemon does is its business. mesh just hosts.
          </li>
        </ul>
      </div>
    </div>
  );
}
