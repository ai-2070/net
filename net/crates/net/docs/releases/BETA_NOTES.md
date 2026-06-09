Package versions that become the beta (7):**
- `crates/net/Cargo.toml` (net-mesh) → `0.27.1-beta.1`
- `sdk/Cargo.toml` (net-mesh-sdk) → `0.27.1-beta.1`
- `sdk-macros/Cargo.toml` (net-mesh-sdk-macros) → `0.27.1-beta.1`
- `bindings/node/Cargo.toml` (net-node) → `0.27.1-beta.1`
- `bindings/python/Cargo.toml` (net-python) → `0.27.1-beta.1`
- `bindings/node/package.json` (@net-mesh/core) → `0.27.1-beta.1`
- `sdk-ts/package.json` (@net-mesh/sdk) → `0.27.1-beta.1`
- `bindings/python/pyproject.toml` (PyPI `net-mesh`) → `0.27.1b1`

**Internal dependency edges that point at one of the above (these get the beta version too):**
- `sdk/Cargo.toml:25` `net-mesh version = "0.27.1"` → `0.27.1-beta.1`
- `sdk/Cargo.toml:43` `net-mesh-sdk-macros version = "0.27.1"` → `0.27.1-beta.1`
- `bindings/node/Cargo.toml:138-139` `net-mesh` + `net-mesh-sdk` → `0.27.1-beta.1`
- `bindings/python/Cargo.toml:119-120` `net-mesh` + `net-mesh-sdk` → `0.27.1-beta.1`
- `sdk-ts/package.json` peer `@net-mesh/core ">=0.27.1"` → `">=0.27.1-0"
