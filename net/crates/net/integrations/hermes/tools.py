"""Agent-facing tools for the ``net`` Hermes plugin — the mesh meta-tools.

Each is a thin async handler over the embedded node's
:class:`AsyncCapabilityGateway` (search / describe / invoke) or the shared pin
store (list / request). The consent gate, argument validation, and the pin
store's lock protocol all live in the Rust SDK — these handlers never re-derive
them (bridge doctrine H2). Handlers return a JSON string; the gateway already
speaks a structured ``{status: ...}`` shape, which is passed through verbatim so
the model can relay a ``requires_approval`` pin instruction or self-repair a
``validation_error``.

Naming is deliberate: the model must not confuse these with Hermes's *local*
``tool_search`` / ``tool_describe`` / ``tool_call``. Every description states
the mesh-vs-local distinction explicitly.
"""

from __future__ import annotations

import json
import time
from typing import Any, Dict

from . import node

# Surface a renewal warning once an enrolled device is within this window of its
# annual grant expiry (the "expiry warning before annual grant expiry"
# acceptance) — the signal that silent renewal hasn't refreshed it.
_RENEWAL_WINDOW_SECONDS = 30 * 24 * 60 * 60


def _json(obj: Any) -> str:
    return json.dumps(obj, ensure_ascii=False)


# ---------------------------------------------------------------------------
# Schemas (OpenAI-function shape: {name, description, parameters})
# ---------------------------------------------------------------------------

NET_SEARCH_SCHEMA: Dict[str, Any] = {
    "name": "net_search_capabilities",
    "description": (
        "Search the Net MESH for capabilities running on YOUR OTHER MACHINES "
        "(published there with `net wrap`), across the mesh this node has "
        "joined. This is NOT Hermes's local `tool_search`: `tool_search` finds "
        "tools on THIS machine, while `net_search_capabilities` finds REMOTE "
        "capabilities on your other nodes. Use it when the user wants to do "
        "something on another machine, or to reach a tool that isn't local. "
        "Returns each capability's id, name, description, credential status, and "
        "whether it needs local approval before it can be invoked. An empty "
        "result means no matching remote capability is currently reachable — "
        "that is a normal result, not an error."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": (
                    "Substring matched against a capability's id / name / "
                    "description. Pass an empty string to list every reachable "
                    "capability."
                ),
            },
        },
        "required": ["query"],
        "additionalProperties": False,
    },
}

NET_DESCRIBE_SCHEMA: Dict[str, Any] = {
    "name": "net_describe_capability",
    "description": (
        "Describe one Net MESH capability (found via net_search_capabilities) "
        "by its `provider/capability` id: its full input JSON schema, "
        "credential status, and whether it needs local approval. Call this "
        "before net_invoke_capability so you send well-formed arguments. This "
        "describes a REMOTE mesh capability, not a local Hermes tool."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "cap_id": {
                "type": "string",
                "description": "The `provider/capability` id from net_search_capabilities.",
            },
        },
        "required": ["cap_id"],
        "additionalProperties": False,
    },
}

NET_INVOKE_SCHEMA: Dict[str, Any] = {
    "name": "net_invoke_capability",
    "description": (
        "Invoke a Net MESH capability on another machine by its "
        "`provider/capability` id. Consent is enforced locally: if the "
        "capability needs approval you get `status: requires_approval` with the "
        "id to approve (call net_request_pin) — nothing is invoked. If the "
        "arguments don't match the schema you get `status: validation_error` "
        "with the reason; fix them against net_describe_capability and retry. "
        "On success `status` is `ok` — check the `is_error` field for a "
        "tool-level failure reported by the remote tool itself."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "cap_id": {
                "type": "string",
                "description": "The `provider/capability` id to invoke.",
            },
            "arguments": {
                "type": "object",
                "description": (
                    "The capability's own arguments, matching its input schema "
                    "(see net_describe_capability). Omit for a no-argument "
                    "capability."
                ),
                "additionalProperties": True,
            },
        },
        "required": ["cap_id"],
        "additionalProperties": False,
    },
}

NET_LIST_PINNED_SCHEMA: Dict[str, Any] = {
    "name": "net_list_pinned_capabilities",
    "description": (
        "List the Net MESH capabilities the user has approved (pinned) for "
        "invocation on this machine, plus any pending approval requests. "
        "Approved capabilities can be invoked with net_invoke_capability "
        "without further prompting."
    ),
    "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
}

NET_REQUEST_PIN_SCHEMA: Dict[str, Any] = {
    "name": "net_request_pin",
    "description": (
        "Request approval to invoke a Net MESH capability that currently needs "
        "it (you saw `status: requires_approval` from net_invoke_capability). "
        "This records a PENDING request only — it grants nothing by itself. A "
        "human approves it out of band from a trusted operator surface — Hermes's "
        "own approval UX, or another trusted frontend (the `net mcp pin approve "
        "<cap_id>` CLI is a fallback channel, not the canonical one). Once "
        "approved, net_invoke_capability will succeed. Do not tell the user the "
        "capability is usable until it has actually been approved."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "cap_id": {
                "type": "string",
                "description": "The `provider/capability` id to request approval for.",
            },
        },
        "required": ["cap_id"],
        "additionalProperties": False,
    },
}


# ---------------------------------------------------------------------------
# Handlers — async, matching Hermes's is_async dispatch (await the SDK's
# AsyncCapabilityGateway / AsyncPinStore, never block the event loop).
# ---------------------------------------------------------------------------


async def handle_net_search(args: Dict[str, Any], **_kw) -> str:
    query = str(args.get("query") or "")
    # The gateway returns a structured JSON string already — pass it through.
    return await node.gateway().search(query)


async def handle_net_describe(args: Dict[str, Any], **_kw) -> str:
    cap_id = str(args.get("cap_id") or "").strip()
    if not cap_id:
        return _json({"status": "error", "error": "cap_id is required"})
    return await node.gateway().describe(cap_id)


async def handle_net_invoke(args: Dict[str, Any], **_kw) -> str:
    cap_id = str(args.get("cap_id") or "").strip()
    if not cap_id:
        return _json({"status": "error", "error": "cap_id is required"})
    if not node.delegation_valid_for_invoke():
        # Never invoke under a revoked/expired delegation chain (the provider
        # would reject it too, but fail fast + honestly here).
        return _json(
            {
                "status": "denied",
                "cap_id": cap_id,
                "error": "the gateway delegation is revoked or expired; "
                "re-approve or renew it before invoking",
            }
        )
    arguments = args.get("arguments")
    if arguments is None:
        arguments = {}
    # The gateway takes the tool's own arguments as a JSON string; consent +
    # validation are applied inside it and the result comes back structured.
    return await node.gateway().invoke(cap_id, json.dumps(arguments))


async def handle_net_list_pinned(args: Dict[str, Any], **_kw) -> str:
    from net_sdk import AsyncPinStore

    try:
        store = AsyncPinStore(node.pin_store_path())
        approved = await store.approved()
        pending = await store.pending()
    except Exception as e:  # noqa: BLE001 — surface store failures as data, never raise
        return _json({"status": "error", "error": f"could not list pinned capabilities: {e}"})
    return _json({"status": "ok", "approved": approved, "pending": pending})


async def handle_net_request_pin(args: Dict[str, Any], **_kw) -> str:
    cap_id = str(args.get("cap_id") or "").strip()
    if not cap_id:
        return _json({"status": "error", "error": "cap_id is required"})
    from net_sdk import AsyncPinStore

    store = AsyncPinStore(node.pin_store_path())
    try:
        # `request` only ever writes a *pending* record and reports the state;
        # it never upgrades an existing pin (the model can't approve its own
        # access — that is the operator's out-of-band step).
        state = await store.request(cap_id)
    except Exception as e:  # noqa: BLE001 — surface store failures as data
        return _json({"status": "error", "error": f"could not record pin request: {e}"})
    # The message must match the actual state: `request` never downgrades an
    # already-approved pin, so it can report `approved` — in which case telling
    # the user approval is still required would be wrong.
    if state == "pending":
        message = (
            f"Approval required for `{cap_id}` in Hermes or another trusted "
            f"operator surface before it becomes usable. "
            f"CLI fallback: net mcp pin approve {cap_id}"
        )
    elif state == "approved":
        message = f"`{cap_id}` is already approved — it can be invoked now."
    else:
        message = f"`{cap_id}` is in state '{state}'."
    return _json(
        {
            "status": "pending_approval" if state == "pending" else state,
            "cap_id": cap_id,
            # H9: the CLI is a fallback channel, never the canonical approval UX —
            # a human approves in Hermes (or another trusted operator surface),
            # which writes to the same shared pin store.
            "approval_channels": ["telegram", "desktop", "cli_fallback"],
            "message": message,
        }
    )


# ---------------------------------------------------------------------------
# Device-enrollment (mesh admin) tools — the operator side of V2 Phase 1. These
# manage YOUR mesh's devices (invite / list / revoke); they are NOT capability
# tools. They need the node's user root (NET_MESH_IDENTITY_SEED) to sign
# delegations, and the invite tool serves enrollment on the node so a minted
# invite is dialable. The device-registry / revocation stores + the handshake
# all live in the Rust SDK (bridge doctrine H2).
# ---------------------------------------------------------------------------

NET_MESH_INVITE_SCHEMA: Dict[str, Any] = {
    "name": "net_mesh_invite",
    "description": (
        "Mint an INVITE to add a new device to YOUR operator mesh (device "
        "enrollment). Returns a single-use, short-lived invite STRING to share "
        "with the new device — it joins by running its own net node and calling "
        "`join` with the string — plus your mesh's root FINGERPRINT to confirm "
        "out of band (evil-twin defense). This is an operator action that "
        "authorizes a device to join the mesh you control; it does NOT invoke a "
        "capability. Requires the node to run under your root identity."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "ttl_seconds": {
                "type": "integer",
                "description": (
                    "How long the invite stays valid, in seconds (default 600 = "
                    "10 minutes). Keep it short — an invite is a pre-authorization "
                    "to *ask* to join, single-use."
                ),
            },
        },
        "additionalProperties": False,
    },
}

NET_MESH_DEVICES_SCHEMA: Dict[str, Any] = {
    "name": "net_mesh_devices",
    "description": (
        "List the DEVICES enrolled in YOUR operator mesh: each device's name, "
        "id, tags, when it enrolled, and whether it's revoked. This is your mesh "
        "device inventory — NOT the remote capabilities (that's "
        "net_search_capabilities). Requires your root identity."
    ),
    "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
}

NET_MESH_REVOKE_SCHEMA: Dict[str, Any] = {
    "name": "net_mesh_revoke",
    "description": (
        "REVOKE a device from YOUR operator mesh by its `device_id` (the hex id "
        "from net_mesh_devices). Raises the device's revocation floor — its "
        "delegations stop being honored by your providers on the next check — "
        "and marks it revoked in your inventory. Requires your root identity."
    ),
    "parameters": {
        "type": "object",
        "properties": {
            "device_id": {
                "type": "string",
                "description": "The device's hex id, from net_mesh_devices.",
            },
        },
        "required": ["device_id"],
        "additionalProperties": False,
    },
}


async def handle_net_mesh_invite(args: Dict[str, Any], **_kw) -> str:
    ttl = args.get("ttl_seconds")
    try:
        ttl = int(ttl) if ttl is not None else 600
    except (TypeError, ValueError):
        return _json({"status": "error", "error": "ttl_seconds must be an integer"})
    if ttl <= 0:
        ttl = 600
    try:
        operator = node.operator()
        invite = operator.invite(node.mesh().rendezvous_string(), ttl)
    except Exception as e:  # noqa: BLE001 — surface config/store failures as data
        return _json({"status": "error", "error": f"could not mint invite: {e}"})
    return _json(
        {
            "status": "ok",
            "invite": invite.encode(),
            "root_fingerprint": operator.root_fingerprint(),
            "expires_at": invite.expires_at,
            "message": (
                "Share this invite string with the new device. It is single-use "
                "and expires soon. Confirm the root_fingerprint matches on the "
                "device before it joins."
            ),
        }
    )


async def handle_net_mesh_devices(args: Dict[str, Any], **_kw) -> str:
    try:
        devices = node.operator().devices()
    except Exception as e:  # noqa: BLE001
        return _json({"status": "error", "error": f"could not list devices: {e}"})
    now = int(time.time())
    rows = []
    any_renewal = False
    for d in devices:
        # A device's grant lapses one grant-lifetime after it was (re-)recorded;
        # silent renewal re-records it, keeping this fresh. Revocation, not
        # expiry, is how a device is actually cut off.
        expires_at = d.enrolled_at + node._GRANT_TTL_SECONDS
        seconds_left = expires_at - now
        renewal_recommended = (not d.is_revoked) and seconds_left < _RENEWAL_WINDOW_SECONDS
        any_renewal = any_renewal or renewal_recommended
        rows.append(
            {
                "name": d.name,
                "device_id": d.device.hex(),
                "tags": list(d.tags),
                "enrolled_at": d.enrolled_at,
                "revoked": d.is_revoked,
                "expires_at": expires_at,
                "expires_in_days": max(0, seconds_left // 86400),
                "renewal_recommended": renewal_recommended,
            }
        )
    result: Dict[str, Any] = {"status": "ok", "devices": rows}
    if any_renewal:
        result["warning"] = (
            "One or more devices are within 30 days of their annual grant "
            "expiry and silent renewal has not refreshed them — re-invite the "
            "device or check its connectivity to this operator node."
        )
    return _json(result)


async def handle_net_mesh_revoke(args: Dict[str, Any], **_kw) -> str:
    device_id = str(args.get("device_id") or "").strip()
    if not device_id:
        return _json({"status": "error", "error": "device_id is required"})
    try:
        raw = bytes.fromhex(device_id.removeprefix("0x"))
    except ValueError:
        return _json({"status": "error", "error": "device_id must be a hex string"})
    if len(raw) != 32:
        return _json(
            {"status": "error", "error": "device_id must be 32 bytes (64 hex chars)"}
        )
    try:
        node.operator().revoke(raw)
    except Exception as e:  # noqa: BLE001
        return _json({"status": "error", "error": f"could not revoke device: {e}"})
    return _json(
        {
            "status": "ok",
            "revoked": device_id,
            "message": (
                f"Device {device_id[:16]}… revoked; its delegations stop being "
                "honored by your providers on the next check."
            ),
        }
    )


# The (name, schema, handler, emoji) rows the plugin registers, toolset "net".
TOOLS = (
    ("net_search_capabilities", NET_SEARCH_SCHEMA, handle_net_search, "\U0001f50e"),
    ("net_describe_capability", NET_DESCRIBE_SCHEMA, handle_net_describe, "\U0001f4cb"),
    ("net_invoke_capability", NET_INVOKE_SCHEMA, handle_net_invoke, "⚡"),
    ("net_list_pinned_capabilities", NET_LIST_PINNED_SCHEMA, handle_net_list_pinned, "\U0001f4cc"),
    ("net_request_pin", NET_REQUEST_PIN_SCHEMA, handle_net_request_pin, "\U0001f64b"),
    ("net_mesh_invite", NET_MESH_INVITE_SCHEMA, handle_net_mesh_invite, "\U0001f4e8"),
    ("net_mesh_devices", NET_MESH_DEVICES_SCHEMA, handle_net_mesh_devices, "\U0001f5a5️"),
    ("net_mesh_revoke", NET_MESH_REVOKE_SCHEMA, handle_net_mesh_revoke, "\U0001f6ab"),
)
