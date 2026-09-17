//! Durable admission records: ownership, the D2 transition table, the
//! launch ledger, and the three retention classes
//! (`A2A_PAID_ADMISSION_PLAN.md` D6).
//!
//! Every witness here is about a property a paid provider's money
//! depends on: that one live owner writes a journal, that a launch and
//! its ledger entry are one indivisible fact, that a failed write runs
//! nothing, and that a record whose payment is unaccounted for is never
//! aged out by a retention timer.

#![cfg(all(feature = "net", feature = "testing"))]

use std::path::Path;

use net_sdk::a2a::{A2aBounds, A2aOffer, TaskBrief, TaskOwner, TaskState};
use net_sdk::a2a_journal::{
    A2aAdmissionJournal, A2aAdmissions, A2aJournalError, AdmissionRecord, AdmissionState,
    AdmissionStore, AttemptNote, InsertOutcome, StateTag, DETAIL_ADMISSION_REVOKED,
    DETAIL_OUTCOME_UNKNOWN, DETAIL_PAID_NOT_STARTED,
};

const NOW: u64 = 1_700_000_000;
const PAYER: [u8; 32] = [0xAB; 32];
const SERVICE: &str = "svc.summarize";

fn offer(paid: bool) -> A2aOffer {
    A2aOffer {
        service_id: SERVICE.to_string(),
        revision: "r1".to_string(),
        description: None,
        pricing_terms: paid.then(|| r#"{"object":"net.pricing.terms@1"}"#.to_string()),
        bounds: A2aBounds {
            max_prompt_bytes: 4096,
            max_context_refs: 8,
            max_tags: 8,
            max_tag_bytes: 64,
            max_in_flight: 16,
        },
        reservation_ttl_secs: 300,
        reservation_retention_secs: 7 * 24 * 60 * 60,
        retention_secs: 60 * 60,
    }
}

fn brief(task_id: &str) -> TaskBrief {
    TaskBrief {
        task_id: task_id.to_string(),
        prompt: "summarize the quarterly filings".to_string(),
        context_refs: vec!["blob://ctx".to_string()],
        tags: vec!["finance".to_string()],
        service: Some(SERVICE.to_string()),
        revision: Some("r1".to_string()),
    }
}

/// A fresh reservation for `task_id` under `offer`. The owner is an
/// `Entity`, so every journal round-trip also exercises the 32-byte
/// owner encoding rather than only the cheap `Local` arm.
fn reservation(task_id: &str, offer: &A2aOffer, now: u64) -> AdmissionRecord {
    AdmissionRecord::reserved(
        TaskOwner::Entity([9u8; 32]),
        brief(task_id),
        offer,
        Some(format!("adm-{task_id}")),
        format!("commit-{task_id}"),
        now,
    )
}

fn owner() -> TaskOwner {
    TaskOwner::Entity([9u8; 32])
}

fn paid_state() -> AdmissionState {
    AdmissionState::Paid {
        quote_id: "q-1".to_string(),
        payer: PAYER,
    }
}

// ---------------------------------------------------------------------------
// Ownership (plan finding r3-1)
// ---------------------------------------------------------------------------

/// Env selector that turns this test binary into the second-owner probe.
const CHILD_ENV: &str = "NET_A2A_JOURNAL_CHILD";
/// The journal the probe should try to open.
const CHILD_PATH_ENV: &str = "NET_A2A_JOURNAL_CHILD_PATH";
/// The probe was refused with `OwnedElsewhere` — the expected verdict
/// while the incumbent owner is alive.
const EXIT_OWNED_ELSEWHERE: i32 = 40;
/// The probe opened the journal AND could read the incumbent's record.
const EXIT_OPENED: i32 = 41;
/// Anything else.
const EXIT_OTHER: i32 = 42;

/// The child half of [`a_second_owner_is_excluded_after_a_journal_replacement`]:
/// a genuinely separate process, so the exclusion being tested is the OS
/// lock and not this process's own registry.
async fn child_probe() -> ! {
    let path = std::env::var(CHILD_PATH_ENV).expect("child journal path");
    match A2aAdmissionJournal::open(&path).await {
        Err(A2aJournalError::OwnedElsewhere { .. }) => {
            eprintln!("child: refused with OwnedElsewhere");
            std::process::exit(EXIT_OWNED_ELSEWHERE)
        }
        Ok(journal) => {
            // Opening is only half the claim: a successor must see what
            // the incumbent wrote, which is what proves the sidecar lock
            // guarded a journal that had already been replaced.
            let seen = journal
                .lookup(TaskOwner::Entity([9u8; 32]), "t-owned")
                .await
                .expect("child lookup")
                .is_some();
            eprintln!("child: opened, incumbent record visible = {seen}");
            std::process::exit(if seen { EXIT_OPENED } else { EXIT_OTHER })
        }
        Err(e) => {
            eprintln!("child: unexpected error: {e}");
            std::process::exit(EXIT_OTHER)
        }
    }
}

fn run_child(path: &Path) -> std::process::Output {
    let exe = std::env::current_exe().expect("test binary path");
    std::process::Command::new(exe)
        .args([
            "--exact",
            "a_second_owner_is_excluded_after_a_journal_replacement",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .env(CHILD_PATH_ENV, path)
        .output()
        .expect("spawn the second-owner probe")
}

fn child_verdict(out: &std::process::Output) -> String {
    format!(
        "exit={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The ownership lock survives the journal file being replaced.
///
/// Owner A opens, then performs a successful write — which renames a new
/// file over the old one. A lock taken on the journal *file* would now be
/// guarding an unlinked inode, and a second process would sail in. The
/// lock is on the stable `.owner` sidecar, so it does not.
#[tokio::test]
async fn a_second_owner_is_excluded_after_a_journal_replacement() {
    if std::env::var(CHILD_ENV).is_ok() {
        child_probe().await;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");

    let a = A2aAdmissionJournal::open(&path)
        .await
        .expect("first owner opens");
    let inserted = a
        .insert_reserved(reservation("t-owned", &offer(true), NOW))
        .await
        .expect("insert");
    assert_eq!(inserted, InsertOutcome::Inserted);
    assert!(
        path.exists(),
        "the journal file must have been written (and therefore replaced) before the probe"
    );
    assert_eq!(
        a.durable_writes(),
        1,
        "exactly one atomic replace should have published the reservation"
    );

    let refused = run_child(&path);
    assert_eq!(
        refused.status.code(),
        Some(EXIT_OWNED_ELSEWHERE),
        "a second process must be refused while owner A is alive: {}",
        child_verdict(&refused)
    );

    drop(a);

    let accepted = run_child(&path);
    assert_eq!(
        accepted.status.code(),
        Some(EXIT_OPENED),
        "the successor must acquire cleanly once owner A is gone, and read what A wrote: {}",
        child_verdict(&accepted)
    );
}

#[tokio::test]
async fn a_second_open_in_the_same_process_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");

    let a = A2aAdmissionJournal::open(&path).await.expect("first open");
    match A2aAdmissionJournal::open(&path).await {
        Err(A2aJournalError::OwnedElsewhere { .. }) => {}
        Err(e) => panic!("expected OwnedElsewhere, got {e}"),
        Ok(_) => panic!("a second in-process open must not succeed"),
    }

    // The positive control: the refusal is about the live owner, not
    // about the path being unopenable.
    drop(a);
    let b = A2aAdmissionJournal::open(&path)
        .await
        .expect("reopen after the owner is dropped");
    drop(b);
}

// ---------------------------------------------------------------------------
// The transition table
// ---------------------------------------------------------------------------

/// A representative target state per tag.
fn target(tag: StateTag) -> AdmissionState {
    match tag {
        StateTag::Reserved => AdmissionState::Reserved {
            expires_at: NOW + 900,
        },
        StateTag::Paid => paid_state(),
        StateTag::Launched => AdmissionState::Launched {
            quote_id: Some("q-1".to_string()),
            payer: Some(PAYER),
        },
        StateTag::Terminal => AdmissionState::Terminal {
            quote_id: Some("q-1".to_string()),
            payer: Some(PAYER),
            state: TaskState::Completed {
                result_ref: "blob://out".to_string(),
            },
        },
        StateTag::Reconcile => AdmissionState::Reconcile {
            reason: "preflight_revoked".to_string(),
            claimed_quote_id: Some("q-1".to_string()),
            payer: Some(PAYER),
        },
    }
}

/// Put a fresh record for `task_id` into `tag`, using only the store's
/// own verbs (so a state this store cannot reach at all would fail here
/// rather than being asserted about).
async fn record_in(store: &dyn AdmissionStore, tag: StateTag, task_id: &str) {
    let inserted = store
        .insert_reserved(reservation(task_id, &offer(true), NOW))
        .await
        .expect("insert");
    assert_eq!(inserted, InsertOutcome::Inserted);
    match tag {
        StateTag::Reserved => {}
        StateTag::Paid => store
            .transition(owner(), task_id, &[StateTag::Reserved], paid_state(), NOW)
            .await
            .expect("reserved -> paid"),
        StateTag::Launched | StateTag::Terminal => {
            store
                .transition(owner(), task_id, &[StateTag::Reserved], paid_state(), NOW)
                .await
                .expect("reserved -> paid");
            store
                .claim_launch(owner(), task_id, NOW)
                .await
                .expect("claim launch");
            if tag == StateTag::Terminal {
                store
                    .record_terminal(
                        owner(),
                        task_id,
                        TaskState::Completed {
                            result_ref: "blob://out".to_string(),
                        },
                        NOW,
                    )
                    .await
                    .expect("record terminal");
            }
        }
        StateTag::Reconcile => store
            .transition(
                owner(),
                task_id,
                &[StateTag::Reserved],
                target(StateTag::Reconcile),
                NOW,
            )
            .await
            .expect("reserved -> reconcile"),
    }
    let found = store
        .lookup(owner(), task_id)
        .await
        .expect("lookup")
        .expect("record exists");
    assert_eq!(found.state.tag(), tag, "failed to stage a {tag:?} record");
}

/// Every `(from tag, to tag)` pair, with whether `transition` accepted
/// it. Driven through the trait so the journal and the memory store run
/// the identical body.
async fn transition_verdicts(store: &dyn AdmissionStore) -> Vec<((StateTag, StateTag), bool)> {
    let mut out = Vec::new();
    for from in StateTag::ALL {
        for to in StateTag::ALL {
            let task_id = format!("t-{}-{}", from.as_str(), to.as_str());
            record_in(store, from, &task_id).await;
            let accepted = store
                .transition(owner(), &task_id, &[from], target(to), NOW + 1)
                .await;
            if let Err(e) = &accepted {
                assert!(
                    matches!(e, A2aJournalError::Conflict { .. }),
                    "a refused transition must be a Conflict, not {e}"
                );
            }
            // A refusal must be a refusal all the way down: the record
            // still holds the state it was staged in.
            let state = store
                .lookup(owner(), &task_id)
                .await
                .expect("lookup")
                .expect("record")
                .state
                .tag();
            assert_eq!(
                state,
                if accepted.is_ok() { to } else { from },
                "{from:?} -> {to:?} reported {accepted:?} but the record says {state:?}"
            );
            out.push(((from, to), accepted.is_ok()));
        }
    }
    out
}

/// The `from` sets the D2 sequences pass, spelled out here rather than
/// read back from the implementation.
const ALLOWED: [(StateTag, StateTag); 4] = [
    (StateTag::Reserved, StateTag::Reserved),
    (StateTag::Reserved, StateTag::Paid),
    (StateTag::Reserved, StateTag::Reconcile),
    (StateTag::Paid, StateTag::Reconcile),
];

#[tokio::test]
async fn every_transition_outside_the_table_is_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    let verdicts = transition_verdicts(&journal).await;
    assert_eq!(
        verdicts.len(),
        25,
        "all five tags must be crossed with all five"
    );

    let accepted: Vec<(StateTag, StateTag)> = verdicts
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(pair, _)| *pair)
        .collect();
    // Asserting the count separately from the contents: a table that
    // refused everything would satisfy "no pair outside the set was
    // accepted" vacuously.
    assert_eq!(
        accepted.len(),
        ALLOWED.len(),
        "expected exactly {} allowed transitions, got {accepted:?}",
        ALLOWED.len()
    );
    assert_eq!(accepted, ALLOWED.to_vec());

    // The `from` set is a second, independent guard: a pair that IS in
    // the table is still refused when the record is not in the state the
    // caller believed it was.
    record_in(&journal, StateTag::Reserved, "t-stale-from").await;
    let stale = journal
        .transition(
            owner(),
            "t-stale-from",
            &[StateTag::Paid],
            target(StateTag::Reconcile),
            NOW + 1,
        )
        .await;
    assert!(
        matches!(stale, Err(A2aJournalError::Conflict { .. })),
        "a reserved record must refuse a transition whose `from` set says paid: {stale:?}"
    );
}

#[tokio::test]
async fn the_memory_store_and_the_journal_agree_on_the_transition_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");
    let memory = A2aAdmissions::new();

    let from_journal = transition_verdicts(&journal).await;
    let from_memory = transition_verdicts(&memory).await;

    assert_eq!(
        from_journal, from_memory,
        "the in-memory store must enforce the same table as the journal"
    );
    assert_eq!(
        from_memory.iter().filter(|(_, ok)| *ok).count(),
        ALLOWED.len(),
        "both stores agreeing on 'refuse everything' would not be agreement worth having"
    );
}

#[tokio::test]
async fn a_gate_denial_note_does_not_change_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");
    journal
        .insert_reserved(reservation("t-note", &offer(true), NOW))
        .await
        .expect("insert");
    let before = journal
        .lookup(owner(), "t-note")
        .await
        .expect("lookup")
        .expect("record");

    journal
        .note(
            owner(),
            "t-note",
            AttemptNote {
                at: NOW + 30,
                reason: "quote_not_settled".to_string(),
                claimed_quote_id: Some("q-bad".to_string()),
            },
        )
        .await
        .expect("note");

    let after = journal
        .lookup(owner(), "t-note")
        .await
        .expect("lookup")
        .expect("record");
    assert_eq!(
        after.state, before.state,
        "a denial must not move the state"
    );
    assert_eq!(
        after.updated_at, before.updated_at,
        "a denial must not move the retention clock — otherwise a retry loop \
         keeps a reservation alive forever"
    );
    assert_eq!(after.admission_id, before.admission_id);
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].reason, "quote_not_settled");
    assert_eq!(after.attempts[0].claimed_quote_id.as_deref(), Some("q-bad"));
    assert!(
        !journal.ledger_has(owner(), "t-note").await.expect("ledger"),
        "a denial launches nothing"
    );
}

#[tokio::test]
async fn claim_launch_writes_the_launched_state_and_the_ledger_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    record_in(&journal, StateTag::Paid, "t-claim").await;
    let before = journal.durable_writes();

    let entry = journal
        .claim_launch(owner(), "t-claim", NOW + 5)
        .await
        .expect("claim");

    // ONE atomic replace. Two would mean an instant in which the file
    // says launched and the ledger does not — the torn state a crash
    // between them makes permanent.
    assert_eq!(
        journal.durable_writes() - before,
        1,
        "the launched state and the ledger entry must land in exactly one atomic replace"
    );
    assert_eq!(entry.task_id, "t-claim");
    assert_eq!(entry.quote_id.as_deref(), Some("q-1"));
    assert_eq!(entry.commitment, "commit-t-claim");
    assert_eq!(entry.launched_at, NOW + 5);
    let record = journal
        .lookup(owner(), "t-claim")
        .await
        .expect("lookup")
        .expect("record");
    assert_eq!(
        record.state,
        AdmissionState::Launched {
            quote_id: Some("q-1".to_string()),
            payer: Some(PAYER),
        }
    );
    assert!(journal
        .ledger_has(owner(), "t-claim")
        .await
        .expect("ledger"));

    // And the other side of the same fact: when the replace fails,
    // NEITHER half exists. This is what lets the serving path spawn only
    // after `Ok`.
    record_in(&journal, StateTag::Paid, "t-claim-fails").await;
    let before = journal.durable_writes();
    journal.fail_next_write();
    let failed = journal
        .claim_launch(owner(), "t-claim-fails", NOW + 5)
        .await;
    assert!(
        matches!(failed, Err(A2aJournalError::Io { .. })),
        "expected an Io failure, got {failed:?}"
    );
    assert_eq!(
        journal.durable_writes(),
        before,
        "a failed claim publishes nothing"
    );
    assert_eq!(
        journal
            .lookup(owner(), "t-claim-fails")
            .await
            .expect("lookup")
            .expect("record")
            .state,
        paid_state(),
        "a failed claim leaves the admission paid, so the retry re-enters at S6′"
    );
    assert!(
        !journal
            .ledger_has(owner(), "t-claim-fails")
            .await
            .expect("ledger"),
        "a failed claim must not leave a ledger entry behind"
    );
}

// ---------------------------------------------------------------------------
// Retention (plan finding r3-3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unresolved_paid_launched_and_reconcile_records_survive_result_retention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    record_in(&journal, StateTag::Paid, "t-paid").await;
    record_in(&journal, StateTag::Launched, "t-launched").await;
    record_in(&journal, StateTag::Reconcile, "t-reconcile").await;
    // The positive control: a resolved result under the same terms, so a
    // prune that did nothing at all cannot pass this test.
    record_in(&journal, StateTag::Terminal, "t-terminal").await;

    let far_future = NOW + 10 * 365 * 24 * 60 * 60;
    let pruned = journal.prune(far_future).await.expect("prune");
    assert_eq!(
        pruned, 1,
        "only the resolved result ages out; the control proves prune ran"
    );

    for id in ["t-paid", "t-launched", "t-reconcile"] {
        assert!(
            journal.lookup(owner(), id).await.expect("lookup").is_some(),
            "{id} is unresolved financial evidence and must never be pruned automatically"
        );
    }
    assert!(journal
        .lookup(owner(), "t-terminal")
        .await
        .expect("lookup")
        .is_none());

    let unresolved: Vec<String> = journal
        .unresolved()
        .await
        .expect("unresolved")
        .into_iter()
        .map(|r| r.task_id)
        .collect();
    assert_eq!(
        unresolved,
        vec![
            "t-launched".to_string(),
            "t-paid".to_string(),
            "t-reconcile".to_string()
        ],
        "the operator view is exactly the never-pruned class"
    );

    // `forget` is the immediate path for a *result*; it must refuse the
    // unresolved class just as prune does.
    let refused = journal.forget(owner(), "t-launched").await;
    assert!(
        matches!(refused, Err(A2aJournalError::Conflict { .. })),
        "forget must refuse an unresolved record: {refused:?}"
    );
}

#[tokio::test]
async fn an_unpaid_reservation_is_pruned_after_reservation_retention_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    // Result retention far shorter than reservation retention: the two
    // classes must not be confused for each other.
    let mut terms = offer(true);
    terms.retention_secs = 10;
    terms.reservation_retention_secs = 100;

    journal
        .insert_reserved(reservation("t-quiet", &terms, NOW))
        .await
        .expect("insert");
    journal
        .insert_reserved(reservation("t-noted", &terms, NOW))
        .await
        .expect("insert");
    journal
        .note(
            owner(),
            "t-noted",
            AttemptNote {
                at: NOW + 5,
                reason: "quote_expired".to_string(),
                claimed_quote_id: None,
            },
        )
        .await
        .expect("note");

    assert_eq!(
        journal.prune(NOW + 99).await.expect("prune"),
        0,
        "a reservation must outlive the RESULT retention window"
    );
    for id in ["t-quiet", "t-noted"] {
        assert!(journal.lookup(owner(), id).await.expect("lookup").is_some());
    }

    assert_eq!(
        journal.prune(NOW + 101).await.expect("prune"),
        2,
        "both reservations age out at reservation retention — notes do not extend one"
    );
    for id in ["t-quiet", "t-noted"] {
        assert!(journal.lookup(owner(), id).await.expect("lookup").is_none());
    }
}

#[tokio::test]
async fn a_retired_result_still_answers_ledger_has() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    record_in(&journal, StateTag::Terminal, "t-retired").await;
    assert!(journal
        .ledger_has(owner(), "t-retired")
        .await
        .expect("ledger"));

    assert_eq!(
        journal
            .prune(NOW + offer(true).retention_secs + 1)
            .await
            .expect("prune"),
        1
    );
    assert!(
        journal
            .lookup(owner(), "t-retired")
            .await
            .expect("lookup")
            .is_none(),
        "the result was retired"
    );
    assert!(
        journal
            .ledger_has(owner(), "t-retired")
            .await
            .expect("ledger"),
        "the ledger outlives the result — a retry after retention answers Retired, \
         never a second execution"
    );

    // The same must hold for the immediate `forget` path.
    record_in(&journal, StateTag::Terminal, "t-forgotten").await;
    assert!(journal
        .forget(owner(), "t-forgotten")
        .await
        .expect("forget"));
    assert!(journal
        .ledger_has(owner(), "t-forgotten")
        .await
        .expect("ledger"));
}

// ---------------------------------------------------------------------------
// Durability and recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_write_leaves_the_journal_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");
    let journal = A2aAdmissionJournal::open(&path).await.expect("open");

    journal
        .insert_reserved(reservation("t-io", &offer(true), NOW))
        .await
        .expect("insert");
    let before_bytes = std::fs::read(&path).expect("read journal");
    let before_writes = journal.durable_writes();

    journal.fail_next_write();
    let failed = journal
        .transition(
            owner(),
            "t-io",
            &[StateTag::Reserved],
            paid_state(),
            NOW + 1,
        )
        .await;
    assert!(
        matches!(failed, Err(A2aJournalError::Io { .. })),
        "expected Io, got {failed:?}"
    );

    assert_eq!(
        std::fs::read(&path).expect("read journal"),
        before_bytes,
        "a failed write must leave the file byte-identical"
    );
    assert_eq!(journal.durable_writes(), before_writes);
    assert_eq!(
        journal
            .lookup(owner(), "t-io")
            .await
            .expect("lookup")
            .expect("record")
            .state
            .tag(),
        StateTag::Reserved,
        "the transition did not happen"
    );
    assert!(
        std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp.")),
        "a failed write must not leak a temp sibling"
    );

    // The positive control: the store is still usable, so the assertion
    // above is about one injected failure and not a dead journal.
    journal
        .transition(
            owner(),
            "t-io",
            &[StateTag::Reserved],
            paid_state(),
            NOW + 2,
        )
        .await
        .expect("the next write succeeds");
    assert_eq!(journal.durable_writes(), before_writes + 1);
}

#[tokio::test]
async fn a_successor_owner_surfaces_paid_not_started_and_outcome_unknown_without_relaunching() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("admissions.json");

    let first = A2aAdmissionJournal::open(&path).await.expect("open");
    record_in(&first, StateTag::Paid, "t-paid").await;
    record_in(&first, StateTag::Launched, "t-launched").await;
    record_in(&first, StateTag::Reconcile, "t-reconcile").await;
    record_in(&first, StateTag::Terminal, "t-done").await;
    let before_bytes = std::fs::read(&path).expect("read journal");
    // The crash: the owner goes away with three admissions unresolved.
    drop(first);

    let successor = A2aAdmissionJournal::open(&path).await.expect("reopen");

    assert_eq!(
        successor.durable_writes(),
        0,
        "recovery must not write: a successor never rewrites Paid / Launched / Reconcile"
    );
    assert_eq!(
        std::fs::read(&path).expect("read journal"),
        before_bytes,
        "the journal must be byte-identical after a recovery pass"
    );

    let mut recovered: Vec<(String, TaskState)> = successor
        .recovered()
        .iter()
        .map(|r| (r.task_id.clone(), r.status.clone()))
        .collect();
    recovered.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        recovered,
        vec![
            (
                "t-launched".to_string(),
                TaskState::Interrupted {
                    detail: DETAIL_OUTCOME_UNKNOWN.to_string()
                }
            ),
            (
                "t-paid".to_string(),
                TaskState::Interrupted {
                    detail: DETAIL_PAID_NOT_STARTED.to_string()
                }
            ),
            (
                "t-reconcile".to_string(),
                TaskState::Interrupted {
                    detail: DETAIL_ADMISSION_REVOKED.to_string()
                }
            ),
        ],
        "each unresolved state answers its own ambiguity, and the recorded result is not one"
    );

    // The states themselves are untouched, and the paid admission still
    // has no ledger entry — nothing was relaunched on the way in.
    assert_eq!(
        successor
            .lookup(owner(), "t-paid")
            .await
            .expect("lookup")
            .expect("record")
            .state,
        paid_state()
    );
    assert!(
        !successor
            .ledger_has(owner(), "t-paid")
            .await
            .expect("ledger"),
        "recovery must not claim a launch"
    );
    assert!(successor
        .ledger_has(owner(), "t-launched")
        .await
        .expect("ledger"));
    // The recorded result answers as recorded, not as interrupted.
    assert_eq!(
        successor
            .lookup(owner(), "t-done")
            .await
            .expect("lookup")
            .expect("record")
            .state
            .status(),
        Some(TaskState::Completed {
            result_ref: "blob://out".to_string()
        })
    );

    // An operator resolve is the one exit, and it is what lets retention
    // finally reach the record.
    successor
        .resolve(
            owner(),
            "t-paid",
            TaskState::Failed {
                error: "refunded out of band".to_string(),
            },
            NOW + 10,
        )
        .await
        .expect("operator resolve");
    assert!(successor
        .unresolved()
        .await
        .expect("unresolved")
        .iter()
        .all(|r| r.task_id != "t-paid"));
}

#[tokio::test]
async fn capacity_counts_only_in_flight_admissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = A2aAdmissionJournal::open(dir.path().join("admissions.json"))
        .await
        .expect("open");

    // Holds capacity: a live reservation, a paid admission, a launched one.
    journal
        .insert_reserved(reservation("t-live", &offer(true), NOW))
        .await
        .expect("insert");
    record_in(&journal, StateTag::Paid, "t-paid").await;
    record_in(&journal, StateTag::Launched, "t-launched").await;
    // Holds none: a lapsed reservation, a recorded result, a revocation.
    let mut short = offer(true);
    short.reservation_ttl_secs = 1;
    journal
        .insert_reserved(reservation("t-lapsed", &short, NOW))
        .await
        .expect("insert");
    record_in(&journal, StateTag::Terminal, "t-done").await;
    record_in(&journal, StateTag::Reconcile, "t-revoked").await;

    assert_eq!(
        journal.in_flight(SERVICE, NOW).await.expect("in flight"),
        4,
        "before it lapses, the short reservation counts too — the control for the next assertion"
    );
    assert_eq!(
        journal
            .in_flight(SERVICE, NOW + 2)
            .await
            .expect("in flight"),
        3,
        "a lapsed reservation releases capacity; a recorded result and a revocation never held it"
    );
    assert_eq!(
        journal
            .in_flight("svc.other", NOW + 2)
            .await
            .expect("in flight"),
        0,
        "capacity is per service"
    );
}
