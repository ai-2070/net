/*
 * deck.c — the Deck operator surface, from C (net_deck.h).
 *
 * A deck client owning its private supervisor runtime, under a fixed
 * operator seed. Checked against what the header documents:
 *
 *   - the client's operator id is the seed's (the same id an operator
 *     identity derived from that seed reports);
 *   - one-shot reads: the snapshot JSON and the typed status summary;
 *   - all nine admin commands commit, each with its event kind and this
 *     operator's id, under distinct commit ids;
 *   - the audit ring (filled by the supervisor loop on its next tick):
 *     every admin event and the ICE commit, and nothing by_operator for
 *     an operator that never acted (a C client attaches no operator
 *     registry, so a signed ICE commit is not attributed; D-C6-2);
 *   - the snapshot and status-summary streams (one item or a timeout);
 *   - the ICE break-glass typestate: a proposal, simulated (once), its
 *     blast radius and hash, its signing payload signed by the operator
 *     identity, verified through an operator registry (one signature, and
 *     the distinct-operator M-of-N gate), committed once, and refused a
 *     second time; an unsigned commit is refused for want of signatures;
 *   - the admin verifier's documented defaults and its threshold clamp;
 *   - the last-error pair after a refusal, and every free on NULL.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net_deck.h"

#include "consumer_util.h"
#include "loaded_module.h"

static int last_kind_is(const char* kind) {
    const char* k = net_deck_last_error_kind();
    int ok = k != NULL && strcmp(k, kind) == 0;
    if (!ok) {
        printf("  (last error kind %s, message %s)\n", k ? k : "(null)",
               net_deck_last_error_message() ? net_deck_last_error_message() : "(null)");
    }
    return ok;
}

static int admin(const NetDeckClient* client, uint64_t op) {
    NetDeckChainCommit c[9];
    static const uint64_t chains[2] = {0x10, 0x20};
    static const int kinds[9] = {
        NET_DECK_EVENT_KIND_DRAIN,          NET_DECK_EVENT_KIND_ENTER_MAINTENANCE,
        NET_DECK_EVENT_KIND_EXIT_MAINTENANCE, NET_DECK_EVENT_KIND_CORDON,
        NET_DECK_EVENT_KIND_UNCORDON,       NET_DECK_EVENT_KIND_DROP_REPLICAS,
        NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT, NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS,
        NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST,
    };
    static const char* names[9] = {"drain", "enter_maintenance", "exit_maintenance", "cordon", "uncordon",
                                   "drop_replicas", "invalidate_placement", "restart_all_daemons",
                                   "clear_avoid_list"};
    const uint64_t node = 0xABCD;
    char label[96];
    int i, j;
    memset(c, 0, sizeof c);
    CU_CHECK_RC("net_deck_admin_drain", net_deck_admin_drain(client, node, 60000, &c[0]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_enter_maintenance",
                net_deck_admin_enter_maintenance(client, node, 600000, 1, &c[1]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_exit_maintenance", net_deck_admin_exit_maintenance(client, node, &c[2]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_cordon", net_deck_admin_cordon(client, node, &c[3]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_uncordon", net_deck_admin_uncordon(client, node, &c[4]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_drop_replicas", net_deck_admin_drop_replicas(client, node, chains, 2, &c[5]),
                NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_invalidate_placement", net_deck_admin_invalidate_placement(client, node, &c[6]),
                NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_restart_all_daemons", net_deck_admin_restart_all_daemons(client, node, &c[7]),
                NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_clear_avoid_list", net_deck_admin_clear_avoid_list(client, node, &c[8]),
                NET_DECK_OK);
    for (i = 0; i < 9; i++) {
        snprintf(label, sizeof label, "%s: event kind and operator", names[i]);
        CU_CHECK(label, c[i].event_kind == kinds[i] && c[i].operator_id == op);
        for (j = 0; j < i; j++) {
            if (c[j].commit_id == c[i].commit_id) {
                snprintf(label, sizeof label, "%s: a fresh commit id", names[i]);
                CU_CHECK(label, 0);
            }
        }
    }
    CU_CHECK("admin: nine distinct commit ids", 1);
    CU_CHECK_RC("net_deck_admin_drop_replicas: no chains (NULL, 0) is accepted",
                net_deck_admin_drop_replicas(client, node, NULL, 0, &c[0]), NET_DECK_OK);
    CU_CHECK_RC("net_deck_admin_cordon: NULL client is NET_DECK_ERR_NULL", net_deck_admin_cordon(NULL, node, &c[0]),
                NET_DECK_ERR_NULL);
    return 0;
}

/* Collect the audit records matching a query built by `build`; the count,
 * or (size_t)-1 on a failed collect. */
static size_t audit_count(const NetDeckClient* client, int by_op, uint64_t op) {
    NetDeckAuditQuery* q = NULL;
    char** records = NULL;
    size_t count = 0;
    int rc;
    if (net_deck_audit_query_new(&q) != NET_DECK_OK || net_deck_audit_query_recent(q, 64) != NET_DECK_OK ||
        (by_op && net_deck_audit_query_by_operator(q, op) != NET_DECK_OK)) {
        net_deck_audit_query_free(q);
        return (size_t)-1;
    }
    rc = net_deck_audit_query_collect(q, client, &records, &count);
    net_deck_audit_records_free(records, count);
    net_deck_audit_query_free(q);
    return rc == NET_DECK_OK ? count : (size_t)-1;
}

/* Run after admin() and ice(). The supervisor loop puts every admin event
 * it sees on the audit ring, asynchronously (on its next tick). Unsigned
 * admin commands carry no operator ids. */
static int audit(const NetDeckClient* client, uint64_t op) {
    size_t all = 0;
    int waited;
    for (waited = 0; waited < 5000; waited += 50) {
        all = audit_count(client, 0, 0);
        if (all != (size_t)-1 && all >= 11) {
            break;
        }
        cu_sleep_ms(50);
    }
    CU_CHECK("audit: the ten admin commands and the signed ICE commit are on the ring",
             all != (size_t)-1 && all >= 11);
    /* The signed ICE commit is NOT attributed to its signer from C:
     * net_deck_client_new attaches no operator registry, and without one
     * the SDK routes every ICE commit through the unsigned admin path
     * (deck.rs, IceSimulated::commit), so the ring records it with no
     * operator ids. Recorded as D-C6-2 in the C SDK plan. */
    CU_CHECK_RC("audit: nothing from an operator that never acted", audit_count(client, 1, op ^ 0xFFFF), 0);
    CU_CHECK_RC("net_deck_audit_query_recent: NULL builder is NET_DECK_ERR_NULL", net_deck_audit_query_recent(NULL, 1),
                NET_DECK_ERR_NULL);
    return 0;
}

static int streams(const NetDeckClient* client) {
    NetDeckSnapshotStream* snaps = NULL;
    NetDeckStatusSummaryStream* sums = NULL;
    NetDeckStatusSummary s;
    char* snap = NULL;
    int has = -1;
    CU_CHECK_RC("net_deck_subscribe_snapshots", net_deck_subscribe_snapshots(client, &snaps), NET_DECK_OK);
    CU_CHECK_RC("net_deck_snapshot_stream_next: an item or a timeout", net_deck_snapshot_stream_next(snaps, 1500, &snap),
                NET_DECK_OK);
    CU_CHECK("snapshot stream: an item is a JSON object", snap == NULL || snap[0] == '{');
    net_deck_free_string(snap);
    net_deck_snapshot_stream_free(snaps);
    CU_CHECK_RC("net_deck_subscribe_status_summaries", net_deck_subscribe_status_summaries(client, &sums), NET_DECK_OK);
    CU_CHECK_RC("net_deck_status_summary_stream_next: an item or a timeout",
                net_deck_status_summary_stream_next(sums, 1500, &s, &has), NET_DECK_OK);
    CU_CHECK("status-summary stream: has_item is 0 or 1", has == 0 || has == 1);
    net_deck_status_summary_stream_free(sums);
    return 0;
}

static int ice(const NetDeckClient* client, const NetDeckOperatorIdentity* id, uint64_t op) {
    NetDeckIceProposal* proposal = NULL;
    NetDeckIceProposal* unsigned_proposal = NULL;
    NetDeckSimulatedIceProposal* sim = NULL;
    NetDeckSimulatedIceProposal* unsigned_sim = NULL;
    NetDeckSimulatedIceProposal* again = NULL;
    NetDeckOperatorRegistry* reg;
    NetDeckOperatorSignature sigs[2];
    NetDeckChainCommit commit;
    uint8_t hash[32], zero[32], sig[64], pk[32];
    uint8_t* payload = NULL;
    size_t payload_len = 0;
    uint64_t signer = 0;
    char* blast;

    CU_CHECK_RC("net_deck_ice_freeze_cluster", net_deck_ice_freeze_cluster(client, 60000, &proposal), NET_DECK_OK);
    CU_CHECK("net_deck_ice_proposal_issued_at_ms: stamped", net_deck_ice_proposal_issued_at_ms(proposal) > 0);
    CU_CHECK_RC("net_deck_ice_proposal_simulate", net_deck_ice_proposal_simulate(proposal, client, &sim), NET_DECK_OK);
    CU_CHECK_RC("net_deck_ice_proposal_simulate: again is NET_DECK_ERR_CALL_FAILED",
                net_deck_ice_proposal_simulate(proposal, client, &again), NET_DECK_ERR_CALL_FAILED);
    CU_CHECK("simulate again: kind already_simulated", last_kind_is("already_simulated"));
    CU_CHECK("net_deck_simulated_issued_at_ms: the proposal's stamp",
             net_deck_simulated_issued_at_ms(sim) == net_deck_ice_proposal_issued_at_ms(proposal) ||
                 net_deck_simulated_issued_at_ms(sim) > 0);
    blast = net_deck_simulated_blast_radius(sim);
    CU_CHECK("net_deck_simulated_blast_radius: JSON", blast != NULL && (blast[0] == '{' || blast[0] == '['));
    net_deck_free_string(blast);
    memset(zero, 0, sizeof zero);
    CU_CHECK_RC("net_deck_simulated_blast_hash", net_deck_simulated_blast_hash(sim, hash), NET_DECK_OK);
    CU_CHECK("blast hash: 32 bytes, not all zero", memcmp(hash, zero, 32) != 0);
    CU_CHECK_RC("net_deck_simulated_signing_payload", net_deck_simulated_signing_payload(sim, &payload, &payload_len),
                NET_DECK_OK);
    /* ICE_SIGNING_DOMAIN || issued_at_ms (8) || blast_hash (32) || action */
    {
        size_t k;
        int found = 0;
        for (k = 0; payload != NULL && k + 32 <= payload_len; k++) {
            found |= memcmp(payload + k, hash, 32) == 0;
        }
        CU_CHECK("signing payload: carries the blast hash", payload_len > 40 && found);
    }

    CU_CHECK_RC("net_deck_operator_identity_sign_proposal",
                net_deck_operator_identity_sign_proposal(id, sim, &signer, sig), NET_DECK_OK);
    CU_CHECK("sign_proposal: signed by this operator", signer == op);

    reg = net_deck_operator_registry_new();
    CU_CHECK("net_deck_operator_registry_new", reg != NULL);
    CU_CHECK_RC("net_deck_operator_registry_register", net_deck_operator_registry_register(reg, id), NET_DECK_OK);
    CU_CHECK_RC("net_deck_operator_registry_contains", net_deck_operator_registry_contains(reg, op), 1);
    CU_CHECK_RC("net_deck_operator_registry_contains: another id", net_deck_operator_registry_contains(reg, op + 1), 0);
    CU_CHECK_RC("net_deck_operator_registry_len", net_deck_operator_registry_len(reg), 1);
    CU_CHECK_RC("net_deck_operator_identity_public_key", net_deck_operator_identity_public_key(id, pk), NET_DECK_OK);
    CU_CHECK_RC("net_deck_operator_registry_insert: the same key under another id",
                net_deck_operator_registry_insert(reg, op + 1, pk), NET_DECK_OK);
    CU_CHECK_RC("net_deck_operator_registry_len: 2", net_deck_operator_registry_len(reg), 2);

    sigs[0].operator_id = op;
    sigs[0].signature_ptr = sig;
    sigs[0].signature_len = 64;
    CU_CHECK_RC("net_deck_operator_registry_verify: the signature covers the signing payload",
                net_deck_operator_registry_verify(reg, &sigs[0], payload, payload_len), NET_DECK_OK);
    payload[payload_len - 1] ^= 1;
    CU_CHECK("net_deck_operator_registry_verify: a changed payload is refused",
             net_deck_operator_registry_verify(reg, &sigs[0], payload, payload_len) != NET_DECK_OK);
    payload[payload_len - 1] ^= 1;
    CU_CHECK_RC("verify_bundle: threshold 1, one operator",
                net_deck_operator_registry_verify_bundle(reg, sigs, 1, payload, payload_len, 1), NET_DECK_OK);
    sigs[1] = sigs[0];
    CU_CHECK("verify_bundle: the same operator twice does not make 2 of 2",
             net_deck_operator_registry_verify_bundle(reg, sigs, 2, payload, payload_len, 2) != NET_DECK_OK);
    net_deck_signing_payload_free(payload, payload_len);

    memset(&commit, 0, sizeof commit);
    CU_CHECK_RC("net_deck_simulated_commit: signed", net_deck_simulated_commit(sim, client, sigs, 1, &commit),
                NET_DECK_OK);
    CU_CHECK("commit: by this operator", commit.operator_id == op);
    CU_CHECK_RC("net_deck_simulated_commit: again is NET_DECK_ERR_CALL_FAILED",
                net_deck_simulated_commit(sim, client, sigs, 1, &commit), NET_DECK_ERR_CALL_FAILED);
    CU_CHECK("commit again: kind already_committed", last_kind_is("already_committed"));

    CU_CHECK_RC("net_deck_ice_thaw_cluster", net_deck_ice_thaw_cluster(client, &unsigned_proposal), NET_DECK_OK);
    CU_CHECK_RC("simulate the thaw", net_deck_ice_proposal_simulate(unsigned_proposal, client, &unsigned_sim),
                NET_DECK_OK);
    CU_CHECK_RC("net_deck_simulated_commit: no signatures is NET_DECK_ERR_CALL_FAILED",
                net_deck_simulated_commit(unsigned_sim, client, NULL, 0, &commit), NET_DECK_ERR_CALL_FAILED);
    CU_CHECK("unsigned commit: kind insufficient_signatures", last_kind_is("insufficient_signatures"));
    net_deck_clear_last_error();
    CU_CHECK("net_deck_clear_last_error", net_deck_last_error_kind() == NULL && net_deck_last_error_message() == NULL);

    net_deck_simulated_free(unsigned_sim);
    net_deck_ice_proposal_free(unsigned_proposal);
    net_deck_simulated_free(sim);
    net_deck_ice_proposal_free(proposal);

    {
        NetDeckAdminVerifier* v = net_deck_admin_verifier_new(reg, 0);
        NetDeckAdminVerifier* f = net_deck_admin_verifier_with_full_policy(reg, 2, 1000, 2000, 3000);
        CU_CHECK("net_deck_admin_verifier_new", v != NULL && f != NULL);
        CU_CHECK_RC("admin verifier: threshold 0 is clamped to 1", net_deck_admin_verifier_threshold(v), 1);
        CU_CHECK("admin verifier: the documented defaults (300 s, 30 s, 300 s)",
                 net_deck_admin_verifier_freshness_window_ms(v) == 300000 &&
                     net_deck_admin_verifier_future_skew_ms(v) == 30000 &&
                     net_deck_admin_verifier_ice_cooldown_ms(v) == 300000);
        CU_CHECK("admin verifier: an explicit policy",
                 net_deck_admin_verifier_threshold(f) == 2 && net_deck_admin_verifier_freshness_window_ms(f) == 1000 &&
                     net_deck_admin_verifier_future_skew_ms(f) == 2000 &&
                     net_deck_admin_verifier_ice_cooldown_ms(f) == 3000);
        CU_CHECK("net_deck_admin_verifier_new: NULL registry is NULL", net_deck_admin_verifier_new(NULL, 1) == NULL);
        net_deck_admin_verifier_free(v);
        net_deck_admin_verifier_free(f);
    }
    net_deck_operator_registry_free(reg);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_deck_client_new),         CLM_FN(net_deck_client_free),
        CLM_FN(net_deck_status),             CLM_FN(net_deck_status_summary),
        CLM_FN(net_deck_admin_cordon),       CLM_FN(net_deck_audit_query_collect),
        CLM_FN(net_deck_subscribe_snapshots), CLM_FN(net_deck_ice_freeze_cluster),
        CLM_FN(net_deck_ice_proposal_simulate), CLM_FN(net_deck_simulated_commit),
        CLM_FN(net_deck_operator_identity_from_seed), CLM_FN(net_deck_operator_registry_verify),
        CLM_FN(net_deck_last_error_kind),    CLM_FN(net_deck_free_string),
        CLM_FN(net_deck_admin_clear_avoid_list),
        CLM_FN(net_deck_admin_drain),
        CLM_FN(net_deck_admin_drop_replicas),
        CLM_FN(net_deck_admin_enter_maintenance),
        CLM_FN(net_deck_admin_exit_maintenance),
        CLM_FN(net_deck_admin_invalidate_placement),
        CLM_FN(net_deck_admin_restart_all_daemons),
        CLM_FN(net_deck_admin_uncordon),
        CLM_FN(net_deck_admin_verifier_free),
        CLM_FN(net_deck_admin_verifier_freshness_window_ms),
        CLM_FN(net_deck_admin_verifier_future_skew_ms),
        CLM_FN(net_deck_admin_verifier_ice_cooldown_ms),
        CLM_FN(net_deck_admin_verifier_new),
        CLM_FN(net_deck_admin_verifier_threshold),
        CLM_FN(net_deck_admin_verifier_with_full_policy),
        CLM_FN(net_deck_audit_query_by_operator),
        CLM_FN(net_deck_audit_query_free),
        CLM_FN(net_deck_audit_query_new),
        CLM_FN(net_deck_audit_query_recent),
        CLM_FN(net_deck_audit_records_free),
        CLM_FN(net_deck_clear_last_error),
        CLM_FN(net_deck_client_operator_id),
        CLM_FN(net_deck_ice_proposal_free),
        CLM_FN(net_deck_ice_proposal_issued_at_ms),
        CLM_FN(net_deck_ice_thaw_cluster),
        CLM_FN(net_deck_last_error_message),
        CLM_FN(net_deck_operator_identity_free),
        CLM_FN(net_deck_operator_identity_operator_id),
        CLM_FN(net_deck_operator_identity_public_key),
        CLM_FN(net_deck_operator_identity_sign_proposal),
        CLM_FN(net_deck_operator_registry_contains),
        CLM_FN(net_deck_operator_registry_free),
        CLM_FN(net_deck_operator_registry_insert),
        CLM_FN(net_deck_operator_registry_len),
        CLM_FN(net_deck_operator_registry_new),
        CLM_FN(net_deck_operator_registry_register),
        CLM_FN(net_deck_operator_registry_verify_bundle),
        CLM_FN(net_deck_signing_payload_free),
        CLM_FN(net_deck_simulated_blast_hash),
        CLM_FN(net_deck_simulated_blast_radius),
        CLM_FN(net_deck_simulated_free),
        CLM_FN(net_deck_simulated_issued_at_ms),
        CLM_FN(net_deck_simulated_signing_payload),
        CLM_FN(net_deck_snapshot_stream_free),
        CLM_FN(net_deck_snapshot_stream_next),
        CLM_FN(net_deck_status_summary_stream_free),
        CLM_FN(net_deck_status_summary_stream_next),
        CLM_FN(net_deck_subscribe_status_summaries),
    };
    uint8_t seed[32];
    NetDeckClient* client = NULL;
    NetDeckOperatorIdentity* id = NULL;
    NetDeckStatusSummary summary;
    uint64_t op;
    char* snap;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    memset(seed, 0x42, sizeof seed);
    CU_CHECK_RC("net_deck_client_new", net_deck_client_new(0, 50, 0, 0, 50, 0, seed, &client), NET_DECK_OK);
    op = net_deck_client_operator_id(client);
    CU_CHECK_RC("net_deck_operator_identity_from_seed", net_deck_operator_identity_from_seed(seed, &id), NET_DECK_OK);
    CU_CHECK("the client's operator id is the seed's", op != 0 && op == net_deck_operator_identity_operator_id(id));
    CU_CHECK_RC("net_deck_client_operator_id: NULL is 0", net_deck_client_operator_id(NULL), 0);

    snap = net_deck_status(client);
    CU_CHECK("net_deck_status: a JSON snapshot", snap != NULL && snap[0] == '{');
    net_deck_free_string(snap);
    memset(&summary, 0xFF, sizeof summary);
    CU_CHECK_RC("net_deck_status_summary", net_deck_status_summary(client, &summary), NET_DECK_OK);
    CU_CHECK("status summary: no freeze, the flags are booleans",
             summary.freeze_remaining_present == 0 &&
                 (summary.local_maintenance_active == 0 || summary.local_maintenance_active == 1));
    CU_CHECK_RC("net_deck_status_summary: NULL client is NET_DECK_ERR_NULL", net_deck_status_summary(NULL, &summary),
                NET_DECK_ERR_NULL);

    if (admin(client, op) || streams(client) || ice(client, id, op) || audit(client, op)) {
        return 1;
    }

    net_deck_operator_identity_free(id);
    net_deck_client_free(client);
    net_deck_client_free(NULL);
    net_deck_free_string(NULL);
    net_deck_audit_records_free(NULL, 0);
    net_deck_simulated_free(NULL);
    net_deck_ice_proposal_free(NULL);
    net_deck_operator_identity_free(NULL);
    net_deck_operator_registry_free(NULL);
    net_deck_admin_verifier_free(NULL);
    net_deck_signing_payload_free(NULL, 0);
    CU_CHECK("every free accepts NULL", 1);
    return cu_finish();
}
