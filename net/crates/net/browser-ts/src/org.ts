/**
 * Organization-scoped streaming, as a page wants it (plan §4.5).
 *
 * The eight org verbs — {@link OrgUnaryHandler call}, streaming,
 * client-streaming and duplex, each calling and serving — share one
 * credential/option vocabulary and one terminal error taxonomy
 * (`errors.ts`'s `OrgStreamError` family). The same eight names exist
 * on {@link BrowserNode} and on {@link MeshSession}; the wrappers here
 * serve both, so the two surfaces cannot drift into two spellings of
 * one contract.
 *
 * **Suspension and closure (plan §4.5).** Deadlines are absolute: a
 * frozen tab extends no lease and no window, so a call whose deadline
 * passes while its tab is suspended retires with its deadline
 * terminal on wake — {@link OrgTimeoutError}, or
 * {@link OrgIndeterminateError} where the work may already be
 * executing in another tab. Nothing resumes automatically: a retired
 * call is never transparently re-opened, because re-opening is a
 * fresh call with a fresh proof and MAY repeat effects. Node or tab
 * close retires ownership — pending calls fail typed, and each
 * dropped caller handle emits exactly one CANCEL.
 *
 * **Handler drop (F-S3.1-2), verbatim.** The retire supervisor may
 * drop the handler future without a final poll — cancellation is
 * observed through the retirement observables (the terminal item, the
 * sink's typed closed refusal, and `retired`), never assumed as a
 * handler-side event; a detached observer holding `retired` observes
 * the signal. After retirement a handler sees typed refusals from
 * `send`/`close`, and any return value it produces is discarded.
 */

import {
  fromWasmError,
  orgRetireReason,
  orgTerminalError,
  OrgCancelledError,
  ORG_SINK_CLOSED_REFUSAL,
  UnknownLeafError,
  type OrgRetireReason,
  type OrgStreamError,
} from './errors.js';
import type {
  LeafWasmOrgByteItem,
  LeafWasmOrgByteStreamHandle,
  LeafWasmOrgCallOptions,
  LeafWasmOrgClientStreamHandler,
  LeafWasmOrgDuplexCallHandle,
  LeafWasmOrgDuplexHandler,
  LeafWasmOrgRequestItem,
  LeafWasmOrgRequestStreamHandle,
  LeafWasmOrgResponseSinkHandle,
  LeafWasmOrgServeHandle,
  LeafWasmOrgStreamingHandler,
  LeafWasmOrgUnaryHandler,
  LeafWasmOrgUploadCallHandle,
  LeafWasmOrgCaller,
  LeafWasmOrgCredentials,
  LeafWasmOrgServeOptions,
} from './wasm.js';

/**
 * The credential set one org call presents.
 *
 * The proofs are the caller's own bytes — this package carries them
 * and never mints, re-derives or interprets them. No TypeScript crypto
 * or authority implementation exists here by design (plan §4.5).
 */
export interface OrgCallCredentials {
  /** The caller's membership certificate, its 156-byte wire form. */
  membership: Uint8Array;
  /** The dispatcher grant, its wire form. */
  dispatcher: Uint8Array;
  /** The capability grant, its wire form, when the call carries one. */
  capabilityGrant?: Uint8Array;
  /** The org the caller acts as. */
  actingOrg: string;
  /** The org that owns the provider. */
  providerOwnerOrg: string;
  /** The provider, exactly as the call addresses it. */
  provider: string;
  /** Proof freshness bound, seconds. */
  proofTtlSecs?: number;
}

/** Options for the four org call verbs. */
export interface OrgCallOptions {
  credentials: OrgCallCredentials;
  /**
   * The call's deadline, milliseconds. **Absolute**, in the
   * suspension contract's sense: a frozen tab extends it by nothing,
   * and on wake an overdue call retires with its deadline terminal.
   */
  deadlineMs?: number;
  /**
   * Initial response-direction flow-control window, **chunk
   * credits** — the wire header's unit: one credit permits one item
   * frame, whatever the item's byte size. NOT bytes; a byte count
   * here reads as a huge credit budget and nothing ever parks.
   */
  streamWindowInitial?: number;
  /**
   * Initial request-direction flow-control window, **chunk
   * credits** (as {@link OrgCallOptions.streamWindowInitial}).
   */
  requestWindowInitial?: number;
}

/** Options for the four org serve verbs. */
export interface OrgServeOptions {
  /** The org that owns the provider side of every admitted call. */
  ownerOrg: string;
}

/**
 * Which authority admits a served call: `'same-org'` is owner-org
 * traffic only, `'granted'` admits capability grants too. Anything
 * else is refused loudly at registration.
 */
export type OrgAccess = 'same-org' | 'granted';

/**
 * The verified caller attribution a handler receives. Every field is
 * the leaf's own verified reading — `entity` is 64 hex digits — and
 * none of it is caller-asserted at the handler.
 */
export interface OrgCaller {
  /** The caller's entity id, 64 hex digits. */
  entity: string;
  /** The org the caller acted as. */
  actingOrg: string;
  /** The org that owns this provider. */
  providerOrg: string;
  /** The provider, exactly as the call addressed it. */
  provider: string;
  /** The capability the admission covered. */
  capability: string;
  /** Whether the caller acted inside `providerOrg`. */
  isSameOrg: boolean;
}

/**
 * The response half of an org call: pull items, or cancel.
 *
 * `next()`'s terminal error item carries the §4.3 typed error —
 * `AdmissionDenied('denied')` ({@link OrgRevokedError} where the cause
 * was revocation), {@link OrgTimeoutError}, {@link OrgCancelledError},
 * and the rest of the `OrgStreamError` family. The async iterator
 * **throws** that typed error at the terminal error item, which is
 * idiomatic for `for await`; a clean end is a plain loop end.
 */
export interface OrgByteStream {
  next(): Promise<{ done: boolean; value?: Uint8Array; error?: OrgStreamError }>;
  /**
   * Drop this caller handle — exactly one CANCEL leaves, and the
   * stream's terminal becomes {@link OrgCancelledError}.
   */
  cancel(): void;
  [Symbol.asyncIterator](): AsyncIterator<Uint8Array>;
}

/**
 * A client-streaming upload: send bodies, then `finish()` for the
 * reply. `finish()` rejects with the typed terminal error.
 */
export interface OrgUploadCall {
  send(payload: Uint8Array): Promise<void>;
  /** Half-close the upload and await the reply. */
  finish(): Promise<Uint8Array>;
  /** Drop this caller handle — exactly one CANCEL leaves. */
  cancel(): void;
}

/**
 * The upload half of a duplex call. The caller-side sink has
 * `finishSending`/`cancel` and deliberately no `retired`: its
 * retirement is the stream's terminal item, one observable per side.
 */
export interface OrgDuplexSink {
  send(payload: Uint8Array): Promise<void>;
  /** Half-close the request direction; the response direction runs on. */
  finishSending(): Promise<void>;
  /** Drop this caller handle — exactly one CANCEL leaves. */
  cancel(): void;
}

/** The two halves of a duplex call. */
export interface OrgDuplexHandles {
  sink: OrgDuplexSink;
  stream: OrgByteStream;
}

/**
 * The response sink a streaming or duplex **handler** receives.
 *
 * **Handler drop (F-S3.1-2), verbatim.** The retire supervisor may
 * drop the handler future without a final poll — cancellation is
 * observed through the retirement observables (the terminal item, the
 * sink's typed closed refusal, and `retired`), never assumed as a
 * handler-side event; a detached observer holding `retired` observes
 * the signal. After retirement `send`/`close` refuse typed with THE
 * closed refusal ({@link ORG_SINK_CLOSED_REFUSAL} — one text whatever
 * the verdict, matching the leaf's own hardening byte for byte) and a
 * handler return value is discarded.
 */
export interface OrgResponseSink {
  send(payload: Uint8Array): Promise<void>;
  /** Half-close the response direction: the call's normal ending. */
  close(): Promise<void>;
  /**
   * THE retirement signal. Resolves with exactly one of
   * {@link OrgRetireReason}'s seven verdicts when — and only when —
   * this sink is retired before its call completed. On normal
   * completion the call's own terminal settles every consumer and
   * this promise stays pending: hold it as a signal, not a completion
   * latch. Memoized at construction — one `retired()` crosses the
   * boundary, and a detached observer holding this promise observes
   * the signal.
   */
  retired: Promise<OrgRetireReason>;
}

/**
 * The request stream a client-streaming or duplex **handler**
 * receives.
 *
 * Its `next()` has **no** error arm: this stream's termination is
 * observed ONLY through {@link OrgRequestStream.retired}, and
 * iteration ends at the `done` item. The F-S3.1-2 level is the same
 * as {@link OrgResponseSink}'s — the retire supervisor may drop the
 * handler future without a final poll, and the retirement observables
 * (the `done` terminal item, the sink's typed closed refusal, and
 * `retired`) are the only reliable cancellation record; a detached
 * observer holding `retired` observes the signal.
 */
export interface OrgRequestStream {
  next(): Promise<{ done: boolean; value?: Uint8Array }>;
  /** As {@link OrgResponseSink.retired} — one verdict, memoized. */
  retired: Promise<OrgRetireReason>;
  [Symbol.asyncIterator](): AsyncIterator<Uint8Array>;
}

/**
 * One serve registration.
 *
 * **`close()` is a retirement, not a graceful unregister (C9's
 * protected split).** Closing an org serve handle retires its live
 * protected calls — each with its exact retirement terminal, the
 * deadline/cancel/revocation vocabulary — and refuses new openings on
 * that service. This is deliberately NOT the public path's
 * let-existing-calls-finish behavior.
 */
export interface OrgServeHandle {
  readonly service: string;
  close(): void;
}

/** The unary org handler: one request in, one reply out. */
export type OrgUnaryHandler = (caller: OrgCaller, request: Uint8Array) => Promise<Uint8Array>;
/** The server-streaming org handler: one request in, items out. */
export type OrgStreamingHandler = (
  caller: OrgCaller,
  request: Uint8Array,
  sink: OrgResponseSink,
) => Promise<void>;
/** The client-streaming org handler: requests in, one reply out. */
export type OrgClientStreamHandler = (
  caller: OrgCaller,
  requests: OrgRequestStream,
) => Promise<Uint8Array>;
/** The duplex org handler: requests in, items out, independently. */
export type OrgDuplexHandler = (
  caller: OrgCaller,
  requests: OrgRequestStream,
  sink: OrgResponseSink,
) => Promise<void>;

/**
 * A live caller-side org handle its owner may have to retire — the
 * registration {@link OrgHandleRegistry} keeps.
 */
export interface OrgCallHandle {
  /**
   * Retire it now: at most one CANCEL leaves, and every parked
   * consumer settles with `error` as its typed terminal.
   */
  retire(error: OrgStreamError): void;
}

/**
 * The live org handles one node or session handed out, so its
 * teardown can retire them: node close retires ownership — pending
 * calls fail typed, one CANCEL per dropped caller handle.
 */
export class OrgHandleRegistry {
  private readonly live = new Set<OrgCallHandle>();

  add(handle: OrgCallHandle): void {
    this.live.add(handle);
  }

  delete(handle: OrgCallHandle): void {
    this.live.delete(handle);
  }

  /**
   * Retire every live handle with one typed terminal. Drains first,
   * so a handle's own teardown cannot re-enter this loop. Every
   * handle is attempted; the raw failures come back rather than being
   * swallowed, for the owner's teardown ledger.
   */
  retireAll(error: OrgStreamError): unknown[] {
    const open = [...this.live];
    this.live.clear();
    const failures: unknown[] = [];
    for (const handle of open) {
      try {
        handle.retire(error);
      } catch (failure) {
        failures.push(failure);
      }
    }
    return failures;
  }
}

/**
 * The response half of an org call, wrapped: terminal items become
 * the §4.3 typed errors, and the async iterator throws them.
 */
export class OrgStream implements OrgByteStream, OrgCallHandle {
  private terminal: ByteItemOut | null = null;
  /** A boundary rejection, latched so a second pull sees the same typed failure. */
  private failure: unknown = null;
  private handleDropped = false;
  private readonly waiters: Array<{ resolve: (item: ByteItemOut) => void; reject: (error: unknown) => void }> = [];

  /**
   * @param `onCancel` drops the underlying handle — at most once, so
   * one CANCEL leaves however many halves initiate the drop.
   */
  constructor(
    private readonly inner: LeafWasmOrgByteStreamHandle,
    private readonly onCancel: () => void,
    private readonly onDone?: () => void,
  ) {}

  next(): Promise<ByteItemOut> {
    if (this.failure !== null) return Promise.reject(this.failure);
    if (this.terminal !== null) return Promise.resolve(this.terminal);
    const { promise, resolve, reject } = Promise.withResolvers<ByteItemOut>();
    const waiter = { resolve, reject };
    this.waiters.push(waiter);
    void this.inner.next().then(
      (raw) => {
        // BROWSER-4: this pull's waiter leaves the list the moment the
        // pull settles — a completed pull retains nothing to the
        // terminal.
        spliceWaiter(this.waiters, waiter);
        const item = byteItem(raw);
        if (item.done) this.complete(item);
        // With-resolvers settlement is first-wins, so a pull a
        // retirement already settled simply keeps that outcome.
        resolve(item);
      },
      (error: unknown) => {
        // A boundary rejection propagates as its classified self —
        // a proxied `LeaderLost` is the existing `RpcError` kind,
        // re-typed by the one mapper — and is latched for the next
        // pull. Not folded into an error item: the item vocabulary
        // is the §4.3 terminal set, and inventing a translation for
        // a throw this package cannot name is how a taxonomy turns
        // into guessing.
        spliceWaiter(this.waiters, waiter);
        const failure = fromWasmError(error);
        // `fail` settles every still-parked pull and latches the
        // failure for the next one; this pull settles itself.
        this.fail(failure);
        reject(failure);
      },
    );
    return promise;
  }

  cancel(): void {
    this.retire(new OrgCancelledError());
  }

  /**
   * Retire this stream: one CANCEL at most, every parked and future
   * `next()` settled with `error` as the terminal item.
   */
  retire(error: OrgStreamError): void {
    if (this.terminal !== null || this.failure !== null) return;
    this.dropHandle();
    this.complete({ done: true, error });
  }

  [Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
    // A terminal's final body is yielded BEFORE the end (BROWSER-1):
    // one done item carrying bytes is one yield plus one end, so the
    // end is this iterator's own latch, not the item's `done`.
    let ended = false;
    return {
      next: async () => {
        if (ended) return { done: true, value: undefined };
        const item = await this.next();
        // THROWS the typed error at the terminal error item — the
        // idiomatic `for await` shape, so `catch` sees the §4.3 class.
        if (item.error !== undefined) throw item.error;
        if (item.value === undefined) {
          ended = true;
          return { done: true, value: undefined };
        }
        if (item.done) ended = true;
        return { done: false, value: item.value };
      },
      return: async () => {
        ended = true;
        this.cancel();
        return { done: true, value: undefined };
      },
    };
  }

  private complete(item: ByteItemOut): void {
    if (this.terminal !== null) return;
    this.terminal = item;
    this.onDone?.();
    const waiters = [...this.waiters];
    this.waiters.length = 0;
    for (const waiter of waiters) waiter.resolve(item);
  }

  private fail(error: unknown): void {
    if (this.terminal !== null || this.failure !== null) return;
    this.failure = error;
    this.onDone?.();
    const waiters = [...this.waiters];
    this.waiters.length = 0;
    for (const waiter of waiters) waiter.reject(error);
  }

  private dropHandle(): void {
    if (this.handleDropped) return;
    this.handleDropped = true;
    try {
      this.onCancel();
    } catch {
      // A handle that refuses its own drop is already retired at the
      // boundary; the typed terminal below is what the consumer sees.
    }
  }
}

/** A client-streaming upload, wrapped. */
export class OrgUpload implements OrgUploadCall, OrgCallHandle {
  private failure: OrgStreamError | null = null;
  private handleDropped = false;
  private readonly waiters: Array<{ reject: (error: unknown) => void }> = [];

  constructor(
    private readonly inner: LeafWasmOrgUploadCallHandle,
    private readonly onDone?: () => void,
  ) {}

  async send(payload: Uint8Array): Promise<void> {
    if (this.failure !== null) throw this.failure;
    try {
      await this.inner.send(payload);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  finish(): Promise<Uint8Array> {
    if (this.failure !== null) return Promise.reject(this.failure);
    const { promise, resolve, reject } = Promise.withResolvers<Uint8Array>();
    const waiter = { reject };
    this.waiters.push(waiter);
    void this.inner.finish().then(
      (body) => {
        spliceWaiter(this.waiters, waiter);
        this.onDone?.();
        resolve(body);
      },
      (error: unknown) => {
        spliceWaiter(this.waiters, waiter);
        this.onDone?.();
        // finish() rejects with the typed terminal error (§4.3).
        reject(fromWasmError(error));
      },
    );
    return promise;
  }

  cancel(): void {
    this.retire(new OrgCancelledError());
  }

  /** Retire this upload: one CANCEL at most; parked `finish()` rejects typed. */
  retire(error: OrgStreamError): void {
    if (this.failure !== null) return;
    this.failure = error;
    if (!this.handleDropped) {
      this.handleDropped = true;
      try {
        this.inner.cancel();
      } catch {
        // Already retired at the boundary; `error` is the outcome.
      }
    }
    this.onDone?.();
    const waiters = [...this.waiters];
    this.waiters.length = 0;
    for (const waiter of waiters) waiter.reject(error);
  }
}

/** A duplex call, wrapped: one call-level CANCEL however both halves end. */
export class OrgDuplex implements OrgDuplexHandles, OrgCallHandle {
  /** The two halves share one drop, so one CANCEL leaves per call. */
  private handleDropped = false;
  private failure: OrgStreamError | null = null;
  readonly sink: OrgDuplexSink;
  readonly stream: OrgStream;

  constructor(
    private readonly inner: LeafWasmOrgDuplexCallHandle,
    private readonly onDone?: () => void,
  ) {
    this.stream = new OrgStream(
      inner.stream(),
      () => this.dropHandle(),
      () => this.onDone?.(),
    );
    this.sink = {
      send: async (payload: Uint8Array) => {
        if (this.failure !== null) throw this.failure;
        try {
          await this.inner.send(payload);
        } catch (error) {
          throw fromWasmError(error);
        }
      },
      finishSending: async () => {
        if (this.failure !== null) throw this.failure;
        try {
          await this.inner.finish_sending();
        } catch (error) {
          throw fromWasmError(error);
        }
      },
      cancel: () => {
        this.retire(new OrgCancelledError());
      },
    };
  }

  /** Retire the whole call: one CANCEL, both halves settled typed. */
  retire(error: OrgStreamError): void {
    this.failure ??= error;
    this.dropHandle();
    // Idempotent one level down: a stream that already reached its
    // own terminal keeps that outcome.
    this.stream.retire(error);
    this.onDone?.();
  }

  private dropHandle(): void {
    if (this.handleDropped) return;
    this.handleDropped = true;
    try {
      this.inner.cancel();
    } catch {
      // Already retired at the boundary; `error` is the outcome.
    }
  }
}

/**
 * The response sink a handler receives, wrapped. `retired` is
 * memoized at construction — the boundary's `retired()` is called
 * exactly once and the one signal is what a detached observer holds.
 */
export class OrgSink implements OrgResponseSink {
  readonly retired: Promise<OrgRetireReason>;
  private verdict: OrgRetireReason | null = null;

  constructor(private readonly inner: LeafWasmOrgResponseSinkHandle) {
    this.retired = inner.retired().then(
      (raw: string) => {
        this.verdict ??= orgRetireReason(raw);
        return this.verdict;
      },
      () => {
        // A `retired()` that itself fails is reported as a teardown
        // verdict rather than left unsettled — an observer must get
        // ONE signal, never a hang and never a rejection.
        this.verdict ??= 'replaced';
        return this.verdict;
      },
    );
  }

  async send(payload: Uint8Array): Promise<void> {
    if (this.verdict !== null) throw new OrgCancelledError(ORG_SINK_CLOSED_REFUSAL);
    try {
      await this.inner.send(payload);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /** Half-close the response direction: the call's normal ending. */
  async close(): Promise<void> {
    if (this.verdict !== null) throw new OrgCancelledError(ORG_SINK_CLOSED_REFUSAL);
    try {
      await this.inner.close();
    } catch (error) {
      throw fromWasmError(error);
    }
  }
}

/**
 * The request stream a handler receives, wrapped. Its terminal is the
 * `done` item; the abnormal record is `retired`, memoized once as
 * {@link OrgResponseSink.retired} is.
 */
export class OrgRequests implements OrgRequestStream {
  readonly retired: Promise<OrgRetireReason>;
  private verdict: OrgRetireReason | null = null;
  private readonly waiters: Array<(item: { done: boolean; value?: Uint8Array }) => void> = [];

  constructor(private readonly inner: LeafWasmOrgRequestStreamHandle) {
    this.retired = inner.retired().then(
      (raw: string) => {
        this.settle(orgRetireReason(raw));
        return this.verdict as OrgRetireReason;
      },
      () => {
        this.settle('replaced');
        return this.verdict as OrgRetireReason;
      },
    );
  }

  next(): Promise<{ done: boolean; value?: Uint8Array }> {
    if (this.verdict !== null) return Promise.resolve({ done: true });
    const { promise, resolve } = Promise.withResolvers<{ done: boolean; value?: Uint8Array }>();
    this.waiters.push(resolve);
    void this.inner.next().then(
      (raw: LeafWasmOrgRequestItem) => {
        spliceWaiter(this.waiters, resolve);
        resolve(requestItem(raw));
      },
      () => {
        // A boundary throw here ends the iteration: the request
        // direction is over one way or the other, and `retired` is
        // where a handler reads which.
        spliceWaiter(this.waiters, resolve);
        resolve({ done: true });
      },
    );
    return promise;
  }

  [Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
    return {
      next: async () => {
        const item = await this.next();
        if (item.done || item.value === undefined) return { done: true, value: undefined };
        return { done: false, value: item.value };
      },
      return: async () => ({ done: true, value: undefined }),
    };
  }

  private settle(verdict: OrgRetireReason): void {
    if (this.verdict !== null) return;
    this.verdict = verdict;
    const waiters = [...this.waiters];
    this.waiters.length = 0;
    for (const waiter of waiters) waiter({ done: true });
  }
}

/**
 * One serve registration, wrapped. `close()` is C9's protected-split
 * retirement: it retires the registration's live protected calls with
 * their exact terminals and refuses new openings on the service.
 */
export class OrgServe implements OrgServeHandle, OrgCallHandle {
  private closed = false;

  constructor(
    private readonly inner: LeafWasmOrgServeHandle,
    private readonly onDone?: () => void,
  ) {}

  get service(): string {
    return this.inner.service();
  }

  /**
   * **A retirement, not a graceful unregister (C9's protected
   * split).** Every live protected call this registration admitted is
   * retired with its exact retirement terminal — the
   * deadline/cancel/revocation vocabulary — and new openings on the
   * service are refused. Deliberately NOT the public path's
   * let-existing-calls-finish behavior.
   */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.onDone?.();
    this.inner.close();
  }

  /** The registry's teardown verb: for a registration it is `close()`. */
  retire(_error: OrgStreamError): void {
    this.close();
  }
}

/**
 * Build exactly the options object `call_org*` reads.
 *
 * A fresh object, so a caller mutating its own afterwards cannot
 * change a call already in flight. The byte fields are forwarded as
 * the `Uint8Array`s they must be — the leaf refuses anything else
 * loudly, and a silent re-encode here would be a second codec.
 */
export function toWasmOrgCallOptions(options: OrgCallOptions): LeafWasmOrgCallOptions {
  const credentials: LeafWasmOrgCredentials = {
    membership: options.credentials.membership,
    dispatcher: options.credentials.dispatcher,
    ...(options.credentials.capabilityGrant === undefined
      ? {}
      : { capabilityGrant: options.credentials.capabilityGrant }),
    actingOrg: options.credentials.actingOrg,
    providerOwnerOrg: options.credentials.providerOwnerOrg,
    provider: options.credentials.provider,
    ...(options.credentials.proofTtlSecs === undefined
      ? {}
      : { proofTtlSecs: options.credentials.proofTtlSecs }),
  };
  return {
    credentials,
    ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
    ...(options.streamWindowInitial === undefined
      ? {}
      : { streamWindowInitial: options.streamWindowInitial }),
    ...(options.requestWindowInitial === undefined
      ? {}
      : { requestWindowInitial: options.requestWindowInitial }),
  };
}

/** Build exactly the options object `serve_org*` reads. */
export function toWasmOrgServeOptions(options: OrgServeOptions): LeafWasmOrgServeOptions {
  return { ownerOrg: options.ownerOrg };
}

/**
 * Read one handler invocation's caller attribution.
 *
 * The boundary hands the crate's `to_json` JSON **string** (the same
 * convention `on_event` uses), camelCase keys verbatim
 * {@link OrgCaller}'s; anything that does not parse to an object is
 * refused rather than served with an attribution this build cannot
 * vouch for.
 */
export function parseOrgCaller(raw: LeafWasmOrgCaller): OrgCaller {
  const value: unknown = JSON.parse(raw);
  if (value === null || typeof value !== 'object') {
    throw new UnknownLeafError('the caller attribution is not an OrgCaller object');
  }
  const record = value as Record<string, unknown>;
  return {
    entity: String(record.entity ?? ''),
    actingOrg: String(record.actingOrg ?? ''),
    providerOrg: String(record.providerOrg ?? ''),
    provider: String(record.provider ?? ''),
    capability: String(record.capability ?? ''),
    isSameOrg: record.isSameOrg === true,
  };
}

/** Adapt a unary handler to the boundary's `(caller, request)` trampoline. */
export function unaryOrgTrampoline(handler: OrgUnaryHandler): LeafWasmOrgUnaryHandler {
  return async (caller, request) => await handler(parseOrgCaller(caller), request);
}

/**
 * Adapt a streaming handler to `(caller, request, sink)`. The sink is
 * wrapped once per invocation; its `retired` is the one signal the
 * F-S3.1-2 level names.
 */
export function streamingOrgTrampoline(handler: OrgStreamingHandler): LeafWasmOrgStreamingHandler {
  return async (caller, request, sink) => {
    await handler(parseOrgCaller(caller), request, new OrgSink(sink));
  };
}

/** Adapt a client-streaming handler to `(caller, requests)`. */
export function clientStreamOrgTrampoline(
  handler: OrgClientStreamHandler,
): LeafWasmOrgClientStreamHandler {
  return async (caller, requests) =>
    await handler(parseOrgCaller(caller), new OrgRequests(requests));
}

/** Adapt a duplex handler to `(caller, requests, sink)`. */
export function duplexOrgTrampoline(handler: OrgDuplexHandler): LeafWasmOrgDuplexHandler {
  return async (caller, requests, sink) => {
    await handler(parseOrgCaller(caller), new OrgRequests(requests), new OrgSink(sink));
  };
}

/**
 * One waiter leaves its list the moment its pull settles (BROWSER-4):
 * a resolved pull retains no closure until the terminal.
 */
function spliceWaiter<T>(waiters: T[], waiter: T): void {
  const index = waiters.indexOf(waiter);
  if (index >= 0) waiters.splice(index, 1);
}

/** The boundary's byte item, typed through the §4.3 vocabulary. */
function byteItem(raw: LeafWasmOrgByteItem): ByteItemOut {
  if (!raw.done) return { done: false, value: raw.value };
  if (raw.error !== undefined) return { done: true, error: orgTerminalError(raw.error) };
  // A terminal that carries a final body keeps it (BROWSER-1): the
  // item is `{ done: true, value }`, never a flattened end.
  return raw.value === undefined ? { done: true } : { done: true, value: raw.value };
}

/** The boundary's request item: no error arm, by contract. */
function requestItem(raw: LeafWasmOrgRequestItem): { done: boolean; value?: Uint8Array } {
  return raw.done ? { done: true } : { done: false, value: raw.value };
}

/** One `OrgByteStream.next()` outcome — the contract's inline shape. */
type ByteItemOut = { done: boolean; value?: Uint8Array; error?: OrgStreamError };
