/*
 * rpc_no_free.c — a handler dispatcher registered without a deallocator is
 * refused, on every platform (C SDK plan, C4).
 *
 * net_rpc.h: net_rpc_set_handler_dispatcher returns -1 unless
 * net_rpc_set_callback_free has been called, because without it the
 * library has no way to release a handler's buffers. Both registrations are
 * process-wide and first-call-wins, so this needs a process in which the
 * deallocator was never registered: this one, on its own, not
 * rpc_callbacks.c.
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include "net_rpc.h"

#include "consumer_util.h"
#include "loaded_module.h"

static int dispatch(uint64_t handler_id, const uint8_t* req, size_t req_len, uint8_t** out_resp,
                    size_t* out_resp_len, char** out_err) {
    (void)handler_id, (void)req, (void)req_len, (void)out_resp, (void)out_resp_len, (void)out_err;
    return 1;
}

static int run(void) {
    CU_CHECK_RC("net_rpc_set_handler_dispatcher without a deallocator is -1",
                net_rpc_set_handler_dispatcher(dispatch), -1);
    CU_CHECK_RC("... and stays refused on a second try", net_rpc_set_handler_dispatcher(dispatch), -1);
    CU_CHECK_RC("net_rpc_set_callback_free: NULL is -1, and registers nothing",
                net_rpc_set_callback_free(NULL), -1);
    CU_CHECK_RC("net_rpc_set_handler_dispatcher: still refused after a NULL deallocator",
                net_rpc_set_handler_dispatcher(dispatch), -1);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_rpc_set_handler_dispatcher),
        CLM_FN(net_rpc_set_callback_free),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (run() != 0) {
        return 1;
    }
    return cu_finish();
}
