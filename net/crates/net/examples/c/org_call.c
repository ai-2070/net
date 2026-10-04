/*
 * org_call.c — a protected organization call, unary, served and called
 * from C (net_org.h).
 *
 * NET-NEEDS: org-scenario
 *
 * Credentials are issued material: this program mints none. The runner
 * generates a throwaway cross-org scenario with the in-repo generator
 * (gen_org_scenario, the manifest every binding's live test loads) and
 * names its directory in NET_ORG_SCENARIO. Org B's provider serves a
 * Granted capability; org A's caller holds the grant.
 *
 * Checked against net_org.h:
 *   - the ABI stamp matches the header (exact equality);
 *   - provisioning: both nodes install their authority, the provider its
 *     grant audience (bytes, and the audience secret as a PATH);
 *   - the caller binds credentials (bytes in, secret path) — the
 *     credentials pointer is consumed and NULLed;
 *   - the registered-release contract: the dispatcher is refused before
 *     net_org_set_callback_free, and the handler's buffers come back
 *     through it;
 *   - a unary net_org_call reaches the handler and returns its response;
 *     the handler sees the provider-verified admission facts, which match
 *     the manifest: acting for the caller's org, served by the provider's;
 *   - an application error the handler returns reaches the caller as
 *     NET_ORG_ERR_RPC, with its `org:` wire;
 *   - an unserved service is NET_ORG_ERR_DISCOVERY, at once;
 *   - cancel tokens, and every free on NULL / a pointer to NULL.
 *
 * Discovery: the scoped announcements ride the announce path, so the
 * first call retries ONLY the local `org:discovery:no_authorized_provider`
 * refusal (nothing was sent), bounded — the same precondition the other
 * bindings' live tests use.
 */

#define _CRT_SECURE_NO_WARNINGS /* getenv, on MSVC */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_org.h"

#include "consumer_util.h"
#include "loaded_module.h"

static cu_mutex* lock;
static net_org_caller_t seen;
static int handled, frees;

static void callback_free(void* p) {
    cu_mutex_lock(lock);
    frees++;
    cu_mutex_unlock(lock);
    free(p);
}

static int handler(uint64_t handler_id, const net_org_caller_t* caller, const uint8_t* req, size_t req_len,
                   uint8_t** out_resp, size_t* out_resp_len, char** out_err) {
    (void)handler_id;
    cu_mutex_lock(lock);
    seen = *caller;
    handled++;
    cu_mutex_unlock(lock);
    if (req_len == 4 && memcmp(req, "fail", 4) == 0) {
        static const char msg[] = "nrpc:app_error:0x8001:refused by the C handler";
        *out_err = (char*)malloc(sizeof msg);
        if (*out_err) {
            memcpy(*out_err, msg, sizeof msg);
        }
        return -1;
    }
    *out_resp = (uint8_t*)malloc(req_len + 5);
    if (!*out_resp) {
        return -1;
    }
    memcpy(*out_resp, "echo:", 5);
    memcpy(*out_resp + 5, req, req_len);
    *out_resp_len = req_len + 5;
    return 0;
}

static int nibble(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int hex32(const char* hex, uint8_t* out) {
    int i;
    for (i = 0; i < 32; i++) {
        int hi = nibble(hex[2 * i]), lo = hi < 0 ? -1 : nibble(hex[2 * i + 1]);
        if (lo < 0) {
            return -1;
        }
        out[i] = (uint8_t)(hi << 4 | lo);
    }
    return 0;
}

static int build(const char* psk, const char* seed, net_meshnode_t** out, char* addr, size_t addr_len) {
    char cfg[512];
    int attempt;
    for (attempt = 0; attempt < 8; attempt++) {
        if (cu_reserve_port(addr, addr_len) != 0) {
            continue;
        }
        snprintf(cfg, sizeof cfg, "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\",\"identity_seed_hex\":\"%s\",\"heartbeat_ms\":200}",
                 addr, psk, seed);
        if (net_mesh_new(cfg, out) == 0) {
            return 0;
        }
    }
    return -1;
}

static unsigned char* scenario_file(const char* dir, const char* rel, size_t* len) {
    char path[1024];
    snprintf(path, sizeof path, "%s/%s", dir, rel);
    return cu_read_file(path, len);
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_mesh_arc_clone),
        CLM_FN(net_org_check_abi_version),
        CLM_FN(net_org_install_authority),
        CLM_FN(net_org_install_provider_grant_audience),
        CLM_FN(net_org_credentials_new),
        CLM_FN(net_org_bind),
        CLM_FN(net_org_set_callback_free),
        CLM_FN(net_org_set_handler_dispatcher),
        CLM_FN(net_org_serve),
        CLM_FN(net_org_call),
        CLM_FN(net_org_response_free),
        CLM_FN(net_org_free_cstring),
        CLM_FN(net_org_client_free),
        CLM_FN(net_org_credentials_free),
        CLM_FN(net_org_reserve_cancel_token),
        CLM_FN(net_org_reserve_handler_id),
        CLM_FN(net_org_serve_handle_free),
    };
    const char* dir = getenv("NET_ORG_SCENARIO");
    char psk[80], service[128], p_seed[80], p_org[80], p_auth[256], p_grant[256], p_secret[256];
    char c_seed[80], c_org[80], c_auth[256], c_mem[256], c_disp[256], c_grant[256], c_secret[256];
    char path_a[1024], path_b[1024], secret_full[1024], prov_addr[32], call_addr[32];
    unsigned char* manifest;
    const char *prov_block, *call_block;
    uint8_t provider_org[32], caller_org[32];
    size_t len = 0;
    net_meshnode_t *provider = NULL, *caller = NULL;
    NetOrgCredentials* creds = NULL;
    NetOrgClient* client = NULL;
    NetOrgServeHandle* serve = NULL;
    uint8_t* resp = NULL;
    size_t resp_len = 0;
    char* err = NULL;
    uint64_t handler_id;
    int rc, attempt, released;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    CU_CHECK("NET_ORG_SCENARIO names the generated scenario", dir != NULL);
    CU_CHECK_RC("net_org_check_abi_version: the header's", net_org_check_abi_version(NET_ORG_ABI_VERSION), NET_ORG_OK);
    CU_CHECK("net_org_check_abi_version: anything else is refused",
             net_org_check_abi_version(NET_ORG_ABI_VERSION + 1) != NET_ORG_OK);
    if (cu_net_init() != 0) {
        printf("FAIL setup: cu_net_init\n");
        return 1;
    }
    lock = cu_mutex_new();
    CU_CHECK("a mutex for the handler's facts", lock != NULL);

    manifest = scenario_file(dir, "manifest.json", &len);
    CU_CHECK("the manifest", manifest != NULL);
    prov_block = strstr((const char*)manifest, "\"provider\"");
    call_block = strstr((const char*)manifest, "\"caller\"");
    CU_CHECK("the manifest's provider and caller blocks", prov_block && call_block && prov_block < call_block);
    CU_CHECK("manifest fields",
             cu_json_str((const char*)manifest, NULL, "psk_hex", psk, sizeof psk) == 0 &&
                 cu_json_str((const char*)manifest, NULL, "granted_service", service, sizeof service) == 0 &&
                 cu_json_str(prov_block, call_block, "seed_hex", p_seed, sizeof p_seed) == 0 &&
                 cu_json_str(prov_block, call_block, "org_id_hex", p_org, sizeof p_org) == 0 &&
                 cu_json_str(prov_block, call_block, "authority_dir", p_auth, sizeof p_auth) == 0 &&
                 cu_json_str(prov_block, call_block, "grant_path", p_grant, sizeof p_grant) == 0 &&
                 cu_json_str(prov_block, call_block, "grant_secret_path", p_secret, sizeof p_secret) == 0 &&
                 cu_json_str(call_block, NULL, "seed_hex", c_seed, sizeof c_seed) == 0 &&
                 cu_json_str(call_block, NULL, "org_id_hex", c_org, sizeof c_org) == 0 &&
                 cu_json_str(call_block, NULL, "authority_dir", c_auth, sizeof c_auth) == 0 &&
                 cu_json_str(call_block, NULL, "membership_path", c_mem, sizeof c_mem) == 0 &&
                 cu_json_str(call_block, NULL, "dispatcher_path", c_disp, sizeof c_disp) == 0 &&
                 cu_json_str(call_block, NULL, "grant_path", c_grant, sizeof c_grant) == 0 &&
                 cu_json_str(call_block, NULL, "grant_secret_path", c_secret, sizeof c_secret) == 0 &&
                 hex32(p_org, provider_org) == 0 && hex32(c_org, caller_org) == 0);
    free(manifest);

    CU_CHECK_RC("bring-up: provider", build(psk, p_seed, &provider, prov_addr, sizeof prov_addr), 0);
    CU_CHECK_RC("bring-up: caller", build(psk, c_seed, &caller, call_addr, sizeof call_addr), 0);

    snprintf(path_a, sizeof path_a, "%s/%s", dir, p_auth);
    rc = net_org_install_authority(net_mesh_arc_clone(provider), path_a, strlen(path_a), &err);
    CU_CHECK_RC("net_org_install_authority: provider", rc, NET_ORG_OK);
    snprintf(path_b, sizeof path_b, "%s/%s", dir, c_auth);
    rc = net_org_install_authority(net_mesh_arc_clone(caller), path_b, strlen(path_b), &err);
    CU_CHECK_RC("net_org_install_authority: caller", rc, NET_ORG_OK);
    {
        unsigned char* grant = scenario_file(dir, p_grant, &len);
        snprintf(secret_full, sizeof secret_full, "%s/%s", dir, p_secret);
        rc = net_org_install_provider_grant_audience(net_mesh_arc_clone(provider), grant, len, secret_full,
                                                     strlen(secret_full), &err);
        free(grant);
        CU_CHECK_RC("net_org_install_provider_grant_audience (bytes + secret PATH)", rc, NET_ORG_OK);
    }
    {
        size_t mem_len = 0, disp_len = 0, grant_len = 0;
        unsigned char* mem = scenario_file(dir, c_mem, &mem_len);
        unsigned char* disp = scenario_file(dir, c_disp, &disp_len);
        unsigned char* grant = scenario_file(dir, c_grant, &grant_len);
        const uint8_t* grants[1];
        size_t grant_lens[1];
        const char* secrets[1];
        grants[0] = grant;
        grant_lens[0] = grant_len;
        snprintf(secret_full, sizeof secret_full, "%s/%s", dir, c_secret);
        secrets[0] = secret_full;
        rc = net_org_credentials_new(mem, mem_len, disp, disp_len, grants, grant_lens, 1, secrets, 1, &creds, &err);
        free(mem);
        free(disp);
        free(grant);
        CU_CHECK_RC("net_org_credentials_new", rc, NET_ORG_OK);
    }
    rc = net_org_bind(net_mesh_arc_clone(caller), &creds, &client, &err);
    CU_CHECK_RC("net_org_bind", rc, NET_ORG_OK);
    CU_CHECK("net_org_bind: consumed the credentials (NULLed)", creds == NULL && client != NULL);

    CU_CHECK_RC("bring-up: handshake", cu_mesh_handshake(provider, caller, prov_addr), 0);
    CU_CHECK_RC("bring-up: start caller", net_mesh_start(caller), 0);
    CU_CHECK_RC("bring-up: start provider", net_mesh_start(provider), 0);

    CU_CHECK("net_org_set_handler_dispatcher: refused before the deallocator",
             net_org_set_handler_dispatcher(handler) != NET_ORG_OK);
    CU_CHECK_RC("net_org_set_callback_free", net_org_set_callback_free(callback_free), NET_ORG_OK);
    CU_CHECK_RC("net_org_set_handler_dispatcher", net_org_set_handler_dispatcher(handler), NET_ORG_OK);
    handler_id = net_org_reserve_handler_id();
    rc = net_org_serve(net_mesh_arc_clone(provider), service, strlen(service), NET_ORG_ACCESS_GRANTED, handler_id,
                       &serve, &err);
    CU_CHECK_RC("net_org_serve: Granted", rc, NET_ORG_OK);

    for (attempt = 0;; attempt++) {
        err = NULL;
        rc = net_org_call(client, service, strlen(service), (const uint8_t*)"hello", 5, 10000, 0, &resp, &resp_len,
                          &err);
        if (rc == NET_ORG_OK || !(rc == NET_ORG_ERR_DISCOVERY && err &&
                                  strncmp(err, "org:discovery:no_authorized_provider", 36) == 0) ||
            attempt >= 120) {
            break;
        }
        net_org_free_cstring(err);
        cu_sleep_ms(500);
    }
    if (rc != NET_ORG_OK) {
        printf("  (call: rc %d, err %s)\n", rc, err ? err : "(null)");
    }
    CU_CHECK_RC("net_org_call: unary, cross-org, Granted", rc, NET_ORG_OK);
    CU_CHECK("net_org_call: the handler's response", resp_len == 10 && memcmp(resp, "echo:hello", 10) == 0);
    net_org_response_free(resp, resp_len);
    cu_mutex_lock(lock);
    CU_CHECK("handler: ran once", handled == 1);
    CU_CHECK("handler: acting for the caller's organization (the manifest's)",
             memcmp(seen.acting_org, caller_org, 32) == 0);
    CU_CHECK("handler: served by the provider's organization (the manifest's)",
             memcmp(seen.provider_org, provider_org, 32) == 0);
    CU_CHECK("handler: cross-org, so the two differ", memcmp(seen.acting_org, seen.provider_org, 32) != 0);
    released = frees;
    cu_mutex_unlock(lock);
    CU_CHECK("the response buffer came back through the registered free", released >= 1);

    err = NULL;
    resp = NULL;
    rc = net_org_call(client, service, strlen(service), (const uint8_t*)"fail", 4, 10000, 0, &resp, &resp_len, &err);
    if (rc == NET_ORG_OK || err == NULL) {
        printf("  (fail call: rc %d, err %s)\n", rc, err ? err : "(null)");
    }
    CU_CHECK_RC("net_org_call: the handler's application error is NET_ORG_ERR_RPC", rc, NET_ORG_ERR_RPC);
    CU_CHECK("application error: the org: wire", err != NULL && strncmp(err, "org:", 4) == 0);
    net_org_free_cstring(err);
    cu_mutex_lock(lock);
    released = frees;
    cu_mutex_unlock(lock);
    CU_CHECK("the handler's error string came back through the registered free too", released >= 2);

    err = NULL;
    rc = net_org_call(client, "no.such.service", 15, (const uint8_t*)"x", 1, 3000, 0, &resp, &resp_len, &err);
    CU_CHECK_RC("net_org_call: an unserved service is NET_ORG_ERR_DISCOVERY", rc, NET_ORG_ERR_DISCOVERY);
    CU_CHECK("discovery refusal: org:discovery:", err != NULL && strncmp(err, "org:discovery:", 14) == 0);
    net_org_free_cstring(err);

    CU_CHECK("net_org_reserve_cancel_token: non-zero", net_org_reserve_cancel_token(client) != 0);
    CU_CHECK("net_org_reserve_cancel_token: 0 for NULL", net_org_reserve_cancel_token(NULL) == 0);

    net_org_serve_handle_free(&serve);
    CU_CHECK("net_org_serve_handle_free: NULLs the handle", serve == NULL);
    net_org_client_free(&client);
    CU_CHECK("net_org_client_free: NULLs the client", client == NULL);
    net_org_client_free(&client);
    net_org_client_free(NULL);
    net_org_credentials_free(NULL);
    net_org_free_cstring(NULL);
    CU_SURVIVED("every free accepts NULL and a pointer to NULL");
    CU_CHECK_RC("net_mesh_shutdown: caller", net_mesh_shutdown(caller), 0);
    CU_CHECK_RC("net_mesh_shutdown: provider", net_mesh_shutdown(provider), 0);
    net_mesh_free(caller);
    net_mesh_free(provider);
    cu_mutex_free(lock);
    return cu_finish();
}
