# Identity and Token SDK Consistency Audit

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Audited commit:** `d6d6225fc6e19da66477e900ef04ee59b71fb947`  
**Branch:** `sdk-bugs`  
**Scope:** Identity creation, signing and verification, token issuance, scope projection, revocation generation, parsing, numeric boundaries, and documentation across Rust, Node/TypeScript, Python, Go, and C. Channel-specific delegated-chain reachability and stale token wire-size prose remain in the separate channel audit and are not repeated as findings here.

## Executive summary

The most serious defect is in the core issuance lifecycle: revocation floors are generation-based, but every public issuance path hardcodes generation zero and the documented generation-aware issuer method does not exist. Once an issuer raises its floor above zero, it cannot issue a valid replacement credential without changing identity. Foreign SDKs additionally omit wildcard scope, hide generation when parsing, and expose inconsistent signing verification and numeric/error behavior.

## 1. High — Generation-based revocation is irreversible through every public issuance surface

`PermissionToken` carries `issuer_generation`:

- `net/crates/net/src/adapter/net/identity/token.rs:144-149`

`RevocationRegistry` rejects tokens below the issuer's monotonic floor:

- `net/crates/net/src/adapter/net/identity/token.rs:1026-1071`

Every normal issuance path hardcodes generation zero:

- `net/crates/net/src/adapter/net/identity/token.rs:230-299`, especially `:279-284`

The source comment says callers can reissue with a higher generation through `try_issue_with_generation`, but no such method exists in the repository. Directly mutating the field after issuance would invalidate the signature, so public struct construction is not a viable replacement path.

The Rust SDK routes through the generation-zero issuer:

- `net/crates/net/sdk/src/identity.rs:189-205`

Delegation-chain issuance does the same:

- `net/crates/net/sdk/src/delegation.rs:127-216`

Consequently, after:

```text
revoke_below(issuer, 1)
```

old generation-zero tokens fail as intended, but every newly issued token from that issuer is also generation zero and remains revoked forever. Recovery requires changing identity rather than advancing the issuer generation.

### Required closure

- Add a signed, public generation-aware issuance method.
- Thread current generation through Rust SDK and every foreign binding.
- Define who owns and persists the issuer generation.
- Add a witness that issues generation zero, revokes below one, rejects the old token, issues generation one from the same identity, and accepts the replacement.
- Cover delegated children and restart/persistence behavior.

## 2. Medium — Foreign SDKs omit the core `WILDCARD` permission scope

Rust defines wildcard scope for cross-channel authorization:

- `net/crates/net/src/adapter/net/identity/token.rs:34-44`

Foreign scope converters support only publish, subscribe, admin, and delegate:

- Node/NAPI: `net/crates/net/bindings/node/src/identity.rs:79-113`
- Python/PyO3: `net/crates/net/bindings/python/src/identity.rs:76-109`
- C/Go conversion: `net/crates/net/src/ffi/mesh.rs:2433-2465`
- TypeScript public type: `net/crates/net/sdk-ts/src/identity.ts:42-50`
- Python stub: `net/crates/net/bindings/python/python/net/_net.pyi:1068-1071`

Live Node and Python reproductions rejected issuance with `"wildcard"`. A Rust-created wildcard token can cross the wire, but foreign parse projections omit the wildcard bit and therefore misrepresent the credential's authority.

### Required closure

Expose wildcard consistently in parsers, renderers, public types, stubs, and issuance methods. Add a Rust-issued foreign-parse witness and foreign-issued Rust-verify witness.

## 3. Medium — TypeScript coerces token numeric inputs and misclassifies native token errors

TypeScript exposes unchecked `number` fields:

- `IssueTokenOptions`: `net/crates/net/sdk-ts/src/identity.ts:217-229`
- forwarding: `net/crates/net/sdk-ts/src/identity.ts:293-309`

NAPI expects `u32` TTL and `u8` delegation depth:

- `net/crates/net/bindings/node/src/identity.rs:225-249`

NAPI emits stable `zero_ttl` and `ttl_too_long` kinds:

- `net/crates/net/bindings/node/src/identity.rs:47-60`

The TypeScript union omits those kinds and maps every unknown native kind to `invalid_format`:

- `net/crates/net/sdk-ts/src/identity.ts:74-107`

Boundary reproductions produced:

- `ttlSeconds: 0` → `TokenError.kind === "invalid_format"`, message `token: zero_ttl`;
- `ttlSeconds: 31_536_001` → `invalid_format`, message `token: ttl_too_long`;
- `ttlSeconds: 1.5` → silently truncated to one second;
- `ttlSeconds: 2**32` → wrapped to zero;
- `delegationDepth: 1.5` → silently truncated to one.

Python preserves the precise error kinds and rejects incompatible argument types.

### Required closure

Validate safe integers and explicit ranges in TypeScript before NAPI. Add every native token error kind to `TokenErrorKind`; never remap a recognized native error to `invalid_format`.

## 4. Medium — Only Rust exposes verification for arbitrary signatures produced by `Identity.sign`

Rust exposes public-key verification:

- `net/crates/net/src/adapter/net/identity/entity.rs:66-94`
- SDK `EntityId` re-export: `net/crates/net/sdk/src/identity.rs:54-59`

Foreign SDKs expose signing:

- Node: `net/crates/net/bindings/node/src/identity.rs:210-215`
- Python: `net/crates/net/bindings/python/src/identity.rs:214-217`
- Go: `go/identity.go:203-226`
- C declaration: `go/net.h:512-515`

But no NAPI, PyO3, Go, or C identity surface exposes entity/public-key verification for an arbitrary message signature. Their tests assert signature length rather than a foreign-SDK sign/verify round trip.

### Required closure

Expose detached signature verification with exact key, message, and signature length checks. Add same-language and cross-language vectors, including malformed signatures and wrong-message/wrong-key negatives.

## 5. Medium/Low — Parsed token projections hide `issuer_generation`

Core stores and serializes the field:

- declaration and wire offsets: `net/crates/net/src/adapter/net/identity/token.rs:144-149,610-629`

Foreign parsed projections omit it:

- Node `TokenInfo`: `net/crates/net/bindings/node/src/identity.rs:345-380`
- Python parsed dictionary: `net/crates/net/bindings/python/src/identity.rs:289-306`
- C JSON projection: `net/crates/net/src/ffi/mesh.rs:2795-2842`
- Go `ParsedToken`: `go/identity.go:355-367`

Live TypeScript and Python parsed objects contained no generation field. This prevents operators from explaining why a token is below a revocation floor and compounds the generation-issuance defect.

### Required closure

Include generation in all parsed projections and public models. Add round-trip tests at zero and nonzero generations after generation-aware issuance exists.

## 6. Low — Identity documentation describes a nonce-list revocation model that is not implemented

Public identity documentation says revocation uses a nonce plus a revocation list:

- `web/src/content/docs/concepts/identity.md:38-48`

Current implementation uses per-issuer monotonic generation floors:

- `net/crates/net/src/adapter/net/identity/token.rs:1026-1071`

The nonce distinguishes/replay-separates token instances; it is not the revocation selector.

### Required closure

Document generation floors, propagation/persistence expectations, delegated-child effects, and replacement issuance. Do not describe nonce-list revocation unless such a list is implemented.

## Verification

Evidence collected against the audited commit included:

- focused Rust identity tests: **4 passed**;
- focused delegation/revocation witness from the audit lane: passed;
- TypeScript identity suite: **14 passed**;
- Python identity suite: **23 passed**;
- live Node/Python boundary reproductions for wildcard rejection, omitted generation, TypeScript coercion, and error-kind collapse.

An independent attempt to rerun the named delegation test with a crate-unit filter selected zero tests because that witness is not in the filtered unit target; it is therefore not counted as a second execution.

### Limitations

- Go runtime tests were unavailable with `CGO_ENABLED=0`; Go/C conclusions are source-verified.
- Core supports public-only `EntityKeypair` handles and fallible signing, but ordinary foreign identity constructors create full keypairs and do not expose public-only handles. No additional foreign public-only signing defect was classified.

## Conclusion

Revocation cannot be considered operational until the same issuer can mint a higher-generation replacement credential. The binding spine should then project complete scope and generation data, validate numeric inputs before native coercion, and provide the verification half of its detached-signature API.
