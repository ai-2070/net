# Internal engineering docs

Working history for the Net substrate: design plans, code reviews, bug and
security audits, and performance analyses. **Not** user documentation.

| Folder | What's in it |
|---|---|
| [`plans/`](plans/) | Design and implementation plans, one per feature track. Written before the work, amended as it lands. |
| [`misc/`](misc/) | Code reviews, bug audits, security audits, perf audits — dated, point-in-time findings with resolutions. |
| [`performance/`](performance/) | Benchmark analyses and hot-path studies. |

These are a record of *how decisions were reached*, kept because the reasoning
is often more useful than the outcome. They are frequently stale by design — a
plan describes intent at a moment, not the current shape of the code. **Read
the code, or the docs below, for what is true today.**

## Where the current docs live

| Audience | Location |
|---|---|
| Users, operators, agents | [`web/src/content/docs/`](../../web/src/content/docs/) — renders as the docs site |
| Protocol / wire-level engineering | [`net/crates/net/docs/`](../../net/crates/net/docs/) — ships with the `net-mesh` crate |
| Release notes | [`web/src/content/docs/releases/`](../../web/src/content/docs/releases/) |

## Why this is outside the crate

These files used to live at `net/crates/net/docs/{plans,misc,performance}/`.
The `net-mesh` crate declares no `include`/`exclude`, so `cargo package` swept
all 244 of them — 7 MB of internal review history, including security audits —
into every publish to crates.io, where they were browsable on docs.rs.

Moving them out of the crate directory fixes that at the source: nothing here
is reachable by `cargo package`, so no `exclude` list has to be maintained in
step with the folder layout.
