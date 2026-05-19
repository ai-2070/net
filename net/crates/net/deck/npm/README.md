# @net-mesh/deck

The operator cyberdeck — terminal UI for the Net mesh.

```sh
npm install -g @net-mesh/deck
net-deck
```

`@net-mesh/deck` is a thin Node.js shim — installing it pulls in
the right per-platform binary package as an `optionalDependency`
(npm refuses to install packages that don't match the host's
`os` / `cpu` / `libc`). The shim resolves the installed package
at runtime and `exec`s the bundled `net-deck` binary.

Supported targets:

- linux x86_64 (glibc + musl)
- linux aarch64 (glibc + musl)
- macOS x86_64 + aarch64
- Windows x86_64 + aarch64

## Other install paths

- **crates.io** — `cargo install net-deck`
- **cargo-binstall** — `cargo binstall net-deck` (downloads the
  prebuilt tarball from GitHub Releases, no compile)
- **GitHub Releases** — download the tarball / zip directly from
  [the releases page](https://github.com/ai-2070/net/releases)
- **PyPI** — `pip install net-deck`

See the [main README](https://github.com/ai-2070/net) for the
full surface.
