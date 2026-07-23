# Contributing to Net

Thanks for your interest in Net. This document covers the contribution
agreement and the basics of getting a change merged.

## Contributor License Agreement

Net requires every contributor to sign a CLA before their first contribution is
merged. It records a license to the Project so that Net can be distributed under
its dual [MIT](LICENSE-MIT) / [Apache-2.0](LICENSE-APACHE) license without
ambiguity about provenance.

**You keep full copyright ownership of your Contributions.** The CLA is a
license grant, not an assignment — you may reuse your own work anywhere else,
for any purpose.

**Released code stays under the license it shipped under, permanently.** Both
grants are irrevocable: every published version remains available under those
terms, and anyone may fork from the last released commit at any time. A change
of license affecting future development could not reach back and withdraw what
has already shipped.

| You are… | Sign |
| --- | --- |
| An individual contributing on your own behalf | [Individual CLA](CLA/individual-cla.md) |
| Contributing work your employer owns the copyright in | [Corporate CLA](CLA/corporate-cla.md) — and your employer must execute it |

### Signing the Individual CLA

Open your pull request as usual. A bot will comment if you have not signed yet.
Reply on the PR with exactly:

```
I have read the CLA Document and I hereby sign the CLA
```

Your signature is recorded once and covers all of your future contributions.

### Signing the Corporate CLA

The Corporate CLA is executed out of band. Email a completed and signed copy,
including Schedule A listing your authorized employees, to the address in the
document.

## Pull requests

- Branch from `master` and open the PR against `master`.
- Keep the change focused; unrelated cleanups are easier to review separately.
- CI runs the Rust, Node, Python, and Go suites plus clippy and rustfmt. Every
  job must be green before merge.

Useful local checks before pushing (run from `net/`):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets
cargo test --lib
```

Go bindings live in `go/` (`go test ./...`), the web docs in `web/`.

## Reporting security issues

Please do not open a public issue for a security vulnerability. Report it
privately to makerseven7@gmail.com.
