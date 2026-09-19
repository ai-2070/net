"""Apply / restore the inverse mutation for one round-6 core item.

Usage: python inverse.py <item> apply|restore
Exits non-zero if the expected text is not found exactly once, so a
receipt can never be taken against a mutation that did not land.
"""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "net" / "crates" / "net"
MESH = ROOT / "src" / "adapter" / "net" / "mesh.rs"
FRAG = ROOT / "src" / "adapter" / "net" / "rtc" / "fragment.rs"

# item -> list of (file, repaired_text, mutated_text)
ITEMS = {
    # R4-6: the reliability guard and the peer-facing StreamReset.
    "R4-6": [
        (
            MESH,
            "        if !group.provenance.reliable {\n",
            "        if false {\n",
        ),
        (
            MESH,
            "        session.reset_rx_stream(stream_id);\n        reassembly.retire_stream(group.session_id, stream_id, group.epoch, now);\n",
            "        session.reset_rx_stream(stream_id);\n        session.note_receive_terminal(stream_id);\n        reassembly.retire_stream(group.session_id, stream_id, group.epoch, now);\n",
        ),
    ],
    # R4-7: the close-time retirement of the closed lifetime's groups.
    "R4-7": [
        (
            MESH,
            "            let closing = peer.session.try_stream(stream_id).map(|s| s.epoch());\n",
            "            let closing: Option<u64> = None;\n",
        ),
    ],
    # R4-8: the non-expiring authority read at the guarded insertion.
    "R4-8": [
        (
            FRAG,
            "        if retired() {\n            return Err(FragmentOutcome::Retired);\n        }\n",
            "        if false {\n            return Err(FragmentOutcome::Retired);\n        }\n",
        ),
    ],
    # R4-1: old-handle inheritance on a promoted stream.
    "R4-1": [
        (
            MESH,
            "        let reliable = stream.config().reliability.is_reliable()\n            || session\n                .try_stream(stream_id)\n                .is_some_and(|s| s.tx_promoted());\n",
            "        let reliable = stream.config().reliability.is_reliable();\n",
        ),
    ],
    # R5-N1: whole-group sequence reservation.
    "R5-N1": [
        (
            MESH,
            "                pieces as u32,\n",
            "                1_u32, // INVERSE MUTATION R5-N1\n",
        ),
    ],
    # R5-N2: whole-call descriptor admission.
    "R5-N2": [
        (
            MESH,
            "            Some(state)\n                if reliable\n                    && state\n                        .with_reliability(|r| r.retransmit_headroom())\n                        .is_some_and(|headroom| headroom < packets) =>\n",
            "            Some(state)\n                if reliable\n                    && state\n                        .with_reliability(|r| r.retransmit_headroom())\n                        .is_some_and(|headroom| headroom < 0) =>\n",
        ),
    ],
}


def main() -> int:
    item, action = sys.argv[1], sys.argv[2]
    for path, repaired, mutated in ITEMS[item]:
        src = path.read_text(encoding="utf-8")
        frm, to = (repaired, mutated) if action == "apply" else (mutated, repaired)
        if src.count(frm) != 1:
            print(f"FAIL {path.name}: expected exactly one occurrence, found {src.count(frm)}")
            return 2
        path.write_text(src.replace(frm, to), encoding="utf-8", newline="")
    print(f"{action} {item} ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
