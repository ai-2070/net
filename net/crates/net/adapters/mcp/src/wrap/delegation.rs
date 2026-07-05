//! Delegated-invoke verification gate (`HERMES_INTEGRATION_PLAN.md` Phase 3,
//! Slice B).
//!
//! A wrapped tool's owner-scope gate checks the AEAD-verified `caller_origin`
//! — but `origin_hash` is **spoofable inside a channel** (`identity/origin.rs`:
//! any peer admitted to the channel can mint packets under an arbitrary
//! origin). So an invoke that wants *same-root delegation* instead carries, in
//! request headers:
//!
//!   * [`HDR_DELEGATION`]     — a serialized [`DelegationChain`] (`root → … → leaf`)
//!   * [`HDR_DELEGATION_SIG`] — a fresh per-invoke signed envelope by the **leaf's** private key
//!
//! and this gate verifies, **fail-closed**: the chain roots at the owner root
//! the provider trusts + is unrevoked + valid, **and** the envelope signature
//! verifies against the chain's leaf over a request-binding challenge, within a
//! fresh time window and with a non-replayed nonce. The signature (not the
//! spoofable origin) is what proves the caller holds the leaf key — so a
//! captured chain can't be replayed by a member spoofing the leaf's origin.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use net_sdk::delegation::{DelegationChain, RevocationRegistry};
use net_sdk::identity::EntityId;
use net_sdk::revocation::RevocationStore;
use net_sdk::Identity;

/// Request header carrying the serialized [`DelegationChain`] (`root → leaf`).
pub const HDR_DELEGATION: &str = "net-delegation";
/// Request header carrying the per-invoke signed envelope (ts + nonce + sig).
pub const HDR_DELEGATION_SIG: &str = "net-delegation-sig";

/// Domain separator for the per-invoke challenge — versioned so a future
/// change to the signed layout is a distinct namespace, not a silent
/// cross-version collision.
const CHALLENGE_DOMAIN: &[u8] = b"net-mcp-invoke-v1";

/// Signed-envelope length: `u64` timestamp + `u64` nonce + 64-byte signature.
const ENVELOPE_LEN: usize = 8 + 8 + 64;

/// Hard cap on the replay-nonce cache. Only leaf-signed invokes ever reach the
/// cache (the signature is checked first), so this bounds memory against a
/// *compromised leaf* flooding distinct nonces — not an unauthenticated peer.
const MAX_NONCES: usize = 100_000;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Why a delegated invoke was rejected. All map to a single wire error code;
/// the `Display` is log / diagnostic detail (it never carries key material).
#[derive(Debug)]
pub enum DelegationReject {
    /// The `net-delegation` header wasn't a decodable chain.
    MalformedChain,
    /// The `net-delegation-sig` envelope was the wrong length.
    MalformedEnvelope,
    /// The signature timestamp was outside `now ± window`.
    TimestampOutOfWindow,
    /// The signature didn't verify against the chain's leaf.
    BadSignature,
    /// This nonce was already seen (replay), or the nonce cache is saturated.
    Replay,
    /// The chain itself failed to verify (root anchor / revocation / validity /
    /// continuity / scope); carries the underlying token-error kind.
    Chain(String),
}

impl std::fmt::Display for DelegationReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedChain => write!(f, "malformed delegation chain"),
            Self::MalformedEnvelope => write!(f, "malformed delegation signature envelope"),
            Self::TimestampOutOfWindow => {
                write!(
                    f,
                    "delegation signature timestamp outside the accepted window"
                )
            }
            Self::BadSignature => {
                write!(
                    f,
                    "delegation signature does not verify against the chain leaf"
                )
            }
            Self::Replay => write!(f, "delegation nonce already seen (replay)"),
            Self::Chain(k) => write!(f, "delegation chain does not verify: {k}"),
        }
    }
}
impl std::error::Error for DelegationReject {}

/// An admitted delegated invoke — handed to the audit sink so a provider can
/// record *which* delegated agent (`leaf`) invoked under *which* `root`.
#[derive(Debug, Clone)]
pub struct DelegationAudit {
    /// The wrapped tool that was invoked.
    pub tool: String,
    /// The terminal (leaf) agent identity the chain attributes to — the
    /// gateway, or a subagent.
    pub leaf: EntityId,
    /// The user-root the chain is anchored at (the provider's configured owner).
    pub root: EntityId,
}

/// Sink invoked once per **admitted** delegated invoke (log / audit-chain).
pub type AuditSink = Arc<dyn Fn(&DelegationAudit) + Send + Sync>;

/// Verifies delegated invokes for one provider: the owner root it trusts, a
/// shared revocation registry, clock tolerances, and a replay-nonce cache.
pub struct DelegationGate {
    owner_root: EntityId,
    revocation: Arc<RevocationRegistry>,
    /// Clock-skew tolerance for the CHAIN's token time bounds.
    skew_secs: u64,
    /// Half-width of the accepted window for the per-invoke signature
    /// timestamp (reject if `|now - ts| > window`).
    window_secs: u64,
    /// `(leaf entity-id, nonce) → expiry-secs`. Keyed by leaf so two distinct
    /// authenticated delegates can't collide on nonce values (relevant on the
    /// `getrandom`-failure seed fallback, where same-second signers can emit
    /// identical nonce sequences). Only authenticated invokes are recorded.
    nonces: Mutex<HashMap<([u8; 32], u64), u64>>,
    audit: Option<AuditSink>,
    /// Optional machine-shared revocation store. When set, each verify reloads
    /// its floors into `revocation` (cheap JSON read) *before* the chain check,
    /// so an operator's `RevocationStore::revoke_below` reaches a running
    /// provider without a restart. Floors are monotonic, so a transient read
    /// error just keeps the last-known floors (logged, never fail-open into
    /// *granting* — the chain must still root-anchor + verify).
    revocation_store: Option<PathBuf>,
}

impl std::fmt::Debug for DelegationGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationGate")
            .field("owner_root", &self.owner_root)
            .field("skew_secs", &self.skew_secs)
            .field("window_secs", &self.window_secs)
            .field("audit", &self.audit.is_some())
            .field("revocation_store", &self.revocation_store)
            .finish()
    }
}

impl DelegationGate {
    /// A gate anchoring at `owner_root`, honoring `revocation`. Defaults: 60s
    /// signature window, 5s chain skew, no audit sink.
    pub fn new(owner_root: EntityId, revocation: Arc<RevocationRegistry>) -> Self {
        Self {
            owner_root,
            revocation,
            skew_secs: 5,
            window_secs: 60,
            nonces: Mutex::new(HashMap::new()),
            audit: None,
            revocation_store: None,
        }
    }

    /// Honor a machine-shared [`RevocationStore`] at `path`: each verify reloads
    /// its floors first, so an operator's revocation of a delegated gateway
    /// reaches this running provider without a restart.
    pub fn with_revocation_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.revocation_store = Some(path.into());
        self
    }

    /// Override the per-invoke signature window (seconds).
    pub fn with_window_secs(mut self, window_secs: u64) -> Self {
        self.window_secs = window_secs;
        self
    }

    /// Override the chain time-bound clock-skew tolerance (seconds).
    pub fn with_skew_secs(mut self, skew_secs: u64) -> Self {
        self.skew_secs = skew_secs;
        self
    }

    /// Install an audit sink invoked once per **admitted** delegated invoke.
    pub fn with_audit(mut self, audit: AuditSink) -> Self {
        self.audit = Some(audit);
        self
    }

    /// The owner root this gate anchors chains at.
    pub fn owner_root(&self) -> &EntityId {
        &self.owner_root
    }

    /// Verify a delegated invoke of `tool` with `args_body`, carrying
    /// `chain_bytes` ([`HDR_DELEGATION`]) + `sig_env` ([`HDR_DELEGATION_SIG`]).
    /// Returns the admitted leaf on success; **every** failure path returns a
    /// typed rejection (fail-closed — no path falls through to admit).
    pub fn verify(
        &self,
        tool: &str,
        args_body: &[u8],
        chain_bytes: &[u8],
        sig_env: &[u8],
    ) -> Result<EntityId, DelegationReject> {
        // [0] Parse both inputs before any crypto.
        let chain = DelegationChain::from_bytes(chain_bytes)
            .map_err(|_| DelegationReject::MalformedChain)?;
        if sig_env.len() != ENVELOPE_LEN {
            return Err(DelegationReject::MalformedEnvelope);
        }
        // Length is already checked above, so these slices are exactly 8 bytes;
        // still parse fallibly (no panic) to keep the gate strictly fail-closed.
        let ts = u64::from_le_bytes(
            sig_env[0..8]
                .try_into()
                .map_err(|_| DelegationReject::MalformedEnvelope)?,
        );
        let nonce = u64::from_le_bytes(
            sig_env[8..16]
                .try_into()
                .map_err(|_| DelegationReject::MalformedEnvelope)?,
        );
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_env[16..80]);

        // [1] Freshness — cheap reject of a stale / future envelope before the
        //     ed25519 verify.
        let now = now_secs();
        let low = now.saturating_sub(self.window_secs);
        let high = now.saturating_add(self.window_secs);
        if ts < low || ts > high {
            return Err(DelegationReject::TimestampOutOfWindow);
        }

        // [2] Prove the caller holds the leaf's key: verify the per-invoke
        //     signature over a request-binding challenge. This is the binding
        //     the spoofable origin can't provide.
        let leaf = chain.leaf();
        let challenge = build_challenge(tool, args_body, ts, nonce);
        leaf.verify_bytes(&challenge, &sig)
            .map_err(|_| DelegationReject::BadSignature)?;

        // [3] The chain must root at the owner and be unrevoked / valid. Reload
        //     the machine-shared revocation floors first (if configured) so an
        //     operator's just-written revocation takes effect on this running
        //     provider. The presenter is the leaf itself (step [2] already bound
        //     the caller to the leaf), so `verify` enforces root-anchor +
        //     continuity + scope + revocation + time bounds.
        self.refresh_revocations();
        chain
            .verify(&leaf, &self.owner_root, &self.revocation, self.skew_secs)
            .map_err(|e| DelegationReject::Chain(format!("{e:?}")))?;

        // [4] Replay: only now, after authentication, touch the nonce cache —
        //     so an unauthenticated peer can't grow it. Keyed by the leaf, so
        //     one delegate's nonces can't false-replay another's. Check + insert
        //     atomic.
        self.check_and_record_nonce(&leaf, nonce, now)?;

        // [5] Audit the admitted leaf / root.
        if let Some(audit) = &self.audit {
            audit(&DelegationAudit {
                tool: tool.to_string(),
                leaf: leaf.clone(),
                root: self.owner_root.clone(),
            });
        }
        Ok(leaf)
    }

    /// Reload the shared revocation floors into `revocation` (monotonic). A
    /// read/parse error keeps the last-known floors and logs — never opens a
    /// hole, since the chain must still root-anchor + verify regardless.
    fn refresh_revocations(&self) {
        if let Some(path) = &self.revocation_store {
            match RevocationStore::load(path) {
                Ok(store) => store.apply_to(&self.revocation),
                Err(e) => eprintln!(
                    "net wrap: revocation store unreadable ({e}); keeping last-known floors"
                ),
            }
        }
    }

    fn check_and_record_nonce(
        &self,
        leaf: &EntityId,
        nonce: u64,
        now: u64,
    ) -> Result<(), DelegationReject> {
        let expiry = now.saturating_add(self.window_secs.saturating_mul(2));
        let mut leaf_key = [0u8; 32];
        leaf_key.copy_from_slice(leaf.as_bytes());
        let key = (leaf_key, nonce);
        let mut cache = self.nonces.lock();
        // Prune expired first so the cap reflects live entries.
        cache.retain(|_, &mut exp| exp > now);
        if cache.contains_key(&key) {
            return Err(DelegationReject::Replay);
        }
        if cache.len() >= MAX_NONCES {
            // Fail closed rather than grow unbounded. A legitimate leaf never
            // hits this; only a flood would, and dropping it is the safe move.
            return Err(DelegationReject::Replay);
        }
        cache.insert(key, expiry);
        Ok(())
    }
}

/// Build the canonical, domain-separated, length-prefixed challenge the leaf
/// signs and the gate reconstructs. Length prefixes make the framing
/// unambiguous — no field-boundary confusion between `tool` and `args`.
pub fn build_challenge(tool: &str, args_body: &[u8], ts: u64, nonce: u64) -> Vec<u8> {
    let mut msg =
        Vec::with_capacity(CHALLENGE_DOMAIN.len() + 4 + tool.len() + 4 + args_body.len() + 16);
    msg.extend_from_slice(CHALLENGE_DOMAIN);
    msg.extend_from_slice(&(tool.len() as u32).to_le_bytes());
    msg.extend_from_slice(tool.as_bytes());
    msg.extend_from_slice(&(args_body.len() as u32).to_le_bytes());
    msg.extend_from_slice(args_body);
    msg.extend_from_slice(&ts.to_le_bytes());
    msg.extend_from_slice(&nonce.to_le_bytes());
    msg
}

/// Assemble the signed-envelope bytes (the [`HDR_DELEGATION_SIG`] value) from
/// its parts: the caller signs [`build_challenge`] with the leaf key and passes
/// the 64-byte signature here. Exposed so the caller side (and tests) build the
/// exact envelope [`DelegationGate::verify`] parses.
pub fn build_envelope(ts: u64, nonce: u64, sig: &[u8; 64]) -> Vec<u8> {
    let mut env = Vec::with_capacity(ENVELOPE_LEN);
    env.extend_from_slice(&ts.to_le_bytes());
    env.extend_from_slice(&nonce.to_le_bytes());
    env.extend_from_slice(sig);
    env
}

/// Caller-side counterpart to [`DelegationGate`]: holds the leaf identity (its
/// signing key) plus the serialized chain, and mints the two request headers
/// ([`HDR_DELEGATION`] + a fresh [`HDR_DELEGATION_SIG`]) for each invoke.
///
/// A caller (the demand-side gateway) attaches [`Self::headers`] to every
/// invoke; the provider's [`DelegationGate`] verifies them. Each call mints a
/// fresh timestamp + nonce + signature, so the provider's replay guard admits
/// every distinct attempt (and a retry after a lost reply is a *new* nonce, not
/// a replay).
pub struct DelegationSigner {
    leaf: Identity,
    chain_bytes: Vec<u8>,
    /// Monotone nonce source. Seeded randomly so two signers (distinct leaves)
    /// hitting the same provider don't emit colliding nonce sequences — the
    /// provider's cache keys on the nonce value regardless of leaf.
    nonce: AtomicU64,
}

impl DelegationSigner {
    /// Build a signer from the leaf `Identity` (must own its signing key) and a
    /// serialized [`DelegationChain`] whose leaf is that identity.
    pub fn new(leaf: Identity, chain_bytes: Vec<u8>) -> Self {
        // Random seed so distinct signers don't share a nonce sequence; a
        // clock-derived fallback keeps this infallible if the RNG is
        // unavailable.
        let mut seed = [0u8; 8];
        let base = match getrandom::fill(&mut seed) {
            Ok(()) => u64::from_le_bytes(seed),
            Err(_) => now_secs().wrapping_mul(2_654_435_761),
        };
        Self {
            leaf,
            chain_bytes,
            nonce: AtomicU64::new(base),
        }
    }

    /// The `(net-delegation, net-delegation-sig)` headers for invoking `service`
    /// with request `body`. Signs [`build_challenge`] with the leaf key over a
    /// fresh timestamp + nonce.
    pub fn headers(&self, service: &str, body: &[u8]) -> Vec<(String, Vec<u8>)> {
        let ts = now_secs();
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let sig = self.leaf.sign(&build_challenge(service, body, ts, nonce));
        vec![
            (HDR_DELEGATION.to_string(), self.chain_bytes.clone()),
            (
                HDR_DELEGATION_SIG.to_string(),
                build_envelope(ts, nonce, &sig),
            ),
        ]
    }
}

impl std::fmt::Debug for DelegationSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationSigner")
            .field("leaf", self.leaf.entity_id())
            .field("chain_len", &self.chain_bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use net_sdk::delegation::{derive_child_seed, DEFAULT_DELEGATION_DEPTH};
    use net_sdk::Identity;

    const TOOL: &str = "github/create_issue";
    const ARGS: &[u8] = br#"{"title":"hi"}"#;

    /// Root + machine + gateway(leaf), derived exactly as a deployment would.
    fn identities() -> (Identity, Identity, Identity) {
        let root = Identity::generate();
        let seed = root.to_bytes();
        let machine = Identity::from_seed(derive_child_seed(&seed, "machine:h"));
        let gateway = Identity::from_seed(derive_child_seed(&seed, "gateway:h:hermes"));
        (root, machine, gateway)
    }

    fn chain_for(root: &Identity, machine: &Identity, leaf: &Identity) -> DelegationChain {
        DelegationChain::derive_gateway(
            root,
            machine,
            leaf.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap()
    }

    /// Sign a fresh envelope for (tool, args) with the leaf key at `ts`/`nonce`.
    fn envelope(leaf: &Identity, tool: &str, args: &[u8], ts: u64, nonce: u64) -> Vec<u8> {
        let sig = leaf.sign(&build_challenge(tool, args, ts, nonce));
        build_envelope(ts, nonce, &sig)
    }

    fn gate(owner_root: &Identity, reg: Arc<RevocationRegistry>) -> DelegationGate {
        DelegationGate::new(owner_root.entity_id().clone(), reg)
    }

    #[test]
    fn valid_delegated_invoke_admits_the_leaf() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        let leaf = g
            .verify(TOOL, ARGS, &chain, &env)
            .expect("valid invoke admits");
        assert_eq!(&leaf, gateway.entity_id());
    }

    #[test]
    fn a_chain_rooted_elsewhere_is_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        // Gate anchored at a DIFFERENT owner root.
        let stranger = Identity::generate();
        let g = gate(&stranger, Arc::new(RevocationRegistry::new()));
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, &env),
            Err(DelegationReject::Chain(_))
        ));
    }

    #[test]
    fn a_revoked_gateway_is_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let reg = Arc::new(RevocationRegistry::new());
        let g = gate(&root, reg.clone());
        // Revoke this machine's gateway delegation.
        reg.revoke_below(machine.entity_id(), 1);
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, &env),
            Err(DelegationReject::Chain(_))
        ));
    }

    #[test]
    fn a_stale_timestamp_is_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let g = gate(&root, Arc::new(RevocationRegistry::new())).with_window_secs(30);
        let stale = now_secs().saturating_sub(120); // well past the 30s window
        let env = envelope(&gateway, TOOL, ARGS, stale, 1);
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, &env),
            Err(DelegationReject::TimestampOutOfWindow)
        ));
    }

    #[test]
    fn a_replayed_nonce_is_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        let ts = now_secs();
        let env = envelope(&gateway, TOOL, ARGS, ts, 7);
        assert!(
            g.verify(TOOL, ARGS, &chain, &env).is_ok(),
            "first use admits"
        );
        assert!(
            matches!(
                g.verify(TOOL, ARGS, &chain, &env),
                Err(DelegationReject::Replay)
            ),
            "replaying the same (ts, nonce, sig) is rejected"
        );
    }

    #[test]
    fn two_delegates_with_the_same_nonce_do_not_false_replay() {
        // Two distinct gateways under the same root sign the SAME (ts, nonce).
        // Keyed by leaf, neither is rejected as the other's replay — but each
        // still can't replay its OWN nonce.
        let root = Identity::generate();
        let seed = root.to_bytes();
        let machine = Identity::from_seed(derive_child_seed(&seed, "machine:h"));
        let g1 = Identity::from_seed(derive_child_seed(&seed, "gateway:h:one"));
        let g2 = Identity::from_seed(derive_child_seed(&seed, "gateway:h:two"));
        let chain1 = DelegationChain::derive_gateway(
            &root,
            &machine,
            g1.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap()
        .to_bytes();
        let chain2 = DelegationChain::derive_gateway(
            &root,
            &machine,
            g2.entity_id(),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .unwrap()
        .to_bytes();

        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        let ts = now_secs();
        let nonce = 42;
        assert!(g
            .verify(TOOL, ARGS, &chain1, &envelope(&g1, TOOL, ARGS, ts, nonce))
            .is_ok());
        // Same nonce, different leaf — NOT a replay of g1.
        assert!(g
            .verify(TOOL, ARGS, &chain2, &envelope(&g2, TOOL, ARGS, ts, nonce))
            .is_ok());
        // g1 replaying its own (leaf, nonce) is still rejected.
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain1, &envelope(&g1, TOOL, ARGS, ts, nonce)),
            Err(DelegationReject::Replay)
        ));
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let mut env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        let last = env.len() - 1;
        env[last] ^= 0x01; // flip a signature bit
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, &env),
            Err(DelegationReject::BadSignature)
        ));
    }

    #[test]
    fn arguments_are_bound_by_the_signature() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        // Sign over ARGS, but verify against different args → signature fails.
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        assert!(matches!(
            g.verify(TOOL, br#"{"title":"tampered"}"#, &chain, &env),
            Err(DelegationReject::BadSignature)
        ));
    }

    #[test]
    fn the_tool_name_is_bound_by_the_signature() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        assert!(matches!(
            g.verify("github/delete_repo", ARGS, &chain, &env),
            Err(DelegationReject::BadSignature)
        ));
    }

    #[test]
    fn a_non_leaf_signer_is_rejected() {
        // A member who holds the chain but NOT the leaf key can't forge the
        // per-invoke signature — this is the origin-spoofing defense.
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let impostor = Identity::generate();
        let env = envelope(&impostor, TOOL, ARGS, now_secs(), 1);
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, &env),
            Err(DelegationReject::BadSignature)
        ));
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let g = gate(&root, Arc::new(RevocationRegistry::new()));
        // Garbage chain.
        assert!(matches!(
            g.verify(
                TOOL,
                ARGS,
                b"not-a-chain",
                &envelope(&gateway, TOOL, ARGS, now_secs(), 1)
            ),
            Err(DelegationReject::MalformedChain)
        ));
        // Wrong-length envelope.
        assert!(matches!(
            g.verify(TOOL, ARGS, &chain, b"short"),
            Err(DelegationReject::MalformedEnvelope)
        ));
    }

    #[test]
    fn a_store_revocation_propagates_to_a_running_gate() {
        // The provider side of revocation: an operator revokes a delegated
        // gateway in the machine-shared store, and the running gate rejects the
        // next invoke — no restart.
        use net_sdk::revocation::RevocationStore;

        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.json");

        let g = DelegationGate::new(
            root.entity_id().clone(),
            Arc::new(RevocationRegistry::new()),
        )
        .with_revocation_store(&path);

        // Admits before any revocation.
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        assert!(
            g.verify(TOOL, ARGS, &chain, &env).is_ok(),
            "admits before revocation"
        );

        // An operator revokes this machine's gateway delegation in the store.
        RevocationStore::revoke_below(&path, machine.entity_id(), 1).unwrap();

        // The next invoke reloads the store and is rejected.
        let env2 = envelope(&gateway, TOOL, ARGS, now_secs(), 2);
        assert!(
            matches!(
                g.verify(TOOL, ARGS, &chain, &env2),
                Err(DelegationReject::Chain(_))
            ),
            "a store revocation must reject the next invoke",
        );
    }

    #[test]
    fn signer_headers_verify_through_the_gate() {
        // The caller-side DelegationSigner and the provider-side DelegationGate
        // are a matched pair: what the signer mints, the gate admits.
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway);
        let signer = DelegationSigner::new(gateway.clone(), chain.to_bytes());
        let g = gate(&root, Arc::new(RevocationRegistry::new()));

        let headers = signer.headers(TOOL, ARGS);
        let find = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_slice())
                .unwrap()
        };
        let leaf = g
            .verify(TOOL, ARGS, find(HDR_DELEGATION), find(HDR_DELEGATION_SIG))
            .expect("signer output must verify");
        assert_eq!(&leaf, gateway.entity_id());

        // A second mint for the same (tool, args) is a fresh nonce → also
        // admitted (not a self-replay).
        let headers2 = signer.headers(TOOL, ARGS);
        let find2 = |name: &str| {
            headers2
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_slice())
                .unwrap()
        };
        assert!(g
            .verify(TOOL, ARGS, find2(HDR_DELEGATION), find2(HDR_DELEGATION_SIG))
            .is_ok());
    }

    #[test]
    fn the_audit_sink_records_the_admitted_leaf() {
        use std::sync::Mutex as StdMutex;
        let (root, machine, gateway) = identities();
        let chain = chain_for(&root, &machine, &gateway).to_bytes();
        let seen: Arc<StdMutex<Vec<DelegationAudit>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let g = gate(&root, Arc::new(RevocationRegistry::new())).with_audit(Arc::new(
            move |a: &DelegationAudit| {
                sink_seen.lock().unwrap().push(a.clone());
            },
        ));
        let env = envelope(&gateway, TOOL, ARGS, now_secs(), 1);
        g.verify(TOOL, ARGS, &chain, &env).unwrap();
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(&recorded[0].leaf, gateway.entity_id());
        assert_eq!(&recorded[0].root, root.entity_id());
        assert_eq!(recorded[0].tool, TOOL);
    }
}
