/*
 * mcp.c — the MCP bridge helpers and the consent / pin surface, from C
 * (net_mcp.h), against the cross-language golden vectors.
 *
 * mcp_cases.h, generated from net/crates/net/tests/cross_lang_mcp/ by
 * .github/scripts/gen-c-fixture-cases.py, carries every case the other
 * bindings run:
 *
 *   helper_vectors.json   net_mcp_classify, net_mcp_lower_tool
 *   consent_vectors.json  net_mcp_cap_id_canonicalize (valid and invalid),
 *                         net_mcp_credential_requires_consent, and
 *                         ConsentPolicy decisions after allow / pin / unpin
 *
 * A lowered tool is checked for its tool_id, mcp_name and every
 * bridge-metadata entry; the descriptor's own rendering is the Rust tests'
 * concern. Then the pin store, path-scoped, in a scratch file: request is
 * "pending", approve changes it once, a second request reports "approved",
 * the list holds the record, and a corrupt store file is refused.
 *
 * net_mcp.h's memory model: every char* is freed with net_mcp_free_string,
 * the policy with net_mcp_consent_policy_free. Its error model: NULL / -1,
 * with the reason in net_mcp_last_error_message.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net_mcp.h"

#include "consumer_util.h"
#include "loaded_module.h"
#include "mcp_cases.h"

static char label[512];

static const char* named(const char* what, const char* name) {
    snprintf(label, sizeof label, "%s: %s", what, name);
    return label;
}

/* 1 if every newline-separated needle in `needles` occurs in `hay`. */
static int contains_all(const char* hay, const char* needles) {
    char buf[2048];
    char* line;
    char* next;
    snprintf(buf, sizeof buf, "%s", needles);
    for (line = buf; line && *line; line = next) {
        next = strchr(line, '\n');
        if (next) {
            *next++ = '\0';
        }
        if (!strstr(hay, line)) {
            return 0;
        }
    }
    return 1;
}

static int helper_cases(void) {
    size_t i;
    for (i = 0; i < CU_CLASSIFY_CASES_N; i++) {
        const cu_classify_case* c = &cu_classify_cases[i];
        char* got = net_mcp_classify(c->program, c->args, c->envs, c->override_, c->force);
        CU_CHECK(named("net_mcp_classify", c->name), got != NULL && strcmp(got, c->expected) == 0);
        net_mcp_free_string(got);
    }
    {
        /* helper_vectors.json omits it; net_mcp.h: "no-credentials" is a
         * downward override, which needs `force`. */
        char* got = net_mcp_classify("uvx", "[\"t\"]", "{}", "no-credentials", 0);
        CU_CHECK("net_mcp_classify: a downward override without force is refused", got == NULL);
        CU_CHECK("net_mcp_classify: the refusal sets the last error",
                 net_mcp_last_error_message() != NULL);
    }
    for (i = 0; i < CU_LOWER_CASES_N; i++) {
        const cu_lower_case* c = &cu_lower_cases[i];
        char want[256];
        char* dto = net_mcp_lower_tool(c->tool, c->server_version, c->credential_status,
                                       c->substitutability);
        CU_CHECK(named("net_mcp_lower_tool", c->name), dto != NULL);
        snprintf(want, sizeof want, "\"tool_id\":\"%s\"", c->tool_id);
        CU_CHECK(named("net_mcp_lower_tool: tool_id", c->name), strstr(dto, want) != NULL);
        snprintf(want, sizeof want, "\"mcp_name\":\"%s\"", c->mcp_name);
        CU_CHECK(named("net_mcp_lower_tool: mcp_name", c->name), strstr(dto, want) != NULL);
        CU_CHECK(named("net_mcp_lower_tool: every bridge-metadata entry", c->name),
                 contains_all(dto, c->bridge_metadata));
        net_mcp_free_string(dto);
    }
    CU_CHECK("net_mcp_lower_tool: an unknown credential status is refused",
             net_mcp_lower_tool(cu_lower_cases[0].tool, "1.0.0", "totally-fine-trust-me", NULL) == NULL);
    CU_CHECK("net_mcp_lower_tool: an unknown substitutability is refused",
             net_mcp_lower_tool(cu_lower_cases[0].tool, "1.0.0", "none", "anything") == NULL);
    net_mcp_free_string(NULL);
    CU_SURVIVED("net_mcp_free_string: NULL is a no-op");
    return 0;
}

static int consent_cases(void) {
    size_t i;
    for (i = 0; i < CU_CANON_CASES_N; i++) {
        const cu_canon_case* c = &cu_canon_cases[i];
        char* got = net_mcp_cap_id_canonicalize(c->input);
        CU_CHECK(named("net_mcp_cap_id_canonicalize", c->name),
                 got != NULL && strcmp(got, c->expected) == 0);
        net_mcp_free_string(got);
    }
    for (i = 0; i < CU_INVALID_CASES_N; i++) {
        const cu_invalid_case* c = &cu_invalid_cases[i];
        CU_CHECK(named("net_mcp_cap_id_canonicalize: refused", c->name),
                 net_mcp_cap_id_canonicalize(c->input) == NULL);
        CU_CHECK(named("net_mcp_cap_id_canonicalize: the refusal sets the last error", c->name),
                 net_mcp_last_error_message() != NULL);
    }
    for (i = 0; i < CU_REQUIRES_CASES_N; i++) {
        const cu_requires_case* c = &cu_requires_cases[i];
        CU_CHECK_RC(named("net_mcp_credential_requires_consent", c->name),
                    net_mcp_credential_requires_consent(c->status), c->expected);
    }
    /* net_mcp.h: "none" / NULL / anything unrecognised returns 1. */
    CU_CHECK_RC("net_mcp_credential_requires_consent: NULL gates",
                net_mcp_credential_requires_consent(NULL), 1);

    for (i = 0; i < CU_DECISION_CASES_N; i++) {
        const cu_decision_case* c = &cu_decision_cases[i];
        ConsentPolicyHandle* policy = net_mcp_consent_policy_new();
        char ops[512];
        char* op;
        char* next;
        char* decision;
        CU_CHECK(named("net_mcp_consent_policy_new", c->name), policy != NULL);
        snprintf(ops, sizeof ops, "%s", c->ops);
        for (op = ops; *op; op = next) {
            char* cap;
            int rc = -1;
            next = strchr(op, ';');
            if (next) {
                *next++ = '\0';
            } else {
                next = op + strlen(op);
            }
            cap = strchr(op, ' ');
            if (cap) {
                *cap++ = '\0';
                if (strcmp(op, "allow") == 0) {
                    rc = net_mcp_consent_policy_allow(policy, cap);
                } else if (strcmp(op, "pin") == 0) {
                    rc = net_mcp_consent_policy_pin(policy, cap);
                } else if (strcmp(op, "unpin") == 0) {
                    rc = net_mcp_consent_policy_unpin(policy, cap);
                }
            }
            CU_CHECK_RC(named("ConsentPolicy op", c->name), rc, 0);
        }
        decision = net_mcp_consent_policy_decide(policy, c->cap_id, c->credential_status);
        CU_CHECK(named("net_mcp_consent_policy_decide", c->name),
                 decision != NULL && strcmp(decision, c->expected) == 0);
        net_mcp_free_string(decision);
        net_mcp_consent_policy_free(policy);
    }
    {
        ConsentPolicyHandle* policy = net_mcp_consent_policy_new();
        char* pinned;
        CU_CHECK_RC("net_mcp_consent_policy_pin: 0x2a/echo", net_mcp_consent_policy_pin(policy, "0x2a/echo"), 0);
        CU_CHECK_RC("net_mcp_consent_policy_is_pinned: the decimal form is the same id",
                    net_mcp_consent_policy_is_pinned(policy, "42/echo"), 1);
        CU_CHECK_RC("net_mcp_consent_policy_is_pinned: another id is not",
                    net_mcp_consent_policy_is_pinned(policy, "42/other"), 0);
        pinned = net_mcp_consent_policy_pinned(policy);
        CU_CHECK("net_mcp_consent_policy_pinned: the canonical id, as a JSON array",
                 pinned != NULL && strcmp(pinned, "[\"42/echo\"]") == 0);
        net_mcp_free_string(pinned);
        CU_CHECK_RC("net_mcp_consent_policy_pin: an id without a slash is -1",
                    net_mcp_consent_policy_pin(policy, "no-slash"), -1);
        CU_CHECK("net_mcp_consent_policy_decide: an invalid id is NULL",
                 net_mcp_consent_policy_decide(policy, "no-slash", "credentialed") == NULL);
        net_mcp_consent_policy_free(policy);
        net_mcp_consent_policy_free(NULL);
        CU_SURVIVED("net_mcp_consent_policy_free: NULL is a no-op");
    }
    return 0;
}

static int pin_store(void) {
    char dir[256], path[320];
    char* state;
    char* list;
    snprintf(dir, sizeof dir, "net-c-mcp-%lu", cu_pid());
    CU_CHECK("scratch directory", cu_mkdir(dir) == 0);
    snprintf(path, sizeof path, "%s/pins.json", dir);

    state = net_mcp_pin_state(path, "b/echo");
    CU_CHECK("net_mcp_pin_state: no record is the empty string", state != NULL && state[0] == '\0');
    net_mcp_free_string(state);

    state = net_mcp_pin_request(path, "b/echo");
    CU_CHECK("net_mcp_pin_request: a new request is pending", state != NULL && strcmp(state, "pending") == 0);
    net_mcp_free_string(state);
    CU_CHECK_RC("net_mcp_pin_is_approved: pending is not approved", net_mcp_pin_is_approved(path, "b/echo"), 0);
    CU_CHECK_RC("net_mcp_pin_approve: changes the store", net_mcp_pin_approve(path, "b/echo"), 1);
    CU_CHECK_RC("net_mcp_pin_approve: again changes nothing", net_mcp_pin_approve(path, "b/echo"), 0);
    CU_CHECK_RC("net_mcp_pin_is_approved: now approved", net_mcp_pin_is_approved(path, "b/echo"), 1);

    state = net_mcp_pin_request(path, "b/echo");
    CU_CHECK("net_mcp_pin_request: leaves an approved record approved",
             state != NULL && strcmp(state, "approved") == 0);
    net_mcp_free_string(state);
    state = net_mcp_pin_state(path, "b/echo");
    CU_CHECK("net_mcp_pin_state: approved", state != NULL && strcmp(state, "approved") == 0);
    net_mcp_free_string(state);

    list = net_mcp_pin_list(path);
    CU_CHECK("net_mcp_pin_list: the one approved record",
             list != NULL && strstr(list, "\"cap_id\":\"b/echo\"") != NULL &&
                 strstr(list, "\"state\":\"approved\"") != NULL);
    net_mcp_free_string(list);

    CU_CHECK("corrupt the store file", cu_write_file(path, "{ not valid json", 16) == 0);
    CU_CHECK("net_mcp_pin_list: a corrupt store is refused", net_mcp_pin_list(path) == NULL);
    CU_CHECK_RC("net_mcp_pin_approve: a corrupt store is refused", net_mcp_pin_approve(path, "b/echo"), -1);
    CU_CHECK("the refusal sets the last error", net_mcp_last_error_message() != NULL);
    net_mcp_clear_last_error();
    CU_CHECK("net_mcp_clear_last_error: clears it", net_mcp_last_error_message() == NULL);
    remove(path);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_mcp_classify),
        CLM_FN(net_mcp_lower_tool),
        CLM_FN(net_mcp_cap_id_canonicalize),
        CLM_FN(net_mcp_credential_requires_consent),
        CLM_FN(net_mcp_consent_policy_new),
        CLM_FN(net_mcp_consent_policy_decide),
        CLM_FN(net_mcp_pin_request),
        CLM_FN(net_mcp_pin_list),
        CLM_FN(net_mcp_last_error_message),
        CLM_FN(net_mcp_free_string),
        CLM_FN(net_mcp_clear_last_error),
        CLM_FN(net_mcp_consent_policy_allow),
        CLM_FN(net_mcp_consent_policy_free),
        CLM_FN(net_mcp_consent_policy_is_pinned),
        CLM_FN(net_mcp_consent_policy_pin),
        CLM_FN(net_mcp_consent_policy_pinned),
        CLM_FN(net_mcp_consent_policy_unpin),
        CLM_FN(net_mcp_pin_approve),
        CLM_FN(net_mcp_pin_is_approved),
        CLM_FN(net_mcp_pin_state),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (helper_cases() || consent_cases() || pin_store()) {
        return 1;
    }
    return cu_finish();
}
