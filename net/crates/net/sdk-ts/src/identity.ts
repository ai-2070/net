/**
 * Identity + Token surface — ed25519 keypair, signed permission
 * tokens, and a local token cache. Pure compute; the network-side
 * wiring (mesh auth on `subscribeChannel`) lands in a later stage.
 *
 * @example
 * ```ts
 * import { Identity, TokenScope } from '@ai2070/net-sdk';
 *
 * // Generate once, persist the bytes, reload on subsequent runs.
 * const id = Identity.generate();
 * const seed = id.toBytes();
 * const reloaded = Identity.fromBytes(seed);
 *
 * // Issue a subscribe grant.
 * const subscriber = Identity.generate();
 * const token = id.issueToken({
 *   subject: subscriber.entityId,
 *   scope: ['subscribe'],
 *   channel: 'sensors/temp',
 *   ttlSeconds: 300,
 * });
 *
 * // Subscriber installs; signature is verified at install time.
 * subscriber.installToken(token);
 * ```
 */

import {
  Identity as NapiIdentity,
  channelHash as napiChannelHash,
  delegateToken as napiDelegateToken,
  parseToken as napiParseToken,
  tokenIsExpired as napiTokenIsExpired,
  verifyToken as napiVerifyToken,
} from '@ai2070/net';

// ----------------------------------------------------------------------------
// Scope — string-array alias with a fixed set of values.
// ----------------------------------------------------------------------------

/** Discrete permissions a token can authorize. */
export type TokenScope = 'publish' | 'subscribe' | 'admin' | 'delegate';

const VALID_SCOPES: ReadonlySet<TokenScope> = new Set([
  'publish',
  'subscribe',
  'admin',
  'delegate',
]);

// ----------------------------------------------------------------------------
// Errors — prefix dispatch mirrors the mesh / channel pattern.
// ----------------------------------------------------------------------------

/**
 * Base class for identity-layer errors (malformed seed / subject /
 * channel name). Not thrown for token-validity issues — those are
 * `TokenError`.
 */
export class IdentityError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'IdentityError';
    Object.setPrototypeOf(this, IdentityError.prototype);
  }
}

/**
 * Token-layer error. `kind` is a stable discriminator for programmatic
 * dispatch (`if (e.kind === 'expired')`) that doesn't rely on the
 * message string.
 */
export type TokenErrorKind =
  | 'invalid_signature'
  | 'not_yet_valid'
  | 'expired'
  | 'delegation_exhausted'
  | 'delegation_not_allowed'
  | 'not_authorized'
  | 'invalid_format';

export class TokenError extends Error {
  readonly kind: TokenErrorKind;
  constructor(kind: TokenErrorKind, message?: string) {
    super(message ?? `token ${kind.replace(/_/g, ' ')}`);
    this.name = 'TokenError';
    this.kind = kind;
    Object.setPrototypeOf(this, TokenError.prototype);
  }
}

function toIdentityError(e: unknown): never {
  const msg = (e as Error | undefined)?.message ?? String(e);
  if (msg.startsWith('token:')) {
    const kind = msg.slice('token:'.length).trim() as TokenErrorKind;
    // Unknown kind → treat as invalid_format defensively so the type is sound.
    const known: ReadonlySet<string> = new Set<TokenErrorKind>([
      'invalid_signature',
      'not_yet_valid',
      'expired',
      'delegation_exhausted',
      'delegation_not_allowed',
      'not_authorized',
      'invalid_format',
    ]);
    throw new TokenError(known.has(kind) ? kind : 'invalid_format', msg);
  }
  if (msg.startsWith('identity:')) {
    throw new IdentityError(msg.slice('identity:'.length).trim());
  }
  throw e;
}

function runMapped<T>(fn: () => T): T {
  try {
    return fn();
  } catch (e) {
    toIdentityError(e);
  }
}

// ----------------------------------------------------------------------------
// Token — parsed, typed view over a serialized PermissionToken buffer.
// ----------------------------------------------------------------------------

/**
 * A signed, delegatable permission token. Construct via
 * `Identity.issueToken` (locally) or `Token.parse` (from wire bytes).
 *
 * The wire form is 161 bytes: issuer (32) + subject (32) + scope (4)
 * + channel hash (4, canonical 32-bit) + not-before (8) + not-after
 * (8) + delegation depth (1) + nonce (8) + ed25519 signature (64).
 */
export class Token {
  /** Raw serialized bytes. Safe to send over the wire. */
  readonly bytes: Buffer;
  readonly issuer: Buffer;
  readonly subject: Buffer;
  readonly scope: ReadonlySet<TokenScope>;
  /**
   * Canonical 64-bit hash of the channel name this token authorizes
   * (combine with `wildcard` scope for cross-channel grants). Compare
   * against `channelHash(name)` to check whether a token applies to a
   * named channel.
   */
  readonly channelHash: bigint;
  readonly notBefore: Date;
  readonly notAfter: Date;
  readonly delegationDepth: number;
  readonly nonce: bigint;

  private constructor(bytes: Buffer, info: TokenFields) {
    this.bytes = bytes;
    this.issuer = info.issuer;
    this.subject = info.subject;
    this.scope = info.scope;
    this.channelHash = info.channelHash;
    this.notBefore = info.notBefore;
    this.notAfter = info.notAfter;
    this.delegationDepth = info.delegationDepth;
    this.nonce = info.nonce;
  }

  /** Parse a serialized token. Throws `TokenError { kind: 'invalid_format' }` on bad bytes. */
  static parse(bytes: Buffer): Token {
    return new Token(bytes, parseTokenBytes(bytes));
  }

  /** Verify the ed25519 signature. Does NOT check time bounds — use `isExpired()` for that. */
  verify(): boolean {
    return runMapped(() => napiVerifyToken(this.bytes));
  }

  /** `true` if the token's `notAfter` has passed. */
  isExpired(): boolean {
    return runMapped(() => napiTokenIsExpired(this.bytes));
  }

  /** Same instance — provided so callers can treat `Token` as a union with the serialized buffer. */
  toBuffer(): Buffer {
    return this.bytes;
  }
}

interface TokenFields {
  issuer: Buffer;
  subject: Buffer;
  scope: ReadonlySet<TokenScope>;
  channelHash: bigint;
  notBefore: Date;
  notAfter: Date;
  delegationDepth: number;
  nonce: bigint;
}

function parseTokenBytes(bytes: Buffer): TokenFields {
  const info = runMapped(() => napiParseToken(bytes));
  return {
    issuer: info.issuer,
    subject: info.subject,
    scope: new Set(info.scope as TokenScope[]),
    channelHash: info.channelHash,
    notBefore: new Date(Number(info.notBefore) * 1000),
    notAfter: new Date(Number(info.notAfter) * 1000),
    delegationDepth: info.delegationDepth,
    nonce: info.nonce,
  };
}

// ----------------------------------------------------------------------------
// Identity — generate / persist / sign / issue / install / lookup.
// ----------------------------------------------------------------------------

/** Options for `Identity.issueToken`. */
export interface IssueTokenOptions {
  /** 32-byte entity id of the grantee. */
  subject: Buffer;
  /** Scopes granted; union of `'publish' | 'subscribe' | 'admin' | 'delegate'`. */
  scope: readonly TokenScope[];
  /** Channel name. Hashed to u64 canonical; the wire-side fast-path
   *  hint is the low 16 bits of that hash. */
  channel: string;
  /** Validity window in seconds. Pick a short TTL + re-issue instead of building a revocation list. */
  ttlSeconds: number;
  /** How many times the grantee can re-delegate. Default 0 (forbidden). */
  delegationDepth?: number;
}

/**
 * Caller-owned identity bundle. Cheap to carry around; token cache
 * entries are runtime-only and don't serialize.
 */
export class Identity {
  private readonly inner: NapiIdentity;

  private constructor(inner: NapiIdentity) {
    this.inner = inner;
  }

  /** Generate a fresh ed25519 identity. Persist via `toBytes()` to keep `nodeId` stable across restarts. */
  static generate(): Identity {
    return new Identity(NapiIdentity.generate());
  }

  /** Load from a caller-owned 32-byte ed25519 seed. */
  static fromSeed(seed: Buffer): Identity {
    return new Identity(runMapped(() => NapiIdentity.fromSeed(seed)));
  }

  /** Alias for `fromSeed` — the persisted form IS the 32-byte seed. */
  static fromBytes(bytes: Buffer): Identity {
    return new Identity(runMapped(() => NapiIdentity.fromBytes(bytes)));
  }

  /** Wrap an existing NAPI `Identity` handle — escape hatch for builder code. */
  static fromNapi(inner: NapiIdentity): Identity {
    return new Identity(inner);
  }

  /** The underlying NAPI handle. Used by the mesh builder to bind this identity to a `MeshNode`. */
  toNapi(): NapiIdentity {
    return this.inner;
  }

  /** Serialize as the 32-byte ed25519 seed. Treat as secret material. */
  toBytes(): Buffer {
    return this.inner.toBytes();
  }

  /** Ed25519 public key. 32 bytes. */
  get entityId(): Buffer {
    return this.inner.entityId;
  }

  /** Derived 64-bit origin hash used in packet headers. */
  get originHash(): bigint {
    return this.inner.originHash;
  }

  /** Derived 64-bit node id used for routing / addressing. */
  get nodeId(): bigint {
    return this.inner.nodeId;
  }

  /** Sign arbitrary bytes. Returns 64-byte ed25519 signature. */
  sign(message: Buffer): Buffer {
    return this.inner.sign(message);
  }

  /** Issue a scoped token to another entity. */
  issueToken(opts: IssueTokenOptions): Token {
    for (const s of opts.scope) {
      if (!VALID_SCOPES.has(s)) {
        throw new IdentityError(`unknown scope ${JSON.stringify(s)}`);
      }
    }
    const bytes = runMapped(() =>
      this.inner.issueToken(
        opts.subject,
        Array.from(opts.scope),
        opts.channel,
        opts.ttlSeconds,
        opts.delegationDepth ?? 0,
      ),
    );
    return Token.parse(bytes);
  }

  /** Install a token this node received. Throws `TokenError` on bad signature or malformed bytes. */
  installToken(token: Token | Buffer): void {
    const bytes = token instanceof Token ? token.bytes : token;
    runMapped(() => this.inner.installToken(bytes));
  }

  /** Look up a cached token. Returns `null` when no exact-channel entry is cached. */
  lookupToken(subject: Buffer, channel: string): Token | null {
    const bytes = runMapped(() => this.inner.lookupToken(subject, channel));
    return bytes == null ? null : Token.parse(bytes);
  }

  /** Number of cached tokens. Testing aid. */
  get tokenCacheLen(): number {
    return this.inner.tokenCacheLen;
  }
}

// ----------------------------------------------------------------------------
// Free-function helpers.
// ----------------------------------------------------------------------------

/**
 * Hash a channel name to its canonical 64-bit substrate identifier.
 * Compare against `token.channelHash` to check whether a token
 * applies to a named channel without trial-decoding. The per-packet
 * wire `NetHeader` fast-path hint is the low 16 bits of this value.
 */
export function channelHash(channel: string): bigint {
  return runMapped(() => napiChannelHash(channel));
}

/**
 * Delegate a token to a new subject. The parent token must include
 * `'delegate'` in its scope and have `delegationDepth > 0`; the
 * signer must be the subject of the parent token. `restrictedScope`
 * is intersected with the parent's scope.
 */
export function delegateToken(
  signer: Identity,
  parent: Token | Buffer,
  newSubject: Buffer,
  restrictedScope: readonly TokenScope[],
): Token {
  const parentBytes = parent instanceof Token ? parent.bytes : parent;
  const childBytes = runMapped(() =>
    napiDelegateToken(
      signer.toNapi(),
      parentBytes,
      newSubject,
      Array.from(restrictedScope),
    ),
  );
  return Token.parse(childBytes);
}
