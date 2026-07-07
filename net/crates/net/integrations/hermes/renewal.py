"""Silent grant renewal for an enrolled **device** (Hermes V2 Phase 1).

A device enrolled into an operator's mesh holds a `root -> device` grant with a
long (1-year) lifetime. This service keeps that grant fresh **silently and
automatically** while the device is healthy: a background thread periodically
checks whether the grant is within its renewal window and, if so, renews it over
the mesh (``NetMesh.renew``) and re-persists the refreshed
:class:`~net.DeviceEnrollment`. A **revoked** device is refused renewal by the
operator (its floor was bumped) and simply keeps failing until it re-enrolls —
manual revocation is never waited out.

The renewal handshake, the crypto, and the persistence all live in the Rust SDK
(bridge doctrine H2): this module is only the *scheduler* — decide when, call
``renew``, save the result.
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Callable, Optional

logger = logging.getLogger(__name__)

# How often the background loop wakes to check (seconds). A day is plenty for a
# 1-year grant with a 30-day window; small enough that a machine that was off
# for a while renews soon after it comes back.
_DEFAULT_CHECK_INTERVAL = 24 * 60 * 60

# The floor the check interval is clamped to. `_stop.wait(0)` (a zero/negative
# NET_MESH_RENEWAL_INTERVAL) returns immediately — the loop would spin at 100%
# CPU. A minute is far tighter than any sane renewal cadence needs.
_MIN_CHECK_INTERVAL = 60

# Renew once the grant is within this window of expiry (seconds). 30 days gives
# many retry opportunities before a 1-year grant lapses.
_DEFAULT_RENEWAL_WINDOW = 30 * 24 * 60 * 60


class RenewalService:
    """Keeps a device's `root -> device` grant fresh in the background.

    Construct with the device's live ``mesh`` (a started, permissive
    ``NetMesh``), its current :class:`~net.DeviceEnrollment`, and the path the
    enrollment is persisted at. :meth:`start` spawns the loop; :meth:`stop` is
    idempotent. :meth:`maybe_renew` performs one check+renew and is what the loop
    calls — exposed so callers (and tests) can trigger a check directly.
    """

    def __init__(
        self,
        mesh,
        enrollment,
        path: str,
        *,
        check_interval: int = _DEFAULT_CHECK_INTERVAL,
        renewal_window: int = _DEFAULT_RENEWAL_WINDOW,
        on_renew: Optional[Callable[[object], None]] = None,
    ) -> None:
        self._mesh = mesh
        self._enrollment = enrollment
        self._path = path
        if check_interval < _MIN_CHECK_INTERVAL:
            logger.warning(
                "net plugin: renewal check interval %ss is below the %ss floor; clamping",
                check_interval,
                _MIN_CHECK_INTERVAL,
            )
            check_interval = _MIN_CHECK_INTERVAL
        self._check_interval = check_interval
        self._renewal_window = renewal_window
        self._on_renew = on_renew
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

    @property
    def enrollment(self):
        """The current (possibly just-renewed) enrollment."""
        with self._lock:
            return self._enrollment

    def maybe_renew(self, now: Optional[int] = None) -> bool:
        """Renew iff the grant is within its renewal window. Returns ``True`` if
        it renewed. Renewal failures (a revoked device, an unreachable operator)
        are logged and return ``False`` — the next check retries."""
        now = int(time.time()) if now is None else now
        with self._lock:
            enrollment = self._enrollment
        if not enrollment.needs_renewal(self._renewal_window, now):
            return False
        # Deferred import so this module loads even where the native wheel is
        # absent (the plugin degrades rather than failing to load).
        import net

        try:
            new_chain = self._mesh.renew(enrollment)
        except Exception as e:  # noqa: BLE001 — a failed renewal is not fatal; retry next tick
            logger.info("net plugin: grant renewal failed (%s); will retry", e)
            return False
        renewed = net.DeviceEnrollment(
            enrollment.device, new_chain, enrollment.rendezvous, int(time.time())
        )
        try:
            renewed.save(self._path)
        except Exception:  # noqa: BLE001 — keep the in-memory renewal even if persistence fails
            logger.warning(
                "net plugin: renewed grant could not be persisted", exc_info=True
            )
        with self._lock:
            self._enrollment = renewed
        logger.info(
            "net plugin: device grant renewed (expires_at=%s)", renewed.expires_at
        )
        if self._on_renew is not None:
            try:
                self._on_renew(renewed)
            except Exception:  # noqa: BLE001 — a callback error must not sink renewal
                logger.debug("net plugin: renewal callback failed", exc_info=True)
        return True

    def start(self) -> None:
        """Start the background renewal loop (idempotent)."""
        with self._lock:
            if self._thread is not None:
                return
            self._stop.clear()
            self._thread = threading.Thread(
                target=self._loop, name="net-renewal", daemon=True
            )
            self._thread.start()

    def _loop(self) -> None:
        # Check once promptly on start (a machine that just booted may already be
        # inside the window), then on the interval.
        while not self._stop.is_set():
            try:
                self.maybe_renew()
            except Exception:  # noqa: BLE001 — the loop must survive any check error
                logger.debug("net plugin: renewal check errored", exc_info=True)
            self._stop.wait(self._check_interval)

    def stop(self) -> None:
        """Stop the loop (idempotent; safe to call from any thread)."""
        self._stop.set()
        with self._lock:
            thread = self._thread
            self._thread = None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=5)
