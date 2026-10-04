/*
 * capabilities.c — the stateless capability and predicate helpers, from C,
 * against the cross-language golden vectors.
 *
 * Every binding tests these helpers against the same fixtures in
 * net/crates/net/tests/cross_lang_capability/. This program runs every case
 * of six of them through the C ABI (net.go.h), from capability_cases.h,
 * which .github/scripts/gen-c-fixture-cases.py generates from the fixtures:
 *
 *   predicate_eval.json                   net_predicate_evaluate
 *   capability_validation.json            net_validate_capabilities
 *   predicate_trace.json                  net_predicate_evaluate_with_trace
 *   predicate_debug_report.json           net_predicate_aggregate_debug_report
 *   predicate_debug_report_redacted.json  net_predicate_redact_metadata_keys
 *   predicate_nrpc_envelope.json          net_predicate_to_where_header
 *
 * Then the refusals the header documents for each: NULL inputs are
 * NET_ERR_NULL_POINTER and unparseable JSON is NET_ERR_INVALID_JSON.
 * Every string the library returns is freed with net_free_string.
 *
 * Validation reports are compared by count and kind, per list: the
 * fixture's entries carry fields (paths, sizes) whose rendering is the
 * Rust tests' concern; the kinds and their counts are the contract.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net.go.h"

#include "capability_cases.h"
#include "consumer_util.h"
#include "loaded_module.h"

static char label[512];

static const char* named(const char* what, const char* name) {
    snprintf(label, sizeof label, "%s: %s", what, name);
    return label;
}

/* Occurrences of `"kind":` in [begin, end). */
static int count_kinds(const char* begin, const char* end) {
    int n = 0;
    const char* p = begin;
    while ((p = strstr(p, "\"kind\":")) != NULL && p < end) {
        n++;
        p += 7;
    }
    return n;
}

/* 1 if every comma-separated kind in `kinds` appears as "kind":"<k>"
 * in [begin, end). */
static int has_kinds(const char* begin, const char* end, const char* kinds) {
    char buf[512], needle[544];
    char* save = NULL;
    char* k;
    size_t len = (size_t)(end - begin);
    char* seg = (char*)malloc(len + 1);
    int ok = 1;
    if (!seg) {
        return 0;
    }
    memcpy(seg, begin, len);
    seg[len] = '\0';
    snprintf(buf, sizeof buf, "%s", kinds);
    for (k = buf; *k; k = save) {
        save = strchr(k, ',');
        if (save) {
            *save++ = '\0';
        } else {
            save = k + strlen(k);
        }
        snprintf(needle, sizeof needle, "\"kind\":\"%s\"", k);
        if (!strstr(seg, needle)) {
            ok = 0;
        }
    }
    free(seg);
    return ok;
}

static int eval_cases(void) {
    size_t i;
    for (i = 0; i < CU_EVAL_CASES_N; i++) {
        const cu_eval_case* c = &cu_eval_cases[i];
        CU_CHECK_RC(named("net_predicate_evaluate", c->name),
                    net_predicate_evaluate(c->wire, c->tags, c->metadata), c->expected);
    }
    CU_CHECK_RC("net_predicate_evaluate: NULL predicate is NET_ERR_NULL_POINTER",
                net_predicate_evaluate(NULL, "[]", "{}"), NET_ERR_NULL_POINTER);
    CU_CHECK_RC("net_predicate_evaluate: unparseable predicate is NET_ERR_INVALID_JSON",
                net_predicate_evaluate("{not json", "[]", "{}"), NET_ERR_INVALID_JSON);
    CU_CHECK_RC("net_predicate_evaluate: a malformed tag is NET_ERR_INVALID_JSON",
                net_predicate_evaluate(cu_eval_cases[0].wire, "[42]", "{}"), NET_ERR_INVALID_JSON);
    return 0;
}

static int validation_cases(void) {
    size_t i;
    for (i = 0; i < CU_VALIDATION_CASES_N; i++) {
        const cu_validation_case* c = &cu_validation_cases[i];
        char* report = NULL;
        size_t len = 0;
        const char *errors, *warnings, *end;
        CU_CHECK_RC(named("net_validate_capabilities", c->name),
                    net_validate_capabilities(c->caps, &report, &len), 0);
        CU_CHECK(named("net_validate_capabilities: report length", c->name),
                 report != NULL && len == strlen(report));
        errors = strstr(report, "\"errors\":");
        warnings = strstr(report, "\"warnings\":");
        CU_CHECK(named("net_validate_capabilities: errors and warnings lists", c->name),
                 errors != NULL && warnings != NULL);
        /* Each list runs to the other key or to the end of the report. */
        end = report + len;
        if (errors < warnings) {
            CU_CHECK_RC(named("net_validate_capabilities: error count", c->name),
                        count_kinds(errors, warnings), c->n_errors);
            CU_CHECK_RC(named("net_validate_capabilities: warning count", c->name),
                        count_kinds(warnings, end), c->n_warnings);
            CU_CHECK(named("net_validate_capabilities: error kinds", c->name),
                     has_kinds(errors, warnings, c->error_kinds));
            CU_CHECK(named("net_validate_capabilities: warning kinds", c->name),
                     has_kinds(warnings, end, c->warning_kinds));
        } else {
            CU_CHECK_RC(named("net_validate_capabilities: error count", c->name),
                        count_kinds(errors, end), c->n_errors);
            CU_CHECK_RC(named("net_validate_capabilities: warning count", c->name),
                        count_kinds(warnings, errors), c->n_warnings);
            CU_CHECK(named("net_validate_capabilities: error kinds", c->name),
                     has_kinds(errors, end, c->error_kinds));
            CU_CHECK(named("net_validate_capabilities: warning kinds", c->name),
                     has_kinds(warnings, errors, c->warning_kinds));
        }
        net_free_string(report);
    }
    {
        char* report = NULL;
        size_t len = 0;
        CU_CHECK_RC("net_validate_capabilities: NULL output is NET_ERR_NULL_POINTER",
                    net_validate_capabilities("{\"tags\":[],\"metadata\":{}}", NULL, &len),
                    NET_ERR_NULL_POINTER);
        CU_CHECK_RC("net_validate_capabilities: unparseable caps is NET_ERR_INVALID_JSON",
                    net_validate_capabilities("[1,2", &report, &len), NET_ERR_INVALID_JSON);
    }
    return 0;
}

static int trace_cases(void) {
    size_t i;
    for (i = 0; i < CU_TRACE_CASES_N; i++) {
        const cu_trace_case* c = &cu_trace_cases[i];
        char* trace = NULL;
        size_t len = 0;
        int result = -1;
        CU_CHECK_RC(named("net_predicate_evaluate_with_trace", c->name),
                    net_predicate_evaluate_with_trace(c->wire, c->tags, c->metadata, &result,
                                                      &trace, &len),
                    0);
        CU_CHECK_RC(named("net_predicate_evaluate_with_trace: result", c->name), result,
                    c->expected_result);
        CU_CHECK(named("net_predicate_evaluate_with_trace: the fixture's trace", c->name),
                 trace != NULL && len == strlen(trace) && strcmp(trace, c->expected_trace) == 0);
        net_free_string(trace);
    }
    return 0;
}

static int report_cases(void) {
    size_t i;
    char want[4096];
    for (i = 0; i < CU_REPORT_CASES_N; i++) {
        const cu_report_case* c = &cu_report_cases[i];
        char* report = NULL;
        size_t len = 0;
        uint64_t total = 0, matched = 0;
        const char* top;
        CU_CHECK_RC(named("net_predicate_aggregate_debug_report", c->name),
                    net_predicate_aggregate_debug_report(c->wire, c->contexts, &report, &len), 0);
        /* Each clause stat has its own "matched"; the report's own follows
         * the clause_stats array (the object's keys come back sorted). */
        top = report ? strstr(report, "],\"matched\":") : NULL;
        CU_CHECK(named("net_predicate_aggregate_debug_report: total and matched", c->name),
                 top != NULL && cu_json_u64(report, "total_candidates", &total) == 0 &&
                     cu_json_u64(top + 1, "matched", &matched) == 0 && total == c->total &&
                     matched == c->matched);
        snprintf(want, sizeof want, "\"clause_stats\":%s", c->clause_stats);
        CU_CHECK(named("net_predicate_aggregate_debug_report: the fixture's clause stats", c->name),
                 strstr(report, want) != NULL);
        net_free_string(report);
    }
    return 0;
}

static int redact_cases(void) {
    size_t i;
    for (i = 0; i < CU_REDACT_CASES_N; i++) {
        const cu_redact_case* c = &cu_redact_cases[i];
        char *out = NULL, *again = NULL;
        size_t len = 0, again_len = 0;
        CU_CHECK_RC(named("net_predicate_redact_metadata_keys", c->name),
                    net_predicate_redact_metadata_keys(c->report, c->keys, &out, &len), 0);
        CU_CHECK(named("net_predicate_redact_metadata_keys: the fixture's redacted report", c->name),
                 out != NULL && strcmp(out, c->redacted) == 0);
        /* net.go.h: "Idempotent: redact(redact(r,k),k) == redact(r,k)." */
        CU_CHECK_RC(named("net_predicate_redact_metadata_keys: again", c->name),
                    net_predicate_redact_metadata_keys(out, c->keys, &again, &again_len), 0);
        CU_CHECK(named("net_predicate_redact_metadata_keys: idempotent", c->name),
                 again != NULL && strcmp(again, out) == 0);
        net_free_string(out);
        net_free_string(again);
    }
    return 0;
}

static int envelope_cases(void) {
    size_t i;
    for (i = 0; i < CU_ENVELOPE_CASES_N; i++) {
        const cu_envelope_case* c = &cu_envelope_cases[i];
        char *name = NULL, *value = NULL;
        size_t name_len = 0, value_len = 0;
        CU_CHECK_RC(named("net_predicate_to_where_header", c->name),
                    net_predicate_to_where_header(c->wire, &name, &name_len, &value, &value_len), 0);
        CU_CHECK(named("net_predicate_to_where_header: the name is net-where", c->name),
                 name != NULL && name_len == 9 && strcmp(name, "net-where") == 0);
        CU_CHECK(named("net_predicate_to_where_header: the value is the wire predicate", c->name),
                 value != NULL && value_len == strlen(value) && strcmp(value, c->wire) == 0);
        net_free_string(name);
        net_free_string(value);
    }
    {
        char *name = NULL, *value = NULL;
        size_t name_len = 0, value_len = 0;
        CU_CHECK_RC("net_predicate_to_where_header: NULL output is NET_ERR_NULL_POINTER",
                    net_predicate_to_where_header(cu_envelope_cases[0].wire, NULL, &name_len,
                                                  &value, &value_len),
                    NET_ERR_NULL_POINTER);
        CU_CHECK_RC("net_predicate_to_where_header: unparseable predicate is NET_ERR_INVALID_JSON",
                    net_predicate_to_where_header("{", &name, &name_len, &value, &value_len),
                    NET_ERR_INVALID_JSON);
    }
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_predicate_evaluate),
        CLM_FN(net_validate_capabilities),
        CLM_FN(net_predicate_evaluate_with_trace),
        CLM_FN(net_predicate_aggregate_debug_report),
        CLM_FN(net_predicate_redact_metadata_keys),
        CLM_FN(net_predicate_to_where_header),
        CLM_FN(net_free_string),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (eval_cases() || validation_cases() || trace_cases() || report_cases() || redact_cases() ||
        envelope_cases()) {
        return 1;
    }
    return cu_finish();
}
