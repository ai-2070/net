# Plan: Credential & Header Forwarding (opt-in, filtered, deny-by-default)

**Companion to:** the MCP bridge plan. Scope: forwarding caller-supplied headers (including bearer tokens) through the mesh to a wrapped or native capability, and the policy machinery to disable or filter that forwarding at every level.

**Posture:** Net's default model is credential locality — secrets live on the machine that owns the tool and never transit. Forwarding inverts that for the cases that need per-caller identity at an upstream service. It is a **tagged concession**, not a headline feature: replayable secrets re-enter transit, so every default is hostile and both ends must opt in.

**Preference order (doctrine):** provider-held credentials > Net delegation/identity > forwarded credentials. Forwarding is the compatibility escape hatch for services that only understand bearer auth.

---

## Doctrine

1. **Off by default, everywhere.** Caller daemons forward nothing; wrappers accept nothing. Forwarding happens only when caller policy allows *sending* AND destination policy allows *accepting*. Deny wins on any mismatch.
2. **The model never sees values.** Secrets live in the daemon secret store; agents reference them by name at most. Header values are injected daemon-side at send time. No tool argument, tool result, config surface, or A2A message ever carries a secret value.
3. **Destination-bound secrets.** A secret ref is authorized for specific providers (node ID / org attestation), specific capabilities, and specific header names. A request to forward it anywhere else fails closed — this is the prompt-injection exfiltration defense, and it is policy-enforced, not convention.
4. **Sealed end-to-end.** Forwarded context is encrypted to the destination node's key specifically (`net.invoke.forwarded_context@1`), not merely hop-encrypted. Relays and intermediate nodes cannot read it under any topology.
5. **Never logged, never stored, never in events.** Values appear in no logs, traces, billing events, or audit records on either side. Audit records the *fact* of forwarding: header names, secret-ref name, destination, decision. Wrappers inject downstream and drop; no persistence.
6. **Forwarded context is authority metadata, never capability input.** It appears in no schemas, examples, tool arguments, transcripts, or result objects. `arguments` = model-visible typed input; `forwarded_context` = daemon-sealed authority. SDKs must never expose headers as "advanced args" — the type separation makes that unrepresentable.
7. **Honest labeling.** Capabilities that accept forwarded credentials carry `accepts_forwarded_credentials` in risk tags; callers see it in describe/pinned descriptions before anything is sent.

## Threat model (honesty section)

Defends against: prompt-injected attempts to send secrets to the wrong provider (destination binding); intermediate relay observation (sealed to destination); replay of captured context (invocation binding); wrappers silently accepting arbitrary headers (accept-lists + auto-tagging); cross-caller leakage in shared providers (isolation test); secret leakage via logs/events/traces/tool args (redaction + sentinel tests).

Does NOT defend against: a destination leaking the header after injection; an upstream service logging `Authorization`; a user deliberately granting a secret to a malicious provider; compromised endpoint machines. Say so in the docs.

**Public naming:** the feature is "Forwarded Invocation Context" in docs (the object carries secret and non-secret headers alike). Internally it stays "credential forwarding" so nobody forgets what it is.

## Object

```
net.invoke.forwarded_context@1
  sealed_to: <destination node id>
  headers: { name: value, ... }     # sealed payload only
  invocation_id
  capability_id
  caller_origin
  header_names                      # declared, matches sealed contents
  issued_at / expires_at            # short TTL, default ~30s
  nonce
```

Versioned, canonicalized, golden-vectored like every other protocol object.

### Canonicalization and binding

Header names are normalized before any policy check; duplicates after normalization are rejected. Security-sensitive headers (`Authorization`, `Cookie`, `Set-Cookie`) are single-value only and never folded — no case games, no duplicate-header smuggling, no folded values.

The sealed payload is bound via AEAD associated data to destination node id, caller origin, capability id, invocation id, accepted header names, and expiry. A captured blob cannot be replayed against another destination, capability, caller, or invocation — and even a perfectly replayed context dies at the TTL. Expiry is the backstop for the day an invocation-id cache misbehaves; sealed bearer material is never valid at rest.

Forwarded values live in secret wrapper types (`ForwardedHeaderValue`: no `Debug` value output, redacted `Display`, no `Serialize`, zeroize-on-drop where practical, explicit expose method callable only at the injection boundary). Values are stripped before any structured error or log object is assembled — generic error serialization can't capture what it can't reach.

## Caller-side policy (daemon)

```yaml
forwarding:
  enabled: false                # global kill switch, default off
  secrets:
    github-token:
      header: Authorization
      allow:
        providers: [<node_id | org:acme>]
        capabilities: ["github.*"]
  plain_headers:                # non-secret headers (trace, tenant ids)
    X-Trace-Id:
      allow: { providers: any }
```

- `net secret set github-token --header Authorization --allow provider:<id>` — values enter the store via CLI/OS keychain, never via agent surfaces.
- Non-secret headers get the same allowlist shape, lighter defaults — but `plain_headers` is not a loophole: tenant/org/user-id headers are sensitive too. Explicit allowlist, destination-bound, size-limited; `providers: any` only for a small vetted built-in set (trace ids), warned otherwise.
- **secret_ref names are user-visible and appear in audit** — convention: never encode values, account names, or sensitive scopes in the name (`prod-stripe-admin` is a leak; audit may show ref-hash + display label).
- Optional `purpose:` field per secret (`purpose: github-api`) — no enforcement, pure audit legibility.
- Blocked always, allowlist or not: hop-by-hop headers (`Connection`, `Proxy-*`, `Transfer-Encoding`), and `Cookie`/`Set-Cookie` unless `--force` (session cookies are ambient authority in its worst form).
- Size and count limits on forwarded headers; oversize fails closed.

## Destination-side policy (wrapper / native capability)

```
net wrap ghapi --accept-forwarded-headers Authorization,X-Tenant-Id -- <cmd>
```

- Default: accept none; unlisted headers are stripped before the downstream call, and the stripping is logged (names only).
- No wildcard. Accepting `Authorization` implies the capability is auto-tagged `accepts_forwarded_credentials`.
- Injection targets: HTTP headers for remote-HTTP wrapped servers and native HTTP-ish capabilities. Optional extension (explicit config only): mapping a forwarded header into a tool-argument template for MCP servers that take tokens as arguments. Never env injection into running stdio processes — per-call env mutation of a shared child process is a cross-caller contamination bug factory.

## Destination processing order (fixed, not adapter discretion)

Accepting forwarded credentials never substitutes for authorization: (1) authenticate the Net caller, (2) authorize the capability invocation under normal policy, (3) verify the requested headers are accepted, (4) only then decrypt `forwarded_context`, (5) verify invocation binding, (6) strip unaccepted headers (log names), (7) inject downstream, (8) drop values. Unauthorized callers never trigger a decrypt.

## Filtering / disabling — the levels, caller and destination symmetric

| Level | Caller | Destination |
|---|---|---|
| Global | `forwarding.enabled: false` (default) | accept-list absent (default) |
| Per-header | secret/header allowlist | `--accept-forwarded-headers` names |
| Per-capability | `capabilities:` glob in secret policy | wrapper config per tool |
| Per-identity | `providers:` binding on the secret | caller identity/scope checks (existing) |

Any level saying no = stripped or refused, fail closed, structured error naming the level that denied (without naming values).

## Tests (extend the existing token-leak fixture)

- Sentinel forwarded end-to-end: grep logs, packet captures, traces, audit, billing on BOTH sides — absent or redacted everywhere.
- Replay matrix: captured `forwarded_context` against a different invocation, capability, destination, or caller → rejected; same context after TTL → rejected; duplicate/case-varied `Authorization` headers → rejected at normalization.
- Exfil: agent requests forwarding an authorized secret to an unauthorized provider → destination-binding denial, audit event emitted.
- Strip: unaccepted header present → downstream call verified clean, strip logged by name.
- Cross-caller: two callers forwarding different tokens to one wrapper → downstream calls verified isolated (no bleed through shared state).
- Descriptor honesty: wrapper with `--accept-forwarded-headers Authorization` → announcement carries `accepts_forwarded_credentials`; wrapper accepting none → no tag. No stealth forwarding surfaces.
- Value-entry rejection: secret-looking values in tool args, capability args, A2A messages, agent-generated config, or CLI ref fields → rejected or redacted; refs hold refs, never values.

## Scope & phasing

- **Phase 0 (now, spec only):** object definition, canonicalization, policy schema, risk tags, test fixture design, the never-for-stdio doctrine. Exists so future bridge work can't smuggle in "just forward Authorization" under pressure.
- **Phase 1:** secret store + audit (`net secret set`, policy parser, `net security audit` surface) — useful independent of forwarding, no forwarding yet.
- **Phase 2:** sealed context + caller injection + destination accept-lists + conformance tests, for native HTTP-facing capabilities.
- **Phase 3:** remote/HTTP MCP wrapping rides on it. Stdio wrapping keeps pure credential locality forever — single-user child processes; forwarding doesn't apply and won't be bolted on.
- **Never:** forwarding of Net identity keys or settlement keys (covered by the key invariants — those are not headers and no mapping may exist), agent-visible secret values, wildcard acceptance.

## Risks

| Risk | Mitigation |
|---|---|
| Replayable secrets re-enter transit | Sealed-to-destination, invocation-bound, both-ends opt-in, tagged capability; doctrine prefers delegation |
| Prompt-injected exfil via forwarding request | Destination-bound secrets; model never supplies values or destinations outside policy |
| Wrapper-side leakage into downstream logs | Out of Net's control past injection — documented honestly; risk tag warns callers |
| Policy sprawl / users flip the global switch and forget | Audit surfaces active forwarding rules; `net security audit` lists every secret→destination grant |
| Cookie/session forwarding requests | Blocked default, `--force` only, separately tagged |
