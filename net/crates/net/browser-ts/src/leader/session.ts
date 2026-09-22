/**
 * `MeshSession` — one node per origin, whichever tab you opened.
 *
 * §8's decision is that a browser origin runs exactly one Net node:
 * tabs contend for a Web Lock, the holder runs the node on the main
 * thread, and the others attach over a `BroadcastChannel` and get the
 * same API. This is that API in TypeScript.
 *
 * The difference from {@link BrowserNode} is one of scope, not of
 * capability: `connect()` gives a page *this tab's* node, and
 * `openSession()` gives it *the origin's* node. Everything a page can
 * do with one it can do with the other — nRPC calls, channels,
 * streams, announcements, queries, enrollment — and three methods are
 * promises here that are synchronous there, because on a follower the
 * answer lives in another tab:
 *
 * | | `BrowserNode` | `MeshSession` |
 * |---|---|---|
 * | `openStream` | synchronous | promise |
 * | `counters` | synchronous | promise |
 * | `isEnrolled` | synchronous | promise |
 *
 * Failures are the same taxonomy, re-typed by the same mapper, so a
 * proxied refusal and a direct one arrive as the same class. The one
 * addition is {@link NotLeaderError}: a tab that was superseded — a
 * suspended tab that resumed, say — is refused rather than served, and
 * that refusal is typed.
 */

import { fromWasmError, type LeafError } from '../errors.js';
import {
  buildConnectRequest,
  parseAttemptStatus,
  parseCounters,
  parseDescriptors,
  type ConnectOptions,
  type NodeDescriptor,
  type PeerConnectOutcome,
} from '../node.js';
import {
  acceptPeer as driveAccept,
  connectPeer as driveConnect,
  type PeerPrimitives,
} from '../peer-driver.js';
import { LeafStream, type OpenStreamOptions } from '../stream.js';
import { loadLeafWasm } from '../wasm.js';
import {
  isLifecycleEvent,
  parseSessionEvent,
  type SessionEvent,
  type SessionEventOf,
  type SessionLifecycleEvent,
} from './events.js';
import { asSessionModule, type LeafWasmSession, type LeafWasmSessionOptions } from './wasm.js';
import { EventHub, type Unsubscribe } from '../events.js';

/** Whether this tab runs the node or talks to the tab that does. */
export type SessionRole = 'leader' | 'follower';

/** {@link openSession}'s argument. */
export interface SessionOptions extends ConnectOptions {
  /**
   * Capabilities to announce, and to re-announce whenever this tab
   * becomes the leader.
   *
   * On the session rather than on `announce()` because a new leader
   * has to re-publish them without being asked — D2's restoration
   * step. Calling `announce()` as well is fine and additive.
   */
  capabilities?: readonly string[];
  /**
   * Channels this tab depends on.
   *
   * Declared up front so a *new* leader can restore them: the leader
   * keeps the union of every follower's declaration subscribed, and a
   * tab that only called `subscribe()` after a handoff would have a
   * window where its channel was not subscribed by anybody.
   */
  subscriptions?: readonly string[];
  /** The IndexedDB database holding the identity. Defaults to `net-mesh-leaf`. */
  dbName?: string;
  /**
   * Override the Web Lock and `BroadcastChannel` name. Defaults to
   * `net-mesh/<origin>/<identity-fingerprint>`.
   *
   * For tests that need two independent elections inside one origin.
   * A page should not set it: two scopes on one identity is two nodes
   * on one identity, which is the eviction §8 exists to prevent.
   */
  lockScope?: string;
}

/** The origin's Net node, as this tab sees it. */
export class MeshSession {
  /**
   * The same fan-out the direct surface uses, instantiated over the
   * wider event union. One implementation, so a throwing listener,
   * the never-unwind-into-Rust rule and the iterator buffering behave
   * identically on both surfaces.
   */
  private readonly hub = new EventHub<SessionEvent>();
  /**
   * Streams this session handed out, so the ones still open can be
   * ended when leadership moves.
   *
   * A stream is session-scoped and is **not** restored across a
   * leader change, which the Rust side enforces by binding each
   * handle to its opening generation. That enforcement makes a stale
   * `send` reject — but it says nothing about a consumer sitting in
   * `await stream[Symbol.asyncIterator]().next()`, which would simply
   * never settle: the wasm stream stops emitting and nothing tells
   * the queue to end. So the session owns the set and closes it,
   * which is what turns "no more data, ever" into the end of the
   * iteration a page is already written to handle.
   */
  private readonly streams = new Set<LeafStream>();
  /**
   * The generation the streams in that set were opened under.
   *
   * Tracked because the generation moving is the *only* signal an
   * abrupt leader disappearance gives. A page that crashes or
   * navigates away never sends its `leader_lost` announcement: the
   * promoted follower fails its own pending requests and reports
   * `leader_changed`, and so does every other surviving follower
   * when the successor's Leadership arrives. The Rust handle then
   * correctly suppresses the old stream's bytes by its opening
   * generation — which is precisely what leaves a consumer sitting
   * in `await iterator.next()` forever, waiting on a stream that is
   * now guaranteed never to emit again.
   */
  private streamGeneration: string;
  private closed = false;

  /** @internal — use {@link openSession}. */
  constructor(private readonly inner: LeafWasmSession) {
    this.streamGeneration = inner.generation();
    inner.on_event((json) => this.hub.deliver(json, parseSessionEvent));
    this.hub.onAny((event) => {
      // `leader_lost` is this tab losing the leader it was talking
      // to; `not_leader` is this tab discovering it *was* the leader
      // and is not any more. Either way every stream this session
      // handed out belonged to a node that is gone — and neither
      // moves this tab's generation, so both are named explicitly.
      if (event.type === 'leader_lost' || event.type === 'not_leader') {
        this.endStreams();
        return;
      }
      // And the abrupt case: a generation that moved under us. Read
      // off the session rather than out of the event, so a
      // `leader_changed` that merely repeats the generation this
      // tab already holds ends nothing — a duplicate notification
      // must not kill the streams the new generation just opened.
      const current = this.inner.generation();
      if (current === this.streamGeneration) return;
      this.streamGeneration = current;
      this.endStreams();
    });
  }

  /** `'leader'` if this tab runs the node, `'follower'` if another does. */
  role(): SessionRole {
    return this.inner.role() === 'leader' ? 'leader' : 'follower';
  }

  /**
   * The generation in force for this tab, as an exact decimal string.
   *
   * A string because it is a `u64`: a number would round above 2^53,
   * and a rounded generation is a fence that has stopped fencing.
   */
  generation(): string {
    return this.inner.generation();
  }

  /** The node's id, 16 lowercase hex digits, or `null` before one is known. */
  nodeIdHex(): string | null {
    return this.inner.node_id_hex() ?? null;
  }

  /** The identity fingerprint the Web Lock is named after. */
  fingerprint(): string {
    return this.inner.fingerprint();
  }

  /** The Web Lock and `BroadcastChannel` name. */
  scope(): string {
    return this.inner.scope();
  }

  /**
   * How long this tab took to take over, in milliseconds, or `null` if
   * it was the origin's first leader.
   *
   * Measured from the lock coming free to the node being re-bootstrapped
   * and the subscriptions and announcement restored. It is a
   * measurement, not a promise: the interruption a *peer* observes is
   * bounded by Net's own failure detection, which is a different and
   * much larger number.
   */
  interruptionMs(): number | null {
    return this.inner.interruption_ms() ?? null;
  }

  /**
   * Call an nRPC service.
   *
   * Rejects with {@link RpcError} — including `leader-lost` when the
   * tab running the node was replaced while the call was in flight.
   * Never a silent retry: §8's rule is that the caller is told and
   * decides.
   *
   * On a follower the deadline is armed here as well as on the
   * leader, and that is true whether or not `timeoutMs` was given:
   * omitting it takes the leaf's own 30 s default rather than
   * leaving the promise to a leader that may be frozen. Either way
   * expiry is `rpc-indeterminate` — the work may already have been
   * admitted in the other tab, and nothing here re-issues it.
   */
  async call(service: string, payload: Uint8Array, timeoutMs?: number): Promise<Uint8Array> {
    return await this.guard(() => this.inner.call(service, payload, timeoutMs));
  }

  /**
   * Subscribe to a channel.
   *
   * Also adds it to what a future leader restores, so a handoff does
   * not silently drop it.
   */
  async subscribe(channel: string): Promise<void> {
    await this.guard(() => this.inner.subscribe(channel));
  }

  /**
   * Release this tab's claim on a channel.
   *
   * Not `subscribe` run backwards. The origin runs one node and the
   * leader holds **one** membership for the union of every tab's
   * declarations, so this drops this tab's claim and the membership
   * itself is given up only when no other tab still wants the
   * channel. One tab closing a subscription therefore cannot stop
   * another tab that is still reading the same channel.
   *
   * Also removes it from what a future leader restores, so a handoff
   * does not silently bring back a channel this tab released.
   */
  async unsubscribe(channel: string): Promise<void> {
    await this.guard(() => this.inner.unsubscribe(channel));
  }

  /** Publish one payload to a channel. */
  async publish(channel: string, payload: Uint8Array): Promise<void> {
    await this.guard(() => this.inner.publish(channel, payload));
  }

  /** Publish this node's capability announcement. */
  async announce(capabilities: readonly string[]): Promise<void> {
    await this.guard(() => this.inner.announce([...capabilities]));
  }

  /** Find the nodes offering a capability. */
  async query(capability: string): Promise<NodeDescriptor[]> {
    return parseDescriptors(await this.guard(() => this.inner.query(capability)));
  }

  /** Every leaf counter. Values stay strings because they are `u64`. */
  async counters(): Promise<Record<string, string>> {
    return parseCounters(await this.guard(() => this.inner.counters_json()));
  }

  /**
   * Run the enrollment exchange if the node is not enrolled.
   *
   * Works on a follower too: one node per origin means one enrollment
   * per origin, so a follower asking to enroll is asking the only
   * session there is.
   */
  async enroll(): Promise<void> {
    await this.guard(() => this.inner.enroll());
  }

  /** Whether the anchor has admitted this node. */
  async isEnrolled(): Promise<boolean> {
    return await this.guard(() => this.inner.is_enrolled());
  }

  /**
   * Sign and send a `0x0D02` signalling envelope — the
   * session-independent path, so no session with `peer` is needed.
   */
  async signal(peerHex: string, dialogHex: string, kind: string, payload: Uint8Array): Promise<void> {
    await this.guard(() => this.inner.signal(peerHex, dialogHex, kind, payload));
  }

  /**
   * Connect directly to `nodeIdHex` (§9), wherever the node is.
   *
   * The same drive loop `BrowserNode.connectPeer` runs — literally
   * the same function, from `peer-driver.ts`, with this session's
   * primitives instead of the direct node's. One loop means the
   * offerer and the answerer cannot disagree about what supersession
   * or a terminal reading looks like, which is a disagreement neither
   * side could detect.
   *
   * On a follower every step is a proxy round trip and the attempt
   * runs on the leader tab's node. A step whose attempt was replaced
   * while the request was in flight is refused by the leaf before it
   * touches the replacement, and arrives here as `superseded`.
   */
  async connectPeer(nodeIdHex: string): Promise<PeerConnectOutcome> {
    return driveConnect(nodeIdHex, this.peerPrimitives(), parseAttemptStatus);
  }

  /**
   * Answer the offer `nodeIdHex` filed — {@link connectPeer}'s
   * counterpart, on whichever tab holds the lock.
   */
  async acceptPeer(nodeIdHex: string): Promise<PeerConnectOutcome> {
    return driveAccept(nodeIdHex, this.peerPrimitives(), parseAttemptStatus);
  }

  /**
   * The proxied primitives, each re-typed by the same mapper every
   * other method here uses, so a proxied refusal is the same class a
   * direct one is — which is what lets one drive loop classify both.
   */
  private peerPrimitives(): PeerPrimitives {
    return {
      offer: (peer) => this.guard(() => this.inner.peer_offer(peer)),
      acceptOffer: (peer) => this.guard(() => this.inner.peer_accept_offer(peer)),
      candidate: (peer, dialog) => this.guard(() => this.inner.peer_candidate(peer, dialog)),
      handshake: (peer, dialog) => this.guard(() => this.inner.peer_handshake(peer, dialog)),
    };
  }

  /**
   * Open a reliable or fire-and-forget stream.
   *
   * A promise where {@link BrowserNode.openStream} is synchronous: on
   * a follower the stream is opened by the tab that owns the
   * DataChannel. The object it resolves to is the same
   * {@link LeafStream}, so `await stream.send(bytes)` and
   * `for await (const payload of stream)` read the same either way.
   *
   * A stream is **not** restored across a leader change — it is
   * session-scoped, and pretending otherwise would hide a real
   * interruption. A page that wants one back opens one. What this
   * session does guarantee is that the interruption is *observable*:
   * the handle is retained here and ended when leadership moves, so
   * `for await (const payload of stream)` finishes instead of hanging
   * on a node that is gone.
   *
   * That guarantee has to cover the open itself. On a follower this
   * is a proxy round trip, so a result can arrive *after* the
   * leadership-loss notification that already drained the retained
   * set — and the handle it carries was stamped by Rust with the
   * generation the request was issued under, so it is stale on
   * arrival. Registering it would put a permanently silent stream
   * into a set nothing will drain again. The generation is therefore
   * captured before the await and compared after it, and a crossing
   * result is ended exactly as a consumer a microtask later would
   * have seen it.
   */
  async openStream(options: OpenStreamOptions): Promise<LeafStream> {
    const openedUnder = this.inner.generation();
    const stream = new LeafStream(await this.guard(() => this.inner.open_stream(options)));
    if (this.closed || this.inner.generation() !== openedUnder) {
      // Leadership (or this session) went away while the open was in
      // flight. Ending it here is the same disposition a consumer
      // would have got a microtask later, rather than a live handle
      // on a dead node.
      stream.close();
      return stream;
    }
    this.streams.add(stream);
    return stream;
  }

  /** Listen for one event tag. Returns a cancel handle. */
  on<T extends SessionEvent['type']>(
    type: T,
    handler: (event: SessionEventOf<T>) => void,
  ): Unsubscribe {
    return this.hub.on(type, handler);
  }

  /** Listen for every event. Returns a cancel handle. */
  onEvent(handler: (event: SessionEvent) => void): Unsubscribe {
    return this.hub.onAny(handler);
  }

  /**
   * Listen for the five lifecycle tags only — `leader_changed`,
   * `subscription_restored`, `leader_lost`, `generation_fenced`,
   * `not_leader`.
   *
   * A page that wants to render "reconnecting" wants exactly these and
   * none of the node traffic.
   */
  onLifecycle(handler: (event: SessionLifecycleEvent) => void): Unsubscribe {
    return this.hub.onAny((event) => {
      if (isLifecycleEvent(event)) handler(event);
    });
  }

  /** Consume events as an async iterable. */
  events(): AsyncIterableIterator<SessionEvent> {
    return this.hub.events();
  }

  /**
   * Stand down and release the lock.
   *
   * On a leader this is what lets a follower promote, so a page that
   * navigates away without calling it leaves the handoff to the
   * browser's own lock release.
   *
   * Every stream this session handed out is ended first, in that
   * order: a consumer awaiting the next payload has to be settled by
   * something, and after `inner.close()` nothing will ever emit
   * again.
   */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.endStreams();
    this.hub.close();
    this.inner.close();
  }

  /**
   * End every stream this session handed out.
   *
   * `LeafStream.close` ends each waiting consumer and clears the
   * listeners, which is precisely the disposition D2 specifies for a
   * stream across a leader change: failed and observable, never
   * resurrected and never left pending. Called on explicit
   * leadership loss, on a generation that moved under this tab, and
   * on close; idempotent, because a page may have closed a stream
   * itself.
   */
  private endStreams(): void {
    const open = [...this.streams];
    this.streams.clear();
    for (const stream of open) stream.close();
  }

  /**
   * Re-type whatever the boundary threw.
   *
   * One wrapper for every method because they all cross the same
   * boundary and a proxied failure must arrive as the same class a
   * direct one does — that is the whole point of the failure text
   * being carried verbatim through the proxy instead of re-parsed on
   * the Rust side.
   */
  private async guard<T>(operation: () => Promise<T>): Promise<T> {
    try {
      return await operation();
    } catch (error) {
      throw fromWasmError(error);
    }
  }
}

/**
 * Join the origin's Net node, electing a leader if there is none.
 *
 * Resolves as soon as this tab knows its role: the leader once it has
 * bootstrapped, a follower once it has attached. A follower keeps
 * queueing for the lock in the background, so when the leader goes
 * away it promotes itself, re-bootstraps with the same identity and
 * restores the subscriptions — without the page doing anything.
 *
 * Rejects with a typed {@link LeafError}. {@link IdentityError} covers
 * the two refusals worth knowing by name: a context with no Web Locks
 * API (where one-node-per-origin cannot be enforced, so the leaf
 * refuses rather than let two tabs evict each other) and a context
 * with no IndexedDB (where an identity cannot be stored, so the page
 * must inject one).
 */
export async function openSession(options: SessionOptions): Promise<MeshSession> {
  const module = asSessionModule(await loadLeafWasm(options));
  const request: LeafWasmSessionOptions = {
    // Shared with `connect()` deliberately: the two surfaces take the
    // same connect keys, and a key added to one must not be silently
    // missing from the other.
    ...buildConnectRequest(options),
    ...(options.capabilities ? { capabilities: [...options.capabilities] } : {}),
    ...(options.subscriptions ? { subscriptions: [...options.subscriptions] } : {}),
    ...(options.dbName === undefined ? {} : { dbName: options.dbName }),
    ...(options.lockScope === undefined ? {} : { lockScope: options.lockScope }),
  };

  try {
    return new MeshSession(await module.MeshSession.open(request));
  } catch (error) {
    throw fromWasmError(error);
  }
}
