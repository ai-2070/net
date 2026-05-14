import { BlackwallViz } from "./BlackwallViz";
import { SectionLabel } from "./SectionLabel";
import { DisplayHeading } from "./DisplayHeading";

interface BlackwallItem {
  tag: string;
  body: string;
}

const BLACKWALL_ITEMS: readonly BlackwallItem[] = [
  {
    tag: "▸ Backpressure",
    body: "Nodes limit in-flight events, prevent overload, and apply pushback by going silent. No node can be forced to accept more than it can process.",
  },
  {
    tag: "▸ Bounded queues",
    body: "No infinite buffers. Ring buffers have explicit capacity limits. A flood fills a buffer and gets evicted, it doesn't grow the buffer.",
  },
  {
    tag: "▸ Fanout limits",
    body: "Events don't propagate to everyone. Dissemination is controlled by the proximity graph and routing table. Prevents O(n²) explosion.",
  },
  {
    tag: "▸ Deduplication",
    body: "The same event doesn't explode repeatedly. Idempotency at the event level protects against loops and amplification.",
  },
  {
    tag: "▸ TTL limits",
    body: "Events expire. Pingwaves have a hop radius. A misbehaving node's traffic dies at the boundary of its TTL, not the edge of the mesh.",
  },
  {
    tag: "▸ Rate limits",
    body: "Per-node, per-peer limits. One node cannot flood the mesh. Its neighbors enforce their own limits independently through device autonomy rules.",
  },
];

export function BlackwallSection() {
  return (
    <section id="wall" className="blackwall-bg border-b border-line px-6 py-20">
      <SectionLabel>§12 / the blackwall</SectionLabel>
      <DisplayHeading>
        safety isn&apos;t declared.
        <br />
        it&apos;s <span className="text-accent">derived.</span>
      </DisplayHeading>

      <BlackwallViz />

      <p className="text-[16px] text-ink max-w-[740px] leading-[1.6] font-light mb-12">
        In Cyberpunk, the Blackwall isn&apos;t a wall around the threats —
        it&apos;s a wall around the safe zone. NET works the same way. The
        &quot;safe mesh&quot; is the part you can observe: nodes that respond
        within heartbeat intervals, honor their capability announcements,
        don&apos;t flood, respect TTL.
      </p>

      <p className="text-[16px] text-accent max-w-[740px] leading-[1.6] font-light -mt-8 mb-12">
        The wall isn&apos;t one mechanism. It&apos;s the emergent effect of
        every constraint working together.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-8 mt-10">
        {BLACKWALL_ITEMS.map((item) => (
          <div key={item.tag} className="border-t border-accent-dim pt-4">
            <h4 className="text-[11px] text-accent uppercase tracking-[0.15em] mb-2.5">
              {item.tag}
            </h4>
            <p className="text-ink-dim text-[12px] leading-[1.6]">
              {item.body}
            </p>
          </div>
        ))}
      </div>

      <div className="border-l-2 border-accent pl-8 pr-8 py-6 bg-accent/[0.02] mt-16 max-w-[900px]">
        <p className="text-[18px] text-ink leading-[1.5] font-light">
          Any single mechanism can be overwhelmed. All of them together form the
          wall.{" "}
          <strong className="text-accent font-medium">
            No single point to breach because the Blackwall is the mesh itself.
          </strong>
        </p>
      </div>
    </section>
  );
}
