"""Paid agent-to-agent admission, end to end in one process (Python).

Run: python a2a_paid.py

What it proves: a provider that is **explicitly paid** by configuration
refuses free work, quotes before any money moves, and runs the task exactly
once after the caller pays for that exact reservation. Both halves are real —
a `PaymentProvider` with a `PaymentEngine`, a durable admission journal on
disk, and a `CapabilityGateway` with a spend policy and a durable purchase
store. Nothing is stubbed.

The two nodes are in one process only so the example is a single file. Nothing
about the flow depends on that: they are separate mesh nodes that complete a
real noise handshake over loopback UDP and speak the same wire two machines
would.

WHAT IT DELIBERATELY DOES NOT DO: settle on a real chain. The facilitator is
the mock (`unsafe_dev_mock_facilitator=True`), which is the only honest choice
for an example — a runnable rail needs funded keys and a testnet. The
*lifecycle* is real; the settlement is not. See `payments.md`.

The order is the whole design, and it is what the three numbered steps below
show: everything that can refuse the work happens BEFORE a quote exists, and
the launch is claimed durably BEFORE the executor is spawned.
"""

from __future__ import annotations

import json
import tempfile
import threading
import time
from pathlib import Path

from net import CapabilityGateway, NetMesh, PaymentProvider, PaymentRefused

PSK = "a7" * 32
SERVICE = "summarize"
REVISION = "r1"

# What the provider charges. The mock scheme settles instantly and moves no
# real value; every other field is shaped exactly as a live x402 rail's would
# be, which is why the caller's spend policy can reason about it unchanged.
PRICE = [
    {
        "scheme": "mock",
        "network": "mock:net",
        "amount": "2500",
        "asset": "musd",
        "payTo": "mock-provider-settle-addr",
        "maxTimeoutSeconds": 60,
    }
]


def handshake(connector: NetMesh, acceptor: NetMesh) -> None:
    """Complete a real noise handshake between two live nodes.

    `accept` blocks until a peer dials, so it runs on its own thread. This is
    the ordinary two-sided mesh connect, not an A2A detail.
    """
    errors: list[BaseException] = []

    def accept() -> None:
        try:
            acceptor.accept(connector.node_id)
        except BaseException as exc:  # noqa: BLE001 - re-raised on the main thread
            errors.append(exc)

    thread = threading.Thread(target=accept, daemon=True)
    thread.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    thread.join(timeout=5)
    if errors:
        raise errors[0]
    # A join that TIMED OUT is not a completed handshake. Without this the
    # example sails on with no session and fails much later with a routing
    # error that names neither the handshake nor this thread.
    if thread.is_alive():
        raise RuntimeError(
            "the accept side is still blocked 5s after connect returned: the "
            "noise handshake never completed, so nothing below is routable"
        )


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        state = Path(tmp)
        provider_mesh = NetMesh("127.0.0.1:0", PSK, permissive_channels=True)
        caller_mesh = NetMesh("127.0.0.1:0", PSK, permissive_channels=True)

        # THE CROSS-NODE IDIOM, AND IT IS EASY TO GET WRONG: handshake while
        # both nodes are still UNSTARTED, then start them. A started node's
        # receive loop auto-accepts and races the manual `accept`, which times
        # the handshake out — and the symptom is not a handshake error, it is
        # every later call failing with "no route to target".
        handshake(caller_mesh, provider_mesh)
        provider_mesh.start()
        caller_mesh.start()

        ran: list[str] = []

        async def execute(task_id, prompt, context_refs, tags, *, service, revision):
            """The actual work. Reached only after the provider has redeemed
            the payment and durably claimed the launch."""
            ran.append(task_id)
            return f"blob://{service}/{task_id}"

        provider = PaymentProvider(
            provider_mesh,
            str(state / "payment-engine.json"),
            billing_log_path=str(state / "billing.jsonl"),
            unsafe_dev_mock_facilitator=True,
        )

        # Pricing terms are bound to the exact capability being sold, so a
        # quote for one service can never admit work on another.
        terms = provider.pricing_terms(
            f"{provider_mesh.node_id}/net.a2a.task/{SERVICE}", json.dumps(PRICE)
        )

        # A paid service MUST carry pricing terms, a payment gate and a
        # journal. Omit any one and `serve_a2a_configured` refuses to start
        # rather than quietly serving the work for free.
        serving = provider.serve_a2a_configured(
            execute,
            {
                SERVICE: {
                    "revision": REVISION,
                    "pricing_terms": terms,
                    "bounds": {
                        "max_prompt_bytes": 1024,
                        "max_context_refs": 8,
                        "max_tags": 8,
                        "max_tag_bytes": 64,
                        "max_in_flight": 4,
                    },
                    "reservation_ttl_secs": 600,
                    "reservation_retention_secs": 604800,
                    "retention_secs": 3600,
                    "description": "summarize a document",
                }
            },
            str(state / "admission-journal.json"),
        )

        gateway = CapabilityGateway(
            caller_mesh,
            payment_policy_path=str(state / "spend-policy.json"),
            payment_profile="dev_test",
            a2a_purchase_path=str(state / "a2a-purchases.json"),
        )

        try:
            # 1. PREPARE — validate, preflight, reserve capacity, mint the
            #    admission id, quote. No money moves here. `busy` reserved
            #    nothing and quoted nothing, so it is retryable; it is also how
            #    a not-yet-routable first call to a fresh peer reports itself
            #    (a handshake establishes the session, but the reply channel
            #    additionally needs the target to have pinned our EntityId
            #    from a signature-verified announcement).
            for _ in range(80):
                prep = json.loads(
                    gateway.prepare_task(
                        provider_mesh.node_id, SERVICE, "summarize the filings"
                    )
                )
                if prep["status"] != "busy":
                    break
                time.sleep(0.1)
            assert prep["status"] == "ok", f"provider refused to prepare: {prep}"
            prepared = json.dumps(prep["prepared"])
            print(f"prepared: quote={prep['quote']['amount']} (no money moved yet)")

            # A RESERVATION IS NOT AN ADMISSION. That prepare reserved
            # capacity and minted an admission id for this exact work, and a
            # submit carrying no payment is STILL refused before the executor
            # — with the provider's machine-actionable failure schematic
            # surviving the language boundary rather than flattening to an
            # error string. This is what "didn't pay" looks like on the wire:
            # the real handle, an empty proof.
            try:
                caller_mesh.submit_task_paid(
                    prepared, json.dumps({"quote_id": "", "binding_sig": []})
                )
                raise AssertionError("a paid service accepted unpaid work")
            except PaymentRefused as refused:
                # Optional by type, and the assertion is the point: a refusal
                # MAY arrive with no schematic, and one that does is a much
                # weaker answer — prose a caller has to parse.
                assert refused.schematic is not None, (
                    "the provider's failure schematic must survive the boundary"
                )
                schematic = json.loads(refused.schematic)
                assert schematic["object"] == "net.payment.failure@1", schematic
                assert schematic["handler_executed"] is False, schematic
                print(
                    f"unpaid submit refused at {schematic['stage']}: "
                    f"{schematic['reason']} (executor never ran)"
                )

            # 2. PURCHASE — consumes THAT quote, against that one reservation.
            #    Never re-quote a live attempt: a fresh quote for the same work
            #    is a second charge.
            buy = json.loads(gateway.purchase_task(prepared))
            assert buy["status"] == "paid", f"purchase did not complete: {buy}"
            print("purchased: the attempt is durable and resumable")

            # 3. SUBMIT — sends the brief with the stored proof, byte-identical
            #    on every retry. The provider redeems, claims the launch, runs.
            ack = json.loads(gateway.submit_task(prepared))
            assert ack["status"] == "accepted", f"submit was not accepted: {ack}"
            task_id = ack["task_id"]

            # Poll the observable the claim is about, never a fixed sleep.
            for _ in range(160):
                raw = caller_mesh.task_status(provider_mesh.node_id, task_id)
                if raw is not None and json.loads(raw)["state"]["state"] == "completed":
                    break
                time.sleep(0.05)
            else:
                raise AssertionError(f"task {task_id} never completed")

            # The two properties the whole design exists for: the work ran
            # once, and it was charged once.
            assert ran == [task_id], f"expected exactly one run, got {ran}"
            billing = [json.loads(e) for e in provider.read_billing()]
            assert len(billing) == 1, f"expected exactly one charge, got {billing}"

            print(f"paid task ran once: task={task_id} charges={len(billing)}")
        finally:
            serving.stop()
            caller_mesh.shutdown()
            provider_mesh.shutdown()


if __name__ == "__main__":
    main()
