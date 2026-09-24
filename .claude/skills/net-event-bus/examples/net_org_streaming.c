/*
 * Protected org streaming over the C ABI — against the single `libnet`.
 *
 * One file, one shape (server-streaming), both roles: this program brings up
 * TWO live mesh nodes over loopback UDP, serves a protected service on one
 * (net_org_serve_streaming + a process-wide shape dispatcher carrying the
 * verified net_org_caller_t), and invokes it from the other with
 * net_org_call_streaming — whose stream is the SHARED handle type, drained
 * through the very net_rpc_stream_next a public stream uses (ABI 0x0002:
 * one libnet, one handle vocabulary). It then checks the two properties it
 * exists for: that every item the handler emitted arrived at the caller, and
 * that the handler observed a verified cross-org caller.
 *
 * # Why this links against ONE library
 *
 *   gcc -o app net_org_streaming.c -I net/crates/net/include \
 *       -L <dir holding libnet> -lnet -lpthread -ldl -lm
 *
 * Do not add a second `-l`. Every `net_org_*` and `net_rpc_*` and
 * `net_mesh_*` symbol here resolves out of the one cdylib `libnet`; a second
 * library exporting the core would put two copies of the core's statics in
 * this process (the parked-thread registry that made the old
 * one-cdylib-per-surface layout lose wakeups).
 *
 * # Credentials are issued material — this example mints none
 *
 * Org credentials (membership certs, dispatcher grants, capability grants,
 * adopted authorities) come from your organization's issuance process (the
 * `net org` CLI). So this runnable demo boots a throwaway cross-org chain
 * with the in-repo scenario generator — the same artifacts every language
 * harness loads:
 *
 *   cargo run -q --release -p net-mesh-sdk --features net,cortex,fixtures \
 *       --example gen_org_scenario -- <dir>
 *
 * It then consumes only the manifest's bytes-and-PATHS (the audience secret
 * crosses as a file PATH, never as bytes — the one rule that shapes
 * net_org.h).
 *
 * # Handler-drop contract
 *
 * The retire supervisor may drop the handler future WITHOUT a final poll.
 * At this callback boundary: the handler receives NO cancellation callback
 * and NO final-polled notification when a deadline / cancel / revocation /
 * teardown retires the call. For the shapes with an input side, retirement
 * is observed through net_rpc_request_stream_next returning
 * NET_RPC_ERR_STREAM_DONE. A response sink is NOT a retirement signal:
 * RpcResponseSink::send is a lossy, non-blocking try_send, so
 * net_rpc_response_sink_send returns NET_RPC_OK on a live handle even after
 * the call is retired. This server-streaming handler therefore has no
 * retirement observable at all — it emits its bounded batch and returns.
 */
#include "net.go.h"  /* mesh bring-up: net_mesh_new / accept / connect / start */
#include "net_rpc.h" /* the shared streaming handles + net_rpc_stream_next    */
#include "net_org.h" /* the org verbs and net_org_caller_t                    */

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef _WIN32
#include <windows.h>
#include <process.h>
#define getpid _getpid
#else
#include <unistd.h>
#endif

/* ------------------------------------------------------------------ utils */

static void die(const char* what, const char* detail) {
    fprintf(stderr, "net_org_streaming: %s: %s\n", what, detail ? detail : "(no detail)");
    exit(1);
}

static char* read_file(const char* path, size_t* out_len) {
    FILE* f = fopen(path, "rb");
    if (!f) die("open", path);
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char* buf = (char*)malloc((size_t)n + 1);
    if (!buf) die("malloc", path);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) die("read", path);
    fclose(f);
    buf[n] = '\0';
    *out_len = (size_t)n;
    return buf;
}

/* A minimal `"key": "value"` scanner — enough for the generated manifest's
 * flat string fields. `from` scopes the search (role blocks). */
static void json_str(char* out, size_t cap, const char* json, const char* from,
                     const char* key, const char* end) {
    char pattern[128];
    snprintf(pattern, sizeof pattern, "\"%s\"", key);
    const char* p = strstr(from, pattern);
    if (!p || (end && p >= end)) die("manifest key missing", key);
    p = strchr(p + strlen(pattern), '"'); /* the opening quote of the value */
    if (!p) die("manifest value missing", key);
    p++;
    const char* q = strchr(p, '"');
    if (!q) die("manifest value unterminated", key);
    size_t n = (size_t)(q - p);
    if (n + 1 > cap) die("manifest value too long", key);
    memcpy(out, p, n);
    out[n] = '\0';
}

static void join2(char* out, size_t cap, const char* dir, const char* rel) {
    if (snprintf(out, cap, "%s/%s", dir, rel) >= (int)cap) die("path too long", rel);
}

static void sleep_ms(int ms) {
#ifdef _WIN32
    Sleep((unsigned long)ms);
#else
    struct timespec ts = { ms / 1000, (long)(ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
#endif
}

/* ------------------------------------------------------- handler + facts */

static int g_chunks = 0;
static int g_attribution_ok = 0;

/* The shape dispatcher: rpc-ffi's streaming fn type plus the leading
 * provider-verified caller. Every field of `caller` was verified by the
 * admission engine before this ran; none is caller-claimed. */
static int serve_handler(uint64_t handler_id,
                         const net_org_caller_t* caller,
                         const uint8_t* req_ptr, size_t req_len,
                         RpcResponseSinkHandleC* sink,
                         char** out_err) {
    (void)handler_id;
    (void)req_ptr;
    (void)req_len;

    /* This demo serves Granted cross-org: the caller acts for ANOTHER org. */
    int same_org = memcmp(caller->acting_org, caller->provider_org, 32) == 0;

    int sent = 0;
    for (int i = 1; i <= 3; i++) {
        char body[64];
        int n = snprintf(body, sizeof body, "{\"n\":%d,\"servedBy\":\"c-s4\"}", i);
        int rc = net_rpc_response_sink_send(sink, (const uint8_t*)body, (size_t)n);
        if (rc != 0) {
            /* NOT a retirement observable — a live sink reports success even
             * for a retired call (RpcResponseSink::send is a lossy try_send),
             * and -6 here means the handle was torn down or consumed. The
             * retire supervisor may still drop this handler without a final
             * poll, so the batch below is deliberately bounded. */
            if (out_err) {
                *out_err = (char*)malloc(32);
                if (*out_err) snprintf(*out_err, 32, "sink send failed at %d", i);
            }
            return -1;
        }
        sent++;
    }

    g_chunks = sent;
    g_attribution_ok = same_org ? 0 : 1; /* cross-org: IsSameOrg must be false */
    return 0;
}

static void callback_free(void* p) { free(p); }

/* --------------------------------------------------------- accept helper */

struct accept_args {
    net_meshnode_t* node;
    uint64_t peer_node_id;
    int rc;
};

static void* accept_thread(void* raw) {
    struct accept_args* a = (struct accept_args*)raw;
    char* addr = NULL;
    size_t len = 0;
    a->rc = net_mesh_accept(a->node, a->peer_node_id, &addr, &len);
    if (addr) net_free_string(addr);
    return NULL;
}

/* ------------------------------------------------------------------- main */

int main(void) {
    /* Pin the ABI this file was written against — hard-fail on drift. */
    if (net_org_check_abi_version(NET_ORG_ABI_VERSION) != NET_ORG_OK) {
        fprintf(stderr, "net_org_streaming: ABI mismatch (library 0x%04x, header 0x%04x)\n",
                net_org_abi_version(), (unsigned)NET_ORG_ABI_VERSION);
        return 1;
    }

    /* 1. Mint the throwaway cross-org scenario with the in-repo generator
     *    (issued material belongs to the `net org` CLI; this example mints
     *    none of its own). Fails loudly if the toolchain cannot run — never
     *    a silent pass. */
    const char* tmp = getenv("TMPDIR");
    if (!tmp) tmp = getenv("TEMP");
    if (!tmp) tmp = getenv("TMP");
    if (!tmp) tmp = ".";
    char scenario[512];
    snprintf(scenario, sizeof scenario, "%s/net-org-streaming-%ld", tmp, (long)getpid());

    char cmd[1024];
    /* The example runs from the repo root (the skill-examples runner's
     * cwd): `--manifest-path` pins the net workspace explicitly so the
     * invocation does not depend on cargo finding it from cwd. */
    snprintf(cmd, sizeof cmd,
             "cargo run -q --release "
             "--manifest-path net/crates/net/Cargo.toml "
             "-p net-mesh-sdk "
             "--features net,cortex,fixtures "
             "--example gen_org_scenario -- %s", scenario);
    if (system(cmd) != 0) {
        die("scenario generation failed", "this example boots a throwaway org chain "
            "with `cargo run --example gen_org_scenario`; without a Rust toolchain "
            "it cannot run (linking against libnet is not enough to ISSUE credentials)");
    }

    char manifest_path[600];
    join2(manifest_path, sizeof manifest_path, scenario, "manifest.json");
    size_t manifest_len = 0;
    char* manifest = read_file(manifest_path, &manifest_len);

    /* 2. Pull the manifest's bytes-and-PATHS (role blocks: `provider` then
     *    `caller` — the generator's field order). */
    char psk_hex[128], service[128];
    char prov_seed[128], prov_auth[512], prov_grant[512], prov_secret[512];
    char call_seed[128], call_auth[512], call_mem[512], call_disp[512], call_grant[512], call_secret[512];
    json_str(psk_hex, sizeof psk_hex, manifest, manifest, "psk_hex", NULL);
    json_str(service, sizeof service, manifest, manifest, "granted_service", NULL);
    const char* p0 = strstr(manifest, "\"provider\"");
    const char* c0 = strstr(manifest, "\"caller\"");
    if (!p0 || !c0) die("manifest role blocks missing", NULL);
    json_str(prov_seed, sizeof prov_seed, manifest, p0, "seed_hex", c0);
    json_str(prov_auth, sizeof prov_auth, manifest, p0, "authority_dir", c0);
    json_str(prov_grant, sizeof prov_grant, manifest, p0, "grant_path", c0);
    json_str(prov_secret, sizeof prov_secret, manifest, p0, "grant_secret_path", c0);
    json_str(call_seed, sizeof call_seed, manifest, c0, "seed_hex", NULL);
    json_str(call_auth, sizeof call_auth, manifest, c0, "authority_dir", NULL);
    json_str(call_mem, sizeof call_mem, manifest, c0, "membership_path", NULL);
    json_str(call_disp, sizeof call_disp, manifest, c0, "dispatcher_path", NULL);
    json_str(call_grant, sizeof call_grant, manifest, c0, "grant_path", NULL);
    json_str(call_secret, sizeof call_secret, manifest, c0, "grant_secret_path", NULL);

    char prov_auth_full[768], call_auth_full[768];
    char prov_grant_full[768], prov_secret_full[768];
    char call_mem_full[768], call_disp_full[768], call_grant_full[768], call_secret_full[768];
    join2(prov_auth_full, sizeof prov_auth_full, scenario, prov_auth);
    join2(call_auth_full, sizeof call_auth_full, scenario, call_auth);
    join2(prov_grant_full, sizeof prov_grant_full, scenario, prov_grant);
    join2(prov_secret_full, sizeof prov_secret_full, scenario, prov_secret);
    join2(call_mem_full, sizeof call_mem_full, scenario, call_mem);
    join2(call_disp_full, sizeof call_disp_full, scenario, call_disp);
    join2(call_grant_full, sizeof call_grant_full, scenario, call_grant);
    join2(call_secret_full, sizeof call_secret_full, scenario, call_secret);

    /* 3. Two nodes. The ACCEPTOR binds a known address (net_mesh_new on :0
     *    binds a port C cannot read back), chosen by retrying the bind. */
    char prov_cfg[1024], call_cfg[1024], prov_addr[64];
    net_meshnode_t* provider = NULL;
    net_meshnode_t* caller = NULL;
    long port = 20000 + ((long)getpid() % 20000);
    for (int attempt = 0; attempt < 32 && !provider; attempt++) {
        snprintf(prov_addr, sizeof prov_addr, "127.0.0.1:%ld", port + attempt);
        snprintf(prov_cfg, sizeof prov_cfg,
                 "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\","
                 "\"identity_seed_hex\":\"%s\",\"heartbeat_ms\":200}",
                 prov_addr, psk_hex, prov_seed);
        if (net_mesh_new(prov_cfg, &provider) != 0) provider = NULL;
    }
    if (!provider) die("net_mesh_new (provider)", "no free bind port in range");
    snprintf(call_cfg, sizeof call_cfg,
             "{\"bind_addr\":\"127.0.0.1:0\",\"psk_hex\":\"%s\","
             "\"identity_seed_hex\":\"%s\",\"heartbeat_ms\":200}",
             psk_hex, call_seed);
    if (net_mesh_new(call_cfg, &caller) != 0) die("net_mesh_new (caller)", NULL);

    /* 4. Provision (node startup — adoption/issuance stay in the CLI): both
     *    nodes install their adopted authority; the Granted provider installs
     *    its grant audience (bytes + secret PATH). */
    char* err = NULL;
    if (net_org_install_authority(net_mesh_arc_clone(provider),
                                  prov_auth_full, strlen(prov_auth_full), &err) != 0)
        die("install provider authority", err);
    if (net_org_install_authority(net_mesh_arc_clone(caller),
                                  call_auth_full, strlen(call_auth_full), &err) != 0)
        die("install caller authority", err);
    {
        size_t grant_len = 0;
        char* grant = read_file(prov_grant_full, &grant_len);
        int rc = net_org_install_provider_grant_audience(
            net_mesh_arc_clone(provider),
            (const uint8_t*)grant, grant_len,
            prov_secret_full, strlen(prov_secret_full), &err);
        free(grant);
        if (rc != 0) die("install provider grant audience", err);
    }

    /* 5. Bind the caller: credentials cross as BYTES, the audience secret as
     *    a PATH (never bytes). */
    NetOrgCredentials* creds = NULL;
    {
        size_t mem_len = 0, disp_len = 0, grant_len = 0;
        char* mem = read_file(call_mem_full, &mem_len);
        char* disp = read_file(call_disp_full, &disp_len);
        char* grant = read_file(call_grant_full, &grant_len);
        const uint8_t* grants[1] = { (const uint8_t*)grant };
        const size_t grant_lens[1] = { grant_len };
        const char* secrets[1] = { call_secret_full };
        int rc = net_org_credentials_new(
            (const uint8_t*)mem, mem_len, (const uint8_t*)disp, disp_len,
            grants, grant_lens, 1, secrets, 1, &creds, &err);
        free(mem);
        free(disp);
        free(grant);
        if (rc != 0) die("net_org_credentials_new", err);
    }
    NetOrgClient* client = NULL;
    if (net_org_bind(net_mesh_arc_clone(caller), &creds, &client, &err) != 0)
        die("net_org_bind", err);

    /* 6. Handshake — every accept() lands before any start(), and accept
     *    BLOCKS until a connector dials, so the two run concurrently (the
     *    shape every harness uses). */
    {
        struct accept_args args = { provider, net_mesh_node_id(caller), 0 };
        pthread_t th;
        if (pthread_create(&th, NULL, accept_thread, &args) != 0) die("pthread_create", NULL);

        char* pub_hex = NULL;
        size_t pub_len = 0;
        if (net_mesh_public_key_hex(provider, &pub_hex, &pub_len) != 0)
            die("net_mesh_public_key_hex", NULL);
        int rc = net_mesh_connect(caller, prov_addr, pub_hex, net_mesh_node_id(provider));
        net_free_string(pub_hex);
        if (rc != 0) die("net_mesh_connect", NULL);
        pthread_join(th, NULL);
        if (args.rc != 0) die("net_mesh_accept", NULL);
    }
    /* Start the CALLER first: the scoped envelope ships on the announce
     * path, and the provider's start-time announcement must not race the
     * caller's receive loop coming up (a dropped first announce waits a
     * whole re-announce window to recover). */
    if (net_mesh_start(caller) != 0) die("net_mesh_start (caller)", NULL);
    if (net_mesh_start(provider) != 0) die("net_mesh_start (provider)", NULL);

    /* 7. Serve. Callback-buffer ownership first (the library refuses
     *    dispatchers without a deallocator — the allocator that creates a
     *    buffer must release it), then the shape dispatcher, then
     *    reserve → store → serve (pre-registration closes the
     *    request-arrives-before-store race). */
    if (net_org_set_callback_free(callback_free) != 0) die("net_org_set_callback_free", NULL);
    if (net_org_set_streaming_handler_dispatcher(serve_handler) != 0)
        die("net_org_set_streaming_handler_dispatcher", NULL);
    uint64_t handler_id = net_org_reserve_handler_id();

    NetOrgServeHandle* sh = NULL;
    if (net_org_serve_streaming(net_mesh_arc_clone(provider),
                                service, strlen(service),
                                NET_ORG_ACCESS_GRANTED, handler_id, &sh, &err) != 0)
        die("net_org_serve_streaming", err);

    /* 8. Call — one request in (the signed opening binds it), chunks out
     *    through the SHARED handle. The opening call rides a bounded
     *    discovery-convergence retry — the language harnesses' precondition
     *    (tests_live.rs `converge_discovery`; go/org_test.go
     *    `convergeOrgCall`): the scoped/private announcements ride the
     *    announce path at the core's re-announce cadence, and the first call
     *    must not race their arrival. A `no_authorized_provider` refusal is
     *    LOCAL — nothing was sent and no proof was minted — so retrying it
     *    cannot resend a signed proof (the no-retry rule binds ISSUED
     *    proofs). Every other outcome is immediate. */
    RpcStreamHandleC* stream = NULL;
    const char* req = "{\"n\":1}";
    for (int attempt = 0;; attempt++) {
        err = NULL;
        int rc = net_org_call_streaming(client, service, strlen(service),
                                        (const uint8_t*)req, strlen(req),
                                        0, 0, &stream, &err);
        if (rc == 0) break;
        int retryable = (rc == NET_ORG_ERR_DISCOVERY) && err != NULL &&
            strncmp(err, "org:discovery:no_authorized_provider", 36) == 0;
        if (!retryable) die("net_org_call_streaming", err);
        if (err) net_org_free_cstring(err);
        stream = NULL;
        if (attempt >= 120) {
            die("discovery did not converge",
                "org:discovery:no_authorized_provider persisted for 60s");
        }
        sleep_ms(500);
    }

    int received = 0;
    for (;;) {
        uint8_t* chunk = NULL;
        size_t chunk_len = 0;
        char* cerr = NULL;
        int rc = net_rpc_stream_next(stream, &chunk, &chunk_len, &cerr);
        if (rc == NET_RPC_ERR_STREAM_DONE) break; /* clean terminal */
        if (rc != 0) {
            /* An org stream's midstream errors speak the `org:` wire —
             * the canonical vocabulary net_org.h documents. */
            die("net_rpc_stream_next (midstream)", cerr);
        }
        received++;
        net_rpc_response_free(chunk, chunk_len);
    }
    net_rpc_stream_free(stream);

    /* 9. Assert the two properties this example exists for: delivery +
     *    explicit completion (every emitted item arrived, counts match), and
     *    handler-side attribution of the VERIFIED caller. */
    if (received != 3 || g_chunks != 3) {
        fprintf(stderr, "net_org_streaming: received=%d handler-sent=%d, want 3\n",
                received, g_chunks);
        return 1;
    }
    if (!g_attribution_ok) {
        fprintf(stderr, "net_org_streaming: handler did not observe a verified "
                        "cross-org caller\n");
        return 1;
    }
    printf("RESULT ok chunks=%d attribution=verified\n", received);

    /* Teardown: close the registration, close the client (the withdrawal
     * step), then the nodes. */
    net_org_serve_handle_close(sh);
    net_org_serve_handle_free(&sh);
    net_org_client_free(&client);
    net_mesh_shutdown(provider);
    net_mesh_free(provider);
    net_mesh_shutdown(caller);
    net_mesh_free(caller);
    free(manifest);
    return 0;
}
