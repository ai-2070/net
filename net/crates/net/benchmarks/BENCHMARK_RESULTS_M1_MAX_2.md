     Running unittests src/bin/net-blob.rs (target/release/deps/net_blob-de7a148125f529de)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running benches/auth_guard.rs (target/release/deps/auth_guard-323ef34508d0bdd2)
Gnuplot not found, using plotters backend
auth_guard_check_fast_hit/single_thread
                        time:   [24.149 ns 24.246 ns 24.353 ns]
                        thrpt:  [41.063 Melem/s 41.243 Melem/s 41.409 Melem/s]
                 change:
                        time:   [+1.6965% +2.3538% +2.9044%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8224% −2.2997% −1.6682%]
                        Performance has regressed.

auth_guard_check_fast_miss/single_thread
                        time:   [3.7867 ns 3.7882 ns 3.7903 ns]
                        thrpt:  [263.83 Melem/s 263.98 Melem/s 264.08 Melem/s]
                 change:
                        time:   [−0.1591% +0.0301% +0.2145%] (p = 0.76 > 0.05)
                        thrpt:  [−0.2140% −0.0301% +0.1594%]
                        No change in performance detected.
Found 7 outliers among 50 measurements (14.00%)
  2 (4.00%) high mild
  5 (10.00%) high severe

auth_guard_check_fast_contended/eight_threads
                        time:   [30.835 ns 31.228 ns 31.600 ns]
                        thrpt:  [31.645 Melem/s 32.023 Melem/s 32.430 Melem/s]
                 change:
                        time:   [+3.8604% +5.0398% +6.2104%] (p = 0.00 < 0.05)
                        thrpt:  [−5.8472% −4.7980% −3.7169%]
                        Performance has regressed.
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) low mild

auth_guard_allow_channel/insert
                        time:   [165.85 ns 170.60 ns 175.63 ns]
                        thrpt:  [5.6937 Melem/s 5.8616 Melem/s 6.0296 Melem/s]
                 change:
                        time:   [+0.9477% +6.7096% +12.568%] (p = 0.03 < 0.05)
                        thrpt:  [−11.164% −6.2877% −0.9388%]
                        Change within noise threshold.
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.8199 ms 2.8217 ms 2.8238 ms]
                        change: [−0.9142% −0.3520% +0.0645%] (p = 0.19 > 0.05)
                        No change in performance detected.
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) high mild
  2 (4.00%) high severe

     Running benches/cortex.rs (target/release/deps/cortex-a73fb128244b1e22)
Gnuplot not found, using plotters backend
cortex_ingest/tasks_create
                        time:   [118.53 ns 119.75 ns 121.12 ns]
                        thrpt:  [8.2561 Melem/s 8.3506 Melem/s 8.4368 Melem/s]
                 change:
                        time:   [−9.0502% −0.4826% +8.4349%] (p = 0.92 > 0.05)
                        thrpt:  [−7.7788% +0.4850% +9.9507%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe
cortex_ingest/memories_store
                        time:   [301.87 ns 308.54 ns 316.43 ns]
                        thrpt:  [3.1603 Melem/s 3.2411 Melem/s 3.3126 Melem/s]
                 change:
                        time:   [−16.964% −5.6957% +6.8164%] (p = 0.40 > 0.05)
                        thrpt:  [−6.3814% +6.0397% +20.430%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  10 (10.00%) high mild
  4 (4.00%) high severe

cortex_fold_barrier/tasks_create_and_wait
                        time:   [5.6885 µs 5.6935 µs 5.6987 µs]
                        thrpt:  [175.48 Kelem/s 175.64 Kelem/s 175.79 Kelem/s]
                 change:
                        time:   [−1.0471% −0.3528% +0.3784%] (p = 0.34 > 0.05)
                        thrpt:  [−0.3770% +0.3540% +1.0582%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
cortex_fold_barrier/memories_store_and_wait
                        time:   [5.9460 µs 5.9527 µs 5.9597 µs]
                        thrpt:  [167.79 Kelem/s 167.99 Kelem/s 168.18 Kelem/s]
                 change:
                        time:   [−0.5949% +0.1381% +0.7990%] (p = 0.71 > 0.05)
                        thrpt:  [−0.7926% −0.1379% +0.5984%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe

cortex_query/tasks_find_many/100
                        time:   [2.1485 µs 2.1556 µs 2.1630 µs]
                        thrpt:  [46.232 Melem/s 46.391 Melem/s 46.543 Melem/s]
                 change:
                        time:   [−2.9506% −2.5854% −2.1878%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2367% +2.6541% +3.0403%]
                        Performance has improved.
cortex_query/tasks_count_where/100
                        time:   [164.89 ns 165.33 ns 166.03 ns]
                        thrpt:  [602.31 Melem/s 604.86 Melem/s 606.45 Melem/s]
                 change:
                        time:   [−0.5284% −0.2085% +0.1046%] (p = 0.20 > 0.05)
                        thrpt:  [−0.1045% +0.2090% +0.5312%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
cortex_query/tasks_find_unique/100
                        time:   [8.9749 ns 9.0237 ns 9.0794 ns]
                        thrpt:  [11.014 Gelem/s 11.082 Gelem/s 11.142 Gelem/s]
                 change:
                        time:   [−0.3415% +0.3524% +1.0912%] (p = 0.34 > 0.05)
                        thrpt:  [−1.0794% −0.3511% +0.3426%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
cortex_query/memories_find_many_tag/100
                        time:   [1.1204 µs 1.1236 µs 1.1271 µs]
                        thrpt:  [88.726 Melem/s 88.999 Melem/s 89.252 Melem/s]
                 change:
                        time:   [+1.5390% +1.8805% +2.2082%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1605% −1.8458% −1.5157%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
cortex_query/memories_count_where/100
                        time:   [771.03 ns 772.75 ns 774.57 ns]
                        thrpt:  [129.10 Melem/s 129.41 Melem/s 129.70 Melem/s]
                 change:
                        time:   [+2.9460% +3.3487% +3.7844%] (p = 0.00 < 0.05)
                        thrpt:  [−3.6464% −3.2402% −2.8617%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
cortex_query/tasks_find_many/1000
                        time:   [19.129 µs 19.180 µs 19.257 µs]
                        thrpt:  [51.930 Melem/s 52.137 Melem/s 52.277 Melem/s]
                 change:
                        time:   [−1.2460% −1.0046% −0.7426%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7481% +1.0148% +1.2617%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
cortex_query/tasks_count_where/1000
                        time:   [1.6310 µs 1.6316 µs 1.6323 µs]
                        thrpt:  [612.64 Melem/s 612.91 Melem/s 613.10 Melem/s]
                 change:
                        time:   [−0.5706% −0.4092% −0.2571%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2577% +0.4109% +0.5739%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
cortex_query/tasks_find_unique/1000
                        time:   [9.0205 ns 9.0765 ns 9.1357 ns]
                        thrpt:  [109.46 Gelem/s 110.18 Gelem/s 110.86 Gelem/s]
                 change:
                        time:   [+0.0951% +0.6203% +1.2100%] (p = 0.03 < 0.05)
                        thrpt:  [−1.1955% −0.6165% −0.0950%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
cortex_query/memories_find_many_tag/1000
                        time:   [13.236 µs 13.251 µs 13.264 µs]
                        thrpt:  [75.390 Melem/s 75.468 Melem/s 75.551 Melem/s]
                 change:
                        time:   [+2.6646% +2.9296% +3.2065%] (p = 0.00 < 0.05)
                        thrpt:  [−3.1069% −2.8462% −2.5955%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
cortex_query/memories_count_where/1000
                        time:   [11.268 µs 11.319 µs 11.379 µs]
                        thrpt:  [87.881 Melem/s 88.348 Melem/s 88.744 Melem/s]
                 change:
                        time:   [−1.8536% −1.1345% −0.4642%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4663% +1.1475% +1.8886%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
cortex_query/tasks_find_many/10000
                        time:   [219.61 µs 225.54 µs 232.08 µs]
                        thrpt:  [43.089 Melem/s 44.338 Melem/s 45.536 Melem/s]
                 change:
                        time:   [−18.908% −14.755% −10.352%] (p = 0.00 < 0.05)
                        thrpt:  [+11.548% +17.308% +23.316%]
                        Performance has improved.
cortex_query/tasks_count_where/10000
                        time:   [26.943 µs 28.316 µs 29.736 µs]
                        thrpt:  [336.29 Melem/s 353.16 Melem/s 371.16 Melem/s]
                 change:
                        time:   [−24.113% −20.786% −17.475%] (p = 0.00 < 0.05)
                        thrpt:  [+21.175% +26.240% +31.775%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
cortex_query/tasks_find_unique/10000
                        time:   [9.3474 ns 9.4329 ns 9.5214 ns]
                        thrpt:  [1050.3 Gelem/s 1060.1 Gelem/s 1069.8 Gelem/s]
                 change:
                        time:   [+3.9885% +4.7836% +5.7011%] (p = 0.00 < 0.05)
                        thrpt:  [−5.3936% −4.5652% −3.8355%]
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
cortex_query/memories_find_many_tag/10000
                        time:   [175.25 µs 175.67 µs 176.08 µs]
                        thrpt:  [56.792 Melem/s 56.924 Melem/s 57.062 Melem/s]
                 change:
                        time:   [+0.6546% +1.4901% +2.2854%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2343% −1.4682% −0.6504%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
cortex_query/memories_count_where/10000
                        time:   [152.39 µs 152.69 µs 153.00 µs]
                        thrpt:  [65.358 Melem/s 65.494 Melem/s 65.620 Melem/s]
                 change:
                        time:   [−1.4440% −0.6588% +0.0737%] (p = 0.09 > 0.05)
                        thrpt:  [−0.0737% +0.6632% +1.4652%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

cortex_snapshot/tasks_encode/100
                        time:   [3.2280 µs 3.2523 µs 3.2935 µs]
                        thrpt:  [30.363 Melem/s 30.748 Melem/s 30.979 Melem/s]
                 change:
                        time:   [−3.1096% −2.3067% −1.4230%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4435% +2.3612% +3.2094%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/memories_encode/100
                        time:   [5.5887 µs 5.6233 µs 5.6768 µs]
                        thrpt:  [17.615 Melem/s 17.783 Melem/s 17.893 Melem/s]
                 change:
                        time:   [+0.6069% +1.1523% +1.8358%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8027% −1.1392% −0.6033%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_3939/100
                        time:   [2.2260 µs 2.2365 µs 2.2475 µs]
                        thrpt:  [44.495 Melem/s 44.713 Melem/s 44.924 Melem/s]
                 change:
                        time:   [+0.7740% +1.1891% +1.7006%] (p = 0.00 < 0.05)
                        thrpt:  [−1.6722% −1.1751% −0.7680%]
                        Change within noise threshold.
cortex_snapshot/netdb_bundle_decode/100
                        time:   [2.2418 µs 2.2440 µs 2.2464 µs]
                        thrpt:  [44.516 Melem/s 44.564 Melem/s 44.607 Melem/s]
                 change:
                        time:   [−0.6181% −0.3829% −0.1477%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1479% +0.3844% +0.6219%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/tasks_encode/1000
                        time:   [30.378 µs 30.415 µs 30.451 µs]
                        thrpt:  [32.840 Melem/s 32.879 Melem/s 32.919 Melem/s]
                 change:
                        time:   [−0.9049% −0.5411% −0.1968%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1972% +0.5441% +0.9131%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/memories_encode/1000
                        time:   [56.183 µs 56.239 µs 56.303 µs]
                        thrpt:  [17.761 Melem/s 17.781 Melem/s 17.799 Melem/s]
                 change:
                        time:   [−0.5701% −0.2554% +0.0220%] (p = 0.10 > 0.05)
                        thrpt:  [−0.0220% +0.2561% +0.5733%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_48274/1000
                        time:   [22.627 µs 22.699 µs 22.763 µs]
                        thrpt:  [43.932 Melem/s 44.055 Melem/s 44.195 Melem/s]
                 change:
                        time:   [−0.7987% −0.3609% +0.0984%] (p = 0.12 > 0.05)
                        thrpt:  [−0.0983% +0.3622% +0.8052%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
cortex_snapshot/netdb_bundle_decode/1000
                        time:   [26.691 µs 26.846 µs 26.994 µs]
                        thrpt:  [37.045 Melem/s 37.249 Melem/s 37.465 Melem/s]
                 change:
                        time:   [+0.4895% +0.8507% +1.2556%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2400% −0.8435% −0.4871%]
                        Change within noise threshold.
Found 21 outliers among 100 measurements (21.00%)
  8 (8.00%) high mild
  13 (13.00%) high severe
cortex_snapshot/tasks_encode/10000
                        time:   [308.18 µs 315.07 µs 321.92 µs]
                        thrpt:  [31.063 Melem/s 31.739 Melem/s 32.449 Melem/s]
                 change:
                        time:   [−11.686% −8.7882% −5.6846%] (p = 0.00 < 0.05)
                        thrpt:  [+6.0272% +9.6349% +13.232%]
                        Performance has improved.
cortex_snapshot/memories_encode/10000
                        time:   [667.77 µs 687.45 µs 707.90 µs]
                        thrpt:  [14.126 Melem/s 14.547 Melem/s 14.975 Melem/s]
                 change:
                        time:   [+2.2691% +5.5570% +8.9786%] (p = 0.00 < 0.05)
                        thrpt:  [−8.2389% −5.2644% −2.2187%]
                        Performance has regressed.
cortex_snapshot/netdb_bundle_encode_bytes_511774/10000
                        time:   [248.65 µs 259.92 µs 272.49 µs]
                        thrpt:  [36.699 Melem/s 38.473 Melem/s 40.218 Melem/s]
                 change:
                        time:   [−8.2818% −2.9963% +2.4039%] (p = 0.29 > 0.05)
                        thrpt:  [−2.3475% +3.0888% +9.0296%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_decode/10000
                        time:   [278.66 µs 279.42 µs 280.34 µs]
                        thrpt:  [35.671 Melem/s 35.789 Melem/s 35.886 Melem/s]
                 change:
                        time:   [−15.786% −13.364% −10.862%] (p = 0.00 < 0.05)
                        thrpt:  [+12.186% +15.425% +18.746%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  2 (2.00%) high mild
  16 (16.00%) high severe

     Running benches/ingestion.rs (target/release/deps/ingestion-c07bd3637786d45d)
Gnuplot not found, using plotters backend
shard/ingest_raw/1024   time:   [46.450 ns 46.653 ns 46.898 ns]
                        thrpt:  [21.323 Melem/s 21.435 Melem/s 21.529 Melem/s]
                 change:
                        time:   [−1.9579% −1.6279% −1.2679%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2842% +1.6548% +1.9970%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  9 (9.00%) high severe
shard/ingest_raw_pop/1024
                        time:   [44.041 ns 44.247 ns 44.484 ns]
                        thrpt:  [22.480 Melem/s 22.600 Melem/s 22.706 Melem/s]
                 change:
                        time:   [+0.8489% +1.2714% +1.6730%] (p = 0.00 < 0.05)
                        thrpt:  [−1.6455% −1.2555% −0.8418%]
                        Change within noise threshold.
shard/ingest_raw/8192   time:   [46.292 ns 46.313 ns 46.344 ns]
                        thrpt:  [21.578 Melem/s 21.592 Melem/s 21.602 Melem/s]
                 change:
                        time:   [−0.5506% −0.2014% +0.1596%] (p = 0.29 > 0.05)
                        thrpt:  [−0.1593% +0.2018% +0.5536%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
shard/ingest_raw_pop/8192
                        time:   [43.648 ns 43.668 ns 43.694 ns]
                        thrpt:  [22.887 Melem/s 22.900 Melem/s 22.911 Melem/s]
                 change:
                        time:   [−0.3860% −0.1695% +0.0298%] (p = 0.11 > 0.05)
                        thrpt:  [−0.0298% +0.1698% +0.3875%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
shard/ingest_raw/65536  time:   [45.896 ns 46.038 ns 46.215 ns]
                        thrpt:  [21.638 Melem/s 21.721 Melem/s 21.788 Melem/s]
                 change:
                        time:   [−1.6730% −0.6053% +0.4994%] (p = 0.29 > 0.05)
                        thrpt:  [−0.4969% +0.6090% +1.7014%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
shard/ingest_raw_pop/65536
                        time:   [43.770 ns 43.789 ns 43.812 ns]
                        thrpt:  [22.825 Melem/s 22.837 Melem/s 22.847 Melem/s]
                 change:
                        time:   [−1.0054% −0.3933% +0.2159%] (p = 0.23 > 0.05)
                        thrpt:  [−0.2154% +0.3949% +1.0156%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
shard/ingest_raw/1048576
                        time:   [38.813 ns 39.240 ns 39.584 ns]
                        thrpt:  [25.263 Melem/s 25.484 Melem/s 25.764 Melem/s]
                 change:
                        time:   [−1.1976% +0.3426% +1.9863%] (p = 0.69 > 0.05)
                        thrpt:  [−1.9476% −0.3415% +1.2121%]
                        No change in performance detected.
shard/ingest_raw_pop/1048576
                        time:   [45.583 ns 45.871 ns 46.162 ns]
                        thrpt:  [21.663 Melem/s 21.801 Melem/s 21.938 Melem/s]
                 change:
                        time:   [−0.0996% +0.3974% +0.8544%] (p = 0.11 > 0.05)
                        thrpt:  [−0.8471% −0.3958% +0.0997%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe

timestamp/next          time:   [7.4706 ns 7.4744 ns 7.4785 ns]
                        thrpt:  [133.72 Melem/s 133.79 Melem/s 133.86 Melem/s]
                 change:
                        time:   [−0.0930% +0.0827% +0.2641%] (p = 0.38 > 0.05)
                        thrpt:  [−0.2634% −0.0826% +0.0931%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
timestamp/now_raw       time:   [620.75 ps 621.02 ps 621.37 ps]
                        thrpt:  [1.6094 Gelem/s 1.6103 Gelem/s 1.6110 Gelem/s]
                 change:
                        time:   [−0.2135% −0.0472% +0.1183%] (p = 0.58 > 0.05)
                        thrpt:  [−0.1182% +0.0472% +0.2139%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe

event/internal_event_new
                        time:   [286.24 ns 289.38 ns 292.47 ns]
                        thrpt:  [3.4192 Melem/s 3.4556 Melem/s 3.4936 Melem/s]
                 change:
                        time:   [−0.2797% +0.7418% +1.8190%] (p = 0.17 > 0.05)
                        thrpt:  [−1.7865% −0.7363% +0.2805%]
                        No change in performance detected.
event/internal_event_from_bytes
                        time:   [12.433 ns 12.439 ns 12.448 ns]
                        thrpt:  [80.336 Melem/s 80.392 Melem/s 80.433 Melem/s]
                 change:
                        time:   [−1.7186% −1.2056% −0.7355%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7410% +1.2203% +1.7487%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
event/json_creation     time:   [168.27 ns 170.39 ns 172.52 ns]
                        thrpt:  [5.7964 Melem/s 5.8688 Melem/s 5.9430 Melem/s]
                 change:
                        time:   [+0.0527% +1.1640% +2.1502%] (p = 0.03 < 0.05)
                        thrpt:  [−2.1050% −1.1506% −0.0527%]
                        Change within noise threshold.

batch/pop_batch_steady_state/100
                        time:   [3.8093 µs 3.8115 µs 3.8142 µs]
                        thrpt:  [26.218 Melem/s 26.236 Melem/s 26.252 Melem/s]
                 change:
                        time:   [−0.2262% +0.0105% +0.2122%] (p = 0.93 > 0.05)
                        thrpt:  [−0.2118% −0.0105% +0.2267%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
batch/pop_batch_steady_state/1000
                        time:   [37.979 µs 38.085 µs 38.272 µs]
                        thrpt:  [26.129 Melem/s 26.257 Melem/s 26.331 Melem/s]
                 change:
                        time:   [−0.2103% +0.0282% +0.2761%] (p = 0.82 > 0.05)
                        thrpt:  [−0.2753% −0.0282% +0.2108%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
batch/pop_batch_steady_state/10000
                        time:   [382.54 µs 382.71 µs 382.95 µs]
                        thrpt:  [26.113 Melem/s 26.129 Melem/s 26.141 Melem/s]
                 change:
                        time:   [−0.6475% −0.3587% −0.1161%] (p = 0.01 < 0.05)
                        thrpt:  [+0.1162% +0.3600% +0.6518%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe

event_bus_ingest_raw_concurrent/producers/1
                        time:   [522.64 µs 526.06 µs 529.76 µs]
                        thrpt:  [15.464 Melem/s 15.572 Melem/s 15.674 Melem/s]
                 change:
                        time:   [−2.7332% −0.8582% +1.1019%] (p = 0.40 > 0.05)
                        thrpt:  [−1.0899% +0.8656% +2.8100%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
event_bus_ingest_raw_concurrent/producers/2
                        time:   [829.82 µs 838.31 µs 846.58 µs]
                        thrpt:  [9.6765 Melem/s 9.7721 Melem/s 9.8720 Melem/s]
                 change:
                        time:   [−4.5844% −1.3318% +1.7204%] (p = 0.43 > 0.05)
                        thrpt:  [−1.6913% +1.3498% +4.8047%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
event_bus_ingest_raw_concurrent/producers/4
                        time:   [873.77 µs 886.38 µs 898.98 µs]
                        thrpt:  [9.1125 Melem/s 9.2421 Melem/s 9.3754 Melem/s]
                 change:
                        time:   [−14.378% −8.8366% −3.5971%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7313% +9.6931% +16.792%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking event_bus_ingest_raw_concurrent/producers/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, enable flat sampling, or reduce sample count to 50.
event_bus_ingest_raw_concurrent/producers/8
                        time:   [1.4867 ms 1.5192 ms 1.5510 ms]
                        thrpt:  [5.2816 Melem/s 5.3924 Melem/s 5.5101 Melem/s]
                 change:
                        time:   [−6.1879% +0.7548% +8.5497%] (p = 0.85 > 0.05)
                        thrpt:  [−7.8763% −0.7492% +6.5961%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

     Running benches/mesh.rs (target/release/deps/mesh-7942c8ee297881cd)
Gnuplot not found, using plotters backend
mesh_reroute/triangle_failure
                        time:   [7.4626 µs 7.5698 µs 7.6753 µs]
                        thrpt:  [130.29 Kelem/s 132.10 Kelem/s 134.00 Kelem/s]
                 change:
                        time:   [+1.1262% +2.6398% +4.3218%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1428% −2.5719% −1.1136%]
                        Performance has regressed.
mesh_reroute/10_peers_10_routes
                        time:   [40.558 µs 40.944 µs 41.343 µs]
                        thrpt:  [24.188 Kelem/s 24.424 Kelem/s 24.656 Kelem/s]
                 change:
                        time:   [−0.0010% +1.0217% +2.0425%] (p = 0.04 < 0.05)
                        thrpt:  [−2.0016% −1.0113% +0.0010%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
mesh_reroute/50_peers_100_routes
                        time:   [412.06 µs 413.87 µs 415.78 µs]
                        thrpt:  [2.4051 Kelem/s 2.4162 Kelem/s 2.4268 Kelem/s]
                 change:
                        time:   [−1.4208% −0.8654% −0.3113%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3122% +0.8730% +1.4413%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

mesh_proximity/on_pingwave_new
                        time:   [171.94 ns 174.03 ns 176.37 ns]
                        thrpt:  [5.6700 Melem/s 5.7463 Melem/s 5.8160 Melem/s]
                 change:
                        time:   [−3.1506% +1.2925% +5.4670%] (p = 0.56 > 0.05)
                        thrpt:  [−5.1836% −1.2760% +3.2531%]
                        No change in performance detected.
Found 26 outliers among 100 measurements (26.00%)
  8 (8.00%) low severe
  6 (6.00%) low mild
  8 (8.00%) high mild
  4 (4.00%) high severe
mesh_proximity/on_pingwave_dedup
                        time:   [69.658 ns 69.689 ns 69.738 ns]
                        thrpt:  [14.339 Melem/s 14.349 Melem/s 14.356 Melem/s]
                 change:
                        time:   [−0.9795% −0.6329% −0.3192%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3202% +0.6370% +0.9892%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  11 (11.00%) high severe
mesh_proximity/pingwave_serialize
                        time:   [1.9821 ns 1.9892 ns 1.9976 ns]
                        thrpt:  [500.61 Melem/s 502.73 Melem/s 504.52 Melem/s]
                 change:
                        time:   [−0.0084% +0.3919% +0.8195%] (p = 0.07 > 0.05)
                        thrpt:  [−0.8129% −0.3904% +0.0084%]
                        No change in performance detected.
mesh_proximity/pingwave_deserialize
                        time:   [2.2365 ns 2.2387 ns 2.2407 ns]
                        thrpt:  [446.29 Melem/s 446.69 Melem/s 447.13 Melem/s]
                 change:
                        time:   [−0.4360% −0.1903% +0.0411%] (p = 0.12 > 0.05)
                        thrpt:  [−0.0411% +0.1906% +0.4379%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low severe
  3 (3.00%) high mild
  1 (1.00%) high severe
mesh_proximity/node_count
                        time:   [310.36 ps 310.49 ps 310.71 ps]
                        thrpt:  [3.2184 Gelem/s 3.2207 Gelem/s 3.2221 Gelem/s]
                 change:
                        time:   [−0.2346% −0.0844% +0.0839%] (p = 0.30 > 0.05)
                        thrpt:  [−0.0838% +0.0845% +0.2351%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
mesh_proximity/all_nodes_100
                        time:   [4.6786 µs 4.7037 µs 4.7284 µs]
                        thrpt:  [211.49 Kelem/s 212.60 Kelem/s 213.74 Kelem/s]
                 change:
                        time:   [+0.6762% +1.2323% +1.8056%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7736% −1.2173% −0.6717%]
                        Change within noise threshold.

mesh_dispatch/classify_direct
                        time:   [621.69 ps 624.08 ps 626.97 ps]
                        thrpt:  [1.5950 Gelem/s 1.6024 Gelem/s 1.6085 Gelem/s]
                 change:
                        time:   [+0.0051% +0.2685% +0.6009%] (p = 0.06 > 0.05)
                        thrpt:  [−0.5973% −0.2678% −0.0051%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) high mild
  12 (12.00%) high severe
mesh_dispatch/classify_routed
                        time:   [442.14 ps 442.32 ps 442.59 ps]
                        thrpt:  [2.2594 Gelem/s 2.2608 Gelem/s 2.2617 Gelem/s]
                 change:
                        time:   [−0.6621% −0.3594% −0.0777%] (p = 0.01 < 0.05)
                        thrpt:  [+0.0778% +0.3607% +0.6665%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  3 (3.00%) high mild
  7 (7.00%) high severe
mesh_dispatch/classify_pingwave
                        time:   [313.39 ps 314.60 ps 315.99 ps]
                        thrpt:  [3.1647 Gelem/s 3.1787 Gelem/s 3.1910 Gelem/s]
                 change:
                        time:   [+1.0576% +1.6387% +2.1881%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1412% −1.6122% −1.0465%]
                        Performance has regressed.

mesh_routing/lookup_hit time:   [14.977 ns 14.996 ns 15.016 ns]
                        thrpt:  [66.597 Melem/s 66.686 Melem/s 66.769 Melem/s]
                 change:
                        time:   [−1.2052% −0.6540% −0.2259%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2264% +0.6583% +1.2200%]
                        Change within noise threshold.
Found 24 outliers among 100 measurements (24.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  16 (16.00%) high mild
  1 (1.00%) high severe
mesh_routing/lookup_miss
                        time:   [14.480 ns 14.519 ns 14.556 ns]
                        thrpt:  [68.701 Melem/s 68.874 Melem/s 69.062 Melem/s]
                 change:
                        time:   [−4.8287% −4.3589% −3.9110%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0702% +4.5576% +5.0737%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  6 (6.00%) low severe
  7 (7.00%) low mild
  3 (3.00%) high mild
mesh_routing/is_local   time:   [311.05 ps 311.50 ps 312.01 ps]
                        thrpt:  [3.2051 Gelem/s 3.2102 Gelem/s 3.2149 Gelem/s]
                 change:
                        time:   [−0.7563% −0.4040% −0.0509%] (p = 0.03 < 0.05)
                        thrpt:  [+0.0509% +0.4056% +0.7620%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
mesh_routing/all_routes/10
                        time:   [1.7485 µs 1.7504 µs 1.7526 µs]
                        thrpt:  [570.57 Kelem/s 571.31 Kelem/s 571.93 Kelem/s]
                 change:
                        time:   [−0.1026% +0.2003% +0.4956%] (p = 0.18 > 0.05)
                        thrpt:  [−0.4931% −0.1999% +0.1027%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
mesh_routing/all_routes/100
                        time:   [2.6408 µs 2.6554 µs 2.6716 µs]
                        thrpt:  [374.31 Kelem/s 376.59 Kelem/s 378.68 Kelem/s]
                 change:
                        time:   [−3.2972% −2.9218% −2.4975%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5615% +3.0097% +3.4097%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
mesh_routing/all_routes/1000
                        time:   [12.529 µs 12.604 µs 12.679 µs]
                        thrpt:  [78.870 Kelem/s 79.343 Kelem/s 79.813 Kelem/s]
                 change:
                        time:   [−1.9510% −1.2690% −0.6035%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6072% +1.2854% +1.9898%]
                        Change within noise threshold.
mesh_routing/add_route  time:   [44.376 ns 45.204 ns 45.932 ns]
                        thrpt:  [21.771 Melem/s 22.122 Melem/s 22.535 Melem/s]
                 change:
                        time:   [−5.4094% −2.9487% −0.6731%] (p = 0.02 < 0.05)
                        thrpt:  [+0.6776% +3.0383% +5.7187%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) low severe
  6 (6.00%) low mild

     Running benches/net.rs (target/release/deps/net-064190ef7fb92ec8)
Gnuplot not found, using plotters backend
net_header/serialize    time:   [2.1903 ns 2.1909 ns 2.1915 ns]
                        thrpt:  [456.30 Melem/s 456.44 Melem/s 456.55 Melem/s]
                 change:
                        time:   [−0.2251% −0.0459% +0.1140%] (p = 0.63 > 0.05)
                        thrpt:  [−0.1139% +0.0460% +0.2256%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
net_header/deserialize  time:   [2.3482 ns 2.3487 ns 2.3491 ns]
                        thrpt:  [425.69 Melem/s 425.77 Melem/s 425.85 Melem/s]
                 change:
                        time:   [−0.4492% −0.1914% +0.0552%] (p = 0.14 > 0.05)
                        thrpt:  [−0.0552% +0.1918% +0.4512%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
net_header/roundtrip    time:   [2.3486 ns 2.3496 ns 2.3508 ns]
                        thrpt:  [425.38 Melem/s 425.61 Melem/s 425.79 Melem/s]
                 change:
                        time:   [−0.4067% −0.1463% +0.0747%] (p = 0.25 > 0.05)
                        thrpt:  [−0.0747% +0.1465% +0.4083%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

net_event_frame/write_single/64
                        time:   [21.473 ns 21.494 ns 21.520 ns]
                        thrpt:  [2.7698 GiB/s 2.7731 GiB/s 2.7758 GiB/s]
                 change:
                        time:   [−0.0447% +0.1371% +0.3175%] (p = 0.15 > 0.05)
                        thrpt:  [−0.3165% −0.1369% +0.0447%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
net_event_frame/write_single_reused/64
                        time:   [2.5472 ns 2.5637 ns 2.5816 ns]
                        thrpt:  [23.088 GiB/s 23.249 GiB/s 23.400 GiB/s]
                 change:
                        time:   [−0.0545% +0.3660% +0.7795%] (p = 0.08 > 0.05)
                        thrpt:  [−0.7735% −0.3647% +0.0545%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
net_event_frame/write_single/256
                        time:   [46.047 ns 46.699 ns 47.386 ns]
                        thrpt:  [5.0315 GiB/s 5.1054 GiB/s 5.1777 GiB/s]
                 change:
                        time:   [−1.7556% −0.0679% +1.6402%] (p = 0.94 > 0.05)
                        thrpt:  [−1.6137% +0.0679% +1.7870%]
                        No change in performance detected.
net_event_frame/write_single_reused/256
                        time:   [5.2757 ns 5.2779 ns 5.2808 ns]
                        thrpt:  [45.148 GiB/s 45.173 GiB/s 45.192 GiB/s]
                 change:
                        time:   [−0.2022% −0.0053% +0.1836%] (p = 0.96 > 0.05)
                        thrpt:  [−0.1833% +0.0053% +0.2026%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single/1024
                        time:   [33.961 ns 34.028 ns 34.102 ns]
                        thrpt:  [27.965 GiB/s 28.026 GiB/s 28.081 GiB/s]
                 change:
                        time:   [−0.6949% −0.3364% +0.0230%] (p = 0.06 > 0.05)
                        thrpt:  [−0.0230% +0.3375% +0.6997%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  10 (10.00%) high mild
  2 (2.00%) high severe
net_event_frame/write_single_reused/1024
                        time:   [14.604 ns 14.626 ns 14.653 ns]
                        thrpt:  [65.083 GiB/s 65.206 GiB/s 65.303 GiB/s]
                 change:
                        time:   [−21.126% −16.603% −11.972%] (p = 0.00 < 0.05)
                        thrpt:  [+13.600% +19.909% +26.785%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
net_event_frame/write_single/4096
                        time:   [75.628 ns 76.349 ns 77.128 ns]
                        thrpt:  [49.460 GiB/s 49.964 GiB/s 50.440 GiB/s]
                 change:
                        time:   [−0.0476% +1.9455% +3.7385%] (p = 0.05 < 0.05)
                        thrpt:  [−3.6037% −1.9084% +0.0476%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
net_event_frame/write_single_reused/4096
                        time:   [54.800 ns 55.918 ns 57.198 ns]
                        thrpt:  [66.692 GiB/s 68.219 GiB/s 69.611 GiB/s]
                 change:
                        time:   [+0.0438% +1.9020% +3.6920%] (p = 0.05 < 0.05)
                        thrpt:  [−3.5605% −1.8665% −0.0437%]
                        Change within noise threshold.
Found 18 outliers among 100 measurements (18.00%)
  18 (18.00%) high severe
net_event_frame/write_batch/1
                        time:   [21.531 ns 21.556 ns 21.586 ns]
                        thrpt:  [2.7612 GiB/s 2.7651 GiB/s 2.7683 GiB/s]
                 change:
                        time:   [−0.1140% +0.1044% +0.3474%] (p = 0.39 > 0.05)
                        thrpt:  [−0.3462% −0.1043% +0.1141%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
net_event_frame/write_batch/10
                        time:   [67.834 ns 68.435 ns 69.144 ns]
                        thrpt:  [8.6204 GiB/s 8.7097 GiB/s 8.7869 GiB/s]
                 change:
                        time:   [−3.3913% −2.3496% −1.2932%] (p = 0.00 < 0.05)
                        thrpt:  [+1.3101% +2.4061% +3.5103%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  15 (15.00%) high mild
net_event_frame/write_batch/50
                        time:   [146.11 ns 146.33 ns 146.58 ns]
                        thrpt:  [20.331 GiB/s 20.366 GiB/s 20.398 GiB/s]
                 change:
                        time:   [−0.3898% +0.0517% +0.4021%] (p = 0.82 > 0.05)
                        thrpt:  [−0.4005% −0.0517% +0.3913%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
net_event_frame/write_batch/100
                        time:   [271.69 ns 272.47 ns 273.28 ns]
                        thrpt:  [21.811 GiB/s 21.876 GiB/s 21.938 GiB/s]
                 change:
                        time:   [−0.0391% +0.2204% +0.4672%] (p = 0.09 > 0.05)
                        thrpt:  [−0.4650% −0.2199% +0.0391%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
net_event_frame/read_batch_10
                        time:   [136.79 ns 137.77 ns 138.70 ns]
                        thrpt:  [72.100 Melem/s 72.587 Melem/s 73.107 Melem/s]
                 change:
                        time:   [−2.1306% −1.1214% −0.1707%] (p = 0.03 < 0.05)
                        thrpt:  [+0.1710% +1.1341% +2.1770%]
                        Change within noise threshold.

net_packet_pool/get_return/16
                        time:   [36.925 ns 36.998 ns 37.074 ns]
                        thrpt:  [26.973 Melem/s 27.029 Melem/s 27.082 Melem/s]
                 change:
                        time:   [−2.7578% −2.4462% −2.1312%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1777% +2.5075% +2.8360%]
                        Performance has improved.
Found 19 outliers among 100 measurements (19.00%)
  15 (15.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
net_packet_pool/get_return/64
                        time:   [37.029 ns 37.106 ns 37.184 ns]
                        thrpt:  [26.894 Melem/s 26.950 Melem/s 27.006 Melem/s]
                 change:
                        time:   [−2.0625% −1.6528% −1.2805%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2971% +1.6806% +2.1059%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
net_packet_pool/get_return/256
                        time:   [36.610 ns 36.665 ns 36.722 ns]
                        thrpt:  [27.231 Melem/s 27.274 Melem/s 27.315 Melem/s]
                 change:
                        time:   [−2.5937% −2.0855% −1.6745%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7031% +2.1299% +2.6628%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

net_packet_build/build_packet/1
                        time:   [298.87 ns 299.12 ns 299.41 ns]
                        thrpt:  [203.85 MiB/s 204.05 MiB/s 204.22 MiB/s]
                 change:
                        time:   [−13.476% −13.192% −12.894%] (p = 0.00 < 0.05)
                        thrpt:  [+14.803% +15.197% +15.574%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
net_packet_build/build_packet/10
                        time:   [708.11 ns 708.38 ns 708.70 ns]
                        thrpt:  [861.22 MiB/s 861.61 MiB/s 861.95 MiB/s]
                 change:
                        time:   [−7.9426% −7.4334% −6.9914%] (p = 0.00 < 0.05)
                        thrpt:  [+7.5170% +8.0303% +8.6279%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
net_packet_build/build_packet/50
                        time:   [2.4331 µs 2.4421 µs 2.4537 µs]
                        thrpt:  [1.2146 GiB/s 1.2203 GiB/s 1.2249 GiB/s]
                 change:
                        time:   [−2.2342% −1.7592% −1.2517%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2676% +1.7907% +2.2853%]
                        Performance has improved.
Found 19 outliers among 100 measurements (19.00%)
  4 (4.00%) high mild
  15 (15.00%) high severe

net_encryption/encrypt/64
                        time:   [299.27 ns 301.09 ns 303.38 ns]
                        thrpt:  [201.18 MiB/s 202.71 MiB/s 203.94 MiB/s]
                 change:
                        time:   [−11.691% −11.218% −10.633%] (p = 0.00 < 0.05)
                        thrpt:  [+11.898% +12.636% +13.238%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
net_encryption/encrypt/256
                        time:   [476.58 ns 476.74 ns 476.95 ns]
                        thrpt:  [511.88 MiB/s 512.10 MiB/s 512.28 MiB/s]
                 change:
                        time:   [−6.8188% −6.5407% −6.2995%] (p = 0.00 < 0.05)
                        thrpt:  [+6.7230% +6.9984% +7.3178%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
net_encryption/encrypt/1024
                        time:   [907.84 ns 908.20 ns 908.67 ns]
                        thrpt:  [1.0495 GiB/s 1.0501 GiB/s 1.0505 GiB/s]
                 change:
                        time:   [−4.1887% −3.8861% −3.6432%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7810% +4.0432% +4.3718%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
net_encryption/encrypt/4096
                        time:   [2.8859 µs 2.8872 µs 2.8893 µs]
                        thrpt:  [1.3203 GiB/s 1.3212 GiB/s 1.3218 GiB/s]
                 change:
                        time:   [−1.3503% −1.0904% −0.8294%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8363% +1.1024% +1.3687%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
net_encryption/raw_aead/64
                        time:   [354.27 ns 364.52 ns 375.87 ns]
                        thrpt:  [162.38 MiB/s 167.44 MiB/s 172.28 MiB/s]
                 change:
                        time:   [−2.1868% +0.5683% +3.3783%] (p = 0.69 > 0.05)
                        thrpt:  [−3.2679% −0.5651% +2.2357%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) high mild
  16 (16.00%) high severe
net_encryption/raw_aead/256
                        time:   [753.60 ns 762.46 ns 772.37 ns]
                        thrpt:  [316.09 MiB/s 320.20 MiB/s 323.97 MiB/s]
                 change:
                        time:   [−0.1809% +0.9437% +2.1077%] (p = 0.11 > 0.05)
                        thrpt:  [−2.0642% −0.9349% +0.1812%]
                        No change in performance detected.
net_encryption/raw_aead/1024
                        time:   [2.3877 µs 2.4041 µs 2.4223 µs]
                        thrpt:  [403.15 MiB/s 406.20 MiB/s 409.00 MiB/s]
                 change:
                        time:   [−0.9925% −0.3730% +0.2309%] (p = 0.24 > 0.05)
                        thrpt:  [−0.2303% +0.3744% +1.0024%]
                        No change in performance detected.
Found 21 outliers among 100 measurements (21.00%)
  18 (18.00%) high mild
  3 (3.00%) high severe
net_encryption/raw_aead/4096
                        time:   [8.9570 µs 9.0076 µs 9.0625 µs]
                        thrpt:  [431.04 MiB/s 433.66 MiB/s 436.11 MiB/s]
                 change:
                        time:   [+0.3825% +0.7934% +1.2551%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2396% −0.7871% −0.3810%]
                        Change within noise threshold.
net_encryption/raw_ring/64
                        time:   [236.24 ns 236.33 ns 236.45 ns]
                        thrpt:  [258.13 MiB/s 258.26 MiB/s 258.36 MiB/s]
                 change:
                        time:   [−0.2696% −0.1092% +0.0319%] (p = 0.17 > 0.05)
                        thrpt:  [−0.0319% +0.1093% +0.2703%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
net_encryption/raw_ring/256
                        time:   [294.01 ns 294.14 ns 294.35 ns]
                        thrpt:  [829.42 MiB/s 830.01 MiB/s 830.39 MiB/s]
                 change:
                        time:   [−0.4028% −0.1495% +0.0713%] (p = 0.23 > 0.05)
                        thrpt:  [−0.0712% +0.1497% +0.4044%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
net_encryption/raw_ring/1024
                        time:   [822.65 ns 823.30 ns 824.10 ns]
                        thrpt:  [1.1572 GiB/s 1.1584 GiB/s 1.1593 GiB/s]
                 change:
                        time:   [−3.3448% −2.6063% −1.9063%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9433% +2.6761% +3.4605%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
net_encryption/raw_ring/4096
                        time:   [2.6211 µs 2.6224 µs 2.6241 µs]
                        thrpt:  [1.4537 GiB/s 1.4547 GiB/s 1.4554 GiB/s]
                 change:
                        time:   [−0.1784% −0.0178% +0.1573%] (p = 0.85 > 0.05)
                        thrpt:  [−0.1571% +0.0178% +0.1788%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe

net_keypair/generate    time:   [12.441 µs 12.456 µs 12.474 µs]
                        thrpt:  [80.167 Kelem/s 80.285 Kelem/s 80.377 Kelem/s]
                 change:
                        time:   [−0.0758% +0.1393% +0.3728%] (p = 0.23 > 0.05)
                        thrpt:  [−0.3714% −0.1391% +0.0759%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) high mild
  13 (13.00%) high severe

net_aad/generate        time:   [1.8626 ns 1.8632 ns 1.8640 ns]
                        thrpt:  [536.48 Melem/s 536.70 Melem/s 536.89 Melem/s]
                 change:
                        time:   [−0.1763% −0.0113% +0.1545%] (p = 0.90 > 0.05)
                        thrpt:  [−0.1542% +0.0113% +0.1766%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe

pool_comparison/shared_pool_get_return
                        time:   [47.547 ns 47.648 ns 47.756 ns]
                        thrpt:  [20.940 Melem/s 20.987 Melem/s 21.032 Melem/s]
                 change:
                        time:   [+26.766% +27.174% +27.582%] (p = 0.00 < 0.05)
                        thrpt:  [−21.619% −21.368% −21.114%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
pool_comparison/thread_local_pool_get_return
                        time:   [102.64 ns 104.99 ns 107.90 ns]
                        thrpt:  [9.2676 Melem/s 9.5243 Melem/s 9.7429 Melem/s]
                 change:
                        time:   [+23.001% +25.730% +28.524%] (p = 0.00 < 0.05)
                        thrpt:  [−22.193% −20.464% −18.700%]
                        Performance has regressed.
pool_comparison/shared_pool_10x
                        time:   [348.47 ns 349.64 ns 350.97 ns]
                        thrpt:  [2.8492 Melem/s 2.8601 Melem/s 2.8697 Melem/s]
                 change:
                        time:   [−1.9695% −1.7167% −1.4918%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5144% +1.7467% +2.0090%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) high mild
  13 (13.00%) high severe
pool_comparison/thread_local_pool_10x
                        time:   [1.1114 µs 1.1130 µs 1.1149 µs]
                        thrpt:  [896.97 Kelem/s 898.47 Kelem/s 899.77 Kelem/s]
                 change:
                        time:   [+16.097% +16.475% +16.866%] (p = 0.00 < 0.05)
                        thrpt:  [−14.432% −14.145% −13.865%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

cipher_comparison/shared_pool/64
                        time:   [297.47 ns 297.93 ns 298.52 ns]
                        thrpt:  [204.46 MiB/s 204.86 MiB/s 205.18 MiB/s]
                 change:
                        time:   [−11.729% −11.434% −11.094%] (p = 0.00 < 0.05)
                        thrpt:  [+12.478% +12.910% +13.287%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
cipher_comparison/fast_chacha20/64
                        time:   [352.32 ns 353.85 ns 355.53 ns]
                        thrpt:  [171.67 MiB/s 172.49 MiB/s 173.24 MiB/s]
                 change:
                        time:   [−11.721% −11.419% −11.085%] (p = 0.00 < 0.05)
                        thrpt:  [+12.467% +12.891% +13.277%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
cipher_comparison/shared_pool/256
                        time:   [476.62 ns 476.80 ns 477.01 ns]
                        thrpt:  [511.81 MiB/s 512.05 MiB/s 512.23 MiB/s]
                 change:
                        time:   [−7.1345% −6.5685% −6.1993%] (p = 0.00 < 0.05)
                        thrpt:  [+6.6090% +7.0303% +7.6826%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/256
                        time:   [515.34 ns 515.62 ns 515.93 ns]
                        thrpt:  [473.20 MiB/s 473.49 MiB/s 473.75 MiB/s]
                 change:
                        time:   [−9.7328% −9.4810% −9.2264%] (p = 0.00 < 0.05)
                        thrpt:  [+10.164% +10.474% +10.782%]
                        Performance has improved.
Found 19 outliers among 100 measurements (19.00%)
  3 (3.00%) high mild
  16 (16.00%) high severe
cipher_comparison/shared_pool/1024
                        time:   [908.12 ns 908.66 ns 909.42 ns]
                        thrpt:  [1.0487 GiB/s 1.0495 GiB/s 1.0502 GiB/s]
                 change:
                        time:   [−4.0298% −3.8202% −3.5984%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7327% +3.9720% +4.1990%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
cipher_comparison/fast_chacha20/1024
                        time:   [943.94 ns 944.40 ns 945.10 ns]
                        thrpt:  [1.0091 GiB/s 1.0098 GiB/s 1.0103 GiB/s]
                 change:
                        time:   [−5.8404% −5.5888% −5.3424%] (p = 0.00 < 0.05)
                        thrpt:  [+5.6439% +5.9196% +6.2027%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  8 (8.00%) high mild
  9 (9.00%) high severe
cipher_comparison/shared_pool/4096
                        time:   [2.8737 µs 2.8750 µs 2.8768 µs]
                        thrpt:  [1.3260 GiB/s 1.3269 GiB/s 1.3275 GiB/s]
                 change:
                        time:   [−2.2927% −1.9766% −1.6563%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6842% +2.0164% +2.3465%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
cipher_comparison/fast_chacha20/4096
                        time:   [2.8883 µs 2.8899 µs 2.8923 µs]
                        thrpt:  [1.3189 GiB/s 1.3200 GiB/s 1.3208 GiB/s]
                 change:
                        time:   [−2.3312% −2.0612% −1.8077%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8410% +2.1046% +2.3869%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe

adaptive_batcher/optimal_size
                        time:   [981.58 ps 986.72 ps 991.83 ps]
                        thrpt:  [1.0082 Gelem/s 1.0135 Gelem/s 1.0188 Gelem/s]
                 change:
                        time:   [+0.5529% +0.8772% +1.2158%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2012% −0.8696% −0.5499%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) high mild
adaptive_batcher/record time:   [3.8585 ns 3.8605 ns 3.8638 ns]
                        thrpt:  [258.81 Melem/s 259.03 Melem/s 259.17 Melem/s]
                 change:
                        time:   [−0.1369% −0.0049% +0.1503%] (p = 0.96 > 0.05)
                        thrpt:  [−0.1500% +0.0049% +0.1371%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
adaptive_batcher/full_cycle
                        time:   [4.3708 ns 4.3735 ns 4.3775 ns]
                        thrpt:  [228.44 Melem/s 228.65 Melem/s 228.79 Melem/s]
                 change:
                        time:   [−0.2121% −0.0329% +0.1503%] (p = 0.72 > 0.05)
                        thrpt:  [−0.1501% +0.0329% +0.2126%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

e2e_packet_build/shared_pool_50_events
                        time:   [2.4257 µs 2.4268 µs 2.4283 µs]
                        thrpt:  [1.2273 GiB/s 1.2281 GiB/s 1.2286 GiB/s]
                 change:
                        time:   [−2.3696% −2.0790% −1.7829%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8153% +2.1231% +2.4271%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
e2e_packet_build/fast_50_events
                        time:   [2.4599 µs 2.4609 µs 2.4626 µs]
                        thrpt:  [1.2102 GiB/s 1.2110 GiB/s 1.2115 GiB/s]
                 change:
                        time:   [−2.6186% −2.3972% −2.1858%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2346% +2.4561% +2.6890%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.2s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/shared_pool/8
                        time:   [1.8227 ms 1.8532 ms 1.8927 ms]
                        thrpt:  [4.2268 Melem/s 4.3170 Melem/s 4.3890 Melem/s]
                 change:
                        time:   [−8.3728% −6.9519% −5.2595%] (p = 0.00 < 0.05)
                        thrpt:  [+5.5514% +7.4713% +9.1379%]
                        Performance has improved.
Found 20 outliers among 100 measurements (20.00%)
  6 (6.00%) high mild
  14 (14.00%) high severe
multithread_packet_build/thread_local_pool/8
                        time:   [678.87 µs 686.14 µs 695.76 µs]
                        thrpt:  [11.498 Melem/s 11.659 Melem/s 11.784 Melem/s]
                 change:
                        time:   [+5.3142% +8.5622% +12.072%] (p = 0.00 < 0.05)
                        thrpt:  [−10.772% −7.8869% −5.0460%]
                        Performance has regressed.
multithread_packet_build/shared_pool/16
                        time:   [4.2251 ms 4.3103 ms 4.4021 ms]
                        thrpt:  [3.6346 Melem/s 3.7120 Melem/s 3.7869 Melem/s]
                 change:
                        time:   [−15.719% −12.936% −10.063%] (p = 0.00 < 0.05)
                        thrpt:  [+11.189% +14.858% +18.651%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.1s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/16
                        time:   [1.3946 ms 1.3962 ms 1.3979 ms]
                        thrpt:  [11.446 Melem/s 11.460 Melem/s 11.473 Melem/s]
                 change:
                        time:   [−3.4655% −3.0514% −2.6590%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7316% +3.1475% +3.5900%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) low severe
  2 (2.00%) high mild
  5 (5.00%) high severe
multithread_packet_build/shared_pool/24
                        time:   [6.6026 ms 6.7933 ms 6.9992 ms]
                        thrpt:  [3.4290 Melem/s 3.5329 Melem/s 3.6350 Melem/s]
                 change:
                        time:   [−18.296% −14.482% −10.644%] (p = 0.00 < 0.05)
                        thrpt:  [+11.912% +16.934% +22.393%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
multithread_packet_build/thread_local_pool/24
                        time:   [2.1034 ms 2.1242 ms 2.1473 ms]
                        thrpt:  [11.177 Melem/s 11.298 Melem/s 11.410 Melem/s]
                 change:
                        time:   [−2.1587% −1.2996% −0.2188%] (p = 0.01 < 0.05)
                        thrpt:  [+0.2193% +1.3167% +2.2064%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe
multithread_packet_build/shared_pool/32
                        time:   [9.3313 ms 9.6371 ms 9.9624 ms]
                        thrpt:  [3.2121 Melem/s 3.3205 Melem/s 3.4293 Melem/s]
                 change:
                        time:   [−21.616% −17.350% −12.958%] (p = 0.00 < 0.05)
                        thrpt:  [+14.887% +20.992% +27.577%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
multithread_packet_build/thread_local_pool/32
                        time:   [2.7874 ms 2.7898 ms 2.7925 ms]
                        thrpt:  [11.459 Melem/s 11.470 Melem/s 11.480 Melem/s]
                 change:
                        time:   [−3.0115% −2.8386% −2.6851%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7592% +2.9215% +3.1050%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.3s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/shared_mixed/8
                        time:   [1.0369 ms 1.0381 ms 1.0396 ms]
                        thrpt:  [11.543 Melem/s 11.560 Melem/s 11.574 Melem/s]
                 change:
                        time:   [−12.304% −11.431% −10.575%] (p = 0.00 < 0.05)
                        thrpt:  [+11.826% +12.907% +14.030%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
multithread_mixed_frames/fast_mixed/8
                        time:   [633.78 µs 635.02 µs 637.12 µs]
                        thrpt:  [18.835 Melem/s 18.897 Melem/s 18.934 Melem/s]
                 change:
                        time:   [−6.8272% −6.0607% −5.3525%] (p = 0.00 < 0.05)
                        thrpt:  [+5.6552% +6.4517% +7.3274%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high severe
multithread_mixed_frames/shared_mixed/16
                        time:   [2.3792 ms 2.4291 ms 2.4845 ms]
                        thrpt:  [9.6598 Melem/s 9.8802 Melem/s 10.087 Melem/s]
                 change:
                        time:   [−24.772% −21.488% −18.125%] (p = 0.00 < 0.05)
                        thrpt:  [+22.138% +27.368% +32.929%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.5s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/fast_mixed/16
                        time:   [1.2168 ms 1.2185 ms 1.2207 ms]
                        thrpt:  [19.660 Melem/s 19.697 Melem/s 19.724 Melem/s]
                 change:
                        time:   [−8.1190% −6.9186% −5.8065%] (p = 0.00 < 0.05)
                        thrpt:  [+6.1644% +7.4329% +8.8364%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  7 (7.00%) high mild
  3 (3.00%) high severe
multithread_mixed_frames/shared_mixed/24
                        time:   [3.9496 ms 4.0760 ms 4.2113 ms]
                        thrpt:  [8.5484 Melem/s 8.8321 Melem/s 9.1149 Melem/s]
                 change:
                        time:   [−25.346% −21.019% −16.635%] (p = 0.00 < 0.05)
                        thrpt:  [+19.954% +26.612% +33.952%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking multithread_mixed_frames/fast_mixed/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.3s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/fast_mixed/24
                        time:   [1.7909 ms 1.8125 ms 1.8375 ms]
                        thrpt:  [19.592 Melem/s 19.862 Melem/s 20.102 Melem/s]
                 change:
                        time:   [−10.570% −9.3160% −8.0196%] (p = 0.00 < 0.05)
                        thrpt:  [+8.7188% +10.273% +11.819%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  14 (14.00%) high severe
multithread_mixed_frames/shared_mixed/32
                        time:   [5.3545 ms 5.6080 ms 5.8928 ms]
                        thrpt:  [8.1456 Melem/s 8.5592 Melem/s 8.9645 Melem/s]
                 change:
                        time:   [−32.944% −27.942% −22.525%] (p = 0.00 < 0.05)
                        thrpt:  [+29.074% +38.777% +49.129%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
multithread_mixed_frames/fast_mixed/32
                        time:   [2.3089 ms 2.3118 ms 2.3155 ms]
                        thrpt:  [20.730 Melem/s 20.763 Melem/s 20.790 Melem/s]
                 change:
                        time:   [−13.063% −11.875% −10.709%] (p = 0.00 < 0.05)
                        thrpt:  [+11.993% +13.476% +15.026%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

pool_contention/shared_acquire_release/8
                        time:   [17.338 ms 17.374 ms 17.416 ms]
                        thrpt:  [4.5936 Melem/s 4.6045 Melem/s 4.6140 Melem/s]
                 change:
                        time:   [−0.7060% −0.0893% +0.6602%] (p = 0.82 > 0.05)
                        thrpt:  [−0.6558% +0.0894% +0.7110%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
Benchmarking pool_contention/fast_acquire_release/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.0s, enable flat sampling, or reduce sample count to 50.
pool_contention/fast_acquire_release/8
                        time:   [1.1690 ms 1.1804 ms 1.1944 ms]
                        thrpt:  [66.982 Melem/s 67.775 Melem/s 68.437 Melem/s]
                 change:
                        time:   [−4.8121% −2.2609% +0.1363%] (p = 0.11 > 0.05)
                        thrpt:  [−0.1361% +2.3132% +5.0554%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
pool_contention/shared_acquire_release/16
                        time:   [37.536 ms 37.810 ms 38.085 ms]
                        thrpt:  [4.2012 Melem/s 4.2316 Melem/s 4.2626 Melem/s]
                 change:
                        time:   [+3.0823% +4.6745% +6.3228%] (p = 0.00 < 0.05)
                        thrpt:  [−5.9468% −4.4657% −2.9901%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
pool_contention/fast_acquire_release/16
                        time:   [2.1954 ms 2.2019 ms 2.2085 ms]
                        thrpt:  [72.446 Melem/s 72.666 Melem/s 72.880 Melem/s]
                 change:
                        time:   [−11.684% −10.223% −9.1714%] (p = 0.00 < 0.05)
                        thrpt:  [+10.098% +11.387% +13.230%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking pool_contention/shared_acquire_release/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, or reduce sample count to 80.
pool_contention/shared_acquire_release/24
                        time:   [60.382 ms 61.180 ms 62.042 ms]
                        thrpt:  [3.8683 Melem/s 3.9228 Melem/s 3.9747 Melem/s]
                 change:
                        time:   [+8.4558% +11.262% +14.129%] (p = 0.00 < 0.05)
                        thrpt:  [−12.380% −10.122% −7.7966%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
pool_contention/fast_acquire_release/24
                        time:   [3.3101 ms 3.3226 ms 3.3352 ms]
                        thrpt:  [71.960 Melem/s 72.233 Melem/s 72.506 Melem/s]
                 change:
                        time:   [−7.9546% −6.3821% −5.0823%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3544% +6.8172% +8.6421%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking pool_contention/shared_acquire_release/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.9s, or reduce sample count to 60.
pool_contention/shared_acquire_release/32
                        time:   [80.466 ms 82.277 ms 84.287 ms]
                        thrpt:  [3.7965 Melem/s 3.8893 Melem/s 3.9768 Melem/s]
                 change:
                        time:   [+1.9993% +5.2033% +8.6002%] (p = 0.00 < 0.05)
                        thrpt:  [−7.9191% −4.9459% −1.9601%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
pool_contention/fast_acquire_release/32
                        time:   [4.2438 ms 4.5433 ms 5.0642 ms]
                        thrpt:  [63.189 Melem/s 70.434 Melem/s 75.404 Melem/s]
                 change:
                        time:   [−11.201% −4.2349% +8.0051%] (p = 0.50 > 0.05)
                        thrpt:  [−7.4117% +4.4222% +12.614%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe

throughput_scaling/fast_pool_scaling/1
                        time:   [2.4735 ms 2.4787 ms 2.4859 ms]
                        thrpt:  [804.53 Kelem/s 806.86 Kelem/s 808.57 Kelem/s]
                 change:
                        time:   [−1.1914% −0.6935% −0.2052%] (p = 0.01 < 0.05)
                        thrpt:  [+0.2056% +0.6984% +1.2057%]
                        Change within noise threshold.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/2
                        time:   [2.5800 ms 2.5825 ms 2.5847 ms]
                        thrpt:  [1.5476 Melem/s 1.5489 Melem/s 1.5504 Melem/s]
                 change:
                        time:   [−1.7209% −1.0124% −0.4193%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4211% +1.0227% +1.7510%]
                        Change within noise threshold.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/4
                        time:   [2.7873 ms 2.7908 ms 2.7948 ms]
                        thrpt:  [2.8624 Melem/s 2.8666 Melem/s 2.8702 Melem/s]
                 change:
                        time:   [−2.0531% −1.0590% +0.1744%] (p = 0.07 > 0.05)
                        thrpt:  [−0.1741% +1.0703% +2.0961%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/8
                        time:   [3.0268 ms 3.0433 ms 3.0655 ms]
                        thrpt:  [5.2195 Melem/s 5.2574 Melem/s 5.2862 Melem/s]
                 change:
                        time:   [−4.7855% −2.2412% −0.4114%] (p = 0.04 < 0.05)
                        thrpt:  [+0.4131% +2.2925% +5.0260%]
                        Change within noise threshold.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/16
                        time:   [5.9584 ms 5.9857 ms 6.0074 ms]
                        thrpt:  [5.3268 Melem/s 5.3461 Melem/s 5.3706 Melem/s]
                 change:
                        time:   [−2.0856% −1.1274% −0.1533%] (p = 0.03 < 0.05)
                        thrpt:  [+0.1535% +1.1403% +2.1300%]
                        Change within noise threshold.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/24
                        time:   [8.6699 ms 8.7039 ms 8.7568 ms]
                        thrpt:  [5.4814 Melem/s 5.5148 Melem/s 5.5364 Melem/s]
                 change:
                        time:   [−3.3529% −2.6440% −1.9807%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0208% +2.7158% +3.4692%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe
throughput_scaling/fast_pool_scaling/32
                        time:   [11.969 ms 12.350 ms 12.800 ms]
                        thrpt:  [4.9999 Melem/s 5.1820 Melem/s 5.3473 Melem/s]
                 change:
                        time:   [+1.8130% +4.5041% +7.7487%] (p = 0.01 < 0.05)
                        thrpt:  [−7.1915% −4.3100% −1.7807%]
                        Performance has regressed.

routing_header/serialize
                        time:   [623.46 ps 624.31 ps 625.28 ps]
                        thrpt:  [1.5993 Gelem/s 1.6018 Gelem/s 1.6040 Gelem/s]
                 change:
                        time:   [−0.4855% −0.1467% +0.1718%] (p = 0.40 > 0.05)
                        thrpt:  [−0.1715% +0.1469% +0.4879%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
routing_header/deserialize
                        time:   [931.35 ps 931.90 ps 932.64 ps]
                        thrpt:  [1.0722 Gelem/s 1.0731 Gelem/s 1.0737 Gelem/s]
                 change:
                        time:   [−0.4979% −0.2428% −0.0079%] (p = 0.05 > 0.05)
                        thrpt:  [+0.0079% +0.2434% +0.5004%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
routing_header/roundtrip
                        time:   [931.11 ps 931.81 ps 932.84 ps]
                        thrpt:  [1.0720 Gelem/s 1.0732 Gelem/s 1.0740 Gelem/s]
                 change:
                        time:   [−0.5195% −0.2672% −0.0107%] (p = 0.04 < 0.05)
                        thrpt:  [+0.0107% +0.2679% +0.5222%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high severe
routing_header/forward  time:   [570.04 ps 571.42 ps 572.75 ps]
                        thrpt:  [1.7460 Gelem/s 1.7500 Gelem/s 1.7542 Gelem/s]
                 change:
                        time:   [−0.7414% −0.2945% +0.1646%] (p = 0.21 > 0.05)
                        thrpt:  [−0.1643% +0.2953% +0.7470%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

routing_table/lookup_hit
                        time:   [36.916 ns 37.730 ns 38.648 ns]
                        thrpt:  [25.874 Melem/s 26.504 Melem/s 27.089 Melem/s]
                 change:
                        time:   [−2.7224% −0.0677% +2.6693%] (p = 0.96 > 0.05)
                        thrpt:  [−2.5999% +0.0677% +2.7986%]
                        No change in performance detected.
routing_table/lookup_miss
                        time:   [15.176 ns 15.219 ns 15.261 ns]
                        thrpt:  [65.525 Melem/s 65.708 Melem/s 65.892 Melem/s]
                 change:
                        time:   [−0.4136% +0.0736% +0.5917%] (p = 0.78 > 0.05)
                        thrpt:  [−0.5882% −0.0735% +0.4153%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low severe
  3 (3.00%) high mild
routing_table/is_local  time:   [311.25 ps 313.39 ps 316.23 ps]
                        thrpt:  [3.1623 Gelem/s 3.1909 Gelem/s 3.2129 Gelem/s]
                 change:
                        time:   [−2.7194% −1.3990% −0.2492%] (p = 0.02 < 0.05)
                        thrpt:  [+0.2498% +1.4188% +2.7955%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
routing_table/add_route time:   [45.897 ns 46.487 ns 47.021 ns]
                        thrpt:  [21.267 Melem/s 21.511 Melem/s 21.788 Melem/s]
                 change:
                        time:   [−2.6962% −0.6848% +1.2278%] (p = 0.50 > 0.05)
                        thrpt:  [−1.2129% +0.6895% +2.7709%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) low severe
  8 (8.00%) low mild
  2 (2.00%) high mild
routing_table/record_in time:   [48.796 ns 49.694 ns 50.522 ns]
                        thrpt:  [19.793 Melem/s 20.123 Melem/s 20.494 Melem/s]
                 change:
                        time:   [−5.1941% −3.4571% −1.6736%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7020% +3.5809% +5.4787%]
                        Performance has improved.
routing_table/record_out
                        time:   [21.723 ns 22.084 ns 22.387 ns]
                        thrpt:  [44.670 Melem/s 45.282 Melem/s 46.034 Melem/s]
                 change:
                        time:   [−2.7374% −0.8788% +0.8390%] (p = 0.33 > 0.05)
                        thrpt:  [−0.8320% +0.8866% +2.8145%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  9 (9.00%) low severe
  7 (7.00%) low mild
routing_table/aggregate_stats
                        time:   [2.1314 µs 2.1356 µs 2.1419 µs]
                        thrpt:  [466.88 Kelem/s 468.25 Kelem/s 469.18 Kelem/s]
                 change:
                        time:   [−1.6936% −1.4745% −1.2649%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2811% +1.4966% +1.7228%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe

fair_scheduler/creation time:   [388.09 ns 391.85 ns 395.23 ns]
                        thrpt:  [2.5302 Melem/s 2.5520 Melem/s 2.5767 Melem/s]
                 change:
                        time:   [+1.8704% +2.9086% +3.9111%] (p = 0.00 < 0.05)
                        thrpt:  [−3.7639% −2.8264% −1.8360%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) low mild
fair_scheduler/stream_count_empty
                        time:   [199.31 ns 199.39 ns 199.49 ns]
                        thrpt:  [5.0127 Melem/s 5.0152 Melem/s 5.0172 Melem/s]
                 change:
                        time:   [−0.2328% −0.0607% +0.0968%] (p = 0.50 > 0.05)
                        thrpt:  [−0.0967% +0.0607% +0.2334%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
fair_scheduler/total_queued
                        time:   [310.81 ps 311.25 ps 311.83 ps]
                        thrpt:  [3.2069 Gelem/s 3.2128 Gelem/s 3.2173 Gelem/s]
                 change:
                        time:   [+0.0862% +0.4619% +0.8562%] (p = 0.02 < 0.05)
                        thrpt:  [−0.8490% −0.4598% −0.0861%]
                        Change within noise threshold.
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
fair_scheduler/cleanup_empty
                        time:   [200.24 ns 200.45 ns 200.70 ns]
                        thrpt:  [4.9826 Melem/s 4.9888 Melem/s 4.9939 Melem/s]
                 change:
                        time:   [−0.3244% −0.1286% +0.0677%] (p = 0.21 > 0.05)
                        thrpt:  [−0.0676% +0.1287% +0.3255%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe

routing_table_concurrent/concurrent_lookup/4
                        time:   [153.29 µs 158.46 µs 163.30 µs]
                        thrpt:  [24.494 Melem/s 25.243 Melem/s 26.095 Melem/s]
                 change:
                        time:   [−1.3879% +1.8982% +5.3319%] (p = 0.25 > 0.05)
                        thrpt:  [−5.0620% −1.8629% +1.4074%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  12 (12.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
routing_table_concurrent/concurrent_stats/4
                        time:   [290.80 µs 291.31 µs 291.79 µs]
                        thrpt:  [13.709 Melem/s 13.731 Melem/s 13.755 Melem/s]
                 change:
                        time:   [−0.5461% −0.1564% +0.3265%] (p = 0.51 > 0.05)
                        thrpt:  [−0.3254% +0.1567% +0.5491%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
routing_table_concurrent/concurrent_lookup/8
                        time:   [246.00 µs 249.46 µs 253.61 µs]
                        thrpt:  [31.544 Melem/s 32.069 Melem/s 32.520 Melem/s]
                 change:
                        time:   [−0.3969% +0.4110% +1.2694%] (p = 0.35 > 0.05)
                        thrpt:  [−1.2535% −0.4093% +0.3985%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe
routing_table_concurrent/concurrent_stats/8
                        time:   [405.75 µs 406.38 µs 407.22 µs]
                        thrpt:  [19.645 Melem/s 19.686 Melem/s 19.717 Melem/s]
                 change:
                        time:   [−13.122% −11.031% −9.0487%] (p = 0.00 < 0.05)
                        thrpt:  [+9.9490% +12.398% +15.104%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
routing_table_concurrent/concurrent_lookup/16
                        time:   [426.19 µs 428.00 µs 431.23 µs]
                        thrpt:  [37.104 Melem/s 37.383 Melem/s 37.542 Melem/s]
                 change:
                        time:   [−5.9754% −5.5203% −5.0518%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3205% +5.8428% +6.3551%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
routing_table_concurrent/concurrent_stats/16
                        time:   [798.15 µs 799.27 µs 800.66 µs]
                        thrpt:  [19.983 Melem/s 20.018 Melem/s 20.046 Melem/s]
                 change:
                        time:   [−3.9561% −3.2597% −2.5937%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6627% +3.3695% +4.1191%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
  5 (5.00%) high severe

routing_decision/parse_lookup_forward
                        time:   [37.258 ns 37.481 ns 37.768 ns]
                        thrpt:  [26.478 Melem/s 26.680 Melem/s 26.840 Melem/s]
                 change:
                        time:   [−3.0502% −1.2745% +0.5195%] (p = 0.17 > 0.05)
                        thrpt:  [−0.5168% +1.2909% +3.1462%]
                        No change in performance detected.
Found 19 outliers among 100 measurements (19.00%)
  3 (3.00%) high mild
  16 (16.00%) high severe
routing_decision/full_with_stats
                        time:   [105.87 ns 106.24 ns 106.64 ns]
                        thrpt:  [9.3776 Melem/s 9.4129 Melem/s 9.4459 Melem/s]
                 change:
                        time:   [−0.7646% −0.3855% +0.0050%] (p = 0.05 < 0.05)
                        thrpt:  [−0.0050% +0.3870% +0.7704%]
                        Change within noise threshold.
Found 23 outliers among 100 measurements (23.00%)
  9 (9.00%) high mild
  14 (14.00%) high severe

stream_multiplexing/lookup_all/10
                        time:   [291.28 ns 292.48 ns 293.80 ns]
                        thrpt:  [34.037 Melem/s 34.191 Melem/s 34.331 Melem/s]
                 change:
                        time:   [−0.2250% +0.0797% +0.3741%] (p = 0.60 > 0.05)
                        thrpt:  [−0.3727% −0.0797% +0.2255%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe
stream_multiplexing/stats_all/10
                        time:   [461.53 ns 465.58 ns 469.21 ns]
                        thrpt:  [21.312 Melem/s 21.479 Melem/s 21.667 Melem/s]
                 change:
                        time:   [−1.2059% −0.4819% +0.2477%] (p = 0.19 > 0.05)
                        thrpt:  [−0.2471% +0.4842% +1.2206%]
                        No change in performance detected.
Found 22 outliers among 100 measurements (22.00%)
  10 (10.00%) low severe
  5 (5.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
stream_multiplexing/lookup_all/100
                        time:   [2.9128 µs 2.9158 µs 2.9194 µs]
                        thrpt:  [34.253 Melem/s 34.296 Melem/s 34.331 Melem/s]
                 change:
                        time:   [+0.1082% +0.3307% +0.5760%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5727% −0.3296% −0.1081%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe
stream_multiplexing/stats_all/100
                        time:   [4.5538 µs 4.5977 µs 4.6403 µs]
                        thrpt:  [21.550 Melem/s 21.750 Melem/s 21.960 Melem/s]
                 change:
                        time:   [−0.3282% +0.3530% +1.0289%] (p = 0.32 > 0.05)
                        thrpt:  [−1.0184% −0.3518% +0.3293%]
                        No change in performance detected.
Found 24 outliers among 100 measurements (24.00%)
  15 (15.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
stream_multiplexing/lookup_all/1000
                        time:   [29.081 µs 29.094 µs 29.112 µs]
                        thrpt:  [34.350 Melem/s 34.371 Melem/s 34.386 Melem/s]
                 change:
                        time:   [−0.7256% −0.4430% −0.1919%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1923% +0.4450% +0.7309%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
stream_multiplexing/stats_all/1000
                        time:   [46.119 µs 46.384 µs 46.599 µs]
                        thrpt:  [21.460 Melem/s 21.559 Melem/s 21.683 Melem/s]
                 change:
                        time:   [−1.6078% −0.6317% +0.2313%] (p = 0.18 > 0.05)
                        thrpt:  [−0.2308% +0.6357% +1.6340%]
                        No change in performance detected.
Found 33 outliers among 100 measurements (33.00%)
  22 (22.00%) low severe
  4 (4.00%) high mild
  7 (7.00%) high severe
stream_multiplexing/lookup_all/10000
                        time:   [291.04 µs 291.12 µs 291.23 µs]
                        thrpt:  [34.337 Melem/s 34.350 Melem/s 34.360 Melem/s]
                 change:
                        time:   [−0.3320% −0.0334% +0.2574%] (p = 0.83 > 0.05)
                        thrpt:  [−0.2567% +0.0334% +0.3331%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
stream_multiplexing/stats_all/10000
                        time:   [504.10 µs 508.24 µs 513.14 µs]
                        thrpt:  [19.488 Melem/s 19.676 Melem/s 19.837 Melem/s]
                 change:
                        time:   [+3.1181% +4.6078% +6.0144%] (p = 0.00 < 0.05)
                        thrpt:  [−5.6732% −4.4048% −3.0238%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

multihop_packet_builder/build/64
                        time:   [25.899 ns 25.934 ns 25.970 ns]
                        thrpt:  [2.2952 GiB/s 2.2983 GiB/s 2.3015 GiB/s]
                 change:
                        time:   [−0.5702% −0.3135% −0.0582%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0582% +0.3144% +0.5735%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
multihop_packet_builder/build_priority/64
                        time:   [23.956 ns 24.004 ns 24.071 ns]
                        thrpt:  [2.4762 GiB/s 2.4831 GiB/s 2.4881 GiB/s]
                 change:
                        time:   [+0.6427% +1.6117% +2.8946%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8131% −1.5861% −0.6386%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
multihop_packet_builder/build/256
                        time:   [50.782 ns 51.517 ns 52.245 ns]
                        thrpt:  [4.5634 GiB/s 4.6280 GiB/s 4.6950 GiB/s]
                 change:
                        time:   [−0.3461% +1.2169% +2.8600%] (p = 0.12 > 0.05)
                        thrpt:  [−2.7804% −1.2023% +0.3473%]
                        No change in performance detected.
multihop_packet_builder/build_priority/256
                        time:   [47.278 ns 47.969 ns 48.727 ns]
                        thrpt:  [4.8930 GiB/s 4.9703 GiB/s 5.0430 GiB/s]
                 change:
                        time:   [−0.4773% +1.0691% +2.7384%] (p = 0.20 > 0.05)
                        thrpt:  [−2.6654% −1.0578% +0.4796%]
                        No change in performance detected.
multihop_packet_builder/build/1024
                        time:   [39.474 ns 39.581 ns 39.691 ns]
                        thrpt:  [24.027 GiB/s 24.094 GiB/s 24.159 GiB/s]
                 change:
                        time:   [+0.3982% +0.7033% +1.0284%] (p = 0.00 < 0.05)
                        thrpt:  [−1.0179% −0.6984% −0.3966%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_packet_builder/build_priority/1024
                        time:   [36.300 ns 36.372 ns 36.448 ns]
                        thrpt:  [26.166 GiB/s 26.220 GiB/s 26.272 GiB/s]
                 change:
                        time:   [−1.6424% −1.0790% −0.5450%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5480% +1.0908% +1.6698%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build/4096
                        time:   [77.405 ns 77.966 ns 78.742 ns]
                        thrpt:  [48.445 GiB/s 48.928 GiB/s 49.283 GiB/s]
                 change:
                        time:   [−13.104% −11.136% −9.1946%] (p = 0.00 < 0.05)
                        thrpt:  [+10.126% +12.531% +15.080%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
multihop_packet_builder/build_priority/4096
                        time:   [75.641 ns 75.934 ns 76.312 ns]
                        thrpt:  [49.988 GiB/s 50.237 GiB/s 50.431 GiB/s]
                 change:
                        time:   [−12.353% −11.014% −9.5563%] (p = 0.00 < 0.05)
                        thrpt:  [+10.566% +12.377% +14.094%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

multihop_chain/forward_chain/1
                        time:   [60.817 ns 61.656 ns 62.431 ns]
                        thrpt:  [16.018 Melem/s 16.219 Melem/s 16.443 Melem/s]
                 change:
                        time:   [+0.4779% +1.5138% +2.5865%] (p = 0.00 < 0.05)
                        thrpt:  [−2.5212% −1.4913% −0.4757%]
                        Change within noise threshold.
Found 25 outliers among 100 measurements (25.00%)
  17 (17.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
multihop_chain/forward_chain/2
                        time:   [115.10 ns 116.46 ns 117.75 ns]
                        thrpt:  [8.4926 Melem/s 8.5868 Melem/s 8.6878 Melem/s]
                 change:
                        time:   [+1.3489% +2.3855% +3.3640%] (p = 0.00 < 0.05)
                        thrpt:  [−3.2546% −2.3299% −1.3309%]
                        Performance has regressed.
multihop_chain/forward_chain/3
                        time:   [158.16 ns 160.03 ns 162.02 ns]
                        thrpt:  [6.1721 Melem/s 6.2489 Melem/s 6.3228 Melem/s]
                 change:
                        time:   [−2.8877% −1.6394% −0.4858%] (p = 0.01 < 0.05)
                        thrpt:  [+0.4882% +1.6667% +2.9736%]
                        Change within noise threshold.
multihop_chain/forward_chain/4
                        time:   [215.50 ns 217.27 ns 219.02 ns]
                        thrpt:  [4.5658 Melem/s 4.6026 Melem/s 4.6403 Melem/s]
                 change:
                        time:   [−1.2080% −0.2100% +0.8008%] (p = 0.68 > 0.05)
                        thrpt:  [−0.7944% +0.2104% +1.2227%]
                        No change in performance detected.
multihop_chain/forward_chain/5
                        time:   [268.72 ns 271.11 ns 273.51 ns]
                        thrpt:  [3.6562 Melem/s 3.6886 Melem/s 3.7213 Melem/s]
                 change:
                        time:   [+0.1421% +1.2597% +2.3438%] (p = 0.02 < 0.05)
                        thrpt:  [−2.2902% −1.2441% −0.1419%]
                        Change within noise threshold.

hop_latency/single_hop_process
                        time:   [1.4530 ns 1.4653 ns 1.4803 ns]
                        thrpt:  [675.52 Melem/s 682.45 Melem/s 688.23 Melem/s]
                 change:
                        time:   [−0.0560% +0.3105% +0.7784%] (p = 0.17 > 0.05)
                        thrpt:  [−0.7724% −0.3095% +0.0560%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
hop_latency/single_hop_full
                        time:   [57.316 ns 58.018 ns 58.640 ns]
                        thrpt:  [17.053 Melem/s 17.236 Melem/s 17.447 Melem/s]
                 change:
                        time:   [+4.1399% +5.3715% +6.5423%] (p = 0.00 < 0.05)
                        thrpt:  [−6.1406% −5.0977% −3.9753%]
                        Performance has regressed.
Found 15 outliers among 100 measurements (15.00%)
  9 (9.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild

hop_scaling/64B_1hops   time:   [31.687 ns 31.758 ns 31.827 ns]
                        thrpt:  [1.8727 GiB/s 1.8768 GiB/s 1.8810 GiB/s]
                 change:
                        time:   [−0.4331% −0.1133% +0.1987%] (p = 0.48 > 0.05)
                        thrpt:  [−0.1983% +0.1134% +0.4350%]
                        No change in performance detected.
hop_scaling/64B_2hops   time:   [83.374 ns 84.664 ns 85.924 ns]
                        thrpt:  [710.34 MiB/s 720.91 MiB/s 732.07 MiB/s]
                 change:
                        time:   [+6.7044% +8.1282% +9.5207%] (p = 0.00 < 0.05)
                        thrpt:  [−8.6930% −7.5172% −6.2831%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) low mild
hop_scaling/64B_3hops   time:   [110.42 ns 111.24 ns 111.98 ns]
                        thrpt:  [545.04 MiB/s 548.67 MiB/s 552.77 MiB/s]
                 change:
                        time:   [+4.4845% +5.6041% +6.6979%] (p = 0.00 < 0.05)
                        thrpt:  [−6.2774% −5.3067% −4.2921%]
                        Performance has regressed.
Found 19 outliers among 100 measurements (19.00%)
  5 (5.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  10 (10.00%) high severe
hop_scaling/64B_4hops   time:   [139.75 ns 140.62 ns 141.43 ns]
                        thrpt:  [431.57 MiB/s 434.03 MiB/s 436.76 MiB/s]
                 change:
                        time:   [+2.8741% +3.5937% +4.2779%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1024% −3.4691% −2.7938%]
                        Performance has regressed.
hop_scaling/64B_5hops   time:   [166.06 ns 167.47 ns 168.92 ns]
                        thrpt:  [361.32 MiB/s 364.46 MiB/s 367.55 MiB/s]
                 change:
                        time:   [+2.1273% +3.1643% +4.2227%] (p = 0.00 < 0.05)
                        thrpt:  [−4.0516% −3.0672% −2.0830%]
                        Performance has regressed.
hop_scaling/256B_1hops  time:   [57.564 ns 58.446 ns 59.366 ns]
                        thrpt:  [4.0161 GiB/s 4.0793 GiB/s 4.1418 GiB/s]
                 change:
                        time:   [−1.3320% +0.2145% +1.6957%] (p = 0.78 > 0.05)
                        thrpt:  [−1.6674% −0.2140% +1.3500%]
                        No change in performance detected.
hop_scaling/256B_2hops  time:   [112.55 ns 113.84 ns 115.12 ns]
                        thrpt:  [2.0710 GiB/s 2.0943 GiB/s 2.1183 GiB/s]
                 change:
                        time:   [−0.7538% +0.3025% +1.4220%] (p = 0.60 > 0.05)
                        thrpt:  [−1.4020% −0.3016% +0.7595%]
                        No change in performance detected.
hop_scaling/256B_3hops  time:   [157.21 ns 159.04 ns 160.81 ns]
                        thrpt:  [1.4826 GiB/s 1.4991 GiB/s 1.5166 GiB/s]
                 change:
                        time:   [−6.5006% −5.3031% −4.0392%] (p = 0.00 < 0.05)
                        thrpt:  [+4.2092% +5.6001% +6.9525%]
                        Performance has improved.
hop_scaling/256B_4hops  time:   [212.62 ns 214.88 ns 217.18 ns]
                        thrpt:  [1.0978 GiB/s 1.1095 GiB/s 1.1213 GiB/s]
                 change:
                        time:   [−3.7868% −2.4388% −1.0654%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0768% +2.4998% +3.9358%]
                        Performance has improved.
hop_scaling/256B_5hops  time:   [267.60 ns 270.25 ns 273.03 ns]
                        thrpt:  [894.20 MiB/s 903.39 MiB/s 912.32 MiB/s]
                 change:
                        time:   [+2.3117% +3.4347% +4.6014%] (p = 0.00 < 0.05)
                        thrpt:  [−4.3990% −3.3206% −2.2595%]
                        Performance has regressed.
hop_scaling/1024B_1hops time:   [44.839 ns 44.878 ns 44.920 ns]
                        thrpt:  [21.231 GiB/s 21.251 GiB/s 21.269 GiB/s]
                 change:
                        time:   [−1.6858% −1.4512% −1.2075%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2223% +1.4726% +1.7147%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
hop_scaling/1024B_2hops time:   [107.06 ns 107.25 ns 107.45 ns]
                        thrpt:  [8.8753 GiB/s 8.8923 GiB/s 8.9076 GiB/s]
                 change:
                        time:   [−1.7797% −0.0586% +2.2340%] (p = 0.96 > 0.05)
                        thrpt:  [−2.1852% +0.0586% +1.8120%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
hop_scaling/1024B_3hops time:   [148.85 ns 150.63 ns 152.37 ns]
                        thrpt:  [6.2588 GiB/s 6.3314 GiB/s 6.4069 GiB/s]
                 change:
                        time:   [−1.0568% +0.2094% +1.4150%] (p = 0.75 > 0.05)
                        thrpt:  [−1.3952% −0.2090% +1.0680%]
                        No change in performance detected.
hop_scaling/1024B_4hops time:   [198.98 ns 200.62 ns 202.22 ns]
                        thrpt:  [4.7160 GiB/s 4.7536 GiB/s 4.7928 GiB/s]
                 change:
                        time:   [−3.1921% −2.4684% −1.7911%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8238% +2.5309% +3.2974%]
                        Performance has improved.
hop_scaling/1024B_5hops time:   [232.45 ns 233.58 ns 234.91 ns]
                        thrpt:  [4.0597 GiB/s 4.0828 GiB/s 4.1027 GiB/s]
                 change:
                        time:   [−3.5506% −2.7818% −2.0244%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0662% +2.8614% +3.6813%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

multihop_with_routing/route_and_forward/1
                        time:   [156.65 ns 157.11 ns 157.59 ns]
                        thrpt:  [6.3458 Melem/s 6.3648 Melem/s 6.3836 Melem/s]
                 change:
                        time:   [−0.4909% −0.1289% +0.2493%] (p = 0.50 > 0.05)
                        thrpt:  [−0.2487% +0.1291% +0.4933%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_with_routing/route_and_forward/2
                        time:   [308.50 ns 310.08 ns 311.87 ns]
                        thrpt:  [3.2065 Melem/s 3.2250 Melem/s 3.2415 Melem/s]
                 change:
                        time:   [+0.6356% +1.3831% +2.0249%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9847% −1.3642% −0.6316%]
                        Change within noise threshold.
multihop_with_routing/route_and_forward/3
                        time:   [461.26 ns 462.31 ns 463.80 ns]
                        thrpt:  [2.1561 Melem/s 2.1631 Melem/s 2.1680 Melem/s]
                 change:
                        time:   [+1.3109% +2.6151% +4.2157%] (p = 0.00 < 0.05)
                        thrpt:  [−4.0452% −2.5484% −1.2940%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
multihop_with_routing/route_and_forward/4
                        time:   [620.00 ns 620.72 ns 621.50 ns]
                        thrpt:  [1.6090 Melem/s 1.6110 Melem/s 1.6129 Melem/s]
                 change:
                        time:   [+0.2702% +0.5036% +0.7454%] (p = 0.00 < 0.05)
                        thrpt:  [−0.7399% −0.5011% −0.2695%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
multihop_with_routing/route_and_forward/5
                        time:   [774.29 ns 775.19 ns 776.08 ns]
                        thrpt:  [1.2885 Melem/s 1.2900 Melem/s 1.2915 Melem/s]
                 change:
                        time:   [+0.0294% +0.3217% +0.6056%] (p = 0.02 < 0.05)
                        thrpt:  [−0.6020% −0.3207% −0.0294%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

multihop_concurrent/concurrent_forward/4
                        time:   [706.06 µs 709.80 µs 713.13 µs]
                        thrpt:  [5.6091 Melem/s 5.6354 Melem/s 5.6652 Melem/s]
                 change:
                        time:   [−26.937% −22.654% −18.771%] (p = 0.00 < 0.05)
                        thrpt:  [+23.108% +29.289% +36.868%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
multihop_concurrent/concurrent_forward/8
                        time:   [1.1239 ms 1.1528 ms 1.1990 ms]
                        thrpt:  [6.6721 Melem/s 6.9396 Melem/s 7.1179 Melem/s]
                 change:
                        time:   [−38.480% −37.079% −35.541%] (p = 0.00 < 0.05)
                        thrpt:  [+55.137% +58.929% +62.548%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
multihop_concurrent/concurrent_forward/16
                        time:   [1.6773 ms 1.7385 ms 1.7921 ms]
                        thrpt:  [8.9281 Melem/s 9.2036 Melem/s 9.5391 Melem/s]
                 change:
                        time:   [−20.091% −18.627% −16.721%] (p = 0.00 < 0.05)
                        thrpt:  [+20.078% +22.891% +25.142%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe

pingwave/serialize      time:   [778.00 ps 781.32 ps 785.74 ps]
                        thrpt:  [1.2727 Gelem/s 1.2799 Gelem/s 1.2854 Gelem/s]
                 change:
                        time:   [−2.7105% −2.3101% −1.7847%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8171% +2.3647% +2.7860%]
                        Performance has improved.
Found 20 outliers among 100 measurements (20.00%)
  4 (4.00%) high mild
  16 (16.00%) high severe
pingwave/deserialize    time:   [931.17 ps 931.39 ps 931.65 ps]
                        thrpt:  [1.0734 Gelem/s 1.0737 Gelem/s 1.0739 Gelem/s]
                 change:
                        time:   [−2.9056% −2.7963% −2.6641%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7370% +2.8767% +2.9925%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe
pingwave/roundtrip      time:   [931.50 ps 931.83 ps 932.24 ps]
                        thrpt:  [1.0727 Gelem/s 1.0732 Gelem/s 1.0735 Gelem/s]
                 change:
                        time:   [+0.1354% +0.3586% +0.6022%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5986% −0.3573% −0.1352%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
pingwave/forward        time:   [624.00 ps 625.27 ps 626.88 ps]
                        thrpt:  [1.5952 Gelem/s 1.5993 Gelem/s 1.6026 Gelem/s]
                 change:
                        time:   [−0.4498% −0.0348% +0.3866%] (p = 0.87 > 0.05)
                        thrpt:  [−0.3851% +0.0348% +0.4519%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

capabilities/serialize_simple
                        time:   [20.704 ns 20.728 ns 20.759 ns]
                        thrpt:  [48.172 Melem/s 48.245 Melem/s 48.300 Melem/s]
                 change:
                        time:   [−1.7933% −1.0290% −0.5237%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5265% +1.0397% +1.8260%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
capabilities/deserialize_simple
                        time:   [5.5735 ns 5.5787 ns 5.5840 ns]
                        thrpt:  [179.08 Melem/s 179.25 Melem/s 179.42 Melem/s]
                 change:
                        time:   [−0.2181% +0.0144% +0.2733%] (p = 0.91 > 0.05)
                        thrpt:  [−0.2726% −0.0144% +0.2186%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
capabilities/serialize_complex
                        time:   [43.933 ns 43.985 ns 44.031 ns]
                        thrpt:  [22.711 Melem/s 22.735 Melem/s 22.762 Melem/s]
                 change:
                        time:   [−1.3425% −1.1152% −0.8828%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8907% +1.1278% +1.3608%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  11 (11.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe
capabilities/deserialize_complex
                        time:   [376.67 ns 378.30 ns 380.18 ns]
                        thrpt:  [2.6303 Melem/s 2.6434 Melem/s 2.6549 Melem/s]
                 change:
                        time:   [+0.4624% +1.2249% +1.9281%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8916% −1.2101% −0.4603%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

local_graph/create_pingwave
                        time:   [2.0985 ns 2.1014 ns 2.1042 ns]
                        thrpt:  [475.23 Melem/s 475.88 Melem/s 476.52 Melem/s]
                 change:
                        time:   [−0.4971% −0.1413% +0.1975%] (p = 0.43 > 0.05)
                        thrpt:  [−0.1971% +0.1415% +0.4995%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe
local_graph/on_pingwave_new
                        time:   [39.295 ns 39.492 ns 39.734 ns]
                        thrpt:  [25.168 Melem/s 25.321 Melem/s 25.449 Melem/s]
                 change:
                        time:   [−5.0824% −0.9849% +3.1880%] (p = 0.66 > 0.05)
                        thrpt:  [−3.0896% +0.9947% +5.3545%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  7 (7.00%) high mild
  7 (7.00%) high severe
local_graph/on_pingwave_duplicate
                        time:   [22.762 ns 22.815 ns 22.868 ns]
                        thrpt:  [43.728 Melem/s 43.831 Melem/s 43.933 Melem/s]
                 change:
                        time:   [−0.6570% −0.2772% +0.1334%] (p = 0.17 > 0.05)
                        thrpt:  [−0.1333% +0.2779% +0.6613%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
local_graph/get_node    time:   [15.118 ns 15.127 ns 15.141 ns]
                        thrpt:  [66.044 Melem/s 66.107 Melem/s 66.145 Melem/s]
                 change:
                        time:   [−3.6753% −3.3247% −2.9588%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0490% +3.4390% +3.8156%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
local_graph/node_count  time:   [317.03 ps 319.63 ps 322.43 ps]
                        thrpt:  [3.1015 Gelem/s 3.1286 Gelem/s 3.1543 Gelem/s]
                 change:
                        time:   [+0.9712% +1.4697% +2.0145%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9747% −1.4484% −0.9619%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
local_graph/stats       time:   [387.99 ps 388.18 ps 388.47 ps]
                        thrpt:  [2.5742 Gelem/s 2.5762 Gelem/s 2.5774 Gelem/s]
                 change:
                        time:   [−0.2704% −0.0688% +0.1403%] (p = 0.51 > 0.05)
                        thrpt:  [−0.1401% +0.0688% +0.2711%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe

graph_scaling/all_nodes/100
                        time:   [2.8401 µs 2.8506 µs 2.8614 µs]
                        thrpt:  [34.947 Melem/s 35.081 Melem/s 35.210 Melem/s]
                 change:
                        time:   [−1.2713% +0.7749% +3.3687%] (p = 0.60 > 0.05)
                        thrpt:  [−3.2590% −0.7690% +1.2877%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
graph_scaling/nodes_within_hops/100
                        time:   [3.1801 µs 3.1941 µs 3.2083 µs]
                        thrpt:  [31.169 Melem/s 31.308 Melem/s 31.445 Melem/s]
                 change:
                        time:   [−0.2845% +0.1509% +0.5698%] (p = 0.50 > 0.05)
                        thrpt:  [−0.5666% −0.1507% +0.2853%]
                        No change in performance detected.
graph_scaling/all_nodes/500
                        time:   [8.2878 µs 8.3259 µs 8.3665 µs]
                        thrpt:  [59.762 Melem/s 60.054 Melem/s 60.329 Melem/s]
                 change:
                        time:   [−0.4301% +0.1171% +0.6463%] (p = 0.68 > 0.05)
                        thrpt:  [−0.6422% −0.1169% +0.4320%]
                        No change in performance detected.
graph_scaling/nodes_within_hops/500
                        time:   [9.7179 µs 9.7512 µs 9.7851 µs]
                        thrpt:  [51.098 Melem/s 51.276 Melem/s 51.451 Melem/s]
                 change:
                        time:   [+0.2494% +0.7866% +1.3375%] (p = 0.00 < 0.05)
                        thrpt:  [−1.3198% −0.7805% −0.2487%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
graph_scaling/all_nodes/1000
                        time:   [33.599 µs 37.378 µs 41.050 µs]
                        thrpt:  [24.361 Melem/s 26.754 Melem/s 29.763 Melem/s]
                 change:
                        time:   [−34.705% −27.307% −19.837%] (p = 0.00 < 0.05)
                        thrpt:  [+24.746% +37.564% +53.150%]
                        Performance has improved.
graph_scaling/nodes_within_hops/1000
                        time:   [61.839 µs 64.030 µs 66.209 µs]
                        thrpt:  [15.104 Melem/s 15.618 Melem/s 16.171 Melem/s]
                 change:
                        time:   [+16.186% +20.310% +24.285%] (p = 0.00 < 0.05)
                        thrpt:  [−19.540% −16.881% −13.931%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
graph_scaling/all_nodes/5000
                        time:   [111.27 µs 113.37 µs 115.66 µs]
                        thrpt:  [43.232 Melem/s 44.102 Melem/s 44.935 Melem/s]
                 change:
                        time:   [+4.8256% +7.3230% +9.7848%] (p = 0.00 < 0.05)
                        thrpt:  [−8.9127% −6.8234% −4.6034%]
                        Performance has regressed.
graph_scaling/nodes_within_hops/5000
                        time:   [129.53 µs 131.58 µs 133.82 µs]
                        thrpt:  [37.364 Melem/s 37.999 Melem/s 38.602 Melem/s]
                 change:
                        time:   [+11.702% +13.699% +15.824%] (p = 0.00 < 0.05)
                        thrpt:  [−13.662% −12.049% −10.476%]
                        Performance has regressed.

capability_search/find_with_gpu
                        time:   [27.588 µs 27.648 µs 27.714 µs]
                        thrpt:  [36.083 Kelem/s 36.169 Kelem/s 36.247 Kelem/s]
                 change:
                        time:   [−1.5554% −1.2071% −0.8546%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8619% +1.2219% +1.5799%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
capability_search/find_by_tool_python
                        time:   [59.598 µs 59.701 µs 59.834 µs]
                        thrpt:  [16.713 Kelem/s 16.750 Kelem/s 16.779 Kelem/s]
                 change:
                        time:   [−2.9294% −2.5357% −2.1386%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1854% +2.6017% +3.0178%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
capability_search/find_by_tool_rust
                        time:   [77.970 µs 78.087 µs 78.225 µs]
                        thrpt:  [12.784 Kelem/s 12.806 Kelem/s 12.826 Kelem/s]
                 change:
                        time:   [−2.2208% −1.9061% −1.5989%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6249% +1.9431% +2.2712%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe

graph_concurrent/concurrent_pingwave/4
                        time:   [112.45 µs 112.78 µs 113.37 µs]
                        thrpt:  [17.642 Melem/s 17.733 Melem/s 17.785 Melem/s]
                 change:
                        time:   [−1.0819% +1.1314% +4.1140%] (p = 0.47 > 0.05)
                        thrpt:  [−3.9514% −1.1188% +1.0937%]
                        No change in performance detected.
Found 4 outliers among 20 measurements (20.00%)
  4 (20.00%) high severe
graph_concurrent/concurrent_pingwave/8
                        time:   [183.16 µs 185.60 µs 189.10 µs]
                        thrpt:  [21.153 Melem/s 21.552 Melem/s 21.839 Melem/s]
                 change:
                        time:   [+2.4152% +4.2618% +6.1670%] (p = 0.00 < 0.05)
                        thrpt:  [−5.8088% −4.0876% −2.3583%]
                        Performance has regressed.
graph_concurrent/concurrent_pingwave/16
                        time:   [330.87 µs 332.44 µs 334.30 µs]
                        thrpt:  [23.931 Melem/s 24.064 Melem/s 24.179 Melem/s]
                 change:
                        time:   [−1.3115% −0.1451% +1.0569%] (p = 0.81 > 0.05)
                        thrpt:  [−1.0458% +0.1453% +1.3289%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

path_finding/path_1_hop time:   [2.2974 µs 2.3039 µs 2.3108 µs]
                        thrpt:  [432.75 Kelem/s 434.05 Kelem/s 435.28 Kelem/s]
                 change:
                        time:   [−0.2003% +0.1650% +0.5275%] (p = 0.37 > 0.05)
                        thrpt:  [−0.5248% −0.1647% +0.2007%]
                        No change in performance detected.
path_finding/path_2_hops
                        time:   [2.3276 µs 2.3335 µs 2.3397 µs]
                        thrpt:  [427.40 Kelem/s 428.54 Kelem/s 429.63 Kelem/s]
                 change:
                        time:   [−0.8577% −0.4478% −0.0602%] (p = 0.03 < 0.05)
                        thrpt:  [+0.0602% +0.4498% +0.8652%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
path_finding/path_4_hops
                        time:   [2.6929 µs 2.6945 µs 2.6963 µs]
                        thrpt:  [370.89 Kelem/s 371.12 Kelem/s 371.35 Kelem/s]
                 change:
                        time:   [+1.5554% +1.7563% +1.9505%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9131% −1.7260% −1.5316%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  2 (2.00%) high severe
path_finding/path_not_found
                        time:   [2.4390 µs 2.4460 µs 2.4544 µs]
                        thrpt:  [407.43 Kelem/s 408.83 Kelem/s 410.00 Kelem/s]
                 change:
                        time:   [+2.1498% +2.6420% +3.2027%] (p = 0.00 < 0.05)
                        thrpt:  [−3.1033% −2.5740% −2.1046%]
                        Performance has regressed.
path_finding/path_complex_graph
                        time:   [259.87 µs 260.29 µs 260.73 µs]
                        thrpt:  [3.8353 Kelem/s 3.8418 Kelem/s 3.8480 Kelem/s]
                 change:
                        time:   [−0.5037% +0.1246% +0.6944%] (p = 0.70 > 0.05)
                        thrpt:  [−0.6896% −0.1245% +0.5063%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe

failure_detector/heartbeat_existing
                        time:   [39.446 ns 39.762 ns 40.017 ns]
                        thrpt:  [24.990 Melem/s 25.150 Melem/s 25.351 Melem/s]
                 change:
                        time:   [−1.5063% −0.8415% −0.1042%] (p = 0.02 < 0.05)
                        thrpt:  [+0.1043% +0.8486% +1.5293%]
                        Change within noise threshold.
Found 21 outliers among 100 measurements (21.00%)
  8 (8.00%) low severe
  7 (7.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
failure_detector/heartbeat_new
                        time:   [234.90 ns 237.77 ns 240.14 ns]
                        thrpt:  [4.1642 Melem/s 4.2058 Melem/s 4.2572 Melem/s]
                 change:
                        time:   [−4.0839% +0.1370% +4.6589%] (p = 0.95 > 0.05)
                        thrpt:  [−4.4515% −0.1368% +4.2578%]
                        No change in performance detected.
failure_detector/status_check
                        time:   [14.847 ns 15.102 ns 15.344 ns]
                        thrpt:  [65.173 Melem/s 66.218 Melem/s 67.352 Melem/s]
                 change:
                        time:   [−1.0189% +0.9480% +2.9767%] (p = 0.35 > 0.05)
                        thrpt:  [−2.8907% −0.9391% +1.0294%]
                        No change in performance detected.
failure_detector/check_all
                        time:   [12.145 µs 12.198 µs 12.262 µs]
                        thrpt:  [81.555 Kelem/s 81.978 Kelem/s 82.342 Kelem/s]
                 change:
                        time:   [−0.2373% +0.0776% +0.4306%] (p = 0.66 > 0.05)
                        thrpt:  [−0.4287% −0.0776% +0.2378%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
failure_detector/stats  time:   [10.687 µs 10.749 µs 10.814 µs]
                        thrpt:  [92.472 Kelem/s 93.029 Kelem/s 93.575 Kelem/s]
                 change:
                        time:   [+0.6768% +1.0862% +1.5290%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5059% −1.0746% −0.6723%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

loss_simulator/should_drop_1pct
                        time:   [2.7844 ns 2.7864 ns 2.7890 ns]
                        thrpt:  [358.55 Melem/s 358.89 Melem/s 359.14 Melem/s]
                 change:
                        time:   [−0.2545% −0.0274% +0.1919%] (p = 0.81 > 0.05)
                        thrpt:  [−0.1915% +0.0274% +0.2552%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
loss_simulator/should_drop_5pct
                        time:   [3.1431 ns 3.1451 ns 3.1475 ns]
                        thrpt:  [317.71 Melem/s 317.96 Melem/s 318.15 Melem/s]
                 change:
                        time:   [−0.1031% +0.1908% +0.5764%] (p = 0.27 > 0.05)
                        thrpt:  [−0.5731% −0.1905% +0.1032%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  5 (5.00%) high mild
  12 (12.00%) high severe
loss_simulator/should_drop_10pct
                        time:   [3.6086 ns 3.6118 ns 3.6160 ns]
                        thrpt:  [276.55 Melem/s 276.87 Melem/s 277.11 Melem/s]
                 change:
                        time:   [−0.3089% −0.0654% +0.1521%] (p = 0.59 > 0.05)
                        thrpt:  [−0.1518% +0.0654% +0.3098%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
loss_simulator/should_drop_20pct
                        time:   [4.5623 ns 4.5659 ns 4.5702 ns]
                        thrpt:  [218.81 Melem/s 219.01 Melem/s 219.19 Melem/s]
                 change:
                        time:   [−0.0528% +0.1728% +0.4043%] (p = 0.14 > 0.05)
                        thrpt:  [−0.4026% −0.1725% +0.0529%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
loss_simulator/should_drop_burst
                        time:   [2.9168 ns 2.9188 ns 2.9213 ns]
                        thrpt:  [342.31 Melem/s 342.60 Melem/s 342.84 Melem/s]
                 change:
                        time:   [−0.6729% −0.3424% −0.0441%] (p = 0.03 < 0.05)
                        thrpt:  [+0.0441% +0.3436% +0.6775%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe

circuit_breaker/allow_closed
                        time:   [9.5150 ns 9.5519 ns 9.5958 ns]
                        thrpt:  [104.21 Melem/s 104.69 Melem/s 105.10 Melem/s]
                 change:
                        time:   [−0.2441% +0.0438% +0.3331%] (p = 0.77 > 0.05)
                        thrpt:  [−0.3319% −0.0438% +0.2447%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe
circuit_breaker/record_success
                        time:   [8.3791 ns 8.3906 ns 8.4015 ns]
                        thrpt:  [119.03 Melem/s 119.18 Melem/s 119.34 Melem/s]
                 change:
                        time:   [−0.3368% +0.0195% +0.3696%] (p = 0.91 > 0.05)
                        thrpt:  [−0.3682% −0.0195% +0.3379%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low mild
  7 (7.00%) high mild
circuit_breaker/record_failure
                        time:   [7.4122 ns 7.4171 ns 7.4223 ns]
                        thrpt:  [134.73 Melem/s 134.82 Melem/s 134.91 Melem/s]
                 change:
                        time:   [−0.3065% −0.0700% +0.2008%] (p = 0.61 > 0.05)
                        thrpt:  [−0.2004% +0.0701% +0.3075%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
circuit_breaker/state   time:   [9.5029 ns 9.5163 ns 9.5328 ns]
                        thrpt:  [104.90 Melem/s 105.08 Melem/s 105.23 Melem/s]
                 change:
                        time:   [+0.0836% +0.5201% +1.0458%] (p = 0.02 < 0.05)
                        thrpt:  [−1.0350% −0.5175% −0.0835%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe

recovery_manager/on_failure_with_alternates
                        time:   [249.79 ns 257.51 ns 264.46 ns]
                        thrpt:  [3.7813 Melem/s 3.8833 Melem/s 4.0034 Melem/s]
                 change:
                        time:   [−6.2996% −1.4510% +3.4589%] (p = 0.57 > 0.05)
                        thrpt:  [−3.3432% +1.4723% +6.7231%]
                        No change in performance detected.
recovery_manager/on_failure_no_alternates
                        time:   [173.49 ns 178.22 ns 182.34 ns]
                        thrpt:  [5.4843 Melem/s 5.6111 Melem/s 5.7640 Melem/s]
                 change:
                        time:   [−36.511% −29.412% −21.438%] (p = 0.00 < 0.05)
                        thrpt:  [+27.289% +41.667% +57.508%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
recovery_manager/get_action
                        time:   [37.116 ns 37.147 ns 37.184 ns]
                        thrpt:  [26.893 Melem/s 26.920 Melem/s 26.943 Melem/s]
                 change:
                        time:   [+0.7238% +1.2343% +1.7766%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7456% −1.2193% −0.7186%]
                        Change within noise threshold.
Found 23 outliers among 100 measurements (23.00%)
  2 (2.00%) high mild
  21 (21.00%) high severe
recovery_manager/is_failed
                        time:   [13.542 ns 13.954 ns 14.343 ns]
                        thrpt:  [69.723 Melem/s 71.664 Melem/s 73.842 Melem/s]
                 change:
                        time:   [−7.3374% −4.3415% −1.2144%] (p = 0.01 < 0.05)
                        thrpt:  [+1.2293% +4.5385% +7.9185%]
                        Performance has improved.
recovery_manager/on_recovery
                        time:   [98.957 ns 99.187 ns 99.466 ns]
                        thrpt:  [10.054 Melem/s 10.082 Melem/s 10.105 Melem/s]
                 change:
                        time:   [−0.8376% −0.3413% +0.0666%] (p = 0.16 > 0.05)
                        thrpt:  [−0.0665% +0.3424% +0.8446%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) low mild
  6 (6.00%) high mild
  8 (8.00%) high severe
recovery_manager/stats  time:   [698.39 ps 699.22 ps 700.33 ps]
                        thrpt:  [1.4279 Gelem/s 1.4302 Gelem/s 1.4319 Gelem/s]
                 change:
                        time:   [−0.2945% −0.0209% +0.1919%] (p = 0.88 > 0.05)
                        thrpt:  [−0.1915% +0.0209% +0.2954%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe

failure_scaling/check_all/100
                        time:   [2.4699 µs 2.4792 µs 2.4893 µs]
                        thrpt:  [40.171 Melem/s 40.336 Melem/s 40.487 Melem/s]
                 change:
                        time:   [−9.1562% −3.2365% +0.4722%] (p = 0.32 > 0.05)
                        thrpt:  [−0.4700% +3.3448% +10.079%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) low mild
failure_scaling/healthy_nodes/100
                        time:   [2.1495 µs 2.1508 µs 2.1524 µs]
                        thrpt:  [46.460 Melem/s 46.494 Melem/s 46.522 Melem/s]
                 change:
                        time:   [−0.5486% −0.2988% −0.0389%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0389% +0.2997% +0.5516%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
failure_scaling/check_all/500
                        time:   [6.5135 µs 6.5892 µs 6.6626 µs]
                        thrpt:  [75.046 Melem/s 75.882 Melem/s 76.764 Melem/s]
                 change:
                        time:   [−0.5452% +0.5546% +1.7716%] (p = 0.35 > 0.05)
                        thrpt:  [−1.7407% −0.5515% +0.5482%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
failure_scaling/healthy_nodes/500
                        time:   [6.1207 µs 6.1351 µs 6.1529 µs]
                        thrpt:  [81.263 Melem/s 81.498 Melem/s 81.690 Melem/s]
                 change:
                        time:   [−0.2312% +0.0190% +0.2718%] (p = 0.89 > 0.05)
                        thrpt:  [−0.2710% −0.0190% +0.2317%]
                        No change in performance detected.
Found 23 outliers among 100 measurements (23.00%)
  7 (7.00%) low mild
  5 (5.00%) high mild
  11 (11.00%) high severe
failure_scaling/check_all/1000
                        time:   [12.090 µs 12.105 µs 12.120 µs]
                        thrpt:  [82.508 Melem/s 82.610 Melem/s 82.714 Melem/s]
                 change:
                        time:   [+4.1190% +4.8932% +5.6706%] (p = 0.00 < 0.05)
                        thrpt:  [−5.3663% −4.6650% −3.9560%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
failure_scaling/healthy_nodes/1000
                        time:   [10.597 µs 10.616 µs 10.645 µs]
                        thrpt:  [93.939 Melem/s 94.197 Melem/s 94.367 Melem/s]
                 change:
                        time:   [−3.8614% −3.6762% −3.4570%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5808% +3.8165% +4.0165%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
failure_scaling/check_all/5000
                        time:   [54.569 µs 54.931 µs 55.211 µs]
                        thrpt:  [90.561 Melem/s 91.023 Melem/s 91.627 Melem/s]
                 change:
                        time:   [−1.2626% −0.0742% +1.0672%] (p = 0.90 > 0.05)
                        thrpt:  [−1.0560% +0.0742% +1.2788%]
                        No change in performance detected.
failure_scaling/healthy_nodes/5000
                        time:   [50.809 µs 50.910 µs 51.009 µs]
                        thrpt:  [98.023 Melem/s 98.212 Melem/s 98.408 Melem/s]
                 change:
                        time:   [+0.0273% +0.3916% +0.7557%] (p = 0.04 < 0.05)
                        thrpt:  [−0.7500% −0.3901% −0.0273%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

failure_concurrent/concurrent_heartbeat/4
                        time:   [195.72 µs 197.85 µs 201.57 µs]
                        thrpt:  [9.9222 Melem/s 10.109 Melem/s 10.219 Melem/s]
                 change:
                        time:   [−2.3475% −1.3346% −0.2329%] (p = 0.02 < 0.05)
                        thrpt:  [+0.2334% +1.3527% +2.4040%]
                        Change within noise threshold.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high severe
failure_concurrent/concurrent_heartbeat/8
                        time:   [259.02 µs 259.48 µs 260.13 µs]
                        thrpt:  [15.377 Melem/s 15.416 Melem/s 15.443 Melem/s]
                 change:
                        time:   [−6.9076% −2.4549% +0.3623%] (p = 0.31 > 0.05)
                        thrpt:  [−0.3610% +2.5167% +7.4201%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
failure_concurrent/concurrent_heartbeat/16
                        time:   [480.10 µs 486.95 µs 499.53 µs]
                        thrpt:  [16.015 Melem/s 16.429 Melem/s 16.663 Melem/s]
                 change:
                        time:   [−9.0634% +0.2714% +7.2238%] (p = 0.96 > 0.05)
                        thrpt:  [−6.7371% −0.2707% +9.9667%]
                        No change in performance detected.

failure_recovery_cycle/full_cycle
                        time:   [285.14 ns 290.75 ns 295.48 ns]
                        thrpt:  [3.3843 Melem/s 3.4394 Melem/s 3.5070 Melem/s]
                 change:
                        time:   [−2.6960% +1.2661% +5.6208%] (p = 0.55 > 0.05)
                        thrpt:  [−5.3216% −1.2502% +2.7707%]
                        No change in performance detected.

capability_set/create   time:   [19.009 µs 19.025 µs 19.042 µs]
                        thrpt:  [52.515 Kelem/s 52.563 Kelem/s 52.608 Kelem/s]
                 change:
                        time:   [+0.1521% +0.3511% +0.5412%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5383% −0.3499% −0.1519%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
capability_set/serialize
                        time:   [10.770 µs 10.787 µs 10.805 µs]
                        thrpt:  [92.548 Kelem/s 92.703 Kelem/s 92.850 Kelem/s]
                 change:
                        time:   [+1.7289% +1.9568% +2.2175%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1694% −1.9193% −1.6995%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  10 (10.00%) high mild
  2 (2.00%) high severe
capability_set/deserialize
                        time:   [10.036 µs 10.078 µs 10.127 µs]
                        thrpt:  [98.746 Kelem/s 99.225 Kelem/s 99.640 Kelem/s]
                 change:
                        time:   [−1.2331% −0.2698% +0.5196%] (p = 0.59 > 0.05)
                        thrpt:  [−0.5169% +0.2705% +1.2485%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
capability_set/roundtrip
                        time:   [21.017 µs 21.072 µs 21.126 µs]
                        thrpt:  [47.336 Kelem/s 47.456 Kelem/s 47.580 Kelem/s]
                 change:
                        time:   [+0.0012% +0.4561% +0.8890%] (p = 0.05 < 0.05)
                        thrpt:  [−0.8811% −0.4540% −0.0012%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
capability_set/serialize_compact
                        time:   [2.6754 µs 2.6898 µs 2.7050 µs]
                        thrpt:  [369.68 Kelem/s 371.78 Kelem/s 373.78 Kelem/s]
                 change:
                        time:   [−0.8257% −0.2368% +0.3760%] (p = 0.45 > 0.05)
                        thrpt:  [−0.3746% +0.2374% +0.8326%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_set/deserialize_compact
                        time:   [7.3846 µs 7.3952 µs 7.4070 µs]
                        thrpt:  [135.01 Kelem/s 135.22 Kelem/s 135.42 Kelem/s]
                 change:
                        time:   [−11.235% −4.2331% −0.1432%] (p = 0.22 > 0.05)
                        thrpt:  [+0.1434% +4.4202% +12.658%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_set/roundtrip_compact
                        time:   [9.9307 µs 9.9671 µs 10.002 µs]
                        thrpt:  [99.982 Kelem/s 100.33 Kelem/s 100.70 Kelem/s]
                 change:
                        time:   [+1.3479% +1.7367% +2.1704%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1243% −1.7071% −1.3300%]
                        Performance has regressed.
capability_set/has_tag  time:   [46.726 ns 46.747 ns 46.779 ns]
                        thrpt:  [21.377 Melem/s 21.392 Melem/s 21.401 Melem/s]
                 change:
                        time:   [−45.883% −45.272% −44.828%] (p = 0.00 < 0.05)
                        thrpt:  [+81.250% +82.722% +84.785%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
capability_set/has_model
                        time:   [37.245 ns 37.260 ns 37.282 ns]
                        thrpt:  [26.822 Melem/s 26.839 Melem/s 26.849 Melem/s]
                 change:
                        time:   [−5.1918% −4.9603% −4.7549%] (p = 0.00 < 0.05)
                        thrpt:  [+4.9922% +5.2192% +5.4761%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) high mild
  7 (7.00%) high severe
capability_set/has_tool time:   [63.002 ns 63.146 ns 63.299 ns]
                        thrpt:  [15.798 Melem/s 15.836 Melem/s 15.873 Melem/s]
                 change:
                        time:   [+189.28% +189.87% +190.41%] (p = 0.00 < 0.05)
                        thrpt:  [−65.566% −65.502% −65.432%]
                        Performance has regressed.
capability_set/has_gpu  time:   [40.287 ns 40.519 ns 40.741 ns]
                        thrpt:  [24.545 Melem/s 24.680 Melem/s 24.822 Melem/s]
                 change:
                        time:   [+0.9409% +1.3328% +1.6990%] (p = 0.00 < 0.05)
                        thrpt:  [−1.6706% −1.3153% −0.9321%]
                        Change within noise threshold.
Found 21 outliers among 100 measurements (21.00%)
  16 (16.00%) high mild
  5 (5.00%) high severe

capability_announcement/create
                        time:   [3.4087 µs 3.4149 µs 3.4212 µs]
                        thrpt:  [292.29 Kelem/s 292.83 Kelem/s 293.36 Kelem/s]
                 change:
                        time:   [−1.2065% −0.8058% −0.4197%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4215% +0.8123% +1.2213%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
capability_announcement/serialize
                        time:   [11.104 µs 11.132 µs 11.160 µs]
                        thrpt:  [89.607 Kelem/s 89.831 Kelem/s 90.054 Kelem/s]
                 change:
                        time:   [+0.3119% +0.8860% +1.7851%] (p = 0.01 < 0.05)
                        thrpt:  [−1.7538% −0.8783% −0.3109%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_announcement/deserialize
                        time:   [10.338 µs 10.354 µs 10.370 µs]
                        thrpt:  [96.435 Kelem/s 96.583 Kelem/s 96.729 Kelem/s]
                 change:
                        time:   [+0.1258% +0.3435% +0.5642%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5610% −0.3424% −0.1257%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_announcement/is_expired
                        time:   [25.164 ns 25.175 ns 25.191 ns]
                        thrpt:  [39.697 Melem/s 39.722 Melem/s 39.740 Melem/s]
                 change:
                        time:   [−0.2096% −0.0202% +0.1604%] (p = 0.83 > 0.05)
                        thrpt:  [−0.1601% +0.0202% +0.2100%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

capability_filter/match_single_tag
                        time:   [57.112 ns 57.136 ns 57.178 ns]
                        thrpt:  [17.489 Melem/s 17.502 Melem/s 17.510 Melem/s]
                 change:
                        time:   [−14.162% −13.992% −13.807%] (p = 0.00 < 0.05)
                        thrpt:  [+16.019% +16.268% +16.499%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
capability_filter/match_require_gpu
                        time:   [46.691 ns 46.726 ns 46.772 ns]
                        thrpt:  [21.380 Melem/s 21.402 Melem/s 21.417 Melem/s]
                 change:
                        time:   [−1.2115% −0.7962% −0.4154%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4171% +0.8026% +1.2264%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
capability_filter/match_gpu_vendor
                        time:   [150.77 ns 152.37 ns 153.83 ns]
                        thrpt:  [6.5008 Melem/s 6.5628 Melem/s 6.6328 Melem/s]
                 change:
                        time:   [+1.5555% +3.0028% +4.5555%] (p = 0.00 < 0.05)
                        thrpt:  [−4.3570% −2.9152% −1.5316%]
                        Performance has regressed.
capability_filter/match_min_memory
                        time:   [31.352 ns 31.373 ns 31.401 ns]
                        thrpt:  [31.846 Melem/s 31.874 Melem/s 31.896 Melem/s]
                 change:
                        time:   [+1.6942% +2.9298% +3.9200%] (p = 0.00 < 0.05)
                        thrpt:  [−3.7721% −2.8464% −1.6659%]
                        Performance has regressed.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
capability_filter/match_complex
                        time:   [4.6967 µs 4.7046 µs 4.7125 µs]
                        thrpt:  [212.20 Kelem/s 212.56 Kelem/s 212.91 Kelem/s]
                 change:
                        time:   [+0.6135% +0.9212% +1.2283%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2134% −0.9128% −0.6097%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
capability_filter/match_no_match
                        time:   [83.359 ns 83.412 ns 83.475 ns]
                        thrpt:  [11.980 Melem/s 11.989 Melem/s 11.996 Melem/s]
                 change:
                        time:   [+0.2202% +0.3469% +0.4901%] (p = 0.00 < 0.05)
                        thrpt:  [−0.4877% −0.3457% −0.2197%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe

capability_fold_insert/index_nodes/100
                        time:   [4.0262 ms 4.0300 ms 4.0347 ms]
                        thrpt:  [24.785 Kelem/s 24.814 Kelem/s 24.837 Kelem/s]
                 change:
                        time:   [−13.428% −5.6264% −0.8711%] (p = 0.18 > 0.05)
                        thrpt:  [+0.8788% +5.9619% +15.511%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
capability_fold_insert/index_nodes/1000
                        time:   [40.444 ms 40.637 ms 40.857 ms]
                        thrpt:  [24.476 Kelem/s 24.608 Kelem/s 24.726 Kelem/s]
                 change:
                        time:   [−0.2616% +0.3186% +0.8832%] (p = 0.29 > 0.05)
                        thrpt:  [−0.8755% −0.3175% +0.2623%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 44.0s, or reduce sample count to 10.
capability_fold_insert/index_nodes/10000
                        time:   [422.32 ms 423.46 ms 424.91 ms]
                        thrpt:  [23.534 Kelem/s 23.615 Kelem/s 23.679 Kelem/s]
                 change:
                        time:   [−1.7838% −0.9576% −0.2169%] (p = 0.02 < 0.05)
                        thrpt:  [+0.2173% +0.9669% +1.8162%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe

capability_fold_query/query_single_tag
                        time:   [109.16 µs 109.69 µs 110.34 µs]
                        thrpt:  [9.0630 Kelem/s 9.1163 Kelem/s 9.1609 Kelem/s]
                 change:
                        time:   [−9.7869% −3.6595% −0.0909%] (p = 0.22 > 0.05)
                        thrpt:  [+0.0910% +3.7986% +10.849%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
capability_fold_query/query_require_gpu
                        time:   [286.41 µs 299.35 µs 314.35 µs]
                        thrpt:  [3.1811 Kelem/s 3.3406 Kelem/s 3.4915 Kelem/s]
                 change:
                        time:   [+10.972% +14.355% +18.432%] (p = 0.00 < 0.05)
                        thrpt:  [−15.563% −12.553% −9.8873%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_gpu_vendor
                        time:   [411.29 µs 438.66 µs 471.11 µs]
                        thrpt:  [2.1226 Kelem/s 2.2797 Kelem/s 2.4313 Kelem/s]
                 change:
                        time:   [+16.346% +25.318% +34.370%] (p = 0.00 < 0.05)
                        thrpt:  [−25.579% −20.203% −14.049%]
                        Performance has regressed.
capability_fold_query/query_min_memory
                        time:   [327.93 µs 345.90 µs 366.59 µs]
                        thrpt:  [2.7278 Kelem/s 2.8910 Kelem/s 3.0495 Kelem/s]
                 change:
                        time:   [−8.3744% −3.3842% +1.9577%] (p = 0.18 > 0.05)
                        thrpt:  [−1.9202% +3.5027% +9.1397%]
                        No change in performance detected.
Found 20 outliers among 100 measurements (20.00%)
  6 (6.00%) high mild
  14 (14.00%) high severe
capability_fold_query/query_complex
                        time:   [296.27 µs 325.36 µs 357.69 µs]
                        thrpt:  [2.7957 Kelem/s 3.0735 Kelem/s 3.3753 Kelem/s]
                 change:
                        time:   [+35.851% +47.704% +61.434%] (p = 0.00 < 0.05)
                        thrpt:  [−38.055% −32.297% −26.390%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
capability_fold_query/query_model
                        time:   [71.333 µs 71.391 µs 71.474 µs]
                        thrpt:  [13.991 Kelem/s 14.007 Kelem/s 14.019 Kelem/s]
                 change:
                        time:   [−0.3876% −0.2109% +0.0041%] (p = 0.03 < 0.05)
                        thrpt:  [−0.0041% +0.2114% +0.3891%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
capability_fold_query/query_tool
                        time:   [266.35 µs 270.06 µs 274.02 µs]
                        thrpt:  [3.6494 Kelem/s 3.7028 Kelem/s 3.7545 Kelem/s]
                 change:
                        time:   [−8.8513% −5.3210% −1.9106%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9478% +5.6200% +9.7108%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
capability_fold_query/query_no_results
                        time:   [81.484 ns 81.762 ns 82.070 ns]
                        thrpt:  [12.185 Melem/s 12.231 Melem/s 12.272 Melem/s]
                 change:
                        time:   [−8.3410% −7.1775% −5.9556%] (p = 0.00 < 0.05)
                        thrpt:  [+6.3328% +7.7325% +9.1000%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

capability_fold_find_best/find_best_simple
                        time:   [265.05 µs 272.81 µs 283.68 µs]
                        thrpt:  [3.5251 Kelem/s 3.6656 Kelem/s 3.7729 Kelem/s]
                 change:
                        time:   [−16.586% −11.488% −5.8711%] (p = 0.00 < 0.05)
                        thrpt:  [+6.2374% +12.978% +19.884%]
                        Performance has improved.
Found 22 outliers among 100 measurements (22.00%)
  22 (22.00%) high severe
capability_fold_find_best/find_best_with_prefs
                        time:   [420.62 µs 456.30 µs 490.56 µs]
                        thrpt:  [2.0385 Kelem/s 2.1916 Kelem/s 2.3775 Kelem/s]
                 change:
                        time:   [−18.040% −12.352% −5.5331%] (p = 0.00 < 0.05)
                        thrpt:  [+5.8572% +14.093% +22.011%]
                        Performance has improved.

capability_fold_scaling/query_tag/1000
                        time:   [9.4139 µs 9.4969 µs 9.5920 µs]
                        thrpt:  [52.127 Melem/s 52.649 Melem/s 53.113 Melem/s]
                 change:
                        time:   [−0.3523% +0.0706% +0.4742%] (p = 0.76 > 0.05)
                        thrpt:  [−0.4719% −0.0706% +0.3535%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  14 (14.00%) high severe
capability_fold_scaling/query_complex/1000
                        time:   [18.404 µs 18.414 µs 18.425 µs]
                        thrpt:  [24.532 Melem/s 24.546 Melem/s 24.559 Melem/s]
                 change:
                        time:   [−0.9382% −0.7800% −0.5573%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5604% +0.7861% +0.9471%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
capability_fold_scaling/query_tag_rare/1000
                        time:   [1.8939 µs 1.8962 µs 1.8984 µs]
                        thrpt:  [52.675 Melem/s 52.738 Melem/s 52.800 Melem/s]
                 change:
                        time:   [−0.8794% −0.7014% −0.5221%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5248% +0.7064% +0.8872%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_fold_scaling/query_tag/5000
                        time:   [52.670 µs 52.894 µs 53.166 µs]
                        thrpt:  [47.022 Melem/s 47.265 Melem/s 47.466 Melem/s]
                 change:
                        time:   [−7.1741% −4.6736% −2.9402%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0293% +4.9028% +7.7285%]
                        Performance has improved.
capability_fold_scaling/query_complex/5000
                        time:   [178.22 µs 202.02 µs 225.42 µs]
                        thrpt:  [10.039 Melem/s 11.202 Melem/s 12.698 Melem/s]
                 change:
                        time:   [−2.2180% +8.1454% +18.106%] (p = 0.14 > 0.05)
                        thrpt:  [−15.331% −7.5319% +2.2683%]
                        No change in performance detected.
capability_fold_scaling/query_tag_rare/5000
                        time:   [1.8894 µs 1.8909 µs 1.8926 µs]
                        thrpt:  [52.836 Melem/s 52.884 Melem/s 52.928 Melem/s]
                 change:
                        time:   [−5.2181% −4.6000% −4.1104%] (p = 0.00 < 0.05)
                        thrpt:  [+4.2866% +4.8218% +5.5054%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
capability_fold_scaling/query_tag/10000
                        time:   [108.39 µs 108.44 µs 108.50 µs]
                        thrpt:  [46.083 Melem/s 46.108 Melem/s 46.128 Melem/s]
                 change:
                        time:   [−10.428% −8.8391% −7.4280%] (p = 0.00 < 0.05)
                        thrpt:  [+8.0240% +9.6962% +11.642%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  10 (10.00%) high severe
capability_fold_scaling/query_complex/10000
                        time:   [323.78 µs 367.38 µs 415.84 µs]
                        thrpt:  [10.891 Melem/s 12.328 Melem/s 13.988 Melem/s]
                 change:
                        time:   [−28.916% −20.450% −11.785%] (p = 0.00 < 0.05)
                        thrpt:  [+13.359% +25.707% +40.679%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_fold_scaling/query_tag_rare/10000
                        time:   [1.8983 µs 1.9007 µs 1.9030 µs]
                        thrpt:  [52.548 Melem/s 52.612 Melem/s 52.679 Melem/s]
                 change:
                        time:   [−5.6722% −4.9037% −4.3642%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5634% +5.1565% +6.0132%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
capability_fold_scaling/query_tag/50000
                        time:   [642.53 µs 643.95 µs 645.34 µs]
                        thrpt:  [38.739 Melem/s 38.823 Melem/s 38.908 Melem/s]
                 change:
                        time:   [−14.019% −13.186% −12.395%] (p = 0.00 < 0.05)
                        thrpt:  [+14.149% +15.189% +16.305%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_fold_scaling/query_complex/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.6s, enable flat sampling, or reduce sample count to 50.
capability_fold_scaling/query_complex/50000
                        time:   [1.6393 ms 1.6939 ms 1.7554 ms]
                        thrpt:  [12.905 Melem/s 13.374 Melem/s 13.819 Melem/s]
                 change:
                        time:   [−25.303% −22.849% −20.501%] (p = 0.00 < 0.05)
                        thrpt:  [+25.788% +29.616% +33.874%]
                        Performance has improved.
capability_fold_scaling/query_tag_rare/50000
                        time:   [1.9070 µs 1.9104 µs 1.9138 µs]
                        thrpt:  [52.251 Melem/s 52.345 Melem/s 52.438 Melem/s]
                 change:
                        time:   [−4.7274% −4.2040% −3.7705%] (p = 0.00 < 0.05)
                        thrpt:  [+3.9182% +4.3885% +4.9620%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild

capability_fold_concurrent/concurrent_index/4
                        time:   [15.558 ms 15.573 ms 15.586 ms]
                        thrpt:  [128.32 Kelem/s 128.43 Kelem/s 128.55 Kelem/s]
                 change:
                        time:   [−7.4156% −3.4438% −1.0692%] (p = 0.05 > 0.05)
                        thrpt:  [+1.0808% +3.5666% +8.0096%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_query/4
                        time:   [154.55 ms 161.43 ms 169.55 ms]
                        thrpt:  [11.796 Kelem/s 12.389 Kelem/s 12.941 Kelem/s]
                 change:
                        time:   [−22.859% −18.757% −13.733%] (p = 0.00 < 0.05)
                        thrpt:  [+15.920% +23.088% +29.633%]
                        Performance has improved.
Found 4 outliers among 20 measurements (20.00%)
  3 (15.00%) high mild
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_mixed/4
                        time:   [60.739 ms 61.375 ms 62.102 ms]
                        thrpt:  [32.205 Kelem/s 32.587 Kelem/s 32.928 Kelem/s]
                 change:
                        time:   [−1.8038% −0.2553% +1.1640%] (p = 0.75 > 0.05)
                        thrpt:  [−1.1506% +0.2559% +1.8369%]
                        No change in performance detected.
capability_fold_concurrent/concurrent_index/8
                        time:   [16.258 ms 16.313 ms 16.377 ms]
                        thrpt:  [244.25 Kelem/s 245.20 Kelem/s 246.04 Kelem/s]
                 change:
                        time:   [−4.9330% −3.2756% −1.6357%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6629% +3.3865% +5.1890%]
                        Performance has improved.
Found 5 outliers among 20 measurements (25.00%)
  2 (10.00%) low mild
  2 (10.00%) high mild
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_query/8
                        time:   [165.05 ms 171.05 ms 178.07 ms]
                        thrpt:  [22.463 Kelem/s 23.386 Kelem/s 24.234 Kelem/s]
                 change:
                        time:   [−8.1762% −1.0033% +5.9834%] (p = 0.81 > 0.05)
                        thrpt:  [−5.6456% +1.0134% +8.9042%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
capability_fold_concurrent/concurrent_mixed/8
                        time:   [70.617 ms 70.784 ms 70.974 ms]
                        thrpt:  [56.359 Kelem/s 56.510 Kelem/s 56.643 Kelem/s]
                 change:
                        time:   [−11.335% −10.407% −9.5086%] (p = 0.00 < 0.05)
                        thrpt:  [+10.508% +11.616% +12.784%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_index/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.5s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/16
                        time:   [34.437 ms 34.600 ms 34.773 ms]
                        thrpt:  [230.06 Kelem/s 231.21 Kelem/s 232.31 Kelem/s]
                 change:
                        time:   [−7.6700% −6.2761% −5.0383%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3056% +6.6964% +8.3071%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking capability_fold_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.4s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/16
                        time:   [269.57 ms 272.98 ms 276.44 ms]
                        thrpt:  [28.939 Kelem/s 29.306 Kelem/s 29.676 Kelem/s]
                 change:
                        time:   [−16.231% −11.455% −6.5500%] (p = 0.00 < 0.05)
                        thrpt:  [+7.0091% +12.937% +19.376%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
capability_fold_concurrent/concurrent_mixed/16
                        time:   [177.69 ms 178.62 ms 179.71 ms]
                        thrpt:  [44.516 Kelem/s 44.788 Kelem/s 45.022 Kelem/s]
                 change:
                        time:   [−7.4565% −5.9905% −4.5933%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8145% +6.3722% +8.0573%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild

capability_fold_updates/update_higher_version
                        time:   [29.578 µs 29.732 µs 29.895 µs]
                        thrpt:  [33.450 Kelem/s 33.633 Kelem/s 33.809 Kelem/s]
                 change:
                        time:   [−5.3417% −4.5672% −3.9073%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0662% +4.7858% +5.6432%]
                        Performance has improved.
capability_fold_updates/update_same_version
                        time:   [29.137 µs 29.339 µs 29.559 µs]
                        thrpt:  [33.831 Kelem/s 34.084 Kelem/s 34.321 Kelem/s]
                 change:
                        time:   [−7.6730% −6.6495% −5.7459%] (p = 0.00 < 0.05)
                        thrpt:  [+6.0962% +7.1231% +8.3107%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  12 (12.00%) high mild
  4 (4.00%) high severe
capability_fold_updates/remove_and_readd
                        time:   [44.600 µs 44.893 µs 45.201 µs]
                        thrpt:  [22.123 Kelem/s 22.275 Kelem/s 22.422 Kelem/s]
                 change:
                        time:   [−13.161% −12.038% −10.899%] (p = 0.00 < 0.05)
                        thrpt:  [+12.232% +13.686% +15.156%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

location_info/create    time:   [59.063 ns 59.523 ns 59.962 ns]
                        thrpt:  [16.677 Melem/s 16.800 Melem/s 16.931 Melem/s]
                 change:
                        time:   [+1.1428% +1.8383% +2.5157%] (p = 0.00 < 0.05)
                        thrpt:  [−2.4540% −1.8051% −1.1299%]
                        Performance has regressed.
location_info/distance_to
                        time:   [4.2256 ns 4.2442 ns 4.2637 ns]
                        thrpt:  [234.54 Melem/s 235.62 Melem/s 236.65 Melem/s]
                 change:
                        time:   [−2.3268% −2.0486% −1.7679%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7997% +2.0914% +2.3822%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
location_info/same_continent
                        time:   [7.1386 ns 7.1424 ns 7.1482 ns]
                        thrpt:  [139.90 Melem/s 140.01 Melem/s 140.08 Melem/s]
                 change:
                        time:   [−5.2785% −3.6866% −2.4487%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5101% +3.8278% +5.5726%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
location_info/same_continent_cross
                        time:   [310.37 ps 310.51 ps 310.75 ps]
                        thrpt:  [3.2180 Gelem/s 3.2205 Gelem/s 3.2220 Gelem/s]
                 change:
                        time:   [−3.1399% −2.3239% −1.8114%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8448% +2.3792% +3.2417%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
location_info/same_region
                        time:   [4.0355 ns 4.0376 ns 4.0403 ns]
                        thrpt:  [247.50 Melem/s 247.67 Melem/s 247.80 Melem/s]
                 change:
                        time:   [−3.2457% −2.2821% −1.7002%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7296% +2.3354% +3.3546%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

topology_hints/create   time:   [3.1130 ns 3.1166 ns 3.1206 ns]
                        thrpt:  [320.46 Melem/s 320.86 Melem/s 321.24 Melem/s]
                 change:
                        time:   [−3.3949% −2.5402% −1.9348%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9729% +2.6064% +3.5143%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
topology_hints/connectivity_score
                        time:   [310.41 ps 310.54 ps 310.73 ps]
                        thrpt:  [3.2183 Gelem/s 3.2201 Gelem/s 3.2216 Gelem/s]
                 change:
                        time:   [−2.3627% −1.0883% −0.1616%] (p = 0.03 < 0.05)
                        thrpt:  [+0.1619% +1.1003% +2.4199%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
topology_hints/average_latency_empty
                        time:   [621.17 ps 622.43 ps 624.13 ps]
                        thrpt:  [1.6022 Gelem/s 1.6066 Gelem/s 1.6099 Gelem/s]
                 change:
                        time:   [−0.0275% +0.1664% +0.3778%] (p = 0.12 > 0.05)
                        thrpt:  [−0.3763% −0.1661% +0.0275%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe
topology_hints/average_latency_100
                        time:   [70.405 ns 70.444 ns 70.493 ns]
                        thrpt:  [14.186 Melem/s 14.196 Melem/s 14.203 Melem/s]
                 change:
                        time:   [−0.2370% −0.0152% +0.2265%] (p = 0.90 > 0.05)
                        thrpt:  [−0.2260% +0.0152% +0.2376%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) high mild
  9 (9.00%) high severe

nat_type/difficulty     time:   [310.36 ps 310.49 ps 310.69 ps]
                        thrpt:  [3.2186 Gelem/s 3.2208 Gelem/s 3.2221 Gelem/s]
                 change:
                        time:   [−3.1974% −1.0134% +0.2098%] (p = 0.55 > 0.05)
                        thrpt:  [−0.2094% +1.0238% +3.3030%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
nat_type/can_connect_direct
                        time:   [310.40 ps 310.53 ps 310.70 ps]
                        thrpt:  [3.2186 Gelem/s 3.2203 Gelem/s 3.2216 Gelem/s]
                 change:
                        time:   [−0.1607% −0.0106% +0.1652%] (p = 0.91 > 0.05)
                        thrpt:  [−0.1649% +0.0106% +0.1610%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
nat_type/can_connect_symmetric
                        time:   [310.62 ps 310.81 ps 311.04 ps]
                        thrpt:  [3.2150 Gelem/s 3.2174 Gelem/s 3.2193 Gelem/s]
                 change:
                        time:   [+0.1547% +0.3225% +0.4964%] (p = 0.00 < 0.05)
                        thrpt:  [−0.4939% −0.3215% −0.1545%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe

node_metadata/create_simple
                        time:   [50.595 ns 50.680 ns 50.769 ns]
                        thrpt:  [19.697 Melem/s 19.732 Melem/s 19.765 Melem/s]
                 change:
                        time:   [−1.0900% −0.7208% −0.3827%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3841% +0.7260% +1.1021%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
node_metadata/create_full
                        time:   [624.63 ns 626.37 ns 627.94 ns]
                        thrpt:  [1.5925 Melem/s 1.5965 Melem/s 1.6009 Melem/s]
                 change:
                        time:   [+3.8945% +4.2496% +4.5888%] (p = 0.00 < 0.05)
                        thrpt:  [−4.3875% −4.0763% −3.7485%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
node_metadata/routing_score
                        time:   [2.9487 ns 2.9546 ns 2.9616 ns]
                        thrpt:  [337.66 Melem/s 338.46 Melem/s 339.14 Melem/s]
                 change:
                        time:   [+0.9104% +1.7060% +2.3119%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2596% −1.6774% −0.9022%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
node_metadata/age       time:   [27.830 ns 27.946 ns 28.119 ns]
                        thrpt:  [35.563 Melem/s 35.783 Melem/s 35.932 Melem/s]
                 change:
                        time:   [+1.4512% +1.7173% +2.0497%] (p = 0.00 < 0.05)
                        thrpt:  [−2.0085% −1.6884% −1.4304%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
node_metadata/is_stale  time:   [25.943 ns 26.034 ns 26.136 ns]
                        thrpt:  [38.262 Melem/s 38.411 Melem/s 38.546 Melem/s]
                 change:
                        time:   [+1.0786% +1.8234% +3.2116%] (p = 0.00 < 0.05)
                        thrpt:  [−3.1117% −1.7908% −1.0671%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
node_metadata/serialize time:   [778.72 ns 782.81 ns 787.15 ns]
                        thrpt:  [1.2704 Melem/s 1.2775 Melem/s 1.2842 Melem/s]
                 change:
                        time:   [+0.8354% +1.5454% +2.1884%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1415% −1.5219% −0.8285%]
                        Change within noise threshold.
node_metadata/deserialize
                        time:   [1.6816 µs 1.6942 µs 1.7073 µs]
                        thrpt:  [585.72 Kelem/s 590.26 Kelem/s 594.67 Kelem/s]
                 change:
                        time:   [−2.9280% −2.2377% −1.5842%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6097% +2.2890% +3.0163%]
                        Performance has improved.

metadata_query/match_status
                        time:   [3.4131 ns 3.4148 ns 3.4179 ns]
                        thrpt:  [292.58 Melem/s 292.84 Melem/s 292.99 Melem/s]
                 change:
                        time:   [−8.3763% −8.2843% −8.1873%] (p = 0.00 < 0.05)
                        thrpt:  [+8.9174% +9.0325% +9.1420%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
metadata_query/match_min_tier
                        time:   [3.4150 ns 3.4236 ns 3.4377 ns]
                        thrpt:  [290.89 Melem/s 292.09 Melem/s 292.83 Melem/s]
                 change:
                        time:   [−1.5062% −0.9368% −0.3811%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3825% +0.9457% +1.5293%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) high mild
  14 (14.00%) high severe
metadata_query/match_continent
                        time:   [11.214 ns 11.238 ns 11.270 ns]
                        thrpt:  [88.729 Melem/s 88.983 Melem/s 89.178 Melem/s]
                 change:
                        time:   [−6.0317% −4.4140% −3.0960%] (p = 0.00 < 0.05)
                        thrpt:  [+3.1949% +4.6178% +6.4188%]
                        Performance has improved.
metadata_query/match_complex
                        time:   [10.551 ns 10.574 ns 10.621 ns]
                        thrpt:  [94.150 Melem/s 94.569 Melem/s 94.778 Melem/s]
                 change:
                        time:   [−3.0488% −2.8585% −2.6242%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6949% +2.9426% +3.1447%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
metadata_query/match_no_match
                        time:   [3.4228 ns 3.4365 ns 3.4551 ns]
                        thrpt:  [289.43 Melem/s 290.99 Melem/s 292.16 Melem/s]
                 change:
                        time:   [+0.1822% +0.6007% +1.0648%] (p = 0.01 < 0.05)
                        thrpt:  [−1.0536% −0.5972% −0.1819%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high severe

metadata_store_basic/create
                        time:   [745.30 ns 746.96 ns 748.70 ns]
                        thrpt:  [1.3356 Melem/s 1.3388 Melem/s 1.3417 Melem/s]
                 change:
                        time:   [−0.1742% +0.0845% +0.3352%] (p = 0.53 > 0.05)
                        thrpt:  [−0.3341% −0.0844% +0.1745%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
metadata_store_basic/upsert_new
                        time:   [2.0921 µs 2.1117 µs 2.1300 µs]
                        thrpt:  [469.48 Kelem/s 473.54 Kelem/s 478.00 Kelem/s]
                 change:
                        time:   [−2.8325% −0.5840% +1.6191%] (p = 0.61 > 0.05)
                        thrpt:  [−1.5933% +0.5875% +2.9150%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_basic/upsert_existing
                        time:   [1.3546 µs 1.3603 µs 1.3662 µs]
                        thrpt:  [731.94 Kelem/s 735.13 Kelem/s 738.24 Kelem/s]
                 change:
                        time:   [+1.6057% +2.3441% +2.9503%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8657% −2.2904% −1.5803%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_basic/get
                        time:   [24.959 ns 25.608 ns 26.233 ns]
                        thrpt:  [38.120 Melem/s 39.050 Melem/s 40.066 Melem/s]
                 change:
                        time:   [+0.4530% +3.8707% +7.6529%] (p = 0.04 < 0.05)
                        thrpt:  [−7.1089% −3.7264% −0.4510%]
                        Change within noise threshold.
metadata_store_basic/get_miss
                        time:   [24.089 ns 24.753 ns 25.538 ns]
                        thrpt:  [39.157 Melem/s 40.399 Melem/s 41.512 Melem/s]
                 change:
                        time:   [−5.4388% −1.6554% +2.2031%] (p = 0.42 > 0.05)
                        thrpt:  [−2.1556% +1.6833% +5.7516%]
                        No change in performance detected.
metadata_store_basic/len
                        time:   [310.39 ps 311.58 ps 313.28 ps]
                        thrpt:  [3.1920 Gelem/s 3.2095 Gelem/s 3.2217 Gelem/s]
                 change:
                        time:   [−0.8378% −0.1209% +0.3747%] (p = 0.77 > 0.05)
                        thrpt:  [−0.3733% +0.1211% +0.8449%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) high mild
  13 (13.00%) high severe
metadata_store_basic/stats
                        time:   [5.4417 µs 5.4496 µs 5.4575 µs]
                        thrpt:  [183.23 Kelem/s 183.50 Kelem/s 183.77 Kelem/s]
                 change:
                        time:   [+0.7904% +0.9838% +1.1831%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1693% −0.9742% −0.7842%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

metadata_store_query/query_by_status
                        time:   [215.80 µs 222.82 µs 230.18 µs]
                        thrpt:  [4.3444 Kelem/s 4.4880 Kelem/s 4.6340 Kelem/s]
                 change:
                        time:   [−37.642% −34.590% −31.272%] (p = 0.00 < 0.05)
                        thrpt:  [+45.500% +52.882% +60.364%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_query/query_by_continent
                        time:   [145.90 µs 146.20 µs 146.61 µs]
                        thrpt:  [6.8210 Kelem/s 6.8398 Kelem/s 6.8538 Kelem/s]
                 change:
                        time:   [−1.1835% −0.4535% +0.2394%] (p = 0.22 > 0.05)
                        thrpt:  [−0.2388% +0.4556% +1.1976%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
metadata_store_query/query_by_tier
                        time:   [551.21 µs 565.71 µs 579.13 µs]
                        thrpt:  [1.7267 Kelem/s 1.7677 Kelem/s 1.8142 Kelem/s]
                 change:
                        time:   [−17.050% −14.240% −11.334%] (p = 0.00 < 0.05)
                        thrpt:  [+12.783% +16.604% +20.554%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
metadata_store_query/query_accepting_work
                        time:   [589.38 µs 603.98 µs 617.77 µs]
                        thrpt:  [1.6187 Kelem/s 1.6557 Kelem/s 1.6967 Kelem/s]
                 change:
                        time:   [−20.690% −16.632% −12.025%] (p = 0.00 < 0.05)
                        thrpt:  [+13.669% +19.950% +26.088%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
metadata_store_query/query_with_limit
                        time:   [507.78 µs 525.09 µs 543.22 µs]
                        thrpt:  [1.8409 Kelem/s 1.9045 Kelem/s 1.9694 Kelem/s]
                 change:
                        time:   [+25.496% +31.528% +37.671%] (p = 0.00 < 0.05)
                        thrpt:  [−27.363% −23.971% −20.316%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
metadata_store_query/query_complex
                        time:   [275.76 µs 276.28 µs 277.10 µs]
                        thrpt:  [3.6088 Kelem/s 3.6195 Kelem/s 3.6264 Kelem/s]
                 change:
                        time:   [−0.9771% −0.5308% −0.0051%] (p = 0.04 < 0.05)
                        thrpt:  [+0.0051% +0.5337% +0.9867%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

metadata_store_spatial/find_nearby_100km
                        time:   [326.59 µs 326.80 µs 327.04 µs]
                        thrpt:  [3.0577 Kelem/s 3.0600 Kelem/s 3.0620 Kelem/s]
                 change:
                        time:   [−1.5017% −0.6599% +0.1783%] (p = 0.14 > 0.05)
                        thrpt:  [−0.1780% +0.6643% +1.5246%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
metadata_store_spatial/find_nearby_1000km
                        time:   [406.17 µs 408.67 µs 411.78 µs]
                        thrpt:  [2.4285 Kelem/s 2.4469 Kelem/s 2.4620 Kelem/s]
                 change:
                        time:   [+0.3569% +1.3174% +2.4053%] (p = 0.01 < 0.05)
                        thrpt:  [−2.3488% −1.3003% −0.3556%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) high mild
  12 (12.00%) high severe
metadata_store_spatial/find_nearby_5000km
                        time:   [556.29 µs 585.45 µs 616.90 µs]
                        thrpt:  [1.6210 Kelem/s 1.7081 Kelem/s 1.7976 Kelem/s]
                 change:
                        time:   [−11.963% −8.1932% −4.5160%] (p = 0.00 < 0.05)
                        thrpt:  [+4.7296% +8.9243% +13.589%]
                        Performance has improved.
metadata_store_spatial/find_best_for_routing
                        time:   [362.88 µs 386.99 µs 411.65 µs]
                        thrpt:  [2.4293 Kelem/s 2.5841 Kelem/s 2.7557 Kelem/s]
                 change:
                        time:   [+23.567% +32.275% +40.801%] (p = 0.00 < 0.05)
                        thrpt:  [−28.978% −24.400% −19.072%]
                        Performance has regressed.
metadata_store_spatial/find_relays
                        time:   [503.78 µs 516.46 µs 531.41 µs]
                        thrpt:  [1.8818 Kelem/s 1.9363 Kelem/s 1.9850 Kelem/s]
                 change:
                        time:   [−31.582% −28.970% −25.924%] (p = 0.00 < 0.05)
                        thrpt:  [+34.997% +40.785% +46.160%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  12 (12.00%) high mild
  1 (1.00%) high severe

metadata_store_scaling/query_status/1000
                        time:   [18.733 µs 18.765 µs 18.797 µs]
                        thrpt:  [53.201 Kelem/s 53.292 Kelem/s 53.383 Kelem/s]
                 change:
                        time:   [−2.0779% −1.6352% −1.2210%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2361% +1.6624% +2.1220%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/query_complex/1000
                        time:   [21.539 µs 21.585 µs 21.642 µs]
                        thrpt:  [46.206 Kelem/s 46.329 Kelem/s 46.427 Kelem/s]
                 change:
                        time:   [+0.8080% +1.9307% +2.7158%] (p = 0.00 < 0.05)
                        thrpt:  [−2.6440% −1.8941% −0.8016%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
metadata_store_scaling/find_nearby/1000
                        time:   [57.877 µs 57.955 µs 58.040 µs]
                        thrpt:  [17.229 Kelem/s 17.255 Kelem/s 17.278 Kelem/s]
                 change:
                        time:   [+1.1376% +1.3929% +1.7126%] (p = 0.00 < 0.05)
                        thrpt:  [−1.6837% −1.3737% −1.1248%]
                        Performance has regressed.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
metadata_store_scaling/query_status/5000
                        time:   [99.523 µs 100.04 µs 100.73 µs]
                        thrpt:  [9.9275 Kelem/s 9.9958 Kelem/s 10.048 Kelem/s]
                 change:
                        time:   [+4.8510% +6.0623% +7.3129%] (p = 0.00 < 0.05)
                        thrpt:  [−6.8146% −5.7158% −4.6266%]
                        Performance has regressed.
metadata_store_scaling/query_complex/5000
                        time:   [118.08 µs 118.17 µs 118.27 µs]
                        thrpt:  [8.4551 Kelem/s 8.4625 Kelem/s 8.4690 Kelem/s]
                 change:
                        time:   [−1.8239% −1.4046% −0.9838%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9936% +1.4247% +1.8578%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
metadata_store_scaling/find_nearby/5000
                        time:   [389.19 µs 400.03 µs 411.01 µs]
                        thrpt:  [2.4330 Kelem/s 2.4998 Kelem/s 2.5695 Kelem/s]
                 change:
                        time:   [+4.7107% +9.1736% +14.035%] (p = 0.00 < 0.05)
                        thrpt:  [−12.308% −8.4027% −4.4988%]
                        Performance has regressed.
metadata_store_scaling/query_status/10000
                        time:   [385.80 µs 395.01 µs 403.75 µs]
                        thrpt:  [2.4768 Kelem/s 2.5316 Kelem/s 2.5920 Kelem/s]
                 change:
                        time:   [+40.023% +46.912% +53.829%] (p = 0.00 < 0.05)
                        thrpt:  [−34.993% −31.932% −28.583%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_scaling/query_complex/10000
                        time:   [380.83 µs 398.11 µs 414.12 µs]
                        thrpt:  [2.4148 Kelem/s 2.5119 Kelem/s 2.6258 Kelem/s]
                 change:
                        time:   [+3.5137% +10.281% +17.237%] (p = 0.00 < 0.05)
                        thrpt:  [−14.702% −9.3226% −3.3944%]
                        Performance has regressed.
metadata_store_scaling/find_nearby/10000
                        time:   [673.05 µs 695.19 µs 716.18 µs]
                        thrpt:  [1.3963 Kelem/s 1.4384 Kelem/s 1.4858 Kelem/s]
                 change:
                        time:   [−11.660% −8.7929% −5.8685%] (p = 0.00 < 0.05)
                        thrpt:  [+6.2344% +9.6406% +13.200%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
metadata_store_scaling/query_status/50000
                        time:   [2.4162 ms 2.4457 ms 2.4763 ms]
                        thrpt:  [403.83  elem/s 408.88  elem/s 413.87  elem/s]
                 change:
                        time:   [−6.8728% −5.3194% −3.6568%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7956% +5.6182% +7.3800%]
                        Performance has improved.
metadata_store_scaling/query_complex/50000
                        time:   [2.7240 ms 2.7558 ms 2.7879 ms]
                        thrpt:  [358.70  elem/s 362.87  elem/s 367.11  elem/s]
                 change:
                        time:   [−1.8254% −0.3114% +1.3103%] (p = 0.71 > 0.05)
                        thrpt:  [−1.2933% +0.3124% +1.8593%]
                        No change in performance detected.
metadata_store_scaling/find_nearby/50000
                        time:   [3.2681 ms 3.2983 ms 3.3282 ms]
                        thrpt:  [300.46  elem/s 303.18  elem/s 305.99  elem/s]
                 change:
                        time:   [−6.5280% −4.9341% −3.4401%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5627% +5.1902% +6.9840%]
                        Performance has improved.

metadata_store_concurrent/concurrent_upsert/4
                        time:   [1.6418 ms 1.6559 ms 1.6705 ms]
                        thrpt:  [1.1973 Melem/s 1.2078 Melem/s 1.2181 Melem/s]
                 change:
                        time:   [−0.7163% +1.5699% +4.3346%] (p = 0.24 > 0.05)
                        thrpt:  [−4.1545% −1.5457% +0.7215%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.3s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/4
                        time:   [330.88 ms 350.06 ms 369.77 ms]
                        thrpt:  [5.4088 Kelem/s 5.7133 Kelem/s 6.0444 Kelem/s]
                 change:
                        time:   [−15.463% −6.9426% +2.0194%] (p = 0.16 > 0.05)
                        thrpt:  [−1.9794% +7.4605% +18.292%]
                        No change in performance detected.
Benchmarking metadata_store_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.2s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/4
                        time:   [236.16 ms 292.16 ms 358.14 ms]
                        thrpt:  [5.5845 Kelem/s 6.8455 Kelem/s 8.4688 Kelem/s]
                 change:
                        time:   [+31.813% +61.940% +94.902%] (p = 0.00 < 0.05)
                        thrpt:  [−48.692% −38.249% −24.135%]
                        Performance has regressed.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
metadata_store_concurrent/concurrent_upsert/8
                        time:   [4.5130 ms 4.6408 ms 4.8108 ms]
                        thrpt:  [831.46 Kelem/s 861.92 Kelem/s 886.33 Kelem/s]
                 change:
                        time:   [+0.7699% +2.2180% +4.2018%] (p = 0.01 < 0.05)
                        thrpt:  [−4.0324% −2.1699% −0.7640%]
                        Change within noise threshold.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 15.9s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/8
                        time:   [786.74 ms 790.74 ms 795.16 ms]
                        thrpt:  [5.0305 Kelem/s 5.0585 Kelem/s 5.0843 Kelem/s]
                 change:
                        time:   [−2.1137% −1.3698% −0.6519%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6562% +1.3888% +2.1593%]
                        Change within noise threshold.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 17.6s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/8
                        time:   [871.93 ms 878.45 ms 885.29 ms]
                        thrpt:  [4.5183 Kelem/s 4.5535 Kelem/s 4.5875 Kelem/s]
                 change:
                        time:   [−2.3964% −1.3649% −0.3366%] (p = 0.02 < 0.05)
                        thrpt:  [+0.3378% +1.3838% +2.4553%]
                        Change within noise threshold.
metadata_store_concurrent/concurrent_upsert/16
                        time:   [10.025 ms 10.043 ms 10.064 ms]
                        thrpt:  [794.91 Kelem/s 796.61 Kelem/s 798.01 Kelem/s]
                 change:
                        time:   [+3.0835% +3.6037% +4.1655%] (p = 0.00 < 0.05)
                        thrpt:  [−3.9989% −3.4784% −2.9913%]
                        Performance has regressed.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 30.8s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/16
                        time:   [1.5391 s 1.5510 s 1.5627 s]
                        thrpt:  [5.1194 Kelem/s 5.1578 Kelem/s 5.1978 Kelem/s]
                 change:
                        time:   [+1.7251% +2.7246% +3.6974%] (p = 0.00 < 0.05)
                        thrpt:  [−3.5656% −2.6524% −1.6959%]
                        Performance has regressed.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) low mild
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 34.1s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/16
                        time:   [1.6983 s 1.7235 s 1.7629 s]
                        thrpt:  [4.5380 Kelem/s 4.6418 Kelem/s 4.7107 Kelem/s]
                 change:
                        time:   [−4.0319% −2.4769% −0.0819%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0819% +2.5398% +4.2013%]
                        Change within noise threshold.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high severe

metadata_store_versioning/update_versioned_success
                        time:   [274.23 ns 276.11 ns 277.77 ns]
                        thrpt:  [3.6001 Melem/s 3.6217 Melem/s 3.6466 Melem/s]
                 change:
                        time:   [−4.2827% −3.6849% −3.2069%] (p = 0.00 < 0.05)
                        thrpt:  [+3.3132% +3.8259% +4.4743%]
                        Performance has improved.
metadata_store_versioning/update_versioned_conflict
                        time:   [267.72 ns 270.37 ns 273.05 ns]
                        thrpt:  [3.6624 Melem/s 3.6987 Melem/s 3.7352 Melem/s]
                 change:
                        time:   [−6.4860% −5.7469% −5.0412%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3088% +6.0973% +6.9359%]
                        Performance has improved.

schema_validation/validate_string
                        time:   [3.4502 ns 3.4573 ns 3.4646 ns]
                        thrpt:  [288.63 Melem/s 289.24 Melem/s 289.84 Melem/s]
                 change:
                        time:   [−1.1952% −0.9019% −0.6377%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6418% +0.9101% +1.2097%]
                        Change within noise threshold.
schema_validation/validate_integer
                        time:   [3.4560 ns 3.4616 ns 3.4669 ns]
                        thrpt:  [288.44 Melem/s 288.88 Melem/s 289.35 Melem/s]
                 change:
                        time:   [−0.7831% −0.6028% −0.4341%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4360% +0.6065% +0.7893%]
                        Change within noise threshold.
schema_validation/validate_object
                        time:   [76.464 ns 76.737 ns 77.058 ns]
                        thrpt:  [12.977 Melem/s 13.032 Melem/s 13.078 Melem/s]
                 change:
                        time:   [+1.5517% +2.4541% +3.0945%] (p = 0.00 < 0.05)
                        thrpt:  [−3.0016% −2.3953% −1.5280%]
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
schema_validation/validate_array_10
                        time:   [36.159 ns 36.236 ns 36.316 ns]
                        thrpt:  [27.536 Melem/s 27.597 Melem/s 27.655 Melem/s]
                 change:
                        time:   [−0.2846% −0.0505% +0.1879%] (p = 0.67 > 0.05)
                        thrpt:  [−0.1876% +0.0506% +0.2854%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
schema_validation/validate_complex
                        time:   [201.76 ns 202.04 ns 202.34 ns]
                        thrpt:  [4.9421 Melem/s 4.9496 Melem/s 4.9564 Melem/s]
                 change:
                        time:   [−0.5571% +0.2996% +0.9755%] (p = 0.49 > 0.05)
                        thrpt:  [−0.9661% −0.2987% +0.5602%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

endpoint_matching/match_success
                        time:   [283.49 ns 285.09 ns 286.94 ns]
                        thrpt:  [3.4850 Melem/s 3.5077 Melem/s 3.5275 Melem/s]
                 change:
                        time:   [+0.1769% +0.8295% +1.4455%] (p = 0.01 < 0.05)
                        thrpt:  [−1.4249% −0.8226% −0.1766%]
                        Change within noise threshold.
endpoint_matching/match_failure
                        time:   [284.18 ns 285.04 ns 285.93 ns]
                        thrpt:  [3.4973 Melem/s 3.5083 Melem/s 3.5189 Melem/s]
                 change:
                        time:   [−3.2950% −1.7885% −0.4791%] (p = 0.01 < 0.05)
                        thrpt:  [+0.4814% +1.8211% +3.4073%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
endpoint_matching/match_multi_param
                        time:   [644.45 ns 647.83 ns 651.34 ns]
                        thrpt:  [1.5353 Melem/s 1.5436 Melem/s 1.5517 Melem/s]
                 change:
                        time:   [−6.1667% −5.6441% −5.1177%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3937% +5.9817% +6.5719%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high mild

api_version/is_compatible_with
                        time:   [311.01 ps 311.42 ps 311.95 ps]
                        thrpt:  [3.2057 Gelem/s 3.2111 Gelem/s 3.2153 Gelem/s]
                 change:
                        time:   [+1.0680% +1.4318% +1.8533%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8196% −1.4116% −1.0568%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_version/parse       time:   [38.766 ns 38.831 ns 38.910 ns]
                        thrpt:  [25.700 Melem/s 25.752 Melem/s 25.796 Melem/s]
                 change:
                        time:   [+0.1138% +0.2906% +0.4710%] (p = 0.00 < 0.05)
                        thrpt:  [−0.4688% −0.2897% −0.1136%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
api_version/to_string   time:   [49.568 ns 49.602 ns 49.635 ns]
                        thrpt:  [20.147 Melem/s 20.161 Melem/s 20.174 Melem/s]
                 change:
                        time:   [−0.1380% +0.0264% +0.1963%] (p = 0.76 > 0.05)
                        thrpt:  [−0.1959% −0.0264% +0.1382%]
                        No change in performance detected.
Found 22 outliers among 100 measurements (22.00%)
  6 (6.00%) low severe
  3 (3.00%) low mild
  5 (5.00%) high mild
  8 (8.00%) high severe

api_schema/create       time:   [2.1816 µs 2.1877 µs 2.1947 µs]
                        thrpt:  [455.63 Kelem/s 457.10 Kelem/s 458.37 Kelem/s]
                 change:
                        time:   [−0.6961% −0.3898% −0.0737%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0738% +0.3913% +0.7010%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_schema/serialize    time:   [2.0515 µs 2.0566 µs 2.0615 µs]
                        thrpt:  [485.07 Kelem/s 486.25 Kelem/s 487.45 Kelem/s]
                 change:
                        time:   [−0.7690% −0.4909% −0.2055%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2059% +0.4933% +0.7750%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_schema/deserialize  time:   [6.7323 µs 6.7541 µs 6.7779 µs]
                        thrpt:  [147.54 Kelem/s 148.06 Kelem/s 148.54 Kelem/s]
                 change:
                        time:   [−1.9429% −1.4988% −1.0507%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0619% +1.5216% +1.9814%]
                        Performance has improved.
api_schema/find_endpoint
                        time:   [135.07 ns 136.04 ns 136.94 ns]
                        thrpt:  [7.3026 Melem/s 7.3509 Melem/s 7.4038 Melem/s]
                 change:
                        time:   [+0.5671% +0.9739% +1.3902%] (p = 0.00 < 0.05)
                        thrpt:  [−1.3711% −0.9645% −0.5639%]
                        Change within noise threshold.
Found 21 outliers among 100 measurements (21.00%)
  20 (20.00%) high mild
  1 (1.00%) high severe
api_schema/endpoints_by_tag
                        time:   [113.67 ns 115.04 ns 116.55 ns]
                        thrpt:  [8.5801 Melem/s 8.6929 Melem/s 8.7972 Melem/s]
                 change:
                        time:   [−0.4767% +0.8594% +2.1830%] (p = 0.20 > 0.05)
                        thrpt:  [−2.1364% −0.8520% +0.4790%]
                        No change in performance detected.

request_validation/validate_full_request
                        time:   [70.396 ns 70.504 ns 70.618 ns]
                        thrpt:  [14.161 Melem/s 14.183 Melem/s 14.205 Melem/s]
                 change:
                        time:   [−0.6336% −0.1504% +0.3165%] (p = 0.56 > 0.05)
                        thrpt:  [−0.3155% +0.1507% +0.6377%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
request_validation/validate_path_only
                        time:   [21.559 ns 21.671 ns 21.764 ns]
                        thrpt:  [45.947 Melem/s 46.146 Melem/s 46.383 Melem/s]
                 change:
                        time:   [+1.0112% +1.6198% +2.2853%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2342% −1.5940% −1.0011%]
                        Performance has regressed.
Found 26 outliers among 100 measurements (26.00%)
  21 (21.00%) low severe
  1 (1.00%) high mild
  4 (4.00%) high severe

api_registry_basic/create
                        time:   [412.37 ns 413.20 ns 414.11 ns]
                        thrpt:  [2.4148 Melem/s 2.4201 Melem/s 2.4250 Melem/s]
                 change:
                        time:   [−0.9771% −0.5362% −0.1057%] (p = 0.01 < 0.05)
                        thrpt:  [+0.1058% +0.5391% +0.9867%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
api_registry_basic/register_new
                        time:   [4.4362 µs 4.5668 µs 4.7018 µs]
                        thrpt:  [212.68 Kelem/s 218.97 Kelem/s 225.42 Kelem/s]
                 change:
                        time:   [−22.280% −19.561% −16.962%] (p = 0.00 < 0.05)
                        thrpt:  [+20.426% +24.318% +28.668%]
                        Performance has improved.
api_registry_basic/get  time:   [23.866 ns 24.557 ns 25.331 ns]
                        thrpt:  [39.477 Melem/s 40.722 Melem/s 41.900 Melem/s]
                 change:
                        time:   [−11.464% −8.0409% −4.5172%] (p = 0.00 < 0.05)
                        thrpt:  [+4.7309% +8.7440% +12.948%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  13 (13.00%) high mild
api_registry_basic/len  time:   [310.31 ps 310.44 ps 310.64 ps]
                        thrpt:  [3.2192 Gelem/s 3.2212 Gelem/s 3.2226 Gelem/s]
                 change:
                        time:   [−0.0590% +0.0848% +0.2443%] (p = 0.31 > 0.05)
                        thrpt:  [−0.2437% −0.0848% +0.0590%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  6 (6.00%) high mild
  10 (10.00%) high severe
api_registry_basic/stats
                        time:   [2.8246 µs 2.8278 µs 2.8310 µs]
                        thrpt:  [353.23 Kelem/s 353.63 Kelem/s 354.03 Kelem/s]
                 change:
                        time:   [−0.8101% −0.6139% −0.4194%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4212% +0.6177% +0.8167%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

api_registry_query/query_by_name
                        time:   [91.961 µs 93.047 µs 94.123 µs]
                        thrpt:  [10.624 Kelem/s 10.747 Kelem/s 10.874 Kelem/s]
                 change:
                        time:   [−3.8017% −2.8006% −1.7729%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8049% +2.8813% +3.9519%]
                        Performance has improved.
api_registry_query/query_by_tag
                        time:   [840.60 µs 844.94 µs 850.18 µs]
                        thrpt:  [1.1762 Kelem/s 1.1835 Kelem/s 1.1896 Kelem/s]
                 change:
                        time:   [+34.248% +37.391% +40.754%] (p = 0.00 < 0.05)
                        thrpt:  [−28.954% −27.215% −25.511%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  6 (6.00%) low severe
  2 (2.00%) high mild
  8 (8.00%) high severe
api_registry_query/query_with_version
                        time:   [56.080 µs 56.114 µs 56.149 µs]
                        thrpt:  [17.810 Kelem/s 17.821 Kelem/s 17.832 Kelem/s]
                 change:
                        time:   [+1.5799% +1.7331% +1.9042%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8686% −1.7036% −1.5554%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
api_registry_query/find_by_endpoint
                        time:   [2.4052 ms 2.4731 ms 2.5399 ms]
                        thrpt:  [393.72  elem/s 404.35  elem/s 415.77  elem/s]
                 change:
                        time:   [+27.214% +31.883% +36.510%] (p = 0.00 < 0.05)
                        thrpt:  [−26.745% −24.175% −21.392%]
                        Performance has regressed.
api_registry_query/find_compatible
                        time:   [63.699 µs 63.746 µs 63.796 µs]
                        thrpt:  [15.675 Kelem/s 15.687 Kelem/s 15.699 Kelem/s]
                 change:
                        time:   [−0.0892% +0.0454% +0.1953%] (p = 0.54 > 0.05)
                        thrpt:  [−0.1949% −0.0454% +0.0892%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

api_registry_scaling/query_by_name/1000
                        time:   [7.4713 µs 7.4849 µs 7.4979 µs]
                        thrpt:  [133.37 Kelem/s 133.60 Kelem/s 133.85 Kelem/s]
                 change:
                        time:   [+1.0519% +1.4940% +1.9511%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9138% −1.4721% −1.0409%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_tag/1000
                        time:   [48.321 µs 48.370 µs 48.422 µs]
                        thrpt:  [20.652 Kelem/s 20.674 Kelem/s 20.695 Kelem/s]
                 change:
                        time:   [−0.0845% +0.2076% +0.5197%] (p = 0.20 > 0.05)
                        thrpt:  [−0.5170% −0.2071% +0.0845%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
api_registry_scaling/query_by_name/5000
                        time:   [43.583 µs 43.943 µs 44.346 µs]
                        thrpt:  [22.550 Kelem/s 22.757 Kelem/s 22.945 Kelem/s]
                 change:
                        time:   [−1.7777% −1.1897% −0.5712%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5745% +1.2041% +1.8099%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high mild
api_registry_scaling/query_by_tag/5000
                        time:   [463.17 µs 473.04 µs 485.26 µs]
                        thrpt:  [2.0608 Kelem/s 2.1140 Kelem/s 2.1590 Kelem/s]
                 change:
                        time:   [+47.439% +53.104% +58.753%] (p = 0.00 < 0.05)
                        thrpt:  [−37.009% −34.685% −32.175%]
                        Performance has regressed.
Found 18 outliers among 100 measurements (18.00%)
  8 (8.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
api_registry_scaling/query_by_name/10000
                        time:   [94.903 µs 95.968 µs 97.038 µs]
                        thrpt:  [10.305 Kelem/s 10.420 Kelem/s 10.537 Kelem/s]
                 change:
                        time:   [−2.5780% −1.2220% +0.1632%] (p = 0.07 > 0.05)
                        thrpt:  [−0.1629% +1.2371% +2.6462%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
api_registry_scaling/query_by_tag/10000
                        time:   [823.04 µs 836.98 µs 850.84 µs]
                        thrpt:  [1.1753 Kelem/s 1.1948 Kelem/s 1.2150 Kelem/s]
                 change:
                        time:   [+40.203% +43.779% +48.067%] (p = 0.00 < 0.05)
                        thrpt:  [−32.463% −30.449% −28.675%]
                        Performance has regressed.
Found 21 outliers among 100 measurements (21.00%)
  6 (6.00%) low severe
  4 (4.00%) high mild
  11 (11.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 15.9s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/4
                        time:   [728.91 ms 750.74 ms 777.49 ms]
                        thrpt:  [2.5724 Kelem/s 2.6640 Kelem/s 2.7438 Kelem/s]
                 change:
                        time:   [−13.466% −9.4366% −4.7674%] (p = 0.00 < 0.05)
                        thrpt:  [+5.0061% +10.420% +15.562%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 11.8s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/4
                        time:   [562.83 ms 588.45 ms 615.62 ms]
                        thrpt:  [3.2488 Kelem/s 3.3988 Kelem/s 3.5535 Kelem/s]
                 change:
                        time:   [+21.885% +31.635% +41.291%] (p = 0.00 < 0.05)
                        thrpt:  [−29.224% −24.033% −17.955%]
                        Performance has regressed.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 22.6s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/8
                        time:   [1.1135 s 1.1256 s 1.1373 s]
                        thrpt:  [3.5171 Kelem/s 3.5537 Kelem/s 3.5924 Kelem/s]
                 change:
                        time:   [−5.6820% −4.0455% −2.2281%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2788% +4.2161% +6.0243%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 18.9s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/8
                        time:   [942.50 ms 947.48 ms 952.73 ms]
                        thrpt:  [4.1985 Kelem/s 4.2217 Kelem/s 4.2440 Kelem/s]
                 change:
                        time:   [+3.5859% +4.8392% +5.9485%] (p = 0.00 < 0.05)
                        thrpt:  [−5.6145% −4.6158% −3.4617%]
                        Performance has regressed.
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 38.3s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/16
                        time:   [1.9174 s 1.9439 s 1.9782 s]
                        thrpt:  [4.0441 Kelem/s 4.1154 Kelem/s 4.1723 Kelem/s]
                 change:
                        time:   [+1.7444% +3.2547% +5.1844%] (p = 0.00 < 0.05)
                        thrpt:  [−4.9288% −3.1521% −1.7145%]
                        Performance has regressed.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 35.8s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/16
                        time:   [1.8290 s 1.8548 s 1.8855 s]
                        thrpt:  [4.2429 Kelem/s 4.3132 Kelem/s 4.3739 Kelem/s]
                 change:
                        time:   [+6.1117% +7.8813% +9.8873%] (p = 0.00 < 0.05)
                        thrpt:  [−8.9976% −7.3055% −5.7597%]
                        Performance has regressed.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe

compare_op/eq           time:   [1.9672 ns 1.9699 ns 1.9724 ns]
                        thrpt:  [507.00 Melem/s 507.65 Melem/s 508.33 Melem/s]
                 change:
                        time:   [−0.7296% −0.5786% −0.4344%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4363% +0.5820% +0.7349%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
compare_op/gt           time:   [2.0194 ns 2.0230 ns 2.0274 ns]
                        thrpt:  [493.24 Melem/s 494.32 Melem/s 495.19 Melem/s]
                 change:
                        time:   [−0.3286% −0.0870% +0.1543%] (p = 0.49 > 0.05)
                        thrpt:  [−0.1540% +0.0870% +0.3297%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) high mild
  10 (10.00%) high severe
compare_op/contains_string
                        time:   [24.828 ns 24.845 ns 24.866 ns]
                        thrpt:  [40.215 Melem/s 40.250 Melem/s 40.277 Melem/s]
                 change:
                        time:   [−0.0906% +0.0721% +0.2394%] (p = 0.43 > 0.05)
                        thrpt:  [−0.2388% −0.0720% +0.0907%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe
compare_op/in_array     time:   [6.8276 ns 6.8304 ns 6.8340 ns]
                        thrpt:  [146.33 Melem/s 146.40 Melem/s 146.46 Melem/s]
                 change:
                        time:   [−0.0128% +0.0935% +0.2223%] (p = 0.13 > 0.05)
                        thrpt:  [−0.2218% −0.0934% +0.0128%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe

condition/simple        time:   [55.640 ns 55.707 ns 55.771 ns]
                        thrpt:  [17.931 Melem/s 17.951 Melem/s 17.973 Melem/s]
                 change:
                        time:   [−0.2106% +0.0687% +0.3397%] (p = 0.64 > 0.05)
                        thrpt:  [−0.3386% −0.0686% +0.2110%]
                        No change in performance detected.
Found 20 outliers among 100 measurements (20.00%)
  6 (6.00%) low severe
  2 (2.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
condition/nested_field  time:   [896.00 ns 901.48 ns 907.54 ns]
                        thrpt:  [1.1019 Melem/s 1.1093 Melem/s 1.1161 Melem/s]
                 change:
                        time:   [−0.8334% −0.1566% +0.5659%] (p = 0.67 > 0.05)
                        thrpt:  [−0.5627% +0.1569% +0.8404%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  11 (11.00%) high mild
condition/string_eq     time:   [93.726 ns 94.776 ns 95.877 ns]
                        thrpt:  [10.430 Melem/s 10.551 Melem/s 10.669 Melem/s]
                 change:
                        time:   [−2.9480% −1.8703% −0.7792%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7854% +1.9059% +3.0375%]
                        Change within noise threshold.

condition_expr/single   time:   [56.633 ns 56.799 ns 56.970 ns]
                        thrpt:  [17.553 Melem/s 17.606 Melem/s 17.658 Melem/s]
                 change:
                        time:   [+0.2357% +0.4298% +0.6640%] (p = 0.00 < 0.05)
                        thrpt:  [−0.6596% −0.4280% −0.2351%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe
condition_expr/and_2    time:   [114.15 ns 114.42 ns 114.82 ns]
                        thrpt:  [8.7092 Melem/s 8.7396 Melem/s 8.7606 Melem/s]
                 change:
                        time:   [+0.7840% +1.2893% +1.7932%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7616% −1.2729% −0.7779%]
                        Change within noise threshold.
Found 30 outliers among 100 measurements (30.00%)
  9 (9.00%) low mild
  2 (2.00%) high mild
  19 (19.00%) high severe
condition_expr/and_5    time:   [398.35 ns 401.70 ns 404.97 ns]
                        thrpt:  [2.4693 Melem/s 2.4894 Melem/s 2.5103 Melem/s]
                 change:
                        time:   [+1.0167% +1.6795% +2.4454%] (p = 0.00 < 0.05)
                        thrpt:  [−2.3870% −1.6518% −1.0065%]
                        Performance has regressed.
condition_expr/or_3     time:   [224.70 ns 225.66 ns 226.74 ns]
                        thrpt:  [4.4103 Melem/s 4.4314 Melem/s 4.4504 Melem/s]
                 change:
                        time:   [−2.0692% −1.5561% −1.0344%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0453% +1.5807% +2.1130%]
                        Performance has improved.
condition_expr/nested   time:   [167.74 ns 168.55 ns 169.72 ns]
                        thrpt:  [5.8922 Melem/s 5.9329 Melem/s 5.9618 Melem/s]
                 change:
                        time:   [−0.9370% −0.1653% +0.6815%] (p = 0.70 > 0.05)
                        thrpt:  [−0.6769% +0.1655% +0.9458%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) high mild
  6 (6.00%) high severe

rule/create             time:   [554.16 ns 561.56 ns 568.90 ns]
                        thrpt:  [1.7578 Melem/s 1.7808 Melem/s 1.8045 Melem/s]
                 change:
                        time:   [−3.6911% −2.7505% −1.8814%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9174% +2.8283% +3.8325%]
                        Performance has improved.
rule/matches            time:   [113.16 ns 113.53 ns 113.92 ns]
                        thrpt:  [8.7778 Melem/s 8.8080 Melem/s 8.8367 Melem/s]
                 change:
                        time:   [−0.2365% +0.2239% +0.6786%] (p = 0.34 > 0.05)
                        thrpt:  [−0.6741% −0.2234% +0.2371%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe

rule_context/create     time:   [1.4105 µs 1.4200 µs 1.4297 µs]
                        thrpt:  [699.43 Kelem/s 704.21 Kelem/s 708.95 Kelem/s]
                 change:
                        time:   [−3.0903% −2.4654% −1.8620%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8973% +2.5277% +3.1889%]
                        Performance has improved.
rule_context/get_simple time:   [54.477 ns 54.547 ns 54.614 ns]
                        thrpt:  [18.310 Melem/s 18.333 Melem/s 18.356 Melem/s]
                 change:
                        time:   [−1.3166% −0.7933% −0.3221%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3231% +0.7996% +1.3342%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
rule_context/get_nested time:   [891.06 ns 895.71 ns 900.45 ns]
                        thrpt:  [1.1106 Melem/s 1.1164 Melem/s 1.1223 Melem/s]
                 change:
                        time:   [+0.4793% +0.8445% +1.2040%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1897% −0.8375% −0.4770%]
                        Change within noise threshold.
rule_context/get_deep_nested
                        time:   [905.61 ns 914.25 ns 923.55 ns]
                        thrpt:  [1.0828 Melem/s 1.0938 Melem/s 1.1042 Melem/s]
                 change:
                        time:   [+1.6710% +2.2856% +2.9709%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8852% −2.2345% −1.6436%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

rule_engine_basic/create
                        time:   [8.1314 ns 8.1371 ns 8.1437 ns]
                        thrpt:  [122.79 Melem/s 122.89 Melem/s 122.98 Melem/s]
                 change:
                        time:   [−0.2568% −0.0673% +0.1200%] (p = 0.49 > 0.05)
                        thrpt:  [−0.1199% +0.0673% +0.2574%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
rule_engine_basic/add_rule
                        time:   [3.0092 µs 3.1696 µs 3.3063 µs]
                        thrpt:  [302.46 Kelem/s 315.49 Kelem/s 332.31 Kelem/s]
                 change:
                        time:   [−8.5167% +2.6798% +14.446%] (p = 0.64 > 0.05)
                        thrpt:  [−12.622% −2.6098% +9.3096%]
                        No change in performance detected.
rule_engine_basic/get_rule
                        time:   [22.024 ns 22.935 ns 23.718 ns]
                        thrpt:  [42.162 Melem/s 43.601 Melem/s 45.404 Melem/s]
                 change:
                        time:   [−6.9173% −1.7626% +3.7916%] (p = 0.53 > 0.05)
                        thrpt:  [−3.6531% +1.7942% +7.4313%]
                        No change in performance detected.
rule_engine_basic/rules_by_tag
                        time:   [1.1498 µs 1.1563 µs 1.1627 µs]
                        thrpt:  [860.07 Kelem/s 864.85 Kelem/s 869.68 Kelem/s]
                 change:
                        time:   [−2.2524% −1.6285% −0.9228%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9314% +1.6554% +2.3043%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
rule_engine_basic/stats time:   [7.9686 µs 7.9753 µs 7.9824 µs]
                        thrpt:  [125.28 Kelem/s 125.39 Kelem/s 125.49 Kelem/s]
                 change:
                        time:   [−0.3775% −0.1102% +0.1043%] (p = 0.41 > 0.05)
                        thrpt:  [−0.1042% +0.1103% +0.3789%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

rule_engine_evaluate/evaluate_10_rules
                        time:   [3.5147 µs 3.5496 µs 3.5853 µs]
                        thrpt:  [278.91 Kelem/s 281.72 Kelem/s 284.52 Kelem/s]
                 change:
                        time:   [−2.6638% −1.7771% −0.9423%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9513% +1.8092% +2.7367%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  10 (10.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_first_10_rules
                        time:   [426.08 ns 435.94 ns 446.05 ns]
                        thrpt:  [2.2419 Melem/s 2.2939 Melem/s 2.3470 Melem/s]
                 change:
                        time:   [−5.1641% −2.9484% −0.5041%] (p = 0.01 < 0.05)
                        thrpt:  [+0.5066% +3.0380% +5.4453%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
rule_engine_evaluate/evaluate_100_rules
                        time:   [35.836 µs 36.161 µs 36.495 µs]
                        thrpt:  [27.401 Kelem/s 27.654 Kelem/s 27.905 Kelem/s]
                 change:
                        time:   [−1.6148% −0.7727% +0.0943%] (p = 0.08 > 0.05)
                        thrpt:  [−0.0942% +0.7787% +1.6413%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
rule_engine_evaluate/evaluate_first_100_rules
                        time:   [428.81 ns 439.39 ns 450.35 ns]
                        thrpt:  [2.2205 Melem/s 2.2759 Melem/s 2.3321 Melem/s]
                 change:
                        time:   [−1.4925% +0.9702% +3.5040%] (p = 0.45 > 0.05)
                        thrpt:  [−3.3854% −0.9609% +1.5152%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  13 (13.00%) high mild
rule_engine_evaluate/evaluate_matching_100_rules
                        time:   [35.687 µs 35.801 µs 35.943 µs]
                        thrpt:  [27.822 Kelem/s 27.932 Kelem/s 28.021 Kelem/s]
                 change:
                        time:   [+0.5273% +1.0079% +1.5274%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5044% −0.9978% −0.5245%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_engine_evaluate/evaluate_1000_rules
                        time:   [532.46 µs 544.83 µs 554.98 µs]
                        thrpt:  [1.8019 Kelem/s 1.8354 Kelem/s 1.8781 Kelem/s]
                 change:
                        time:   [+45.160% +49.951% +55.637%] (p = 0.00 < 0.05)
                        thrpt:  [−35.748% −33.311% −31.110%]
                        Performance has regressed.
Found 21 outliers among 100 measurements (21.00%)
  14 (14.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
rule_engine_evaluate/evaluate_first_1000_rules
                        time:   [431.08 ns 440.24 ns 449.88 ns]
                        thrpt:  [2.2228 Melem/s 2.2715 Melem/s 2.3197 Melem/s]
                 change:
                        time:   [−2.0386% +0.3713% +2.9085%] (p = 0.76 > 0.05)
                        thrpt:  [−2.8263% −0.3699% +2.0811%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  14 (14.00%) high mild

rule_engine_scaling/evaluate/10
                        time:   [3.4534 µs 3.4804 µs 3.5151 µs]
                        thrpt:  [284.48 Kelem/s 287.32 Kelem/s 289.57 Kelem/s]
                 change:
                        time:   [−2.0690% −1.4800% −0.7695%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7755% +1.5023% +2.1127%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
rule_engine_scaling/evaluate_first/10
                        time:   [400.93 ns 406.19 ns 411.26 ns]
                        thrpt:  [2.4316 Melem/s 2.4619 Melem/s 2.4942 Melem/s]
                 change:
                        time:   [+2.0984% +3.0054% +3.8281%] (p = 0.00 < 0.05)
                        thrpt:  [−3.6869% −2.9177% −2.0552%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) low mild
rule_engine_scaling/evaluate/50
                        time:   [17.866 µs 17.957 µs 18.055 µs]
                        thrpt:  [55.387 Kelem/s 55.688 Kelem/s 55.972 Kelem/s]
                 change:
                        time:   [+1.3116% +1.8085% +2.3047%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2528% −1.7763% −1.2946%]
                        Performance has regressed.
rule_engine_scaling/evaluate_first/50
                        time:   [406.10 ns 408.57 ns 411.06 ns]
                        thrpt:  [2.4327 Melem/s 2.4476 Melem/s 2.4625 Melem/s]
                 change:
                        time:   [+3.1187% +3.6253% +4.1653%] (p = 0.00 < 0.05)
                        thrpt:  [−3.9987% −3.4985% −3.0243%]
                        Performance has regressed.
rule_engine_scaling/evaluate/100
                        time:   [35.323 µs 35.413 µs 35.508 µs]
                        thrpt:  [28.163 Kelem/s 28.238 Kelem/s 28.310 Kelem/s]
                 change:
                        time:   [−2.0262% −1.5413% −1.0571%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0684% +1.5655% +2.0681%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_engine_scaling/evaluate_first/100
                        time:   [404.09 ns 408.42 ns 412.51 ns]
                        thrpt:  [2.4242 Melem/s 2.4484 Melem/s 2.4747 Melem/s]
                 change:
                        time:   [+0.6029% +1.5130% +2.3084%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2563% −1.4904% −0.5993%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) low mild
rule_engine_scaling/evaluate/500
                        time:   [341.21 µs 354.31 µs 365.33 µs]
                        thrpt:  [2.7373 Kelem/s 2.8224 Kelem/s 2.9308 Kelem/s]
                 change:
                        time:   [+73.264% +81.419% +88.616%] (p = 0.00 < 0.05)
                        thrpt:  [−46.982% −44.879% −42.285%]
                        Performance has regressed.
Found 21 outliers among 100 measurements (21.00%)
  15 (15.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
rule_engine_scaling/evaluate_first/500
                        time:   [408.91 ns 412.60 ns 416.27 ns]
                        thrpt:  [2.4023 Melem/s 2.4236 Melem/s 2.4455 Melem/s]
                 change:
                        time:   [−0.4406% +0.4293% +1.3879%] (p = 0.35 > 0.05)
                        thrpt:  [−1.3689% −0.4275% +0.4426%]
                        No change in performance detected.
rule_engine_scaling/evaluate/1000
                        time:   [502.72 µs 526.76 µs 551.39 µs]
                        thrpt:  [1.8136 Kelem/s 1.8984 Kelem/s 1.9892 Kelem/s]
                 change:
                        time:   [+34.345% +38.566% +43.036%] (p = 0.00 < 0.05)
                        thrpt:  [−30.087% −27.832% −25.565%]
                        Performance has regressed.
Found 20 outliers among 100 measurements (20.00%)
  12 (12.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high severe
rule_engine_scaling/evaluate_first/1000
                        time:   [404.67 ns 406.80 ns 408.63 ns]
                        thrpt:  [2.4472 Melem/s 2.4582 Melem/s 2.4711 Melem/s]
                 change:
                        time:   [+2.0484% +2.5444% +3.0595%] (p = 0.00 < 0.05)
                        thrpt:  [−2.9687% −2.4813% −2.0073%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild

rule_set/create         time:   [6.0822 µs 6.0916 µs 6.1007 µs]
                        thrpt:  [163.92 Kelem/s 164.16 Kelem/s 164.41 Kelem/s]
                 change:
                        time:   [+0.0901% +0.3263% +0.5655%] (p = 0.01 < 0.05)
                        thrpt:  [−0.5623% −0.3253% −0.0900%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
rule_set/load_into_engine
                        time:   [10.443 µs 10.455 µs 10.466 µs]
                        thrpt:  [95.549 Kelem/s 95.647 Kelem/s 95.755 Kelem/s]
                 change:
                        time:   [−0.7270% −0.3744% −0.0478%] (p = 0.03 < 0.05)
                        thrpt:  [+0.0479% +0.3758% +0.7324%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe

trace_id/generate       time:   [556.19 ns 560.04 ns 563.52 ns]
                        thrpt:  [1.7746 Melem/s 1.7856 Melem/s 1.7980 Melem/s]
                 change:
                        time:   [+1.7604% +2.3788% +2.9865%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8999% −2.3236% −1.7300%]
                        Performance has regressed.
trace_id/to_hex         time:   [107.26 ns 107.47 ns 107.69 ns]
                        thrpt:  [9.2857 Melem/s 9.3048 Melem/s 9.3232 Melem/s]
                 change:
                        time:   [−18.884% −18.582% −18.270%] (p = 0.00 < 0.05)
                        thrpt:  [+22.354% +22.823% +23.280%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
trace_id/from_hex       time:   [23.162 ns 23.171 ns 23.187 ns]
                        thrpt:  [43.127 Melem/s 43.156 Melem/s 43.174 Melem/s]
                 change:
                        time:   [−0.0342% +0.0794% +0.2163%] (p = 0.24 > 0.05)
                        thrpt:  [−0.2158% −0.0793% +0.0342%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe

context_operations/create
                        time:   [860.80 ns 868.07 ns 875.45 ns]
                        thrpt:  [1.1423 Melem/s 1.1520 Melem/s 1.1617 Melem/s]
                 change:
                        time:   [+3.5603% +4.3597% +5.2262%] (p = 0.00 < 0.05)
                        thrpt:  [−4.9667% −4.1776% −3.4379%]
                        Performance has regressed.
context_operations/child
                        time:   [282.29 ns 283.36 ns 284.66 ns]
                        thrpt:  [3.5129 Melem/s 3.5290 Melem/s 3.5424 Melem/s]
                 change:
                        time:   [−0.7725% −0.1780% +0.4027%] (p = 0.57 > 0.05)
                        thrpt:  [−0.4011% +0.1784% +0.7785%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
context_operations/for_remote
                        time:   [281.52 ns 281.90 ns 282.31 ns]
                        thrpt:  [3.5422 Melem/s 3.5474 Melem/s 3.5521 Melem/s]
                 change:
                        time:   [−3.1284% −2.3834% −1.6652%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6934% +2.4416% +3.2294%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
context_operations/to_traceparent
                        time:   [324.28 ns 326.68 ns 329.35 ns]
                        thrpt:  [3.0363 Melem/s 3.0611 Melem/s 3.0838 Melem/s]
                 change:
                        time:   [−2.4573% −1.7501% −0.9883%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9982% +1.7813% +2.5192%]
                        Change within noise threshold.
context_operations/from_traceparent
                        time:   [383.52 ns 385.40 ns 387.52 ns]
                        thrpt:  [2.5805 Melem/s 2.5947 Melem/s 2.6074 Melem/s]
                 change:
                        time:   [+2.1495% +3.1874% +4.3191%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1403% −3.0889% −2.1043%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

baggage/create          time:   [2.0346 ns 2.0356 ns 2.0374 ns]
                        thrpt:  [490.83 Melem/s 491.25 Melem/s 491.51 Melem/s]
                 change:
                        time:   [−0.2758% −0.1012% +0.1304%] (p = 0.37 > 0.05)
                        thrpt:  [−0.1302% +0.1013% +0.2766%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
baggage/get             time:   [19.823 ns 20.410 ns 21.013 ns]
                        thrpt:  [47.589 Melem/s 48.997 Melem/s 50.447 Melem/s]
                 change:
                        time:   [+0.8491% +5.5058% +10.701%] (p = 0.03 < 0.05)
                        thrpt:  [−9.6667% −5.2185% −0.8419%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
baggage/set             time:   [76.865 ns 77.609 ns 78.335 ns]
                        thrpt:  [12.766 Melem/s 12.885 Melem/s 13.010 Melem/s]
                 change:
                        time:   [−4.7317% −3.9001% −3.1496%] (p = 0.00 < 0.05)
                        thrpt:  [+3.2521% +4.0584% +4.9668%]
                        Performance has improved.
baggage/merge           time:   [1.6233 µs 1.6270 µs 1.6306 µs]
                        thrpt:  [613.27 Kelem/s 614.64 Kelem/s 616.03 Kelem/s]
                 change:
                        time:   [+0.7739% +1.1218% +1.4986%] (p = 0.00 < 0.05)
                        thrpt:  [−1.4764% −1.1093% −0.7679%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe

span/create             time:   [336.53 ns 338.73 ns 340.95 ns]
                        thrpt:  [2.9330 Melem/s 2.9522 Melem/s 2.9715 Melem/s]
                 change:
                        time:   [+1.0321% +1.6507% +2.3024%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2506% −1.6239% −1.0216%]
                        Performance has regressed.
span/set_attribute      time:   [74.783 ns 75.516 ns 76.169 ns]
                        thrpt:  [13.129 Melem/s 13.242 Melem/s 13.372 Melem/s]
                 change:
                        time:   [+0.9748% +1.7306% +2.4494%] (p = 0.00 < 0.05)
                        thrpt:  [−2.3908% −1.7011% −0.9654%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) low severe
  4 (4.00%) low mild
  5 (5.00%) high mild
span/add_event          time:   [47.944 ns 48.206 ns 48.467 ns]
                        thrpt:  [20.633 Melem/s 20.744 Melem/s 20.858 Melem/s]
                 change:
                        time:   [−0.0424% +1.1409% +2.3150%] (p = 0.05 > 0.05)
                        thrpt:  [−2.2626% −1.1281% +0.0425%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
span/with_kind          time:   [343.22 ns 346.71 ns 350.48 ns]
                        thrpt:  [2.8532 Melem/s 2.8843 Melem/s 2.9136 Melem/s]
                 change:
                        time:   [+4.3282% +5.0982% +5.8778%] (p = 0.00 < 0.05)
                        thrpt:  [−5.5515% −4.8508% −4.1486%]
                        Performance has regressed.

context_store/create_context
                        time:   [978.31 ns 984.29 ns 990.66 ns]
                        thrpt:  [1.0094 Melem/s 1.0160 Melem/s 1.0222 Melem/s]
                 change:
                        time:   [+0.4419% +1.2054% +1.9376%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9008% −1.1911% −0.4399%]
                        Change within noise threshold.
context_store/get_context
                        time:   [50.621 ns 50.700 ns 50.782 ns]
                        thrpt:  [19.692 Melem/s 19.724 Melem/s 19.755 Melem/s]
                 change:
                        time:   [−0.2227% −0.0373% +0.1544%] (p = 0.70 > 0.05)
                        thrpt:  [−0.1542% +0.0374% +0.2232%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
context_store/add_span  time:   [381.80 ns 382.88 ns 384.15 ns]
                        thrpt:  [2.6031 Melem/s 2.6118 Melem/s 2.6192 Melem/s]
                 change:
                        time:   [−2.3058% −1.5542% −0.9372%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9461% +1.5787% +2.3602%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe

propagation_context/from_context
                        time:   [844.41 ns 846.63 ns 848.91 ns]
                        thrpt:  [1.1780 Melem/s 1.1812 Melem/s 1.1843 Melem/s]
                 change:
                        time:   [−2.2218% −1.6605% −1.1012%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1134% +1.6885% +2.2723%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
propagation_context/to_context
                        time:   [922.59 ns 928.33 ns 934.95 ns]
                        thrpt:  [1.0696 Melem/s 1.0772 Melem/s 1.0839 Melem/s]
                 change:
                        time:   [+1.4252% +2.3800% +3.3664%] (p = 0.00 < 0.05)
                        thrpt:  [−3.2568% −2.3247% −1.4052%]
                        Performance has regressed.

context_store_concurrent/concurrent_get
                        time:   [58.782 ns 58.835 ns 58.900 ns]
                        thrpt:  [16.978 Melem/s 16.997 Melem/s 17.012 Melem/s]
                 change:
                        time:   [+0.0762% +0.2594% +0.4681%] (p = 0.01 < 0.05)
                        thrpt:  [−0.4659% −0.2587% −0.0762%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe

endpoint/create         time:   [3.0531 ns 3.0543 ns 3.0559 ns]
                        thrpt:  [327.24 Melem/s 327.40 Melem/s 327.54 Melem/s]
                 change:
                        time:   [−0.1502% −0.0153% +0.1380%] (p = 0.84 > 0.05)
                        thrpt:  [−0.1378% +0.0153% +0.1504%]
                        No change in performance detected.
Found 18 outliers among 100 measurements (18.00%)
  5 (5.00%) high mild
  13 (13.00%) high severe
endpoint/create_with_config
                        time:   [106.03 ns 107.69 ns 109.36 ns]
                        thrpt:  [9.1442 Melem/s 9.2857 Melem/s 9.4315 Melem/s]
                 change:
                        time:   [−6.2013% −4.8853% −3.6101%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7454% +5.1362% +6.6113%]
                        Performance has improved.
endpoint/effective_weight
                        time:   [310.31 ps 310.74 ps 311.35 ps]
                        thrpt:  [3.2118 Gelem/s 3.2181 Gelem/s 3.2226 Gelem/s]
                 change:
                        time:   [−0.0041% +0.1690% +0.3909%] (p = 0.11 > 0.05)
                        thrpt:  [−0.3893% −0.1687% +0.0041%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe

load_metrics/load_score time:   [310.28 ps 310.41 ps 310.63 ps]
                        thrpt:  [3.2193 Gelem/s 3.2215 Gelem/s 3.2229 Gelem/s]
                 change:
                        time:   [−0.0699% +0.0626% +0.1961%] (p = 0.37 > 0.05)
                        thrpt:  [−0.1957% −0.0626% +0.0700%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
load_metrics/is_overloaded
                        time:   [313.66 ps 315.17 ps 316.78 ps]
                        thrpt:  [3.1568 Gelem/s 3.1729 Gelem/s 3.1882 Gelem/s]
                 change:
                        time:   [+0.7513% +1.1081% +1.5002%] (p = 0.00 < 0.05)
                        thrpt:  [−1.4780% −1.0960% −0.7457%]
                        Change within noise threshold.

lb_strategies/round_robin
                        time:   [293.26 ns 296.38 ns 299.27 ns]
                        thrpt:  [3.3415 Melem/s 3.3740 Melem/s 3.4099 Melem/s]
                 change:
                        time:   [+1.1580% +2.6025% +4.0010%] (p = 0.00 < 0.05)
                        thrpt:  [−3.8471% −2.5365% −1.1447%]
                        Performance has regressed.
lb_strategies/weighted_round_robin
                        time:   [321.20 ns 324.03 ns 326.61 ns]
                        thrpt:  [3.0617 Melem/s 3.0861 Melem/s 3.1133 Melem/s]
                 change:
                        time:   [−1.5807% −0.5382% +0.5788%] (p = 0.32 > 0.05)
                        thrpt:  [−0.5755% +0.5411% +1.6060%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
lb_strategies/least_connections
                        time:   [298.61 ns 303.52 ns 308.73 ns]
                        thrpt:  [3.2390 Melem/s 3.2947 Melem/s 3.3488 Melem/s]
                 change:
                        time:   [+7.4944% +8.9109% +10.260%] (p = 0.00 < 0.05)
                        thrpt:  [−9.3054% −8.1818% −6.9719%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
lb_strategies/random    time:   [568.30 ns 572.22 ns 576.32 ns]
                        thrpt:  [1.7352 Melem/s 1.7476 Melem/s 1.7596 Melem/s]
                 change:
                        time:   [−2.3623% −1.7105% −1.0498%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0610% +1.7403% +2.4195%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
lb_strategies/power_of_two
                        time:   [838.54 ns 842.44 ns 846.35 ns]
                        thrpt:  [1.1815 Melem/s 1.1870 Melem/s 1.1925 Melem/s]
                 change:
                        time:   [−1.7304% −1.1233% −0.5367%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5396% +1.1361% +1.7609%]
                        Change within noise threshold.
lb_strategies/consistent_hash
                        time:   [48.833 µs 50.400 µs 51.855 µs]
                        thrpt:  [19.285 Kelem/s 19.841 Kelem/s 20.478 Kelem/s]
                 change:
                        time:   [+1.4795% +5.5241% +9.6429%] (p = 0.01 < 0.05)
                        thrpt:  [−8.7948% −5.2349% −1.4580%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  11 (11.00%) low mild
lb_strategies/least_load
                        time:   [496.04 ns 500.18 ns 504.46 ns]
                        thrpt:  [1.9823 Melem/s 1.9993 Melem/s 2.0160 Melem/s]
                 change:
                        time:   [−0.6591% +0.0678% +0.8052%] (p = 0.86 > 0.05)
                        thrpt:  [−0.7987% −0.0678% +0.6634%]
                        No change in performance detected.

lb_scaling/select/10    time:   [301.89 ns 303.06 ns 304.17 ns]
                        thrpt:  [3.2876 Melem/s 3.2997 Melem/s 3.3124 Melem/s]
                 change:
                        time:   [−1.1346% −0.0576% +1.0677%] (p = 0.92 > 0.05)
                        thrpt:  [−1.0565% +0.0576% +1.1476%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
lb_scaling/select/50    time:   [656.64 ns 663.11 ns 669.70 ns]
                        thrpt:  [1.4932 Melem/s 1.5080 Melem/s 1.5229 Melem/s]
                 change:
                        time:   [−0.5862% +0.3801% +1.4022%] (p = 0.44 > 0.05)
                        thrpt:  [−1.3829% −0.3787% +0.5897%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
lb_scaling/select/100   time:   [1.0099 µs 1.0205 µs 1.0312 µs]
                        thrpt:  [969.73 Kelem/s 979.91 Kelem/s 990.18 Kelem/s]
                 change:
                        time:   [−3.5798% −2.8292% −1.9983%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0391% +2.9116% +3.7127%]
                        Performance has improved.
lb_scaling/select/500   time:   [2.1679 µs 2.1771 µs 2.1873 µs]
                        thrpt:  [457.19 Kelem/s 459.33 Kelem/s 461.27 Kelem/s]
                 change:
                        time:   [−1.3525% −0.8344% −0.2916%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2925% +0.8414% +1.3710%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

lb_zone_aware/zone_match
                        time:   [354.76 ns 357.14 ns 359.40 ns]
                        thrpt:  [2.7824 Melem/s 2.8000 Melem/s 2.8188 Melem/s]
                 change:
                        time:   [−3.5252% −2.5140% −1.4733%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4954% +2.5789% +3.6540%]
                        Performance has improved.
lb_zone_aware/zone_fallback
                        time:   [306.51 ns 307.60 ns 308.83 ns]
                        thrpt:  [3.2380 Melem/s 3.2510 Melem/s 3.2626 Melem/s]
                 change:
                        time:   [+2.0985% +3.2692% +4.4756%] (p = 0.00 < 0.05)
                        thrpt:  [−4.2839% −3.1657% −2.0554%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild

lb_health_updates/update_health
                        time:   [25.859 ns 26.151 ns 26.479 ns]
                        thrpt:  [37.765 Melem/s 38.240 Melem/s 38.671 Melem/s]
                 change:
                        time:   [+0.2029% +2.7753% +5.4690%] (p = 0.04 < 0.05)
                        thrpt:  [−5.1854% −2.7004% −0.2025%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
lb_health_updates/update_metrics
                        time:   [126.49 ns 129.75 ns 132.92 ns]
                        thrpt:  [7.5233 Melem/s 7.7070 Melem/s 7.9060 Melem/s]
                 change:
                        time:   [−3.1835% −1.1582% +0.9406%] (p = 0.27 > 0.05)
                        thrpt:  [−0.9318% +1.1718% +3.2882%]
                        No change in performance detected.
Found 22 outliers among 100 measurements (22.00%)
  20 (20.00%) low severe
  2 (2.00%) high mild

     Running benches/origin_cache_bench.rs (target/release/deps/origin_cache_bench-737315febe330fd8)
Gnuplot not found, using plotters backend
origin_cache_hit/dashmap
                        time:   [12.461 ns 12.515 ns 12.596 ns]
                        change: [−5.9330% −3.2481% −0.8502%] (p = 0.01 < 0.05)
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
origin_cache_hit/mutex_lru
                        time:   [11.175 ns 11.180 ns 11.187 ns]
                        change: [−0.0924% +0.0393% +0.1700%] (p = 0.57 > 0.05)
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe

origin_cache_insert_256/dashmap
                        time:   [11.860 µs 11.902 µs 11.947 µs]
                        change: [−0.3265% +0.1201% +0.5584%] (p = 0.60 > 0.05)
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
origin_cache_insert_256/mutex_lru
                        time:   [16.744 µs 17.765 µs 18.880 µs]
                        change: [+24.343% +34.776% +46.143%] (p = 0.00 < 0.05)
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

     Running benches/parallel.rs (target/release/deps/parallel-e1dbe26b4bd77aab)
Gnuplot not found, using plotters backend
shard_manager/ingest_json/1
                        time:   [355.60 ns 361.93 ns 368.18 ns]
                        thrpt:  [2.7160 Melem/s 2.7630 Melem/s 2.8122 Melem/s]
                 change:
                        time:   [−4.6835% −2.3720% −0.2493%] (p = 0.05 < 0.05)
                        thrpt:  [+0.2499% +2.4296% +4.9137%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
shard_manager/ingest_json/4
                        time:   [356.36 ns 363.88 ns 371.51 ns]
                        thrpt:  [2.6917 Melem/s 2.7482 Melem/s 2.8062 Melem/s]
                 change:
                        time:   [−5.8718% −3.4946% −1.0432%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0542% +3.6212% +6.2381%]
                        Performance has improved.
shard_manager/ingest_json/8
                        time:   [353.37 ns 359.94 ns 366.70 ns]
                        thrpt:  [2.7270 Melem/s 2.7783 Melem/s 2.8299 Melem/s]
                 change:
                        time:   [−4.5662% −2.2450% +0.0657%] (p = 0.07 > 0.05)
                        thrpt:  [−0.0656% +2.2965% +4.7846%]
                        No change in performance detected.
shard_manager/ingest_json/16
                        time:   [361.01 ns 368.05 ns 375.02 ns]
                        thrpt:  [2.6665 Melem/s 2.7170 Melem/s 2.7700 Melem/s]
                 change:
                        time:   [−3.2882% −0.8666% +1.5624%] (p = 0.49 > 0.05)
                        thrpt:  [−1.5383% +0.8742% +3.4000%]
                        No change in performance detected.
shard_manager/ingest_raw/1
                        time:   [46.507 ns 46.593 ns 46.707 ns]
                        thrpt:  [21.410 Melem/s 21.463 Melem/s 21.502 Melem/s]
                 change:
                        time:   [+0.0658% +0.3108% +0.5744%] (p = 0.02 < 0.05)
                        thrpt:  [−0.5711% −0.3098% −0.0658%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
shard_manager/ingest_raw/4
                        time:   [46.634 ns 46.750 ns 46.897 ns]
                        thrpt:  [21.323 Melem/s 21.390 Melem/s 21.444 Melem/s]
                 change:
                        time:   [+1.0377% +1.4361% +1.8408%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8076% −1.4158% −1.0270%]
                        Performance has regressed.
shard_manager/ingest_raw/8
                        time:   [46.394 ns 46.408 ns 46.426 ns]
                        thrpt:  [21.539 Melem/s 21.548 Melem/s 21.554 Melem/s]
                 change:
                        time:   [−0.4485% −0.1529% +0.1442%] (p = 0.32 > 0.05)
                        thrpt:  [−0.1440% +0.1531% +0.4505%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
shard_manager/ingest_raw/16
                        time:   [46.376 ns 46.393 ns 46.420 ns]
                        thrpt:  [21.542 Melem/s 21.555 Melem/s 21.563 Melem/s]
                 change:
                        time:   [−0.0690% +0.0552% +0.1898%] (p = 0.45 > 0.05)
                        thrpt:  [−0.1895% −0.0552% +0.0691%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe

event_size/small_50b_json
                        time:   [280.90 ns 289.06 ns 297.40 ns]
                        thrpt:  [3.3625 Melem/s 3.4595 Melem/s 3.5599 Melem/s]
                 change:
                        time:   [−4.2534% −1.2169% +1.7940%] (p = 0.44 > 0.05)
                        thrpt:  [−1.7623% +1.2319% +4.4423%]
                        No change in performance detected.
event_size/small_50b_raw
                        time:   [46.077 ns 46.106 ns 46.134 ns]
                        thrpt:  [21.676 Melem/s 21.689 Melem/s 21.703 Melem/s]
                 change:
                        time:   [−0.0863% +0.0369% +0.1617%] (p = 0.56 > 0.05)
                        thrpt:  [−0.1614% −0.0369% +0.0864%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
event_size/medium_200b_json
                        time:   [800.48 ns 810.29 ns 820.02 ns]
                        thrpt:  [1.2195 Melem/s 1.2341 Melem/s 1.2493 Melem/s]
                 change:
                        time:   [−1.7826% −0.3167% +1.1292%] (p = 0.66 > 0.05)
                        thrpt:  [−1.1166% +0.3177% +1.8149%]
                        No change in performance detected.
event_size/medium_200b_raw
                        time:   [46.151 ns 46.263 ns 46.392 ns]
                        thrpt:  [21.556 Melem/s 21.616 Melem/s 21.668 Melem/s]
                 change:
                        time:   [−0.1186% +0.1448% +0.4032%] (p = 0.27 > 0.05)
                        thrpt:  [−0.4015% −0.1445% +0.1187%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) high mild
  12 (12.00%) high severe
event_size/large_1kb_json
                        time:   [2.6443 µs 2.6590 µs 2.6726 µs]
                        thrpt:  [374.16 Kelem/s 376.08 Kelem/s 378.17 Kelem/s]
                 change:
                        time:   [−3.6137% −2.6952% −1.8933%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9299% +2.7698% +3.7492%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild
event_size/large_1kb_raw
                        time:   [46.412 ns 46.430 ns 46.456 ns]
                        thrpt:  [21.526 Melem/s 21.538 Melem/s 21.546 Melem/s]
                 change:
                        time:   [−0.0936% +0.0500% +0.2228%] (p = 0.56 > 0.05)
                        thrpt:  [−0.2223% −0.0500% +0.0937%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe

Benchmarking parallel/threads/1: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
parallel/threads/1      time:   [1.5012 ms 1.5022 ms 1.5032 ms]
                        thrpt:  [6.6527 Melem/s 6.6568 Melem/s 6.6612 Melem/s]
                 change:
                        time:   [−0.1119% +0.2454% +0.6384%] (p = 0.19 > 0.05)
                        thrpt:  [−0.6343% −0.2448% +0.1121%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
parallel/threads/2      time:   [2.2428 ms 2.2830 ms 2.3285 ms]
                        thrpt:  [8.5892 Melem/s 8.7606 Melem/s 8.9173 Melem/s]
                 change:
                        time:   [+1.9795% +3.7056% +5.7990%] (p = 0.00 < 0.05)
                        thrpt:  [−5.4811% −3.5732% −1.9411%]
                        Performance has regressed.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) high mild
  13 (13.00%) high severe
parallel/threads/4      time:   [2.9309 ms 2.9991 ms 3.0872 ms]
                        thrpt:  [12.957 Melem/s 13.337 Melem/s 13.648 Melem/s]
                 change:
                        time:   [−4.9548% −0.9177% +2.9657%] (p = 0.68 > 0.05)
                        thrpt:  [−2.8803% +0.9262% +5.2131%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
parallel/threads/8      time:   [9.8141 ms 9.8780 ms 9.9497 ms]
                        thrpt:  [8.0404 Melem/s 8.0988 Melem/s 8.1515 Melem/s]
                 change:
                        time:   [+0.5202% +1.1678% +1.9292%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8927% −1.1543% −0.5176%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) high mild
  13 (13.00%) high severe

     Running benches/placement.rs (target/release/deps/placement-e77e28c33cade87a)
Gnuplot not found, using plotters backend
standard_placement_score/baseline_no_custom_filter/100
                        time:   [61.239 µs 61.852 µs 62.388 µs]
                        thrpt:  [1.6029 Melem/s 1.6168 Melem/s 1.6329 Melem/s]
                 change:
                        time:   [−1.4173% −0.0449% +1.3206%] (p = 0.95 > 0.05)
                        thrpt:  [−1.3034% +0.0450% +1.4377%]
                        No change in performance detected.
standard_placement_score/with_custom_filter_rust_callback/100
                        time:   [65.113 µs 65.764 µs 66.456 µs]
                        thrpt:  [1.5047 Melem/s 1.5206 Melem/s 1.5358 Melem/s]
                 change:
                        time:   [+0.5312% +1.7187% +3.0094%] (p = 0.01 < 0.05)
                        thrpt:  [−2.9215% −1.6897% −0.5284%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
standard_placement_score/with_custom_filter_predicate/100
                        time:   [116.11 µs 117.21 µs 118.26 µs]
                        thrpt:  [845.62 Kelem/s 853.16 Kelem/s 861.28 Kelem/s]
                 change:
                        time:   [−0.1639% +0.5968% +1.3484%] (p = 0.16 > 0.05)
                        thrpt:  [−1.3305% −0.5932% +0.1642%]
                        No change in performance detected.

     Running benches/redex.rs (target/release/deps/redex-7747e7cc38414c23)
Gnuplot not found, using plotters backend
redex_append_inline/heap_file
                        time:   [35.171 ns 35.356 ns 35.538 ns]
                        thrpt:  [28.139 Melem/s 28.283 Melem/s 28.432 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

redex_append_heap/heap_file/32
                        time:   [39.721 ns 40.360 ns 40.973 ns]
                        thrpt:  [744.83 MiB/s 756.13 MiB/s 768.30 MiB/s]
Found 15 outliers among 100 measurements (15.00%)
  14 (14.00%) high mild
  1 (1.00%) high severe
redex_append_heap/heap_file/256
                        time:   [71.383 ns 72.420 ns 73.659 ns]
                        thrpt:  [3.2368 GiB/s 3.2922 GiB/s 3.3400 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
redex_append_heap/heap_file/1024
                        time:   [176.87 ns 179.09 ns 181.38 ns]
                        thrpt:  [5.2578 GiB/s 5.3251 GiB/s 5.3920 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) low mild
  1 (1.00%) high mild

redex_append_watcher_paths/no_watchers
                        time:   [70.809 ns 71.826 ns 73.046 ns]
                        thrpt:  [3.2639 GiB/s 3.3194 GiB/s 3.3671 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
redex_append_watcher_paths/with_tail
                        time:   [201.43 ns 203.22 ns 204.78 ns]
                        thrpt:  [1.1643 GiB/s 1.1732 GiB/s 1.1836 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_batch/batch_64_x_64B
                        time:   [1.7024 µs 1.7180 µs 1.7330 µs]
                        thrpt:  [36.929 Melem/s 37.252 Melem/s 37.595 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

redex_append_disk/disk_file/32
                        time:   [4.3119 µs 4.3346 µs 4.3668 µs]
                        thrpt:  [6.9885 MiB/s 7.0404 MiB/s 7.0774 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) high mild
  9 (9.00%) high severe
redex_append_disk/disk_file/256
                        time:   [4.5152 µs 4.5329 µs 4.5561 µs]
                        thrpt:  [53.585 MiB/s 53.860 MiB/s 54.071 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
redex_append_disk/disk_file/1024
                        time:   [5.3155 µs 5.3326 µs 5.3536 µs]
                        thrpt:  [182.41 MiB/s 183.13 MiB/s 183.72 MiB/s]
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe

redex_append_batch_disk/batch_64_x/64
                        time:   [11.610 µs 11.661 µs 11.727 µs]
                        thrpt:  [5.4575 Melem/s 5.4884 Melem/s 5.5127 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
redex_append_batch_disk/batch_64_x/1024
                        time:   [29.667 µs 30.166 µs 30.676 µs]
                        thrpt:  [2.0864 Melem/s 2.1216 Melem/s 2.1573 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

redex_append_disk_policies/disk_file_256B/never
                        time:   [4.5019 µs 4.5343 µs 4.5906 µs]
                        thrpt:  [53.183 MiB/s 53.843 MiB/s 54.231 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe
redex_append_disk_policies/disk_file_256B/every_n_1
                        time:   [669.44 µs 838.95 µs 1.0090 ms]
                        thrpt:  [247.76 KiB/s 297.99 KiB/s 373.45 KiB/s]
redex_append_disk_policies/disk_file_256B/every_n_64
                        time:   [198.25 µs 208.49 µs 217.14 µs]
                        thrpt:  [1.1244 MiB/s 1.1710 MiB/s 1.2315 MiB/s]
redex_append_disk_policies/disk_file_256B/interval_50ms
                        time:   [5.0701 µs 5.2522 µs 5.4133 µs]
                        thrpt:  [45.100 MiB/s 46.484 MiB/s 48.153 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
redex_append_disk_policies/disk_file_256B/interval_or_bytes
                        time:   [7.8292 µs 8.0491 µs 8.2581 µs]
                        thrpt:  [29.564 MiB/s 30.331 MiB/s 31.183 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

redex_append_batch_disk_policies/batch_64_x_64B/never
                        time:   [11.894 µs 12.090 µs 12.360 µs]
                        thrpt:  [5.1782 Melem/s 5.2937 Melem/s 5.3807 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  7 (7.00%) high mild
  11 (11.00%) high severe
redex_append_batch_disk_policies/batch_64_x_64B/every_n_1
                        time:   [1.2342 ms 1.5113 ms 1.7730 ms]
                        thrpt:  [36.098 Kelem/s 42.347 Kelem/s 51.855 Kelem/s]
Benchmarking redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.4s, enable flat sampling, or reduce sample count to 60.
redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small
                        time:   [1.9017 ms 2.0903 ms 2.2565 ms]
                        thrpt:  [28.362 Kelem/s 30.618 Kelem/s 33.654 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild

redex_tail/append_to_next
                        time:   [162.05 ns 163.11 ns 164.34 ns]
                        thrpt:  [6.0849 Melem/s 6.1307 Melem/s 6.1711 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe