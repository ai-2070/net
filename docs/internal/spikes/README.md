# Spike reports

The reports here, [`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
and [`WEBRTC_DOUBLE_AEAD.md`](../performance/WEBRTC_DOUBLE_AEAD.md) cite a
top-level `spikes/` tree: scratch crates (`s0a-wire`, `s0b-rtc`), stage
briefs (`S*_BRIEF.md`), review probes (`kyra/`) and receipt logs. Every
finding in it was either recorded in these documents or landed as a real
test, so the tree was removed. Every `spikes/...` path cited anywhere in
the repository resolves at `4a98529f2`:

    git show 4a98529f2:spikes/<path>
    git worktree add --detach ../spikes-archive 4a98529f2

The one piece CI still ran, the R5-A wire-feature consumer
(`spikes/tools/feature_consumer`), moved to
`net/crates/net/tests/feature_consumer`.
