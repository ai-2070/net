"use client";

import { DisplayHeading } from "./DisplayHeading";
import { SectionLabel } from "./SectionLabel";
import { useEffect, useState } from "react";

interface AxiomCard {
  id: string;
  title: string;
  body: string;
  ascii: React.ReactNode;
}

const SUBSCRIPT_DIGITS = "₀₁₂₃₄₅₆₇₈₉";

function subscript(n: number): string {
  return String(n)
    .split("")
    .map((d) => SUBSCRIPT_DIGITS[Number.parseInt(d, 10)] ?? d)
    .join("");
}

function MarchingArrows() {
  const [pos, setPos] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setPos((p) => (p + 1) % 14);
    }, 180);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {Array.from({ length: 14 }, (_, i) => {
        const trail = (pos - i + 14) % 14;
        if (trail < 3) {
          const op = trail === 0 ? 1 : trail === 1 ? 0.7 : 0.35;
          return (
            <span key={i} className="text-accent" style={{ opacity: op }}>
              ▶
            </span>
          );
        }
        return <span key={i}>▶</span>;
      })}
      {"\n"}
      ░░░░░░░░░░░░░░
    </>
  );
}

function SequenceAdvance() {
  const [seq, setSeq] = useState(1);
  useEffect(() => {
    const id = window.setInterval(() => {
      setSeq((s) => (s % 99) + 1);
    }, 1200);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      e{subscript(seq)} → e{subscript(seq + 1)} → e{subscript(seq + 2)}
      {"\n"}
      chain.verify()
    </>
  );
}

interface LatencySample {
  value: string;
  label: string;
}

const LATENCY_SAMPLES: ReadonlyArray<LatencySample> = [
  { value: "0.20 ns", label: "fwd" },
  { value: "1.31 ns", label: "serialize" },
  { value: "0.93 ns", label: "pingwave" },
  { value: "0.31 ns", label: "gpu check" },
];

function LatencyPulse() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % LATENCY_SAMPLES.length);
    }, 1500);
    return () => window.clearInterval(id);
  }, []);
  const sample = LATENCY_SAMPLES[idx];
  if (!sample) return null;
  return (
    <>
      <span className="text-accent">{sample.value}</span>
      {"  ▸  "}
      {sample.label}
      {"\nsub-ns floor"}
    </>
  );
}

const TRUST_VALUES: ReadonlyArray<string> = [
  "observation",
  "evidence",
  "behavior",
  "proof",
];

function TrustCycle() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % TRUST_VALUES.length);
    }, 1600);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {"trust := "}
      <span className="text-accent">{TRUST_VALUES[idx]}</span>
      {"\nnot assumption"}
    </>
  );
}

const PAYLOAD_CHARS: ReadonlyArray<string> = ["░", "▒", "▓", "█", "▓", "▒"];

function SchemaPayload() {
  const [shift, setShift] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setShift((s) => (s + 1) % PAYLOAD_CHARS.length);
    }, 190);
    return () => window.clearInterval(id);
  }, []);
  const display = Array.from(
    { length: 5 },
    (_, i) => PAYLOAD_CHARS[(i + shift) % PAYLOAD_CHARS.length] ?? "░",
  ).join("");
  return (
    <>
      {"[hdr][hash]["}
      <span className="text-accent">{display}</span>
      {"]\nopaque payload"}
    </>
  );
}

const TYPE_VALUES: ReadonlyArray<string> = [
  "peer-pair",
  "{Token, Result}",
  "{Cmd, Ack}",
  "runtime",
];

function TypedCycle() {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setIdx((i) => (i + 1) % TYPE_VALUES.length);
    }, 1700);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      {"type ∈ "}
      <span className="text-accent">{TYPE_VALUES[idx]}</span>
      {"\nnot network"}
    </>
  );
}

function BackpressureFlow() {
  const [active, setActive] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => {
      setActive((a) => (a + 1) % 3);
    }, 1300);
    return () => window.clearInterval(id);
  }, []);
  return (
    <>
      <span className={active === 0 ? "text-accent" : undefined}>silent</span>
      {" → "}
      <span className={active === 1 ? "text-accent" : undefined}>suspect</span>
      {"\nsuspect → "}
      <span className={active === 2 ? "text-accent" : undefined}>reroute</span>
    </>
  );
}

const AXIOMS: readonly AxiomCard[] = [
  {
    id: "P.01",
    title: "Latency-first",
    body: "Sub-nanosecond header serialization. Nanosecond heartbeats, hops, recovery. Packet scheduling at timescales reserved for local function calls.",
    ascii: <LatencyPulse />,
  },
  {
    id: "P.02",
    title: "Streaming-first",
    body: "Data is continuous flow, not documents. Sharded ring buffers, adaptive batching. No requests and responses — everything is a stream.",
    ascii: <MarchingArrows />,
  },
  {
    id: "P.03",
    title: "Zero-copy",
    body: "Ring buffers, no garbage collector, native Rust. No unsafe. Forwarding doesn't allocate or copy payload data. Design principle, not optimization.",
    ascii: "[mem]──refs──▶[wire]\n   no alloc",
  },
  {
    id: "P.04",
    title: "Encrypted E2E",
    body: "Noise protocol handshakes. ChaCha20-Poly1305 AEAD with counter nonces. Every packet encrypted source→dest. Intermediate nodes never see plaintext.",
    ascii: "A ─ChaCha20──▶ B\n    relay sees ░░░",
  },
  {
    id: "P.05",
    title: "Untrusted relay",
    body: "Nodes forward packets without decrypting payloads. The mesh routes through infrastructure you don't trust. Networks grow through adversarial nodes.",
    ascii: <TrustCycle />,
  },
  {
    id: "P.06",
    title: "Schema-agnostic",
    body: "Transport moves bytes, not structures. Raw event = payload + hash. Protocol never inspects content. Structure emerges where participants agree.",
    ascii: <SchemaPayload />,
  },
  {
    id: "P.07",
    title: "Optionally ordered",
    body: "Ordering is per-stream, not global. Unordered path is the fast path. Causal ordering available where streams need it. Cost paid only by streams that require it.",
    ascii: <SequenceAdvance />,
  },
  {
    id: "P.08",
    title: "Optionally typed",
    body: "The protocol doesn't care what's in the payload. Behavior plane can. Typing is a local agreement between nodes, not a network requirement.",
    ascii: <TypedCycle />,
  },
  {
    id: "P.09",
    title: "Native backpressure",
    body: "Nodes drop without reply. Not a failure mode — the design. The proximity graph makes silence a signal. Automatic rerouting.",
    ascii: <BackpressureFlow />,
  },
];

export function PropertiesSection() {
  return (
    <section id="properties" className="border-b border-line px-6 py-20">
      <SectionLabel>§03 / protocol properties</SectionLabel>
      <DisplayHeading>
        nine axioms.
        <br />
        one runtime.
      </DisplayHeading>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-px bg-line border border-line">
        {AXIOMS.map((p) => (
          <div key={p.id} className="bg-bg p-7 transition-colors hover:bg-bg-2">
            <div className="text-[10px] text-accent tracking-[0.15em] mb-4">
              {p.id}
            </div>
            <h3 className="font-mono text-[14px] font-semibold tracking-[0.05em] text-ink mb-3 uppercase">
              {p.title}
            </h3>
            <p className="text-ink-dim text-[12px] leading-[1.65]">{p.body}</p>
            <pre className="text-accent-dim text-[10px] mt-4 leading-[1.2] whitespace-pre opacity-70">
              {p.ascii}
            </pre>
          </div>
        ))}
      </div>
    </section>
  );
}
