import { DaemonCasePanel } from "./DaemonCasePanel";

export function DaemonCaseBlock() {
  return (
    <div className="grid grid-cols-1 lg:grid-cols-[1.1fr_0.9fr] gap-8 my-12 items-start">
      <DaemonCasePanel />

      <div>
        <h3 className="text-accent font-mono text-[14px] font-semibold tracking-[0.05em] uppercase mb-3.5">
          // what is a daemon
        </h3>
        <p className="text-ink-dim text-[13px] leading-[1.7] mb-4">
          Stateful programs that live on the mesh, not on a machine. It holds
          working state, snapshots periodically, and exposes five trait methods.
          Everything else — placement, migration, durability — is the runtime.
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
