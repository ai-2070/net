# net-deck

The operator cyberdeck — terminal UI for the Net mesh, packaged
as a PyPI wheel so it installs cleanly into any Python
environment.

```sh
pip install net-deck
net-deck
```

The wheel bundles the Rust `net-deck` binary directly (built with
[`maturin`](https://github.com/PyO3/maturin)'s `bindings = "bin"`
mode); `pip install` puts it on your `$PATH` with no compilation
step and no Python shim layer.

Supported platforms: linux x86_64 (glibc + musl), linux aarch64,
macOS x86_64 + aarch64, Windows x86_64 + aarch64. A source distribution is
also published for any platform pip can't find a wheel for —
that path needs a Rust toolchain.

## Other install paths

- **crates.io** — `cargo install net-deck`
- **cargo-binstall** — `cargo binstall net-deck`
- **npm** — `npm install -g @net-mesh/deck`
- **GitHub Releases** — prebuilt tarballs at
  https://github.com/ai-2070/net/releases

See the [main README](https://github.com/ai-2070/net) for the
full surface.
