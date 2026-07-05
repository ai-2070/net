"""Agent-facing tools for the ``net`` Hermes plugin — the five mesh meta-tools.

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
from typing import Any, Dict

from . import node


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
    arguments = args.get("arguments")
    if arguments is None:
        arguments = {}
    # The gateway takes the tool's own arguments as a JSON string; consent +
    # validation are applied inside it and the result comes back structured.
    return await node.gateway().invoke(cap_id, json.dumps(arguments))


async def handle_net_list_pinned(args: Dict[str, Any], **_kw) -> str:
    from net_sdk import AsyncPinStore

    store = AsyncPinStore(node.pin_store_path())
    return _json(
        {
            "status": "ok",
            "approved": await store.approved(),
            "pending": await store.pending(),
        }
    )


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


# The (name, schema, handler, emoji) rows the plugin registers, toolset "net".
TOOLS = (
    ("net_search_capabilities", NET_SEARCH_SCHEMA, handle_net_search, "\U0001f50e"),
    ("net_describe_capability", NET_DESCRIBE_SCHEMA, handle_net_describe, "\U0001f4cb"),
    ("net_invoke_capability", NET_INVOKE_SCHEMA, handle_net_invoke, "⚡"),
    ("net_list_pinned_capabilities", NET_LIST_PINNED_SCHEMA, handle_net_list_pinned, "\U0001f4cc"),
    ("net_request_pin", NET_REQUEST_PIN_SCHEMA, handle_net_request_pin, "\U0001f64b"),
)
