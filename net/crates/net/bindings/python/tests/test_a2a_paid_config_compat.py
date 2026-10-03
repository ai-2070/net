"""Input compatibility of ``PaymentProvider.serve_a2a_configured``'s catalog.

`docs/internal/plans/NODE_A2A_PAID_ADMISSION_PLAN.md` WS-A moves the
binding-neutral half of ``src/a2a_paid.rs`` into shared Rust so the Node
binding can call it. The **catalog parser does not move** (review finding R5):
it is PyO3 extraction, and a generic ``dict -> JSON -> shared parser`` route
would change what Python accepts. This file was written and made green against
the parser **before** the move, and must stay green **unedited** across it.

It pins exactly the behaviors ``test_a2a_paid.py`` does not reach:

- keys the parser does not read are ignored, even when their value is not
  JSON-representable (``object()``), in a service entry and in its bounds;
- a key present with ``None`` reads exactly like an absent key — for the
  optional terms (``pricing_terms``, ``description``) *and* for a required
  one, which is refused as missing rather than as mistyped;
- a service entry or a ``bounds`` value that is not a dict is refused with
  the parser's own message;
- a wrong-typed term is refused with the parser's field-named message.

Messages are pinned by the parser-owned prefix only; the tail after it is
PyO3's own extraction text and is not this binding's contract.
"""

from __future__ import annotations

import json

import pytest

from test_a2a_paid import (
    FREE,
    MOCK_REQS,
    PAID,
    PaymentProvider,
    _mesh,
    _offer,
    _release,
    _topology,
)


class _Opaque:
    """A value no JSON encoder can represent. If the parser ever serialized
    the whole dict instead of reading the keys it names, this would raise."""


def _cb():
    async def cb(task_id, prompt, refs, tags, *, service, revision):
        return "blob://x"

    return cb


@pytest.fixture
def lone(tmp_path):
    """A started mesh and a provider over it, for refusals that never serve."""
    mesh = _mesh()
    mesh.start()
    provider = PaymentProvider(
        mesh, str(tmp_path / "engine.json"), unsafe_dev_mock_facilitator=True
    )
    terms = provider.pricing_terms(
        f"{mesh.node_id}/net.a2a.task/{PAID}", json.dumps(MOCK_REQS)
    )
    counter = iter(range(1_000_000))

    def serve(services, **kwargs):
        path = str(tmp_path / f"journal-{next(counter)}.json")
        return provider.serve_a2a_configured(_cb(), services, path, **kwargs)

    try:
        yield serve, terms
    finally:
        provider = None
        try:
            mesh.shutdown()
        except Exception:  # noqa: BLE001
            pass


def _refusal(serve, services):
    with pytest.raises(ValueError) as err:
        serve(services)
    return str(err.value)


# ---------------------------------------------------------------------------
# Accepted shapes, observed through the published catalog
# ---------------------------------------------------------------------------


def test_unread_keys_are_ignored_even_when_not_json_representable(tmp_path):
    def paid(terms):
        entry = _offer(terms)
        entry["operator_note"] = _Opaque()
        entry["bounds"] = {**entry["bounds"], "max_burst": _Opaque()}
        return entry

    free = {**_offer(None), "unused": _Opaque(), "also_unused": [_Opaque()]}

    provider, (caller,) = _topology(tmp_path, offers={PAID: paid, FREE: free})
    try:
        offers = {
            o["service_id"]: o
            for o in json.loads(caller.mesh.describe_a2a(caller.provider_node))
        }
        assert set(offers) == {PAID, FREE}
        for offer in offers.values():
            assert "operator_note" not in offer and "unused" not in offer
            assert set(offer["bounds"]) == {
                "max_prompt_bytes",
                "max_context_refs",
                "max_tags",
                "max_tag_bytes",
                "max_in_flight",
            }
    finally:
        _release(caller, caller.mesh)
        _release(provider, provider.mesh)


def test_none_reads_as_absent_for_the_optional_terms(tmp_path):
    explicit_none = {**_offer(None), "pricing_terms": None, "description": None}
    absent = {
        k: v for k, v in _offer(None).items() if k not in ("pricing_terms", "description")
    }

    provider, (caller,) = _topology(
        tmp_path, offers={"explicit-none": explicit_none, "absent": absent}
    )
    try:
        offers = {
            o["service_id"]: o
            for o in json.loads(caller.mesh.describe_a2a(caller.provider_node))
        }
        for sid in ("explicit-none", "absent"):
            # Free (no terms) and undescribed, identically.
            assert offers[sid].get("pricing_terms") is None, offers[sid]
            assert offers[sid].get("description") is None, offers[sid]
        a, b = dict(offers["explicit-none"]), dict(offers["absent"])
        a.pop("service_id"), b.pop("service_id")
        assert a == b
    finally:
        _release(caller, caller.mesh)
        _release(provider, provider.mesh)


# ---------------------------------------------------------------------------
# Refusals, pinned by the parser's own message
# ---------------------------------------------------------------------------


def test_none_for_a_required_term_is_refused_as_missing(lone):
    serve, terms = lone
    for key in ("revision", "bounds", "reservation_ttl_secs", "retention_secs"):
        msg = _refusal(serve, {PAID: {**_offer(terms), key: None}})
        assert msg == f'services["{PAID}"] is missing required key "{key}"', msg


def test_none_for_a_required_bound_is_refused_as_missing(lone):
    serve, terms = lone
    entry = _offer(terms)
    entry["bounds"] = {**entry["bounds"], "max_tags": None}
    msg = _refusal(serve, {PAID: entry})
    assert msg == f'services["{PAID}"] is missing required key "max_tags"', msg


def test_a_non_dict_service_entry_is_refused(lone):
    serve, _ = lone
    for bad in (["revision", "r1"], "r1", 7):
        msg = _refusal(serve, {PAID: bad})
        assert msg.startswith(f'services["{PAID}"] must be a dict of {{revision, '), msg


def test_a_non_dict_bounds_is_refused(lone):
    serve, terms = lone
    for bad in ([1, 2, 3, 4, 5], "small", 1024):
        msg = _refusal(serve, {PAID: {**_offer(terms), "bounds": bad}})
        assert msg.startswith(
            f'services["{PAID}"]["bounds"] must be a dict of {{max_prompt_bytes, '
        ), msg


@pytest.mark.parametrize("bad", ["600", -1, 1.5, 1 << 64])
def test_a_mistyped_integer_term_is_refused_by_name(lone, bad):
    serve, terms = lone
    msg = _refusal(serve, {PAID: {**_offer(terms), "reservation_ttl_secs": bad}})
    assert msg.startswith(
        f'services["{PAID}"]["reservation_ttl_secs"] must be a non-negative int: '
    ), msg


@pytest.mark.parametrize("bad", ["8", -1, 2.0, 1 << 64])
def test_a_mistyped_bound_is_refused_by_name(lone, bad):
    serve, terms = lone
    entry = _offer(terms)
    entry["bounds"] = {**entry["bounds"], "max_context_refs": bad}
    msg = _refusal(serve, {PAID: entry})
    assert msg.startswith(
        f'services["{PAID}"]["max_context_refs"] must be a non-negative int: '
    ), msg


def test_mistyped_string_terms_are_refused_by_name(lone):
    serve, terms = lone
    msg = _refusal(serve, {PAID: {**_offer(terms), "revision": 1}})
    assert msg.startswith(f'services["{PAID}"]["revision"] must be a str: '), msg
    msg = _refusal(serve, {PAID: {**_offer(terms), "description": 1}})
    assert msg.startswith(
        f'services["{PAID}"]["description"] must be a str or None: '
    ), msg
    msg = _refusal(serve, {PAID: {**_offer(terms), "pricing_terms": {"a": 1}}})
    assert msg.startswith(
        f'services["{PAID}"]["pricing_terms"] must be a str or None: '
    ), msg


def test_a_non_string_service_key_is_refused(lone):
    serve, terms = lone
    msg = _refusal(serve, {7: _offer(terms)})
    assert msg.startswith("services keys must be service-id strings: "), msg
