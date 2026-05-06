```bash
# Main release
git tag v0.8.0 && git push origin v0.8.0

# Rust crates → crates.io
git tag crates-v0.8.0 && git push origin crates-v0.8.0

# Python binding wheels → PyPI (`ai2070-net`)
git tag python-v0.8.0 && git push origin python-v0.8.0

# Python SDK → PyPI (`ai2070-net-sdk`)
git tag pypi-sdk-v0.8.0 && git push origin pypi-sdk-v0.8.0

# Node binding → npm (`@ai2070/net`)
git tag node-v0.8.0 && git push origin node-v0.8.0

# TS SDK → npm (`@ai2070/net-sdk`)
git tag npm-sdk-v0.8.0 && git push origin npm-sdk-v0.8.0
```
