---
title: Protected Streaming
description: "A long-lived protected call across organizations — token streams, uploads, interactive sessions: the four shapes, the provider handler contract, and how a call retires."
capability: Organization-scoped streaming RPC
---

# Protected Streaming

A protected [organization call](/docs/concepts/organizations) does not have to be
one request and one reply. The same authority that admits a unary call admits a
**long-lived** one: a model streaming tokens, a client uploading a file, an
interactive session pushing messages both ways. Every one of these is
organization-scoped — the caller proves which org it acts for, and the provider's
admission engine verifies it before the first item flows.

The shapes, the bind, and the failure vocabulary are the same in every binding.
This page walks the Rust surface end to end, then names the rest.

## The four shapes

| Shape | Caller verb | Provider verb |
| --- | --- | --- |
| unary | `call` | `serve_org` |
| server-streaming | `call_streaming` | `serve_org_streaming` |
| client-streaming | `call_client_stream` | `serve_org_client_stream` |
| duplex | `call_duplex` | `serve_org_duplex` |

All four ride the same `OrgAccess` choice and the same ordered admission checks. A
streaming frame arriving at a unary registration is refused as unsupported; a
streaming registration whose flags or opening-proof kind disagree with its shape
is denied on the merits. Both reach the caller as a coarse denial.

A streaming opening is bound to the transport session it rides — the opening
proof commits that session's full 32-byte Noise handshake hash. A captured
opening cannot be replayed on a later session, and a session with no binding
never admits a protected stream.

## A provider, all four shapes

```rust
use futures::StreamExt;
use net_sdk::org::{OrgAccess, OrgCaller};

mesh.install_org_authority(Path::new("/etc/net/authority"))?;

// unary
mesh.serve_org("customer.read", OrgAccess::Granted,
    |caller: OrgCaller, req: GetCustomer| async move {
        read_customer(req).await.map_err(|e| e.to_string())
    })?;

// server-streaming: emit items, then return.
mesh.serve_org_streaming("chat.complete", OrgAccess::Granted,
    |caller: OrgCaller, req: Prompt, sink| async move {
        for tok in complete(req).await? {
            sink.send(&tok)?;
        }
        Ok(())
    })?;

// client-streaming: drain the upload, return one terminal.
mesh.serve_org_client_stream("blob.put", OrgAccess::Granted,
    |caller: OrgCaller, mut upload| async move {
        let mut total = 0usize;
        while let Some(chunk) = upload.next().await {
            total += chunk.map_err(|e| e.to_string())?.len();
        }
        Ok(Stored { total })
    })?;

// duplex: independent request and response halves.
mesh.serve_org_duplex("session.open", OrgAccess::Granted,
    |caller: OrgCaller, mut up, mut down| async move {
        while let Some(msg) = up.next().await {
            let msg = msg.map_err(|e| e.to_string())?;
            down.send(&reply(msg))?;
        }
        Ok(())
    })?;
```

`access` selects who may call **and** how the service is announced, in one choice:
`SameOrg` admits your own organization and announces inside the owner audience;
`Granted` admits grant-holding organizations and announces inside the per-grant
audiences. There is no third variant and no separate visibility knob. Every
handler receives the provider-verified `OrgCaller`; nothing on it is
caller-claimed.

## A caller, all four shapes

```rust
use futures::StreamExt;
use net_sdk::org::OrgCredentials;

let credentials = OrgCredentials::from_parts(
    &membership_bytes, &dispatcher_bytes, &[grant_bytes],
    &[PathBuf::from("/etc/net/grants/cr.audience")],
)?;
let org = mesh.org(credentials)?;

// server-streaming: pull items until the stream ends.
let mut tokens = org.call_streaming("chat.complete", &prompt).await?;
while let Some(token) = tokens.next().await {
    print!("{}", token?);
}

// client-streaming: the signed opening rides the first item.
let mut upload = org.call_client_stream::<Chunk, Stored>("blob.put").await?;
for chunk in chunks {
    upload.send(&chunk).await?;
}
let stored: Stored = upload.finish().await?;

// duplex: split into independent send and receive halves.
let call = org.call_duplex::<Msg, Reply>("session.open").await?;
let (mut sink, mut stream) = call.into_split();
sink.send(&hello).await?;
while let Some(reply) = stream.next().await {
    handle(reply?);
}
```

Each verb resolves one provider and pins it for the call's whole life — every
`send` writes into that one call and never re-resolves. Dropping a call handle
emits exactly one CANCEL to the provider once the call is no longer live — a
split duplex call fires it when both halves drop. Protected calls are
direct-only; no direct session to the provider is a discovery failure, not a
relayed call.

## The handler contract

A protected handler runs inside the provider's supervisor, and the supervisor may
**drop the handler's future without a final poll** — when the call hits its
deadline, when the caller cancels, when a credential is revoked, or on teardown.
There is no cancellation callback and no notification that this happened.

Structure the handler so it can end on its own: drain the request stream or emit
the response items it owes, then return. Do not park work that must complete in a
detached task and assume a last poll.

The retirement observable depends on the surface, but where one exists it means
the same thing — stop:

- a handler with an **input side** observes retirement at the C ABI through
  `net_rpc_request_stream_next` returning `NET_RPC_ERR_STREAM_DONE`, and in Rust
  as its protected request stream ending (yields `None`). That end is shared with
  the clean-completion path, so a drain-and-return handler treats either as
  "stop" rather than trying to tell them apart;
- the **response sink is not an observable**. `net_rpc_response_sink_send` is a
  lossy, non-blocking send: on a live handle it reports success even after the
  call is retired, and its `NET_RPC_ERR_STREAM_DONE` means the handle is no
  longer live rather than that the call just retired. A handler whose only I/O is
  the sink — a server-streaming producer — therefore has no retirement
  observable at all, and must bound its own work.

## Lifetime and cancellation

A protected call's lifetime is finite by contract.

- The streaming verbs request no deadline, so a zero deadline resolves to the
  facade's **300 s default** — never "no deadline". (The unary verb is the
  exception: with no deadline it sets none.)
- A provider caps an explicitly requested deadline at **3600 s**. A request
  beyond the cap is **refused at opening**, not clamped: the caller is told it
  asked for something the provider does not offer.
- A cancel token can be reserved before a call and fired to cancel it. A token
  of zero is uncancellable.

Execution control is per binding: Node passes `{ deadlineMs, cancelToken }` on
the call verb, Python takes `deadline_ms` / `cancel_token`, and C takes them as
`net_org_call*` arguments. In Rust the streaming verbs carry the 300 s default;
the deadline-bearing byte seams belong to the binding layer. None of these is an
authorization input — they select no grant and no authority.

## The retirement contract

Every shape surfaces the same frozen `org:` vocabulary, but which item arrives
depends on the shape and on who retired the call:

| What happened | What the caller sees |
| --- | --- |
| Opening refused by admission | the stream's **terminal item** — `org:admission_denied:` with the coarse reason (`denied`, `not_supported`, `unavailable`) — on server-streaming / duplex, and `finish()`'s error on client-streaming (its opening is lazy: it rides the first `send`, or `finish` on the zero-item path). A **call-verb** failure is reserved for LOCAL opening-stage errors — no route, a binding refusal, an unauthorizable credential set — where nothing was sent |
| Credential revoked mid-call | the stream's final item (`finish()`'s terminal on client-streaming), `org:admission_denied:denied` |
| Deadline expiry, any shape | `org:rpc:timeout` — the stream's final item on server-streaming / duplex, `finish()`'s error on client-streaming, the call verb's error on unary |
| Caller cancel (the reserved token), any shape | `org:rpc:cancelled`, at the same seam as the deadline row |
| Stream handle dropped instead of cancelled | exactly one CANCEL on the wire and nothing observable (see above) |

The kind is not chosen by the shape. It is mapped from the wire status the
terminal frame carries — `Timeout` (`0x0003`) is `org:rpc:timeout`, `Cancelled`
(`0x0005`) is `org:rpc:cancelled` — and it tracks the retirement cause because
only the engine mints those reserved codes: a handler cannot, since any
handler application code outside `0x8000`–`0xFFFF` is clamped to `Internal` on
every call shape. So a deadline is `org:rpc:timeout`, a caller cancel is
`org:rpc:cancelled`, a revocation or a credential-clamp expiry is
`org:admission_denied:denied` (never a timeout), and any other remote terminal
is `org:rpc:server_error` with its status and diagnostic verbatim.

A **local** deadline is its own case and never reports `org:rpc:timeout`: on
the browser and leaf port a follower tab can reach its own deadline on a call
the leader node owns, and that outcome surfaces as **indeterminate** (the
browser kind `rpc-indeterminate`, not `rpc-timeout`) — the remote operation may
still have executed, and it is never retried. See
[the browser session](/docs/sdk/browser/session).

Only a credential-validity clamp or the next opening can stop a call already
running on a grant; grants have no revocation floor, so a revoked grant is not
enforced in flight.

## Names in every binding

| Shape | Rust | Node / TS | Python | Go | C |
| --- | --- | --- | --- | --- | --- |
| bind | `mesh.org(creds)` | `TypedOrgClient.bind` | `TypedOrgClient.bind` | `NewOrgClient` | `net_org_bind` |
| call unary | `call` | `call` | `call` | `OrgCall` | `net_org_call` |
| call server-streaming | `call_streaming` | `callStreaming` | `call_streaming` | `CallStreaming` | `net_org_call_streaming` |
| call client-streaming | `call_client_stream` | `callClientStream` | `call_client_stream` | `CallClientStream` | `net_org_call_client_stream` |
| call duplex | `call_duplex` | `callDuplex` | `call_duplex` | `CallDuplex` | `net_org_call_duplex` |
| serve unary | `serve_org` | `serveOrgTyped` | `serve_org_typed` | `ServeOrg` | `net_org_serve` |
| serve server-streaming | `serve_org_streaming` | `serveOrgStreamingTyped` | `serve_org_streaming` | `ServeOrgStreaming` | `net_org_serve_streaming` |
| serve client-streaming | `serve_org_client_stream` | `serveOrgClientStreamTyped` | `serve_org_client_stream` | `ServeOrgClientStream` | `net_org_serve_client_stream` |
| serve duplex | `serve_org_duplex` | `serveOrgDuplexTyped` | `serve_org_duplex` | `ServeOrgDuplex` | `net_org_serve_duplex` |

The C handles are the shared `net_rpc.h` stream and sink types — `net_org_call_streaming`
hands back the same handle a public stream uses, driven by the same
`net_rpc_stream_*` entry points. One `libnet`, so one `-l net`. Pin the org ABI with
`net_org_check_abi_version(NET_ORG_ABI_VERSION)` and refuse on any mismatch: the
check is exact equality, so rebuild against the current headers.

```c
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "net_org.h"
#include "net_rpc.h"

/* A server-streaming handler. It has NO retirement observable: the sink
 * send is a lossy, non-blocking send that reports success on a live handle
 * even after the call is retired. The supervisor may drop this handler
 * WITHOUT a final poll, so the work here is bounded and returns. */
static int on_stream(uint64_t handler_id, const net_org_caller_t* caller,
                     const uint8_t* req, size_t req_len,
                     RpcResponseSinkHandleC* sink, char** out_err) {
    static const uint8_t tick[] = "tick";
    (void)handler_id; (void)caller; (void)req; (void)req_len; (void)out_err;
    for (uint32_t n = 0; n < 3; ++n) {
        int rc = net_rpc_response_sink_send(sink, tick, sizeof(tick) - 1);
        if (rc == NET_RPC_ERR_STREAM_DONE) {
            return 0; /* defensive: the handle is no longer live */
        }
        if (rc != NET_RPC_OK) {
            return rc;
        }
    }
    return 0;
}

/* Once, at init, BEFORE any mesh clone is minted: register the
 * deallocator (every dispatcher refuses registration without it), then the
 * shape dispatcher. A failure here has no clone in hand to strand. */
static int init_org_dispatch(void) {
    if (net_org_set_callback_free(free) != 0) {
        return -1;
    }
    return net_org_set_streaming_handler_dispatcher(on_stream) == 0 ? 0 : -1;
}

/* After init_org_dispatch succeeded: reserve a handler id, then serve the
 * shape. The mesh arc is consumed on EVERY path — success and failure
 * alike: the call takes ownership of the clone the moment it is entered,
 * so never free it yourself (mint a fresh net_mesh_arc_clone per serve,
 * right before this call, so no early return sits between the mint and
 * the call that consumes it). */
static int serve_streaming(net_compute_mesh_arc_t* mesh, char** out_err) {
    uint64_t hid = net_org_reserve_handler_id();
    NetOrgServeHandle* serve = NULL;
    return net_org_serve_streaming(mesh, "chat.complete", 13,
                                   NET_ORG_ACCESS_GRANTED, hid, &serve, out_err);
}
```

## Where to read next

- [Organizations](/docs/concepts/organizations)
- [Private capabilities](/docs/guides/private-capabilities)
- [Typed RPC with nRPC](/docs/guides/nrpc)
- [Error codes](/docs/reference/error-codes)
