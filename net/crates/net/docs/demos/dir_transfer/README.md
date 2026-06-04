# Demo — directory transfer at realistic scale

This demo shows `net transfer recv-dir` pulling a whole directory tree
from a holder node and reconstructing it atomically, with throughput that
is largely invariant to file count.

## The headline shape

```
$ time net transfer recv-dir --from <publisher> --remote-ref <hex> --out ./received
[⠋] reconstructing directory from peer <publisher>
{ "peer": <publisher>, "out": "./received", "files": 30247, "dirs": 412,
  "symlinks": 0, "bytes": 537000000, "duration_secs": 12.3,
  "throughput_mib_s": 41.6, "atomic": true }

real    0m12.4s
```

The `atomic: true` field confirms the target was renamed into place from a
sibling temp directory — on any failure the existing target is left
untouched (see `docs/cli/TRANSFER.md` §4 and `FETCH_DIR_ATOMIC_PLAN.md`).

## Runnable end-to-end proof

A self-contained shell script would need a long-running holder process
serving the blob-transfer engine. The substrate does not (yet) expose a
`net transfer serve` daemon verb, so the **executable** end-to-end demo
lives as an integration test that boots an in-process holder `Mesh`,
serves a stored directory, and drives the real `net-mesh` binary as a
subprocess over the routed-attach path:

```sh
# Real recv-dir reconstruction over the mesh, asserting byte-for-byte
# equality + atomic reconstruction (no temp dir left behind):
cargo test -p net-cli --test transfer_cli_dir -- --nocapture

# The single-blob analogue:
cargo test -p net-cli --test transfer_cli_blob -- --nocapture
```

These are the same code paths a production fetch exercises; the test
harness only stands in for the holder daemon.

## Manual walkthrough (two hosts)

On the **publisher** (a node already running the blob-transfer engine over
a store directory):

```sh
net transfer send-dir ./source_directory --store /var/lib/net/blobs --output json
# → prints remote_ref=<hex>, manifest_size, staged_to
```

On the **fetcher**:

```sh
net transfer recv-dir \
  --from <publisher-node-id> \
  --remote-ref <hex> \
  --out ./received \
  --node-addr <publisher-ip:port> \
  --node-pubkey <hex> \
  --node-id <publisher-node-id> \
  --psk-hex <hex>
```

See `docs/cli/TRANSFER.md` for the full flag reference, atomicity
guarantees, and failure-mode recovery.
