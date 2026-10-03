"""Cross-binding shape fixture for the paid-A2A JSON documents
(`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md` WS-F, captured before
WS-A).

Every document a binding hands a paid-A2A caller or operator — the
``{status: ...}`` envelopes of prepare / purchase / submit, the caller's
attempt rows and the provider's unresolved rows — is pinned here as a
**shape**: every key, every list length, every boolean and every stable
vocabulary value literally, and every per-run value (ids, hashes,
signatures, timestamps, node ids, free-text messages) masked to its type.

The fixture, ``tests/cross_lang_a2a_paid/envelopes.json``, was captured from
the Python binding **before** the binding-neutral projection moved into
``net_payments::flow::a2a::json``. This suite asserts Python still produces
it; the Node suite asserts the same file. A drift in either binding — or in
the shared projection — fails here.

Masking is by type, never by parsing a number: an int becomes ``"<int>"``,
so a u64 above 2^53 is pinned by position without any reader having to hold
its value (JavaScript readers cannot, see the plan's D2a).

Re-capture deliberately with ``NET_A2A_CAPTURE=1`` — and say why in the
commit, because a changed fixture is a changed cross-binding contract.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest

from test_a2a_paid import PAID, _topology, _wait_billing, _wait_state

FIXTURE = (
    Path(__file__).resolve().parents[3] / "tests" / "cross_lang_a2a_paid" / "envelopes.json"
)

# String values pinned literally: closed vocabularies, and the values this
# scenario chooses itself (service, revision, task ids). Everything else that
# is a string is per-run (ids, hashes, signatures, messages) and is masked.
STABLE = {
    "status",
    "state",
    "admission",
    "kind",
    "network",
    "asset",
    "amount",
    "scheme",
    "outcome",
    "resolution",
    "reason",
    "service",
    "service_id",
    "revision",
    "task_id",
}


def shape(value, key=None):
    if isinstance(value, dict):
        return {k: shape(v, k) for k, v in sorted(value.items())}
    if isinstance(value, list):
        return [shape(v, key) for v in value]
    if isinstance(value, bool) or value is None:
        return value
    if isinstance(value, int):
        return "<int>"
    if isinstance(value, float):
        return "<float>"
    if isinstance(value, str):
        return value if key in STABLE else "<str>"
    return f"<{type(value).__name__}>"


def _capture(tmp_path):
    docs = {}

    # --- the happy path --------------------------------------------------
    provider, (caller,) = _topology(tmp_path / "happy")
    try:
        prep = caller.prepare("summarize the cross-lang fixture", task_id="xl-ok")
        assert prep["status"] == "ok", prep
        docs["prepare_ok"] = prep
        docs["prepare_unknown_service"] = caller.prepare(
            "summarize", service="no-such-service", task_id="xl-none"
        )
        bought = caller.purchase(prep["prepared"])
        assert bought["status"] == "paid", bought
        docs["purchase_paid"] = bought
        _wait_billing(provider, 1)
        sent = caller.submit(prep["prepared"])
        assert sent["status"] == "accepted", sent
        docs["submit_accepted"] = sent
        docs["attempts_after_submit"] = caller.attempts()
        # A launched task is in the provider's unresolved class until its
        # terminal row is written — and that write is the terminal hook's,
        # which lands *after* the registry already reports `completed`. So
        # poll the queue itself (the observable this document is about)
        # rather than the task state, or the document races the hook.
        _wait_state(caller.mesh, caller.provider_node, "xl-ok", "completed")
        deadline = time.monotonic() + 8.0
        queue = provider.unresolved()
        while queue and time.monotonic() < deadline:
            time.sleep(0.02)
            queue = provider.unresolved()
        docs["unresolved_empty"] = queue
    finally:
        caller.close()
        provider.close()

    # --- post-payment revocation: the unresolved-financial class ---------
    calls = []

    async def preflight(owner_json, offer_json, brief_json):
        calls.append(brief_json)
        return None if len(calls) == 1 else "authority revoked before execution"

    revoked_dir = tmp_path / "revoked"
    revoked_dir.mkdir()
    provider, (caller,) = _topology(revoked_dir, preflight=preflight)
    try:
        prep = caller.prepare("summarize then revoke", task_id="xl-revoked")
        assert prep["status"] == "ok", prep
        assert caller.purchase(prep["prepared"])["status"] == "paid"
        _wait_billing(provider, 1)
        sent = caller.submit(prep["prepared"])
        assert sent["status"] == "unexecutable", sent
        docs["submit_unexecutable"] = sent
        docs["attempts_paid_unexecutable"] = caller.attempts()
        docs["purchase_after_unexecutable"] = caller.purchase(prep["prepared"])
        docs["unresolved_reconcile"] = provider.unresolved()
    finally:
        caller.close()
        provider.close()

    return {name: shape(doc) for name, doc in docs.items()}


def test_the_paid_a2a_documents_keep_their_cross_binding_shape(tmp_path):
    (tmp_path / "happy").mkdir()
    got = _capture(tmp_path)
    if os.environ.get("NET_A2A_CAPTURE") == "1":
        FIXTURE.parent.mkdir(parents=True, exist_ok=True)
        FIXTURE.write_text(json.dumps(got, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        pytest.skip(f"captured {FIXTURE}")
    want = json.loads(FIXTURE.read_text(encoding="utf-8"))
    assert sorted(got) == sorted(want), "the set of pinned documents changed"
    for name in want:
        assert got[name] == want[name], f"{name} drifted:\n{json.dumps(got[name], indent=2)}"
