"""Delegated agent identity for the ``net`` Hermes plugin (Phase 3, Slice A).

Derives the ``root -> machine -> gateway`` delegation chain at node init from
the user root seed (``NET_MESH_IDENTITY_SEED``), holds a shared
:class:`~net.RevocationRegistry`, and can extend the chain to per-task
subagents. The derivation, chain verification, and revocation model all live
once in the Rust SDK (``net_sdk.delegation`` — bridge doctrine H2); this module
only orchestrates them for the plugin and exposes a :meth:`GatewayDelegation.verify`
self-check that :func:`node.check_net_available` gates on.

**H8 (no key material, ever).** Identities are opaque :class:`~net.Identity`
handles; only public entity-ids and the (public-token) chain are held here. The
one private seed the plugin ever holds is the *root*, loaded from the
environment exactly as the node identity is today — and even the machine /
gateway seeds are derived and kept inside Rust, never surfaced.

**Scope boundary (Slice A vs Slice B).** This module derives, verifies, extends,
and revokes the chain. It does **not** yet make the embedded node *operate under*
the gateway identity, nor attach the chain to capability invocations on the wire
— that is Slice B (a cross-cutting wrap-protocol change: the provider must carry
and verify the chain, and audit its terminal subject). See
``HERMES_INTEGRATION_PLAN.md`` Phase 3.
"""

from __future__ import annotations

import logging
import os
import platform
import socket
from typing import Optional

logger = logging.getLogger(__name__)

# Chain lifetime. Short-TTL + re-derive is the SDK's documented v1 revocation
# story; a day balances "not renewing constantly" against "a leaked chain
# self-expires". Renewal (re-derive before expiry) is a Slice-B concern.
_DEFAULT_TTL_SECONDS = 24 * 60 * 60


def _hostname() -> str:
    try:
        return socket.gethostname() or platform.node() or "unknown-host"
    except Exception:  # noqa: BLE001 — a hostname lookup must never sink the node
        return "unknown-host"


def _machine_id_path() -> Optional["pathlib.Path"]:
    """Where the persistent per-machine id lives — next to the machine-shared
    pin store (a stable per-user net-mesh dir). ``None`` if no dir resolves."""
    import pathlib

    pin = (os.environ.get("NET_MESH_PIN_STORE") or "").strip()
    if pin:
        return pathlib.Path(pin).parent / "machine-id"
    try:
        from net_sdk import default_pin_store_path

        base = default_pin_store_path()
    except Exception:  # noqa: BLE001
        base = None
    if base:
        return pathlib.Path(base).parent / "machine-id"
    home = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    if home:
        import pathlib as _pl

        return _pl.Path(home) / ".net-mesh" / "machine-id"
    return None


#: How long a starter that lost the race to publish the machine id waits for a
#: file it found EMPTY to gain its value. A current writer never publishes an
#: empty file (see :func:`_publish_machine_id`), so this only covers a writer
#: on a filesystem without hard links, or an older plugin version mid-write.
_MACHINE_ID_WAIT_SECONDS = 2.0


#: Every machine id this plugin has ever generated is ``secrets.token_hex(16)``.
_MACHINE_ID_LEN = 32

#: How long after its last write a file that could still be a write in
#: progress (see :func:`_may_be_partial`) is taken as final. A writer finishes
#: in one write + fsync, so a file this old is not being written any more.
_MACHINE_ID_SETTLE_SECONDS = 60.0


def _may_be_partial(value: str) -> bool:
    """Whether ``value`` could be a write in progress: empty, or a strict
    lowercase-hex PREFIX of a generated id (the POSIX no-hard-link fallback
    writes the name before its value). Anything else — a complete generated id,
    an id an operator wrote by hand, uppercase hex, any other shape an older
    plugin version accepted — is never a partial write of this plugin's, so it
    is the committed id as it stands."""
    return len(value) < _MACHINE_ID_LEN and all(c in "0123456789abcdef" for c in value)


def _settled(idfile) -> bool:
    """Whether ``idfile`` was last written long enough ago that no writer is
    still filling it in."""
    import time

    try:
        return time.time() - idfile.stat().st_mtime >= _MACHINE_ID_SETTLE_SECONDS
    except OSError:
        return False


def _read_machine_id(idfile) -> str:
    """The committed machine id in ``idfile``, or ``""`` when it is absent,
    unreadable, empty, or possibly still being written.

    A value that could be a partial write is adopted only once the file has
    settled: then it is all that was ever persisted (its writer died before
    completing it, so nobody used the full value), and every later start reads
    the same. A FRESH prefix may be completed any moment, so it is not adopted.
    """
    try:
        value = idfile.read_text(encoding="utf-8").strip()
    except OSError:
        return ""
    if not _may_be_partial(value):
        return value
    if value and _settled(idfile):
        return value
    return ""


def _sync_dir(path) -> None:
    """Best-effort durability barrier for a new directory entry: ``fsync`` the
    directory on POSIX, so a power loss after a successful publish cannot drop
    the ``machine-id`` name (a restart would then mint a new node identity).
    There is no portable equivalent elsewhere; NTFS journals the entry."""
    if os.name == "nt":
        return
    try:
        fd = os.open(str(path), os.O_RDONLY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)


#: Windows ``os.rename`` refuses an existing target, so there it is an atomic
#: create-with-contents; POSIX ``rename`` replaces. (A module constant so the
#: POSIX fallback can be exercised on any host.)
_RENAME_NEVER_REPLACES = os.name == "nt"


def _publish_machine_id(idfile, value: str) -> bool:
    """Publish ``value`` as the machine id **with its contents already in
    place**: ``True`` if this call published it, ``False`` if another starter
    already had.

    The value is written + fsynced to a private temp file first, then linked
    to the final name — ``os.link`` fails if the name exists, so creation stays
    exclusive, and the name never exists without its complete value. (An
    exclusive ``open(.., "x")`` + write, the previous scheme, made the NAME
    exclusive but published an empty file first: a concurrent starter read it
    empty and returned its own never-persisted id — HERMES-08.)
    """
    import secrets

    tmp = idfile.with_name(f".{idfile.name}.{secrets.token_hex(8)}.tmp")
    with open(tmp, "x", encoding="utf-8") as f:
        f.write(value)
        f.flush()
        os.fsync(f.fileno())
    try:
        try:
            os.link(tmp, idfile)
            _sync_dir(idfile.parent)
            return True
        except FileExistsError:
            return False
        except OSError:
            # The filesystem has no hard links (FAT, some network/FUSE mounts).
            if _RENAME_NEVER_REPLACES:
                # Windows rename never replaces an existing target, so it is
                # the same atomic create-with-contents.
                try:
                    os.rename(tmp, idfile)
                    _sync_dir(idfile.parent)
                    return True
                except FileExistsError:
                    return False
            # POSIX without hard links has no no-replace rename in the stdlib:
            # fall back to exclusive create + immediate write. The name can be
            # briefly empty here; losers wait for the value instead of
            # inventing their own (`_persistent_machine_id`).
            try:
                fd = os.open(idfile, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            except FileExistsError:
                return False
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as f:
                    f.write(value)
                    f.flush()
                    os.fsync(f.fileno())
            except BaseException:
                # This call created the name, so a failed write must not leave
                # a partial id behind for later starts (they would also refuse
                # it — `_read_machine_id` accepts only a complete id).
                try:
                    os.unlink(idfile)
                except OSError:
                    pass
                raise
            _sync_dir(idfile.parent)
            return True
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def _persistent_machine_id() -> str:
    """A unique, persistent per-machine id — generated once and stored so it
    survives restarts. Returns ``""`` if it can't be read or written (the caller
    then warns and falls back to the hostname).

    Concurrent first starts agree on ONE id: whoever publishes first wins and
    every other starter returns the published value — never a value of its own
    that is not on disk (that would give one namespace two live machine
    identities, while every later start uses only the persisted one)."""
    import secrets
    import time

    idfile = _machine_id_path()
    if idfile is None:
        return ""
    existing = _read_machine_id(idfile)
    if existing:
        return existing
    try:
        idfile.parent.mkdir(parents=True, exist_ok=True)
        value = secrets.token_hex(16)
        if _publish_machine_id(idfile, value):
            return value
    except OSError:
        return ""
    # Lost the race: adopt the winner's value. It is normally already complete;
    # wait briefly only for a writer that cannot publish atomically.
    deadline = time.monotonic() + _MACHINE_ID_WAIT_SECONDS
    while True:
        committed = _read_machine_id(idfile)
        # A settled empty file has no writer left to wait for.
        if committed or time.monotonic() >= deadline or _settled(idfile):
            break
        time.sleep(0.01)
    if not committed:
        logger.warning(
            "net plugin: machine id file %s exists but holds no complete id; not "
            "inventing an id that would not be persisted. Delete the file to "
            "regenerate it.",
            idfile,
        )
    return committed


def _machine_label() -> str:
    """The delegation label of :func:`_machine_labels`."""
    return _machine_labels()[0]


def _machine_labels() -> "tuple[str, str]":
    """``(delegation_label, transport_label)`` for this machine, from ONE read
    of the machine id.

    The delegation label must be **unique and persistent** per machine: machine
    + gateway identities are derived deterministically from it
    (`derive_child_seed(root, f"machine:{label}")`), so two machines sharing a
    label produce identical `machine_id` / `gateway_id` from the same root —
    breaking per-machine revocation isolation and conflating audit trails.
    Order: explicit ``NET_MESH_MACHINE_ID`` → a persistent random per-machine id
    (host-prefixed for readability) → **hostname with a loud warning** (the only
    unstable path, and only when no id can be persisted).

    The transport label (what the node's transport identity derives from) is
    hostname-independent: the explicit ``NET_MESH_MACHINE_ID`` or the persistent
    machine id alone. The delegation label keeps its readable host prefix. So
    renaming the host does not rotate the node's mesh identity. Only the
    hostname fallback (no id could be persisted) uses the hostname for both.
    """
    override = (os.environ.get("NET_MESH_MACHINE_ID") or "").strip()
    if override:
        return override, override
    host = _hostname()
    persistent = _persistent_machine_id()
    if persistent:
        label = f"{host}:{persistent}" if host != "unknown-host" else persistent
        return label, persistent
    logger.warning(
        "net plugin: could not establish a unique persistent machine id; "
        "delegation identities derive from the hostname (%s) and may COLLIDE "
        "with another machine sharing it (breaking per-machine revocation). Set "
        "NET_MESH_MACHINE_ID to a unique, stable value.",
        host,
    )
    return host, host


#: Domain separation for :func:`transport_identity_seed` — keyed BLAKE2b, a
#: different function and context than the SDK's blake3 child-seed KDF, so a
#: node's transport key never coincides with its machine / gateway delegation
#: keys derived from the same root.
_TRANSPORT_SEED_PERSON = b"net-hermes-node"
_TRANSPORT_SEED_CONTEXT = b"net-mesh hermes transport identity v1\x00"


def transport_identity_seed(root_seed: bytes, machine_label: str) -> bytes:
    """The 32-byte seed this machine's embedded mesh node runs as, derived from
    the operator ``root_seed`` and the per-machine ``machine_label``.

    The root is signing *authority* (delegation, device enrollment), not a
    transport identity: a node built from the root seed itself has the root's
    entity id and therefore its node id, so every machine under one root would
    be the SAME mesh node — discovery and routing key providers by node id and
    could not tell them apart (HERMES-02). Deriving per machine keeps each
    node distinct, and stable across restarts as long as its label is (the
    persistent machine id, or ``NET_MESH_MACHINE_ID``).

    Keyed BLAKE2b is a PRF, so the result reveals nothing about the root. The
    derived seed is held in Python only as long as the root seed already is
    (both are passed to ``NetMesh`` as bytes), and it carries strictly less
    authority: it can run this one node, not sign delegations.
    """
    import hashlib

    if len(root_seed) != 32:
        raise ValueError(f"root seed must be 32 bytes, got {len(root_seed)}")
    h = hashlib.blake2b(key=root_seed, digest_size=32, person=_TRANSPORT_SEED_PERSON)
    h.update(_TRANSPORT_SEED_CONTEXT)
    h.update(machine_label.encode("utf-8"))
    return h.digest()


def _revocation_store_path() -> Optional[str]:
    """The machine-shared delegation-revocation store path — the same file a
    ``net wrap --owner-root`` provider honors and ``net identity revoke`` writes.

    ``NET_MESH_REVOCATION_STORE`` overrides; otherwise the SDK's per-user default.
    ``None`` when neither resolves — the caller-side check then can't observe a
    revocation (the provider still enforces it on every invoke)."""
    override = (os.environ.get("NET_MESH_REVOCATION_STORE") or "").strip()
    if override:
        return override
    try:
        from net_sdk import default_revocation_store_path

        return default_revocation_store_path()
    except Exception:  # noqa: BLE001 — resolver unavailable ⇒ no caller-side reload
        return None


class GatewayDelegation:
    """The plugin's ``root -> machine -> gateway`` chain plus its shared
    revocation registry and the gateway identity handle (for subagent
    delegation).

    Construction is pure computation over the SDK — no mesh I/O — so it fails
    fast on a bad seed or a wheel missing the ``delegation`` feature (raising
    ``ImportError`` for the latter, which ``node`` treats as "run un-delegated"
    rather than as an acquisition failure).
    """

    def __init__(
        self,
        root_seed: bytes,
        *,
        machine_label: Optional[str] = None,
        gateway_label: str = "hermes",
        ttl_seconds: int = _DEFAULT_TTL_SECONDS,
        revocation_store_path: Optional[str] = None,
    ) -> None:
        # Deferred so importing this module never requires the native wheel;
        # `node` catches the ImportError and degrades to an un-delegated node.
        from net import (  # noqa: PLC0415
            DelegationChain,
            Identity,
            RevocationRegistry,
            derive_child_identity,
        )

        self._machine_label = machine_label or _machine_label()
        self._gateway_label = gateway_label

        root = Identity.from_seed(root_seed)
        machine = derive_child_identity(root, f"machine:{self._machine_label}")
        gateway = derive_child_identity(
            root, f"gateway:{self._machine_label}:{gateway_label}"
        )

        # Public entity-ids (bytes) — the revocation lever is the *machine*
        # issuer id; the *gateway* id is the chain's leaf / presenter.
        self._root_id: bytes = root.entity_id
        self._machine_id: bytes = machine.entity_id
        self._gateway = gateway  # keep the handle: the gateway signs subagents

        self._registry = RevocationRegistry()
        self._chain = DelegationChain.derive_gateway(
            root, machine, gateway, ttl_seconds
        )

        # The machine-shared revocation store `verify()` reloads before each
        # check, so an operator's `net identity revoke` is observed caller-side.
        # An explicit path wins (tests, non-default deployments); otherwise the
        # per-user default (env override or SDK resolver).
        self._revocation_store_path: Optional[str] = (
            revocation_store_path
            if revocation_store_path is not None
            else _revocation_store_path()
        )

    # --- accessors ---------------------------------------------------------

    @property
    def chain(self):
        """The gateway :class:`~net.DelegationChain` (``root -> machine ->
        gateway``)."""
        return self._chain

    @property
    def registry(self):
        """The shared :class:`~net.RevocationRegistry` this chain (and any
        subagent chains extended from it) verify against."""
        return self._registry

    @property
    def gateway_id(self) -> bytes:
        """The gateway (leaf) entity-id — the presenter of the chain."""
        return self._gateway.entity_id

    @property
    def gateway_identity(self):
        """The gateway (leaf) :class:`~net.Identity` **handle** — used to build
        the caller-side signer that signs each invoke. The private key stays
        inside the handle (H8); this is plugin-internal plumbing, never
        model-visible."""
        return self._gateway

    def chain_bytes(self) -> bytes:
        """The serialized gateway chain, for the caller-side signer."""
        return self._chain.to_bytes()

    @property
    def root_id(self) -> bytes:
        """The user-root entity-id the chain anchors at."""
        return self._root_id

    @property
    def machine_id(self) -> bytes:
        """The machine entity-id — the issuer whose floor revokes this
        gateway (and its subagents)."""
        return self._machine_id

    # --- operations --------------------------------------------------------

    def verify(self) -> bool:
        """``True`` iff the gateway chain still verifies — anchored at the
        root, presented by the gateway, and neither expired nor revoked.

        Reloads the machine-shared revocation store first (when a path resolved),
        so an operator's ``net identity revoke`` — written to the same file the
        provider honors — makes this caller-side check fail fast too, not only the
        provider's per-invoke re-verify. Floors are monotonic, so a transient
        store-read error keeps the last-known floors (logged) and never opens a
        hole; the chain's own TTL still bounds a store that can't be read at all.
        """
        if self._revocation_store_path:
            try:
                self._registry.load_from_store(self._revocation_store_path)
            except Exception:  # noqa: BLE001 — keep last-known floors on a read error
                logger.debug(
                    "net plugin: revocation store reload failed; keeping last-known floors",
                    exc_info=True,
                )
        return self._chain.verify(self._gateway.entity_id, self._root_id, self._registry)

    def delegate_subagent(self, subagent_id: bytes):
        """Extend the chain to a subagent entity-id (``... -> gateway ->
        subagent``), signed by the gateway.

        The returned :class:`~net.DelegationChain` is what that subagent
        presents: individually attributable, and (via TTL / non-renewal)
        individually retireable. A ``delegate_task`` handler wires this to the
        spawned subagent's identity (Slice B carries it on the subagent's mesh
        calls).
        """
        return self._chain.extend_to_subagent(self._gateway, subagent_id)

    def revoke_gateway(self) -> None:
        """Revoke this machine's gateway delegation — and, transitively, its
        subagents — by bumping the machine issuer's revocation floor.

        Another machine's gateway under the same user root is untouched (its
        chain is issued by a *different* machine identity), which is exactly
        the Phase-3 acceptance: "revoking one machine's Hermes doesn't touch
        the other machine's."
        """
        self._registry.revoke(self._machine_id)
