/*
 * Net C SDK — Capability + Predicate Example
 *
 * Demonstrates the stateless capability / predicate helpers
 * shipped in the v0.10+1 cycle (Phase 9a, 9b, 9c, 9d of
 * `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`):
 *
 *   - net_validate_capabilities       — wire-format CapabilitySet
 *                                       validator
 *   - net_predicate_evaluate          — local predicate evaluator
 *   - net_predicate_evaluate_with_trace
 *                                     — single-eval clause trace
 *   - net_predicate_aggregate_debug_report
 *                                     — corpus-wide aggregator
 *   - net_predicate_redact_metadata_keys
 *                                     — host-side label redaction
 *   - net_predicate_to_where_header   — `cyberdeck-where:` encoder
 *
 * No mesh or RPC handle required — every helper is pure
 * (JSON in, JSON out). Pair the where-header output with
 * net_rpc_call_with_headers (declared in net_rpc.h, see the
 * predicate-pushdown comment block at the bottom) for end-to-
 * end Phase 9b filtering.
 *
 * Build (Linux):
 *   cargo build --release --features ffi,net
 *   gcc -o capability examples/capability.c -L target/release -lnet -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=target/release ./capability
 */

/* `net.go.h` is the broader Go-binding superset that already
 * declares every symbol this example uses (capability + predicate
 * helpers PLUS `net_free_string`). The canonical `net.h` shares
 * the same `NET_SDK_H` include guard, so including both in one
 * TU silently skips the second. CR-5: drop the redundant include
 * — it produced implicit-declaration errors on GCC 14+/Clang 16+
 * for every `net_validate_capabilities` / `net_predicate_*`
 * symbol the example calls. */
#include "../include/net.go.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Tiny helper: print a NUL-terminated FFI string and free it. */
static void print_and_free(const char* label, char* s) {
    if (s == NULL) {
        printf("%s: <null>\n", label);
        return;
    }
    printf("%s: %s\n", label, s);
    net_free_string(s);
}

int main(void) {
    /* =====================================================
     * 1. Validate a CapabilitySet against the canonical
     *    AXIS_SCHEMA. Non-canonical axes (e.g. "compute.gpu"
     *    typo) surface as legacy-tag warnings, not errors;
     *    type mismatches (e.g. "memory_mb=lots") fire as
     *    type_mismatch errors.
     * ===================================================== */
    {
        const char* caps =
            "{\"tags\": [\"hardware.gpu\", \"hardware.memory_mb=lots\","
            " \"compute.gpu\"], \"metadata\": {\"region\": \"us-east\"}}";
        char* report = NULL;
        size_t report_len = 0;
        int rc = net_validate_capabilities(caps, &report, &report_len);
        if (rc != 0) {
            fprintf(stderr, "validate_capabilities returned %d\n", rc);
            return 1;
        }
        print_and_free("[validate] report", report);
    }

    /* =====================================================
     * 2. Evaluate a predicate against a (tags, metadata)
     *    context. Returns 1 (true) / 0 (false) / negative on
     *    error.
     *
     *    Predicate: hardware.gpu present AND region == us-east
     * ===================================================== */
    {
        const char* pred =
            "{\"nodes\":["
            "{\"kind\":\"exists\",\"key\":{\"axis\":\"hardware\",\"key\":\"gpu\"}},"
            "{\"kind\":\"metadata_equals\",\"key\":\"region\",\"value\":\"us-east\"},"
            "{\"kind\":\"and\",\"children\":[0,1]}"
            "],\"root_idx\":2}";
        const char* tags = "[\"hardware.gpu\"]";
        const char* metadata = "{\"region\":\"us-east\"}";

        int rc = net_predicate_evaluate(pred, tags, metadata);
        printf("[evaluate] result: %d (1=match, 0=no-match, <0=error)\n", rc);
    }

    /* =====================================================
     * 3. Evaluate WITH TRACE — same predicate, but also get
     *    back the per-clause execution tree showing which
     *    clauses ran and what they returned. Useful for
     *    debugging "why did my query return 0?".
     * ===================================================== */
    {
        const char* pred =
            "{\"nodes\":["
            "{\"kind\":\"exists\",\"key\":{\"axis\":\"hardware\",\"key\":\"gpu\"}},"
            "{\"kind\":\"metadata_equals\",\"key\":\"region\",\"value\":\"us-east\"},"
            "{\"kind\":\"and\",\"children\":[0,1]}"
            "],\"root_idx\":2}";
        int result = -1;
        char* trace = NULL;
        size_t trace_len = 0;
        int rc = net_predicate_evaluate_with_trace(
            pred, "[\"hardware.gpu\"]", "{\"region\":\"us-east\"}",
            &result, &trace, &trace_len);
        if (rc != 0) {
            fprintf(stderr, "evaluate_with_trace returned %d\n", rc);
            return 1;
        }
        printf("[trace] result=%d\n", result);
        print_and_free("[trace] tree", trace);
    }

    /* =====================================================
     * 4. Aggregate a debug report across a 3-row corpus —
     *    answers "how many candidates matched, how often did
     *    each clause filter?" Useful when tuning a predicate
     *    for selectivity.
     * ===================================================== */
    {
        const char* pred =
            "{\"nodes\":["
            "{\"kind\":\"metadata_equals\",\"key\":\"region\",\"value\":\"us-east\"}"
            "],\"root_idx\":0}";
        const char* contexts =
            "[{\"tags\":[],\"metadata\":{\"region\":\"us-east\"}},"
            "{\"tags\":[],\"metadata\":{\"region\":\"us-west\"}},"
            "{\"tags\":[],\"metadata\":{\"region\":\"us-east\"}}]";
        char* report = NULL;
        size_t report_len = 0;
        int rc = net_predicate_aggregate_debug_report(pred, contexts, &report, &report_len);
        if (rc != 0) {
            fprintf(stderr, "aggregate_debug_report returned %d\n", rc);
            return 1;
        }
        print_and_free("[report] aggregated", report);
    }

    /* =====================================================
     * 5. Redact metadata-clause labels before persistence —
     *    rewrites
     *      MetadataEquals(api_key=sk-secret-1) →
     *      MetadataEquals(api_key=<redacted>)
     *    leaving non-targeted labels untouched. Idempotent.
     * ===================================================== */
    {
        const char* report =
            "{"
            "\"total_candidates\":10,"
            "\"matched\":4,"
            "\"clause_stats\":["
            "{\"label\":\"MetadataEquals(api_key=sk-secret-1)\",\"evaluated\":10,\"matched\":4},"
            "{\"label\":\"Exists(hardware.gpu)\",\"evaluated\":10,\"matched\":8}"
            "]}";
        const char* keys = "[\"api_key\"]";
        char* redacted = NULL;
        size_t redacted_len = 0;
        int rc = net_predicate_redact_metadata_keys(report, keys, &redacted, &redacted_len);
        if (rc != 0) {
            fprintf(stderr, "redact_metadata_keys returned %d\n", rc);
            return 1;
        }
        print_and_free("[redact] result", redacted);
    }

    /* =====================================================
     * 6. Predicate-to-where-header — encode a predicate as
     *    the canonical (name, value) request-header pair.
     *    The pair drops into net_rpc_call_with_headers (in
     *    net_rpc.h) for end-to-end Phase 9b filtering:
     *
     *      net_rpc_header_t hdrs[1] = {{
     *          name, name_len, (uint8_t*)value, value_len }};
     *      net_rpc_call_with_headers(rpc, target, svc, svc_len,
     *          req, req_len, deadline_ms, 0,
     *          hdrs, 1,
     *          &resp, &resp_len, &err);
     * ===================================================== */
    {
        const char* pred =
            "{\"nodes\":["
            "{\"kind\":\"exists\",\"key\":{\"axis\":\"hardware\",\"key\":\"gpu\"}}"
            "],\"root_idx\":0}";
        char* name = NULL;
        size_t name_len = 0;
        char* value = NULL;
        size_t value_len = 0;
        int rc = net_predicate_to_where_header(
            pred, &name, &name_len, &value, &value_len);
        if (rc != 0) {
            fprintf(stderr, "to_where_header returned %d\n", rc);
            return 1;
        }
        printf("[where] name=%.*s value_len=%zu\n",
               (int)name_len, name, value_len);
        printf("[where] value=%.*s\n", (int)value_len, value);
        net_free_string(name);
        net_free_string(value);
    }

    return 0;
}
