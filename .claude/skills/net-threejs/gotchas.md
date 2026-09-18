# Gotchas, and the review invariant

## The review invariant

One sentence, and most defects here are a violation of it:

> **The store carries bounded JSON game state; the renderer derives appearance;
> the transport proves identity; and nothing crosses a layer boundary.**

Check a change against each clause:

- **Store** — does any replicated value describe *how it looks* rather than
  *what is true*? A colour index, a hull value, a position: yes. A
  `THREE.Object3D`, a React node, a camera matrix: no.
- **Renderer** — does the render path decide *what is visible*, rather than *how
  it looks*? If a render loop filters entities, the visibility decision belongs
  in `project` on the owner.
- **Transport** — is any identity taken from a frame field rather than the
  authenticated peer? No store frame has an originator to consult.
- **Boundary** — is an audience narrowing something it should refuse, or a
  replica writing `setState`?

## Footguns that cost the most

### Peer ids come in two spellings

Events carry the authenticated peer as an **exact decimal** string.
`openStream({ peer })`, `connectPeer`, `acceptPeer` and `handshakePeer` all want
**16 lowercase hex**. Handing the decimal straight back is rejected for a short
id and — worse — names a *different* peer for a 16-digit decimal one, which is a
cross-peer defect wearing the shape of a formatting bug. `peerIdHex()` (or
`NodeDescriptor.peerIdHex`) is the conversion; `peerHexOf` and `samePeer` are the
store's own reconciliation.

### `openStream` takes `label`, never a textual `streamId`

The leaf **derives** the numeric stream id from the label and sets a
discriminator bit that makes an unsolicited arrival classify as stream data
rather than a channel message. A textual id is refused by the wasm option reader
outright; an arbitrary number loses the discriminator. Two peer-addressed streams
opened under one `label` on one leaf share an id and both consumers receive both
peers' payloads with no error — vary the label per peer.

### A stream handle is fenced to its session incarnation

On the routed→direct upgrade the session is **replaced**, and a `send` on a
stream opened while routed rejects with a session error ("stale stream handle …
reopen the stream"). Reopen with the same peer and stream id on that rejection.
`LeafStream` keys inbound frames on `(peerNode, streamId)`, both compared
numerically — the event spells ids in decimal, the handle in hex.

### A replica has no `setState`

It is a type-level fact. Writes go through `act` (acknowledged) and `input`
(latest-value). If you find yourself wanting `setState` on a joiner, the design
you want is an `action` the owner authorizes.

### An `input` is lossy by contract

Newest wins, no reply, no replay, no gap recovery, and **loss does not imply a
successor**. A dropped steering update may be the last one. Never build gameplay
that requires every input to arrive; that is what `actions` are for.

### Never retry after `indeterminate`

An action that was **never submitted** is known not to have executed. An action
that was submitted and then timed out is `indeterminate`, and the store says
exactly that rather than retrying, because a silent resend is how one action
becomes two. Surface it, or wait. The same discipline applies to the session's
frozen-leader case: `indeterminate` **and** a follower role **and** an unchanged
generation **and** no reply to anything, including control chatter.

### `result-expired` is not a success receipt

It means this request cannot execute again and its original result is
unavailable. It asserts nothing about whether the original attempt committed — do
not report it as success.

### ICE timeout is not "UDP blocked"

An anchor that is down, misconfigured or saturated produces an identical timeout.
An ICE failure surfaces as `ice-timeout`, and only two observations together
narrow it to `udp-blocked`: the HTTPS bootstrap to *that anchor* succeeded, and a
STUN binding to the `rtc_addr` *that same anchor published* went unanswered. The
type system enforces this — there is no way to build a `udp-blocked` failure
without evidence. An explicit `iceServers: []` means *none*; saying nothing is
how you get the anchor-announced default.

### One node per origin

Two tabs calling `connect()` on one origin are two nodes contending for one
identity. Use `openSession()`, and serve both tabs from the same origin.

### An audience is an authorization boundary

A denied audience is **refused**, never narrowed. Visibility lives in the schema:
invisible entities are omitted, hidden fields are explicit `null`, and `empty()`
means absence. Zero must never be readable as "you cannot see this".

### `?mode=local` is not evidence that two browsers can play

The in-page demo runs host and joiners in one page over a development bus:
delivery is a function call and the authenticated peer is *assigned*, not proved
by a handshake. The store, codec, chunker and assembler are real; the mesh is
not. A screenshot of it proves nothing about two browsers. See `testing.md`.

## What not to build

- A second failure detector at the store or session layer. Timing a leader out
  and forcing an election races a tab that is about to resume holding a valid
  generation.
- A wrapper that retries `indeterminate` or re-sends an action. It causes the
  effect twice.
- A client-side audience filter. Filtering in the renderer leaves the data on the
  wire; the owner's `project` is the boundary.
- An ECS, schema DSL, or a second entity registry. One state object with nested
  records keyed by entity id is enough.
- A `setState`-shaped API for replicas, host election, CRDT, or multi-writer
  merge. v1 has none of these, deliberately.
