/*
 * subnet.c — a subnet-exported, organization-protected service: gateway
 * provisioning, the exported serve, and the exported call, from C
 * (net_subnet.h, net_org.h).
 *
 * NET-NEEDS: subnet-scenario
 *
 * The C mirror of bindings/go/org-ffi/tests/subnet_live_c_abi.rs. The
 * runner generates the scenario (gen_subnet_scenario: a subnet authority
 * root, an EXPORT credential at the exact crossing, the boundary, the
 * provider's org authority, a same-org caller and a foreign-org caller)
 * and names its directory in NET_SUBNET_SCENARIO.
 *
 * Checked:
 *   - the provider's trust anchors, attachment and named export are
 *     CONFIGURATION (net_mesh_new's JSON); callers carry none;
 *   - net_subnet_install_gateway_credentials (wholesale) and
 *     net_subnet_declare_boundaries accept the generated artifacts, and a
 *     malformed credential set refuses the whole batch with subnet:<kind>;
 *   - net_subnet_serve_exported refuses an unconfigured export name
 *     locally (subnet:unknown_export_name) and serves the configured one;
 *   - net_org_call_exported, from the same-org caller, reaches the handler,
 *     which sees the provider-verified facts the manifest names (the
 *     caller entity, acting for the provider's org);
 *   - a FOREIGN-org caller with valid credentials is refused, and the
 *     handler never runs for it — twice (a denial is not retried);
 *   - after net_org_serve_handle_close (idempotent) a call is refused.
 *
 * Discovery: the exported service is announced on the public plane; the
 * first call retries, bounded, re-announcing each round — the same
 * convergence the other bindings' S4 harnesses use. The refused calls
 * carry a 5 s deadline, so a correct refusal cannot hang the program.
 */

#define _CRT_SECURE_NO_WARNINGS /* getenv, on MSVC */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_subnet.h"

#include "consumer_util.h"
#include "loaded_module.h"

static cu_mutex* lock;
static net_org_caller_t seen;
static int calls;

static void callback_free(void* p) {
    free(p);
}

static int handler(uint64_t handler_id, const net_org_caller_t* caller, const uint8_t* req, size_t req_len,
                   uint8_t** out_resp, size_t* out_resp_len, char** out_err) {
    (void)handler_id, (void)out_err;
    cu_mutex_lock(lock);
    seen = *caller;
    calls++;
    cu_mutex_unlock(lock);
    *out_resp = (uint8_t*)malloc(req_len + 7);
    if (!*out_resp) {
        return -1;
    }
    memcpy(*out_resp, "served:", 7);
    memcpy(*out_resp + 7, req, req_len);
    *out_resp_len = req_len + 7;
    return 0;
}

static int call_count(void) {
    int n;
    cu_mutex_lock(lock);
    n = calls;
    cu_mutex_unlock(lock);
    return n;
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

/* The integers of the first `[a, b, ...]` after `"key":`, at or after
 * `from`. Returns the count, or -1. */
static int int_list(const char* from, const char* key, int* out, int max) {
    char pat[64];
    const char* p;
    int n = 0;
    snprintf(pat, sizeof pat, "\"%s\":", key);
    p = strstr(from, pat);
    p = p ? strchr(p, '[') : NULL;
    if (p == NULL) {
        return -1;
    }
    p++;
    while (*p && *p != ']') {
        if (*p >= '0' && *p <= '9') {
            if (n == max) {
                return -1;
            }
            out[n] = (int)strtol(p, (char**)&p, 10);
            n++;
        } else {
            p++;
        }
    }
    return *p == ']' ? n : -1;
}

/* The first string inside the array that follows `"key":`. */
static int first_str_in_list(const char* from, const char* key, char* out, size_t cap) {
    char pat[64];
    const char *p, *q;
    snprintf(pat, sizeof pat, "\"%s\":", key);
    p = strstr(from, pat);
    p = p ? strchr(p, '[') : NULL;
    p = p ? strchr(p, '"') : NULL;
    if (p == NULL) {
        return -1;
    }
    p++;
    q = strchr(p, '"');
    if (!q || (size_t)(q - p) + 1 > cap) {
        return -1;
    }
    memcpy(out, p, (size_t)(q - p));
    out[q - p] = '\0';
    return 0;
}

static void list_json(const int* v, int n, char* out, size_t cap) {
    size_t used = 0;
    int i;
    used += (size_t)snprintf(out + used, cap - used, "[");
    for (i = 0; i < n; i++) {
        used += (size_t)snprintf(out + used, cap - used, i ? ",%d" : "%d", v[i]);
    }
    snprintf(out + used, cap - used, "]");
}

static unsigned char* scenario_file(const char* dir, const char* rel, size_t* len) {
    char path[1024];
    snprintf(path, sizeof path, "%s/%s", dir, rel);
    return cu_read_file(path, len);
}

static int build(const char* json_tail, const char* psk, const char* seed, net_meshnode_t** out, char* addr,
                 size_t addr_len) {
    char cfg[2048];
    int attempt;
    for (attempt = 0; attempt < 8; attempt++) {
        if (cu_reserve_port(addr, addr_len) != 0) {
            continue;
        }
        snprintf(cfg, sizeof cfg,
                 "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\",\"identity_seed_hex\":\"%s\",\"heartbeat_ms\":200,"
                 "\"permissive_channels\":true%s}",
                 addr, psk, seed, json_tail);
        if (net_mesh_new(cfg, out) == 0) {
            return 0;
        }
    }
    return -1;
}

static int install_authority(net_meshnode_t* n, const char* dir, const char* rel) {
    char path[1024];
    char* err = NULL;
    int rc;
    snprintf(path, sizeof path, "%s/%s", dir, rel);
    rc = net_org_install_authority(net_mesh_arc_clone(n), path, strlen(path), &err);
    net_org_free_cstring(err);
    return rc;
}

static NetOrgClient* bind_client(net_meshnode_t* n, const char* dir, const char* mem_rel, const char* disp_rel) {
    size_t mem_len = 0, disp_len = 0;
    unsigned char* mem = scenario_file(dir, mem_rel, &mem_len);
    unsigned char* disp = scenario_file(dir, disp_rel, &disp_len);
    NetOrgCredentials* creds = NULL;
    NetOrgClient* client = NULL;
    char* err = NULL;
    if (mem && disp && net_org_credentials_new(mem, mem_len, disp, disp_len, NULL, NULL, 0, NULL, 0, &creds, &err) == 0) {
        if (net_org_bind(net_mesh_arc_clone(n), &creds, &client, &err) != 0) {
            client = NULL;
        }
    }
    net_org_free_cstring(err);
    free(mem);
    free(disp);
    return client;
}

/* A call that must be refused (5 s deadline); 1 if it was. */
static int refused(NetOrgClient* c, const char* service, const char* body) {
    uint8_t* out = NULL;
    size_t out_len = 0;
    char* err = NULL;
    int rc = net_org_call_exported(c, service, strlen(service), (const uint8_t*)body, strlen(body), 5000, 0, &out,
                                   &out_len, &err);
    net_org_free_cstring(err);
    if (rc == NET_ORG_OK) {
        net_org_response_free(out, out_len);
        return 0;
    }
    return 1;
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_mesh_arc_clone),
        CLM_FN(net_mesh_announce_capabilities),
        CLM_FN(net_subnet_install_gateway_credentials),
        CLM_FN(net_subnet_declare_boundaries),
        CLM_FN(net_subnet_serve_exported),
        CLM_FN(net_org_install_authority),
        CLM_FN(net_org_credentials_new),
        CLM_FN(net_org_bind),
        CLM_FN(net_org_set_callback_free),
        CLM_FN(net_org_set_handler_dispatcher),
        CLM_FN(net_org_call_exported),
        CLM_FN(net_org_serve_handle_close),
        CLM_FN(net_org_response_free),
        CLM_FN(net_org_free_cstring),
    };
    const char* dir = getenv("NET_SUBNET_SCENARIO");
    char psk[80], service[128], export_name[128], unknown_export[128], access[32], auth_hex[80], root_hex[80];
    char bind_auth_hex[80], p_seed[80], p_org[80], p_auth[256], p_gw[256];
    char c_seed[80], c_entity[80], c_org[80], c_auth[256], c_mem[256], c_disp[256];
    char f_seed[80], f_auth[256], f_mem[256], f_disp[256];
    char attach_json[64], bind_path_json[64], tail[1536], prov_addr[32], c_addr[32], f_addr[32];
    int attach[4], bind_path[4], boundary[4], n_attach, n_bind, n_boundary, attempt;
    uint64_t life = 0, epoch = 0;
    uint8_t provider_org[32], caller_org[32], caller_entity[32], bind_authority[32];
    unsigned char* manifest;
    const char *prov, *callr, *foreign_b, *binding;
    size_t len = 0;
    net_meshnode_t *provider = NULL, *caller = NULL, *foreign = NULL;
    NetOrgClient *client = NULL, *foreign_client = NULL;
    NetOrgServeHandle *serve = NULL, *unknown_serve = NULL;
    net_subnet_path_t path;
    char* err = NULL;
    uint8_t* resp = NULL;
    size_t resp_len = 0;
    int rc, admitted_calls;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    CU_CHECK("NET_SUBNET_SCENARIO names the generated scenario", dir != NULL);
    CU_CHECK_RC("net_org_check_abi_version", net_org_check_abi_version(NET_ORG_ABI_VERSION), NET_ORG_OK);
    if (cu_net_init() != 0) {
        printf("FAIL setup: cu_net_init\n");
        return 1;
    }
    lock = cu_mutex_new();
    CU_CHECK("a mutex for the handler's facts", lock != NULL);

    manifest = scenario_file(dir, "manifest.json", &len);
    CU_CHECK("the manifest", manifest != NULL);
    {
        const char* m = (const char*)manifest;
        binding = strstr(m, "\"export_binding\"");
        prov = strstr(m, "\"provider\"");
        callr = strstr(m, "\"caller\"");
        foreign_b = strstr(m, "\"foreign_caller\"");
        CU_CHECK("the manifest's blocks", binding && prov && callr && foreign_b && prov < callr && callr < foreign_b);
        CU_CHECK("manifest: scenario fields",
                 cu_json_str(m, NULL, "psk_hex", psk, sizeof psk) == 0 &&
                     cu_json_str(m, NULL, "exported_service", service, sizeof service) == 0 &&
                     cu_json_str(m, NULL, "export_name", export_name, sizeof export_name) == 0 &&
                     cu_json_str(m, NULL, "unknown_export_name", unknown_export, sizeof unknown_export) == 0 &&
                     cu_json_str(m, NULL, "export_access", access, sizeof access) == 0 &&
                     cu_json_str(m, NULL, "authority_hex", auth_hex, sizeof auth_hex) == 0 &&
                     first_str_in_list(m, "root_hexes", root_hex, sizeof root_hex) == 0 &&
                     cu_json_u64(m, "maximum_grant_lifetime_secs", &life) == 0);
        CU_CHECK("manifest: the export binding",
                 cu_json_str(binding, prov, "authority_hex", bind_auth_hex, sizeof bind_auth_hex) == 0 &&
                     (n_bind = int_list(binding, "path", bind_path, 4)) >= 1 &&
                     cu_json_u64(binding, "topology_epoch", &epoch) == 0 && hex32(bind_auth_hex, bind_authority) == 0);
        CU_CHECK("manifest: the provider",
                 cu_json_str(prov, callr, "seed_hex", p_seed, sizeof p_seed) == 0 &&
                     cu_json_str(prov, callr, "org_id_hex", p_org, sizeof p_org) == 0 &&
                     cu_json_str(prov, callr, "authority_dir", p_auth, sizeof p_auth) == 0 &&
                     cu_json_str(prov, callr, "gateway_credentials_path", p_gw, sizeof p_gw) == 0 &&
                     (n_attach = int_list(prov, "attachment", attach, 4)) >= 1 &&
                     (n_boundary = int_list(prov, "boundary_paths", boundary, 4)) >= 1 && hex32(p_org, provider_org) == 0);
        CU_CHECK("manifest: the callers",
                 cu_json_str(callr, foreign_b, "seed_hex", c_seed, sizeof c_seed) == 0 &&
                     cu_json_str(callr, foreign_b, "entity_id_hex", c_entity, sizeof c_entity) == 0 &&
                     cu_json_str(callr, foreign_b, "org_id_hex", c_org, sizeof c_org) == 0 &&
                     cu_json_str(callr, foreign_b, "authority_dir", c_auth, sizeof c_auth) == 0 &&
                     cu_json_str(callr, foreign_b, "membership_path", c_mem, sizeof c_mem) == 0 &&
                     cu_json_str(callr, foreign_b, "dispatcher_path", c_disp, sizeof c_disp) == 0 &&
                     cu_json_str(foreign_b, NULL, "seed_hex", f_seed, sizeof f_seed) == 0 &&
                     cu_json_str(foreign_b, NULL, "authority_dir", f_auth, sizeof f_auth) == 0 &&
                     cu_json_str(foreign_b, NULL, "membership_path", f_mem, sizeof f_mem) == 0 &&
                     cu_json_str(foreign_b, NULL, "dispatcher_path", f_disp, sizeof f_disp) == 0 &&
                     hex32(c_entity, caller_entity) == 0 && hex32(c_org, caller_org) == 0);
    }
    free(manifest);
    CU_CHECK("scenario: the same-org caller is in the provider's organization",
             memcmp(caller_org, provider_org, 32) == 0);

    list_json(attach, n_attach, attach_json, sizeof attach_json);
    list_json(bind_path, n_bind, bind_path_json, sizeof bind_path_json);
    snprintf(tail, sizeof tail,
             ",\"subnet_authorities\":[{\"authority_hex\":\"%s\",\"root_hexes\":[\"%s\"],"
             "\"maximum_grant_lifetime_secs\":%llu}],\"subnet_attachment\":%s,"
             "\"subnet_exports\":[{\"name\":\"%s\",\"access\":\"%s\",\"binding\":{\"subnet\":{\"authority_hex\":\"%s\","
             "\"path\":{\"levels\":%s}},\"topology_epoch\":%llu}}]",
             auth_hex, root_hex, (unsigned long long)life, attach_json, export_name, access, bind_auth_hex,
             bind_path_json, (unsigned long long)epoch);
    CU_CHECK_RC("bring-up: the provider, its subnet configuration in net_mesh_new's JSON",
                build(tail, psk, p_seed, &provider, prov_addr, sizeof prov_addr), 0);
    CU_CHECK_RC("bring-up: the same-org caller (no subnet configuration)",
                build("", psk, c_seed, &caller, c_addr, sizeof c_addr), 0);
    CU_CHECK_RC("bring-up: the foreign-org caller", build("", psk, f_seed, &foreign, f_addr, sizeof f_addr), 0);

    CU_CHECK_RC("net_org_install_authority: provider", install_authority(provider, dir, p_auth), NET_ORG_OK);
    CU_CHECK_RC("net_org_install_authority: caller", install_authority(caller, dir, c_auth), NET_ORG_OK);
    CU_CHECK_RC("net_org_install_authority: foreign caller", install_authority(foreign, dir, f_auth), NET_ORG_OK);

    {
        static const uint8_t junk[3] = {1, 2, 3};
        const uint8_t* bad_ptrs[1];
        size_t bad_lens[1];
        size_t gw_len = 0;
        unsigned char* gw = scenario_file(dir, p_gw, &gw_len);
        const uint8_t* ptrs[1];
        size_t lens[1];
        bad_ptrs[0] = junk;
        bad_lens[0] = sizeof junk;
        err = NULL;
        rc = net_subnet_install_gateway_credentials(net_mesh_arc_clone(provider), bad_ptrs, bad_lens, 1, &err);
        CU_CHECK_RC("install_gateway_credentials: a malformed set is NET_ORG_ERR_SUBNET", rc, NET_ORG_ERR_SUBNET);
        CU_CHECK("install_gateway_credentials: the subnet: wire", err != NULL && strstr(err, "subnet:") != NULL);
        net_org_free_cstring(err);
        ptrs[0] = gw;
        lens[0] = gw_len;
        err = NULL;
        rc = net_subnet_install_gateway_credentials(net_mesh_arc_clone(provider), ptrs, lens, 1, &err);
        if (rc != NET_ORG_OK) {
            printf("  (gateway: %s)\n", err ? err : "(null)");
        }
        net_org_free_cstring(err);
        free(gw);
        CU_CHECK_RC("net_subnet_install_gateway_credentials: the generated set", rc, NET_ORG_OK);
    }
    memset(&path, 0, sizeof path);
    path.depth = (uint8_t)n_boundary;
    for (rc = 0; rc < n_boundary; rc++) {
        path.levels[rc] = (uint8_t)boundary[rc];
    }
    err = NULL;
    rc = net_subnet_declare_boundaries(net_mesh_arc_clone(provider), bind_authority, (uint32_t)epoch, &path, 1, &err);
    if (rc != NET_ORG_OK) {
        printf("  (boundaries: %s)\n", err ? err : "(null)");
    }
    net_org_free_cstring(err);
    CU_CHECK_RC("net_subnet_declare_boundaries", rc, NET_ORG_OK);

    CU_CHECK_RC("bring-up: handshake caller", cu_mesh_handshake(provider, caller, prov_addr), 0);
    CU_CHECK_RC("bring-up: handshake foreign caller", cu_mesh_handshake(provider, foreign, prov_addr), 0);
    CU_CHECK("bring-up: start all three",
             net_mesh_start(provider) == 0 && net_mesh_start(caller) == 0 && net_mesh_start(foreign) == 0);

    CU_CHECK_RC("net_org_set_callback_free", net_org_set_callback_free(callback_free), NET_ORG_OK);
    CU_CHECK_RC("net_org_set_handler_dispatcher", net_org_set_handler_dispatcher(handler), NET_ORG_OK);
    err = NULL;
    rc = net_subnet_serve_exported(net_mesh_arc_clone(provider), service, strlen(service), unknown_export,
                                   strlen(unknown_export), net_org_reserve_handler_id(), &unknown_serve, &err);
    CU_CHECK("net_subnet_serve_exported: an unconfigured export is refused", rc != NET_ORG_OK);
    CU_CHECK("refusal: subnet:unknown_export_name", err != NULL && strstr(err, "subnet:unknown_export_name") != NULL);
    net_org_free_cstring(err);
    err = NULL;
    rc = net_subnet_serve_exported(net_mesh_arc_clone(provider), service, strlen(service), export_name,
                                   strlen(export_name), net_org_reserve_handler_id(), &serve, &err);
    if (rc != NET_ORG_OK) {
        printf("  (serve: %s)\n", err ? err : "(null)");
    }
    net_org_free_cstring(err);
    CU_CHECK_RC("net_subnet_serve_exported: the configured export", rc, NET_ORG_OK);

    client = bind_client(caller, dir, c_mem, c_disp);
    foreign_client = bind_client(foreign, dir, f_mem, f_disp);
    CU_CHECK("net_org_bind: both callers (organization authority only)", client != NULL && foreign_client != NULL);

    for (attempt = 0; attempt < 90; attempt++) {
        net_mesh_announce_capabilities(provider, "{}");
        net_mesh_announce_capabilities(caller, "{}");
        err = NULL;
        rc = net_org_call_exported(client, service, strlen(service), (const uint8_t*)"{\"n\":1}", 7, 0, 0, &resp,
                                   &resp_len, &err);
        if (rc == NET_ORG_OK) {
            break;
        }
        if (attempt == 89) {
            printf("  (exported call: rc %d, err %s)\n", rc, err ? err : "(null)");
        }
        net_org_free_cstring(err);
        cu_sleep_ms(500);
    }
    CU_CHECK_RC("net_org_call_exported: the same-org caller is admitted", rc, NET_ORG_OK);
    CU_CHECK("net_org_call_exported: the handler's response",
             resp_len == 14 && memcmp(resp, "served:{\"n\":1}", 14) == 0);
    net_org_response_free(resp, resp_len);
    admitted_calls = call_count();
    CU_CHECK_RC("handler: ran once", admitted_calls, 1);
    cu_mutex_lock(lock);
    CU_CHECK("handler: the caller entity the manifest names", memcmp(seen.caller, caller_entity, 32) == 0);
    CU_CHECK("handler: acting for the provider's organization (same-org)",
             memcmp(seen.acting_org, provider_org, 32) == 0 && memcmp(seen.provider_org, provider_org, 32) == 0);
    cu_mutex_unlock(lock);

    CU_CHECK("a foreign-org caller with valid credentials is refused", refused(foreign_client, service, "{\"n\":50}"));
    CU_CHECK_RC("the handler never ran for it", call_count(), admitted_calls);
    CU_CHECK("refused again (a denial is not retried into the handler)", refused(foreign_client, service, "{\"n\":51}"));
    CU_CHECK_RC("the handler still never ran for it", call_count(), admitted_calls);

    net_org_serve_handle_close(serve);
    net_org_serve_handle_close(serve);
    CU_CHECK("net_org_serve_handle_close: idempotent", 1);
    CU_CHECK("after close: a call is not served", refused(client, service, "{\"n\":99}"));
    CU_CHECK_RC("after close: the handler did not run", call_count(), admitted_calls);

    net_org_serve_handle_free(&serve);
    net_org_serve_handle_free(&unknown_serve);
    net_org_client_free(&client);
    net_org_client_free(&foreign_client);
    CU_CHECK("handles freed and NULLed", serve == NULL && client == NULL && foreign_client == NULL);
    CU_CHECK("net_mesh_shutdown: all three",
             net_mesh_shutdown(foreign) == 0 && net_mesh_shutdown(caller) == 0 && net_mesh_shutdown(provider) == 0);
    net_mesh_free(foreign);
    net_mesh_free(caller);
    net_mesh_free(provider);
    cu_mutex_free(lock);
    return cu_finish();
}
