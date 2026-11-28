"""Plugin lifecycle tests (HERMES-01) that run WITHOUT the native ``net`` wheel.

``test_plugin.py`` skips wholesale when ``net`` is not importable; the hook
wiring and the teardown scope are pure Python, so they are pinned here.
"""

from __future__ import annotations

import atexit


class _Svc:
    def __init__(self, stopped):
        self._stopped = stopped

    def stop(self):
        self._stopped.append(True)


def test_services_survive_every_hook_the_host_runs_between_turns(plugin, ctx, monkeypatch):
    # HERMES-01: Hermes emits on_session_end after EVERY turn and
    # on_session_start only for a new session. Services started for a session
    # must still be running for its next turn, whatever hook the plugin wired.
    monkeypatch.setattr(atexit, "register", lambda _fn: None)
    plugin.register(ctx)

    stopped, shutdowns = [], []
    monkeypatch.setattr(plugin.node, "shutdown", lambda: shutdowns.append(True))
    monkeypatch.setattr(plugin, "_promotion", _Svc(stopped))
    monkeypatch.setattr(plugin, "_a2a_service", _Svc(stopped))
    for callbacks in ctx.hooks.values():
        for hook in callbacks:
            hook()
    assert stopped == [], "a per-turn hook stopped a process-wide service"
    assert shutdowns == [], "a per-turn hook shut the shared node down"
    assert plugin._promotion is not None and plugin._a2a_service is not None


def test_teardown_is_registered_once_for_process_exit(plugin, ctx, monkeypatch):
    exit_hooks = []
    monkeypatch.setattr(atexit, "register", exit_hooks.append)
    monkeypatch.setattr(plugin, "_shutdown_registered", False, raising=False)
    plugin.register(ctx)
    plugin.register(ctx)
    assert len(exit_hooks) == 1, "teardown is registered for process exit, once"

    stopped, shutdowns = [], []
    monkeypatch.setattr(plugin.node, "shutdown", lambda: shutdowns.append(True))
    monkeypatch.setattr(plugin, "_promotion", _Svc(stopped))
    exit_hooks[0]()
    assert stopped == [True] and shutdowns == [True]
    assert plugin._promotion is None
