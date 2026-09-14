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
import { buildConnectRequest, parseCounters, parseDescriptors, type ConnectOptions, type NodeDescriptor } from '../node.js';
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
  private closed = false;

  /** @internal — use {@link openSession}. */
  constructor(private readonly inner: LeafWasmSession) {
    inner.on_event((json) => this.hub.deliver(json, parseSessionEvent));
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
  async signal(peerHex: string, dialog: number, kind: string, payload: Uint8Array): Promise<void> {
    await this.guard(() => this.inner.signal(peerHex, dialog, kind, payload));
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
   * interruption. A page that wants one back opens one.
   */
  async openStream(options: OpenStreamOptions): Promise<LeafStream> {
    return new LeafStream(await this.guard(() => this.inner.open_stream(options)));
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
   */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.hub.close();
    this.inner.close();
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
