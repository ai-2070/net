//! **Game anchors** — one anchor admitting browsers for several games,
//! each isolated by a game id, with anonymous per-visitor credentials.
//!
//! This is the SDK half of the browser plan's P0
//! (`docs/internal/plans/BROWSER_LOBBIES_AND_LARGE_WORLDS_PLAN.md` §4):
//! a CLI anchor that is *shared-ready* from the start.
//!
//! # A game is an enrollment root
//!
//! Each registered game gets its own enrollment **root** — an ed25519
//! identity derived from the anchor's secret and the game id
//! ([`GameRegistry::root_of`]). A visitor's credential carries an invite
//! naming that root; the join request echoes it; the grant the anchor
//! signs is rooted at it. So which game a session belongs to is a
//! cryptographic fact, not a tag a client chose. Any anchor instance
//! holding the same secret derives the same roots, so no per-game state
//! has to be shared between instances.
//!
//! # Invites that verify themselves
//!
//! An anonymous visitor is not a device worth a registry entry, and a
//! shared anchor must not hold a map of every invite it minted. The
//! 16-byte invite nonce therefore carries its own proof:
//!
//! ```text
//! nonce = random (4) ‖ expires_at (u32 LE, 4) ‖ MAC(secret; root ‖ random ‖ expires_at) (8)
//! ```
//!
//! The enrollment handler rebuilds the invite from the request alone —
//! root and nonce — checks the MAC, then [`EnrollmentAuthority::verify_request`]
//! checks everything else (right root, unexpired, matching nonce, the
//! device holds its key).
//!
//! # Single-use means *bound to one device*
//!
//! An invite is **bound to the first device that redeems it**: that
//! device may enroll with it again, any other device is refused as a
//! replay. Re-enrollment by the same key is not a loophole — only that
//! key can sign its join request — and it is required: `openSession()`
//! keeps the credential it was opened with and a promoted leader tab, or a
//! reconnect, enrolls with it again. A strictly one-shot invite would
//! leave the second leader provisional. What single use protects is the
//! issuance limit: one issued credential is one visitor identity, never
//! an unlimited supply of them.
//!
//! Invites therefore live as long as a play session
//! ([`DEFAULT_INVITE_TTL`]); a page past that fetches a new credential.
//!
//! **What stays per instance:** the nonce → device bindings. Two
//! instances behind one endpoint could each bind the same invite, to one
//! device each, within its lifetime. Strict binding across instances
//! needs that map shared, which is P5 work. Entries are pruned at the
//! invite's deadline, so the map is bounded by the issuance rate.
//!
//! # Limits and counters
//!
//! Issuance is limited **per game** (not one global pool), keyed on the
//! game id the anchor itself resolved, never on anything a client sent
//! unverified. Every game keeps cheap counters — credentials issued,
//! issuance refusals, enrollments admitted and refused — which is what
//! makes a per-game limit enforceable and what a shared anchor's
//! metering will read later ([`GameRegistry::stats`]).
//!
//! # What this does NOT enforce yet
//!
//! That a session enrolled for game A cannot announce, discover or route
//! into game B. The core does not yet record which root promoted a
//! session; that is the plan's second P0 slice.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use crate::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use crate::delegation::DelegationChain;
use crate::enrollment::{
    now_unix, reject, EnrollmentAuthority, EnrollmentError, InviteToken, JoinOutcome, JoinRequest,
};
use crate::identity::{EntityId, Identity};

/// Domain separation for deriving a game's root seed from the secret.
const GAME_ROOT_CONTEXT: &str = "net-mesh game anchor: game root v1";
/// Domain separation for deriving a registry secret from an issuer key.
const ISSUER_SECRET_CONTEXT: &str = "net-mesh game anchor: secret from issuer v1";
/// Domain separation for deriving the invite MAC key from the secret.
const INVITE_MAC_CONTEXT: &str = "net-mesh game anchor: invite mac v1";
/// Longest game id accepted.
pub const MAX_GAME_ID_LEN: usize = 64;
/// Default invite lifetime: a play session. The invite is bound to its
/// first device, which re-enrolls with it on promotions and reconnects.
pub const DEFAULT_INVITE_TTL: Duration = Duration::from_secs(12 * 3600);
/// Default lifetime of the credential's standing half (the PSK).
pub const DEFAULT_PSK_TTL: Duration = Duration::from_secs(86_400);
/// Default lifetime of the `root → device` grant a visitor receives.
pub const DEFAULT_GRANT_TTL: Duration = Duration::from_secs(86_400);
/// Default per-game issuance ceiling, credentials per minute.
pub const DEFAULT_ISSUE_PER_MINUTE: u32 = 600;

/// Is `id` an acceptable game id? Lowercase ASCII letters, digits, `.`,
/// `-` and `_`, 1–[`MAX_GAME_ID_LEN`] bytes, starting with a letter or
/// digit. Deliberately narrow: the id becomes part of discovery tags
/// (`net-lobby:<game>`) and must not contain `:`.
pub fn is_valid_game_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_GAME_ID_LEN
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// One game's registration: its id and its issuance ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameConfig {
    /// The game id ([`is_valid_game_id`]).
    pub id: String,
    /// Credentials this anchor issues for the game per minute.
    pub issue_per_minute: u32,
}

impl GameConfig {
    /// A game with the default issuance ceiling.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            issue_per_minute: DEFAULT_ISSUE_PER_MINUTE,
        }
    }
}

/// Why a game anchor refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GameAnchorError {
    /// The id is not an acceptable game id.
    #[error("invalid game id {0:?}")]
    InvalidGameId(String),
    /// The same game id was registered twice.
    #[error("game {0:?} is registered twice")]
    DuplicateGame(String),
    /// No such game is registered on this anchor.
    #[error("unknown game {0:?}")]
    UnknownGame(String),
    /// The game's issuance ceiling for this minute is spent.
    #[error("game {0:?} is issuing too many credentials; retry shortly")]
    RateLimited(String),
}

/// Per-game counters, read with [`GameRegistry::stats`].
#[derive(Debug, Default)]
struct Counters {
    credentials_issued: AtomicU64,
    credentials_refused: AtomicU64,
    enrollments_admitted: AtomicU64,
    enrollments_refused: AtomicU64,
}

/// A snapshot of one game's counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameStats {
    /// The game id.
    pub game: String,
    /// The game's enrollment root, hex.
    pub root: String,
    /// Credentials issued since the anchor started.
    pub credentials_issued: u64,
    /// Credential requests refused by the game's ceiling.
    pub credentials_refused: u64,
    /// Visitors admitted by enrollment.
    pub enrollments_admitted: u64,
    /// Join requests refused (bad, expired, replayed or forged invites).
    pub enrollments_refused: u64,
}

struct Game {
    config: GameConfig,
    root: Identity,
    authority: EnrollmentAuthority,
    counters: Counters,
    /// `(minute, issued in it)` — a fixed one-minute window.
    window: Mutex<(u64, u32)>,
    /// Invite nonce → `(the device it is bound to, the invite's deadline)`.
    bound: Mutex<HashMap<[u8; 16], (EntityId, u64)>>,
}

impl Game {
    /// Bind `nonce` to `device`, or confirm it already is. A nonce bound
    /// to another device is a replay.
    fn bind(
        &self,
        nonce: [u8; 16],
        device: &EntityId,
        expires: u64,
        now: u64,
    ) -> Result<(), EnrollmentError> {
        let mut bound = self.bound.lock();
        if bound.len() >= PRUNE_AT {
            bound.retain(|_, (_, deadline)| *deadline >= now);
        }
        match bound.get(&nonce) {
            Some((owner, _)) if owner != device => Err(EnrollmentError::Replay),
            Some(_) => Ok(()),
            None => {
                bound.insert(nonce, (device.clone(), expires));
                Ok(())
            }
        }
    }
}

/// Bindings past which expired entries are swept on the next insert.
const PRUNE_AT: usize = 4096;

/// The games an anchor admits browsers for.
pub struct GameRegistry {
    mac_key: [u8; 32],
    games: HashMap<String, Game>,
    /// Game root → game id, for resolving a join request.
    by_root: HashMap<[u8; 32], String>,
}

impl std::fmt::Debug for GameRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ids: Vec<&String> = self.games.keys().collect();
        ids.sort();
        f.debug_struct("GameRegistry")
            .field("games", &ids)
            .finish_non_exhaustive()
    }
}

impl GameRegistry {
    /// A registry deriving every game's root from `secret`. The secret is
    /// the anchor's: keep it private and stable — changing it changes
    /// every game's root, and outstanding credentials stop enrolling.
    pub fn new(secret: [u8; 32], games: Vec<GameConfig>) -> Result<Self, GameAnchorError> {
        let mut map = HashMap::new();
        let mut by_root = HashMap::new();
        for config in games {
            if !is_valid_game_id(&config.id) {
                return Err(GameAnchorError::InvalidGameId(config.id));
            }
            if map.contains_key(&config.id) {
                return Err(GameAnchorError::DuplicateGame(config.id));
            }
            let root = Identity::from_seed(Self::root_seed(&secret, &config.id));
            by_root.insert(*root.entity_id().as_bytes(), config.id.clone());
            map.insert(
                config.id.clone(),
                Game {
                    authority: EnrollmentAuthority::new(root.clone()),
                    root,
                    config,
                    counters: Counters::default(),
                    window: Mutex::new((0, 0)),
                    bound: Mutex::new(HashMap::new()),
                },
            );
        }
        Ok(Self {
            mac_key: blake3::derive_key(INVITE_MAC_CONTEXT, &secret),
            games: map,
            by_root,
        })
    }

    /// A registry whose secret is derived from the anchor's credential
    /// **issuer** identity, so an operator keeps one secret file: every
    /// instance started with the same issuer key derives the same game
    /// roots. The derivation is one-way and domain-separated, so the
    /// roots reveal nothing about the issuer key.
    pub fn from_identity(
        issuer: &Identity,
        games: Vec<GameConfig>,
    ) -> Result<Self, GameAnchorError> {
        Self::new(
            blake3::derive_key(ISSUER_SECRET_CONTEXT, &issuer.to_bytes()),
            games,
        )
    }

    fn root_seed(secret: &[u8; 32], game: &str) -> [u8; 32] {
        let mut material = Vec::with_capacity(32 + game.len());
        material.extend_from_slice(secret);
        material.extend_from_slice(game.as_bytes());
        blake3::derive_key(GAME_ROOT_CONTEXT, &material)
    }

    /// The enrollment root of `game`, or `None` if it is not registered.
    pub fn root_of(&self, game: &str) -> Option<&EntityId> {
        self.games.get(game).map(|g| g.root.entity_id())
    }

    /// The game a root belongs to, or `None`.
    pub fn game_of_root(&self, root: &EntityId) -> Option<&str> {
        self.by_root.get(root.as_bytes()).map(String::as_str)
    }

    /// The registered game ids, sorted.
    pub fn games(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.games.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    fn mac(&self, root: &EntityId, random: &[u8], expires: u32) -> [u8; 8] {
        let mut hasher = blake3::Hasher::new_keyed(&self.mac_key);
        hasher.update(root.as_bytes());
        hasher.update(random);
        hasher.update(&expires.to_le_bytes());
        let mut out = [0u8; 8];
        out.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
        out
    }

    /// Mint a self-verifying invite for `game`, expiring `ttl` after
    /// `now`. Counts against nothing; [`Self::issue_credential_at`] is the
    /// limited path.
    pub fn mint_invite_at(
        &self,
        game: &str,
        rendezvous: impl Into<String>,
        ttl: Duration,
        now: u64,
    ) -> Result<InviteToken, GameAnchorError> {
        let entry = self
            .games
            .get(game)
            .ok_or_else(|| GameAnchorError::UnknownGame(game.to_string()))?;
        let root = entry.root.entity_id().clone();
        let expires = u32::try_from(now.saturating_add(ttl.as_secs())).unwrap_or(u32::MAX);
        let mut random = [0u8; 4];
        if let Err(e) = getrandom::fill(&mut random) {
            // Same stance as `InviteToken::mint`: a predictable nonce is
            // worse than no anchor.
            eprintln!("FATAL: game anchor invite getrandom failure ({e:?}); aborting");
            std::process::abort();
        }
        let mut nonce = [0u8; 16];
        nonce[..4].copy_from_slice(&random);
        nonce[4..8].copy_from_slice(&expires.to_le_bytes());
        nonce[8..].copy_from_slice(&self.mac(&root, &random, expires));
        Ok(InviteToken {
            root,
            rendezvous: rendezvous.into(),
            nonce,
            expires_at: u64::from(expires),
        })
    }

    /// Rebuild the invite a join request refers to, from the request
    /// alone. `None` when the root is not a registered game's or the
    /// nonce's MAC does not verify — an invite this anchor never minted.
    fn reconstruct_invite(&self, request: &JoinRequest) -> Option<(&Game, InviteToken)> {
        let game = self.games.get(self.game_of_root(&request.root)?)?;
        let nonce = request.invite_nonce;
        let expires = u32::from_le_bytes(nonce[4..8].try_into().ok()?);
        let expected = self.mac(&request.root, &nonce[..4], expires);
        // Constant-time: the MAC is the whole of the invite's authority.
        let mut diff = 0u8;
        for (a, b) in expected.iter().zip(&nonce[8..]) {
            diff |= a ^ b;
        }
        if diff != 0 {
            return None;
        }
        Some((
            game,
            InviteToken {
                root: request.root.clone(),
                rendezvous: String::new(),
                nonce,
                expires_at: u64::from(expires),
            },
        ))
    }

    /// Issue a visitor credential for `game`: a fresh self-verifying
    /// invite, signed into a [`BrowserBootstrapCredential`] by `issuer`.
    /// Counted, and refused once the game's per-minute ceiling is spent.
    pub fn issue_credential_at(
        &self,
        game: &str,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
        now: u64,
    ) -> Result<BrowserBootstrapCredential, GameAnchorError> {
        let entry = self
            .games
            .get(game)
            .ok_or_else(|| GameAnchorError::UnknownGame(game.to_string()))?;
        {
            let minute = now / 60;
            let mut window = entry.window.lock();
            if window.0 != minute {
                *window = (minute, 0);
            }
            if window.1 >= entry.config.issue_per_minute {
                entry
                    .counters
                    .credentials_refused
                    .fetch_add(1, Ordering::Relaxed);
                return Err(GameAnchorError::RateLimited(game.to_string()));
            }
            window.1 += 1;
        }
        let invite =
            self.mint_invite_at(game, anchor.bootstrap_url.clone(), anchor.invite_ttl, now)?;
        entry
            .counters
            .credentials_issued
            .fetch_add(1, Ordering::Relaxed);
        Ok(BrowserBootstrapCredential::mint_at(
            issuer,
            invite,
            anchor.noise_pubkey,
            anchor.psk.clone(),
            anchor.bootstrap_url.clone(),
            anchor.psk_ttl,
            now,
        ))
    }

    /// The server side of `net.mesh.enroll` for a game anchor: serialized
    /// [`JoinRequest`] in, serialized [`JoinOutcome`] out. Never fails out
    /// of band — every refusal is a coded `Rejected` the visitor reads.
    pub fn handle_join_request_at(
        &self,
        request_bytes: &[u8],
        grant_ttl: Duration,
        now: u64,
    ) -> Vec<u8> {
        let rejected =
            |code: u16, message: String| JoinOutcome::Rejected { code, message }.to_bytes();
        let request = match JoinRequest::from_bytes(request_bytes) {
            Ok(request) => request,
            Err(e) => return rejected(reject::MALFORMED, e.to_string()),
        };
        let Some((game, invite)) = self.reconstruct_invite(&request) else {
            // Count against the game when the root names one.
            if let Some(game) = self
                .game_of_root(&request.root)
                .and_then(|id| self.games.get(id))
            {
                game.counters
                    .enrollments_refused
                    .fetch_add(1, Ordering::Relaxed);
            }
            return rejected(
                reject::UNKNOWN_INVITE,
                "this anchor did not issue that invite".into(),
            );
        };
        let admitted = game
            .authority
            .verify_request(&request, &invite, now)
            .and_then(|()| game.bind(invite.nonce, &request.device, invite.expires_at, now))
            .and_then(|()| {
                // Depth 0: a browser visitor extends nothing.
                DelegationChain::derive_device(&game.root, &request.device, grant_ttl, 0)
                    .map_err(EnrollmentError::Token)
            });
        match admitted {
            Ok(chain) => {
                game.counters
                    .enrollments_admitted
                    .fetch_add(1, Ordering::Relaxed);
                JoinOutcome::Admitted {
                    chain: chain.to_bytes(),
                }
                .to_bytes()
            }
            Err(e) => {
                game.counters
                    .enrollments_refused
                    .fetch_add(1, Ordering::Relaxed);
                rejected(reject_code(&e), e.to_string())
            }
        }
    }

    /// Every game's counters, sorted by game id.
    pub fn stats(&self) -> Vec<GameStats> {
        let mut out: Vec<GameStats> = self
            .games
            .values()
            .map(|g| GameStats {
                game: g.config.id.clone(),
                root: hex(g.root.entity_id().as_bytes()),
                credentials_issued: g.counters.credentials_issued.load(Ordering::Relaxed),
                credentials_refused: g.counters.credentials_refused.load(Ordering::Relaxed),
                enrollments_admitted: g.counters.enrollments_admitted.load(Ordering::Relaxed),
                enrollments_refused: g.counters.enrollments_refused.load(Ordering::Relaxed),
            })
            .collect();
        out.sort_by(|a, b| a.game.cmp(&b.game));
        out
    }

    /// [`Self::issue_credential_at`] at the current time.
    pub fn issue_credential(
        &self,
        game: &str,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
    ) -> Result<BrowserBootstrapCredential, GameAnchorError> {
        self.issue_credential_at(game, issuer, anchor, now_unix())
    }

    /// [`Self::handle_join_request_at`] at the current time.
    pub fn handle_join_request(&self, request_bytes: &[u8], grant_ttl: Duration) -> Vec<u8> {
        self.handle_join_request_at(request_bytes, grant_ttl, now_unix())
    }
}

/// What every credential this anchor issues carries about the anchor.
#[derive(Debug, Clone)]
pub struct AnchorCredentialParams {
    /// The anchor's Noise static public key — the key the browser pins.
    pub noise_pubkey: [u8; 32],
    /// The transport PSK.
    pub psk: Psk,
    /// The anchor's bootstrap URL.
    pub bootstrap_url: String,
    /// The invite (single-use half) lifetime.
    pub invite_ttl: Duration,
    /// The PSK (standing half) lifetime.
    pub psk_ttl: Duration,
}

fn reject_code(e: &EnrollmentError) -> u16 {
    match e {
        EnrollmentError::Expired => reject::EXPIRED,
        EnrollmentError::Replay => reject::REPLAY,
        EnrollmentError::WrongMesh | EnrollmentError::NonceMismatch => reject::UNKNOWN_INVITE,
        _ => reject::BAD_REQUEST,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Serve `net.mesh.enroll` for every game in `registry` on `mesh`. Hold
/// the returned handle for as long as enrollment should stay open.
#[cfg(feature = "cortex")]
pub fn serve_game_enrollment(
    mesh: &crate::mesh::Mesh,
    registry: std::sync::Arc<GameRegistry>,
    grant_ttl: Duration,
) -> Result<crate::mesh_rpc::ServeHandle, crate::mesh_rpc::ServeError> {
    // Raw bodies both ways: the core's promotion gate reads the
    // outcome's own `NMO1` framing (see `mesh_enroll`).
    mesh.serve_rpc_raw_bytes(
        crate::mesh_enroll::ENROLLMENT_SERVICE,
        move |request: Vec<u8>| {
            let registry = registry.clone();
            async move { Ok::<Vec<u8>, String>(registry.handle_join_request(&request, grant_ttl)) }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7u8; 32];
    const NOW: u64 = 1_900_000_000;

    fn registry(games: &[&str]) -> GameRegistry {
        GameRegistry::new(SECRET, games.iter().map(|g| GameConfig::new(*g)).collect()).unwrap()
    }

    fn params() -> AnchorCredentialParams {
        AnchorCredentialParams {
            noise_pubkey: [9u8; 32],
            psk: Psk::new([3u8; 32]),
            bootstrap_url: "https://anchor.example".into(),
            invite_ttl: DEFAULT_INVITE_TTL,
            psk_ttl: DEFAULT_PSK_TTL,
        }
    }

    /// A visitor's join request for `invite`, signed by a fresh device.
    fn join(invite: &InviteToken) -> Vec<u8> {
        JoinRequest::create(&Identity::generate(), "tab", vec![], invite).to_bytes()
    }

    fn outcome(bytes: &[u8]) -> JoinOutcome {
        JoinOutcome::from_bytes(bytes).unwrap()
    }

    #[test]
    fn a_registry_from_the_issuer_key_is_stable_and_not_the_raw_key() {
        let issuer = Identity::from_seed([5u8; 32]);
        let one = GameRegistry::from_identity(&issuer, vec![GameConfig::new("alpha")]).unwrap();
        let two = GameRegistry::from_identity(&issuer, vec![GameConfig::new("alpha")]).unwrap();
        assert_eq!(one.root_of("alpha"), two.root_of("alpha"));
        let raw = GameRegistry::new([5u8; 32], vec![GameConfig::new("alpha")]).unwrap();
        assert_ne!(
            one.root_of("alpha"),
            raw.root_of("alpha"),
            "derived, not the seed itself"
        );
    }

    #[test]
    fn game_ids_are_narrow() {
        for ok in ["my-game", "a", "space.race_2", "9lives"] {
            assert!(is_valid_game_id(ok), "{ok}");
        }
        for bad in [
            "",
            "My-Game",
            "net-lobby:x",
            "-lead",
            "has space",
            &"x".repeat(65),
        ] {
            assert!(!is_valid_game_id(bad), "{bad}");
        }
        assert_eq!(
            GameRegistry::new(SECRET, vec![GameConfig::new("Bad")]).unwrap_err(),
            GameAnchorError::InvalidGameId("Bad".into())
        );
        assert_eq!(
            GameRegistry::new(SECRET, vec![GameConfig::new("a"), GameConfig::new("a")])
                .unwrap_err(),
            GameAnchorError::DuplicateGame("a".into())
        );
    }

    #[test]
    fn each_game_has_its_own_root_and_every_instance_derives_the_same_one() {
        let one = registry(&["alpha", "beta"]);
        let two = registry(&["beta", "alpha"]);
        assert_ne!(one.root_of("alpha"), one.root_of("beta"));
        assert_eq!(
            one.root_of("alpha"),
            two.root_of("alpha"),
            "stateless across instances"
        );
        let other_secret = GameRegistry::new([8u8; 32], vec![GameConfig::new("alpha")]).unwrap();
        assert_ne!(one.root_of("alpha"), other_secret.root_of("alpha"));
        assert_eq!(one.game_of_root(one.root_of("beta").unwrap()), Some("beta"));
    }

    #[test]
    fn an_issued_credential_names_the_game_root_and_verifies_under_the_issuer() {
        let games = registry(&["alpha"]);
        let issuer = Identity::generate();
        let credential = games
            .issue_credential_at("alpha", &issuer, &params(), NOW)
            .unwrap();
        assert_eq!(&credential.invite.root, games.root_of("alpha").unwrap());
        credential.verify_issuer(issuer.entity_id()).unwrap();
        credential.validate_at(NOW).unwrap();
        credential.check_trust_domain(&Psk::new([3u8; 32])).unwrap();
        assert_eq!(
            credential.nonce_expires_at(),
            NOW + DEFAULT_INVITE_TTL.as_secs()
        );
        assert_eq!(
            games
                .issue_credential_at("nope", &issuer, &params(), NOW)
                .unwrap_err(),
            GameAnchorError::UnknownGame("nope".into())
        );
    }

    /// An invite binds to the first device that redeems it: that device
    /// re-enrolls with it (a promoted leader tab, a reconnect), any other
    /// device is a replay — so one issued credential is one identity.
    #[test]
    fn an_invite_binds_to_its_first_device_which_may_re_enroll_and_nobody_else_may() {
        let games = registry(&["alpha"]);
        let credential = games
            .issue_credential_at("alpha", &Identity::generate(), &params(), NOW)
            .unwrap();
        let device = Identity::generate();
        let request = JoinRequest::create(&device, "tab", vec![], &credential.invite).to_bytes();
        for at in [NOW + 5, NOW + 3600] {
            match outcome(&games.handle_join_request_at(&request, DEFAULT_GRANT_TTL, at)) {
                JoinOutcome::Admitted { chain } => {
                    let chain = DelegationChain::from_bytes(&chain).unwrap();
                    assert_eq!(&chain.root(), games.root_of("alpha").unwrap());
                }
                other => panic!("expected admitted at {at}, got {other:?}"),
            }
        }
        let someone_else = join(&credential.invite);
        assert!(matches!(
            outcome(&games.handle_join_request_at(&someone_else, DEFAULT_GRANT_TTL, NOW + 6)),
            JoinOutcome::Rejected {
                code: reject::REPLAY,
                ..
            }
        ));
        let stats = &games.stats()[0];
        assert_eq!(
            (stats.enrollments_admitted, stats.enrollments_refused),
            (2, 1)
        );
    }

    #[test]
    fn an_expired_invite_is_refused() {
        let games = registry(&["alpha"]);
        let invite = games
            .mint_invite_at("alpha", "", Duration::from_secs(60), NOW)
            .unwrap();
        assert!(matches!(
            outcome(&games.handle_join_request_at(&join(&invite), DEFAULT_GRANT_TTL, NOW + 61)),
            JoinOutcome::Rejected {
                code: reject::EXPIRED,
                ..
            }
        ));
    }

    #[test]
    fn a_forged_or_foreign_invite_is_refused() {
        let games = registry(&["alpha"]);
        let mut invite = games
            .mint_invite_at("alpha", "", DEFAULT_INVITE_TTL, NOW)
            .unwrap();
        // Extending one's own deadline breaks the MAC.
        invite.nonce[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        invite.expires_at = u64::from(u32::MAX);
        assert!(matches!(
            outcome(&games.handle_join_request_at(&join(&invite), DEFAULT_GRANT_TTL, NOW)),
            JoinOutcome::Rejected {
                code: reject::UNKNOWN_INVITE,
                ..
            }
        ));
        // An invite for a root this anchor does not know.
        let stranger = InviteToken::mint_at(
            Identity::generate().entity_id(),
            "",
            DEFAULT_INVITE_TTL,
            NOW,
        );
        assert!(matches!(
            outcome(&games.handle_join_request_at(&join(&stranger), DEFAULT_GRANT_TTL, NOW)),
            JoinOutcome::Rejected {
                code: reject::UNKNOWN_INVITE,
                ..
            }
        ));
        // One anchor's invite does not enroll at an anchor with another secret.
        let elsewhere = GameRegistry::new([8u8; 32], vec![GameConfig::new("alpha")]).unwrap();
        let theirs = elsewhere
            .mint_invite_at("alpha", "", DEFAULT_INVITE_TTL, NOW)
            .unwrap();
        assert!(matches!(
            outcome(&games.handle_join_request_at(&join(&theirs), DEFAULT_GRANT_TTL, NOW)),
            JoinOutcome::Rejected {
                code: reject::UNKNOWN_INVITE,
                ..
            }
        ));
        assert_eq!(
            games.stats()[0].enrollments_refused,
            1,
            "only the alpha-rooted forgery counts"
        );
    }

    #[test]
    fn issuance_is_limited_per_game_per_minute_and_counted() {
        let games = GameRegistry::new(
            SECRET,
            vec![
                GameConfig {
                    id: "busy".into(),
                    issue_per_minute: 2,
                },
                GameConfig::new("quiet"),
            ],
        )
        .unwrap();
        let issuer = Identity::generate();
        let minute = NOW - NOW % 60;
        for _ in 0..2 {
            games
                .issue_credential_at("busy", &issuer, &params(), minute)
                .unwrap();
        }
        assert_eq!(
            games
                .issue_credential_at("busy", &issuer, &params(), minute + 59)
                .unwrap_err(),
            GameAnchorError::RateLimited("busy".into())
        );
        // Another game's pool is untouched, and the next minute refills.
        games
            .issue_credential_at("quiet", &issuer, &params(), minute)
            .unwrap();
        games
            .issue_credential_at("busy", &issuer, &params(), minute + 60)
            .unwrap();
        let busy = games
            .stats()
            .into_iter()
            .find(|s| s.game == "busy")
            .unwrap();
        assert_eq!((busy.credentials_issued, busy.credentials_refused), (3, 1));
    }
}
