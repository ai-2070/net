0. Update docs

1. Update version

2. Merge version branch

3. Draft release note

4. Pull master

5. Run commands

```bash
# Main release
git tag v0.16.0 && git push origin v0.16.0

# Rust crates → crates.io
git tag crates-v0.16.0 && git push origin crates-v0.16.0

# Python binding wheels → PyPI (`ai2070-net`)
git tag python-v0.16.0 && git push origin python-v0.16.0

# Python SDK → PyPI (`ai2070-net-sdk`)
git tag pypi-sdk-v0.16.0 && git push origin pypi-sdk-v0.16.0

# Node binding → npm (`@ai2070/net`)
git tag node-v0.16.0 && git push origin node-v0.16.0

# TS SDK → npm (`@ai2070/net-sdk`)
git tag npm-sdk-v0.16.0 && git push origin npm-sdk-v0.16.0
```


If anything goes wrong during build:
```bash
# Delete release tag
git tag -d 0.16.0 && git push origin --delete 0.16.0
```
