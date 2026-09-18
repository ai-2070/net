# Connecting browsers: the anchor and the credential

Two browsers meet through an **anchor** — a native node that serves the HTTPS
bootstrap endpoint, answers STUN, and relays for pairs ICE cannot connect. The
anchor is control plane and fallback; the application data path is the
DataChannel. The recipe below is the real one; the flags are the real ones.

## Why the operator flow has this shape

- `anchor serve` is behind the `rtc-bootstrap` feature (which implies `webrtc`);
  `anchor credential mint` is in every build and needs no feature. There is no
  `net` feature on the CLI — the three features are `webrtc`, `rtc-bootstrap`
  and `keychain`. Cargo fingerprints per feature set, so pass
  `--features rtc-bootstrap` to all three commands to reuse one build.
- **`serve` runs before `mint`.** `mint` has to pin the anchor's Noise static
  public key, and the only place that key is printed is `serve`'s own JSON
  report, as `noise_pubkey`.
- The RTC endpoint and the STUN endpoint must be **distinct**: `--rtc-public-addr`
  is the anchor's ICE endpoint, and a browser pairing *with this anchor* cannot
  gather against it. A gateway that lands both on one public tuple is not
  allowed; the driver refuses before binding when the two announced endpoints or
  the two binds collide (port 0 exempt).

## The recipe

```bash
cd net/crates/net

# 0. An issuer identity and a trust-domain PSK, once. `identity generate`
#    prints the identity's public_key_hex — that hex is what
#    --credential-issuer wants in step 1.
cargo run -p net-cli -- identity generate --out issuer.toml
openssl rand -hex 32 > psk.hex

# 1. The anchor. It prints one JSON report — keep its `noise_pubkey` — and
#    then serves until ctrl-c.
cargo run -p net-cli --features rtc-bootstrap -- \
  anchor serve \
  --psk-file psk.hex \
  --listen 0.0.0.0:8443 \
  --url https://<name-on-your-certificate>:8443 \
  --credential-issuer <issuer public_key_hex> \
  --allow-origin http://localhost:8173 \
  --tls-cert cert.pem --tls-key key.pem \
  --rtc-bind 0.0.0.0:4433 \
  --rtc-public-addr <public-ip>:4433 \
  --rtc-stun-bind 0.0.0.0:0

# 2. A bootstrap credential for the browser. Prints `net-bootstrap:…` on
#    stdout (`--out <path>` writes it 0600 instead).
cargo run -p net-cli -- \
  anchor credential mint \
  --root <mesh-root-entity-hex> \
  --issuer-identity issuer.toml \
  --anchor-noise-pubkey <noise_pubkey from step 1> \
  --psk-file psk.hex \
  --url https://<name-on-your-certificate>:8443
```

Declared flags worth knowing: `serve` requires `--psk-file`, `--url`,
`--credential-issuer` and at least one `--allow-origin`; `--bind` defaults to
`0.0.0.0:0` and `--listen` to `0.0.0.0:8443`. `mint` requires `--root`,
`--issuer-identity`, `--anchor-noise-pubkey` and `--url`, and exactly one of
`--psk-hex` / `--psk-file` at run time; `--invite-ttl-secs` defaults to 900 and
`--psk-ttl-secs` to 2592000 (30 days). Run `--help` for the rest.

On Windows without `openssl`, any 64 hex characters in a file will do.

## The rules a browser forces

- **The string is `net-bootstrap:` plus URL-safe unpadded base64**, and the whole
  string (prefix included) goes in the URL query — the body survives a query
  string as-is. It **contains the PSK**: the invite half is single-use, the PSK
  half is standing, which is why there are two lifetimes.
- **Never ship an existing private/native deployment's PSK to public visitors.**
  A public-browser deployment is a deliberately separate transport trust domain
  with its own PSK.
- **`--url` must be browser-fetchable:** `https://`, or `http://` only on
  `localhost`, `127.0.0.1` or `[::1]`. It is validated at **mint** time, so a bad
  URL fails at the CLI rather than in a browser.
- **`--allow-origin` is repeatable and has no wildcard** — an endpoint that takes
  a credential does not get one. The origin the tabs are actually on must be
  named, and at least one is required. Both the CORS list and the trickle
  WebSocket list come from it.
- **TLS has exactly two sources, and self-signed is refused by name:** an
  operator PEM chain via `--tls-cert`/`--tls-key`, or ACME via
  `--acme-directory` with `--acme-email` (HTTP-01 on the same listener). A page
  cannot fetch an endpoint whose certificate it does not trust, and the anchor
  does not pretend otherwise. ACME cannot issue for `localhost`.
- **Both tabs must be on the same origin.** A leaf's identity is scoped to its
  origin; serve them from one origin (the demo's static server sends
  `Cross-Origin-Opener-Policy: same-origin`).

## In the page

```ts
const session = await openSession({
  credentialB64: new URLSearchParams(location.search).get('credential'),
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',   // optional override
});
```

`connect({ credentialB64, bootstrapUrl })` gives this tab its own node;
`openSession` gives the origin's node. See `concepts.md`. Identity is generated
inside the wasm leaf from the platform CSPRNG by default; `entitySecretHex` +
`noiseSecretHex` inject it custodially, and `noiseSecretHex` is only read when
the entity half is also present.

## Default ICE, and the two-endpoint rule

Omitted `iceServers`, the leaf gathers against the `stun_addr` the anchor
announces on `GET /rtc/anchor`. An explicit `iceServers: []` is honoured as
*none*. An entry naming this connection's own peer RTC endpoint rejects with an
`ice-server-conflict` error before any ICE work — a peer cannot be its own STUN
server. `diagnosticStunUrl(rtcAddr)` builds the target for the throwaway
diagnostic probe and is **not** a source of `iceServers`.

## The supported harnesses

The by-hand recipe above needs a browser-trusted certificate, which on a
workstation is the hard part. Two harnesses already do the whole bootstrap for
real (certificate, per-engine trust included) and need none of the flags:

- `net/crates/net/tests/rtc_browser/run.ps1` (`run.sh` on Linux/macOS) — the CI
  gate on Chromium and Firefox. It issues its own CA and a `localhost` leaf, pins
  trust per engine (an SPKI pin for Chromium, the profile's own `cert9.db` for
  Firefox — never the platform store), starts the anchor and the bootstrap
  listeners, serves the page, and prints one `RTCB PASS`/`RTCB FAIL` line per
  witness. There is no `--ignore-certificate-errors` anywhere in it.
- `net/crates/net/examples/browser-demo/run.ps1` (`run.sh`) — the same bootstrap
  end to end for the three-tab direct-path demo, with `-Check` for a headless
  asserting run.

Read `net/crates/net/tests/rtc_browser/runner/src/main.rs` before hand-rolling
the recipe: the certificate step is per engine, and a real DNS name pointed at
the machine (or exactly that private-CA-plus-per-engine-trust machinery) is what
a by-hand `mode=mesh` requires.
