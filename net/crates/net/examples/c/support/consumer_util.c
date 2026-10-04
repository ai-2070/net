/*
 * consumer_util.c — see consumer_util.h.
 */

#if defined(_WIN32) && !defined(_CRT_SECURE_NO_WARNINGS)
#define _CRT_SECURE_NO_WARNINGS /* fopen on paths this program built */
#endif
#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)
#define _POSIX_C_SOURCE 200809L
#endif

#include "consumer_util.h"

#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <winsock2.h>
#include <windows.h>
#include <direct.h>
#include <process.h>
#include <sys/stat.h>
#else
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#endif

int cu_checks = 0;

const char* CU_PSK_HEX = "4242424242424242424242424242424242424242424242424242424242424242";

int cu_finish(void) {
    printf("NET-CHECKS: %d\n", cu_checks);
    fflush(stdout);
    return 0;
}

/* ---- OS services ---- */

int cu_net_init(void) {
#ifdef _WIN32
    WSADATA wsa;
    return WSAStartup(MAKEWORD(2, 2), &wsa) == 0 ? 0 : -1;
#else
    return 0;
#endif
}

int cu_reserve_port(char* out, size_t out_len) {
    struct sockaddr_in sa;
#ifdef _WIN32
    SOCKET fd = socket(AF_INET, SOCK_DGRAM, 0);
    int sa_len = (int)sizeof sa;
    if (fd == INVALID_SOCKET) {
        return -1;
    }
#else
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    socklen_t sa_len = sizeof sa;
    if (fd < 0) {
        return -1;
    }
#endif
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    sa.sin_port = 0;
    if (bind(fd, (struct sockaddr*)&sa, sizeof sa) != 0 ||
        getsockname(fd, (struct sockaddr*)&sa, &sa_len) != 0) {
#ifdef _WIN32
        closesocket(fd);
#else
        close(fd);
#endif
        return -1;
    }
    snprintf(out, out_len, "127.0.0.1:%u", (unsigned)ntohs(sa.sin_port));
#ifdef _WIN32
    closesocket(fd);
#else
    close(fd);
#endif
    return 0;
}

struct cu_thread {
    void (*fn)(void*);
    void* arg;
#ifdef _WIN32
    HANDLE handle;
#else
    pthread_t handle;
#endif
};

#ifdef _WIN32
static unsigned __stdcall cu_trampoline(void* p) {
    cu_thread* t = (cu_thread*)p;
    t->fn(t->arg);
    return 0;
}
#else
static void* cu_trampoline(void* p) {
    cu_thread* t = (cu_thread*)p;
    t->fn(t->arg);
    return NULL;
}
#endif

cu_thread* cu_thread_start(void (*fn)(void*), void* arg) {
    cu_thread* t = (cu_thread*)calloc(1, sizeof *t);
    if (t == NULL) {
        return NULL;
    }
    t->fn = fn;
    t->arg = arg;
#ifdef _WIN32
    t->handle = (HANDLE)_beginthreadex(NULL, 0, cu_trampoline, t, 0, NULL);
    if (t->handle == 0) {
        free(t);
        return NULL;
    }
#else
    if (pthread_create(&t->handle, NULL, cu_trampoline, t) != 0) {
        free(t);
        return NULL;
    }
#endif
    return t;
}

void cu_thread_join(cu_thread* t) {
    if (t == NULL) {
        return;
    }
#ifdef _WIN32
    WaitForSingleObject(t->handle, INFINITE);
    CloseHandle(t->handle);
#else
    pthread_join(t->handle, NULL);
#endif
    free(t);
}

void cu_sleep_ms(unsigned ms) {
#ifdef _WIN32
    Sleep(ms);
#else
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
#endif
}

struct cu_mutex {
#ifdef _WIN32
    CRITICAL_SECTION cs;
#else
    pthread_mutex_t mu;
#endif
};

cu_mutex* cu_mutex_new(void) {
    cu_mutex* m = (cu_mutex*)calloc(1, sizeof *m);
    if (m == NULL) {
        return NULL;
    }
#ifdef _WIN32
    InitializeCriticalSection(&m->cs);
#else
    if (pthread_mutex_init(&m->mu, NULL) != 0) {
        free(m);
        return NULL;
    }
#endif
    return m;
}

void cu_mutex_lock(cu_mutex* m) {
#ifdef _WIN32
    EnterCriticalSection(&m->cs);
#else
    pthread_mutex_lock(&m->mu);
#endif
}

void cu_mutex_unlock(cu_mutex* m) {
#ifdef _WIN32
    LeaveCriticalSection(&m->cs);
#else
    pthread_mutex_unlock(&m->mu);
#endif
}

void cu_mutex_free(cu_mutex* m) {
    if (m == NULL) {
        return;
    }
#ifdef _WIN32
    DeleteCriticalSection(&m->cs);
#else
    pthread_mutex_destroy(&m->mu);
#endif
    free(m);
}

unsigned long cu_pid(void) {
#ifdef _WIN32
    return (unsigned long)GetCurrentProcessId();
#else
    return (unsigned long)getpid();
#endif
}

int cu_mkdir(const char* path) {
#ifdef _WIN32
    if (_mkdir(path) == 0) {
        return 0;
    }
#else
    if (mkdir(path, 0755) == 0) {
        return 0;
    }
#endif
    return cu_exists(path) ? 0 : -1;
}

int cu_write_file(const char* path, const void* data, size_t len) {
    FILE* f = fopen(path, "wb");
    size_t n;
    if (f == NULL) {
        return -1;
    }
    n = len ? fwrite(data, 1, len, f) : 0;
    if (fclose(f) != 0 || n != len) {
        return -1;
    }
    return 0;
}

unsigned char* cu_read_file(const char* path, size_t* out_len) {
    FILE* f = fopen(path, "rb");
    unsigned char* buf = NULL;
    size_t cap = 0, len = 0;
    if (f == NULL) {
        return NULL;
    }
    for (;;) {
        size_t n;
        if (len == cap) {
            unsigned char* grown;
            cap = cap ? cap * 2 : 4096;
            grown = (unsigned char*)realloc(buf, cap);
            if (grown == NULL) {
                free(buf);
                fclose(f);
                return NULL;
            }
            buf = grown;
        }
        n = fread(buf + len, 1, cap - len, f);
        len += n;
        if (n == 0) {
            break;
        }
    }
    fclose(f);
    *out_len = len;
    return buf;
}

int cu_exists(const char* path) {
#ifdef _WIN32
    struct _stat st;
    return _stat(path, &st) == 0;
#else
    struct stat st;
    return stat(path, &st) == 0;
#endif
}

/* ---- JSON results ---- */

static const char* cu_json_value(const char* json, const char* key) {
    char pat[96];
    const char* at;
    if (json == NULL) {
        return NULL;
    }
    snprintf(pat, sizeof pat, "\"%s\":", key);
    at = strstr(json, pat);
    if (at == NULL) {
        return NULL;
    }
    at += strlen(pat);
    while (*at == ' ') {
        at++;
    }
    return at;
}

int cu_json_u64(const char* json, const char* key, uint64_t* out) {
    const char* v = cu_json_value(json, key);
    uint64_t n = 0;
    if (v == NULL || *v < '0' || *v > '9') {
        return -1;
    }
    while (*v >= '0' && *v <= '9') {
        n = n * 10 + (uint64_t)(*v - '0');
        v++;
    }
    *out = n;
    return 0;
}

int cu_json_bool(const char* json, const char* key) {
    const char* v = cu_json_value(json, key);
    if (v == NULL) {
        return -1;
    }
    if (strncmp(v, "true", 4) == 0) {
        return 1;
    }
    if (strncmp(v, "false", 5) == 0) {
        return 0;
    }
    return -1;
}

int cu_json_has(const char* json, const char* key) {
    return cu_json_value(json, key) != NULL;
}

int cu_json_str(const char* from, const char* end, const char* key, char* out, size_t cap) {
    const char* v = cu_json_value(from, key);
    const char* close;
    size_t n;
    if (v == NULL || (end != NULL && v >= end) || *v != '"') {
        return -1;
    }
    v++;
    close = strchr(v, '"');
    if (close == NULL || (size_t)(close - v) + 1 > cap) {
        return -1;
    }
    n = (size_t)(close - v);
    memcpy(out, v, n);
    out[n] = '\0';
    return 0;
}

unsigned char* cu_distinct_chunks(size_t n, size_t size) {
    unsigned char* out = (unsigned char*)malloc(n * size);
    size_t i, j;
    if (out == NULL) {
        return NULL;
    }
    for (i = 0; i < n; i++) {
        const unsigned char pat[4] = {(unsigned char)(0x10 + i), (unsigned char)(i * 7), 0xA5,
                                      (unsigned char)(255 - i)};
        for (j = 0; j < size; j++) {
            out[i * size + j] = pat[j % 4];
        }
    }
    return out;
}

/* ---- Mesh ---- */

#define CU_BIND_ATTEMPTS 8

int cu_mesh_build(unsigned char seed_byte, net_meshnode_t** out, char* addr, size_t addr_len) {
    char seed[65];
    int i, attempt;
    for (i = 0; i < 32; i++) {
        snprintf(seed + i * 2, 3, "%02x", seed_byte);
    }
    for (attempt = 0; attempt < CU_BIND_ATTEMPTS; attempt++) {
        char cfg[512];
        if (cu_reserve_port(addr, addr_len) != 0) {
            return -1;
        }
        snprintf(cfg, sizeof cfg,
                 "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\",\"identity_seed_hex\":\"%s\"}", addr,
                 CU_PSK_HEX, seed);
        if (net_mesh_new(cfg, out) == 0) {
            return 0;
        }
    }
    return -1;
}

typedef struct {
    net_meshnode_t* responder;
    uint64_t initiator_id;
    int rc;
} cu_accept_arg;

static void cu_accept(void* p) {
    cu_accept_arg* a = (cu_accept_arg*)p;
    char* addr = NULL;
    size_t len = 0;
    a->rc = net_mesh_accept(a->responder, a->initiator_id, &addr, &len);
    net_free_string(addr);
}

int cu_mesh_handshake(net_meshnode_t* responder, net_meshnode_t* initiator,
                      const char* responder_addr) {
    char* pub = NULL;
    size_t pub_len = 0;
    cu_accept_arg a;
    cu_thread* t;
    int rc;
    if (net_mesh_public_key_hex(responder, &pub, &pub_len) != 0) {
        return -1;
    }
    a.responder = responder;
    a.initiator_id = net_mesh_node_id(initiator);
    a.rc = -1;
    t = cu_thread_start(cu_accept, &a);
    if (t == NULL) {
        net_free_string(pub);
        return -1;
    }
    /* Let the accept park before the initiator dials. */
    cu_sleep_ms(50);
    rc = net_mesh_connect(initiator, responder_addr, pub, net_mesh_node_id(responder));
    cu_thread_join(t);
    net_free_string(pub);
    return (rc == 0 && a.rc == 0) ? 0 : -1;
}
