# binary-size

Measures the shipped artifact sizes that back the **Binary size** table in
the repo-root `README.md`.

```sh
./measure_sizes.sh            # core crate + Node binding + default-set rows
SKIP_NODE=1 ./measure_sizes.sh   # core crate only
```

It builds each feature combo at full-LTO release and records:

- `libnet.dylib` — shipped core cdylib (consumed by Node / Python / C bindings)
- `libnet.rlib` — Rust static lib with metadata (consumed by other Rust crates)
- `libnet.a` — C/C++ static lib, pre-LTO (`staticlib`)
- `libnet_node.dylib` — Node binding cdylib (ships as `net.<triple>.node`)

Results land in `size_results.txt` (pipe-delimited). Numbers are
host-specific — record the target triple and date alongside the README
table when you refresh it.

**Note:** the Node binding's full-LTO link can be very slow and has been
observed to stall on some hosts. Use `SKIP_NODE=1` to measure just the core
crate; fill the Node rows from a host where the link completes, or project
them from the core growth.
