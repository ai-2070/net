# S0 Raw Inverse Receipts — Lifecycle model (slice 0.3 + lifecycle-side composition checks)

Target file (exclusive ownership): `net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs`.
Sibling file `org_stream_registry.rs` and every other file: read-only here.

Baseline sha256 of the target file before the first mutation
(`sha256sum net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs`, 2041 lines):

```
7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc
```

Every receipt below: witness full name(s) under
`adapter::net::behavior::org_stream_lifecycle::`, the mutation as a unified diff
of the bounded production-site hunk (never inside `mod tests`), the exact
command (from `net/crates/net/`), the exit code, the verbatim failing assertion
lines, the restore proof (post-restore sha256 == baseline), and the restored
run's verbatim PASS line. Restore method for every mutation: the Edit tool
applied in reverse (no whole-file write, no `cp`, no `git checkout --`).

Common command prefix (from `net/crates/net/`), shown in full per receipt:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=FULL_TEST_NAME>)'
```

<!-- RECEIPTS APPENDED BELOW -->

### (a) Server-streaming starts `input = Ended` — witness `adapter::net::behavior::org_stream_lifecycle::tests::server_streaming_starts_with_its_input_half_already_ended`

Mutation (production site `CallLifecycle::new`, line 360) — inverse: the record starts ServerStreaming with `Input::Open`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -357,7 +357,7 @@ impl CallLifecycle {
     pub fn new(shape: CallShape, incarnation: u64) -> Self {
         let input = match shape {
-            CallShape::ServerStreaming => Input::Ended,
+            CallShape::ServerStreaming => Input::Open,
             CallShape::ClientStreaming | CallShape::Duplex => Input::Open,
         };
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::server_streaming_starts_with_its_input_half_already_ended)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::server_streaming_starts_with_its_input_half_already_ended' (146420) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1224:9:
    assertion `left == right` failed
      left: Open
     right: Ended
```

(`Summary [   0.030s] 1 test run: 0 passed, 1 failed, 5830 skipped`; incremental rebuild 17 s.)

Restore: reverse anchored Edit (`Input::Open` → `Input::Ended` at line 360). `sha256sum` before first mutation `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc`; after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` — identical. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::server_streaming_starts_with_its_input_half_already_ended
```

### (b) END idempotent, never touches output — witness `adapter::net::behavior::org_stream_lifecycle::tests::end_is_idempotent_and_never_touches_the_output_half`

Mutation (production site `CallLifecycle::on_frame`, `Frame::End` arm, lines 428-432) — inverse: the END arm also ends the output half:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -426,8 +426,9 @@ impl CallLifecycle {
             Frame::End => match self.input {
                 Input::Open => {
                     self.input = Input::Ended;
+                    self.output = Output::Ended;
                     Disposition::Ignored
                 }
                 // Idempotent, and never touches `output`: half-close
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::end_is_idempotent_and_never_touches_the_output_half)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::end_is_idempotent_and_never_touches_the_output_half' (153508) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1235:9:
    assertion `left == right` failed
      left: Ended
     right: Open
```

(`Summary [   0.171s] 1 test run: 0 passed, 1 failed, 5830 skipped`.)

Restore: reverse anchored Edit (drop the `self.output = Output::Ended;` line). `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::end_is_idempotent_and_never_touches_the_output_half
```

### (c) Handler return with queued output and zero credit is NOT terminal; a later GRANT completes the drain to `Completed(Ok)` — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it` AND `adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime`

One mutation (production site `CallLifecycle::handler_returned`, lines 451-455) reddens both witnesses. Inverse: `handler_returned` commits a terminal immediately, skipping `Draining`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -448,7 +448,11 @@ impl CallLifecycle {
         if self.terminal.is_some() || !matches!(self.output, Output::Open) {
             return false;
         }
-        self.output = Output::Draining(result);
+        self.output = Output::Ended;
+        self.terminal = Some(Terminal {
+            reason: TerminalReason::Completed(result),
+            emission: None,
+        });
         if self.input == Input::Open {
             self.input = Input::Closed;
         }
```

Command (from `net/crates/net/`), tokio witness:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it' (149020) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1485:9:
    handler finished, so output is draining
```

Command (from `net/crates/net/`), runtime-free witness:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime' (159400) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1943:13:
    assertion `left == right` failed
      left: Terminal(Completed(Ok))
     right: Blocked
```

Restore: reverse anchored Edit (put `Output::Draining(result)` back, drop the committed terminal block). `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same commands) exit 0, PASS lines:

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it
```

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime
```

### (d)-1a Deadline preempts a drain and discards the remainder (supervisor) — witness `adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder`

Mutation (production site `run_supervisor`, line 777) — inverse: the supervisor's deadline arm is disarmed:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -774,7 +774,7 @@ pub async fn run_supervisor(
     };
     let mut sleep = std::pin::pin!(sleep);
-    let expiry_armed = true;
+    let expiry_armed = false;
 
     let forced: Option<TerminalReason> = loop {
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder)'
```

Exit code: **100**. Verbatim failure output (assertion lines) — the red is the `MODEL_BOUND` timeout, as required:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder' (157352) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1528:14:
    a drain nobody credits must still be bounded by the deadline: Elapsed(())
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder
```

### (d)-1b Deadline preempts a blocked drain (runtime-free driver) — witness `adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder`

The runtime-free driver has no supervisor `select!`; its deadline arm is the expiry check in `pull::PullCall::advance`. Mutation (production site, lines 1044-1049) — inverse: the deadline check is disarmed (never fires):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -1042,10 +1042,8 @@ impl PullCall {
             // publish an item after its end.
             if let Some((end, reason)) = self.deadline.clone() {
                 if now_ns >= end {
-                    self.state.retire(reason);
-                    return self.advance(now_ns);
+                    let _ = reason;
                 }
             }
```

(`now_ns`/`self.deadline` stay read so the mutation is lint-clean; only the retire-on-expiry effect is removed.)

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder' (159532) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1964:13:
    assertion `left == right` failed
      left: Blocked
     right: Terminal(Timeout)
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder
```

### (d)-2 CANCEL preempts a drain — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion` AND `adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end`

Mutation (production site `CallLifecycle::on_frame`, `Frame::Cancel` arm, line 415) — inverse: CANCEL is a no-op while `Draining`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -412,7 +412,10 @@ impl CallLifecycle {
             // walked away should not have to wait out a drain it will
             // never credit.
-            Frame::Cancel => Disposition::Retire(TerminalReason::Cancelled),
+            Frame::Cancel => match self.output {
+                Output::Draining(_) => Disposition::Ignored,
+                _ => Disposition::Retire(TerminalReason::Cancelled),
+            },
             // Credit is what lets a drain finish, so it must survive the
```

Command (from `net/crates/net/`), state witness:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion' (156728) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1291:9:
    assertion `left == right` failed
      left: Ignored
     right: Retire(Cancelled)
```

Command (from `net/crates/net/`), end-to-end witness:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end)'
```

Exit code: **100**. Verbatim failure output (assertion lines, message included):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end' (155792) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1711:9:
    assertion `left == right` failed: CANCEL must remain admissible while draining
      left: Ignored
     right: Retire(Cancelled)
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same commands) exit 0, PASS lines:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion
```

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end
```

### (d)-3a Revocation/retire stops a credit-parked pump — prescribed inverse EXECUTED LITERALLY: **FINDING** (both named witnesses green under it) — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound` and `adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain` (the latter also named by item (i))

Assignment's prescribed inverse: "remove the semaphore closes on the retire path so a credit-parked pump never wakes (the `MODEL_BOUND` timeout must then be the red)". Executed literally (production site `run_supervisor` retire path, lines 812-815):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -809,9 +809,7 @@ pub async fn run_supervisor(
     // "no chunk is published after the terminal" — not the abort call.
     if let Some(reason) = forced.clone() {
         state.lock().retire(reason);
-        pump.credit.close();
-        pump.bytes.close();
         if !pump_done {
             pump_task.as_mut().abort();
```

Commands (from `net/crates/net/`) as in the header with
`-E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound)'`
and
`-E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain)'`.

Exit codes: **0 (both GREEN under the inverse)** — not a red. Verbatim output:

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain
```

**Finding 1 (diagnosis).** `revocation_while_parked_on_credit_retires_within_the_bound` cannot discriminate the semaphore `close()`s: the property "retirement bounds a credit-parked pump" is **overdetermined** in this model. `run_supervisor`'s retire path wakes the pump by two independent mechanisms — `Semaphore::close` (`acquire_owned` errors → `Err(_) => break`) **and** `JoinHandle::abort()` + `await`. Removing only the closes leaves `abort()`+join, which cancels and joins the parked pump exactly as well, so the supervisor still completes with `terminal = Revoked`, `published = 0`, `emission = Queued` inside `MODEL_BOUND`. The stated expectation ("the `MODEL_BOUND` timeout must then be the red") does not follow from the literal mutation; it requires the pump to be neither woken nor cancelled. Receipt (d)-3b executes that intent-realizing mutation and gets the timeout red. Nothing in the witness was weakened or re-pinned.

**Finding 2 (diagnosis).** `runtime_free::retirement_preempts_a_blocked_drain` drives `pull::PullCall`, which owns no semaphore and no supervisor task: a `run_supervisor` mutation cannot reach it at all. Its own inverse is in the state machine's retire admissibility — receipt (i) executes it (`retire` refusing while `Draining`) and gets its red there.

Restore: reverse anchored Edit (the two `close()` lines put back). `sha256sum` before first mutation `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc`; after this restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` — identical. Restored runs (same commands) exit 0, PASS lines:

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound
```

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain
```

### (d)-3b Deadline / CANCEL / revocation stop a credit-parked pump — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound`, `adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder`, `adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end` (item (i) names the first)

Mutation (production site `run_supervisor` retire path, lines 812-818) — the intent-realizing inverse of "the semaphore closes wake a credit-parked pump": the retire path now neither wakes (`Semaphore::close`) nor cancels (`JoinHandle::abort`) the pump; it only joins it, so a credit-parked pump can never wake and the join hangs:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -809,12 +809,8 @@ pub async fn run_supervisor(
     // "no chunk is published after the terminal" — not the abort call.
     if let Some(reason) = forced.clone() {
         state.lock().retire(reason);
-        pump.credit.close();
-        pump.bytes.close();
         if !pump_done {
-            pump_task.as_mut().abort();
             let _ = pump_task.as_mut().await;
         }
```

Why the wider hunk is required: as (d)-3a established, `abort()`+join alone cancels a parked pump just as `close()` does, so removing only the closes leaves the property intact (witnesses green). This mutation removes every wake/cancel mechanism on the retire path while keeping the join — precisely "a credit-parked pump never wakes".

Commands (from `net/crates/net/`), each exit **100**; the red in every case is the `MODEL_BOUND` timeout:

1. `-E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound)'` (item (i)'s named red):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound' (149828) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1561:14:
    retirement must not wait for the handler or the pump: Elapsed(())
```

2. `-E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder)'`:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder' (154812) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1525:14:
    a drain nobody credits must still be bounded by the deadline: Elapsed(())
```

3. `-E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end)'`:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end' (158332) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1714:14:
    finishes: Elapsed(())
```

Restore: reverse anchored Edit (`close()`s and `abort()` put back). `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same three commands) exit 0, PASS lines:

```
        PASS [   0.013s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::the_deadline_fires_while_draining_and_discards_the_remainder
```

```
        PASS [   0.013s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound
```

```
        PASS [   0.013s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::cancel_during_a_drain_preempts_the_completion_end_to_end
```

### (i) A pump parked on zero credit is stopped by retire — witness `adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain` (and `adapter::net::behavior::org_stream_lifecycle::tests::revocation_while_parked_on_credit_retires_within_the_bound`, covered by (d)-3a/(d)-3b above)

The assignment pairs (i) with (d)'s semaphore mutation; per (d)-3a that mutation cannot reach the runtime-free driver, so this receipt executes the witness's **own** inverse in the shared state machine. Mutation (production site `CallLifecycle::retire`, line 486) — inverse: retirement is not admissible while `Draining` (the drain cannot be preempted):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -483,7 +483,7 @@ impl CallLifecycle {
     /// later END, handler return or pump exit are no-ops.
     pub fn retire(&mut self, reason: TerminalReason) -> bool {
-        if self.terminal.is_some() {
+        if self.terminal.is_some() || matches!(self.output, Output::Draining(_)) {
             return false;
         }
```

Command (from `net/crates/net/`), named witness:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain' (159344) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1978:13:
    assertion failed: call.retire(TerminalReason::Revoked)
```

Every other red under this mutation (both executed and named here, per the grouping rule):

- `adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion` — exit **100**:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion' (158704) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1292:9:
    assertion failed: call.retire(TerminalReason::Cancelled)
```

- `adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state` — exit **100** (its `Draining`-state leg):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state' (159128) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1316:9:
    assertion failed: call.retire(TerminalReason::Timeout)
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same three commands) exit 0, PASS lines:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::retirement_preempts_a_blocked_drain
```

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::cancel_is_admissible_while_draining_and_preempts_completion
```

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state
```

### (e) Handler `Err` survives the drain as `Completed(Err(..))`, never `Ok` — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::a_handler_error_survives_the_drain_as_the_terminal` AND `adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_handler_error_is_the_terminal_here_too`

Mutation (production site `CallLifecycle::pump_exited`, line 467) — inverse: `pump_exited` maps `Draining(Err(..))` to `Completed(HandlerResult::Ok)`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -464,7 +464,7 @@ impl CallLifecycle {
         }
         let reason = match std::mem::replace(&mut self.output, Output::Ended) {
-            Output::Draining(result) => TerminalReason::Completed(result),
+            Output::Draining(_) => TerminalReason::Completed(HandlerResult::Ok),
             Output::Open => TerminalReason::PumpFailed,
```

Command (from `net/crates/net/`), state witness — exit **100**:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::a_handler_error_survives_the_drain_as_the_terminal)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::a_handler_error_survives_the_drain_as_the_terminal' (153744) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1263:9:
    assertion `left == right` failed
      left: Completed(Ok)
     right: Completed(Err(32769, "nope"))
```

Command (from `net/crates/net/`), runtime-free witness — exit **100**:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_handler_error_is_the_terminal_here_too)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_handler_error_is_the_terminal_here_too' (154372) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:2032:13:
    assertion `left == right` failed
      left: Some(Completed(Ok))
     right: Some(Completed(Err(32769, "nope")))
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same commands) exit 0, PASS lines:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::a_handler_error_survives_the_drain_as_the_terminal
```

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::runtime_free::a_handler_error_is_the_terminal_here_too
```

### (f) Handler return closes an open input half; later CHUNKs dropped, END a no-op — witness `adapter::net::behavior::org_stream_lifecycle::tests::handler_return_closes_an_open_input_half_and_preserves_the_result`

Mutation (production site `CallLifecycle::handler_returned`, lines 451-456) — inverse: `handler_returned` leaves `input = Open`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -448,9 +448,6 @@ impl CallLifecycle {
         if self.terminal.is_some() || !matches!(self.output, Output::Open) {
             return false;
         }
         self.output = Output::Draining(result);
-        if self.input == Input::Open {
-            self.input = Input::Closed;
-        }
         true
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::handler_return_closes_an_open_input_half_and_preserves_the_result)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::handler_return_closes_an_open_input_half_and_preserves_the_result' (158996) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1243:9:
    assertion `left == right` failed
      left: Open
     right: Closed
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::handler_return_closes_an_open_input_half_and_preserves_the_result
```

### (g) Retire is first-writer-wins from every state incl. `Draining` — witness `adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state`

Mutation (production site `CallLifecycle::retire`, lines 485-488) — inverse: `retire` overwrites an existing terminal (first-writer guard removed):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -482,9 +482,6 @@ impl CallLifecycle {
     /// later END, handler return or pump exit are no-ops.
     pub fn retire(&mut self, reason: TerminalReason) -> bool {
-        if self.terminal.is_some() {
-            return false;
-        }
         self.terminal = Some(Terminal {
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state' (147884) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1302:13:
    assertion failed: !call.retire(TerminalReason::Cancelled)
```

Restore: reverse anchored Edit (guard put back; the plain function-line anchor is ambiguous against `PullCall::retire`, so the restore edit carries the doc-comment context). `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::retire_is_first_writer_wins_from_every_state
```

### (h)-1 Frames after the terminal are dropped — witness `adapter::net::behavior::org_stream_lifecycle::tests::every_frame_after_the_terminal_is_dropped`

Mutation (production site `CallLifecycle::on_frame`, lines 408-411) — inverse: the terminal guard is removed, so frames keep being classified after the terminal:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -405,9 +405,6 @@ impl CallLifecycle {
         // already selected and re-entering retirement would race the
         // emission owner.
-        if self.terminal.is_some() {
-            return Disposition::Ignored;
-        }
         match frame {
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::every_frame_after_the_terminal_is_dropped)'
```

Exit code: **100**. Verbatim failure output (assertion lines, frame name included):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::every_frame_after_the_terminal_is_dropped' (157180) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1325:13:
    assertion `left == right` failed: Chunk
      left: Deliver
     right: Ignored
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::every_frame_after_the_terminal_is_dropped
```

### (h)-2 GRANT during `Draining` credited; CHUNK/END during `Draining` dropped; GRANT after output ended dropped — witness `adapter::net::behavior::org_stream_lifecycle::tests::grants_stay_admissible_while_draining_and_stop_once_output_ended` (+ collateral red `adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it`)

(No single mutation reddens both (h) witnesses: removing the terminal guard (receipt (h)-1) leaves this one green, and vice versa — so (h) is two receipts, one per inverse, per the contract's grouping rule.)

Mutation (production site `CallLifecycle::on_frame`, `Frame::Grant` arm, lines 419-422) — inverse: the `Draining`-credit arm is dropped (a draining call can no longer be credited):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -417,8 +417,8 @@ impl CallLifecycle {
             // credit.
             Frame::Grant => match self.output {
-                Output::Open | Output::Draining(_) => Disposition::Credit,
-                Output::Ended => Disposition::Ignored,
+                Output::Open => Disposition::Credit,
+                Output::Draining(_) | Output::Ended => Disposition::Ignored,
             },
```

Command (from `net/crates/net/`), named witness — exit **100**:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::grants_stay_admissible_while_draining_and_stop_once_output_ended)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::grants_stay_admissible_while_draining_and_stop_once_output_ended' (155944) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1274:9:
    assertion `left == right` failed: a drain that needs credit must be able to receive it
      left: Ignored
     right: Credit
```

Every other red under this mutation (executed and named):

- `adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it` — exit **100**:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it' (140964) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1489:9:
    assertion `left == right` failed
      left: Ignored
     right: Credit
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same two commands) exit 0, PASS lines:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::grants_stay_admissible_while_draining_and_stop_once_output_ended
```

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it
```

### (j) Terminal emitted exactly once, after pump stop — witness `adapter::net::behavior::org_stream_lifecycle::tests::the_terminal_is_emitted_exactly_once`

Mutation (production site `CallLifecycle::record_emission`, lines 535-541) — inverse: `record_emission` records repeatedly (not one-shot):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -533,7 +533,7 @@ impl CallLifecycle {
     /// disposition wins like every other terminal write.
     pub fn record_emission(&mut self, disposition: TerminalDisposition) -> bool {
         match self.terminal.as_mut() {
-            Some(terminal) if terminal.emission.is_none() => {
+            Some(terminal) => {
                 terminal.emission = Some(disposition);
                 true
             }
-            _ => false,
+            None => false,
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::the_terminal_is_emitted_exactly_once)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::the_terminal_is_emitted_exactly_once' (158432) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1368:9:
    the first disposition wins like every other terminal write
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::the_terminal_is_emitted_exactly_once
```

### (k) Queued-data policy per terminal reason matches §2.2's table — witness `adapter::net::behavior::org_stream_lifecycle::tests::only_a_completion_drains_queued_output`

Mutation (production site `TerminalReason::drains_queued_output`, line 282) — inverse: it returns true for a retirement reason (`Revoked`) as well:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -279,7 +279,7 @@ impl TerminalReason {
     /// drains; every retirement discards.
     pub fn drains_queued_output(&self) -> bool {
-        matches!(self, TerminalReason::Completed(_))
+        matches!(self, TerminalReason::Completed(_) | TerminalReason::Revoked)
     }
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::only_a_completion_drains_queued_output)'
```

Exit code: **100**. Verbatim failure output (assertion lines — the per-reason loop names the offending reason):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::only_a_completion_drains_queued_output' (155376) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1395:13:
    Revoked
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::only_a_completion_drains_queued_output
```

### (l) Two calls independent — witness `adapter::net::behavior::org_stream_lifecycle::tests::two_calls_are_independent` — **classification: plumbing (no credited inverse)**

Per witness discipline and the assignment ("If no discriminating production inverse exists at model level, say so and classify it as plumbing … do not invent one"): the property "two calls are independent" is true **by construction** in this model — `CallLifecycle` values are separate owned locals with no shared or global state, no registry, no static. There is no production site whose bounded mutation could couple two records without inventing state the model does not have; a mutation to "make calls share state" would be adding a defect outside the file's design, not falsifying a claim it makes. The test is therefore plumbing: it exercises construction and reads, and would not discriminate any model bug in the receipted set. It is kept (it documents intent and would catch a future introduction of shared state) but carries no inverse credit here.

Evidence of existence and green baseline (pristine tree, from `net/crates/net/`), exit 0:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::two_calls_are_independent)'
```

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::two_calls_are_independent
```

### (m) Undeliverable/over-budget admitted item retires the call `ResourceExhausted`; no `Ok` reachable after — witnesses `adapter::net::behavior::org_stream_lifecycle::tests::an_undeliverable_admitted_input_item_kills_the_call` AND `adapter::net::behavior::org_stream_lifecycle::tests::an_oversized_item_never_waits_for_permits_it_cannot_get`

One mutation (production site `CallLifecycle::input_admission_failed`, lines 500-509) reddens both witnesses. Inverse: `input_admission_failed` does not retire:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -498,11 +498,7 @@ impl CallLifecycle {
     pub fn input_admission_failed(&mut self) -> Option<TerminalReason> {
         if self.input != Input::Open {
             return None;
         }
-        if self.retire(TerminalReason::ResourceExhausted) {
-            Some(TerminalReason::ResourceExhausted)
-        } else {
-            None
-        }
+        None
     }
```

Command (from `net/crates/net/`), first witness — exit **100**:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::an_undeliverable_admitted_input_item_kills_the_call)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::an_undeliverable_admitted_input_item_kills_the_call' (141488) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1341:9:
    assertion `left == right` failed
      left: None
     right: Some(ResourceExhausted)
```

Command (from `net/crates/net/`), second witness — exit **100**:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::an_oversized_item_never_waits_for_permits_it_cannot_get)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::an_oversized_item_never_waits_for_permits_it_cannot_get' (157752) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1730:9:
    assertion `left == right` failed: an unsatisfiable reservation retires the call instead of parking
      left: None
     right: Some(ResourceExhausted)
```

Restore: reverse anchored Edit (unique via the `self.input` guard — the sibling `output_admission_failed` carries `self.output`). `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same commands) exit 0, PASS lines:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::an_undeliverable_admitted_input_item_kills_the_call
```

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::an_oversized_item_never_waits_for_permits_it_cannot_get
```

### `retained_sink_clone_cannot_extend_drain` (composition check) — witness `adapter::net::behavior::org_stream_lifecycle::tests::retained_sink_clone_cannot_extend_drain`

Mutation (production site `run_supervisor` handler arm, line 797) — inverse: `gate.finish()` is omitted on handler return, so the producer gate never closes and the retained clone keeps the queue open (the assignment's second listed inverse; the first — removing `sink_send`'s gate refusal — is non-discriminating in this witness's schedule: by the time the retained clone sends, the pump has exited and dropped the receiver, so the send is refused by the closed channel anyway and every assertion still passes):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -793,7 +793,6 @@ pub async fn run_supervisor(
                 // closes the sink so a retained clone cannot extend the
                 // drain, while grants stay creditable and expiry stays
                 // armed.
                 state.lock().handler_returned(result);
-                gate.finish();
             }
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::retained_sink_clone_cannot_extend_drain)'
```

Exit code: **100**. Verbatim failure output (assertion lines) — the red is the drain running to its 600 s deadline, cut by the `MODEL_BOUND` timeout, as required:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::retained_sink_clone_cannot_extend_drain' (151512) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1646:14:
    a live clone must not keep the drain alive: Elapsed(())
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::retained_sink_clone_cannot_extend_drain
```

**Appendix to `retained_sink_clone_cannot_extend_drain` — the first-listed inverse executed: FINDING (green under it).** The assignment's first alternative — "remove the producer-gate refusal in `sink_send`" — was executed as well, to move its diagnosis from source-established to executed (production site `sink_send`, line 678):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -675,9 +675,6 @@ pub async fn sink_send(
     len: usize,
 ) -> Result<(), SinkClosed> {
-    if gate.is_finished() {
-        return Err(SinkClosed);
-    }
     if len > budget {
```

Same command — exit **0 (GREEN under this inverse)**. Verbatim output:

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::retained_sink_clone_cannot_extend_drain
```

(build shows one benign `warning: unused variable: gate`; the mutation compiles.) **Diagnosis (now executed):** in this witness's schedule the gate check is redundant with the channel — by the time the retained clone sends, the pump has already drained and exited, dropping the receiver, so `tx.send` fails and the refusal comes from the closed channel plus `output_admission_failed()` declining once `output` is `Ended`; the drain-extension claim is decided earlier, by whether the gate CLOSES the producer half at handler return (`gate.finish()`), which is why the second alternative is the discriminating inverse and the first is not. The witness was not weakened or re-pinned. Restore: reverse anchored Edit; `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline.

### `client_stream_single_response_completes_without_pump` (composition check) — witness `adapter::net::behavior::org_stream_lifecycle::tests::client_stream_single_response_completes_without_pump`

Mutation (production site `CallLifecycle::pump_exited`, line 463) — inverse: `pump_exited` refuses to commit while `input != Input::Ended`:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -460,7 +460,7 @@ impl CallLifecycle {
     /// failure and never a success.
     pub fn pump_exited(&mut self) -> Option<TerminalReason> {
-        if self.terminal.is_some() {
+        if self.terminal.is_some() || self.input != Input::Ended {
             return None;
         }
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::client_stream_single_response_completes_without_pump)'
```

Exit code: **100**. Verbatim failure output (assertion lines):

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::client_stream_single_response_completes_without_pump' (156896) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1678:14:
    the single-response emitter supplies the drain-complete event
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::client_stream_single_response_completes_without_pump
```

### `protected_output_refusal_cannot_complete_ok` (composition check) — witness `adapter::net::behavior::org_stream_lifecycle::tests::protected_output_refusal_cannot_complete_ok`

Mutation (production site `sink_send`, lines 681-684) — inverse: the `len > budget` refusal skips `output_admission_failed()` (the metric-only drop):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -679,7 +679,6 @@ pub async fn sink_send(
     }
     if len > budget {
-        state.lock().output_admission_failed();
         return Err(SinkClosed);
     }
```

Command (from `net/crates/net/`):

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::protected_output_refusal_cannot_complete_ok)'
```

Exit code: **100**. Verbatim failure output (assertion lines) — leg (a) (the positive control) passed first inside the same run; the red is leg (b)'s `ResourceExhausted` assertion, as required:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::protected_output_refusal_cannot_complete_ok' (158512) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1810:9:
    assertion `left == right` failed: the refusal latches; `Completed(Ok)` after a dropped item is the defect: SupervisorOutcome { terminal: Completed(Ok), published: 0, emission: Some(Queued), discarded: 0 }
      left: Completed(Ok)
     right: ResourceExhausted
```

Restore: reverse anchored Edit. `sha256sum` after restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored run (same command) exit 0, PASS line:

```
        PASS [   0.012s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::protected_output_refusal_cannot_complete_ok
```

### `terminal_queue_refusal_is_not_peer_receipt` (composition check) — witness `adapter::net::behavior::org_stream_lifecycle::tests::terminal_queue_refusal_is_not_peer_receipt`; the `Refused` and `Unreachable` assertions are the required reds

The witness is one test with three legs ((a) positive control, (b) full queue → `Refused`, (c) gone session → `Unreachable`). A failing leg aborts the test, so the two named assertions cannot both be red in a single run: the prescribed inverse (record `Queued` regardless of the `try_send` result) reddens (b) first and (c) is never reached. Each named assertion therefore gets its own bounded production mutation — (b) under the prescribed inverse, (c) under a mutation that breaks only the `Closed` arm (leaving the `Full` arm's `Refused` correct so leg (b) passes and leg (c) is the first failing assertion).

**Mutation (b)** (production site `run_supervisor` disposition, lines 840-844) — inverse as prescribed: the supervisor records `TerminalDisposition::Queued` regardless of the `ctl.try_send` result:

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -837,11 +837,8 @@ pub async fn run_supervisor(
     // `Refused` and a gone session is `Unreachable` — both interruption,
     // both still release ownership, which is this function returning.
-    let disposition = match ctl.try_send(terminal.clone()) {
-        Ok(()) => TerminalDisposition::Queued,
-        Err(mpsc::error::TrySendError::Full(_)) => TerminalDisposition::Refused,
-        Err(mpsc::error::TrySendError::Closed(_)) => TerminalDisposition::Unreachable,
-    };
+    let _ = ctl.try_send(terminal.clone());
+    let disposition = TerminalDisposition::Queued;
```

Command (from `net/crates/net/`) — exit **100**; legs (a) passed first in the run; the red is the `Refused` assertion, as required:

```
CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invL cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_lifecycle::tests::terminal_queue_refusal_is_not_peer_receipt)'
```

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::terminal_queue_refusal_is_not_peer_receipt' (157548) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1878:9:
    assertion `left == right` failed: a full control queue is interruption, not receipt: SupervisorOutcome { terminal: Completed(Ok), published: 0, emission: Some(Queued), discarded: 0 }
      left: Some(Queued)
     right: Some(Refused)
```

**Mutation (c)** (production site `run_supervisor` disposition, `Closed` arm, line 843) — inverse: a gone session is recorded as `Queued` (the `Full` arm still yields `Refused`, so legs (a)-(b) pass and leg (c) is the first failing assertion):

```diff
--- a/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
+++ b/net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs
@@ -840,7 +840,7 @@ pub async fn run_supervisor(
     let disposition = match ctl.try_send(terminal.clone()) {
         Ok(()) => TerminalDisposition::Queued,
         Err(mpsc::error::TrySendError::Full(_)) => TerminalDisposition::Refused,
-        Err(mpsc::error::TrySendError::Closed(_)) => TerminalDisposition::Unreachable,
+        Err(mpsc::error::TrySendError::Closed(_)) => TerminalDisposition::Queued,
     };
```

Same command — exit **100**; legs (a)-(b) passed first in the run; the red is the `Unreachable` assertion, as required:

```
    thread 'adapter::net::behavior::org_stream_lifecycle::tests::terminal_queue_refusal_is_not_peer_receipt' (158972) panicked at src\adapter\net\behavior\org_stream_lifecycle.rs:1915:9:
    assertion `left == right` failed: a gone session is interruption, not synthetic success: SupervisorOutcome { terminal: Completed(Ok), published: 0, emission: Some(Queued), discarded: 0 }
      left: Some(Queued)
     right: Some(Unreachable)
```

Restore after each mutation: reverse anchored Edit. `sha256sum` after each restore `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` == baseline. Restored runs (same command, once after each restore) exit 0, PASS line (identical both times):

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_lifecycle::tests::terminal_queue_refusal_is_not_peer_receipt
```

---

## Summary and findings

Every named witness of the assignment is executed above with a red under a production-site inverse except where explicitly reported as a finding. Mapping:

| Item | Witness(es) | Receipt | Red |
|---|---|---|---|
| (a) | `server_streaming_starts_with_its_input_half_already_ended` | (a) | yes |
| (b) | `end_is_idempotent_and_never_touches_the_output_half` | (b) | yes |
| (c) | `a_drain_blocked_on_zero_credit_is_not_terminal_and_a_later_grant_completes_it`, `runtime_free::a_blocked_drain_is_not_terminal_and_a_grant_completes_it_without_any_runtime` | (c) | yes (both) |
| (d) | `the_deadline_fires_while_draining_and_discards_the_remainder` | (d)-1a, (d)-3b | yes |
| (d) | `runtime_free::the_deadline_preempts_a_blocked_drain_and_discards_the_remainder` | (d)-1b | yes |
| (d) | `cancel_is_admissible_while_draining_and_preempts_completion` | (d)-2 | yes |
| (d) | `cancel_during_a_drain_preempts_the_completion_end_to_end` | (d)-2, (d)-3b | yes |
| (d)/(i) | `revocation_while_parked_on_credit_retires_within_the_bound` | (d)-3b (finding under (d)-3a's literal inverse) | yes |
| (d)/(i) | `runtime_free::retirement_preempts_a_blocked_drain` | (i) (finding under (d)-3a's literal inverse) | yes |
| (e) | `a_handler_error_survives_the_drain_as_the_terminal`, `runtime_free::a_handler_error_is_the_terminal_here_too` | (e) | yes (both) |
| (f) | `handler_return_closes_an_open_input_half_and_preserves_the_result` | (f) | yes |
| (g) | `retire_is_first_writer_wins_from_every_state` | (g) | yes |
| (h) | `every_frame_after_the_terminal_is_dropped` | (h)-1 | yes |
| (h) | `grants_stay_admissible_while_draining_and_stop_once_output_ended` | (h)-2 | yes |
| (j) | `the_terminal_is_emitted_exactly_once` | (j) | yes |
| (k) | `only_a_completion_drains_queued_output` | (k) | yes |
| (l) | `two_calls_are_independent` | (l) | **plumbing** — no discriminating model-level inverse exists |
| (m) | `an_undeliverable_admitted_input_item_kills_the_call`, `an_oversized_item_never_waits_for_permits_it_cannot_get` | (m) | yes (both) |
| comp | `retained_sink_clone_cannot_extend_drain` | comp receipt + appendix | yes (under the `gate.finish()` inverse); **finding** under the `sink_send`-refusal inverse |
| comp | `client_stream_single_response_completes_without_pump` | comp receipt | yes |
| comp | `protected_output_refusal_cannot_complete_ok` | comp receipt | yes (leg (b)) |
| comp | `terminal_queue_refusal_is_not_peer_receipt` | comp receipt | yes (`Refused` and `Unreachable`, one mutation each) |

Findings (nothing re-pinned, nothing weakened):

1. **(d)-3a / Finding 1** — the prescribed "remove the semaphore closes" inverse cannot redden `revocation_while_parked_on_credit_retires_within_the_bound`: `run_supervisor`'s retire path bounds the pump through two independent mechanisms (`Semaphore::close` AND `JoinHandle::abort`+join); removing one leaves the property intact. The prescribed "MODEL_BOUND timeout" red requires the (d)-3b mutation (no wake, no cancel, join retained).
2. **(d)-3a / Finding 2** — `runtime_free::retirement_preempts_a_blocked_drain` is unreachable from any supervisor mutation (it drives `pull::PullCall`, which has no semaphore/task); its credited red is under the state-machine inverse in receipt (i).
3. **(l)** — `two_calls_are_independent` is plumbing (independence is by construction; no honest production inverse exists at model level). No inverse credit claimed.
4. **comp appendix** — `retained_sink_clone_cannot_extend_drain` is green under the `sink_send` producer-gate-refusal removal (executed); the discriminating inverse is the omission of `gate.finish()` on handler return.

Legs never executed here (explicitly):

- Inside a mutated run, assertions **after** the first failing assertion of that test never execute (standard single-panic semantics) — e.g. under (g)'s mutation the first `assert!(!call.retire(Cancelled))` reds and the later asserts of that test are not reached; each named assertion that needed its own red was given its own mutation (see (h), comp4).
- No other named witness or leg from the assignment is unexecuted; every one has at least one mutated red (or the finding/plumbing classification above).

Final state: `net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs` ends byte-identical to its starting sha256 `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` (verified after every one of the 23 mutation/restore cycles and once more at campaign end — last check and a final green run of `retained_sink_clone_cannot_extend_drain`, `PASS [   0.009s]`). Only the two owned files were touched: the model (transiently, always restored) and this receipt document.

### Post-handoff note — 2026-09-22 (~03:05 +0200)

The "Final state" paragraph above is scoped to **campaign end** (the moment of this lane's handoff/ALL CLEAR): at that point the model file was byte-identical to the starting sha256 `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc`, as verified 24 times (23 post-restore checks plus the campaign-end check). After handoff, two external writes to `org_stream_lifecycle.rs` occurred, both attributed to the coordinator (Main) and accounted for:

1. A rustfmt-mandated hunk: the single-line `assert_eq!(out.terminal, TerminalReason::Completed(HandlerResult::Ok), "{out:?}");` in `terminal_queue_refusal_is_not_peer_receipt` leg (c) reformatted into the 5-line block rustfmt requires (+4 lines: 2041 → 2045; Edit-tool write normalized CRLF → LF).
2. A coordinator spot-check mutation/restore on `sink_send` (independent re-verification of this document's comp-3 receipt: `output_admission_failed()` dropped at the `len > budget` refusal, `protected_output_refusal_cannot_complete_ok` re-run red at the same `ResourceExhausted` assertion, then restored green).

Baseline `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc`; current `b2dce203436e62c8f2c6bf2431dbbae56dbbb6102e4368920ae8d25148ae7ef3` (post-fmt form, stable). Receipt validity is unaffected: every receipted mutation was hash-proven against `7e7fbfcc…` before, during, and after its cycle, and all assertion quotes are verbatim from their runs. Coordinator direction on record: do not revert, do not re-verify.



