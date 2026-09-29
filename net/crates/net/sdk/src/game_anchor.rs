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
//! # Games are kept apart
//!
//! [`serve_game_enrollment`] also installs the core's enrollment tenant
//! resolver: when a visitor is admitted, the core reads the grant's root,
//! this registry names the game (`GameRegistry::tenant_of_chain`, with `webrtc`), and
//! the session is promoted **for that game**. The anchor then neither
//! floods nor replays one game's announcements to another game's players
//! and refuses relayed traffic between them — so a game-B player cannot
//! list, discover or reach a game-A lobby through this anchor. Sessions
//! with no game (native peers, dedicated hosts) meet everyone.
//!
//! # Open games
//!
//! A registry built [`GameRegistry::with_open_games`] also admits games
//! nobody registered: any valid game id, from any page, keyed on the
//! page's **origin and the game id together** ([`OpenGames`]). Their
//! roots and tenants are derived the same stateless way, under their
//! own domain separation, so an open game is as isolated as a
//! registered one and two sites naming their game alike never share
//! one. What bounds them: a capacity with idle reclamation (never
//! eviction of a game in use), a per-game and an all-open-games
//! issuance ceiling, and a per-game cap on enrolled players. The games
//! held are kept in a state file, so a restarted anchor still enrolls a
//! page reconnecting with an invite issued before the restart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
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
/// Domain separation for a game's tenant id.
#[cfg(feature = "webrtc")]
const TENANT_CONTEXT: &str = "net-mesh game anchor: tenant v1";
/// Domain separation for deriving an open game's root seed.
const OPEN_GAME_ROOT_CONTEXT: &str = "net-mesh game anchor: open game root v1";
/// Domain separation for an open game's tenant id.
#[cfg(feature = "webrtc")]
const OPEN_TENANT_CONTEXT: &str = "net-mesh game anchor: open tenant v1";
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
    /// The page origin cannot key an open game ([`is_valid_origin`]).
    #[error("invalid origin {0:?}")]
    InvalidOrigin(String),
    /// Every open-game slot is held by a game still in use.
    #[error("this anchor holds as many open games as it admits; retry later")]
    OpenGamesFull,
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
    /// The page origin of an **open** game; absent for a registered one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
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
    /// The page origin an **open** game belongs to; `None` for a
    /// registered game.
    origin: Option<String>,
    root: Identity,
    authority: EnrollmentAuthority,
    #[cfg(feature = "webrtc")]
    tenant: net::adapter::net::rtc::TenantId,
    counters: Counters,
    /// `(minute, issued in it)` — a fixed one-minute window.
    window: Mutex<(u64, u32)>,
    /// Invite nonce → `(the device it is bound to, the invite's deadline)`.
    bound: Mutex<HashMap<[u8; 16], (EntityId, u64)>>,
    /// Unix seconds of the last credential issued (or of the load, for
    /// an open game read back from the state file). An open game idle
    /// for longer than [`OpenGames::idle_after`] has no invite left
    /// that could enroll, and may be reclaimed.
    last_issued: AtomicU64,
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

    /// Count one issuance against this game's per-minute ceiling.
    fn admit_issuance(&self, now: u64) -> Result<(), GameAnchorError> {
        let minute = now / 60;
        let mut window = self.window.lock();
        if window.0 != minute {
            *window = (minute, 0);
        }
        if window.1 >= self.config.issue_per_minute {
            self.counters
                .credentials_refused
                .fetch_add(1, Ordering::Relaxed);
            return Err(GameAnchorError::RateLimited(self.config.id.clone()));
        }
        window.1 += 1;
        Ok(())
    }
}

/// Bindings past which expired entries are swept on the next insert.
const PRUNE_AT: usize = 4096;

/// **Open games**: admit any game, from any page, without registering
/// it first ([`GameRegistry::with_open_games`]).
///
/// An open game is keyed on **the page's origin and the game id**
/// together, so two sites that both call their game `chess` get two
/// separate games — separate roots, separate tenants, separate lobbies
/// — and neither can land in the other's by picking its name. Browsers
/// send `Origin` truthfully; a native client can claim any origin, and
/// what it gets is what a visitor of that site gets anyway: an
/// anonymous credential.
#[derive(Debug, Clone)]
pub struct OpenGames {
    /// Open games held at once. Past it a NEW game is refused
    /// ([`GameAnchorError::OpenGamesFull`]) until one goes idle; a game
    /// already held is never evicted to make room.
    pub capacity: usize,
    /// Each open game's issuance ceiling, credentials per minute.
    pub issue_per_minute: u32,
    /// All open games' issuance together, credentials per minute, so
    /// inventing game names cannot multiply the budget.
    pub total_per_minute: u32,
    /// Enrolled players one open game may hold on this anchor at once,
    /// so no single game can take every peer slot. Enforced by
    /// [`serve_game_enrollment`].
    pub max_players_per_game: usize,
    /// How long an open game may go without an issuance before it is
    /// reclaimed. Must be at least the invite lifetime: a reclaimed
    /// game's outstanding invites no longer enroll.
    pub idle_after: Duration,
    /// Where the open games are kept across restarts, one
    /// `<origin> <game>` line each. Without it, a restarted anchor
    /// forgets them, and a page that reconnects with an invite issued
    /// before the restart is refused. `None` keeps them in memory only.
    pub state_file: Option<std::path::PathBuf>,
}

/// Default [`OpenGames::capacity`].
pub const DEFAULT_OPEN_GAME_CAPACITY: usize = 8192;
/// Default [`OpenGames::total_per_minute`].
pub const DEFAULT_OPEN_TOTAL_PER_MINUTE: u32 = 9000;
/// Default [`OpenGames::max_players_per_game`].
pub const DEFAULT_OPEN_MAX_PLAYERS_PER_GAME: usize = 256;
/// Longest page origin an open game may be keyed on.
pub const MAX_ORIGIN_LEN: usize = 256;

impl OpenGames {
    /// Open games with the default limits, kept in `state_file`.
    pub fn new(state_file: Option<std::path::PathBuf>) -> Self {
        Self {
            capacity: DEFAULT_OPEN_GAME_CAPACITY,
            issue_per_minute: DEFAULT_ISSUE_PER_MINUTE,
            total_per_minute: DEFAULT_OPEN_TOTAL_PER_MINUTE,
            max_players_per_game: DEFAULT_OPEN_MAX_PLAYERS_PER_GAME,
            idle_after: DEFAULT_INVITE_TTL,
            state_file,
        }
    }
}

/// Is `origin` usable as an open game's key? A serialized web origin:
/// `http://` or `https://`, then a host and optional port, printable
/// ASCII with no whitespace, path, query or fragment, at most
/// [`MAX_ORIGIN_LEN`] bytes.
pub fn is_valid_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    origin.len() <= MAX_ORIGIN_LEN
        && !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b'?' | b'#' | b'@' | b'\\'))
}

struct Open {
    config: OpenGames,
    /// `(minute, issued in it)` across every open game.
    window: Mutex<(u64, u32)>,
    /// Writes to the state file that failed (the game still works until
    /// the next restart).
    state_write_errors: AtomicU64,
}

/// The games an anchor admits browsers for.
pub struct GameRegistry {
    secret: [u8; 32],
    mac_key: [u8; 32],
    /// Key → game. A registered game's key is its id; an open game's is
    /// `<origin> <id>` (neither may contain a space, so they never meet).
    games: RwLock<HashMap<String, Arc<Game>>>,
    /// Game root → key, for resolving a join request.
    by_root: RwLock<HashMap<[u8; 32], String>>,
    open: Option<Open>,
}

impl std::fmt::Debug for GameRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameRegistry")
            .field("games", &self.games())
            .field("open", &self.open.as_ref().map(|o| &o.config))
            .finish_non_exhaustive()
    }
}

fn open_key(origin: &str, game: &str) -> String {
    format!("{origin} {game}")
}

impl GameRegistry {
    /// A registry deriving every game's root from `secret`. The secret is
    /// the anchor's: keep it private and stable — changing it changes
    /// every game's root, and outstanding credentials stop enrolling.
    pub fn new(secret: [u8; 32], games: Vec<GameConfig>) -> Result<Self, GameAnchorError> {
        let registry = Self {
            secret,
            mac_key: blake3::derive_key(INVITE_MAC_CONTEXT, &secret),
            games: RwLock::new(HashMap::new()),
            by_root: RwLock::new(HashMap::new()),
            open: None,
        };
        for config in games {
            if !is_valid_game_id(&config.id) {
                return Err(GameAnchorError::InvalidGameId(config.id));
            }
            if registry.games.read().contains_key(&config.id) {
                return Err(GameAnchorError::DuplicateGame(config.id));
            }
            let root = Identity::from_seed(Self::root_seed(&secret, &config.id));
            #[cfg(feature = "webrtc")]
            let tenant = Self::tenant_id(&config.id);
            let key = config.id.clone();
            registry.insert(
                key,
                Game::new(
                    config,
                    None,
                    root,
                    #[cfg(feature = "webrtc")]
                    tenant,
                    0,
                ),
            );
        }
        Ok(registry)
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
        Self::new(Self::secret_from_identity(issuer), games)
    }

    /// The registry secret [`Self::from_identity`] derives.
    pub fn secret_from_identity(issuer: &Identity) -> [u8; 32] {
        blake3::derive_key(ISSUER_SECRET_CONTEXT, &issuer.to_bytes())
    }

    /// Admit **open games** as well as the registered ones (see
    /// [`OpenGames`]). Reads back the open games in
    /// [`OpenGames::state_file`], if it exists; a line that does not
    /// parse is skipped, never fatal.
    pub fn with_open_games(mut self, open: OpenGames) -> std::io::Result<Self> {
        let now = now_unix();
        if let Some(path) = &open.state_file {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e),
            };
            for line in text.lines() {
                if self.open_count() >= open.capacity {
                    break;
                }
                let Some((origin, game)) = line.trim().split_once(' ') else {
                    continue;
                };
                if is_valid_origin(origin) && is_valid_game_id(game) {
                    // Loaded as just used: its invites may still be out.
                    self.create_open(origin, game, open.issue_per_minute, now);
                }
            }
        }
        self.open = Some(Open {
            config: open,
            window: Mutex::new((0, 0)),
            state_write_errors: AtomicU64::new(0),
        });
        if self.open_count() > 0 {
            // Rewrite, so a file with junk or duplicates comes back clean.
            self.write_state();
        }
        Ok(self)
    }

    /// Does this registry admit open games?
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The per-game player cap for open games, if open.
    pub fn open_max_players_per_game(&self) -> Option<usize> {
        self.open.as_ref().map(|o| o.config.max_players_per_game)
    }

    fn root_seed(secret: &[u8; 32], game: &str) -> [u8; 32] {
        let mut material = Vec::with_capacity(32 + game.len());
        material.extend_from_slice(secret);
        material.extend_from_slice(game.as_bytes());
        blake3::derive_key(GAME_ROOT_CONTEXT, &material)
    }

    fn open_root_seed(secret: &[u8; 32], origin: &str, game: &str) -> [u8; 32] {
        let mut material = Vec::with_capacity(32 + origin.len() + 1 + game.len());
        material.extend_from_slice(secret);
        material.extend_from_slice(origin.as_bytes());
        material.push(0);
        material.extend_from_slice(game.as_bytes());
        blake3::derive_key(OPEN_GAME_ROOT_CONTEXT, &material)
    }

    /// Hold `game` under `key`, unless a game already is — then that
    /// one. One write lock over both maps (games, then roots, the order
    /// every writer takes), so two first requests for one open game
    /// cannot each create it.
    fn insert(&self, key: String, game: Game) -> Arc<Game> {
        let mut games = self.games.write();
        if let Some(existing) = games.get(&key) {
            return Arc::clone(existing);
        }
        let game = Arc::new(game);
        self.by_root
            .write()
            .insert(*game.root.entity_id().as_bytes(), key.clone());
        games.insert(key, Arc::clone(&game));
        game
    }

    fn create_open(&self, origin: &str, game: &str, issue_per_minute: u32, now: u64) -> Arc<Game> {
        let key = open_key(origin, game);
        if let Some(existing) = self.games.read().get(&key) {
            return Arc::clone(existing);
        }
        let root = Identity::from_seed(Self::open_root_seed(&self.secret, origin, game));
        #[cfg(feature = "webrtc")]
        let tenant = Self::open_tenant_id(origin, game);
        self.insert(
            key,
            Game::new(
                GameConfig {
                    id: game.to_string(),
                    issue_per_minute,
                },
                Some(origin.to_string()),
                root,
                #[cfg(feature = "webrtc")]
                tenant,
                now,
            ),
        )
    }

    fn open_count(&self) -> usize {
        self.games
            .read()
            .values()
            .filter(|g| g.origin.is_some())
            .count()
    }

    /// The open game for `(origin, game)`, created if there is room.
    fn open_game(&self, origin: &str, game: &str, now: u64) -> Result<Arc<Game>, GameAnchorError> {
        let Some(open) = self.open.as_ref() else {
            return Err(GameAnchorError::UnknownGame(game.to_string()));
        };
        if !is_valid_game_id(game) {
            return Err(GameAnchorError::InvalidGameId(game.to_string()));
        }
        if !is_valid_origin(origin) {
            return Err(GameAnchorError::InvalidOrigin(origin.to_string()));
        }
        if let Some(existing) = self.games.read().get(&open_key(origin, game)) {
            return Ok(Arc::clone(existing));
        }
        if self.open_count() >= open.config.capacity {
            self.reclaim_idle(now);
            if self.open_count() >= open.config.capacity {
                return Err(GameAnchorError::OpenGamesFull);
            }
        }
        let created = self.create_open(origin, game, open.config.issue_per_minute, now);
        self.append_state(origin, game);
        Ok(created)
    }

    /// Drop the open games idle past [`OpenGames::idle_after`]. Only a
    /// game with no invite left that could enroll goes.
    fn reclaim_idle(&self, now: u64) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let idle = open.config.idle_after.as_secs();
        let gone: Vec<String> = self
            .games
            .read()
            .iter()
            .filter(|(_, g)| {
                g.origin.is_some()
                    && g.last_issued.load(Ordering::Relaxed).saturating_add(idle) < now
            })
            .map(|(key, _)| key.clone())
            .collect();
        if gone.is_empty() {
            return;
        }
        {
            let mut games = self.games.write();
            let mut by_root = self.by_root.write();
            for key in &gone {
                if let Some(game) = games.remove(key) {
                    by_root.remove(game.root.entity_id().as_bytes());
                }
            }
        }
        self.write_state();
    }

    fn append_state(&self, origin: &str, game: &str) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(path) = &open.config.state_file else {
            return;
        };
        use std::io::Write as _;
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{origin} {game}"));
        if written.is_err() {
            open.state_write_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Rewrite the state file with exactly the open games held now.
    fn write_state(&self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(path) = &open.config.state_file else {
            return;
        };
        let mut lines: Vec<String> = self
            .games
            .read()
            .values()
            .filter_map(|g| g.origin.as_ref().map(|o| format!("{o} {}\n", g.config.id)))
            .collect();
        lines.sort();
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let written =
            std::fs::write(&tmp, lines.concat()).and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            open.state_write_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The enrollment root of the registered game `game`, or `None` if
    /// it is not registered.
    pub fn root_of(&self, game: &str) -> Option<EntityId> {
        self.games
            .read()
            .get(game)
            .map(|g| g.root.entity_id().clone())
    }

    /// The enrollment root of the open game `(origin, game)`, if held.
    pub fn open_root_of(&self, origin: &str, game: &str) -> Option<EntityId> {
        self.games
            .read()
            .get(&open_key(origin, game))
            .map(|g| g.root.entity_id().clone())
    }

    fn game_by_root(&self, root: &EntityId) -> Option<Arc<Game>> {
        let key = self.by_root.read().get(root.as_bytes()).cloned()?;
        self.games.read().get(&key).cloned()
    }

    /// The game a root belongs to, or `None`.
    pub fn game_of_root(&self, root: &EntityId) -> Option<String> {
        self.game_by_root(root).map(|g| g.config.id.clone())
    }

    /// The core tenant id of the registered game `game`: how the anchor
    /// tells games apart. A hash of the id, so every instance agrees
    /// without coordination.
    #[cfg(feature = "webrtc")]
    pub fn tenant_id(game: &str) -> net::adapter::net::rtc::TenantId {
        Self::tenant_from(blake3::derive_key(TENANT_CONTEXT, game.as_bytes()))
    }

    /// The core tenant id of the open game `(origin, game)`.
    #[cfg(feature = "webrtc")]
    pub fn open_tenant_id(origin: &str, game: &str) -> net::adapter::net::rtc::TenantId {
        let mut material = Vec::with_capacity(origin.len() + 1 + game.len());
        material.extend_from_slice(origin.as_bytes());
        material.push(0);
        material.extend_from_slice(game.as_bytes());
        Self::tenant_from(blake3::derive_key(OPEN_TENANT_CONTEXT, &material))
    }

    #[cfg(feature = "webrtc")]
    fn tenant_from(digest: [u8; 32]) -> net::adapter::net::rtc::TenantId {
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest[..8]);
        net::adapter::net::rtc::TenantId(u64::from_le_bytes(id))
    }

    /// The tenant of an Admitted grant (delegation-chain bytes) this
    /// registry issued: its root's game. `None` for a chain that does not
    /// parse or is rooted anywhere else.
    #[cfg(feature = "webrtc")]
    pub fn tenant_of_chain(&self, chain: &[u8]) -> Option<net::adapter::net::rtc::TenantId> {
        let chain = DelegationChain::from_bytes(chain).ok()?;
        self.game_by_root(&chain.root()).map(|g| g.tenant)
    }

    /// The registered game ids, sorted. Open games are not listed.
    pub fn games(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .games
            .read()
            .values()
            .filter(|g| g.origin.is_none())
            .map(|g| g.config.id.clone())
            .collect();
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

    /// Mint a self-verifying invite for the registered game `game`,
    /// expiring `ttl` after `now`. Counts against nothing;
    /// [`Self::issue_credential_at`] is the limited path.
    pub fn mint_invite_at(
        &self,
        game: &str,
        rendezvous: impl Into<String>,
        ttl: Duration,
        now: u64,
    ) -> Result<InviteToken, GameAnchorError> {
        let root = self
            .root_of(game)
            .ok_or_else(|| GameAnchorError::UnknownGame(game.to_string()))?;
        Ok(self.mint_invite_for(root, rendezvous.into(), ttl, now))
    }

    fn mint_invite_for(
        &self,
        root: EntityId,
        rendezvous: String,
        ttl: Duration,
        now: u64,
    ) -> InviteToken {
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
        InviteToken {
            root,
            rendezvous,
            nonce,
            expires_at: u64::from(expires),
        }
    }

    /// Rebuild the invite a join request refers to, from the request
    /// alone. `None` when the root is not a held game's or the
    /// nonce's MAC does not verify — an invite this anchor never minted.
    fn reconstruct_invite(&self, request: &JoinRequest) -> Option<(Arc<Game>, InviteToken)> {
        let game = self.game_by_root(&request.root)?;
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

    /// Issue a visitor credential for the registered game `game`: a
    /// fresh self-verifying invite, signed into a
    /// [`BrowserBootstrapCredential`] by `issuer`. Counted, and refused
    /// once the game's per-minute ceiling is spent.
    pub fn issue_credential_at(
        &self,
        game: &str,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
        now: u64,
    ) -> Result<BrowserBootstrapCredential, GameAnchorError> {
        let entry = self
            .games
            .read()
            .get(game)
            .cloned()
            .ok_or_else(|| GameAnchorError::UnknownGame(game.to_string()))?;
        entry.admit_issuance(now)?;
        Ok(self.issue_for(&entry, issuer, anchor, now))
    }

    /// Issue a visitor credential for the **open** game `(origin, game)`,
    /// creating the game on first use. Refused when this registry is not
    /// open, the origin or id is not usable, every open game's slot is
    /// held, or the game's or all open games' ceiling is spent.
    pub fn issue_open_credential_at(
        &self,
        origin: &str,
        game: &str,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
        now: u64,
    ) -> Result<BrowserBootstrapCredential, GameAnchorError> {
        let entry = self.open_game(origin, game, now)?;
        if let Some(open) = self.open.as_ref() {
            let minute = now / 60;
            let mut window = open.window.lock();
            if window.0 != minute {
                *window = (minute, 0);
            }
            if window.1 >= open.config.total_per_minute {
                entry
                    .counters
                    .credentials_refused
                    .fetch_add(1, Ordering::Relaxed);
                return Err(GameAnchorError::RateLimited(game.to_string()));
            }
            entry.admit_issuance(now)?;
            window.1 += 1;
        }
        Ok(self.issue_for(&entry, issuer, anchor, now))
    }

    fn issue_for(
        &self,
        entry: &Game,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
        now: u64,
    ) -> BrowserBootstrapCredential {
        let invite = self.mint_invite_for(
            entry.root.entity_id().clone(),
            anchor.bootstrap_url.clone(),
            anchor.invite_ttl,
            now,
        );
        entry.last_issued.fetch_max(now, Ordering::Relaxed);
        entry
            .counters
            .credentials_issued
            .fetch_add(1, Ordering::Relaxed);
        BrowserBootstrapCredential::mint_at(
            issuer,
            invite,
            anchor.noise_pubkey,
            anchor.psk.clone(),
            anchor.bootstrap_url.clone(),
            anchor.psk_ttl,
            now,
        )
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
        self.handle_join_request_limited_at(request_bytes, grant_ttl, now, |_| true)
    }

    /// [`Self::handle_join_request_at`], refusing an **open** game's
    /// visitor when `has_room(game's tenant)` says the game is full.
    /// The check runs after the invite verifies, so a forged request
    /// learns nothing about occupancy.
    #[cfg(feature = "webrtc")]
    pub fn handle_join_request_with_room_at(
        &self,
        request_bytes: &[u8],
        grant_ttl: Duration,
        now: u64,
        has_room: impl Fn(net::adapter::net::rtc::TenantId) -> bool,
    ) -> Vec<u8> {
        self.handle_join_request_limited_at(request_bytes, grant_ttl, now, |game: &Game| {
            game.origin.is_none() || has_room(game.tenant)
        })
    }

    fn handle_join_request_limited_at(
        &self,
        request_bytes: &[u8],
        grant_ttl: Duration,
        now: u64,
        has_room: impl Fn(&Game) -> bool,
    ) -> Vec<u8> {
        let rejected =
            |code: u16, message: String| JoinOutcome::Rejected { code, message }.to_bytes();
        let request = match JoinRequest::from_bytes(request_bytes) {
            Ok(request) => request,
            Err(e) => return rejected(reject::MALFORMED, e.to_string()),
        };
        let Some((game, invite)) = self.reconstruct_invite(&request) else {
            // Count against the game when the root names one.
            if let Some(game) = self.game_by_root(&request.root) {
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
            .and_then(|()| game.bind(invite.nonce, &request.device, invite.expires_at, now));
        if let Err(e) = admitted {
            game.counters
                .enrollments_refused
                .fetch_add(1, Ordering::Relaxed);
            return rejected(reject_code(&e), e.to_string());
        }
        if !has_room(&game) {
            game.counters
                .enrollments_refused
                .fetch_add(1, Ordering::Relaxed);
            return rejected(
                reject::DENIED,
                format!(
                    "game {:?} holds as many players as this anchor admits for one game; \
                     retry shortly",
                    game.config.id
                ),
            );
        }
        // Depth 0: a browser visitor extends nothing.
        match DelegationChain::derive_device(&game.root, &request.device, grant_ttl, 0) {
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
                let e = EnrollmentError::Token(e);
                rejected(reject_code(&e), e.to_string())
            }
        }
    }

    /// Every game's counters, registered games first by id, then open
    /// games by origin and id.
    pub fn stats(&self) -> Vec<GameStats> {
        let mut out: Vec<GameStats> = self
            .games
            .read()
            .values()
            .map(|g| GameStats {
                game: g.config.id.clone(),
                origin: g.origin.clone(),
                root: hex(g.root.entity_id().as_bytes()),
                credentials_issued: g.counters.credentials_issued.load(Ordering::Relaxed),
                credentials_refused: g.counters.credentials_refused.load(Ordering::Relaxed),
                enrollments_admitted: g.counters.enrollments_admitted.load(Ordering::Relaxed),
                enrollments_refused: g.counters.enrollments_refused.load(Ordering::Relaxed),
            })
            .collect();
        out.sort_by(|a, b| (&a.origin, &a.game).cmp(&(&b.origin, &b.game)));
        out
    }

    /// Open-mode counters, or `None` when the registry is not open.
    pub fn open_stats(&self) -> Option<OpenGameStats> {
        let open = self.open.as_ref()?;
        Some(OpenGameStats {
            games: self.open_count(),
            capacity: open.config.capacity,
            state_write_errors: open.state_write_errors.load(Ordering::Relaxed),
        })
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

    /// [`Self::issue_open_credential_at`] at the current time.
    pub fn issue_open_credential(
        &self,
        origin: &str,
        game: &str,
        issuer: &Identity,
        anchor: &AnchorCredentialParams,
    ) -> Result<BrowserBootstrapCredential, GameAnchorError> {
        self.issue_open_credential_at(origin, game, issuer, anchor, now_unix())
    }

    /// [`Self::handle_join_request_at`] at the current time.
    pub fn handle_join_request(&self, request_bytes: &[u8], grant_ttl: Duration) -> Vec<u8> {
        self.handle_join_request_at(request_bytes, grant_ttl, now_unix())
    }
}

/// Open-mode counters ([`GameRegistry::open_stats`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenGameStats {
    /// Open games held now.
    pub games: usize,
    /// The most open games held at once.
    pub capacity: usize,
    /// State-file writes that failed; those games are forgotten on the
    /// next restart.
    pub state_write_errors: u64,
}

impl Game {
    fn new(
        config: GameConfig,
        origin: Option<String>,
        root: Identity,
        #[cfg(feature = "webrtc")] tenant: net::adapter::net::rtc::TenantId,
        now: u64,
    ) -> Self {
        Self {
            authority: EnrollmentAuthority::new(root.clone()),
            root,
            config,
            origin,
            #[cfg(feature = "webrtc")]
            tenant,
            counters: Counters::default(),
            window: Mutex::new((0, 0)),
            bound: Mutex::new(HashMap::new()),
            last_issued: AtomicU64::new(now),
        }
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
    // Each admitted session is promoted FOR ITS GAME: the core asks this
    // registry which game a grant belongs to, and keeps games apart.
    #[cfg(feature = "webrtc")]
    {
        let registry = registry.clone();
        mesh.node()
            .set_enrollment_tenant_resolver(Some(std::sync::Arc::new(move |chain: &[u8]| {
                registry.tenant_of_chain(chain)
            })));
    }
    // An open game may hold at most its cap of enrolled players here,
    // counted from the core's own sessions.
    #[cfg(feature = "webrtc")]
    let node = mesh.node().clone();
    // Raw bodies both ways: the core's promotion gate reads the
    // outcome's own `NMO1` framing (see `mesh_enroll`).
    mesh.serve_rpc_raw_bytes(
        crate::mesh_enroll::ENROLLMENT_SERVICE,
        move |request: Vec<u8>| {
            let registry = registry.clone();
            #[cfg(feature = "webrtc")]
            let node = node.clone();
            async move {
                #[cfg(feature = "webrtc")]
                if let Some(cap) = registry.open_max_players_per_game() {
                    return Ok::<Vec<u8>, String>(registry.handle_join_request_with_room_at(
                        &request,
                        grant_ttl,
                        now_unix(),
                        |tenant| node.tenant_peer_count(tenant) < cap,
                    ));
                }
                Ok::<Vec<u8>, String>(registry.handle_join_request(&request, grant_ttl))
            }
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

    #[cfg(feature = "webrtc")]
    #[test]
    fn a_grant_names_its_game_and_nothing_else_does() {
        let games = registry(&["alpha", "beta"]);
        let credential = games
            .issue_credential_at("alpha", &Identity::generate(), &params(), NOW)
            .unwrap();
        let JoinOutcome::Admitted { chain } = outcome(&games.handle_join_request_at(
            &join(&credential.invite),
            DEFAULT_GRANT_TTL,
            NOW,
        )) else {
            panic!("admitted");
        };
        assert_eq!(
            games.tenant_of_chain(&chain),
            Some(GameRegistry::tenant_id("alpha"))
        );
        assert_ne!(
            GameRegistry::tenant_id("alpha"),
            GameRegistry::tenant_id("beta")
        );
        assert_eq!(games.tenant_of_chain(b"not a chain"), None);
        let elsewhere = GameRegistry::new([8u8; 32], vec![GameConfig::new("alpha")]).unwrap();
        assert_eq!(
            elsewhere.tenant_of_chain(&chain),
            None,
            "another anchor's roots are not ours"
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
        assert_eq!(
            one.game_of_root(&one.root_of("beta").unwrap()).as_deref(),
            Some("beta")
        );
    }

    #[test]
    fn an_issued_credential_names_the_game_root_and_verifies_under_the_issuer() {
        let games = registry(&["alpha"]);
        let issuer = Identity::generate();
        let credential = games
            .issue_credential_at("alpha", &issuer, &params(), NOW)
            .unwrap();
        assert_eq!(credential.invite.root, games.root_of("alpha").unwrap());
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
                    assert_eq!(chain.root(), games.root_of("alpha").unwrap());
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

    // ---- open games -------------------------------------------------

    const SITE_A: &str = "https://a.example";
    const SITE_B: &str = "https://b.example";

    fn open_registry(open: OpenGames) -> GameRegistry {
        registry(&["alpha"]).with_open_games(open).unwrap()
    }

    fn scratch_file(name: &str) -> std::path::PathBuf {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        std::env::temp_dir().join(format!("net-open-games-{name}-{}", hex(&random)))
    }

    fn admitted(bytes: &[u8]) -> bool {
        matches!(outcome(bytes), JoinOutcome::Admitted { .. })
    }

    /// Two sites naming their game alike get two games, and neither is
    /// the registered game of that name: an open game is keyed on the
    /// origin AND the id.
    #[test]
    fn an_open_game_is_keyed_on_the_origin_and_the_id() {
        let games = open_registry(OpenGames::new(None));
        let issuer = Identity::generate();
        let a = games
            .issue_open_credential_at(SITE_A, "alpha", &issuer, &params(), NOW)
            .unwrap();
        let b = games
            .issue_open_credential_at(SITE_B, "alpha", &issuer, &params(), NOW)
            .unwrap();
        let again = games
            .issue_open_credential_at(SITE_A, "alpha", &issuer, &params(), NOW)
            .unwrap();
        assert_ne!(a.invite.root, b.invite.root);
        assert_eq!(
            a.invite.root, again.invite.root,
            "one game per (origin, id)"
        );
        assert_ne!(Some(a.invite.root.clone()), games.root_of("alpha"));
        assert_eq!(
            games.open_root_of(SITE_A, "alpha"),
            Some(a.invite.root.clone())
        );
        assert_eq!(
            games.games(),
            vec!["alpha".to_string()],
            "open games are not listed"
        );
        #[cfg(feature = "webrtc")]
        {
            assert_ne!(
                GameRegistry::open_tenant_id(SITE_A, "alpha"),
                GameRegistry::open_tenant_id(SITE_B, "alpha")
            );
            assert_ne!(
                GameRegistry::open_tenant_id(SITE_A, "alpha"),
                GameRegistry::tenant_id("alpha")
            );
            let JoinOutcome::Admitted { chain } =
                outcome(&games.handle_join_request_at(&join(&a.invite), DEFAULT_GRANT_TTL, NOW))
            else {
                panic!("an open game's visitor enrolls");
            };
            assert_eq!(
                games.tenant_of_chain(&chain),
                Some(GameRegistry::open_tenant_id(SITE_A, "alpha"))
            );
        }
        let stats = games.stats();
        assert_eq!(
            stats
                .iter()
                .map(|s| (s.origin.as_deref(), s.game.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (None, "alpha"),
                (Some(SITE_A), "alpha"),
                (Some(SITE_B), "alpha")
            ]
        );
    }

    #[test]
    fn a_registry_that_is_not_open_admits_no_open_game_and_bad_keys_are_refused() {
        let closed = registry(&["alpha"]);
        assert_eq!(
            closed
                .issue_open_credential_at(SITE_A, "alpha", &Identity::generate(), &params(), NOW)
                .unwrap_err(),
            GameAnchorError::UnknownGame("alpha".into())
        );
        let games = open_registry(OpenGames::new(None));
        let issuer = Identity::generate();
        for origin in [
            "",
            "a.example",
            "ftp://a.example",
            "https://a.example/path",
            "https://a b",
            "https://",
        ] {
            assert_eq!(
                games
                    .issue_open_credential_at(origin, "alpha", &issuer, &params(), NOW)
                    .unwrap_err(),
                GameAnchorError::InvalidOrigin(origin.into()),
                "{origin:?}"
            );
        }
        assert_eq!(
            games
                .issue_open_credential_at(SITE_A, "Bad Game", &issuer, &params(), NOW)
                .unwrap_err(),
            GameAnchorError::InvalidGameId("Bad Game".into())
        );
        assert!(is_valid_origin("http://localhost:8080"));
        assert!(is_valid_origin("https://[2001:db8::1]:443"));
    }

    /// Full means refused, never evicted: a game still in use keeps its
    /// slot, and only a game idle past `idle_after` makes room.
    #[test]
    fn a_full_table_refuses_a_new_game_until_one_goes_idle() {
        let mut open = OpenGames::new(None);
        open.capacity = 1;
        open.idle_after = Duration::from_secs(3600);
        let games = open_registry(open);
        let issuer = Identity::generate();
        let first = games
            .issue_open_credential_at(SITE_A, "one", &issuer, &params(), NOW)
            .unwrap();
        assert_eq!(
            games
                .issue_open_credential_at(SITE_A, "two", &issuer, &params(), NOW + 1800)
                .unwrap_err(),
            GameAnchorError::OpenGamesFull
        );
        // Still in use: an issuance at NOW + 1800 restarts its idle clock.
        games
            .issue_open_credential_at(SITE_A, "one", &issuer, &params(), NOW + 1800)
            .unwrap();
        assert_eq!(
            games
                .issue_open_credential_at(SITE_A, "two", &issuer, &params(), NOW + 3601)
                .unwrap_err(),
            GameAnchorError::OpenGamesFull,
            "idle for less than idle_after since its LAST issuance"
        );
        games
            .issue_open_credential_at(SITE_A, "two", &issuer, &params(), NOW + 5401)
            .unwrap();
        assert_eq!(
            games.open_root_of(SITE_A, "one"),
            None,
            "the idle game went"
        );
        assert!(!admitted(&games.handle_join_request_at(
            &join(&first.invite),
            DEFAULT_GRANT_TTL,
            NOW + 5402
        )));
        assert_eq!(games.open_stats().unwrap().games, 1);
    }

    /// Inventing names does not multiply the budget: every open game
    /// draws on one ceiling as well as its own.
    #[test]
    fn every_open_game_shares_one_issuance_ceiling() {
        let mut open = OpenGames::new(None);
        open.total_per_minute = 2;
        let games = open_registry(open);
        let issuer = Identity::generate();
        games
            .issue_open_credential_at(SITE_A, "one", &issuer, &params(), NOW)
            .unwrap();
        games
            .issue_open_credential_at(SITE_A, "two", &issuer, &params(), NOW)
            .unwrap();
        assert_eq!(
            games
                .issue_open_credential_at(SITE_A, "three", &issuer, &params(), NOW)
                .unwrap_err(),
            GameAnchorError::RateLimited("three".into())
        );
        // A registered game is not on the open budget.
        games
            .issue_credential_at("alpha", &issuer, &params(), NOW)
            .unwrap();
        games
            .issue_open_credential_at(SITE_A, "three", &issuer, &params(), NOW + 60)
            .unwrap();
    }

    /// A restarted anchor still enrolls a page reconnecting with an
    /// invite issued before the restart: the state file brings the game
    /// back, and its root is derived, not stored.
    #[test]
    fn open_games_survive_a_restart_through_the_state_file() {
        let path = scratch_file("restart");
        let issuer = Identity::generate();
        let before = open_registry(OpenGames::new(Some(path.clone())));
        let credential = before
            .issue_open_credential_at(SITE_A, "chess", &issuer, &params(), NOW)
            .unwrap();
        drop(before);
        // Junk and duplicates in the file are skipped, never fatal.
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(file, "not a line\nhttps://a.example chess\nftp://x y").unwrap();
        }
        let after = open_registry(OpenGames::new(Some(path.clone())));
        assert!(admitted(&after.handle_join_request_at(
            &join(&credential.invite),
            DEFAULT_GRANT_TTL,
            NOW + 60
        )));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "https://a.example chess\n",
            "rewritten clean on load"
        );
        let forgetful = open_registry(OpenGames::new(None));
        assert!(!admitted(&forgetful.handle_join_request_at(
            &join(&credential.invite),
            DEFAULT_GRANT_TTL,
            NOW + 60
        )));
        let _ = std::fs::remove_file(&path);
    }

    /// The player cap refuses an open game's visitor once its invite
    /// verifies, and never touches a registered game.
    #[cfg(feature = "webrtc")]
    #[test]
    fn a_full_open_game_refuses_a_visitor_and_a_registered_game_is_not_capped() {
        let games = open_registry(OpenGames::new(None));
        let issuer = Identity::generate();
        let open = games
            .issue_open_credential_at(SITE_A, "chess", &issuer, &params(), NOW)
            .unwrap();
        let registered = games
            .issue_credential_at("alpha", &issuer, &params(), NOW)
            .unwrap();
        let full = |_: net::adapter::net::rtc::TenantId| false;
        match outcome(&games.handle_join_request_with_room_at(
            &join(&open.invite),
            DEFAULT_GRANT_TTL,
            NOW,
            full,
        )) {
            JoinOutcome::Rejected { code, .. } => assert_eq!(code, reject::DENIED),
            other => panic!("a full open game admitted a visitor: {other:?}"),
        }
        assert!(admitted(&games.handle_join_request_with_room_at(
            &join(&registered.invite),
            DEFAULT_GRANT_TTL,
            NOW,
            full,
        )));
        // A forged invite is refused as unknown, before occupancy is read.
        let mut forged = open.invite.clone();
        forged.nonce[15] ^= 1;
        match outcome(&games.handle_join_request_with_room_at(
            &join(&forged),
            DEFAULT_GRANT_TTL,
            NOW,
            |_| panic!("occupancy read for a forged invite"),
        )) {
            JoinOutcome::Rejected { code, .. } => assert_eq!(code, reject::UNKNOWN_INVITE),
            other => panic!("{other:?}"),
        }
    }
}
