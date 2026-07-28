// Preamble injected into every marked Rust snippet.
//
// INTENTIONALLY EMPTY of imports.
//
// DISCIPLINE: imports and harness scaffolding ONLY — no helper functions, no
// shims, nothing that defines a symbol the SDK is supposed to provide. A
// preamble that supplies behaviour makes the job prove the harness instead of
// the documentation.
//
// It started with `use std::sync::Arc;` and `use std::time::Duration;` as a
// convenience. That was wrong twice over: it collided with the snippets that
// (correctly) import those themselves, and it would have let a snippet pass
// while missing an import its reader needs. If a snippet does not compile
// because something is not in scope, the snippet is missing an import — fix the
// snippet, or leave it unmarked. Do not grow this file to make one pass.
//
// Printed in full whenever a snippet fails, so a reader can see exactly what
// was in scope: nothing.
