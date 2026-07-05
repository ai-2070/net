"""Cross-language consent-surface parity fixture test
(`MCP_BRIDGE_SDK_PLAN.md` conformance).

Loads `crates/net/tests/cross_lang_mcp/consent_vectors.json` — the fixture
the Rust source-of-truth verifier (`sdk/tests/consent_golden_vectors.rs`)
validates — and asserts the Python consent bindings agree with the core.

Build the extension first:  maturin develop --features consent
"""

import json
from pathlib import Path

import pytest

pytest.importorskip("net._net")

netmod = pytest.importorskip("net")
if not hasattr(netmod, "CapabilityId"):
    pytest.skip("wheel built without the `consent` feature", allow_module_level=True)

from net import CapabilityId, ConsentPolicy, credential_requires_consent  # noqa: E402


def _fixture() -> dict:
    root = Path(__file__).resolve().parent.parent.parent.parent
    return json.loads((root / "tests" / "cross_lang_mcp" / "consent_vectors.json").read_text())


FIXTURE = _fixture()


@pytest.mark.parametrize("case", FIXTURE["cap_id_canonicalize"], ids=lambda c: c["name"])
def test_cap_id_canonicalize(case: dict) -> None:
    assert CapabilityId.parse(case["input"]).display() == case["expected"]


@pytest.mark.parametrize("case", FIXTURE["cap_id_invalid"], ids=lambda c: c["name"])
def test_cap_id_invalid(case: dict) -> None:
    with pytest.raises(ValueError):
        CapabilityId.parse(case["input"])


@pytest.mark.parametrize("case", FIXTURE["credential_requires_consent"], ids=lambda c: c["name"])
def test_credential_requires_consent(case: dict) -> None:
    assert credential_requires_consent(case["status"]) == case["expected"]


@pytest.mark.parametrize("case", FIXTURE["consent_decision"], ids=lambda c: c["name"])
def test_consent_decision(case: dict) -> None:
    policy = ConsentPolicy()
    for op in case["ops"]:
        {"allow": policy.allow, "pin": policy.pin, "unpin": policy.unpin}[op["op"]](op["cap_id"])
    assert policy.decide(case["cap_id"], case["credential_status"]) == case["expected"]
