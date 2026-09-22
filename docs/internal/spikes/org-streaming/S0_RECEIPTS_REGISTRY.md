# Stage 0 raw inverse receipts — registry lane (slice 0.4)

File under test: `net/crates/net/src/adapter/net/behavior/org_stream_registry.rs`
(the §3 admission/retirement transaction model). One receipt per plan item,
grouped only where a single mutation reddens several witnesses (every red name
listed). All runs executed from `net/crates/net/` on this host with:

- `CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR`
- `cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex"`

Each receipt's command is given verbatim; the restored run executed the
identical line with the leading `sha256sum` tag pointing at
`src/adapter/net/behavior/org_stream_registry.rs` instead of
`org_stream_lifecycle.rs` (red runs tag the lifecycle file as a phantom-red
guard — see finding F1). Exit codes: `100` = nextest test failure, `0` = pass.

**Starting sha256 (recorded before the first mutation):**
`348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`
— measured again after **every** restore; all 24 measurements equal it (also
the file's final measurement; byte-identical to start).

**Baseline sweep (all 30 witnesses, before any mutation):**
`Summary [   0.118s] 30 tests run: 30 passed, 5801 skipped` / `EXIT_CODE=0`.
All 30 witness names were first enumerated with `cargo nextest list` and match
the assignment verbatim (37 tests exist in the module; 7 non-witness controls
were never run).

**Red-kind census.** Every red below is a runtime failure of the named witness
carrying the witness's own message — 20 assertion failures
(`assert_eq!`/`assert!`, messages and left/right pairs included) and 10
`expect`/`expect_err` panics (the witness's expectation string, with the
surprising value payload). Zero compile errors; every mutated state built
cleanly. No witness was green under its inverse (zero weakening findings).

## Findings (above deliverables)

- **F1 — external write to the sibling model file, not from this lane.** The
  lifecycle lane certified `org_stream_lifecycle.rs` restored to
  `7e7fbfccb1488e72d46b3b483f29145a8654c51a5a000cfc829021955c3b9cfc` at its
  ALL CLEAR, but the file measured
  `b2dce203436e62c8f2c6bf2431dbbae56dbbb6102e4368920ae8d25148ae7ef3`
  within seconds of that message (and has stayed at `b2dce203…` across every
  one of this lane's runs). Per the lifecycle lane's forensics the file
  received an external write after its final verification (line-count and
  newline-form change, rustfmt-clean), not written by either lane. All reds in
  this document are attributed against the `b2dce203…` state, in which the
  baseline sweep was 30/30 green; every red run's output below was captured
  under a run that printed `b2dce203…` in its leading tag.
- **F2 — restore drift on receipt 3, detected and repaired before proof.** The
  inverse for item 3 was a pure block deletion; the reverse edit re-inserted
  the block but left two extra blank lines at the deletion seam. The restore
  proof caught it (`2b50ec68f420412f8ef9d3cc2360a3c7082139f1c898cb1b386c612aef759834`
  ≠ starting), the drift was localized with a read-only `git diff` (two `+`
  blank lines at `@@ -1565,2 +1565,4 @@`, the only hunk not part of the
  pre-existing uncommitted fix), removed with one anchored edit, and the state
  re-proven at `348fc6f4…` with a fresh green run. Later pure deletions
  (items 12/13/18/21/22) were framed with non-empty boundary lines and all
  restored byte-exactly on the first attempt.
- **F3 — mutant fallout abort on receipt 16 (after the valid red).** With the
  three byte-counter increments dropped, the witness's own assertion failed
  first (`left: 0 / right: 1500` at `:3365`), then, during unwinding, the
  dropped `ItemPermit`s hit `release_charge`'s double-release guard
  (`byte permit released twice, or against the wrong call`, `:732`) as a
  second panic, and Windows fail-fast aborted the test process
  (`0xc0000409`, nextest `ABORT`). The recorded red is the witness's
  assertion; the abort is the mutant's broken accounting surfacing in `Drop`.
- **F4 — no witness survived its inverse.** All 30 witnesses went red under
  their assigned mutation; nothing was re-pinned or weakened.

---

### 1. `retire_between_confirm_check_and_owner_transfer`
witness: `adapter::net::behavior::org_stream_registry::tests::retire_between_confirm_check_and_owner_transfer`

Inverse — undo the transaction fix: `begin_confirm` releases the lock before
returning and `ConfirmTxn::transfer` re-acquires it (the pre-fix shape).

```diff
@@ begin_confirm (tail) — §3 step 5 check half @@
         // sound — no removal can run while this transaction is open.
-        Ok(ConfirmTxn {
-            inner,
-            lease: *lease,
-        })
+        drop(inner);
+        Ok(ConfirmTxn {
+            registry: self,
+            lease: *lease,
+        })
@@ pub struct ConfirmTxn<'a> @@
 pub struct ConfirmTxn<'a> {
-    inner: MutexGuard<'a, RegistryInner>,
+    registry: &'a ProtectedCallRegistry,
     lease: AdmissionLease,
 }
@@ ConfirmTxn::transfer @@
-    pub fn transfer(mut self, owner: Arc<SupervisorOwner>) -> Result<RunningCall, Denial> {
+    pub fn transfer(self, owner: Arc<SupervisorOwner>) -> Result<RunningCall, Denial> {
         if owner.key != self.lease.key || owner.incarnation != self.lease.incarnation {
             return Err(Denial::NotAdmitted);
         }
         // Consumes the guard `begin_confirm` opened with. Re-locking here
         // would reopen the exact window this transaction exists to close.
-        let record = self
-            .inner
+        let mut inner = self.registry.inner.lock();
+        let record = inner
             .records
             .get_mut(&self.lease.key)
             .expect("presence validated when this transaction opened");
```

Command (red; the restored run is this line with `…org_stream_lifecycle.rs` →
`…org_stream_registry.rs` in the leading `sha256sum`):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::retire_between_confirm_check_and_owner_transfer)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::retire_between_confirm_check_and_owner_transfer' (159960) panicked at src\adapter\net\behavior\org_stream_registry.rs:2593:9:
assertion `left == right` failed: retirement is serialized with the check→transfer window
  left: Applied(true)
 right: Blocked
```

Restore proof: sha256 before `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == after `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::retire_between_confirm_check_and_owner_transfer
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 2. `publication_before_notification_cannot_authorize_commit`
witness: `adapter::net::behavior::org_stream_registry::tests::publication_before_notification_cannot_authorize_commit`

Inverse — `commit_check_locked` proceeds on a stale captured view (skips the
`is_current`/requalify path: the captured view is replaced by the live one).

```diff
@@ fn commit_check_locked @@
-        let captured = record.captured;
+        let captured = live;
         if captured.is_current(&live) {
             return CommitVerdict::Proceed;
         }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::publication_before_notification_cannot_authorize_commit)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::publication_before_notification_cannot_authorize_commit' (152968) panicked at src\adapter\net\behavior\org_stream_registry.rs:2393:9:
assertion `left == right` failed: a commit inside the pre-notification window is refused
  left: Proceed
 right: Retired(Revoked)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::publication_before_notification_cannot_authorize_commit
     Summary [   0.030s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 3. `raise_between_reserve_and_install_denies_with_zero_effects`
witness: `adapter::net::behavior::org_stream_registry::tests::raise_between_reserve_and_install_denies_with_zero_effects`

Inverse — `install` skips the `authority_epoch`/requalify gate (the whole
movement block is deleted).

```diff
@@ fn install — movement gate @@
-        if epoch_now != reservation.epoch_at_reserve || !captured.is_current(&live) {
-            match captured.movement(&live) {
-                StampMovement::Unusable => {
-                    Self::retire_locked(
-                        &mut inner,
-                        reservation.key,
-                        reservation.incarnation,
-                        TerminalReason::AuthorityUnavailable,
-                    );
-                    return Err(Denial::AuthorityUnavailable);
-                }
-                StampMovement::Unchanged | StampMovement::GenerationOnly => {
-                    if pin.floor_for(facts.acting_org, facts.member) > facts.member_generation {
-                        Self::retire_locked(
-                            &mut inner,
-                            reservation.key,
-                            reservation.incarnation,
-                            TerminalReason::Revoked,
-                        );
-                        return Err(Denial::Revoked);
-                    }
-                }
-            }
-        }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::raise_between_reserve_and_install_denies_with_zero_effects)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim — the witness's `expect_err`
assertion; the mutant's install admitted against a dead view):

```
thread 'adapter::net::behavior::org_stream_registry::tests::raise_between_reserve_and_install_denies_with_zero_effects' (157712) panicked at src\adapter\net\behavior\org_stream_registry.rs:2328:14:
install must deny: AdmissionLease { key: CallKey { caller: 1, call_id: 10 }, incarnation: 1 }
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`
(the first reverse-edit attempt measured `2b50ec68…` — see finding F2; two
stray blank lines removed by anchored edit, then re-measured equal and
re-proven green). Restored run (verbatim, from the byte-identical state):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::raise_between_reserve_and_install_denies_with_zero_effects
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 4. `requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call`
witness: `adapter::net::behavior::org_stream_registry::tests::requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call`

Inverse — generation movement retires on sight (whole-stamp behaviour) instead
of requalifying.

```diff
@@ fn commit_check_locked — movement match @@
             StampMovement::Unchanged => CommitVerdict::Proceed,
             StampMovement::GenerationOnly => {
-                if pin.floor_for(facts.acting_org, facts.member) > facts.member_generation {
-                    Self::retire_locked(inner, key, incarnation, TerminalReason::Revoked);
-                    CommitVerdict::Retired(TerminalReason::Revoked)
-                } else {
-                    let record = inner
-                        .records
-                        .get_mut(&key)
-                        .expect("record presence checked under this same lock");
-                    record.captured = live;
-                    CommitVerdict::Requalified
-                }
+                Self::retire_locked(inner, key, incarnation, TerminalReason::AuthorityUnavailable);
+                CommitVerdict::Retired(TerminalReason::AuthorityUnavailable)
             }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call' (147476) panicked at src\adapter\net\behavior\org_stream_registry.rs:2442:9:
assertion `left == right` failed
  left: Retired(AuthorityUnavailable)
 right: Requalified
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::requalify_keeps_the_unaffected_sibling_and_retires_the_affected_call
     Summary [   0.033s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 5. `store_replacement_retires_records_captured_under_the_old_identity`
witness: `adapter::net::behavior::org_stream_registry::tests::store_replacement_retires_records_captured_under_the_old_identity`

Inverse — store replacement does not retire records captured under the old
`(authority_ptr, store_ptr)` (the identity comparison is inverted).

```diff
@@ ProtectedCallRegistry::on_store_replaced — victim filter @@
                     record.phase != Phase::Terminal
-                        && (record.captured.authority_id != new_stamp.authority_id
-                            || record.captured.store_id != new_stamp.store_id)
+                        && (record.captured.authority_id == new_stamp.authority_id
+                            || record.captured.store_id == new_stamp.store_id)
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::store_replacement_retires_records_captured_under_the_old_identity)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::store_replacement_retires_records_captured_under_the_old_identity' (159952) panicked at src\adapter\net\behavior\org_stream_registry.rs:2482:9:
assertion `left == right` failed
  left: None
 right: Some(AuthorityUnavailable)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::store_replacement_retires_records_captured_under_the_old_identity
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 6. `poisoned_store_retires_every_protected_call`
witness: `adapter::net::behavior::org_stream_registry::tests::poisoned_store_retires_every_protected_call`

Inverse — the empty-slice (poison) notify does not retire all (it retires
none).

```diff
@@ ProtectedCallRegistry::on_floors_raised — empty-slice wake @@
                 if raised.is_empty() {
-                    return Some((
-                        *key,
-                        record.incarnation,
-                        TerminalReason::AuthorityUnavailable,
-                    ));
+                    return None;
                 }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::poisoned_store_retires_every_protected_call)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::poisoned_store_retires_every_protected_call' (132564) panicked at src\adapter\net\behavior\org_stream_registry.rs:2522:9:
assertion `left == right` failed
  left: None
 right: Some(AuthorityUnavailable)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::poisoned_store_retires_every_protected_call
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 7. `retire_before_transfer_prevents_every_admitted_effect` + `fold_prerequisite_refusal_before_transfer_releases_exactly_once`
witnesses: `adapter::net::behavior::org_stream_registry::tests::retire_before_transfer_prevents_every_admitted_effect`, `adapter::net::behavior::org_stream_registry::tests::fold_prerequisite_refusal_before_transfer_releases_exactly_once`

One mutation, two hunks — `begin_confirm` accepts a `Terminal` record **and**
`ConfirmTxn::abandon` also removes the record (double ownership). First red is
attributed to hunk A, second to hunk B.

```diff
@@ begin_confirm — phase match @@
         match record.phase {
-            Phase::Admitted => {}
-            Phase::Terminal => return Err(Denial::AuthorityChanged),
+            Phase::Admitted | Phase::Terminal => {}
             Phase::Opening | Phase::Running => return Err(Denial::NotAdmitted),
         }
@@ ConfirmTxn::abandon @@
-    pub fn abandon(self) {}
+    pub fn abandon(self) {
+        let ConfirmTxn { mut inner, lease } = self;
+        let _ = inner.records.remove(&lease.key);
+    }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::retire_before_transfer_prevents_every_admitted_effect) or test(=adapter::net::behavior::org_stream_registry::tests::fold_prerequisite_refusal_before_transfer_releases_exactly_once)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim, both reds):

```
thread 'adapter::net::behavior::org_stream_registry::tests::retire_before_transfer_prevents_every_admitted_effect' (129720) panicked at src\adapter\net\behavior\org_stream_registry.rs:2632:18:
confirm must refuse a retired record
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::fold_prerequisite_refusal_before_transfer_releases_exactly_once' (42368) panicked at src\adapter\net\behavior\org_stream_registry.rs:2681:9:
assertion `left == right` failed
  left: None
 right: Some(Admitted)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/2) net-mesh adapter::net::behavior::org_stream_registry::tests::fold_prerequisite_refusal_before_transfer_releases_exactly_once
        PASS [   0.011s] (2/2) net-mesh adapter::net::behavior::org_stream_registry::tests::retire_before_transfer_prevents_every_admitted_effect
     Summary [   0.031s] 2 tests run: 2 passed, 5829 skipped
EXIT_CODE=0
```

### 8. `retire_after_transfer_reaches_the_owner_before_its_task_runs`
witness: `adapter::net::behavior::org_stream_registry::tests::retire_after_transfer_reaches_the_owner_before_its_task_runs`

Inverse — retirement does not signal an armed `SupervisorOwner` (the signal is
removed from `retire_locked`, shared by `retire`/`retire_nonblocking`).

```diff
@@ fn retire_locked @@
-        record.terminal = Some(reason.clone());
-        if let Some(owner) = record.owner.as_ref() {
-            owner.signal(reason);
-        }
+        record.terminal = Some(reason);
         true
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::retire_after_transfer_reaches_the_owner_before_its_task_runs)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::retire_after_transfer_reaches_the_owner_before_its_task_runs' (158340) panicked at src\adapter\net\behavior\org_stream_registry.rs:2653:9:
assertion `left == right` failed: retirement reaches the registered owner with no task polled
  left: None
 right: Some(Cancelled)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::retire_after_transfer_reaches_the_owner_before_its_task_runs
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 9. `scheduling_failure_after_transfer_does_not_orphan_a_running_record`
witness: `adapter::net::behavior::org_stream_registry::tests::scheduling_failure_after_transfer_does_not_orphan_a_running_record`

Inverse — the post-transfer failure path skips the owner's completion
(`complete` is routed to the bridge expectation, so a transferred record is
never completed and is orphaned).

```diff
@@ ProtectedCallRegistry::complete @@
     pub fn complete(&self, key: CallKey, incarnation: u64) -> bool {
-        self.remove(key, incarnation, CleanupOwner::Supervisor)
+        self.remove(key, incarnation, CleanupOwner::Bridge)
     }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::scheduling_failure_after_transfer_does_not_orphan_a_running_record)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::scheduling_failure_after_transfer_does_not_orphan_a_running_record' (156728) panicked at src\adapter\net\behavior\org_stream_registry.rs:2708:9:
assertion failed: h.registry.complete(k, lease.incarnation)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.010s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::scheduling_failure_after_transfer_does_not_orphan_a_running_record
     Summary [   0.030s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 10. `pretransfer_retirement_has_one_cleanup_owner`
witness: `adapter::net::behavior::org_stream_registry::tests::pretransfer_retirement_has_one_cleanup_owner`

Inverse — ownerless reservation: the lost-bridge reaper retires but neither
party removes (`reap_expired_openings` skips the bridge `release`).

```diff
@@ ProtectedCallRegistry::reap_expired_openings @@
         for (key, incarnation) in expired {
             self.retire(key, incarnation, TerminalReason::Timeout);
-            if self.release(key, incarnation) {
-                reaped += 1;
-            }
+            reaped += 1;
         }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::pretransfer_retirement_has_one_cleanup_owner)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::pretransfer_retirement_has_one_cleanup_owner' (159608) panicked at src\adapter\net\behavior\org_stream_registry.rs:2736:9:
assertion failed: !h.registry.release(lost, lost_reservation.incarnation)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::pretransfer_retirement_has_one_cleanup_owner
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 11. `complete_removes_exactly_once_and_release_cannot_double_remove` + `late_operations_with_a_stale_incarnation_cannot_touch_the_successor`
witnesses: `adapter::net::behavior::org_stream_registry::tests::complete_removes_exactly_once_and_release_cannot_double_remove`, `adapter::net::behavior::org_stream_registry::tests::late_operations_with_a_stale_incarnation_cannot_touch_the_successor`

Inverse — `remove` unconditional on `CleanupOwner` **and** on incarnation (a
missing record even reports a removal and bumps the ledger).

```diff
@@ fn remove @@
     fn remove(&self, key: CallKey, incarnation: u64, expected: CleanupOwner) -> bool {
         let mut inner = self.inner.lock();
-        let matches = inner
-            .records
-            .get(&key)
-            .is_some_and(|r| r.incarnation == incarnation && r.cleanup == expected);
-        if !matches {
-            return false;
-        }
-        let record = inner
-            .records
-            .remove(&key)
-            .expect("presence checked under this same lock");
+        let _ = expected;
+        let Some(record) = inner.records.remove(&key) else {
+            *self.removals.lock().entry((key, incarnation)).or_insert(0) += 1;
+            return true;
+        };
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::complete_removes_exactly_once_and_release_cannot_double_remove) or test(=adapter::net::behavior::org_stream_registry::tests::late_operations_with_a_stale_incarnation_cannot_touch_the_successor)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim, both reds):

```
thread 'adapter::net::behavior::org_stream_registry::tests::late_operations_with_a_stale_incarnation_cannot_touch_the_successor' (156208) panicked at src\adapter\net\behavior\org_stream_registry.rs:2786:9:
assertion failed: !h.registry.release(k, first.incarnation)
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::complete_removes_exactly_once_and_release_cannot_double_remove' (157656) panicked at src\adapter\net\behavior\org_stream_registry.rs:2761:9:
assertion failed: !h.registry.complete(k, lease.incarnation)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/2) net-mesh adapter::net::behavior::org_stream_registry::tests::late_operations_with_a_stale_incarnation_cannot_touch_the_successor
        PASS [   0.013s] (2/2) net-mesh adapter::net::behavior::org_stream_registry::tests::complete_removes_exactly_once_and_release_cannot_double_remove
     Summary [   0.033s] 2 tests run: 2 passed, 5829 skipped
EXIT_CODE=0
```

### 12. `duplicate_opening_while_live_is_active_call_owned_before_decode`
witness: `adapter::net::behavior::org_stream_registry::tests::duplicate_opening_while_live_is_active_call_owned_before_decode`

Inverse — `reserve` succeeds over a live key (the pre-decode key check is
deleted).

```diff
@@ ProtectedCallRegistry::reserve @@
-        // The key check comes first: it is the one refusal that must land
-        // before decode, and it costs a hash lookup.
-        if inner.records.contains_key(&req.key) {
-            return Err(Denial::ActiveCallOwned);
-        }
         if inner.active_node >= self.limits.max_active_node {
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::duplicate_opening_while_live_is_active_call_owned_before_decode)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim — the witness's `expect_err`;
the duplicate reservation succeeded):

```
thread 'adapter::net::behavior::org_stream_registry::tests::duplicate_opening_while_live_is_active_call_owned_before_decode' (155232) panicked at src\adapter\net\behavior\org_stream_registry.rs:2841:53:
duplicate: Reservation { key: CallKey { caller: 1, call_id: 10 }, incarnation: 2, epoch_at_reserve: 0, verify_by_ns: 1030000000000 }
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::duplicate_opening_while_live_is_active_call_owned_before_decode
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 13. `duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal`
witness: `adapter::net::behavior::org_stream_registry::tests::duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal`

Inverse — the abstract guard silently returns `Admitted` for a retained key
(the existing-entry refusal is deleted; Q4).

```diff
@@ ReplayGuardModel::admit @@
         entries.retain(|_, entry| entry.expires_at_ns > now_ns);
-        if let Some(existing) = entries.get(&key) {
-            return if existing.digest == digest {
-                GuardVerdict::Replay
-            } else {
-                GuardVerdict::CallIdCollision
-            };
-        }
         entries.insert(
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal' (101712) panicked at src\adapter\net\behavior\org_stream_registry.rs:2897:9:
assertion `left == right` failed: an abstract guard returning Admitted here is the failure the plan names
  left: Admitted
 right: Replay
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::duplicate_after_completion_inside_the_retained_window_is_a_guard_refusal
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 14. `policy_veto_releases_the_reservation_but_retains_the_guard_record`
witness: `adapter::net::behavior::org_stream_registry::tests::policy_veto_releases_the_reservation_but_retains_the_guard_record`

Inverse — the veto path also releases the guard slot (bridge `release` drops
the guard entry).

```diff
@@ ProtectedCallRegistry::release @@
     pub fn release(&self, key: CallKey, incarnation: u64) -> bool {
+        self.guard.entries.lock().remove(&key);
         self.remove(key, incarnation, CleanupOwner::Bridge)
     }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::policy_veto_releases_the_reservation_but_retains_the_guard_record)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::policy_veto_releases_the_reservation_but_retains_the_guard_record' (156132) panicked at src\adapter\net\behavior\org_stream_registry.rs:2949:9:
the guard slot the veto consumed stays consumed
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::policy_veto_releases_the_reservation_but_retains_the_guard_record
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 15. `active_call_quotas_refuse_the_n_plus_first_at_every_scope` + `q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one` + `q1_default_per_org_ceiling_admits_512_and_refuses_the_513th`
witnesses: `adapter::net::behavior::org_stream_registry::tests::active_call_quotas_refuse_the_n_plus_first_at_every_scope`, `adapter::net::behavior::org_stream_registry::tests::q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one`, `adapter::net::behavior::org_stream_registry::tests::q1_default_per_org_ceiling_admits_512_and_refuses_the_513th`

One mutation — off-by-one (`>=` → `>`) at all three active-call scopes (node,
caller, org): the N+1th is admitted.

```diff
@@ ProtectedCallRegistry::reserve — node scope @@
-        if inner.active_node >= self.limits.max_active_node {
+        if inner.active_node > self.limits.max_active_node {
@@ ProtectedCallRegistry::reserve — caller scope @@
-        if caller_active >= self.limits.max_active_per_caller {
+        if caller_active > self.limits.max_active_per_caller {
@@ ProtectedCallRegistry::install — org scope @@
-        if org_active >= self.limits.max_active_per_org {
+        if org_active > self.limits.max_active_per_org {
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::active_call_quotas_refuse_the_n_plus_first_at_every_scope) or test(=adapter::net::behavior::org_stream_registry::tests::q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one) or test(=adapter::net::behavior::org_stream_registry::tests::q1_default_per_org_ceiling_admits_512_and_refuses_the_513th)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim, all three reds — each witness's
`expect_err` showing the admitted N+1th record):

```
thread 'adapter::net::behavior::org_stream_registry::tests::active_call_quotas_refuse_the_n_plus_first_at_every_scope' (144152) panicked at src\adapter\net\behavior\org_stream_registry.rs:2975:52:
3rd: Reservation { key: CallKey { caller: 1, call_id: 3 }, incarnation: 3, epoch_at_reserve: 0, verify_by_ns: 1030000000000 }
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::q1_default_per_org_ceiling_admits_512_and_refuses_the_513th' (156784) panicked at src\adapter\net\behavior\org_stream_registry.rs:3095:18:
513th for this org: AdmissionLease { key: CallKey { caller: 9, call_id: 0 }, incarnation: 513 }
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one' (155868) panicked at src\adapter\net\behavior\org_stream_registry.rs:3054:18:
65th for caller 0: Reservation { key: CallKey { caller: 0, call_id: 64 }, incarnation: 65, epoch_at_reserve: 0, verify_by_ns: 1030000000000 }
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.011s] (1/3) net-mesh adapter::net::behavior::org_stream_registry::tests::active_call_quotas_refuse_the_n_plus_first_at_every_scope
        PASS [   0.013s] (2/3) net-mesh adapter::net::behavior::org_stream_registry::tests::q1_default_active_call_ceilings_admit_n_and_refuse_n_plus_one
        PASS [   0.016s] (3/3) net-mesh adapter::net::behavior::org_stream_registry::tests::q1_default_per_org_ceiling_admits_512_and_refuses_the_513th
     Summary [   0.036s] 3 tests run: 3 passed, 5828 skipped
EXIT_CODE=0
```

### 16. `item_credit_does_not_imply_byte_reservation`
witness: `adapter::net::behavior::org_stream_registry::tests::item_credit_does_not_imply_byte_reservation`

Inverse — the item grant admits payload bytes without reserving them (all
three counter increments in `ByteBudgets::reserve` are dropped; the checks
still run, nothing is charged).

```diff
@@ ByteBudgets::reserve — step 1 charge @@
-        state.per_call.insert(call_slot, call_next);
+        let _ = call_next;
@@ ByteBudgets::reserve — step 2 charge @@
-        state.per_caller.insert(key.caller, caller_next);
+        let _ = caller_next;
@@ ByteBudgets::reserve — step 3 charge @@
-        state.node = node_next;
+        let _ = node_next;
         drop(state);
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::item_credit_does_not_imply_byte_reservation)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim — the witness's assertion; see
finding F3 for the unwind-time abort that followed it):

```
thread 'adapter::net::behavior::org_stream_registry::tests::item_credit_does_not_imply_byte_reservation' (155676) panicked at src\adapter\net\behavior\org_stream_registry.rs:3365:9:
assertion `left == right` failed
  left: 0
 right: 1500
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::item_credit_does_not_imply_byte_reservation
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 17. `cancel_dequeue_handoff_consumes_one_permit` + `removal_consumes_the_permits_its_call_still_owned`
witnesses: `adapter::net::behavior::org_stream_registry::tests::cancel_dequeue_handoff_consumes_one_permit`, `adapter::net::behavior::org_stream_registry::tests::removal_consumes_the_permits_its_call_still_owned`

One mutation, two hunks — the permit is released twice (dequeue and discard
both release: `SharedPermit::take` settles the charge before handing it out)
**and** record removal skips its outstanding permits. First red is attributed
to hunk A, second to hunk B.

```diff
@@ SharedPermit::take @@
     /// Consume the permit. Exactly one caller ever gets `Some`.
     pub fn take(&self) -> Option<ItemPermit> {
-        self.slot.lock().take()
+        self.slot.lock().take().map(|mut permit| {
+            permit.settle();
+            permit
+        })
     }
@@ fn remove — queued permit consumption @@
         // Queued items the removed call still owned: their permits are
         // consumed here, not guessed at.
-        for item in &record.queued {
-            if let Some(permit) = item.take() {
-                permit.release();
-            }
-        }
         drop(inner);
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::cancel_dequeue_handoff_consumes_one_permit) or test(=adapter::net::behavior::org_stream_registry::tests::removal_consumes_the_permits_its_call_still_owned)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim, both reds):

```
thread 'adapter::net::behavior::org_stream_registry::tests::removal_consumes_the_permits_its_call_still_owned' (160624) panicked at src\adapter\net\behavior\org_stream_registry.rs:3573:9:
assertion `left == right` failed
  left: 3
 right: 0
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::cancel_dequeue_handoff_consumes_one_permit' (158956) panicked at src\adapter\net\behavior\org_stream_registry.rs:3504:9:
assertion `left == right` failed: handoff is not memory reclamation: the bytes are still charged
  left: 700
 right: 800
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/2) net-mesh adapter::net::behavior::org_stream_registry::tests::removal_consumes_the_permits_its_call_still_owned
        PASS [   0.011s] (2/2) net-mesh adapter::net::behavior::org_stream_registry::tests::cancel_dequeue_handoff_consumes_one_permit
     Summary [   0.031s] 2 tests run: 2 passed, 5829 skipped
EXIT_CODE=0
```

### 18. `oversized_item_never_waits_for_impossible_permits`
witness: `adapter::net::behavior::org_stream_registry::tests::oversized_item_never_waits_for_impossible_permits`

Inverse — an item over the configured call limit gets a *waitable* refusal
instead of failing promptly (the `ExceedsCallBudget` check is deleted from
`validate_item`).

```diff
@@ ByteBudgets::validate_item @@
         if len > MAX_RPC_ITEM_BYTES {
             return Err(ByteRefusal::ItemTooLarge);
         }
-        if len > self.limits.per_call {
-            return Err(ByteRefusal::ExceedsCallBudget);
-        }
         Ok(())
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::oversized_item_never_waits_for_impossible_permits)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::oversized_item_never_waits_for_impossible_permits' (140360) panicked at src\adapter\net\behavior\org_stream_registry.rs:3313:9:
assertion `left == right` failed
  left: CallBudgetFull
 right: ExceedsCallBudget
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::oversized_item_never_waits_for_impossible_permits
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 19. `check_and_commit_are_one_ownership_operation`
witness: `adapter::net::behavior::org_stream_registry::tests::check_and_commit_are_one_ownership_operation`

Inverse — `begin_commit` releases the lock before returning its transaction
(`CommitTxn` re-acquires it in `commit`), re-opening the check→commit window.

```diff
@@ pub struct CommitTxn<'a> @@
 pub struct CommitTxn<'a> {
-    inner: MutexGuard<'a, RegistryInner>,
+    registry: &'a ProtectedCallRegistry,
     key: CallKey,
     incarnation: u64,
     verdict: CommitVerdict,
 }
@@ ProtectedCallRegistry::begin_commit @@
-        let mut inner = self.inner.lock();
-        let authority = Arc::clone(&inner.authority);
-        let verdict = {
-            let pin = authority.pin();
-            Self::commit_check_locked(&mut inner, &pin, key, incarnation)
-        };
+        let verdict = self.commit_check(key, incarnation);
         match verdict {
             CommitVerdict::Proceed | CommitVerdict::Requalified => Ok(CommitTxn {
-                inner,
+                registry: self,
                 key,
                 incarnation,
                 verdict,
             }),
@@ CommitTxn::commit @@
-    pub fn commit(mut self, permit: ItemPermit) -> Arc<SharedPermit> {
+    pub fn commit(self, permit: ItemPermit) -> Arc<SharedPermit> {
         let shared = SharedPermit::new(permit);
-        let record = self
-            .inner
+        let mut inner = self.registry.inner.lock();
+        let record = inner
             .records
             .get_mut(&self.key)
             .expect("presence validated when this transaction opened");
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::check_and_commit_are_one_ownership_operation)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::check_and_commit_are_one_ownership_operation' (161600) panicked at src\adapter\net\behavior\org_stream_registry.rs:2816:9:
assertion `left == right` failed: retirement cannot land between the verdict and the enqueue
  left: Applied(true)
 right: Blocked
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::check_and_commit_are_one_ownership_operation
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 20. `session_retirement_matches_the_exact_peer_establishment_not_a_bare_id`
witness: `adapter::net::behavior::org_stream_registry::tests::session_retirement_matches_the_exact_peer_establishment_not_a_bare_id`

Inverse — session retirement matches a bare truncated session id across peers.

```diff
@@ ProtectedCallRegistry::retire_session — victim filter @@
-            .filter(|(_, r)| r.phase != Phase::Terminal && r.session == *session)
+            .filter(|(_, r)| r.phase != Phase::Terminal && r.session.session_id == session.session_id)
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::session_retirement_matches_the_exact_peer_establishment_not_a_bare_id)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim — the unrelated peer's call was
retired too):

```
thread 'adapter::net::behavior::org_stream_registry::tests::session_retirement_matches_the_exact_peer_establishment_not_a_bare_id' (161084) panicked at src\adapter\net\behavior\org_stream_registry.rs:3136:9:
assertion `left == right` failed
  left: 2
 right: 1
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::session_retirement_matches_the_exact_peer_establishment_not_a_bare_id
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 21. `spent_session_currentness_refuses_admission`
witness: `adapter::net::behavior::org_stream_registry::tests::spent_session_currentness_refuses_admission`

Inverse — the `u64::MAX` exhaustion marker (`None` session generation) is
treated as a valid generation (the refusal is deleted).

```diff
@@ ProtectedCallRegistry::reserve @@
-        if req.session_generation.is_none() {
-            return Err(Denial::SessionCurrentnessExhausted);
-        }
         let mut inner = self.inner.lock();
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::spent_session_currentness_refuses_admission)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim — the witness's `expect_err`;
the spent session was admitted):

```
thread 'adapter::net::behavior::org_stream_registry::tests::spent_session_currentness_refuses_admission' (142528) panicked at src\adapter\net\behavior\org_stream_registry.rs:3194:46:
refused: Reservation { key: CallKey { caller: 1, call_id: 10 }, incarnation: 1, epoch_at_reserve: 0, verify_by_ns: 1030000000000 }
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::spent_session_currentness_refuses_admission
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 22. `expired_verification_deadline_denies_a_late_install` + `reserve_arithmetic_is_checked_against_an_absurd_clock`
witnesses: `adapter::net::behavior::org_stream_registry::tests::expired_verification_deadline_denies_a_late_install`, `adapter::net::behavior::org_stream_registry::tests::reserve_arithmetic_is_checked_against_an_absurd_clock`

One mutation, two hunks — wrapping arithmetic in `reserve` **and** no
verification-deadline check in `install`. First red is attributed to hunk B,
second to hunk A.

```diff
@@ ProtectedCallRegistry::reserve — verification deadline @@
-        let Some(verify_by_ns) = req.now_ns.checked_add(self.limits.verification_deadline_ns)
-        else {
-            return Err(Denial::CounterOverflow);
-        };
+        let verify_by_ns = req.now_ns.wrapping_add(self.limits.verification_deadline_ns);
@@ ProtectedCallRegistry::install — verification deadline gate @@
-        if now_ns > verify_by_ns {
-            Self::retire_locked(
-                &mut inner,
-                reservation.key,
-                reservation.incarnation,
-                TerminalReason::Timeout,
-            );
-            return Err(Denial::VerificationDeadlineExpired);
-        }
-
         if epoch_now != reservation.epoch_at_reserve || !captured.is_current(&live) {
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::expired_verification_deadline_denies_a_late_install) or test(=adapter::net::behavior::org_stream_registry::tests::reserve_arithmetic_is_checked_against_an_absurd_clock)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim, both reds — note the wrapped
deadline `29999999999` in the second):

```
thread 'adapter::net::behavior::org_stream_registry::tests::expired_verification_deadline_denies_a_late_install' (161456) panicked at src\adapter\net\behavior\org_stream_registry.rs:3198:14:
too late: AdmissionLease { key: CallKey { caller: 1, call_id: 10 }, incarnation: 1 }
```

```
thread 'adapter::net::behavior::org_stream_registry::tests::reserve_arithmetic_is_checked_against_an_absurd_clock' (146068) panicked at src\adapter\net\behavior\org_stream_registry.rs:3210:37:
overflow: Reservation { key: CallKey { caller: 1, call_id: 10 }, incarnation: 1, epoch_at_reserve: 0, verify_by_ns: 29999999999 }
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/2) net-mesh adapter::net::behavior::org_stream_registry::tests::expired_verification_deadline_denies_a_late_install
        PASS [   0.010s] (2/2) net-mesh adapter::net::behavior::org_stream_registry::tests::reserve_arithmetic_is_checked_against_an_absurd_clock
     Summary [   0.031s] 2 tests run: 2 passed, 5829 skipped
EXIT_CODE=0
```

### 23. `exhausted_generation_is_unusable_authority_in_either_position`
witness: `adapter::net::behavior::org_stream_registry::tests::exhausted_generation_is_unusable_authority_in_either_position`

Inverse — a frozen (exhausted) generation is accepted as a live view (the
generation-usability guards are dropped from `is_current` and `movement`, in
both positions).

```diff
@@ AuthorityStamp::is_current @@
     pub fn is_current(&self, current: &AuthorityStamp) -> bool {
-        self.generation.is_some()
-            && current.generation.is_some()
-            && self == current
-            && !current.poisoned
+        self == current && !current.poisoned
     }
@@ AuthorityStamp::movement @@
-        if self.generation.is_none()
-            || current.generation.is_none()
-            || self.poisoned
-            || current.poisoned
-        {
+        if self.poisoned || current.poisoned {
             return StampMovement::Unusable;
         }
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::exhausted_generation_is_unusable_authority_in_either_position)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::exhausted_generation_is_unusable_authority_in_either_position' (146484) panicked at src\adapter\net\behavior\org_stream_registry.rs:2551:9:
assertion `left == right` failed: a frozen generation cannot show a captured view still live
  left: Requalified
 right: Retired(AuthorityUnavailable)
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.009s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::exhausted_generation_is_unusable_authority_in_either_position
     Summary [   0.029s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

### 24. `serve_handle_drop_retires_only_its_own_registration`
witness: `adapter::net::behavior::org_stream_registry::tests::serve_handle_drop_retires_only_its_own_registration`

Inverse — handle drop retires records of unrelated registrations too (the
registration filter is dropped).

```diff
@@ ProtectedCallRegistry::retire_registration — victim filter @@
-            .filter(|(_, r)| r.phase != Phase::Terminal && r.registration == registration)
+            .filter(|(_, r)| r.phase != Phase::Terminal)
```

Command (red):

```
sha256sum src/adapter/net/behavior/org_stream_lifecycle.rs && CARGO_TARGET_DIR=C:/Users/chief/orca/workspaces/net/org-streaming/net/crates/net/target-invR cargo nextest run --lib --no-tests=fail --retries 0 --features "net,redex,redex-disk,cortex,netdb,meshdb,meshos,dataforts,nat-traversal,port-mapping,tool,batched-ingress,cli,regex" -E 'test(=adapter::net::behavior::org_stream_registry::tests::serve_handle_drop_retires_only_its_own_registration)' ; echo "EXIT_CODE=$?"
```

Exit code: **100**. Failure output (verbatim):

```
thread 'adapter::net::behavior::org_stream_registry::tests::serve_handle_drop_retires_only_its_own_registration' (32244) panicked at src\adapter\net\behavior\org_stream_registry.rs:3180:9:
assertion `left == right` failed
  left: 2
 right: 1
```

Restore proof: `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b` == `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`. Restored run (verbatim):

```
        PASS [   0.008s] (1/1) net-mesh adapter::net::behavior::org_stream_registry::tests::serve_handle_drop_retires_only_its_own_registration
     Summary [   0.028s] 1 test run: 1 passed, 5830 skipped
EXIT_CODE=0
```

---

## Closing statements

- **Byte identity:** `net/crates/net/src/adapter/net/behavior/org_stream_registry.rs`
  ends byte-identical to its starting state. Starting sha256 (recorded before
  the first mutation) = final sha256 (measured after the last restore) =
  `348fc6f40beb015c770bb0cfc18c66a7882070725c3d5643ce4f0968e3e77c8b`,
  with all 24 per-restore measurements equal to it.
- **Restore method:** every mutation was undone by the Edit tool applied in
  reverse (anchored, in place). No whole-file writes, no `cp` restores, no
  `git checkout --`. The one non-identical intermediate (receipt 3, finding
  F2) was detected by the sha gate and repaired by a further anchored edit
  before its proof was accepted.
- **Mutation discipline:** all mutations were at production sites (state
  machine, supervisor owner, registry methods, guard, budgets). No edit
  landed in `mod tests`; no test was modified, weakened or re-pinned.
- **Legs never executed:** none. All 24 red legs and all 24 restored legs ran
  on this host, plus the pre-mutation baseline sweep (30/30 green) and the
  `cargo nextest list` name enumeration. No leg was inferred; every PASS and
  FAIL line above was observed.
