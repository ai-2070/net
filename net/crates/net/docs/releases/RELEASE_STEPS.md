0. Update docs

1. Update version

2. Merge version branch

3. Draft release note

4. Run commands

```bash
# Main release
git tag vX && git push origin vX

# Rust crates → crates.io
git tag crates-vX && git push origin crates-vX

# Python binding wheels → PyPI (`ai2070-net`)
git tag python-vX && git push origin python-vX

# Python SDK → PyPI (`ai2070-net-sdk`)
git tag pypi-sdk-vX && git push origin pypi-sdk-vX

# Node binding → npm (`@ai2070/net`)
git tag node-vX && git push origin node-vX

# TS SDK → npm (`@ai2070/net-sdk`)
git tag npm-sdk-vX && git push origin npm-sdk-vX
```


If anything goes wrong during build:
```bash
# Delete release tag
git tag -d X && git push origin --delete X
```
