0. Update docs

1. Update version

2. Update codename

3. Merge version branch

4. Draft release note

5. Pull master

6. Run commands

```bash
# Main release
git tag vX && git push origin vX

# Rust crates → crates.io
git tag crates-vX && git push origin crates-vX

# Python binding wheels → PyPI (`net-mesh`)
git tag python-vX && git push origin python-vX

# Python SDK → PyPI (`net-mesh-sdk`)
git tag pypi-sdk-vX && git push origin pypi-sdk-vX

# Node binding → npm (`@net-mesh/core`)
git tag node-vX && git push origin node-vX

# TS SDK → npm (`@net-mesh/sdk`)
git tag npm-sdk-vX && git push origin npm-sdk-vX

# CLI → crates.io (`net-cli`) + GitHub Release tarballs +
# npm (`@net-mesh/cli`) + PyPI (`net-mesh-cli`).
# One tag fans out to four parallel workflows.
git tag cli-vX && git push origin cli-vX

# Deck → crates.io (`net-deck`) + GitHub Release tarballs +
# npm (`@net-mesh/deck`) + PyPI (`net-deck`).
# One tag fans out to four parallel workflows.
git tag deck-vX && git push origin deck-vX
```


If anything goes wrong during build:
```bash
# Delete release tag
git tag -d X && git push origin --delete X
```
