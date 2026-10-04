/*
 * loaded_module.c — see loaded_module.h for the contract.
 */

#if !defined(_WIN32) && !defined(_GNU_SOURCE)
#define _GNU_SOURCE /* dladdr, RTLD_DEFAULT, realpath */
#endif
#if defined(_WIN32) && !defined(_CRT_SECURE_NO_WARNINGS)
/* getenv and strcpy on bounded, locally sized buffers; MSVC's /W4 would
 * otherwise reject them under /WX. */
#define _CRT_SECURE_NO_WARNINGS
#endif

#include "loaded_module.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CLM_PATH_MAX 4096

static int clm_fail(const char* name, const char* reason) {
    printf("NET-LOADED-MODULE-ERROR: %s: %s\n", name ? name : "-", reason);
    fflush(stdout);
    return 1;
}

static int clm_same_path(const char* a, const char* b);

/* Print the module, then refuse it if the runner named another one. The
 * runner passes the staged library's resolved path in NET_EXPECTED_MODULE,
 * so a wrong module is refused here, before the program calls into it. The
 * runner separately checks the file's SHA-256. */
static int clm_report(const char* path) {
    const char* expected = getenv("NET_EXPECTED_MODULE");
    printf("NET-LOADED-MODULE: %s\n", path);
    fflush(stdout);
    if (expected && expected[0] && !clm_same_path(path, expected)) {
        printf("NET-LOADED-MODULE-ERROR: -: loaded %s, expected %s\n", path, expected);
        fflush(stdout);
        return 1;
    }
    return 0;
}

#ifdef _WIN32

#include <windows.h>

static int clm_same_path(const char* a, const char* b) {
    return _stricmp(a, b) == 0;
}

/* The address `name` was bound to in the executable's import address table,
 * or NULL if the executable does not import it by name. */
static void* clm_iat_target(const char* name) {
    unsigned char* base = (unsigned char*)GetModuleHandleW(NULL);
    IMAGE_DOS_HEADER* dos = (IMAGE_DOS_HEADER*)base;
    IMAGE_NT_HEADERS* nt = (IMAGE_NT_HEADERS*)(base + dos->e_lfanew);
    IMAGE_DATA_DIRECTORY dir =
        nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR* d;
    if (dir.VirtualAddress == 0) {
        return NULL;
    }
    for (d = (IMAGE_IMPORT_DESCRIPTOR*)(base + dir.VirtualAddress); d->Name; d++) {
        DWORD lookup = d->OriginalFirstThunk ? d->OriginalFirstThunk : d->FirstThunk;
        IMAGE_THUNK_DATA* names = (IMAGE_THUNK_DATA*)(base + lookup);
        IMAGE_THUNK_DATA* slots = (IMAGE_THUNK_DATA*)(base + d->FirstThunk);
        for (; names->u1.AddressOfData; names++, slots++) {
            IMAGE_IMPORT_BY_NAME* ibn;
            if (IMAGE_SNAP_BY_ORDINAL(names->u1.Ordinal)) {
                continue;
            }
            ibn = (IMAGE_IMPORT_BY_NAME*)(base + names->u1.AddressOfData);
            if (strcmp((const char*)ibn->Name, name) == 0) {
                return (void*)(ULONG_PTR)slots->u1.Function;
            }
        }
    }
    return NULL;
}

/* The module containing `addr`, and its path as UTF-8. 0 on success. */
static int clm_module_of(void* addr, HMODULE* module, char* out, size_t out_len) {
    wchar_t wide[CLM_PATH_MAX];
    DWORD n;
    if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                                GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                            (LPCWSTR)addr, module)) {
        return 1;
    }
    n = GetModuleFileNameW(*module, wide, CLM_PATH_MAX);
    if (n == 0 || n >= CLM_PATH_MAX) {
        return 1;
    }
    return WideCharToMultiByte(CP_UTF8, 0, wide, -1, out, (int)out_len, NULL, NULL) == 0;
}

int clm_check_loaded_module(const clm_fn_t* fns, size_t count) {
    HMODULE exe = GetModuleHandleW(NULL);
    HMODULE first = NULL;
    char first_path[CLM_PATH_MAX];
    size_t i;
    if (count == 0) {
        return clm_fail(NULL, "no functions to check");
    }
    for (i = 0; i < count; i++) {
        char path[CLM_PATH_MAX];
        HMODULE module = NULL;
        void* target = clm_iat_target(fns[i].name);
        if (target == NULL) {
            return clm_fail(fns[i].name, "not imported by name by this executable");
        }
        if (clm_module_of(target, &module, path, sizeof path) != 0) {
            return clm_fail(fns[i].name, "no module contains the bound address");
        }
        if (module == exe) {
            return clm_fail(fns[i].name, "bound inside the executable itself");
        }
        if (first == NULL) {
            first = module;
            strcpy(first_path, path);
        } else if (module != first) {
            printf("NET-LOADED-MODULE-ERROR: %s: resolves into %s, not %s\n",
                   fns[i].name, path, first_path);
            fflush(stdout);
            return 1;
        }
    }
    return clm_report(first_path);
}

#else /* ELF */

#include <dlfcn.h>

static int clm_same_path(const char* a, const char* b) {
    return strcmp(a, b) == 0;
}

/* An address inside the executable, to recognise it in dladdr's answers. */
static void clm_self(void) {}

int clm_check_loaded_module(const clm_fn_t* fns, size_t count) {
    char first_path[CLM_PATH_MAX] = "";
    Dl_info self;
    size_t i;
    if (count == 0) {
        return clm_fail(NULL, "no functions to check");
    }
    if (!dladdr((void*)clm_self, &self)) {
        return clm_fail(NULL, "dladdr cannot place the executable");
    }
    for (i = 0; i < count; i++) {
        Dl_info info;
        char path[CLM_PATH_MAX];
        void* addr = (void*)fns[i].addr;
        void* global = dlsym(RTLD_DEFAULT, fns[i].name);
        if (global == NULL) {
            return clm_fail(fns[i].name, "not found by dlsym(RTLD_DEFAULT)");
        }
        if (global != addr) {
            return clm_fail(fns[i].name,
                            "the executable's address differs from the global "
                            "definition (a PLT address? build with -fPIE)");
        }
        if (!dladdr(addr, &info) || !info.dli_fname || !info.dli_fname[0]) {
            return clm_fail(fns[i].name, "no module contains the address");
        }
        if (info.dli_fbase == self.dli_fbase) {
            return clm_fail(fns[i].name, "resolves inside the executable itself");
        }
        if (!realpath(info.dli_fname, path)) {
            return clm_fail(fns[i].name, "the module's path does not resolve");
        }
        if (!first_path[0]) {
            strcpy(first_path, path);
        } else if (strcmp(path, first_path) != 0) {
            printf("NET-LOADED-MODULE-ERROR: %s: resolves into %s, not %s\n",
                   fns[i].name, path, first_path);
            fflush(stdout);
            return 1;
        }
    }
    return clm_report(first_path);
}

#endif
