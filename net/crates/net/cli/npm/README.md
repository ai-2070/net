# @net-mesh/cli

Unified command-line interface for the Net mesh — the operational
counterpart to [`@net-mesh/deck`](https://www.npmjs.com/package/@net-mesh/deck).

```sh
npm install -g @net-mesh/cli
# or:
npx @net-mesh/cli --help
```

`@net-mesh/cli` is a thin Node.js shim — installing it pulls in
the right per-platform binary package as an `optionalDependency`
(npm refuses to install packages that don't match the host's
`os` / `cpu` / `libc`). The shim resolves the installed package
at runtime and `exec`s the bundled `net-mesh` binary.

Supported targets:

- linux x86_64 (glibc + musl)
- linux aarch64 (glibc + musl)
- macOS x86_64 + aarch64
- Windows x86_64 + aarch64

## Other install paths

- **crates.io** — `cargo install net-cli`
- **cargo-binstall** — `cargo binstall net-cli` (downloads the
  prebuilt tarball from GitHub Releases, no compile)
- **GitHub Releases** — download the tarball / zip directly from
  [the releases page](https://github.com/ai-2070/net/releases)
- **PyPI** — `pip install net-mesh-cli`

See the [main README](https://github.com/ai-2070/net) for the
full surface.
