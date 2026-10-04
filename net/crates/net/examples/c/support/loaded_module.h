/*
 * loaded_module.h — which library did this program actually load?
 *
 * Part of the C consumer runner, not a Net API (hence no `net_` prefix).
 * Every consumer program calls `clm_check_loaded_module` first, before any
 * Net function, with the Net functions it uses. For each one it finds the
 * address the program's imports resolved to, and the module containing it:
 *
 *   Linux    the function's address as the executable sees it (cross-checked
 *            against `dlsym(RTLD_DEFAULT)`), then `dladdr`. A preloaded or
 *            interposed definition resolves to the interposer, so the
 *            interposer is what gets reported.
 *   Windows  the function's slot in the executable's import address table,
 *            then GetModuleHandleExW(FROM_ADDRESS). A thunk's address is not
 *            used: it belongs to the executable.
 *
 * It never opens the expected library by path, which would inspect the right
 * module whatever the program called. All addresses must fall in one module;
 * the program then prints
 *
 *   NET-LOADED-MODULE: <path>
 *
 * If NET_EXPECTED_MODULE is set (the runner sets it to the staged library's
 * resolved path), a different module is refused here, so the program never
 * calls into it. The runner also compares the printed path's SHA-256 with the
 * bundle's PROVENANCE. On failure it prints
 * `NET-LOADED-MODULE-ERROR: <reason>` and returns non-zero. Functions are only
 * addressed, never called.
 *
 * See docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md,
 * "Loaded-library identity".
 */

#ifndef CLM_LOADED_MODULE_H
#define CLM_LOADED_MODULE_H

#include <stddef.h>

typedef void (*clm_fn_ptr)(void);

typedef struct {
    const char* name;
    clm_fn_ptr addr;
} clm_fn_t;

/* One entry per Net function the program uses: CLM_FN(net_version). */
#define CLM_FN(f) { #f, (clm_fn_ptr)(f) }

/* 0 when every function resolves into one module other than the executable
 * (and that module's path has been printed); non-zero otherwise. */
int clm_check_loaded_module(const clm_fn_t* fns, size_t count);

#endif /* CLM_LOADED_MODULE_H */
