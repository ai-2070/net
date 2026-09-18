"""Live tests for **paid** agent-to-agent tasks over the Python binding
(`docs/internal/plans/A2A_PAID_ADMISSION_PLAN.md` WS-E).

The provider is a real ``PaymentProvider`` (one ``PaymentEngine`` behind the
quote/pay wire, the mock facilitator, a durable admission journal) serving a
catalog through ``serve_a2a_configured``. The caller is a real
``CapabilityGateway`` with a spend-policy store and a durable purchase store,
driving ``prepare_task`` -> ``purchase_task`` -> ``submit_task``.

Everything here is driven end to end over two (sometimes three) live mesh
nodes. Nothing is stubbed: the refusals come from the provider's own gate, the
journal is on disk, the purchase attempts are on disk, and the one fault this
suite injects — a provider whose payment wire is gone mid-purchase — is
injected by actually dropping the provider, not by a flag.

**No sleeps as synchronisation.** Every wait is a precondition poll on the
observable the assertion is about (a task state, a recorded billing event),
with a generous deadline and a failure message that prints what it last saw.
"""

from __future__ import annotations

import gc
import json
import os
import subprocess
import sys
import threading
import time

import pytest

_net = pytest.importorskip("net")

NetMesh = _net.__dict__.get("NetMesh")
PaymentProvider = _net.__dict__.get("PaymentProvider")
CapabilityGateway = _net.__dict__.get("CapabilityGateway")
PaymentRefused = _net.__dict__.get("PaymentRefused")
JournalOwnedElsewhere = _net.__dict__.get("JournalOwnedElsewhere")

if None in (
    NetMesh,
    PaymentProvider,
    CapabilityGateway,
    PaymentRefused,
    JournalOwnedElsewhere,
):
    pytest.skip(
        "paid A2A needs a wheel built with net+mcp+payments+publish+a2a",
        allow_module_level=True,
    )

PSK = "a7" * 32

PAID = "summarize"
FREE = "echo"
REVISION = "r1"
AMOUNT = "2500"

MOCK_REQS = [
    {
        "scheme": "mock",
        "network": "mock:net",
        "amount": AMOUNT,
        "asset": "musd",
        "payTo": "mock-provider-settle-addr",
        "maxTimeoutSeconds": 60,
    }
]

# The refusal reasons the provider's admission path can answer with. Pinned as
# a set (not one string) only where the test's claim is "refused before the
# work ran, with a machine-actionable verdict" rather than "refused for this
# exact reason"; tests that mean one reason assert that reason.
ADMISSION_REASONS = {
    "missing_quote",
    "binding_required",
    "binding_rejected",
    "no_reservation",
    "admission_revoked",
    "journal_unavailable",
    "input_binding_mismatch",
    # The engine-backed gate's own arm: a quote id the provider's engine has
    # no record of — which is what an empty or fabricated quote id is.
    "unknown_quote",
}


# ---------------------------------------------------------------------------
# Harness
# ---------------------------------------------------------------------------


def _mesh():
    return NetMesh("127.0.0.1:0", PSK, permissive_channels=True)


def _handshake(connector, acceptor):
    """Dial `acceptor` from `connector` and complete the noise handshake."""
    errs = []

    def _accept():
        try:
            acceptor.accept(connector.node_id)
        except Exception as e:  # noqa: BLE001
            errs.append(e)

    t = threading.Thread(target=_accept, daemon=True)
    t.start()
    time.sleep(0.05)
    connector.connect(acceptor.local_addr, acceptor.public_key, acceptor.node_id)
    t.join(timeout=5)
    if errs:
        raise errs[0]


def _offer(pricing_terms=None, *, max_prompt_bytes=1024, reservation_ttl_secs=600):
    return {
        "revision": REVISION,
        "pricing_terms": pricing_terms,
        "bounds": {
            "max_prompt_bytes": max_prompt_bytes,
            "max_context_refs": 8,
            "max_tags": 8,
            "max_tag_bytes": 64,
            "max_in_flight": 4,
        },
        "reservation_ttl_secs": reservation_ttl_secs,
        "reservation_retention_secs": 604800,
        "retention_secs": 3600,
        "description": f"test service (ttl={reservation_ttl_secs})",
    }


class Provider:
    """A live paid A2A provider: node + engine + journal + catalog."""

    def __init__(self, tmp_path, mesh, *, preflight=None, offers=None, name="p"):
        self.tmp_path = tmp_path
        self.name = name
        self.engine_path = str(tmp_path / f"{name}-engine.json")
        self.billing_path = str(tmp_path / f"{name}-billing.jsonl")
        self.journal_path = str(tmp_path / f"{name}-journal.json")
        self.mesh = mesh
        self.ran = []  # task ids the executor actually started
        self.finished = []
        self.preflight = preflight
        self._offers = offers
        self.provider = None
        self.handle = None
        self.serve()

    # -- lifecycle ------------------------------------------------------

    def serve(self):
        """(Re-)stand the provider up over the same engine + journal paths."""
        self.provider = PaymentProvider(
            self.mesh,
            self.engine_path,
            billing_log_path=self.billing_path,
            unsafe_dev_mock_facilitator=True,
        )
        paid_terms = self.provider.pricing_terms(
            f"{self.mesh.node_id}/net.a2a.task/{PAID}", json.dumps(MOCK_REQS)
        )
        services = self._offers or {PAID: _offer(paid_terms), FREE: _offer(None)}
        if self._offers is None:
            self.services = services
        else:
            # Late-bound terms: the caller passed offer factories.
            self.services = {
                sid: (mk(paid_terms) if callable(mk) else mk)
                for sid, mk in self._offers.items()
            }
            services = self.services
        self.handle = self.provider.serve_a2a_configured(
            self._executor(), services, self.journal_path, preflight=self.preflight
        )
        return self.handle

    def restart(self):
        """Full restart: drop the serving handle and the provider, then
        re-serve over the same engine + journal paths."""
        self.handle.stop()
        self.handle = None
        self.provider = None
        gc.collect()
        return self.serve()

    def close(self):
        if self.handle is not None:
            self.handle.stop()
        self.handle = None
        self.provider = None
        gc.collect()
        try:
            self.mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass

    # -- executor -------------------------------------------------------

    def _executor(self):
        ran, finished = self.ran, self.finished

        async def run_task(task_id, prompt, context_refs, tags, *, service, revision):
            ran.append((task_id, service, revision))
            finished.append(task_id)
            return f"blob://{service}/{task_id}"

        return run_task

    # -- observation ----------------------------------------------------

    def billing(self):
        """The provider's immutable billing events (oldest first)."""
        if self.provider is not None:
            return [json.loads(e) for e in self.provider.read_billing()]
        # The provider object is gone (payment wire dropped); read the log.
        if not os.path.exists(self.billing_path):
            return []
        with open(self.billing_path, "r", encoding="utf-8") as fh:
            return [json.loads(line) for line in fh if line.strip()]

    def unresolved(self):
        return json.loads(self.provider.a2a_unresolved())


class Caller:
    """A live paid A2A caller: node + spend policy + purchase store."""

    def __init__(self, tmp_path, mesh, provider, *, name="c", profile="dev_test"):
        self.tmp_path = tmp_path
        self.name = name
        self.policy_path = str(tmp_path / f"{name}-spend-policy.json")
        self.purchase_path = str(tmp_path / f"{name}-a2a-purchases.json")
        self.profile = profile
        self.mesh = mesh
        self.provider_node = provider.mesh.node_id
        self.gateway = self._gateway()

    def _gateway(self):
        return CapabilityGateway(
            self.mesh,
            payment_policy_path=self.policy_path,
            payment_profile=self.profile,
            a2a_purchase_path=self.purchase_path,
        )

    def restart_gateway(self):
        """Re-create the gateway over the SAME policy + purchase paths."""
        self.gateway = self._gateway()
        return self.gateway

    def close(self):
        self.gateway = None
        gc.collect()
        try:
            self.mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass

    # -- verbs ----------------------------------------------------------

    def prepare(self, prompt, *, service=PAID, task_id=None, attempts=8):
        """`prepare_task`, retrying only the `busy` status (nothing reserved,
        nothing quoted — which is also how a not-yet-routable first call
        reports itself)."""
        last = None
        for _ in range(attempts):
            last = json.loads(
                self.gateway.prepare_task(
                    self.provider_node, service, prompt, task_id=task_id
                )
            )
            if last["status"] != "busy":
                return last
            time.sleep(0.1)
        return last

    def purchase(self, prepared):
        return json.loads(self.gateway.purchase_task(json.dumps(prepared)))

    def submit(self, prepared):
        return json.loads(self.gateway.submit_task(json.dumps(prepared)))

    def attempts(self):
        return json.loads(self.gateway.a2a_attempts())


def _wait_state(mesh, node, task_id, want, timeout=8.0):
    """Poll until the executor's record reaches `want`. The precondition the
    assertion is about — never a fixed sleep."""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        raw = mesh.task_status(node, task_id)
        if raw is not None:
            rec = json.loads(raw)
            last = rec["state"]["state"]
            if last == want:
                return rec
        time.sleep(0.05)
    raise AssertionError(f"task {task_id} never reached {want!r} (last={last!r})")


def _wait_billing(provider, count, timeout=8.0):
    """Poll until the provider has recorded `count` billing events."""
    deadline = time.time() + timeout
    events = provider.billing()
    while time.time() < deadline and len(events) < count:
        time.sleep(0.05)
        events = provider.billing()
    assert len(events) == count, (
        f"expected exactly {count} billing event(s), saw {len(events)}: "
        f"{[e.get('quote_id') for e in events]}"
    )
    return events


def _topology(tmp_path, *, callers=(("c", "dev_test"),), preflight=None, offers=None):
    """Stand up one provider node and N caller nodes.

    The handshake happens while every node is UNSTARTED — the SDK cross-node
    idiom. A started node's receive loop auto-accepts and races a manual
    accept, which times the handshake out.
    """
    pmesh = _mesh()
    cmeshes = [(name, profile, _mesh()) for name, profile in callers]
    for _name, _profile, cmesh in cmeshes:
        _handshake(cmesh, pmesh)
    pmesh.start()
    for _name, _profile, cmesh in cmeshes:
        cmesh.start()
    provider = Provider(tmp_path, pmesh, preflight=preflight, offers=offers)
    built = [
        Caller(tmp_path, cmesh, provider, name=name, profile=profile)
        for name, profile, cmesh in cmeshes
    ]
    return provider, built


@pytest.fixture()
def paid(tmp_path):
    """The common topology: one paid+free provider, one dev_test caller."""
    provider, (caller,) = _topology(tmp_path)
    try:
        yield provider, caller
    finally:
        caller.close()
        provider.close()


# ---------------------------------------------------------------------------
# Serve-time invariants
# ---------------------------------------------------------------------------


def test_describe_publishes_the_catalog_and_the_free_service_runs_unpaid(paid):
    provider, caller = paid

    # Five registrations, not three: the configured path also serves
    # net.a2a.prepare and net.a2a.describe.
    assert provider.handle.serving is True
    assert provider.handle.services == 5

    offers = {o["service_id"]: o for o in json.loads(
        caller.mesh.describe_a2a(caller.provider_node)
    )}
    assert set(offers) == {PAID, FREE}
    # pricing_terms is the free/paid discriminant, and it is the only one.
    assert offers[PAID]["pricing_terms"], "the paid service announces its terms"
    assert offers[FREE].get("pricing_terms") is None
    assert offers[PAID]["bounds"]["max_in_flight"] == 4
    assert offers[PAID]["revision"] == REVISION

    # The free service needs no prepare, no quote and no proof.
    task_id = caller.mesh.submit_task(
        caller.provider_node, "echo this", service=FREE, revision=REVISION
    )
    rec = _wait_state(caller.mesh, caller.provider_node, task_id, "completed")
    assert rec["state"]["result_ref"] == f"blob://{FREE}/{task_id}"
    assert [r for r in provider.ran if r[0] == task_id] == [(task_id, FREE, REVISION)]
    # Free means free: nothing was charged for it.
    assert provider.billing() == []


def test_serve_a2a_configured_refuses_a_price_it_cannot_enforce(tmp_path):
    """A paid service is never served free, and a free one never announces a
    price. Both are refused *before* a single service is registered."""
    mesh = _mesh()
    mesh.start()
    try:
        provider = PaymentProvider(
            mesh,
            str(tmp_path / "engine.json"),
            unsafe_dev_mock_facilitator=True,
        )
        terms = provider.pricing_terms(
            f"{mesh.node_id}/net.a2a.task/{PAID}", json.dumps(MOCK_REQS)
        )

        async def cb(task_id, prompt, refs, tags, *, service, revision):
            return "blob://x"

        # An empty catalog refuses every brief — say so instead of serving it.
        with pytest.raises(ValueError):
            provider.serve_a2a_configured(cb, {}, str(tmp_path / "j1.json"))

        # A service missing a required offer term.
        broken = {PAID: {k: v for k, v in _offer(terms).items() if k != "revision"}}
        with pytest.raises(ValueError) as missing:
            provider.serve_a2a_configured(cb, broken, str(tmp_path / "j2.json"))
        assert "revision" in str(missing.value)

        # A bounds block that is not a dict of the five bounds.
        bad_bounds = {PAID: {**_offer(terms), "bounds": {"max_prompt_bytes": 10}}}
        with pytest.raises(ValueError) as bounds_err:
            provider.serve_a2a_configured(cb, bad_bounds, str(tmp_path / "j3.json"))
        assert "max_context_refs" in str(bounds_err.value)

        # An unknown principal is refused rather than quietly downgraded to
        # the weakest attribution.
        with pytest.raises(ValueError) as principal:
            provider.serve_a2a_configured(
                cb,
                {PAID: _offer(terms)},
                str(tmp_path / "j4.json"),
                principal="anyone",
            )
        assert "session_peer" in str(principal.value)

        # An announced max_prompt_bytes the wire cannot carry. Refused for
        # the same reason as an unenforceable price: the failure mode for
        # exceeding it is a request that is never delivered rather than one
        # that is refused, so an operator must not be able to publish the
        # promise at all.
        with pytest.raises(ValueError) as undeliverable:
            provider.serve_a2a_configured(
                cb,
                {PAID: {**_offer(terms, max_prompt_bytes=1 << 20)}},
                str(tmp_path / "j4b.json"),
            )
        assert "max_prompt_bytes" in str(undeliverable.value)

        # Positive control: the same catalog DOES serve.
        handle = provider.serve_a2a_configured(
            cb, {PAID: _offer(terms)}, str(tmp_path / "j5.json")
        )
        assert handle.services == 5
        handle.stop()
    finally:
        mesh.shutdown()


def test_a_second_provider_on_the_same_journal_is_refused(tmp_path, paid):
    """Two writers over one set of admission records would each believe they
    may launch the same paid work. Driven from a **separate process** — the
    in-process registry alone would not prove the on-disk lock."""
    provider, _caller = paid
    script = """
import json, sys
import net

journal, = sys.argv[1:]
mesh = net.NetMesh("127.0.0.1:0", "a7" * 32, permissive_channels=True)
mesh.start()
try:
    p = net.PaymentProvider(
        mesh, journal + ".engine.json", unsafe_dev_mock_facilitator=True
    )
    terms = p.pricing_terms(
        f"{mesh.node_id}/net.a2a.task/summarize",
        json.dumps([{ "scheme": "mock", "network": "mock:net", "amount": "2500",
                      "asset": "musd", "payTo": "mock-provider-settle-addr",
                      "maxTimeoutSeconds": 60 }]),
    )

    async def cb(task_id, prompt, refs, tags, *, service, revision):
        return "blob://x"

    offer = {
        "revision": "r1",
        "pricing_terms": terms,
        "bounds": {"max_prompt_bytes": 1024, "max_context_refs": 8, "max_tags": 8,
                   "max_tag_bytes": 64, "max_in_flight": 4},
        "reservation_ttl_secs": 600,
        "reservation_retention_secs": 604800,
        "retention_secs": 3600,
    }
    try:
        p.serve_a2a_configured(cb, {"summarize": offer}, journal)
    except net.JournalOwnedElsewhere as e:
        print("REFUSED:" + type(e).__name__)
    else:
        print("SERVED")
finally:
    mesh.shutdown()
"""
    # Control first: the live owner is this process's provider.
    assert provider.handle.serving is True
    out = subprocess.run(
        [sys.executable, "-c", script, provider.journal_path],
        capture_output=True,
        text=True,
        timeout=180,
    )
    assert out.returncode == 0, f"subprocess failed: {out.stderr}"
    assert "REFUSED:JournalOwnedElsewhere" in out.stdout, out.stdout + out.stderr

    # And the control the other way: once the owner lets go, a fresh owner
    # may take it — the refusal is about live ownership, not the file.
    provider.handle.stop()
    provider.handle = None
    provider.provider = None
    gc.collect()
    out2 = subprocess.run(
        [sys.executable, "-c", script, provider.journal_path],
        capture_output=True,
        text=True,
        timeout=180,
    )
    assert "SERVED" in out2.stdout, out2.stdout + out2.stderr


# ---------------------------------------------------------------------------
# Validation before money
# ---------------------------------------------------------------------------


def test_prepare_rejects_an_oversized_brief_before_any_quote_exists(tmp_path):
    # A deliberately tiny prompt bound, so the oversized brief is still a
    # small frame: this witness is about the provider refusing it, not about
    # anything the transport might do to a large body.
    provider, (caller,) = _topology(
        tmp_path,
        offers={
            PAID: lambda terms: _offer(terms, max_prompt_bytes=64),
            FREE: _offer(None),
        },
    )

    reply = caller.prepare("x" * 200)  # max_prompt_bytes is 64
    assert reply["status"] == "rejected", reply
    assert reply["retryable"] is False

    # The point of validating at prepare: there is nothing to reconcile,
    # because nothing was ever quoted or reserved. No purchase attempt was
    # left behind, no billing event exists, and the provider has no
    # unresolved admission.
    assert caller.attempts() == []
    assert provider.billing() == []
    assert provider.unresolved() == []

    # Positive control: a brief inside the bounds DOES prepare and quote.
    ok = caller.prepare("summarize this")
    assert ok["status"] == "ok", ok
    assert ok["quote"]["amount"] == AMOUNT
    assert ok["quote"]["network"] == "mock:net"
    assert ok["quote"]["asset"] == "musd"
    assert ok["quote"]["quote_id"]
    assert ok["quote"]["expires_at_ns"] > 0
    # Read-only on the money side: a quote is not a payment.
    assert provider.billing() == []

    caller.close()
    provider.close()


def test_prepare_is_a_complete_handle_and_moves_no_money(paid):
    provider, caller = paid
    ok = caller.prepare("summarize this", task_id="handle-1")
    assert ok["status"] == "ok", ok

    prepared = ok["prepared"]
    # Complete by design: nothing is resolved from a hash on a later call.
    assert prepared["provider_node"] == caller.provider_node
    assert prepared["brief"]["task_id"] == "handle-1"
    assert prepared["brief"]["service"] == PAID
    assert prepared["brief"]["revision"] == REVISION
    assert len(prepared["offer_hash"]) == 64
    reservation = prepared["reservation"]
    assert reservation["task_id"] == "handle-1"
    assert reservation["admission_id"]
    assert reservation["capability"] == (
        f"{caller.provider_node}/net.a2a.task/{PAID}"
    )
    assert len(reservation["purchase_hash"]) == 64
    assert provider.billing() == []


# ---------------------------------------------------------------------------
# The happy path, and once-only
# ---------------------------------------------------------------------------


def test_a_paid_task_is_purchased_once_and_runs_exactly_once(paid):
    provider, caller = paid

    prep = caller.prepare("summarize the ledger", task_id="once-1")
    assert prep["status"] == "ok", prep
    prepared = prep["prepared"]

    bought = caller.purchase(prepared)
    assert bought["status"] == "paid", bought
    assert bought["task_id"] == "once-1"
    assert bought["quote_id"] == prep["quote"]["quote_id"]
    proof = bought["proof"]
    assert proof["quote_id"] == bought["quote_id"]
    assert proof["binding_sig"], "a task admission always carries the binding"

    sent = caller.submit(prepared)
    assert sent["status"] == "accepted", sent
    assert sent["task_id"] == "once-1"

    rec = _wait_state(caller.mesh, caller.provider_node, "once-1", "completed")
    assert rec["state"]["result_ref"] == f"blob://{PAID}/once-1"
    assert provider.ran == [("once-1", PAID, REVISION)]
    events = _wait_billing(provider, 1)
    assert events[0]["quote_id"] == bought["quote_id"]

    # Re-purchasing returns the SAME stored proof and charges nothing more.
    again = caller.purchase(prepared)
    assert again["status"] in {"paid", "failed"}, again
    if again["status"] == "paid":
        assert again["proof"] == proof
    assert len(caller.attempts()) == 1
    assert len(provider.billing()) == 1

    # Re-submitting the same proof is safe: one payment, one launch, ever.
    resent = caller.submit(prepared)
    assert resent["status"] in {"accepted", "retry"}, resent
    assert provider.ran == [("once-1", PAID, REVISION)], (
        "a re-submitted proof must not launch the work a second time"
    )
    assert len(provider.billing()) == 1


def test_a_retained_id_retry_converges_on_one_admission_and_one_quote(paid):
    _provider, caller = paid

    first = caller.prepare("summarize once", task_id="retain-1")
    assert first["status"] == "ok", first
    second = caller.prepare("summarize once", task_id="retain-1")
    assert second["status"] == "ok", second

    # Same reservation (the provider's prepare is idempotent) and the quote
    # channel was called once, not twice.
    assert (
        second["prepared"]["reservation"]["admission_id"]
        == first["prepared"]["reservation"]["admission_id"]
    )
    assert second["quote"]["quote_id"] == first["quote"]["quote_id"]
    assert [a["key"]["task_id"] for a in caller.attempts()] == ["retain-1"]


def test_an_altered_brief_under_a_retained_id_is_refused(paid):
    _provider, caller = paid

    first = caller.prepare("summarize the original", task_id="alter-1")
    assert first["status"] == "ok", first

    altered = json.loads(
        caller.gateway.prepare_task(
            caller.provider_node, PAID, "summarize something else", task_id="alter-1"
        )
    )
    assert altered["status"] in {"rejected", "conflict"}, altered

    # The original attempt is untouched — a different brief under one id is
    # never a silent re-reservation.
    attempts = caller.attempts()
    assert len(attempts) == 1
    assert attempts[0]["quote_id"] == first["quote"]["quote_id"]
    assert attempts[0]["prepared"]["brief"]["prompt"] == "summarize the original"


def test_a_lapsed_reservation_keeps_its_admission_id_and_stays_purchasable(tmp_path):
    """A reservation whose TTL has lapsed keeps its admission id and is still
    purchasable and runnable — capacity lapsing is not the admission lapsing.

    **What this row does NOT prove, and no longer claims to.** It was named
    "...is_re_acquired..." and asserted ``expires_at >= expires_first``. The
    reviewer is right that neither is reacquisition evidence: ``>=`` permits
    equality, and nothing here observes the *provider* renewing anything — the
    second ``prepare`` could be answered from the caller's own stored attempt
    and this test would still pass.

    Provider-observed reacquisition is covered by the Rust witnesses, which can
    see the store directly:
    ``a2a_paid_admission::prepare_reports_busy_at_max_in_flight_and_frees_on_expiry``
    and ``a2a_admission_identity::a_decision_holds_its_capacity_slot_across_an_expiring_reservation``.
    Claiming it from Python would need a provider-side prepare observation the
    binding does not expose, and inventing one for a test is not a trade worth
    making. Named as a gap instead of dressed up.
    """
    provider, (caller,) = _topology(
        tmp_path,
        offers={
            PAID: lambda terms: _offer(terms, reservation_ttl_secs=0),
            FREE: _offer(None),
        },
    )
    try:
        first = caller.prepare("summarize", task_id="lapse-1")
        assert first["status"] == "ok", first

        # The TTL is zero, so this reservation holds no capacity from the
        # moment it exists. Re-preparing must converge on the same admission
        # rather than mint a second one a paid quote would not match.
        second = caller.prepare("summarize", task_id="lapse-1")
        assert second["status"] == "ok", second
        assert (
            second["prepared"]["reservation"]["admission_id"]
            == first["prepared"]["reservation"]["admission_id"]
        ), "a lapsed reservation converges on its admission, never re-mints one"

        # And it is still purchasable + runnable: capacity lapsing is not the
        # admission lapsing.
        bought = caller.purchase(second["prepared"])
        assert bought["status"] == "paid", bought
        assert caller.submit(second["prepared"])["status"] == "accepted"
        _wait_state(caller.mesh, caller.provider_node, "lapse-1", "completed")
        assert provider.ran == [("lapse-1", PAID, REVISION)]
    finally:
        caller.close()
        provider.close()


# ---------------------------------------------------------------------------
# Refusals
# ---------------------------------------------------------------------------


def test_an_unpaid_submission_is_refused_with_a_schematic_and_never_runs(paid):
    """The refusal a caller must be able to act on: the provider answers the
    payment application error, the machine-actionable schematic crosses the
    language boundary intact, and the work does not run."""
    provider, caller = paid

    prep = caller.prepare("summarize unpaid", task_id="unpaid-1")
    assert prep["status"] == "ok", prep
    prepared_json = json.dumps(prep["prepared"])

    # A proof with no quote and no binding: the caller never paid.
    with pytest.raises(PaymentRefused) as refusal:
        caller.mesh.submit_task_paid(
            prepared_json, json.dumps({"quote_id": "", "binding_sig": []})
        )

    err = refusal.value
    assert len(err.args) == 2, err.args
    message, schematic_json = err.args
    assert message, "the human refusal travels too"
    assert schematic_json is not None, (
        "the provider's failure schematic must survive the boundary"
    )
    assert err.schematic == schematic_json
    schematic = json.loads(schematic_json)
    assert schematic["object"] == "net.payment.failure@1"
    assert schematic["reason"] in ADMISSION_REASONS, schematic
    assert schematic["stage"] in {"admission", "redeem"}, schematic
    assert schematic["handler_executed"] is False
    # `funds_moved` is tri-state ("no" / "maybe" / "yes"), not a bool: an
    # unpaid submission is the unambiguous "no".
    assert schematic["funds_moved"] == "no", schematic
    assert "recovery" in schematic

    # The claim that matters: nothing ran and nothing was charged.
    assert provider.ran == []
    assert provider.billing() == []

    # Positive control — the reservation survived the refusal (a denial is an
    # attempt note, not a state change), so paying properly still runs it.
    bought = caller.purchase(prep["prepared"])
    assert bought["status"] == "paid", bought
    assert caller.submit(prep["prepared"])["status"] == "accepted"
    _wait_state(caller.mesh, caller.provider_node, "unpaid-1", "completed")
    assert provider.ran == [("unpaid-1", PAID, REVISION)]
    _wait_billing(provider, 1)


def test_a_gate_denial_leaves_the_reservation_and_a_valid_retry_runs(paid):
    """A fabricated quote is refused by the gate. The reservation is *not*
    consumed: the caller fixes its payment and retries the same admission
    instead of preparing (and paying for) a second one."""
    provider, caller = paid

    prep = caller.prepare("summarize denied", task_id="denied-1")
    assert prep["status"] == "ok", prep
    prepared_json = json.dumps(prep["prepared"])
    admission_id = prep["prepared"]["reservation"]["admission_id"]

    with pytest.raises(PaymentRefused) as refusal:
        caller.mesh.submit_task_paid(
            prepared_json,
            json.dumps({"quote_id": "deadbeef" * 8, "binding_sig": list(b"\x00" * 64)}),
        )
    schematic = json.loads(refusal.value.args[1])
    assert schematic["reason"] in ADMISSION_REASONS, schematic
    assert schematic["handler_executed"] is False
    assert provider.ran == []

    # The same admission is still there, under the same id.
    again = caller.prepare("summarize denied", task_id="denied-1")
    assert again["status"] == "ok", again
    assert again["prepared"]["reservation"]["admission_id"] == admission_id

    bought = caller.purchase(again["prepared"])
    assert bought["status"] == "paid", bought
    assert caller.submit(again["prepared"])["status"] == "accepted"
    _wait_state(caller.mesh, caller.provider_node, "denied-1", "completed")
    assert provider.ran == [("denied-1", PAID, REVISION)]
    _wait_billing(provider, 1)


def test_a_proof_bought_by_one_caller_is_worthless_from_another(tmp_path):
    """Cross-peer proof reuse. One caller prepares and pays; a *different*
    peer presents the identical prepared document and proof. The admission is
    keyed to the owner that reserved it, so the second peer buys nothing."""
    provider, (buyer, thief) = _topology(
        tmp_path, callers=(("buyer", "dev_test"), ("thief", "dev_test"))
    )
    try:
        prep = buyer.prepare("summarize mine", task_id="cross-1")
        assert prep["status"] == "ok", prep
        bought = buyer.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought

        prepared_json = json.dumps(prep["prepared"])
        proof_json = json.dumps(bought["proof"])

        # The thief has the complete documents — and they are worth nothing.
        with pytest.raises(PaymentRefused) as refusal:
            thief.mesh.submit_task_paid(prepared_json, proof_json)
        schematic = json.loads(refusal.value.args[1])
        assert schematic["reason"] in ADMISSION_REASONS, schematic
        assert schematic["handler_executed"] is False
        assert provider.ran == [], "a replayed proof must launch nothing"

        # Positive control: the buyer's own submission of the same documents
        # is accepted, so the refusal is about *who* presented it.
        assert buyer.mesh.submit_task_paid(prepared_json, proof_json) == "cross-1"
        _wait_state(buyer.mesh, buyer.provider_node, "cross-1", "completed")
        assert provider.ran == [("cross-1", PAID, REVISION)]
        _wait_billing(provider, 1)
    finally:
        thief.close()
        buyer.close()
        provider.close()


# ---------------------------------------------------------------------------
# Approval, denial, and the operator exits
# ---------------------------------------------------------------------------


def test_production_profile_holds_the_purchase_until_the_operator_approves(tmp_path):
    provider, (caller,) = _topology(tmp_path, callers=(("c", "production"),))
    try:
        prep = caller.prepare("summarize held", task_id="hold-1")
        assert prep["status"] == "ok", prep

        held = caller.purchase(prep["prepared"])
        assert held["status"] == "requires_payment_approval", held
        assert held["quote_id"] == prep["quote"]["quote_id"]
        assert held["policy_reason"]
        assert held["approve_hint"]
        # Held means held: nothing was charged and nothing ran.
        assert provider.billing() == []
        assert provider.ran == []
        pending = json.loads(caller.gateway.pending_payments())
        assert pending["status"] == "ok"
        assert held["quote_id"] in pending["pending"]

        # Re-purchasing while held reports the same hold, never a second quote.
        again = caller.purchase(prep["prepared"])
        assert again["status"] == "requires_payment_approval", again
        assert again["quote_id"] == held["quote_id"]

        # The operator grants it; the next purchase redeems that exact quote.
        approved = json.loads(caller.gateway.approve_payment(held["quote_id"]))
        assert approved["status"] == "ok", approved
        bought = caller.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought
        assert bought["quote_id"] == held["quote_id"]
        assert caller.submit(prep["prepared"])["status"] == "accepted"
        _wait_state(caller.mesh, caller.provider_node, "hold-1", "completed")
        _wait_billing(provider, 1)
    finally:
        caller.close()
        provider.close()


def test_a_rejected_approval_denies_unambiguously_and_re_opens_the_key(tmp_path):
    """`denied` with `funds_ambiguous: False` is proven non-settlement: no
    authorization ever left the process, so a fresh `prepare_task` is allowed
    and re-runs policy."""
    provider, (caller,) = _topology(tmp_path, callers=(("c", "production"),))
    try:
        prep = caller.prepare("summarize rejected", task_id="reject-1")
        held = caller.purchase(prep["prepared"])
        assert held["status"] == "requires_payment_approval", held

        rejected = json.loads(caller.gateway.reject_payment(held["quote_id"]))
        assert rejected["status"] == "ok", rejected

        denied = caller.purchase(prep["prepared"])
        assert denied["status"] == "denied", denied
        assert denied["funds_ambiguous"] is False, (
            "a refusal before any authorization left this process is not ambiguous"
        )
        assert provider.billing() == []

        # Proven-unexposed, so the key re-opens: a new prepare mints a NEW
        # quote under the SAME admission id, and policy runs again.
        fresh = caller.prepare("summarize rejected", task_id="reject-1")
        assert fresh["status"] == "ok", fresh
        assert (
            fresh["prepared"]["reservation"]["admission_id"]
            == prep["prepared"]["reservation"]["admission_id"]
        )
        assert fresh["quote"]["quote_id"] != held["quote_id"], (
            "a new approval decision needs a new quote, not the stale hold"
        )
        held_again = caller.purchase(fresh["prepared"])
        assert held_again["status"] == "requires_payment_approval", held_again
        assert held_again["quote_id"] == fresh["quote"]["quote_id"]
    finally:
        caller.close()
        provider.close()


def test_a_post_payment_revocation_is_unresolved_on_both_sides_until_resolved(
    tmp_path,
):
    """The unresolved-financial class, end to end. The application preflight
    admits at prepare and refuses at submit — money moved, the work will never
    run, and *neither* side may close the record automatically.

    Both operator exits are the only way out: `PaymentProvider.a2a_resolve`
    on the provider's admission, `CapabilityGateway.a2a_resolve_attempt` on
    the caller's purchase.
    """
    calls = []

    async def preflight(owner_json, offer_json, brief_json):
        owner = json.loads(owner_json)
        brief = json.loads(brief_json)
        calls.append((owner, brief["task_id"]))
        # Admit the prepare; refuse everything after it.
        if len(calls) == 1:
            return None
        return "provider authority for this brief was revoked"

    provider, (caller,) = _topology(tmp_path, preflight=preflight)
    try:
        prep = caller.prepare("summarize revoked", task_id="revoke-1")
        assert prep["status"] == "ok", prep
        assert len(calls) == 1, "the preflight ran at prepare"
        assert calls[0][0]["kind"] == "peer"
        assert calls[0][0]["node"] == caller.mesh.node_id

        bought = caller.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought
        _wait_billing(provider, 1)

        sent = caller.submit(prep["prepared"])
        assert sent["status"] == "unexecutable", sent
        assert "revoked" in sent["message"], sent
        assert provider.ran == [], "revoked work must never run"

        # Provider side: a Reconcile record in the unresolved class, and the
        # status a requester reads for it.
        unresolved = provider.unresolved()
        assert len(unresolved) == 1, unresolved
        record = unresolved[0]
        assert record["task_id"] == "revoke-1"
        assert record["state"]["admission"] == "reconcile"
        assert record["paid"] is True
        assert record["owner"]["kind"] == "peer"
        status = json.loads(
            caller.mesh.task_status(caller.provider_node, "revoke-1")
        )
        assert status["state"] == {
            "state": "interrupted",
            "detail": "admission_revoked",
        }, status

        # Caller side: the purchase is PaidUnexecutable with its evidence.
        attempts = caller.attempts()
        assert len(attempts) == 1, attempts
        assert attempts[0]["state"]["state"] == "paid_unexecutable"
        assert attempts[0]["state"]["proof"]["quote_id"] == bought["quote_id"]

        # A repeat purchase must not buy a second one out of this state.
        assert caller.purchase(prep["prepared"])["status"] == "failed"
        assert len(provider.billing()) == 1

        # Operator exit, provider side.
        provider.provider.a2a_resolve(
            json.dumps(record["owner"]),
            "revoke-1",
            json.dumps({"state": "failed", "error": "refunded out of band"}),
        )
        assert provider.unresolved() == []

        # Operator exit, caller side.
        caller.gateway.a2a_resolve_attempt(
            "revoke-1",
            json.dumps(
                {
                    "resolution": "closed",
                    "outcome": "refunded",
                    "evidence": {"ticket": "OPS-1"},
                }
            ),
        )
        closed = caller.attempts()
        assert len(closed) == 1
        assert closed[0]["state"]["state"] == "resolved"
        assert closed[0]["state"]["outcome"] == "refunded"
        assert closed[0]["state"]["evidence"] == {"ticket": "OPS-1"}
    finally:
        caller.close()
        provider.close()


def test_resolve_attempt_rejects_an_unparseable_or_unknown_outcome(paid):
    _provider, caller = paid
    prep = caller.prepare("summarize resolvable", task_id="resolve-1")
    assert prep["status"] == "ok", prep

    with pytest.raises(ValueError) as no_tag:
        caller.gateway.a2a_resolve_attempt("resolve-1", json.dumps({"outcome": "x"}))
    assert "resolution" in str(no_tag.value)

    with pytest.raises(ValueError):
        caller.gateway.a2a_resolve_attempt("resolve-1", "not json")

    with pytest.raises(ValueError) as unknown:
        caller.gateway.a2a_resolve_attempt(
            "no-such-task", json.dumps({"resolution": "not_paid", "reason": "x"})
        )
    assert "no purchase attempt" in str(unknown.value)


# ---------------------------------------------------------------------------
# Recovery
# ---------------------------------------------------------------------------


def test_a_full_provider_restart_reconciles_the_original_payment(tmp_path):
    """The provider is restarted — serve handle, journal ownership and engine
    all re-created over the same paths — between the payment and the submit.
    The paid admission survives on disk: the submit is accepted, the work runs
    once, and no second charge appears."""
    provider, (caller,) = _topology(tmp_path)
    try:
        prep = caller.prepare("summarize across a restart", task_id="restart-1")
        assert prep["status"] == "ok", prep
        bought = caller.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought
        _wait_billing(provider, 1)

        provider.restart()

        sent = caller.submit(prep["prepared"])
        assert sent["status"] == "accepted", sent
        _wait_state(caller.mesh, caller.provider_node, "restart-1", "completed")
        assert provider.ran == [("restart-1", PAID, REVISION)]
        assert len(provider.billing()) == 1
        assert provider.unresolved() == []
    finally:
        caller.close()
        provider.close()


def test_a_re_created_gateway_resumes_the_purchase_from_the_store(tmp_path):
    """Caller restart. A brand-new `CapabilityGateway` over the same spend
    policy + purchase store finds the paid attempt and submits it — the
    purchase record, not process memory, is what makes a payment resumable."""
    provider, (caller,) = _topology(tmp_path)
    try:
        prep = caller.prepare("summarize after a caller restart", task_id="caller-1")
        assert prep["status"] == "ok", prep
        bought = caller.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought
        _wait_billing(provider, 1)

        # Everything in memory goes away; only the files remain.
        caller.restart_gateway()

        resumed = caller.attempts()
        assert len(resumed) == 1, resumed
        assert resumed[0]["state"]["state"] == "paid"
        assert resumed[0]["state"]["proof"]["quote_id"] == bought["quote_id"]

        # The new gateway re-reads the stored proof rather than re-buying.
        repurchased = caller.purchase(prep["prepared"])
        assert repurchased["status"] == "paid", repurchased
        assert repurchased["proof"] == bought["proof"]
        assert len(provider.billing()) == 1

        assert caller.submit(prep["prepared"])["status"] == "accepted"
        _wait_state(caller.mesh, caller.provider_node, "caller-1", "completed")
        assert provider.ran == [("caller-1", PAID, REVISION)]
        assert len(provider.billing()) == 1
    finally:
        caller.close()
        provider.close()


# ---------------------------------------------------------------------------
# Surface guards
# ---------------------------------------------------------------------------


def test_the_paid_verbs_refuse_a_gateway_without_a_purchase_store(tmp_path):
    mesh = _mesh()
    mesh.start()
    try:
        # No payment policy at all.
        bare = CapabilityGateway(mesh)
        with pytest.raises(ValueError) as no_policy:
            bare.prepare_task(1, PAID, "hi")
        assert "payment_policy_path" in str(no_policy.value)

        # A payment policy but nowhere durable to record a purchase.
        no_store = CapabilityGateway(
            mesh,
            payment_policy_path=str(tmp_path / "policy.json"),
            payment_profile="dev_test",
        )
        with pytest.raises(ValueError) as missing_store:
            no_store.purchase_task("{}")
        assert "a2a_purchase_path" in str(missing_store.value)

        # A purchase store with no spend policy would record payments nothing
        # authorized — refused at construction.
        with pytest.raises(ValueError) as orphan:
            CapabilityGateway(mesh, a2a_purchase_path=str(tmp_path / "p.json"))
        assert "payment_policy_path" in str(orphan.value)
    finally:
        mesh.shutdown()


def test_submit_task_kwargs_are_validated_at_the_boundary(tmp_path):
    mesh = _mesh()
    mesh.start()
    try:
        # service and revision are both-or-neither: one without the other
        # could never match a catalog offer.
        with pytest.raises(ValueError) as half:
            mesh.submit_task(1, "hi", service=PAID)
        assert "revision" in str(half.value)
        with pytest.raises(ValueError):
            mesh.submit_task(1, "hi", revision=REVISION)
        # An empty retained id is a caller mistake, not a random id.
        with pytest.raises(ValueError):
            mesh.submit_task(1, "hi", task_id="")
        # Malformed handles are caller-shape errors, not purchase outcomes.
        with pytest.raises(ValueError):
            mesh.submit_task_paid("not json", "{}")
        with pytest.raises(ValueError):
            mesh.submit_task_paid(
                json.dumps({"provider_node": 1}), json.dumps({"quote_id": "q"})
            )
    finally:
        mesh.shutdown()


# ---------------------------------------------------------------------------
# Operator recovery: the key is (caller, provider node, task id) — R12
# ---------------------------------------------------------------------------


def test_one_task_id_on_two_providers_is_resolvable_by_naming_the_provider(tmp_path):
    """A purchase key is ``(caller, provider_node, task_id)``. One retained id
    on two providers is two purchases, and the operator must be able to close
    exactly one of them.

    Before the repair the id-only verb refused the collision and pointed at
    "the provider-scoped records" — an API that was not exposed — so a
    two-provider collision was a dead end with real money on both sides.
    """
    cmesh = _mesh()
    pmesh_a, pmesh_b = _mesh(), _mesh()
    _handshake(cmesh, pmesh_a)
    _handshake(cmesh, pmesh_b)
    pmesh_a.start()
    pmesh_b.start()
    cmesh.start()
    prov_a = Provider(tmp_path, pmesh_a, name="pa")
    prov_b = Provider(tmp_path, pmesh_b, name="pb")
    caller = Caller(tmp_path, cmesh, prov_a, name="col")
    try:
        # The SAME task id, prepared and paid on both providers.
        for prov in (prov_a, prov_b):
            caller.provider_node = prov.mesh.node_id
            prep = caller.prepare("summarize both", task_id="shared-1")
            assert prep["status"] == "ok", prep
            paid = caller.purchase(prep["prepared"])
            assert paid["status"] == "paid", paid

        nodes = sorted(a["key"]["provider_node"] for a in caller.attempts())
        assert nodes == sorted([pmesh_a.node_id, pmesh_b.node_id]), nodes

        closed = json.dumps(
            {"resolution": "closed", "outcome": "written_off", "evidence": {}}
        )

        # id alone is ambiguous — and the refusal now hands the operator the
        # exact provider node ids, every one of them a valid argument below.
        with pytest.raises(ValueError) as ambiguous:
            caller.gateway.a2a_resolve_attempt("shared-1", closed)
        message = str(ambiguous.value)
        assert "provider_node" in message, message
        for node in nodes:
            assert str(node) in message, message

        # Naming the provider completes the key, and the verb reaches THAT
        # purchase: a settled `paid` attempt is not the operator's to close,
        # and the refusal names the exact store key it resolved to — which
        # is the observable that distinguishes "selected the right record"
        # from "selected any record".
        for named, other in ((pmesh_a, pmesh_b), (pmesh_b, pmesh_a)):
            with pytest.raises(RuntimeError) as reached:
                caller.gateway.a2a_resolve_attempt(
                    "shared-1", closed, provider_node=named.node_id
                )
            key = str(reached.value)
            assert f"/{named.node_id}/shared-1" in key, key
            assert f"/{other.node_id}/" not in key, key
            assert "is `paid`" in key, key

        # Both purchases are still on file and still `paid`: naming one
        # provider changed nothing about the other, and nothing about
        # either.
        states = {
            a["key"]["provider_node"]: a["state"]["state"] for a in caller.attempts()
        }
        assert states == {pmesh_a.node_id: "paid", pmesh_b.node_id: "paid"}, states
    finally:
        caller.close()
        prov_a.close()
        prov_b.close()


def test_two_caller_identities_sharing_one_store_see_only_their_own(tmp_path):
    """Two gateways over ONE purchase file are two paying entities. The
    operator queue must show each only its own rows.

    ``A2aCallerFlow::attempts`` returns every record in the file, so without
    the caller-identity filter the queue invited an operator to resolve an
    attempt this gateway could not possibly have paid for.
    """
    provider, (alice, bob) = _topology(
        tmp_path, callers=(("alice", "dev_test"), ("bob", "dev_test"))
    )
    try:
        # Bob's gateway is rebuilt over ALICE's purchase file: one store,
        # two caller identities.
        bob.purchase_path = alice.purchase_path
        bob.restart_gateway()

        prep = alice.prepare("summarize mine", task_id="alice-1")
        assert prep["status"] == "ok", prep
        assert alice.purchase(prep["prepared"])["status"] == "paid"

        mine = alice.attempts()
        assert [a["key"]["task_id"] for a in mine] == ["alice-1"], mine
        alice_caller = mine[0]["key"]["caller_hex"]

        assert bob.attempts() == [], (
            "bob's queue must not list alice's purchase: " f"{bob.attempts()}"
        )
        with pytest.raises(ValueError) as refused:
            bob.gateway.a2a_resolve_attempt(
                "alice-1",
                json.dumps({"resolution": "not_paid", "reason": "not mine"}),
            )
        assert "no purchase attempt" in str(refused.value), str(refused.value)

        # Control: bob's OWN purchase, into the same file, is his to see and
        # to resolve — so the filter is about identity, not about the store
        # being unreadable.
        prep = bob.prepare("summarize bob", task_id="bob-1")
        assert prep["status"] == "ok", prep
        assert bob.purchase(prep["prepared"])["status"] == "paid"
        his = bob.attempts()
        assert [a["key"]["task_id"] for a in his] == ["bob-1"], his
        assert his[0]["key"]["caller_hex"] != alice_caller
        # His id resolves to HIS key — a settled `paid` attempt is not the
        # operator's to reopen, and the refusal names the key it reached.
        with pytest.raises(RuntimeError) as reached:
            bob.gateway.a2a_resolve_attempt(
                "bob-1", json.dumps({"resolution": "not_paid", "reason": "mine"})
            )
        assert his[0]["key"]["caller_hex"] in str(reached.value), str(reached.value)
        assert alice_caller not in str(reached.value), str(reached.value)

        # Alice's row is untouched, and still reachable through her own
        # gateway: a valid store key stays valid.
        assert [a["key"]["task_id"] for a in alice.attempts()] == ["alice-1"]
        with pytest.raises(RuntimeError) as hers:
            alice.gateway.a2a_resolve_attempt(
                "alice-1", json.dumps({"resolution": "not_paid", "reason": "mine"})
            )
        assert alice_caller in str(hers.value), str(hers.value)
    finally:
        bob.close()
        alice.close()
        provider.close()


def test_an_admission_is_resolvable_by_its_exact_generation(tmp_path):
    """The provider's operator queue is addressable **by incarnation**.

    Every row `a2a_unresolved()` reports carries the `generation` that
    identifies it, and handing that back closes exactly that row. A
    generation nothing on the queue carries is refused before any write,
    naming the field to read — so an operator who mistypes one does not
    close a charge they did not mean.

    The ambiguous case itself (a retained charge beside its live
    replacement at one key) needs a payment in flight across a
    replacement, which no sequence of these Python verbs can produce; it
    is witnessed in Rust. What is executed here is the exit the operator
    reaches for once they are looking at such a queue.
    """
    calls = []

    async def preflight(owner_json, offer_json, brief_json):
        calls.append(json.loads(brief_json)["task_id"])
        return None if len(calls) == 1 else "authority revoked before execution"

    provider, (caller,) = _topology(tmp_path, preflight=preflight)
    try:
        prep = caller.prepare("summarize by generation", task_id="gen-1")
        assert prep["status"] == "ok", prep
        assert caller.purchase(prep["prepared"])["status"] == "paid"
        assert caller.submit(prep["prepared"])["status"] == "unexecutable"

        queue = provider.unresolved()
        assert len(queue) == 1, queue
        row = queue[0]
        assert row["task_id"] == "gen-1"
        generation = row["generation"]
        assert isinstance(generation, int) and generation > 0, row

        # A generation nothing carries: refused, and the queue is intact.
        with pytest.raises(ValueError) as wrong:
            provider.provider.a2a_resolve(
                json.dumps(row["owner"]),
                "gen-1",
                json.dumps({"state": "failed", "error": "refunded"}),
                generation=generation + 41,
            )
        assert "generation" in str(wrong.value), str(wrong.value)
        assert provider.unresolved() == queue, "a refused resolution wrote nothing"

        # The row's own generation closes exactly that row.
        provider.provider.a2a_resolve(
            json.dumps(row["owner"]),
            "gen-1",
            json.dumps({"state": "failed", "error": "refunded out of band"}),
            generation=generation,
        )
        assert provider.unresolved() == []
        status = json.loads(caller.mesh.task_status(caller.provider_node, "gen-1"))
        assert status["state"] == {
            "state": "failed",
            "error": "refunded out of band",
        }, status
    finally:
        caller.close()
        provider.close()


def test_a_generation_scoped_resolution_refuses_an_unknown_incarnation(tmp_path):
    """A generation-scoped resolution validates its selector and refuses an
    incarnation no archive holds, leaving the live purchase byte-identical.

    **Renamed, because the old name was a claim this test cannot support.**
    It was `..._never_reaches_the_live_attempt`, and the reviewer showed that
    claim is false-green: there is no retained row here at all, and `Closed`
    is illegal against the live `Paid` row, so routing the call to the WRONG
    resolver (`resolve_attempt` instead of `resolve_superseded_attempt`) also
    raises and also leaves state unchanged. Both the correct and the broken
    binding pass this test, so it never discriminated on routing — it only
    ever proved selector validation.

    The routing property it used to claim is now carried by
    `test_a2a_history_boundary.py`, which populates BOTH a historical and a
    live incarnation under one key and drives the live one to
    `paid_unexecutable` so it is genuinely eligible for the same `Closed`
    operation. That test fails under the wrong-route mutation; this one does
    not, which is precisely why both exist and why this one is named for the
    narrower property.
    """
    provider, (caller,) = _topology(tmp_path)
    try:
        prep = caller.prepare("summarize retained-routing", task_id="route-1")
        assert prep["status"] == "ok", prep
        assert caller.purchase(prep["prepared"])["status"] == "paid"

        before = caller.attempts()
        assert len(before) == 1, before
        assert before[0]["retained"] is False, before
        assert before[0]["state"]["state"] == "paid", before

        closed = json.dumps(
            {"resolution": "closed", "outcome": "written_off", "evidence": {}}
        )
        # Malformed: refused at the boundary, naming the shape to pass.
        with pytest.raises(ValueError) as shape:
            caller.gateway.a2a_resolve_attempt("route-1", closed, generation="7")
        assert "incarnation" in str(shape.value), str(shape.value)

        # Well-formed, names no retained incarnation: the archive exit
        # refuses and the live purchase is untouched.
        unknown = json.dumps({"seq": 99, "incarnation": "ff" * 16})
        with pytest.raises(RuntimeError) as missing:
            caller.gateway.a2a_resolve_attempt(
                "route-1", closed, generation=unknown
            )
        assert "superseded" in str(missing.value).lower(), str(missing.value)
        assert caller.attempts() == before, "the live charge is exactly as it was"
    finally:
        caller.close()
        provider.close()
