"""A pytest plugin that makes CPython collect far more often.

Loaded with `-p gcstress`. Exists to attack the residual runtime-drop
panic (#38), whose trigger is:

    a garbage collection runs at a moment when the last `Arc` to some
    Tokio runtime is dropped on a thread that is already inside a
    runtime

Ordinary runs sample that badly. CPython's generational collector fires
gen-0 only after `threshold0` net container allocations (700 by
default), so the set of instants at which a collection can happen is
sparse, and the panic needs one of them to coincide with a drop on a
worker thread. Ten repetitions of the same test order re-run the same
sparse sampling; they do not explore it.

Lowering the thresholds increases the density of collection points by
roughly two orders of magnitude, so far more drops land inside a
collection. `gc.freeze()` at the start moves the interpreter's
startup objects out of the tracked set, so the frequent collections
scan live application objects rather than immortal ones.

This does not create a defect that is not there. It changes *when*
collections happen, which is exactly the free variable the panic
depends on. A run under this plugin that never panics is stronger
evidence than a default run that never panics; a run that does panic
is the finding.

Cost: the suite runs several times slower. That is the trade.
"""

from __future__ import annotations

import gc
import os

#: gen0 / gen1 / gen2 thresholds. The default is (700, 10, 10).
#: 50 keeps the suite finishing in a usable time while collecting
#: roughly 14x more often; dropping to 1 collects on nearly every
#: allocation and makes the suite too slow to run repeatedly.
THRESHOLDS = (50, 5, 5)


#: Collections observed, by generation. Reported at the end of the run
#: so the claim "this run collected more often" is measured rather than
#: assumed — setting a threshold does not by itself prove the collector
#: ran more, and an unverified stressor would make a clean sweep look
#: stronger than it is.
_collections = {"count": 0}


def _count(phase, info):
    if phase == "start":
        _collections["count"] += 1


#: Set `NET_GCSTRESS=0` to count collections WITHOUT changing the
#: thresholds. That is the control: the stress claim is "this run
#: collects far more often than a default run", and that comparison
#: needs a default-threshold number measured the same way.
_ARMED = os.environ.get("NET_GCSTRESS", "1") != "0"


def pytest_configure(config):
    if _ARMED:
        # Move interpreter/import-time objects out of the tracked set so
        # the frequent collections below scan application objects, not
        # the thousands of immortal ones every collection would walk.
        gc.freeze()
        gc.set_threshold(*THRESHOLDS)
    gc.callbacks.append(_count)
    config.addinivalue_line("markers", "gcstress: run under lowered GC thresholds")


def pytest_report_header(config):
    if not _ARMED:
        return (
            f"gcstress: CONTROL (NET_GCSTRESS=0) — counting only, thresholds "
            f"left at {gc.get_threshold()}"
        )
    return (
        f"gcstress: gc.set_threshold{THRESHOLDS} "
        f"(default (700, 10, 10)), gc.freeze() applied"
    )


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    terminalreporter.write_line(
        f"gcstress: {_collections['count']} garbage collections during this run"
    )
