"""Paid A2A through ``net_sdk`` — the live consumer behind
``tests/test_paid_a2a_sdk.py`` (``NODE_A2A_PAID_ADMISSION_PLAN.md`` WS-G).

Run as its own interpreter: ``tests/conftest.py`` stubs ``net`` for the
native-free SDK suite, so an in-process pytest here would test the stub.

    python paid_a2a_consumer.py --cell CELL

Cells:

- ``sync``     — two ``net_sdk.MeshNode``s; provider and gateway from the
  factories; prepare → purchase → submit runs the task exactly once; both
  nodes shut down after stop/close.
- ``async``    — the same over ``AsyncMeshNode``; the paid verbs go through
  ``asyncio.to_thread`` on the sync gateway (paid A2A is sync-gateway-only).
- ``native``   — the factories also accept a raw ``net.NetMesh``. This cell
  imports ``net`` because a native object is what it tests.
- ``refusals`` — the boundary's refusals. Also imports ``net``: the first
  control is that the *native* gateway still refuses an SDK ``MeshNode``.
- ``same_org`` — review R6 from the SDK: a provider serving under
  ``principal="same_org"`` (all five services PROTECTED), an SDK
  ``OrgClient`` installed on BOTH slots — the ``MeshNode``'s (raw verbs) and
  the gateway's (the paid lifecycle) — then cleared, denying before launch.
  Needs ``--scenario-dir``: the generated same-org artifacts.

``sync`` and ``async`` import the standard library and ``net_sdk`` only.
Every cell prints ``CELL_OK <json receipt>`` on success and exits 0; any
assertion failure exits non-zero with its traceback.
"""

from __future__ import annotations

import argparse
import asyncio
import gc
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import traceback

import net_sdk
from net_sdk.payments import (
    create_async_capability_gateway,
    create_capability_gateway,
    create_payment_provider,
    set_a2a_org_caller,
)

PSK = "b4" * 32

# Every cell's scratch directory lives under one per-process root, so a
# failing cell's files can be removed before the hard exit in `main` even
# when a `TemporaryDirectory` could not clean up while unwinding (on Windows
# a live native handle can still hold a file open at that point).
_SCRATCH = tempfile.mkdtemp(prefix="paid-a2a-consumer-")
PAID = "summarize"
MOCK_REQS = json.dumps(
    [
        {
            "scheme": "mock",
            "network": "mock:net",
            "amount": "2500",
            "asset": "musd",
            "payTo": "mock-provider-settle-addr",
            "maxTimeoutSeconds": 60,
        }
    ]
)


def _offer(terms):
    return {
        "revision": "r1",
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
    }


def _handshake(connector, acceptor) -> None:
    errors: list = []

    def _accept() -> None:
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errors:
        raise errors[0]


def _wait(predicate, what: str, timeout: float = 10.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what} (last={last!r})")


class _Stage:
    """A served paid provider and a caller gateway, built by the factories
    over whatever mesh handles the cell hands in."""

    def __init__(self, provider_mesh, caller_mesh, provider_node_id: int, dirpath: str) -> None:
        self.ran: list = []
        self.provider = create_payment_provider(
            provider_mesh,
            os.path.join(dirpath, "engine.json"),
            billing_log_path=os.path.join(dirpath, "billing.jsonl"),
            unsafe_dev_mock_facilitator=True,
        )
        terms = self.provider.pricing_terms(
            f"{provider_node_id}/net.a2a.task/{PAID}", MOCK_REQS
        )
        ran = self.ran

        async def run_task(task_id, prompt, refs, tags, *, service, revision):
            ran.append(task_id)
            return f"blob://{service}/{task_id}"

        self.handle = self.provider.serve_a2a_configured(
            run_task, {PAID: _offer(terms)}, os.path.join(dirpath, "journal.json")
        )
        self.gateway = create_capability_gateway(
            caller_mesh,
            payment_policy_path=os.path.join(dirpath, "spend-policy.json"),
            payment_profile="dev_test",
            a2a_purchase_path=os.path.join(dirpath, "a2a-purchases.json"),
        )
        self.provider_node_id = provider_node_id

    def prepare(self, task_id: str) -> dict:
        def attempt():
            env = json.loads(
                self.gateway.prepare_task(self.provider_node_id, PAID, "sdk paid", task_id=task_id)
            )
            return env if env["status"] != "busy" else None

        return _wait(attempt, "a non-busy prepare")

    def release(self) -> None:
        self.handle.stop()
        self.handle = None
        self.provider = None
        self.gateway = None
        gc.collect()


def cell_sync() -> dict:
    with tempfile.TemporaryDirectory(dir=_SCRATCH) as d:
        provider_node = net_sdk.MeshNode(bind_addr="127.0.0.1:0", psk=PSK)
        caller = net_sdk.MeshNode(bind_addr="127.0.0.1:0", psk=PSK)
        _handshake(caller, provider_node)
        provider_node.start()
        caller.start()
        stage = _Stage(provider_node, caller, provider_node.node_id, d)

        env = stage.prepare("sdk-sync-1")
        assert env["status"] == "ok", env
        # Python ints hold u64 exactly: json.loads / json.dumps is the handoff.
        assert env["prepared"]["provider_node"] == provider_node.node_id
        prepared = json.dumps(env["prepared"])
        bought = json.loads(stage.gateway.purchase_task(prepared))
        assert bought["status"] == "paid", bought
        sent = json.loads(stage.gateway.submit_task(prepared))
        assert sent["status"] == "accepted", sent

        def completed():
            raw = caller.task_status(provider_node.node_id, "sdk-sync-1")
            return raw is not None and json.loads(raw)["state"]["state"] == "completed"

        _wait(completed, "the task to complete")
        assert stage.ran == ["sdk-sync-1"], stage.ran
        billing = [json.loads(e) for e in stage.provider.read_billing()]
        assert len(billing) == 1, billing

        # The MeshNode forwards reach the same native verbs.
        offers = json.loads(caller.describe_a2a(provider_node.node_id))
        assert [o["service_id"] for o in offers] == [PAID], offers
        set_a2a_org_caller(caller, None)
        set_a2a_org_caller(stage.gateway, None)
        caller.set_a2a_org_caller(None)

        stage.release()
        # The factories retained nothing beyond the native objects'
        # documented references: both nodes shut down.
        caller.shutdown()
        provider_node.shutdown()
        return {"ran": stage.ran, "billing_events": len(billing)}


def cell_async() -> dict:
    async def run() -> dict:
        with tempfile.TemporaryDirectory(dir=_SCRATCH) as d:
            provider_node = net_sdk.AsyncMeshNode(bind_addr="127.0.0.1:0", psk=PSK)
            caller = net_sdk.AsyncMeshNode(bind_addr="127.0.0.1:0", psk=PSK)
            accepted = asyncio.ensure_future(provider_node.accept(caller.node_id))
            await asyncio.sleep(0.05)
            await caller.connect(provider_node.local_addr, provider_node.public_key, provider_node.node_id)
            await accepted
            provider_node.start()
            caller.start()
            # Built over AsyncMeshNodes; the paid verbs run on the sync
            # gateway through asyncio.to_thread (sync-gateway-only).
            stage = await asyncio.to_thread(_Stage, provider_node, caller, provider_node.node_id, d)
            env = await asyncio.to_thread(stage.prepare, "sdk-async-1")
            assert env["status"] == "ok", env
            prepared = json.dumps(env["prepared"])
            bought = json.loads(await asyncio.to_thread(stage.gateway.purchase_task, prepared))
            assert bought["status"] == "paid", bought
            sent = json.loads(await asyncio.to_thread(stage.gateway.submit_task, prepared))
            assert sent["status"] == "accepted", sent
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and stage.ran != ["sdk-async-1"]:
                await asyncio.sleep(0.05)
            assert stage.ran == ["sdk-async-1"], stage.ran
            caller.set_a2a_org_caller(None)
            stage.release()
            await caller.shutdown()
            await provider_node.shutdown()
            return {"ran": stage.ran}

    return asyncio.run(run())


def cell_native() -> dict:
    import net  # a native NetMesh is what this cell tests

    with tempfile.TemporaryDirectory(dir=_SCRATCH) as d:
        mesh = net.NetMesh("127.0.0.1:0", PSK)
        mesh.start()
        provider = create_payment_provider(
            mesh, os.path.join(d, "engine.json"), unsafe_dev_mock_facilitator=True
        )
        gateway = create_capability_gateway(mesh, pin_store_path=os.path.join(d, "pins.json"))
        assert type(provider).__name__ == "PaymentProvider"
        assert type(gateway).__name__ == "CapabilityGateway"
        receipt = {"registry_version": provider.registry_version}
        provider = None
        gateway = None
        gc.collect()
        mesh.shutdown()
        return receipt


def _raises(exc_type, fn, *args, **kwargs) -> str:
    try:
        fn(*args, **kwargs)
    except exc_type as e:
        return str(e)
    raise AssertionError(f"{fn.__name__} did not raise {exc_type.__name__}")


def cell_refusals() -> dict:
    import net  # the first control is the NATIVE gateway's own refusal

    with tempfile.TemporaryDirectory(dir=_SCRATCH) as d:
        node = net_sdk.MeshNode(bind_addr="127.0.0.1:0", psk=PSK)
        node.start()
        receipt = {}
        # Native unchanged: it still refuses an SDK MeshNode.
        receipt["native_refuses_meshnode"] = _raises(TypeError, net.CapabilityGateway, node)
        # An unknown keyword is the native constructor's own error.
        receipt["unknown_kwarg"] = _raises(
            TypeError, create_capability_gateway, node, no_such_option=True
        )
        # Anything but a mesh is refused by the adapter, by name.
        receipt["not_a_mesh"] = _raises(TypeError, create_payment_provider, object(), "x.json")
        # The async gateway is a handle adapter for search/describe/invoke
        # only: paid A2A is refused, and it is not an org-slot target.
        receipt["async_refuses_paid"] = _raises(
            TypeError,
            create_async_capability_gateway,
            node,
            payment_policy_path=os.path.join(d, "p.json"),
            a2a_purchase_path=os.path.join(d, "a.json"),
        )
        agw = create_async_capability_gateway(node)
        receipt["async_not_an_org_target"] = _raises(TypeError, set_a2a_org_caller, agw, None)
        # A purchase store without a spend policy is the native refusal.
        receipt["purchase_needs_policy"] = _raises(
            ValueError, create_capability_gateway, node, a2a_purchase_path=os.path.join(d, "a.json")
        )
        agw = None
        gc.collect()
        node.shutdown()
        return receipt


def _converge(provider, caller, attempt, timeout: float = 60.0):
    """Scoped discovery ships on the announce path: announce, retry."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        for node in (provider, caller):
            try:
                node.announce_capabilities({})
            except Exception:  # noqa: BLE001
                pass
        try:
            got = attempt()
            if got is not None:
                return got
        except Exception as e:  # noqa: BLE001
            last = e
        time.sleep(0.5)
    raise AssertionError(f"never converged (last: {last!r})")


def _refused(fn) -> bool:
    try:
        fn()
    except Exception:  # noqa: BLE001
        return True
    return False


def cell_same_org(scenario_dir: str) -> dict:
    import net_sdk.org as org

    with open(os.path.join(scenario_dir, "manifest.json"), encoding="utf-8") as f:
        m = json.load(f)

    def path(rel: str) -> str:
        return os.path.join(scenario_dir, rel)

    def mesh(seed_hex: str):
        return net_sdk.MeshNode(
            bind_addr="127.0.0.1:0",
            psk=m["psk_hex"],
            identity_seed=bytes.fromhex(seed_hex),
            heartbeat_interval_ms=200,
        )

    with tempfile.TemporaryDirectory(dir=_SCRATCH) as d:
        provider_node = mesh(m["provider"]["seed_hex"])
        caller = mesh(m["caller"]["seed_hex"])
        org.install_org_authority(provider_node, path(m["provider"]["authority_dir"]))
        org.install_org_authority(caller, path(m["caller"]["authority_dir"]))
        _handshake(caller, provider_node)
        provider_node.start()
        caller.start()
        target = provider_node.node_id

        ran: list = []
        owners: list = []
        provider = create_payment_provider(
            provider_node,
            os.path.join(d, "engine.json"),
            billing_log_path=os.path.join(d, "billing.jsonl"),
            unsafe_dev_mock_facilitator=True,
        )
        terms = provider.pricing_terms(f"{target}/net.a2a.task/{PAID}", MOCK_REQS)

        async def run_task(task_id, prompt, refs, tags, *, service, revision):
            ran.append(task_id)
            return f"blob://{task_id}"

        async def preflight(owner_json, offer_json, brief_json):
            owners.append(owner_json)
            return None

        handle = provider.serve_a2a_configured(
            run_task,
            {PAID: _offer(terms)},
            os.path.join(d, "journal.json"),
            principal="same_org",
            preflight=preflight,
        )
        gateway = create_capability_gateway(
            caller,
            payment_policy_path=os.path.join(d, "spend-policy.json"),
            payment_profile="dev_test",
            a2a_purchase_path=os.path.join(d, "a2a-purchases.json"),
        )

        # Control: PROTECTED really is protected.
        assert _refused(lambda: caller.describe_a2a(target)), "an un-admitted describe got in"
        denied0 = json.loads(gateway.prepare_task(target, PAID, "x", task_id="org-denied-0"))
        assert denied0["status"] != "ok", denied0

        with open(path(m["caller"]["membership_path"]), "rb") as f:
            membership = f.read()
        with open(path(m["caller"]["dispatcher_path"]), "rb") as f:
            dispatcher = f.read()
        client = org.OrgClient.bind(caller, org.OrgCredentials(membership, dispatcher, [], []))
        caller.set_a2a_org_caller(client)  # the MeshNode's slot (raw verbs)
        set_a2a_org_caller(gateway, client)  # the gateway's slot (paid lifecycle)

        offers = _converge(provider_node, caller, lambda: caller.describe_a2a(target))
        assert [o["service_id"] for o in json.loads(offers)] == [PAID], offers

        def prepared_ok():
            env = json.loads(
                gateway.prepare_task(target, PAID, "summarize under org", task_id="org-task-1")
            )
            return env if env["status"] == "ok" else None

        env = _converge(provider_node, caller, prepared_ok)
        prepared = json.dumps(env["prepared"])
        bought = json.loads(gateway.purchase_task(prepared))
        assert bought["status"] == "paid", bought
        sent = json.loads(gateway.submit_task(prepared))
        assert sent["status"] == "accepted", sent

        def completed():
            raw = caller.task_status(target, "org-task-1")
            if raw is not None and json.loads(raw)["state"]["state"] == "completed":
                return raw
            return None

        _converge(provider_node, caller, completed)
        assert caller.cancel_task(target, "org-task-1") is False  # already terminal
        assert ran == ["org-task-1"], ran
        owner = json.loads(owners[-1])
        assert owner["kind"] == "entity", owner
        assert owner["entity"] == m["caller"]["entity_id_hex"], owner

        # Clear both slots: denied before launch.
        set_a2a_org_caller(gateway, None)
        denied1 = json.loads(gateway.prepare_task(target, PAID, "x", task_id="org-denied-1"))
        assert denied1["status"] != "ok", denied1
        caller.set_a2a_org_caller(None)
        assert _refused(lambda: caller.describe_a2a(target)), "a cleared identity still got in"
        assert ran == ["org-task-1"], ran

        handle.stop()
        client.close()
        handle = provider = gateway = client = None
        gc.collect()
        caller.shutdown()
        provider_node.shutdown()
        return {"ran": ran, "owner_kind": owner["kind"]}


CELLS = {
    "sync": cell_sync,
    "async": cell_async,
    "native": cell_native,
    "refusals": cell_refusals,
    "same_org": cell_same_org,
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cell", required=True, choices=sorted(CELLS))
    parser.add_argument("--scenario-dir", help="the same-org artifacts (same_org cell)")
    args = parser.parse_args()
    if args.cell == "same_org" and not args.scenario_dir:
        parser.error("--scenario-dir is required for the same_org cell")
    try:
        if args.cell == "same_org":
            receipt = cell_same_org(args.scenario_dir)
        else:
            receipt = CELLS[args.cell]()
    except BaseException:  # noqa: BLE001
        # A cell that fails part-way can leave a serve handle and live meshes
        # whose native threads would keep this interpreter from exiting — the
        # wrapper would then see a 300 s timeout instead of this failure.
        # Report the failure and exit hard: every resource is this process's,
        # so it is released with it, on every cell and every failure path.
        traceback.print_exc()
        sys.stdout.flush()
        sys.stderr.flush()
        shutil.rmtree(_SCRATCH, ignore_errors=True)
        os._exit(1)
    print("CELL_OK " + json.dumps(receipt, sort_keys=True))
    sys.stdout.flush()
    shutil.rmtree(_SCRATCH, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
