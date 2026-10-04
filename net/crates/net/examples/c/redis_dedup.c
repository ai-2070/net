/*
 * redis_dedup.c — the consumer-side Redis Streams dedup helper, from C
 * (net.go.h).
 *
 * The helper needs no Redis: it is the in-process window a consumer keys
 * on each entry's dedup_id, so it is exercised here directly. Every check
 * is a documented return of net.go.h:
 *
 *   net_redis_dedup_new           capacity 0 selects the default (4096);
 *                                 never NULL
 *   net_redis_dedup_is_duplicate  0 new (and now marked seen), 1 duplicate,
 *                                 -1 NULL handle or id, -2 invalid UTF-8
 *   net_redis_dedup_len / _capacity / _is_empty, on a handle and on NULL
 *   net_redis_dedup_clear         after a consumer-group rebalance
 *   net_redis_dedup_free          NULL is a no-op
 *
 * A window of capacity 2 then shows the bound: the third distinct id
 * keeps the tracked count at the capacity.
 */

#include <stdio.h>
#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_redis_dedup_new),         CLM_FN(net_redis_dedup_free),
        CLM_FN(net_redis_dedup_is_duplicate), CLM_FN(net_redis_dedup_len),
        CLM_FN(net_redis_dedup_capacity),    CLM_FN(net_redis_dedup_is_empty),
        CLM_FN(net_redis_dedup_clear),
    };
    static const char invalid_utf8[] = "\xC3\x28"; /* a lead byte, then no continuation */
    net_redis_dedup_t* d;
    net_redis_dedup_t* small;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }

    d = net_redis_dedup_new(0);
    CU_CHECK("net_redis_dedup_new: never NULL", d != NULL);
    CU_CHECK_RC("net_redis_dedup_capacity: 0 selects the default 4096", net_redis_dedup_capacity(d), 4096);
    CU_CHECK_RC("net_redis_dedup_is_empty: a new helper is empty", net_redis_dedup_is_empty(d), 1);
    CU_CHECK_RC("net_redis_dedup_len: 0", net_redis_dedup_len(d), 0);

    CU_CHECK_RC("net_redis_dedup_is_duplicate: first sight is new",
                net_redis_dedup_is_duplicate(d, "1700000000000-0"), 0);
    CU_CHECK_RC("net_redis_dedup_is_duplicate: second sight is a duplicate",
                net_redis_dedup_is_duplicate(d, "1700000000000-0"), 1);
    CU_CHECK_RC("net_redis_dedup_is_duplicate: another id is new",
                net_redis_dedup_is_duplicate(d, "1700000000000-1"), 0);
    CU_CHECK_RC("net_redis_dedup_len: two distinct ids", net_redis_dedup_len(d), 2);
    CU_CHECK_RC("net_redis_dedup_is_empty: no longer empty", net_redis_dedup_is_empty(d), 0);

    CU_CHECK_RC("net_redis_dedup_is_duplicate: NULL id is -1", net_redis_dedup_is_duplicate(d, NULL), -1);
    CU_CHECK_RC("net_redis_dedup_is_duplicate: NULL handle is -1",
                net_redis_dedup_is_duplicate(NULL, "x"), -1);
    CU_CHECK_RC("net_redis_dedup_is_duplicate: invalid UTF-8 is -2",
                net_redis_dedup_is_duplicate(d, invalid_utf8), -2);
    CU_CHECK_RC("net_redis_dedup_len: a refused id is not tracked", net_redis_dedup_len(d), 2);

    net_redis_dedup_clear(d);
    CU_CHECK_RC("net_redis_dedup_clear: empties the window", net_redis_dedup_is_empty(d), 1);
    CU_CHECK_RC("net_redis_dedup_is_duplicate: a cleared id is new again",
                net_redis_dedup_is_duplicate(d, "1700000000000-0"), 0);

    CU_CHECK_RC("net_redis_dedup_len: NULL is 0", net_redis_dedup_len(NULL), 0);
    CU_CHECK_RC("net_redis_dedup_capacity: NULL is 0", net_redis_dedup_capacity(NULL), 0);
    CU_CHECK_RC("net_redis_dedup_is_empty: NULL is -1", net_redis_dedup_is_empty(NULL), -1);
    net_redis_dedup_clear(NULL);
    CU_CHECK("net_redis_dedup_clear: NULL is a no-op", 1);

    small = net_redis_dedup_new(2);
    CU_CHECK_RC("net_redis_dedup_capacity: as configured", net_redis_dedup_capacity(small), 2);
    CU_CHECK_RC("bounded window: a", net_redis_dedup_is_duplicate(small, "a"), 0);
    CU_CHECK_RC("bounded window: b", net_redis_dedup_is_duplicate(small, "b"), 0);
    CU_CHECK_RC("bounded window: c", net_redis_dedup_is_duplicate(small, "c"), 0);
    CU_CHECK_RC("bounded window: the tracked count stays at the capacity", net_redis_dedup_len(small), 2);

    net_redis_dedup_free(small);
    net_redis_dedup_free(d);
    net_redis_dedup_free(NULL);
    CU_CHECK("net_redis_dedup_free: NULL is a no-op", 1);
    return cu_finish();
}
