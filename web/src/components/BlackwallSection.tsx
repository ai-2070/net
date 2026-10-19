import { BlackwallViz } from "./BlackwallViz";
import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";

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
    <section
      id="wall"
      className="relative pl-6 pr-12 lg:pl-10 lg:pr-24 py-24 lg:py-32"
    >
      <SectionLabel>§12 / the blackwall</SectionLabel>
      <DisplayHeading>
        safety isn&apos;t declared.
        <br />
        it&apos;s <span className="text-accent">derived.</span>
      </DisplayHeading>

      <BlackwallViz />

      <p className="text-[19px] md:text-[21px] text-ink max-w-[34em] leading-[1.5] font-light mb-14">
        In Cyberpunk, the Blackwall isn&apos;t a wall around the threats —
        it&apos;s a wall around the safe zone. NET works the same way. The
        &quot;safe mesh&quot; is the part you can observe: nodes that respond
        within heartbeat intervals, honor their capability announcements,
        don&apos;t flood, respect TTL.
      </p>

      <p className="text-[19px] md:text-[21px] text-accent max-w-[34em] leading-[1.5] font-light -mt-6 mb-14">
        The wall isn&apos;t one mechanism. It&apos;s the emergent effect of
        every constraint working together.
      </p>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-x-10 gap-y-10 mt-10">
        {BLACKWALL_ITEMS.map((item) => (
          <div key={item.tag} className="border-t border-line pt-5">
            <h4 className="text-[11px] text-ink uppercase tracking-[0.16em] mb-3">
              {item.tag}
            </h4>
            <p className="text-ink-dim text-[14px] leading-[1.6]">
              {item.body}
            </p>
          </div>
        ))}
      </div>

      <div className="slab slab-held px-8 md:px-12 py-10 md:py-12 mt-24 max-w-[960px]">
        <p className="text-[22px] md:text-[28px] text-ink leading-[1.3] font-light tracking-[-0.01em]">
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
