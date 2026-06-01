     Running unittests src/bin/net-blob.rs (target/release/deps/net_blob-23b9993d4b81535e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running benches/auth_guard.rs (target/release/deps/auth_guard-cc36a43992552050)
Gnuplot not found, using plotters backend
auth_guard_check_fast_hit/single_thread
                        time:   [23.875 ns 24.055 ns 24.253 ns]
                        thrpt:  [41.233 Melem/s 41.571 Melem/s 41.885 Melem/s]
                 change:
                        time:   [−3.1249% −2.3687% −1.5502%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5746% +2.4261% +3.2257%]
                        Performance has improved.
Found 4 outliers among 50 measurements (8.00%)
  4 (8.00%) high severe

auth_guard_check_fast_miss/single_thread
                        time:   [3.7871 ns 3.7900 ns 3.7941 ns]
                        thrpt:  [263.56 Melem/s 263.85 Melem/s 264.06 Melem/s]
                 change:
                        time:   [−6.8004% −4.9411% −3.5664%] (p = 0.00 < 0.05)
                        thrpt:  [+3.6982% +5.1979% +7.2966%]
                        Performance has improved.
Found 7 outliers among 50 measurements (14.00%)
  3 (6.00%) high mild
  4 (8.00%) high severe

auth_guard_check_fast_contended/eight_threads
                        time:   [28.625 ns 28.810 ns 29.016 ns]
                        thrpt:  [34.464 Melem/s 34.711 Melem/s 34.935 Melem/s]
                 change:
                        time:   [−9.9319% −7.2847% −4.6451%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8714% +7.8570% +11.027%]
                        Performance has improved.
Found 4 outliers among 50 measurements (8.00%)
  3 (6.00%) high mild
  1 (2.00%) high severe

auth_guard_allow_channel/insert
                        time:   [162.93 ns 167.09 ns 170.63 ns]
                        thrpt:  [5.8605 Melem/s 5.9847 Melem/s 6.1377 Melem/s]
                 change:
                        time:   [−9.9539% −4.5694% +1.0309%] (p = 0.12 > 0.05)
                        thrpt:  [−1.0204% +4.7882% +11.054%]
                        No change in performance detected.

auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.8213 ms 2.8233 ms 2.8255 ms]
                        change: [−0.1248% +0.0726% +0.2521%] (p = 0.49 > 0.05)
                        No change in performance detected.
Found 5 outliers among 50 measurements (10.00%)
  2 (4.00%) high mild
  3 (6.00%) high severe

     Running benches/cortex.rs (target/release/deps/cortex-9e927961583d1018)
Gnuplot not found, using plotters backend
cortex_ingest/tasks_create
                        time:   [239.71 ns 245.91 ns 252.90 ns]
                        thrpt:  [3.9542 Melem/s 4.0666 Melem/s 4.1717 Melem/s]
                 change:
                        time:   [−11.925% −0.5647% +12.404%] (p = 0.93 > 0.05)
                        thrpt:  [−11.035% +0.5679% +13.540%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
cortex_ingest/memories_store
                        time:   [689.27 ns 711.11 ns 738.02 ns]
                        thrpt:  [1.3550 Melem/s 1.4062 Melem/s 1.4508 Melem/s]
                 change:
                        time:   [−8.9583% −1.8341% +6.3158%] (p = 0.65 > 0.05)
                        thrpt:  [−5.9406% +1.8684% +9.8397%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

cortex_fold_barrier/tasks_create_and_wait
                        time:   [5.7014 µs 5.7369 µs 5.7852 µs]
                        thrpt:  [172.86 Kelem/s 174.31 Kelem/s 175.40 Kelem/s]
                 change:
                        time:   [−0.9392% +0.2727% +1.6514%] (p = 0.68 > 0.05)
                        thrpt:  [−1.6246% −0.2719% +0.9481%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) high mild
  12 (12.00%) high severe
cortex_fold_barrier/memories_store_and_wait
                        time:   [5.9888 µs 6.0030 µs 6.0234 µs]
                        thrpt:  [166.02 Kelem/s 166.58 Kelem/s 166.98 Kelem/s]
                 change:
                        time:   [−1.4284% −0.8443% −0.3028%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3037% +0.8515% +1.4491%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe

cortex_query/tasks_find_many/100
                        time:   [2.2008 µs 2.2065 µs 2.2123 µs]
                        thrpt:  [45.202 Melem/s 45.320 Melem/s 45.438 Melem/s]
                 change:
                        time:   [+1.4273% +1.8413% +2.2229%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1745% −1.8081% −1.4072%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_count_where/100
                        time:   [164.21 ns 164.42 ns 164.74 ns]
                        thrpt:  [607.02 Melem/s 608.21 Melem/s 608.99 Melem/s]
                 change:
                        time:   [−0.2524% −0.0334% +0.2419%] (p = 0.80 > 0.05)
                        thrpt:  [−0.2413% +0.0334% +0.2530%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
cortex_query/tasks_find_unique/100
                        time:   [9.0480 ns 9.1100 ns 9.1769 ns]
                        thrpt:  [10.897 Gelem/s 10.977 Gelem/s 11.052 Gelem/s]
                 change:
                        time:   [−1.8650% −0.7987% +0.3158%] (p = 0.14 > 0.05)
                        thrpt:  [−0.3148% +0.8052% +1.9004%]
                        No change in performance detected.
Found 22 outliers among 100 measurements (22.00%)
  7 (7.00%) high mild
  15 (15.00%) high severe
cortex_query/memories_find_many_tag/100
                        time:   [1.0864 µs 1.0893 µs 1.0921 µs]
                        thrpt:  [91.564 Melem/s 91.805 Melem/s 92.044 Melem/s]
                 change:
                        time:   [−5.6002% −5.2082% −4.8066%] (p = 0.00 < 0.05)
                        thrpt:  [+5.0493% +5.4944% +5.9324%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_count_where/100
                        time:   [729.73 ns 731.00 ns 732.38 ns]
                        thrpt:  [136.54 Melem/s 136.80 Melem/s 137.04 Melem/s]
                 change:
                        time:   [−6.0907% −5.7009% −5.2991%] (p = 0.00 < 0.05)
                        thrpt:  [+5.5957% +6.0455% +6.4857%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
cortex_query/tasks_find_many/1000
                        time:   [19.272 µs 19.381 µs 19.511 µs]
                        thrpt:  [51.253 Melem/s 51.596 Melem/s 51.890 Melem/s]
                 change:
                        time:   [+0.8377% +1.3970% +1.9529%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9154% −1.3778% −0.8308%]
                        Change within noise threshold.
cortex_query/tasks_count_where/1000
                        time:   [1.6376 µs 1.6407 µs 1.6446 µs]
                        thrpt:  [608.06 Melem/s 609.49 Melem/s 610.65 Melem/s]
                 change:
                        time:   [+0.2980% +0.6427% +1.0243%] (p = 0.00 < 0.05)
                        thrpt:  [−1.0140% −0.6386% −0.2971%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe
cortex_query/tasks_find_unique/1000
                        time:   [8.9494 ns 9.0044 ns 9.0671 ns]
                        thrpt:  [110.29 Gelem/s 111.06 Gelem/s 111.74 Gelem/s]
                 change:
                        time:   [−1.8911% −1.1478% −0.4264%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4282% +1.1612% +1.9275%]
                        Change within noise threshold.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) high mild
  16 (16.00%) high severe
cortex_query/memories_find_many_tag/1000
                        time:   [13.044 µs 13.067 µs 13.091 µs]
                        thrpt:  [76.389 Melem/s 76.529 Melem/s 76.664 Melem/s]
                 change:
                        time:   [−1.7303% −1.3495% −0.9644%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9738% +1.3679% +1.7608%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_count_where/1000
                        time:   [10.614 µs 10.643 µs 10.685 µs]
                        thrpt:  [93.590 Melem/s 93.963 Melem/s 94.216 Melem/s]
                 change:
                        time:   [−4.2637% −3.8274% −3.4089%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5292% +3.9797% +4.4536%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
cortex_query/tasks_find_many/10000
                        time:   [247.43 µs 259.48 µs 271.60 µs]
                        thrpt:  [36.818 Melem/s 38.539 Melem/s 40.415 Melem/s]
                 change:
                        time:   [+5.0727% +10.573% +16.312%] (p = 0.00 < 0.05)
                        thrpt:  [−14.025% −9.5617% −4.8278%]
                        Performance has regressed.
cortex_query/tasks_count_where/10000
                        time:   [23.516 µs 24.212 µs 24.975 µs]
                        thrpt:  [400.40 Melem/s 413.02 Melem/s 425.24 Melem/s]
                 change:
                        time:   [−8.2269% −5.6704% −2.9200%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0079% +6.0112% +8.9643%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
cortex_query/tasks_find_unique/10000
                        time:   [9.0401 ns 9.0962 ns 9.1624 ns]
                        thrpt:  [1091.4 Gelem/s 1099.4 Gelem/s 1106.2 Gelem/s]
                 change:
                        time:   [−0.4961% +0.3561% +1.1199%] (p = 0.38 > 0.05)
                        thrpt:  [−1.1075% −0.3548% +0.4986%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) high mild
  12 (12.00%) high severe
cortex_query/memories_find_many_tag/10000
                        time:   [167.29 µs 167.69 µs 168.27 µs]
                        thrpt:  [59.427 Melem/s 59.635 Melem/s 59.777 Melem/s]
                 change:
                        time:   [−1.4316% −0.8648% −0.2518%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2525% +0.8724% +1.4524%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
cortex_query/memories_count_where/10000
                        time:   [150.66 µs 151.38 µs 152.29 µs]
                        thrpt:  [65.662 Melem/s 66.060 Melem/s 66.375 Melem/s]
                 change:
                        time:   [+1.1519% +1.5417% +1.9566%] (p = 0.00 < 0.05)
                        thrpt:  [−1.9191% −1.5182% −1.1387%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe

cortex_snapshot/tasks_encode/100
                        time:   [3.2823 µs 3.2977 µs 3.3147 µs]
                        thrpt:  [30.169 Melem/s 30.324 Melem/s 30.467 Melem/s]
                 change:
                        time:   [+1.1249% +1.6390% +2.1778%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1314% −1.6126% −1.1124%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
cortex_snapshot/memories_encode/100
                        time:   [5.6719 µs 5.6881 µs 5.7052 µs]
                        thrpt:  [17.528 Melem/s 17.581 Melem/s 17.631 Melem/s]
                 change:
                        time:   [+1.1101% +1.4765% +1.9068%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8712% −1.4550% −1.0980%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_3939/100
                        time:   [2.2466 µs 2.2580 µs 2.2700 µs]
                        thrpt:  [44.053 Melem/s 44.287 Melem/s 44.511 Melem/s]
                 change:
                        time:   [−1.1489% −0.6875% −0.2102%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2106% +0.6923% +1.1622%]
                        Change within noise threshold.
cortex_snapshot/netdb_bundle_decode/100
                        time:   [2.2419 µs 2.2435 µs 2.2453 µs]
                        thrpt:  [44.537 Melem/s 44.572 Melem/s 44.606 Melem/s]
                 change:
                        time:   [−0.3801% −0.0892% +0.1791%] (p = 0.55 > 0.05)
                        thrpt:  [−0.1788% +0.0893% +0.3816%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/tasks_encode/1000
                        time:   [30.663 µs 30.795 µs 30.954 µs]
                        thrpt:  [32.307 Melem/s 32.473 Melem/s 32.613 Melem/s]
                 change:
                        time:   [+0.4238% +0.9214% +1.4100%] (p = 0.00 < 0.05)
                        thrpt:  [−1.3904% −0.9130% −0.4220%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/memories_encode/1000
                        time:   [56.364 µs 56.446 µs 56.545 µs]
                        thrpt:  [17.685 Melem/s 17.716 Melem/s 17.742 Melem/s]
                 change:
                        time:   [−0.1169% +0.2196% +0.5480%] (p = 0.20 > 0.05)
                        thrpt:  [−0.5450% −0.2191% +0.1170%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_48274/1000
                        time:   [22.780 µs 22.899 µs 23.035 µs]
                        thrpt:  [43.413 Melem/s 43.669 Melem/s 43.899 Melem/s]
                 change:
                        time:   [−0.5667% +0.0031% +0.5669%] (p = 1.00 > 0.05)
                        thrpt:  [−0.5637% −0.0031% +0.5699%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_decode/1000
                        time:   [26.357 µs 26.387 µs 26.431 µs]
                        thrpt:  [37.834 Melem/s 37.898 Melem/s 37.940 Melem/s]
                 change:
                        time:   [−2.0655% −1.5443% −1.0341%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0449% +1.5685% +2.1090%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
cortex_snapshot/tasks_encode/10000
                        time:   [298.66 µs 305.95 µs 314.12 µs]
                        thrpt:  [31.835 Melem/s 32.685 Melem/s 33.483 Melem/s]
                 change:
                        time:   [−37.993% −34.735% −31.088%] (p = 0.00 < 0.05)
                        thrpt:  [+45.112% +53.221% +61.272%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/memories_encode/10000
                        time:   [801.23 µs 829.04 µs 855.15 µs]
                        thrpt:  [11.694 Melem/s 12.062 Melem/s 12.481 Melem/s]
                 change:
                        time:   [−6.3070% −2.9147% +0.6930%] (p = 0.12 > 0.05)
                        thrpt:  [−0.6883% +3.0022% +6.7315%]
                        No change in performance detected.
cortex_snapshot/netdb_bundle_encode_bytes_511774/10000
                        time:   [331.49 µs 342.78 µs 352.55 µs]
                        thrpt:  [28.365 Melem/s 29.174 Melem/s 30.167 Melem/s]
                 change:
                        time:   [−10.958% −6.2697% −1.4011%] (p = 0.02 < 0.05)
                        thrpt:  [+1.4210% +6.6891% +12.307%]
                        Performance has improved.
cortex_snapshot/netdb_bundle_decode/10000
                        time:   [294.36 µs 302.48 µs 311.80 µs]
                        thrpt:  [32.072 Melem/s 33.060 Melem/s 33.972 Melem/s]
                 change:
                        time:   [+5.9123% +8.2506% +10.720%] (p = 0.00 < 0.05)
                        thrpt:  [−9.6820% −7.6217% −5.5823%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  11 (11.00%) high mild

     Running benches/ingestion.rs (target/release/deps/ingestion-dc99f2916d38481a)
Gnuplot not found, using plotters backend
shard/ingest_raw/1024   time:   [46.299 ns 46.322 ns 46.357 ns]
                        thrpt:  [21.572 Melem/s 21.588 Melem/s 21.599 Melem/s]
                 change:
                        time:   [−0.2981% −0.1050% +0.0568%] (p = 0.26 > 0.05)
                        thrpt:  [−0.0567% +0.1051% +0.2990%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  5 (5.00%) high severe
shard/ingest_raw_pop/1024
                        time:   [43.364 ns 43.394 ns 43.431 ns]
                        thrpt:  [23.025 Melem/s 23.045 Melem/s 23.060 Melem/s]
                 change:
                        time:   [−0.0052% +0.1866% +0.3959%] (p = 0.07 > 0.05)
                        thrpt:  [−0.3943% −0.1862% +0.0052%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
shard/ingest_raw/8192   time:   [46.305 ns 46.339 ns 46.379 ns]
                        thrpt:  [21.561 Melem/s 21.580 Melem/s 21.596 Melem/s]
                 change:
                        time:   [−0.5214% −0.1107% +0.2455%] (p = 0.58 > 0.05)
                        thrpt:  [−0.2449% +0.1109% +0.5242%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
shard/ingest_raw_pop/8192
                        time:   [43.367 ns 43.389 ns 43.424 ns]
                        thrpt:  [23.029 Melem/s 23.047 Melem/s 23.059 Melem/s]
                 change:
                        time:   [−0.1276% +0.0147% +0.1906%] (p = 0.86 > 0.05)
                        thrpt:  [−0.1902% −0.0147% +0.1278%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe
shard/ingest_raw/65536  time:   [45.921 ns 45.968 ns 46.013 ns]
                        thrpt:  [21.733 Melem/s 21.754 Melem/s 21.777 Melem/s]
                 change:
                        time:   [−0.3721% +0.7681% +1.8948%] (p = 0.21 > 0.05)
                        thrpt:  [−1.8595% −0.7622% +0.3735%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
shard/ingest_raw_pop/65536
                        time:   [43.490 ns 43.509 ns 43.532 ns]
                        thrpt:  [22.972 Melem/s 22.984 Melem/s 22.994 Melem/s]
                 change:
                        time:   [−0.4851% +0.0417% +0.5568%] (p = 0.88 > 0.05)
                        thrpt:  [−0.5537% −0.0416% +0.4875%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
shard/ingest_raw/1048576
                        time:   [38.567 ns 39.038 ns 39.421 ns]
                        thrpt:  [25.367 Melem/s 25.616 Melem/s 25.929 Melem/s]
                 change:
                        time:   [−1.4714% +0.1327% +1.8230%] (p = 0.88 > 0.05)
                        thrpt:  [−1.7904% −0.1325% +1.4934%]
                        No change in performance detected.
shard/ingest_raw_pop/1048576
                        time:   [44.756 ns 44.832 ns 44.923 ns]
                        thrpt:  [22.260 Melem/s 22.306 Melem/s 22.343 Melem/s]
                 change:
                        time:   [−2.5007% −1.7234% −0.9923%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0022% +1.7536% +2.5648%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

timestamp/next          time:   [7.4717 ns 7.4761 ns 7.4811 ns]
                        thrpt:  [133.67 Melem/s 133.76 Melem/s 133.84 Melem/s]
                 change:
                        time:   [−0.3795% −0.1606% +0.0307%] (p = 0.13 > 0.05)
                        thrpt:  [−0.0307% +0.1609% +0.3810%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
timestamp/now_raw       time:   [620.75 ps 621.02 ps 621.42 ps]
                        thrpt:  [1.6092 Gelem/s 1.6103 Gelem/s 1.6110 Gelem/s]
                 change:
                        time:   [−0.2166% −0.0216% +0.1754%] (p = 0.84 > 0.05)
                        thrpt:  [−0.1751% +0.0216% +0.2171%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe

event/internal_event_new
                        time:   [293.91 ns 295.98 ns 297.92 ns]
                        thrpt:  [3.3566 Melem/s 3.3786 Melem/s 3.4024 Melem/s]
                 change:
                        time:   [−0.6324% +0.1420% +0.8901%] (p = 0.71 > 0.05)
                        thrpt:  [−0.8822% −0.1418% +0.6364%]
                        No change in performance detected.
event/internal_event_from_bytes
                        time:   [12.432 ns 12.439 ns 12.446 ns]
                        thrpt:  [80.346 Melem/s 80.395 Melem/s 80.436 Melem/s]
                 change:
                        time:   [−0.4284% −0.1231% +0.1406%] (p = 0.42 > 0.05)
                        thrpt:  [−0.1404% +0.1233% +0.4302%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
event/json_creation     time:   [169.19 ns 170.31 ns 171.66 ns]
                        thrpt:  [5.8254 Melem/s 5.8715 Melem/s 5.9104 Melem/s]
                 change:
                        time:   [−1.8422% −0.7885% +0.2892%] (p = 0.17 > 0.05)
                        thrpt:  [−0.2884% +0.7947% +1.8767%]
                        No change in performance detected.

batch/pop_batch_steady_state/100
                        time:   [3.8111 µs 3.8150 µs 3.8194 µs]
                        thrpt:  [26.182 Melem/s 26.212 Melem/s 26.239 Melem/s]
                 change:
                        time:   [−0.9790% −0.5417% −0.1872%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1875% +0.5446% +0.9887%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  9 (9.00%) high mild
  5 (5.00%) high severe
batch/pop_batch_steady_state/1000
                        time:   [38.002 µs 38.021 µs 38.049 µs]
                        thrpt:  [26.282 Melem/s 26.301 Melem/s 26.315 Melem/s]
                 change:
                        time:   [−0.5071% −0.2403% −0.0288%] (p = 0.04 < 0.05)
                        thrpt:  [+0.0289% +0.2409% +0.5097%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
batch/pop_batch_steady_state/10000
                        time:   [383.09 µs 383.30 µs 383.54 µs]
                        thrpt:  [26.073 Melem/s 26.089 Melem/s 26.103 Melem/s]
                 change:
                        time:   [−0.3423% −0.0559% +0.1929%] (p = 0.71 > 0.05)
                        thrpt:  [−0.1925% +0.0559% +0.3434%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe

     Running benches/mesh.rs (target/release/deps/mesh-a354eb7fefbdd982)
Gnuplot not found, using plotters backend
mesh_reroute/triangle_failure
                        time:   [7.1222 µs 7.2101 µs 7.3001 µs]
                        thrpt:  [136.98 Kelem/s 138.69 Kelem/s 140.41 Kelem/s]
                 change:
                        time:   [−0.6240% +0.8026% +2.2160%] (p = 0.27 > 0.05)
                        thrpt:  [−2.1679% −0.7962% +0.6279%]
                        No change in performance detected.
mesh_reroute/10_peers_10_routes
                        time:   [40.011 µs 40.287 µs 40.585 µs]
                        thrpt:  [24.640 Kelem/s 24.822 Kelem/s 24.993 Kelem/s]
                 change:
                        time:   [−1.1332% −0.2181% +0.7231%] (p = 0.65 > 0.05)
                        thrpt:  [−0.7180% +0.2185% +1.1462%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
mesh_reroute/50_peers_100_routes
                        time:   [417.43 µs 419.84 µs 422.71 µs]
                        thrpt:  [2.3657 Kelem/s 2.3818 Kelem/s 2.3956 Kelem/s]
                 change:
                        time:   [−0.7632% −0.2767% +0.2097%] (p = 0.27 > 0.05)
                        thrpt:  [−0.2092% +0.2774% +0.7691%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

mesh_proximity/on_pingwave_new
                        time:   [177.22 ns 182.94 ns 189.52 ns]
                        thrpt:  [5.2765 Melem/s 5.4664 Melem/s 5.6429 Melem/s]
                 change:
                        time:   [−0.2771% +4.2615% +9.0940%] (p = 0.08 > 0.05)
                        thrpt:  [−8.3359% −4.0873% +0.2778%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
mesh_proximity/on_pingwave_dedup
                        time:   [69.630 ns 69.664 ns 69.705 ns]
                        thrpt:  [14.346 Melem/s 14.355 Melem/s 14.362 Melem/s]
                 change:
                        time:   [−0.1763% +0.0428% +0.3306%] (p = 0.75 > 0.05)
                        thrpt:  [−0.3296% −0.0428% +0.1766%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
mesh_proximity/pingwave_serialize
                        time:   [1.9685 ns 1.9695 ns 1.9708 ns]
                        thrpt:  [507.42 Melem/s 507.75 Melem/s 507.99 Melem/s]
                 change:
                        time:   [−0.2789% −0.0573% +0.1398%] (p = 0.61 > 0.05)
                        thrpt:  [−0.1397% +0.0574% +0.2797%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
mesh_proximity/pingwave_deserialize
                        time:   [2.2117 ns 2.2144 ns 2.2172 ns]
                        thrpt:  [451.03 Melem/s 451.59 Melem/s 452.13 Melem/s]
                 change:
                        time:   [−1.0129% −0.7666% −0.4547%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4568% +0.7725% +1.0233%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
mesh_proximity/node_count
                        time:   [199.42 ns 199.58 ns 199.78 ns]
                        thrpt:  [5.0055 Melem/s 5.0104 Melem/s 5.0144 Melem/s]
                 change:
                        time:   [+0.0604% +0.3787% +0.7683%] (p = 0.04 < 0.05)
                        thrpt:  [−0.7624% −0.3773% −0.0604%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe
mesh_proximity/all_nodes_100
                        time:   [4.7055 µs 4.7183 µs 4.7320 µs]
                        thrpt:  [211.33 Kelem/s 211.94 Kelem/s 212.52 Kelem/s]
                 change:
                        time:   [−2.5190% −2.1461% −1.7951%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8280% +2.1931% +2.5841%]
                        Performance has improved.

mesh_dispatch/classify_direct
                        time:   [620.96 ps 622.11 ps 623.60 ps]
                        thrpt:  [1.6036 Gelem/s 1.6074 Gelem/s 1.6104 Gelem/s]
                 change:
                        time:   [−2.7629% −2.4939% −2.1765%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2249% +2.5577% +2.8414%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) high mild
  12 (12.00%) high severe
mesh_dispatch/classify_routed
                        time:   [442.11 ps 442.28 ps 442.50 ps]
                        thrpt:  [2.2599 Gelem/s 2.2610 Gelem/s 2.2619 Gelem/s]
                 change:
                        time:   [−0.7465% −0.4480% −0.1653%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1656% +0.4500% +0.7521%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe
mesh_dispatch/classify_pingwave
                        time:   [310.40 ps 310.61 ps 310.87 ps]
                        thrpt:  [3.2168 Gelem/s 3.2194 Gelem/s 3.2216 Gelem/s]
                 change:
                        time:   [−0.4816% −0.1776% +0.2061%] (p = 0.32 > 0.05)
                        thrpt:  [−0.2056% +0.1779% +0.4840%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe

mesh_routing/lookup_hit time:   [15.171 ns 15.213 ns 15.257 ns]
                        thrpt:  [65.543 Melem/s 65.732 Melem/s 65.914 Melem/s]
                 change:
                        time:   [−0.2804% +0.5058% +1.2951%] (p = 0.23 > 0.05)
                        thrpt:  [−1.2785% −0.5032% +0.2812%]
                        No change in performance detected.
Found 27 outliers among 100 measurements (27.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
  11 (11.00%) high mild
  4 (4.00%) high severe
mesh_routing/lookup_miss
                        time:   [15.113 ns 15.178 ns 15.254 ns]
                        thrpt:  [65.558 Melem/s 65.885 Melem/s 66.168 Melem/s]
                 change:
                        time:   [−0.6136% −0.0703% +0.4751%] (p = 0.80 > 0.05)
                        thrpt:  [−0.4728% +0.0704% +0.6174%]
                        No change in performance detected.
Found 18 outliers among 100 measurements (18.00%)
  7 (7.00%) low severe
  6 (6.00%) high mild
  5 (5.00%) high severe
mesh_routing/is_local   time:   [310.85 ps 311.20 ps 311.60 ps]
                        thrpt:  [3.2092 Gelem/s 3.2134 Gelem/s 3.2170 Gelem/s]
                 change:
                        time:   [−0.3154% +0.0552% +0.5032%] (p = 0.80 > 0.05)
                        thrpt:  [−0.5007% −0.0552% +0.3164%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe
mesh_routing/all_routes/10
                        time:   [1.7588 µs 1.7626 µs 1.7672 µs]
                        thrpt:  [565.88 Kelem/s 567.34 Kelem/s 568.57 Kelem/s]
                 change:
                        time:   [−1.4273% −1.0629% −0.6564%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6607% +1.0743% +1.4479%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
mesh_routing/all_routes/100
                        time:   [2.6592 µs 2.6714 µs 2.6840 µs]
                        thrpt:  [372.58 Kelem/s 374.34 Kelem/s 376.05 Kelem/s]
                 change:
                        time:   [−1.3418% −0.8329% −0.3586%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3599% +0.8398% +1.3600%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
mesh_routing/all_routes/1000
                        time:   [12.787 µs 12.852 µs 12.910 µs]
                        thrpt:  [77.458 Kelem/s 77.807 Kelem/s 78.207 Kelem/s]
                 change:
                        time:   [+0.3186% +0.9979% +1.6857%] (p = 0.01 < 0.05)
                        thrpt:  [−1.6577% −0.9881% −0.3176%]
                        Change within noise threshold.
mesh_routing/add_route  time:   [44.262 ns 44.801 ns 45.270 ns]
                        thrpt:  [22.090 Melem/s 22.321 Melem/s 22.593 Melem/s]
                 change:
                        time:   [−4.2418% −1.4160% +1.3866%] (p = 0.32 > 0.05)
                        thrpt:  [−1.3677% +1.4363% +4.4297%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) low severe
  5 (5.00%) low mild

     Running benches/net.rs (target/release/deps/net-6a97487655698bef)
Gnuplot not found, using plotters backend
net_header/serialize    time:   [2.1911 ns 2.1932 ns 2.1960 ns]
                        thrpt:  [455.37 Melem/s 455.95 Melem/s 456.39 Melem/s]
                 change:
                        time:   [−0.0470% +0.0793% +0.2172%] (p = 0.25 > 0.05)
                        thrpt:  [−0.2167% −0.0793% +0.0471%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  8 (8.00%) high mild
  9 (9.00%) high severe
net_header/deserialize  time:   [2.3484 ns 2.3490 ns 2.3497 ns]
                        thrpt:  [425.58 Melem/s 425.72 Melem/s 425.83 Melem/s]
                 change:
                        time:   [−0.2515% −0.0644% +0.0842%] (p = 0.50 > 0.05)
                        thrpt:  [−0.0841% +0.0645% +0.2522%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
net_header/roundtrip    time:   [2.3486 ns 2.3506 ns 2.3538 ns]
                        thrpt:  [424.85 Melem/s 425.43 Melem/s 425.79 Melem/s]
                 change:
                        time:   [−0.0732% +0.0762% +0.2409%] (p = 0.39 > 0.05)
                        thrpt:  [−0.2403% −0.0762% +0.0733%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe

net_event_frame/write_single/64
                        time:   [21.438 ns 21.447 ns 21.459 ns]
                        thrpt:  [2.7776 GiB/s 2.7792 GiB/s 2.7803 GiB/s]
                 change:
                        time:   [−1.4348% −0.9476% −0.4691%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4713% +0.9567% +1.4557%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
net_event_frame/write_single_reused/64
                        time:   [2.5273 ns 2.5278 ns 2.5286 ns]
                        thrpt:  [23.573 GiB/s 23.579 GiB/s 23.585 GiB/s]
                 change:
                        time:   [−0.1843% +0.0019% +0.1853%] (p = 0.99 > 0.05)
                        thrpt:  [−0.1850% −0.0019% +0.1847%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  11 (11.00%) high severe
net_event_frame/write_single/256
                        time:   [46.426 ns 47.207 ns 47.960 ns]
                        thrpt:  [4.9712 GiB/s 5.0505 GiB/s 5.1354 GiB/s]
                 change:
                        time:   [−2.8628% −1.5124% −0.0805%] (p = 0.04 < 0.05)
                        thrpt:  [+0.0806% +1.5356% +2.9471%]
                        Change within noise threshold.
net_event_frame/write_single_reused/256
                        time:   [5.2789 ns 5.2823 ns 5.2866 ns]
                        thrpt:  [45.099 GiB/s 45.135 GiB/s 45.165 GiB/s]
                 change:
                        time:   [−0.0838% +0.0607% +0.2286%] (p = 0.49 > 0.05)
                        thrpt:  [−0.2281% −0.0607% +0.0839%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  10 (10.00%) high mild
  6 (6.00%) high severe
net_event_frame/write_single/1024
                        time:   [33.860 ns 33.872 ns 33.887 ns]
                        thrpt:  [28.143 GiB/s 28.155 GiB/s 28.166 GiB/s]
                 change:
                        time:   [−0.5438% −0.3405% −0.1266%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1268% +0.3417% +0.5468%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
net_event_frame/write_single_reused/1024
                        time:   [14.574 ns 14.591 ns 14.614 ns]
                        thrpt:  [65.259 GiB/s 65.358 GiB/s 65.436 GiB/s]
                 change:
                        time:   [−0.8688% −0.6839% −0.4855%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4879% +0.6886% +0.8764%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
net_event_frame/write_single/4096
                        time:   [73.746 ns 74.190 ns 74.714 ns]
                        thrpt:  [51.057 GiB/s 51.418 GiB/s 51.727 GiB/s]
                 change:
                        time:   [−3.2221% −1.8322% −0.3237%] (p = 0.01 < 0.05)
                        thrpt:  [+0.3247% +1.8664% +3.3294%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe
net_event_frame/write_single_reused/4096
                        time:   [53.494 ns 53.577 ns 53.661 ns]
                        thrpt:  [71.089 GiB/s 71.200 GiB/s 71.310 GiB/s]
                 change:
                        time:   [−1.2781% −0.3118% +0.5271%] (p = 0.54 > 0.05)
                        thrpt:  [−0.5244% +0.3128% +1.2947%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
net_event_frame/write_batch/1
                        time:   [21.499 ns 21.506 ns 21.515 ns]
                        thrpt:  [2.7704 GiB/s 2.7716 GiB/s 2.7724 GiB/s]
                 change:
                        time:   [−0.7467% −0.5176% −0.2775%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2783% +0.5203% +0.7524%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
net_event_frame/write_batch/10
                        time:   [68.217 ns 68.354 ns 68.481 ns]
                        thrpt:  [8.7038 GiB/s 8.7200 GiB/s 8.7376 GiB/s]
                 change:
                        time:   [−0.4126% −0.1037% +0.2183%] (p = 0.52 > 0.05)
                        thrpt:  [−0.2179% +0.1039% +0.4143%]
                        No change in performance detected.
Found 22 outliers among 100 measurements (22.00%)
  10 (10.00%) low severe
  8 (8.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe
net_event_frame/write_batch/50
                        time:   [145.38 ns 145.43 ns 145.50 ns]
                        thrpt:  [20.483 GiB/s 20.492 GiB/s 20.499 GiB/s]
                 change:
                        time:   [−0.3316% −0.1328% +0.0810%] (p = 0.21 > 0.05)
                        thrpt:  [−0.0809% +0.1330% +0.3327%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
net_event_frame/write_batch/100
                        time:   [269.88 ns 270.16 ns 270.52 ns]
                        thrpt:  [22.033 GiB/s 22.063 GiB/s 22.085 GiB/s]
                 change:
                        time:   [−0.1861% +0.0032% +0.1999%] (p = 0.98 > 0.05)
                        thrpt:  [−0.1995% −0.0032% +0.1865%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe
net_event_frame/read_batch_10
                        time:   [131.96 ns 133.14 ns 134.49 ns]
                        thrpt:  [74.356 Melem/s 75.107 Melem/s 75.779 Melem/s]
                 change:
                        time:   [−4.1786% −3.2795% −2.4273%] (p = 0.00 < 0.05)
                        thrpt:  [+2.4877% +3.3907% +4.3608%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

net_packet_pool/get_return/16
                        time:   [51.130 ns 51.281 ns 51.416 ns]
                        thrpt:  [19.449 Melem/s 19.500 Melem/s 19.558 Melem/s]
                 change:
                        time:   [+0.9011% +1.2492% +1.5922%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5672% −1.2338% −0.8930%]
                        Change within noise threshold.
net_packet_pool/get_return/64
                        time:   [51.452 ns 51.535 ns 51.622 ns]
                        thrpt:  [19.371 Melem/s 19.404 Melem/s 19.436 Melem/s]
                 change:
                        time:   [+2.6115% +2.8321% +3.0406%] (p = 0.00 < 0.05)
                        thrpt:  [−2.9509% −2.7541% −2.5450%]
                        Performance has regressed.
net_packet_pool/get_return/256
                        time:   [51.531 ns 51.617 ns 51.707 ns]
                        thrpt:  [19.340 Melem/s 19.373 Melem/s 19.406 Melem/s]
                 change:
                        time:   [+2.5761% +2.8392% +3.1015%] (p = 0.00 < 0.05)
                        thrpt:  [−3.0082% −2.7608% −2.5114%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild

net_packet_build/build_packet/1
                        time:   [500.50 ns 502.36 ns 504.69 ns]
                        thrpt:  [120.94 MiB/s 121.50 MiB/s 121.95 MiB/s]
                 change:
                        time:   [−0.5601% +0.0301% +0.5822%] (p = 0.92 > 0.05)
                        thrpt:  [−0.5789% −0.0301% +0.5633%]
                        No change in performance detected.
net_packet_build/build_packet/10
                        time:   [1.8513 µs 1.8547 µs 1.8602 µs]
                        thrpt:  [328.11 MiB/s 329.08 MiB/s 329.69 MiB/s]
                 change:
                        time:   [−0.3646% −0.0118% +0.3166%] (p = 0.95 > 0.05)
                        thrpt:  [−0.3156% +0.0118% +0.3659%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
net_packet_build/build_packet/50
                        time:   [8.1573 µs 8.1605 µs 8.1651 µs]
                        thrpt:  [373.76 MiB/s 373.96 MiB/s 374.11 MiB/s]
                 change:
                        time:   [−0.2587% +0.0055% +0.2697%] (p = 0.97 > 0.05)
                        thrpt:  [−0.2690% −0.0055% +0.2594%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

net_encryption/encrypt/64
                        time:   [501.90 ns 504.28 ns 507.22 ns]
                        thrpt:  [120.33 MiB/s 121.03 MiB/s 121.61 MiB/s]
                 change:
                        time:   [−0.9106% −0.3283% +0.2348%] (p = 0.25 > 0.05)
                        thrpt:  [−0.2342% +0.3294% +0.9190%]
                        No change in performance detected.
net_encryption/encrypt/256
                        time:   [934.73 ns 936.48 ns 938.64 ns]
                        thrpt:  [260.10 MiB/s 260.70 MiB/s 261.19 MiB/s]
                 change:
                        time:   [−0.4593% −0.0284% +0.3945%] (p = 0.90 > 0.05)
                        thrpt:  [−0.3929% +0.0285% +0.4614%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
net_encryption/encrypt/1024
                        time:   [2.6983 µs 2.7069 µs 2.7171 µs]
                        thrpt:  [359.41 MiB/s 360.77 MiB/s 361.92 MiB/s]
                 change:
                        time:   [+0.0528% +0.3542% +0.6723%] (p = 0.02 < 0.05)
                        thrpt:  [−0.6678% −0.3530% −0.0528%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
net_encryption/encrypt/4096
                        time:   [9.7349 µs 9.7460 µs 9.7605 µs]
                        thrpt:  [400.21 MiB/s 400.80 MiB/s 401.26 MiB/s]
                 change:
                        time:   [−0.0227% +0.1991% +0.4313%] (p = 0.09 > 0.05)
                        thrpt:  [−0.4295% −0.1987% +0.0227%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe

net_keypair/generate    time:   [12.423 µs 12.442 µs 12.469 µs]
                        thrpt:  [80.200 Kelem/s 80.372 Kelem/s 80.498 Kelem/s]
                 change:
                        time:   [−0.2747% −0.0704% +0.1106%] (p = 0.52 > 0.05)
                        thrpt:  [−0.1105% +0.0704% +0.2755%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe

net_aad/generate        time:   [1.8647 ns 1.8704 ns 1.8775 ns]
                        thrpt:  [532.62 Melem/s 534.65 Melem/s 536.28 Melem/s]
                 change:
                        time:   [−2.6611% −2.4125% −2.1142%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1598% +2.4722% +2.7339%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  14 (14.00%) high severe

pool_comparison/shared_pool_get_return
                        time:   [64.715 ns 66.477 ns 68.572 ns]
                        thrpt:  [14.583 Melem/s 15.043 Melem/s 15.452 Melem/s]
                 change:
                        time:   [−8.3516% −3.2693% +2.2610%] (p = 0.23 > 0.05)
                        thrpt:  [−2.2110% +3.3798% +9.1126%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild
pool_comparison/thread_local_pool_get_return
                        time:   [96.681 ns 96.786 ns 96.887 ns]
                        thrpt:  [10.321 Melem/s 10.332 Melem/s 10.343 Melem/s]
                 change:
                        time:   [−2.0549% −1.6555% −1.2443%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2599% +1.6833% +2.0980%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
pool_comparison/shared_pool_10x
                        time:   [470.21 ns 470.32 ns 470.43 ns]
                        thrpt:  [2.1257 Melem/s 2.1262 Melem/s 2.1267 Melem/s]
                 change:
                        time:   [−0.2450% −0.0518% +0.1092%] (p = 0.60 > 0.05)
                        thrpt:  [−0.1090% +0.0518% +0.2456%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
pool_comparison/thread_local_pool_10x
                        time:   [1.2674 µs 1.2883 µs 1.3120 µs]
                        thrpt:  [762.18 Kelem/s 776.23 Kelem/s 789.02 Kelem/s]
                 change:
                        time:   [−1.9114% +0.0088% +2.0894%] (p = 0.99 > 0.05)
                        thrpt:  [−2.0467% −0.0088% +1.9487%]
                        No change in performance detected.

cipher_comparison/shared_pool/64
                        time:   [500.31 ns 502.06 ns 504.25 ns]
                        thrpt:  [121.04 MiB/s 121.57 MiB/s 122.00 MiB/s]
                 change:
                        time:   [−0.4716% +0.1165% +0.6962%] (p = 0.70 > 0.05)
                        thrpt:  [−0.6914% −0.1164% +0.4738%]
                        No change in performance detected.
cipher_comparison/fast_chacha20/64
                        time:   [563.39 ns 566.34 ns 568.94 ns]
                        thrpt:  [107.28 MiB/s 107.77 MiB/s 108.33 MiB/s]
                 change:
                        time:   [+1.3490% +1.8541% +2.3999%] (p = 0.00 < 0.05)
                        thrpt:  [−2.3436% −1.8204% −1.3311%]
                        Performance has regressed.
cipher_comparison/shared_pool/256
                        time:   [934.90 ns 936.77 ns 939.14 ns]
                        thrpt:  [259.96 MiB/s 260.62 MiB/s 261.14 MiB/s]
                 change:
                        time:   [−0.5419% −0.1269% +0.2520%] (p = 0.55 > 0.05)
                        thrpt:  [−0.2513% +0.1271% +0.5448%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
cipher_comparison/fast_chacha20/256
                        time:   [994.89 ns 997.77 ns 1.0003 µs]
                        thrpt:  [244.07 MiB/s 244.69 MiB/s 245.39 MiB/s]
                 change:
                        time:   [−0.6357% −0.2655% +0.0572%] (p = 0.14 > 0.05)
                        thrpt:  [−0.0572% +0.2662% +0.6398%]
                        No change in performance detected.
cipher_comparison/shared_pool/1024
                        time:   [2.6915 µs 2.6934 µs 2.6957 µs]
                        thrpt:  [362.26 MiB/s 362.58 MiB/s 362.83 MiB/s]
                 change:
                        time:   [−0.2955% +0.0135% +0.3877%] (p = 0.94 > 0.05)
                        thrpt:  [−0.3862% −0.0135% +0.2963%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/1024
                        time:   [2.7400 µs 2.7433 µs 2.7464 µs]
                        thrpt:  [355.58 MiB/s 355.98 MiB/s 356.41 MiB/s]
                 change:
                        time:   [−0.2847% −0.0929% +0.0788%] (p = 0.32 > 0.05)
                        thrpt:  [−0.0787% +0.0930% +0.2855%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
cipher_comparison/shared_pool/4096
                        time:   [9.7176 µs 9.7242 µs 9.7329 µs]
                        thrpt:  [401.35 MiB/s 401.70 MiB/s 401.98 MiB/s]
                 change:
                        time:   [−0.1041% +0.1128% +0.3366%] (p = 0.33 > 0.05)
                        thrpt:  [−0.3355% −0.1127% +0.1042%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
cipher_comparison/fast_chacha20/4096
                        time:   [9.7631 µs 9.7759 µs 9.7891 µs]
                        thrpt:  [399.04 MiB/s 399.58 MiB/s 400.10 MiB/s]
                 change:
                        time:   [−0.0128% +0.1865% +0.3973%] (p = 0.08 > 0.05)
                        thrpt:  [−0.3958% −0.1861% +0.0128%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

adaptive_batcher/optimal_size
                        time:   [969.96 ps 970.35 ps 970.96 ps]
                        thrpt:  [1.0299 Gelem/s 1.0306 Gelem/s 1.0310 Gelem/s]
                 change:
                        time:   [+0.0436% +0.4118% +0.7693%] (p = 0.04 < 0.05)
                        thrpt:  [−0.7634% −0.4101% −0.0436%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe
adaptive_batcher/record time:   [3.8578 ns 3.8585 ns 3.8592 ns]
                        thrpt:  [259.12 Melem/s 259.17 Melem/s 259.22 Melem/s]
                 change:
                        time:   [−0.9119% −0.5501% −0.1997%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2001% +0.5532% +0.9203%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) high severe
adaptive_batcher/full_cycle
                        time:   [4.3688 ns 4.3700 ns 4.3713 ns]
                        thrpt:  [228.76 Melem/s 228.83 Melem/s 228.90 Melem/s]
                 change:
                        time:   [−0.2047% +0.0224% +0.2530%] (p = 0.85 > 0.05)
                        thrpt:  [−0.2523% −0.0224% +0.2051%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe

e2e_packet_build/shared_pool_50_events
                        time:   [8.1667 µs 8.1693 µs 8.1725 µs]
                        thrpt:  [373.42 MiB/s 373.57 MiB/s 373.68 MiB/s]
                 change:
                        time:   [−0.6478% −0.3509% −0.0724%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0725% +0.3521% +0.6520%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
e2e_packet_build/fast_50_events
                        time:   [8.1948 µs 8.2033 µs 8.2137 µs]
                        thrpt:  [371.54 MiB/s 372.02 MiB/s 372.40 MiB/s]
                 change:
                        time:   [−0.1794% +0.0112% +0.1822%] (p = 0.91 > 0.05)
                        thrpt:  [−0.1819% −0.0112% +0.1797%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.4s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/shared_pool/8
                        time:   [1.8463 ms 1.8491 ms 1.8530 ms]
                        thrpt:  [4.3174 Melem/s 4.3265 Melem/s 4.3331 Melem/s]
                 change:
                        time:   [−1.0510% −0.5198% −0.0095%] (p = 0.05 < 0.05)
                        thrpt:  [+0.0095% +0.5225% +1.0621%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
multithread_packet_build/thread_local_pool/8
                        time:   [891.97 µs 906.48 µs 924.83 µs]
                        thrpt:  [8.6502 Melem/s 8.8253 Melem/s 8.9689 Melem/s]
                 change:
                        time:   [+0.9717% +2.5161% +4.3772%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1936% −2.4543% −0.9623%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) high severe
multithread_packet_build/shared_pool/16
                        time:   [4.3296 ms 4.4327 ms 4.5439 ms]
                        thrpt:  [3.5212 Melem/s 3.6096 Melem/s 3.6955 Melem/s]
                 change:
                        time:   [−9.6428% −7.0504% −4.2998%] (p = 0.00 < 0.05)
                        thrpt:  [+4.4930% +7.5852% +10.672%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.7s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/16
                        time:   [1.7137 ms 1.7177 ms 1.7233 ms]
                        thrpt:  [9.2847 Melem/s 9.3148 Melem/s 9.3365 Melem/s]
                 change:
                        time:   [−0.5638% −0.0239% +0.6200%] (p = 0.94 > 0.05)
                        thrpt:  [−0.6161% +0.0239% +0.5670%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
multithread_packet_build/shared_pool/24
                        time:   [6.6338 ms 6.8093 ms 6.9965 ms]
                        thrpt:  [3.4303 Melem/s 3.5246 Melem/s 3.6178 Melem/s]
                 change:
                        time:   [−10.595% −7.3142% −3.8922%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0498% +7.8914% +11.851%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
multithread_packet_build/thread_local_pool/24
                        time:   [2.5327 ms 2.5389 ms 2.5460 ms]
                        thrpt:  [9.4267 Melem/s 9.4528 Melem/s 9.4762 Melem/s]
                 change:
                        time:   [−0.3397% +0.1825% +0.6748%] (p = 0.50 > 0.05)
                        thrpt:  [−0.6703% −0.1822% +0.3409%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe
multithread_packet_build/shared_pool/32
                        time:   [9.8481 ms 10.214 ms 10.599 ms]
                        thrpt:  [3.0192 Melem/s 3.1330 Melem/s 3.2494 Melem/s]
                 change:
                        time:   [−6.7765% −1.6980% +3.4273%] (p = 0.51 > 0.05)
                        thrpt:  [−3.3137% +1.7274% +7.2691%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
multithread_packet_build/thread_local_pool/32
                        time:   [3.3303 ms 3.3677 ms 3.4110 ms]
                        thrpt:  [9.3815 Melem/s 9.5020 Melem/s 9.6089 Melem/s]
                 change:
                        time:   [+1.2477% +2.4461% +3.7837%] (p = 0.00 < 0.05)
                        thrpt:  [−3.6457% −2.3877% −1.2323%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) high mild
  11 (11.00%) high severe

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/shared_mixed/8
                        time:   [1.4160 ms 1.4184 ms 1.4218 ms]
                        thrpt:  [8.4402 Melem/s 8.4604 Melem/s 8.4745 Melem/s]
                 change:
                        time:   [−0.2378% +0.6466% +1.6165%] (p = 0.20 > 0.05)
                        thrpt:  [−1.5908% −0.6425% +0.2383%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low mild
  7 (7.00%) high mild
  5 (5.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.4s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/fast_mixed/8
                        time:   [1.0599 ms 1.0622 ms 1.0659 ms]
                        thrpt:  [11.258 Melem/s 11.298 Melem/s 11.322 Melem/s]
                 change:
                        time:   [+0.1162% +1.0779% +2.0314%] (p = 0.02 < 0.05)
                        thrpt:  [−1.9909% −1.0664% −0.1161%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
multithread_mixed_frames/shared_mixed/16
                        time:   [3.0344 ms 3.0795 ms 3.1288 ms]
                        thrpt:  [7.6706 Melem/s 7.7935 Melem/s 7.9094 Melem/s]
                 change:
                        time:   [−3.4365% −0.9614% +1.4470%] (p = 0.45 > 0.05)
                        thrpt:  [−1.4263% +0.9708% +3.5588%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
multithread_mixed_frames/fast_mixed/16
                        time:   [2.0650 ms 2.0697 ms 2.0757 ms]
                        thrpt:  [11.562 Melem/s 11.596 Melem/s 11.622 Melem/s]
                 change:
                        time:   [+0.2219% +0.5084% +0.8298%] (p = 0.00 < 0.05)
                        thrpt:  [−0.8230% −0.5058% −0.2214%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  4 (4.00%) high severe
multithread_mixed_frames/shared_mixed/24
                        time:   [4.6004 ms 4.6999 ms 4.8063 ms]
                        thrpt:  [7.4902 Melem/s 7.6597 Melem/s 7.8254 Melem/s]
                 change:
                        time:   [−1.7909% +1.3574% +4.6007%] (p = 0.41 > 0.05)
                        thrpt:  [−4.3984% −1.3393% +1.8236%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
multithread_mixed_frames/fast_mixed/24
                        time:   [3.0370 ms 3.0405 ms 3.0447 ms]
                        thrpt:  [11.824 Melem/s 11.840 Melem/s 11.854 Melem/s]
                 change:
                        time:   [−0.0641% +0.1451% +0.3446%] (p = 0.18 > 0.05)
                        thrpt:  [−0.3434% −0.1449% +0.0641%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
multithread_mixed_frames/shared_mixed/32
                        time:   [6.3345 ms 6.5285 ms 6.7354 ms]
                        thrpt:  [7.1265 Melem/s 7.3524 Melem/s 7.5775 Melem/s]
                 change:
                        time:   [−5.8573% −0.6460% +4.7226%] (p = 0.81 > 0.05)
                        thrpt:  [−4.5096% +0.6502% +6.2218%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
multithread_mixed_frames/fast_mixed/32
                        time:   [3.9940 ms 3.9987 ms 4.0044 ms]
                        thrpt:  [11.987 Melem/s 12.004 Melem/s 12.018 Melem/s]
                 change:
                        time:   [−0.1893% +0.0076% +0.2204%] (p = 0.94 > 0.05)
                        thrpt:  [−0.2199% −0.0076% +0.1896%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

pool_contention/shared_acquire_release/8
                        time:   [18.602 ms 18.636 ms 18.672 ms]
                        thrpt:  [4.2844 Melem/s 4.2928 Melem/s 4.3006 Melem/s]
                 change:
                        time:   [−1.6607% −1.3515% −1.0582%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0695% +1.3700% +1.6888%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  2 (2.00%) high severe
Benchmarking pool_contention/fast_acquire_release/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.5s, enable flat sampling, or reduce sample count to 60.
pool_contention/fast_acquire_release/8
                        time:   [1.2879 ms 1.2949 ms 1.3035 ms]
                        thrpt:  [61.373 Melem/s 61.781 Melem/s 62.118 Melem/s]
                 change:
                        time:   [−0.5075% +0.8283% +2.3378%] (p = 0.28 > 0.05)
                        thrpt:  [−2.2844% −0.8215% +0.5101%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
pool_contention/shared_acquire_release/16
                        time:   [39.211 ms 39.402 ms 39.600 ms]
                        thrpt:  [4.0404 Melem/s 4.0607 Melem/s 4.0804 Melem/s]
                 change:
                        time:   [−3.8743% −2.6898% −1.5105%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5336% +2.7641% +4.0304%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
pool_contention/fast_acquire_release/16
                        time:   [2.5121 ms 2.5227 ms 2.5336 ms]
                        thrpt:  [63.150 Melem/s 63.423 Melem/s 63.693 Melem/s]
                 change:
                        time:   [+0.2284% +0.8145% +1.3726%] (p = 0.01 < 0.05)
                        thrpt:  [−1.3540% −0.8079% −0.2278%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking pool_contention/shared_acquire_release/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, or reduce sample count to 80.
pool_contention/shared_acquire_release/24
                        time:   [58.188 ms 59.070 ms 59.983 ms]
                        thrpt:  [4.0012 Melem/s 4.0630 Melem/s 4.1246 Melem/s]
                 change:
                        time:   [−14.249% −11.454% −8.6581%] (p = 0.00 < 0.05)
                        thrpt:  [+9.4788% +12.936% +16.617%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
pool_contention/fast_acquire_release/24
                        time:   [3.6345 ms 3.6448 ms 3.6552 ms]
                        thrpt:  [65.659 Melem/s 65.847 Melem/s 66.034 Melem/s]
                 change:
                        time:   [−0.3775% +0.0264% +0.4390%] (p = 0.90 > 0.05)
                        thrpt:  [−0.4371% −0.0264% +0.3790%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking pool_contention/shared_acquire_release/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.3s, or reduce sample count to 50.
pool_contention/shared_acquire_release/32
                        time:   [82.351 ms 84.701 ms 87.336 ms]
                        thrpt:  [3.6640 Melem/s 3.7780 Melem/s 3.8858 Melem/s]
                 change:
                        time:   [−2.4096% +1.2991% +5.1849%] (p = 0.52 > 0.05)
                        thrpt:  [−4.9294% −1.2825% +2.4691%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
pool_contention/fast_acquire_release/32
                        time:   [4.7972 ms 4.8505 ms 4.9113 ms]
                        thrpt:  [65.156 Melem/s 65.973 Melem/s 66.706 Melem/s]
                 change:
                        time:   [+0.8673% +2.0015% +3.2031%] (p = 0.00 < 0.05)
                        thrpt:  [−3.1037% −1.9623% −0.8598%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe

throughput_scaling/fast_pool_scaling/1
                        time:   [6.7328 ms 6.7350 ms 6.7378 ms]
                        thrpt:  [296.83 Kelem/s 296.95 Kelem/s 297.05 Kelem/s]
                 change:
                        time:   [−0.6342% −0.1394% +0.2149%] (p = 0.62 > 0.05)
                        thrpt:  [−0.2144% +0.1396% +0.6382%]
                        No change in performance detected.
Found 6 outliers among 20 measurements (30.00%)
  1 (5.00%) low severe
  2 (10.00%) low mild
  3 (15.00%) high severe
throughput_scaling/fast_pool_scaling/2
                        time:   [6.9818 ms 6.9911 ms 7.0111 ms]
                        thrpt:  [570.52 Kelem/s 572.16 Kelem/s 572.92 Kelem/s]
                 change:
                        time:   [−0.2511% +0.1786% +0.5685%] (p = 0.45 > 0.05)
                        thrpt:  [−0.5653% −0.1783% +0.2517%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
throughput_scaling/fast_pool_scaling/4
                        time:   [7.3617 ms 7.3759 ms 7.3882 ms]
                        thrpt:  [1.0828 Melem/s 1.0846 Melem/s 1.0867 Melem/s]
                 change:
                        time:   [−0.1941% +0.2532% +0.7134%] (p = 0.28 > 0.05)
                        thrpt:  [−0.7083% −0.2526% +0.1945%]
                        No change in performance detected.
throughput_scaling/fast_pool_scaling/8
                        time:   [7.6324 ms 7.6512 ms 7.6789 ms]
                        thrpt:  [2.0836 Melem/s 2.0912 Melem/s 2.0963 Melem/s]
                 change:
                        time:   [−0.8258% −0.0459% +0.7271%] (p = 0.92 > 0.05)
                        thrpt:  [−0.7218% +0.0459% +0.8327%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
throughput_scaling/fast_pool_scaling/16
                        time:   [15.180 ms 15.235 ms 15.291 ms]
                        thrpt:  [2.0927 Melem/s 2.1004 Melem/s 2.1080 Melem/s]
                 change:
                        time:   [−0.8235% +0.0639% +0.7448%] (p = 0.90 > 0.05)
                        thrpt:  [−0.7393% −0.0639% +0.8304%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/24
                        time:   [22.542 ms 22.563 ms 22.582 ms]
                        thrpt:  [2.1256 Melem/s 2.1274 Melem/s 2.1294 Melem/s]
                 change:
                        time:   [−1.5583% −0.2787% +0.5674%] (p = 0.74 > 0.05)
                        thrpt:  [−0.5642% +0.2795% +1.5830%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking throughput_scaling/fast_pool_scaling/32: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 10.
throughput_scaling/fast_pool_scaling/32
                        time:   [29.675 ms 29.736 ms 29.787 ms]
                        thrpt:  [2.1486 Melem/s 2.1523 Melem/s 2.1567 Melem/s]
                 change:
                        time:   [−1.0132% −0.3196% +0.3376%] (p = 0.38 > 0.05)
                        thrpt:  [−0.3365% +0.3206% +1.0235%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

routing_header/serialize
                        time:   [623.10 ps 624.08 ps 625.25 ps]
                        thrpt:  [1.5994 Gelem/s 1.6024 Gelem/s 1.6049 Gelem/s]
                 change:
                        time:   [−0.8120% −0.2492% +0.1879%] (p = 0.37 > 0.05)
                        thrpt:  [−0.1875% +0.2498% +0.8186%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
routing_header/deserialize
                        time:   [931.30 ps 933.15 ps 936.02 ps]
                        thrpt:  [1.0684 Gelem/s 1.0716 Gelem/s 1.0738 Gelem/s]
                 change:
                        time:   [−0.1204% +0.1172% +0.3289%] (p = 0.33 > 0.05)
                        thrpt:  [−0.3278% −0.1171% +0.1205%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) high mild
  12 (12.00%) high severe
routing_header/roundtrip
                        time:   [931.20 ps 932.39 ps 934.67 ps]
                        thrpt:  [1.0699 Gelem/s 1.0725 Gelem/s 1.0739 Gelem/s]
                 change:
                        time:   [−0.3840% −0.0752% +0.2350%] (p = 0.64 > 0.05)
                        thrpt:  [−0.2344% +0.0753% +0.3855%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
routing_header/forward  time:   [569.73 ps 571.68 ps 573.90 ps]
                        thrpt:  [1.7425 Gelem/s 1.7492 Gelem/s 1.7552 Gelem/s]
                 change:
                        time:   [−0.5977% −0.1651% +0.2285%] (p = 0.44 > 0.05)
                        thrpt:  [−0.2280% +0.1654% +0.6013%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

routing_table/lookup_hit
                        time:   [38.603 ns 39.634 ns 40.710 ns]
                        thrpt:  [24.564 Melem/s 25.231 Melem/s 25.905 Melem/s]
                 change:
                        time:   [+2.1998% +5.0050% +8.0870%] (p = 0.00 < 0.05)
                        thrpt:  [−7.4819% −4.7664% −2.1524%]
                        Performance has regressed.
routing_table/lookup_miss
                        time:   [15.043 ns 15.085 ns 15.153 ns]
                        thrpt:  [65.995 Melem/s 66.292 Melem/s 66.476 Melem/s]
                 change:
                        time:   [+0.1010% +0.7415% +1.5163%] (p = 0.04 < 0.05)
                        thrpt:  [−1.4937% −0.7360% −0.1009%]
                        Change within noise threshold.
Found 30 outliers among 100 measurements (30.00%)
  6 (6.00%) low severe
  5 (5.00%) low mild
  19 (19.00%) high severe
routing_table/is_local  time:   [317.46 ps 318.35 ps 319.09 ps]
                        thrpt:  [3.1339 Gelem/s 3.1412 Gelem/s 3.1500 Gelem/s]
                 change:
                        time:   [+0.2783% +0.7266% +1.2160%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2014% −0.7214% −0.2775%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
routing_table/add_route time:   [45.357 ns 46.112 ns 46.793 ns]
                        thrpt:  [21.371 Melem/s 21.686 Melem/s 22.048 Melem/s]
                 change:
                        time:   [+0.6516% +3.7426% +6.9403%] (p = 0.02 < 0.05)
                        thrpt:  [−6.4899% −3.6076% −0.6473%]
                        Change within noise threshold.
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) low severe
  10 (10.00%) low mild
routing_table/record_in time:   [55.283 ns 55.982 ns 56.900 ns]
                        thrpt:  [17.575 Melem/s 17.863 Melem/s 18.089 Melem/s]
                 change:
                        time:   [−8.6160% −7.6043% −6.4048%] (p = 0.00 < 0.05)
                        thrpt:  [+6.8431% +8.2301% +9.4284%]
                        Performance has improved.
Found 27 outliers among 100 measurements (27.00%)
  18 (18.00%) low mild
  1 (1.00%) high mild
  8 (8.00%) high severe
routing_table/record_out
                        time:   [39.682 ns 39.822 ns 39.960 ns]
                        thrpt:  [25.025 Melem/s 25.112 Melem/s 25.200 Melem/s]
                 change:
                        time:   [−3.2411% −2.3847% −1.3261%] (p = 0.00 < 0.05)
                        thrpt:  [+1.3440% +2.4429% +3.3497%]
                        Performance has improved.
Found 20 outliers among 100 measurements (20.00%)
  9 (9.00%) low severe
  5 (5.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
routing_table/aggregate_stats
                        time:   [2.5576 µs 2.5596 µs 2.5620 µs]
                        thrpt:  [390.32 Kelem/s 390.69 Kelem/s 390.99 Kelem/s]
                 change:
                        time:   [−3.1725% −3.0181% −2.8549%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9388% +3.1120% +3.2764%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

fair_scheduler/creation time:   [278.61 ns 279.13 ns 279.77 ns]
                        thrpt:  [3.5744 Melem/s 3.5826 Melem/s 3.5892 Melem/s]
                 change:
                        time:   [−0.1221% +0.1398% +0.4150%] (p = 0.32 > 0.05)
                        thrpt:  [−0.4133% −0.1396% +0.1223%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
fair_scheduler/stream_count_empty
                        time:   [199.27 ns 199.31 ns 199.35 ns]
                        thrpt:  [5.0162 Melem/s 5.0174 Melem/s 5.0184 Melem/s]
                 change:
                        time:   [−0.8321% −0.5380% −0.2661%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2668% +0.5409% +0.8391%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
fair_scheduler/total_queued
                        time:   [310.90 ps 311.59 ps 312.72 ps]
                        thrpt:  [3.1978 Gelem/s 3.2094 Gelem/s 3.2165 Gelem/s]
                 change:
                        time:   [+0.3757% +0.6519% +0.9927%] (p = 0.00 < 0.05)
                        thrpt:  [−0.9830% −0.6477% −0.3742%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  12 (12.00%) high mild
  4 (4.00%) high severe
fair_scheduler/cleanup_empty
                        time:   [200.21 ns 200.38 ns 200.72 ns]
                        thrpt:  [4.9822 Melem/s 4.9904 Melem/s 4.9948 Melem/s]
                 change:
                        time:   [−0.0997% +0.1282% +0.3854%] (p = 0.39 > 0.05)
                        thrpt:  [−0.3839% −0.1280% +0.0998%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

routing_table_concurrent/concurrent_lookup/4
                        time:   [157.65 µs 160.73 µs 163.12 µs]
                        thrpt:  [24.522 Melem/s 24.886 Melem/s 25.372 Melem/s]
                 change:
                        time:   [−1.5249% +2.8336% +7.4984%] (p = 0.20 > 0.05)
                        thrpt:  [−6.9753% −2.7555% +1.5486%]
                        No change in performance detected.
routing_table_concurrent/concurrent_stats/4
                        time:   [360.08 µs 360.50 µs 360.87 µs]
                        thrpt:  [11.084 Melem/s 11.096 Melem/s 11.109 Melem/s]
                 change:
                        time:   [−0.4272% −0.1375% +0.1779%] (p = 0.39 > 0.05)
                        thrpt:  [−0.1776% +0.1377% +0.4290%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  6 (6.00%) low mild
  1 (1.00%) high severe
routing_table_concurrent/concurrent_lookup/8
                        time:   [246.25 µs 246.60 µs 247.01 µs]
                        thrpt:  [32.387 Melem/s 32.441 Melem/s 32.487 Melem/s]
                 change:
                        time:   [+0.3785% +0.8203% +1.2847%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2684% −0.8137% −0.3771%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
routing_table_concurrent/concurrent_stats/8
                        time:   [468.31 µs 468.77 µs 469.31 µs]
                        thrpt:  [17.046 Melem/s 17.066 Melem/s 17.083 Melem/s]
                 change:
                        time:   [−1.0732% −0.0903% +0.7400%] (p = 0.85 > 0.05)
                        thrpt:  [−0.7345% +0.0903% +1.0849%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  6 (6.00%) high severe
routing_table_concurrent/concurrent_lookup/16
                        time:   [413.91 µs 416.95 µs 419.43 µs]
                        thrpt:  [38.147 Melem/s 38.374 Melem/s 38.656 Melem/s]
                 change:
                        time:   [−0.2596% +0.8334% +1.9515%] (p = 0.14 > 0.05)
                        thrpt:  [−1.9142% −0.8265% +0.2603%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  8 (8.00%) high severe
routing_table_concurrent/concurrent_stats/16
                        time:   [907.30 µs 910.22 µs 914.19 µs]
                        thrpt:  [17.502 Melem/s 17.578 Melem/s 17.635 Melem/s]
                 change:
                        time:   [+1.1596% +2.9722% +5.0669%] (p = 0.00 < 0.05)
                        thrpt:  [−4.8225% −2.8864% −1.1463%]
                        Performance has regressed.
Found 18 outliers among 100 measurements (18.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  12 (12.00%) high severe

routing_decision/parse_lookup_forward
                        time:   [39.856 ns 40.622 ns 41.325 ns]
                        thrpt:  [24.198 Melem/s 24.617 Melem/s 25.091 Melem/s]
                 change:
                        time:   [+2.4008% +4.4178% +6.5005%] (p = 0.00 < 0.05)
                        thrpt:  [−6.1037% −4.2309% −2.3445%]
                        Performance has regressed.
routing_decision/full_with_stats
                        time:   [129.67 ns 129.78 ns 129.91 ns]
                        thrpt:  [7.6978 Melem/s 7.7053 Melem/s 7.7122 Melem/s]
                 change:
                        time:   [−2.3457% −2.0377% −1.7211%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7512% +2.0801% +2.4021%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe

stream_multiplexing/lookup_all/10
                        time:   [295.26 ns 295.32 ns 295.39 ns]
                        thrpt:  [33.854 Melem/s 33.861 Melem/s 33.868 Melem/s]
                 change:
                        time:   [+1.5035% +1.7020% +1.9007%] (p = 0.00 < 0.05)
                        thrpt:  [−1.8653% −1.6735% −1.4813%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
stream_multiplexing/stats_all/10
                        time:   [547.68 ns 558.02 ns 567.92 ns]
                        thrpt:  [17.608 Melem/s 17.920 Melem/s 18.259 Melem/s]
                 change:
                        time:   [−2.7474% −0.8286% +1.0532%] (p = 0.39 > 0.05)
                        thrpt:  [−1.0423% +0.8356% +2.8250%]
                        No change in performance detected.
stream_multiplexing/lookup_all/100
                        time:   [2.9053 µs 2.9098 µs 2.9187 µs]
                        thrpt:  [34.262 Melem/s 34.367 Melem/s 34.420 Melem/s]
                 change:
                        time:   [−0.1285% +0.0700% +0.2923%] (p = 0.53 > 0.05)
                        thrpt:  [−0.2914% −0.0700% +0.1287%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
stream_multiplexing/stats_all/100
                        time:   [5.5375 µs 5.6364 µs 5.7305 µs]
                        thrpt:  [17.450 Melem/s 17.742 Melem/s 18.059 Melem/s]
                 change:
                        time:   [−0.5903% +1.4611% +3.4640%] (p = 0.16 > 0.05)
                        thrpt:  [−3.3480% −1.4401% +0.5938%]
                        No change in performance detected.
stream_multiplexing/lookup_all/1000
                        time:   [29.066 µs 29.079 µs 29.096 µs]
                        thrpt:  [34.369 Melem/s 34.389 Melem/s 34.405 Melem/s]
                 change:
                        time:   [−0.3591% +0.0207% +0.3683%] (p = 0.91 > 0.05)
                        thrpt:  [−0.3670% −0.0207% +0.3604%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  2 (2.00%) high mild
  15 (15.00%) high severe
stream_multiplexing/stats_all/1000
                        time:   [55.487 µs 56.708 µs 57.854 µs]
                        thrpt:  [17.285 Melem/s 17.634 Melem/s 18.022 Melem/s]
                 change:
                        time:   [−1.4409% +0.6224% +2.5803%] (p = 0.54 > 0.05)
                        thrpt:  [−2.5154% −0.6186% +1.4620%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
stream_multiplexing/lookup_all/10000
                        time:   [290.94 µs 291.43 µs 292.08 µs]
                        thrpt:  [34.237 Melem/s 34.313 Melem/s 34.372 Melem/s]
                 change:
                        time:   [−0.1413% +0.0406% +0.2438%] (p = 0.68 > 0.05)
                        thrpt:  [−0.2432% −0.0406% +0.1415%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
stream_multiplexing/stats_all/10000
                        time:   [574.02 µs 581.10 µs 587.87 µs]
                        thrpt:  [17.011 Melem/s 17.209 Melem/s 17.421 Melem/s]
                 change:
                        time:   [−1.5412% +0.1551% +1.7466%] (p = 0.86 > 0.05)
                        thrpt:  [−1.7166% −0.1548% +1.5654%]
                        No change in performance detected.

multihop_packet_builder/build/64
                        time:   [25.808 ns 25.862 ns 25.949 ns]
                        thrpt:  [2.2970 GiB/s 2.3047 GiB/s 2.3095 GiB/s]
                 change:
                        time:   [−12.096% −6.0561% −1.6041%] (p = 0.01 < 0.05)
                        thrpt:  [+1.6302% +6.4465% +13.760%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
multihop_packet_builder/build_priority/64
                        time:   [24.093 ns 24.119 ns 24.144 ns]
                        thrpt:  [2.4687 GiB/s 2.4713 GiB/s 2.4739 GiB/s]
                 change:
                        time:   [−2.0528% −1.4374% −0.8741%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8818% +1.4584% +2.0958%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
multihop_packet_builder/build/256
                        time:   [50.396 ns 51.056 ns 51.705 ns]
                        thrpt:  [4.6112 GiB/s 4.6698 GiB/s 4.7309 GiB/s]
                 change:
                        time:   [−0.8889% +0.5657% +2.0679%] (p = 0.46 > 0.05)
                        thrpt:  [−2.0260% −0.5626% +0.8969%]
                        No change in performance detected.
multihop_packet_builder/build_priority/256
                        time:   [48.518 ns 49.313 ns 50.057 ns]
                        thrpt:  [4.7629 GiB/s 4.8348 GiB/s 4.9141 GiB/s]
                 change:
                        time:   [−3.3139% −1.6417% −0.1193%] (p = 0.05 < 0.05)
                        thrpt:  [+0.1194% +1.6691% +3.4274%]
                        Change within noise threshold.
multihop_packet_builder/build/1024
                        time:   [39.315 ns 39.442 ns 39.605 ns]
                        thrpt:  [24.080 GiB/s 24.179 GiB/s 24.257 GiB/s]
                 change:
                        time:   [−1.0918% −0.8068% −0.4825%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4848% +0.8134% +1.1038%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
multihop_packet_builder/build_priority/1024
                        time:   [36.396 ns 36.404 ns 36.415 ns]
                        thrpt:  [26.189 GiB/s 26.197 GiB/s 26.203 GiB/s]
                 change:
                        time:   [−9.7676% −3.8021% −0.3470%] (p = 0.20 > 0.05)
                        thrpt:  [+0.3482% +3.9524% +10.825%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
multihop_packet_builder/build/4096
                        time:   [78.055 ns 79.039 ns 80.177 ns]
                        thrpt:  [47.578 GiB/s 48.264 GiB/s 48.872 GiB/s]
                 change:
                        time:   [−4.8524% −3.8764% −2.7948%] (p = 0.00 < 0.05)
                        thrpt:  [+2.8752% +4.0327% +5.0998%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  4 (4.00%) high mild
  14 (14.00%) high severe
multihop_packet_builder/build_priority/4096
                        time:   [75.973 ns 76.591 ns 77.452 ns]
                        thrpt:  [49.252 GiB/s 49.806 GiB/s 50.211 GiB/s]
                 change:
                        time:   [−3.9461% −2.4061% −1.0121%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0224% +2.4655% +4.1082%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

multihop_chain/forward_chain/1
                        time:   [56.616 ns 57.311 ns 58.058 ns]
                        thrpt:  [17.224 Melem/s 17.449 Melem/s 17.663 Melem/s]
                 change:
                        time:   [−2.7313% −1.1569% +0.4318%] (p = 0.16 > 0.05)
                        thrpt:  [−0.4299% +1.1704% +2.8079%]
                        No change in performance detected.
multihop_chain/forward_chain/2
                        time:   [109.52 ns 110.00 ns 110.60 ns]
                        thrpt:  [9.0414 Melem/s 9.0908 Melem/s 9.1307 Melem/s]
                 change:
                        time:   [−2.2848% −1.3004% −0.3375%] (p = 0.01 < 0.05)
                        thrpt:  [+0.3386% +1.3176% +2.3382%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
multihop_chain/forward_chain/3
                        time:   [158.68 ns 160.42 ns 162.15 ns]
                        thrpt:  [6.1670 Melem/s 6.2336 Melem/s 6.3018 Melem/s]
                 change:
                        time:   [−4.5769% −3.0952% −1.5681%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5931% +3.1941% +4.7964%]
                        Performance has improved.
multihop_chain/forward_chain/4
                        time:   [213.17 ns 215.54 ns 217.91 ns]
                        thrpt:  [4.5891 Melem/s 4.6395 Melem/s 4.6911 Melem/s]
                 change:
                        time:   [−1.0231% +0.2845% +1.5936%] (p = 0.67 > 0.05)
                        thrpt:  [−1.5686% −0.2837% +1.0337%]
                        No change in performance detected.
multihop_chain/forward_chain/5
                        time:   [256.05 ns 256.57 ns 257.12 ns]
                        thrpt:  [3.8892 Melem/s 3.8975 Melem/s 3.9055 Melem/s]
                 change:
                        time:   [−6.4014% −5.2385% −4.0923%] (p = 0.00 < 0.05)
                        thrpt:  [+4.2669% +5.5281% +6.8391%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

hop_latency/single_hop_process
                        time:   [1.4482 ns 1.4487 ns 1.4493 ns]
                        thrpt:  [689.97 Melem/s 690.28 Melem/s 690.54 Melem/s]
                 change:
                        time:   [−3.0023% −2.8301% −2.6172%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6875% +2.9125% +3.0952%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  6 (6.00%) high severe
hop_latency/single_hop_full
                        time:   [56.508 ns 57.430 ns 58.338 ns]
                        thrpt:  [17.141 Melem/s 17.413 Melem/s 17.697 Melem/s]
                 change:
                        time:   [−1.7560% −0.2044% +1.4283%] (p = 0.80 > 0.05)
                        thrpt:  [−1.4082% +0.2048% +1.7874%]
                        No change in performance detected.
Found 29 outliers among 100 measurements (29.00%)
  17 (17.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  8 (8.00%) high severe

hop_scaling/64B_1hops   time:   [31.671 ns 31.741 ns 31.807 ns]
                        thrpt:  [1.8739 GiB/s 1.8779 GiB/s 1.8820 GiB/s]
                 change:
                        time:   [−3.9094% −3.2786% −2.6603%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7330% +3.3897% +4.0685%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
hop_scaling/64B_2hops   time:   [80.781 ns 81.289 ns 81.844 ns]
                        thrpt:  [745.75 MiB/s 750.84 MiB/s 755.56 MiB/s]
                 change:
                        time:   [+2.7943% +3.3789% +3.9270%] (p = 0.00 < 0.05)
                        thrpt:  [−3.7786% −3.2685% −2.7183%]
                        Performance has regressed.
Found 21 outliers among 100 measurements (21.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  15 (15.00%) high severe
hop_scaling/64B_3hops   time:   [107.63 ns 108.78 ns 109.85 ns]
                        thrpt:  [555.61 MiB/s 561.07 MiB/s 567.10 MiB/s]
                 change:
                        time:   [−0.2808% +1.0838% +2.4298%] (p = 0.12 > 0.05)
                        thrpt:  [−2.3721% −1.0722% +0.2816%]
                        No change in performance detected.
hop_scaling/64B_4hops   time:   [136.86 ns 137.46 ns 138.03 ns]
                        thrpt:  [442.19 MiB/s 444.01 MiB/s 445.98 MiB/s]
                 change:
                        time:   [+0.6043% +1.1960% +1.7517%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7216% −1.1818% −0.6007%]
                        Change within noise threshold.
hop_scaling/64B_5hops   time:   [164.68 ns 165.79 ns 167.13 ns]
                        thrpt:  [365.20 MiB/s 368.14 MiB/s 370.62 MiB/s]
                 change:
                        time:   [−4.1287% −2.9059% −1.6246%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6515% +2.9928% +4.3065%]
                        Performance has improved.
Found 22 outliers among 100 measurements (22.00%)
  11 (11.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
hop_scaling/256B_1hops  time:   [56.880 ns 58.001 ns 59.094 ns]
                        thrpt:  [4.0346 GiB/s 4.1106 GiB/s 4.1916 GiB/s]
                 change:
                        time:   [−4.2249% −2.4468% −0.7132%] (p = 0.01 < 0.05)
                        thrpt:  [+0.7184% +2.5082% +4.4112%]
                        Change within noise threshold.
hop_scaling/256B_2hops  time:   [112.01 ns 112.95 ns 113.95 ns]
                        thrpt:  [2.0923 GiB/s 2.1109 GiB/s 2.1286 GiB/s]
                 change:
                        time:   [−2.7103% −1.9004% −1.0881%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1001% +1.9372% +2.7858%]
                        Performance has improved.
hop_scaling/256B_3hops  time:   [156.74 ns 158.41 ns 160.03 ns]
                        thrpt:  [1.4898 GiB/s 1.5051 GiB/s 1.5211 GiB/s]
                 change:
                        time:   [−11.883% −10.751% −9.5733%] (p = 0.00 < 0.05)
                        thrpt:  [+10.587% +12.046% +13.485%]
                        Performance has improved.
hop_scaling/256B_4hops  time:   [218.43 ns 221.45 ns 224.61 ns]
                        thrpt:  [1.0615 GiB/s 1.0766 GiB/s 1.0915 GiB/s]
                 change:
                        time:   [−3.9569% −2.5069% −0.9919%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0019% +2.5714% +4.1199%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
hop_scaling/256B_5hops  time:   [265.69 ns 267.35 ns 269.15 ns]
                        thrpt:  [907.10 MiB/s 913.18 MiB/s 918.88 MiB/s]
                 change:
                        time:   [−1.5826% −0.7155% +0.1960%] (p = 0.12 > 0.05)
                        thrpt:  [−0.1956% +0.7207% +1.6080%]
                        No change in performance detected.
hop_scaling/1024B_1hops time:   [45.564 ns 45.699 ns 45.890 ns]
                        thrpt:  [20.782 GiB/s 20.869 GiB/s 20.931 GiB/s]
                 change:
                        time:   [−1.3589% −1.0217% −0.6653%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6698% +1.0323% +1.3776%]
                        Change within noise threshold.
Found 18 outliers among 100 measurements (18.00%)
  10 (10.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
hop_scaling/1024B_2hops time:   [107.42 ns 107.64 ns 107.86 ns]
                        thrpt:  [8.8414 GiB/s 8.8600 GiB/s 8.8778 GiB/s]
                 change:
                        time:   [−0.5600% −0.2769% +0.0227%] (p = 0.06 > 0.05)
                        thrpt:  [−0.0227% +0.2777% +0.5632%]
                        No change in performance detected.
Found 21 outliers among 100 measurements (21.00%)
  11 (11.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
hop_scaling/1024B_3hops time:   [149.66 ns 151.17 ns 152.70 ns]
                        thrpt:  [6.2454 GiB/s 6.3088 GiB/s 6.3724 GiB/s]
                 change:
                        time:   [−0.4926% +0.8852% +2.3213%] (p = 0.22 > 0.05)
                        thrpt:  [−2.2687% −0.8774% +0.4951%]
                        No change in performance detected.
hop_scaling/1024B_4hops time:   [205.19 ns 205.70 ns 206.19 ns]
                        thrpt:  [4.6253 GiB/s 4.6362 GiB/s 4.6478 GiB/s]
                 change:
                        time:   [−0.4320% −0.0767% +0.2908%] (p = 0.69 > 0.05)
                        thrpt:  [−0.2900% +0.0768% +0.4339%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
hop_scaling/1024B_5hops time:   [242.34 ns 244.01 ns 245.73 ns]
                        thrpt:  [3.8811 GiB/s 3.9083 GiB/s 3.9353 GiB/s]
                 change:
                        time:   [+0.8282% +1.5673% +2.2937%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2423% −1.5431% −0.8214%]
                        Change within noise threshold.

multihop_with_routing/route_and_forward/1
                        time:   [202.97 ns 203.60 ns 204.27 ns]
                        thrpt:  [4.8955 Melem/s 4.9115 Melem/s 4.9268 Melem/s]
                 change:
                        time:   [−1.4295% −0.9639% −0.5185%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5212% +0.9733% +1.4502%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_with_routing/route_and_forward/2
                        time:   [399.82 ns 401.67 ns 403.39 ns]
                        thrpt:  [2.4790 Melem/s 2.4896 Melem/s 2.5011 Melem/s]
                 change:
                        time:   [−2.7232% −2.1145% −1.4795%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5017% +2.1602% +2.7994%]
                        Performance has improved.
multihop_with_routing/route_and_forward/3
                        time:   [598.95 ns 602.79 ns 607.31 ns]
                        thrpt:  [1.6466 Melem/s 1.6590 Melem/s 1.6696 Melem/s]
                 change:
                        time:   [−0.8747% −0.2180% +0.5355%] (p = 0.57 > 0.05)
                        thrpt:  [−0.5326% +0.2185% +0.8824%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
multihop_with_routing/route_and_forward/4
                        time:   [796.19 ns 799.96 ns 804.00 ns]
                        thrpt:  [1.2438 Melem/s 1.2501 Melem/s 1.2560 Melem/s]
                 change:
                        time:   [−0.8024% −0.2446% +0.3272%] (p = 0.40 > 0.05)
                        thrpt:  [−0.3262% +0.2452% +0.8089%]
                        No change in performance detected.
multihop_with_routing/route_and_forward/5
                        time:   [996.28 ns 1.0009 µs 1.0054 µs]
                        thrpt:  [994.63 Kelem/s 999.14 Kelem/s 1.0037 Melem/s]
                 change:
                        time:   [−0.6030% −0.0326% +0.5496%] (p = 0.91 > 0.05)
                        thrpt:  [−0.5466% +0.0326% +0.6067%]
                        No change in performance detected.

multihop_concurrent/concurrent_forward/4
                        time:   [949.68 µs 957.35 µs 965.00 µs]
                        thrpt:  [4.1451 Melem/s 4.1782 Melem/s 4.2119 Melem/s]
                 change:
                        time:   [−3.7500% −1.7271% +0.3781%] (p = 0.13 > 0.05)
                        thrpt:  [−0.3767% +1.7575% +3.8961%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
multihop_concurrent/concurrent_forward/8
                        time:   [1.3530 ms 1.3603 ms 1.3675 ms]
                        thrpt:  [5.8503 Melem/s 5.8809 Melem/s 5.9126 Melem/s]
                 change:
                        time:   [+9.5317% +10.839% +12.120%] (p = 0.00 < 0.05)
                        thrpt:  [−10.810% −9.7787% −8.7022%]
                        Performance has regressed.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low severe
  1 (5.00%) low mild
multihop_concurrent/concurrent_forward/16
                        time:   [1.9466 ms 1.9865 ms 2.0410 ms]
                        thrpt:  [7.8394 Melem/s 8.0542 Melem/s 8.2193 Melem/s]
                 change:
                        time:   [−17.035% −15.542% −13.742%] (p = 0.00 < 0.05)
                        thrpt:  [+15.931% +18.402% +20.533%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe

pingwave/serialize      time:   [776.16 ps 777.16 ps 778.66 ps]
                        thrpt:  [1.2843 Gelem/s 1.2867 Gelem/s 1.2884 Gelem/s]
                 change:
                        time:   [−4.9214% −4.6242% −4.2968%] (p = 0.00 < 0.05)
                        thrpt:  [+4.4897% +4.8485% +5.1761%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe
pingwave/deserialize    time:   [930.98 ps 931.26 ps 931.66 ps]
                        thrpt:  [1.0734 Gelem/s 1.0738 Gelem/s 1.0741 Gelem/s]
                 change:
                        time:   [−3.0062% −2.8353% −2.6445%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7163% +2.9180% +3.0994%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
pingwave/roundtrip      time:   [930.98 ps 931.27 ps 931.75 ps]
                        thrpt:  [1.0732 Gelem/s 1.0738 Gelem/s 1.0741 Gelem/s]
                 change:
                        time:   [−1.7347% −1.4263% −1.1070%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1194% +1.4469% +1.7653%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
pingwave/forward        time:   [623.82 ps 624.83 ps 625.94 ps]
                        thrpt:  [1.5976 Gelem/s 1.6004 Gelem/s 1.6030 Gelem/s]
                 change:
                        time:   [−0.2876% +0.0069% +0.3023%] (p = 0.96 > 0.05)
                        thrpt:  [−0.3013% −0.0069% +0.2884%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe

capabilities/serialize_simple
                        time:   [20.719 ns 20.751 ns 20.785 ns]
                        thrpt:  [48.112 Melem/s 48.191 Melem/s 48.264 Melem/s]
                 change:
                        time:   [−1.0123% −0.6734% −0.3405%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3417% +0.6780% +1.0226%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  9 (9.00%) high mild
  5 (5.00%) high severe
capabilities/deserialize_simple
                        time:   [5.5854 ns 5.5898 ns 5.5946 ns]
                        thrpt:  [178.74 Melem/s 178.90 Melem/s 179.04 Melem/s]
                 change:
                        time:   [+0.1461% +0.3606% +0.6197%] (p = 0.00 < 0.05)
                        thrpt:  [−0.6159% −0.3593% −0.1459%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capabilities/serialize_complex
                        time:   [44.941 ns 45.066 ns 45.164 ns]
                        thrpt:  [22.141 Melem/s 22.189 Melem/s 22.251 Melem/s]
                 change:
                        time:   [+0.8665% +1.1680% +1.4688%] (p = 0.00 < 0.05)
                        thrpt:  [−1.4475% −1.1545% −0.8590%]
                        Change within noise threshold.
capabilities/deserialize_complex
                        time:   [381.92 ns 384.88 ns 388.19 ns]
                        thrpt:  [2.5760 Melem/s 2.5982 Melem/s 2.6184 Melem/s]
                 change:
                        time:   [−12.658% −5.0668% −0.4100%] (p = 0.19 > 0.05)
                        thrpt:  [+0.4117% +5.3372% +14.492%]
                        No change in performance detected.

local_graph/create_pingwave
                        time:   [2.1445 ns 2.1551 ns 2.1660 ns]
                        thrpt:  [461.68 Melem/s 464.02 Melem/s 466.30 Melem/s]
                 change:
                        time:   [−0.9723% −0.3444% +0.2988%] (p = 0.30 > 0.05)
                        thrpt:  [−0.2979% +0.3456% +0.9819%]
                        No change in performance detected.
local_graph/on_pingwave_new
                        time:   [85.388 ns 92.502 ns 98.465 ns]
                        thrpt:  [10.156 Melem/s 10.811 Melem/s 11.711 Melem/s]
                 change:
                        time:   [−12.265% −2.9671% +7.4290%] (p = 0.55 > 0.05)
                        thrpt:  [−6.9153% +3.0578% +13.980%]
                        No change in performance detected.
local_graph/on_pingwave_duplicate
                        time:   [208.00 ns 208.18 ns 208.41 ns]
                        thrpt:  [4.7982 Melem/s 4.8036 Melem/s 4.8076 Melem/s]
                 change:
                        time:   [+0.0655% +0.2134% +0.3819%] (p = 0.01 < 0.05)
                        thrpt:  [−0.3805% −0.2130% −0.0655%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
local_graph/get_node    time:   [15.039 ns 15.051 ns 15.065 ns]
                        thrpt:  [66.379 Melem/s 66.442 Melem/s 66.492 Melem/s]
                 change:
                        time:   [+0.2254% +0.3492% +0.5081%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5056% −0.3480% −0.2249%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
local_graph/node_count  time:   [199.24 ns 199.33 ns 199.47 ns]
                        thrpt:  [5.0132 Melem/s 5.0169 Melem/s 5.0191 Melem/s]
                 change:
                        time:   [−0.0600% +0.0563% +0.1853%] (p = 0.39 > 0.05)
                        thrpt:  [−0.1850% −0.0562% +0.0600%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
local_graph/stats       time:   [596.88 ns 597.26 ns 597.79 ns]
                        thrpt:  [1.6728 Melem/s 1.6743 Melem/s 1.6754 Melem/s]
                 change:
                        time:   [+0.1026% +0.4034% +0.8519%] (p = 0.02 < 0.05)
                        thrpt:  [−0.8447% −0.4018% −0.1025%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  5 (5.00%) high mild
  11 (11.00%) high severe

graph_scaling/all_nodes/100
                        time:   [2.8668 µs 2.8771 µs 2.8874 µs]
                        thrpt:  [34.633 Melem/s 34.757 Melem/s 34.883 Melem/s]
                 change:
                        time:   [+0.7389% +1.1522% +1.5701%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5458% −1.1390% −0.7335%]
                        Change within noise threshold.
graph_scaling/nodes_within_hops/100
                        time:   [3.1159 µs 3.1248 µs 3.1343 µs]
                        thrpt:  [31.905 Melem/s 32.002 Melem/s 32.093 Melem/s]
                 change:
                        time:   [−13.374% −5.8630% −1.3975%] (p = 0.14 > 0.05)
                        thrpt:  [+1.4173% +6.2281% +15.439%]
                        No change in performance detected.
graph_scaling/all_nodes/500
                        time:   [8.3163 µs 8.3635 µs 8.4137 µs]
                        thrpt:  [59.427 Melem/s 59.783 Melem/s 60.123 Melem/s]
                 change:
                        time:   [−1.0096% −0.4942% +0.0127%] (p = 0.06 > 0.05)
                        thrpt:  [−0.0126% +0.4967% +1.0199%]
                        No change in performance detected.
graph_scaling/nodes_within_hops/500
                        time:   [9.6686 µs 9.7043 µs 9.7387 µs]
                        thrpt:  [51.342 Melem/s 51.523 Melem/s 51.714 Melem/s]
                 change:
                        time:   [−1.3046% −0.9783% −0.6407%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6448% +0.9879% +1.3218%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
graph_scaling/all_nodes/1000
                        time:   [56.853 µs 59.775 µs 62.714 µs]
                        thrpt:  [15.946 Melem/s 16.729 Melem/s 17.589 Melem/s]
                 change:
                        time:   [+0.0916% +5.6542% +11.263%] (p = 0.04 < 0.05)
                        thrpt:  [−10.123% −5.3516% −0.0915%]
                        Change within noise threshold.
graph_scaling/nodes_within_hops/1000
                        time:   [55.718 µs 58.336 µs 60.995 µs]
                        thrpt:  [16.395 Melem/s 17.142 Melem/s 17.948 Melem/s]
                 change:
                        time:   [−21.187% −18.457% −15.320%] (p = 0.00 < 0.05)
                        thrpt:  [+18.092% +22.635% +26.883%]
                        Performance has improved.
graph_scaling/all_nodes/5000
                        time:   [109.73 µs 112.21 µs 114.82 µs]
                        thrpt:  [43.548 Melem/s 44.560 Melem/s 45.566 Melem/s]
                 change:
                        time:   [−0.4638% +1.8150% +4.4565%] (p = 0.16 > 0.05)
                        thrpt:  [−4.2663% −1.7826% +0.4659%]
                        No change in performance detected.
graph_scaling/nodes_within_hops/5000
                        time:   [126.67 µs 129.12 µs 131.53 µs]
                        thrpt:  [38.014 Melem/s 38.724 Melem/s 39.472 Melem/s]
                 change:
                        time:   [+4.7346% +7.3037% +9.9906%] (p = 0.00 < 0.05)
                        thrpt:  [−9.0832% −6.8065% −4.5205%]
                        Performance has regressed.

capability_search/find_with_gpu
                        time:   [27.966 µs 28.039 µs 28.121 µs]
                        thrpt:  [35.561 Kelem/s 35.665 Kelem/s 35.758 Kelem/s]
                 change:
                        time:   [+0.3944% +0.7987% +1.2074%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1930% −0.7924% −0.3928%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_search/find_by_tool_python
                        time:   [60.487 µs 60.601 µs 60.739 µs]
                        thrpt:  [16.464 Kelem/s 16.501 Kelem/s 16.533 Kelem/s]
                 change:
                        time:   [−3.8571% −3.5633% −3.2638%] (p = 0.00 < 0.05)
                        thrpt:  [+3.3739% +3.6950% +4.0118%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
capability_search/find_by_tool_rust
                        time:   [79.016 µs 79.116 µs 79.214 µs]
                        thrpt:  [12.624 Kelem/s 12.640 Kelem/s 12.656 Kelem/s]
                 change:
                        time:   [−4.1096% −3.8282% −3.5445%] (p = 0.00 < 0.05)
                        thrpt:  [+3.6748% +3.9806% +4.2857%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

graph_concurrent/concurrent_pingwave/4
                        time:   [112.73 µs 113.83 µs 115.35 µs]
                        thrpt:  [17.338 Melem/s 17.570 Melem/s 17.742 Melem/s]
                 change:
                        time:   [−14.092% −9.8196% −5.3751%] (p = 0.00 < 0.05)
                        thrpt:  [+5.6804% +10.889% +16.403%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe
graph_concurrent/concurrent_pingwave/8
                        time:   [180.80 µs 184.49 µs 187.73 µs]
                        thrpt:  [21.307 Melem/s 21.682 Melem/s 22.123 Melem/s]
                 change:
                        time:   [−4.3282% −2.5980% −0.3473%] (p = 0.02 < 0.05)
                        thrpt:  [+0.3485% +2.6673% +4.5240%]
                        Change within noise threshold.
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) high mild
  1 (5.00%) high severe
graph_concurrent/concurrent_pingwave/16
                        time:   [329.83 µs 330.93 µs 332.02 µs]
                        thrpt:  [24.095 Melem/s 24.174 Melem/s 24.255 Melem/s]
                 change:
                        time:   [−0.8810% −0.2232% +0.5122%] (p = 0.54 > 0.05)
                        thrpt:  [−0.5096% +0.2237% +0.8888%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

path_finding/path_1_hop time:   [2.2916 µs 2.2975 µs 2.3038 µs]
                        thrpt:  [434.07 Kelem/s 435.25 Kelem/s 436.38 Kelem/s]
                 change:
                        time:   [−0.0970% +0.1807% +0.4365%] (p = 0.21 > 0.05)
                        thrpt:  [−0.4346% −0.1804% +0.0971%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
path_finding/path_2_hops
                        time:   [2.3461 µs 2.3506 µs 2.3553 µs]
                        thrpt:  [424.58 Kelem/s 425.42 Kelem/s 426.24 Kelem/s]
                 change:
                        time:   [−1.0599% −0.7147% −0.4250%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4268% +0.7199% +1.0713%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
path_finding/path_4_hops
                        time:   [2.6467 µs 2.6513 µs 2.6561 µs]
                        thrpt:  [376.49 Kelem/s 377.17 Kelem/s 377.83 Kelem/s]
                 change:
                        time:   [−16.186% −7.9928% −3.0441%] (p = 0.05 > 0.05)
                        thrpt:  [+3.1397% +8.6871% +19.312%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
path_finding/path_not_found
                        time:   [2.4195 µs 2.4215 µs 2.4237 µs]
                        thrpt:  [412.59 Kelem/s 412.96 Kelem/s 413.31 Kelem/s]
                 change:
                        time:   [−8.4718% −7.5828% −6.9389%] (p = 0.00 < 0.05)
                        thrpt:  [+7.4563% +8.2049% +9.2560%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
path_finding/path_complex_graph
                        time:   [262.63 µs 264.31 µs 266.12 µs]
                        thrpt:  [3.7576 Kelem/s 3.7834 Kelem/s 3.8077 Kelem/s]
                 change:
                        time:   [−15.574% −10.923% −6.5345%] (p = 0.00 < 0.05)
                        thrpt:  [+6.9914% +12.263% +18.448%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low severe
  10 (10.00%) high mild
  6 (6.00%) high severe

failure_detector/heartbeat_existing
                        time:   [28.548 ns 28.843 ns 29.359 ns]
                        thrpt:  [34.062 Melem/s 34.670 Melem/s 35.028 Melem/s]
                 change:
                        time:   [−21.755% −19.271% −16.554%] (p = 0.00 < 0.05)
                        thrpt:  [+19.837% +23.872% +27.803%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
failure_detector/heartbeat_new
                        time:   [230.90 ns 233.69 ns 235.87 ns]
                        thrpt:  [4.2397 Melem/s 4.2791 Melem/s 4.3309 Melem/s]
                 change:
                        time:   [−15.425% −11.842% −8.0766%] (p = 0.00 < 0.05)
                        thrpt:  [+8.7863% +13.433% +18.238%]
                        Performance has improved.
failure_detector/status_check
                        time:   [14.927 ns 15.175 ns 15.394 ns]
                        thrpt:  [64.962 Melem/s 65.900 Melem/s 66.991 Melem/s]
                 change:
                        time:   [−10.173% −8.2307% −6.2582%] (p = 0.00 < 0.05)
                        thrpt:  [+6.6761% +8.9689% +11.325%]
                        Performance has improved.
Benchmarking failure_detector/check_all: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 35.1s, or reduce sample count to 10.
failure_detector/check_all
                        time:   [342.90 ms 343.01 ms 343.16 ms]
                        thrpt:  [2.9141  elem/s 2.9154  elem/s 2.9163  elem/s]
                 change:
                        time:   [−2.6261% −2.2381% −1.8431%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8777% +2.2893% +2.6970%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
Benchmarking failure_detector/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.0s, or reduce sample count to 60.
failure_detector/stats  time:   [80.256 ms 80.324 ms 80.413 ms]
                        thrpt:  [12.436  elem/s 12.450  elem/s 12.460  elem/s]
                 change:
                        time:   [−8.4880% −3.2052% −0.1998%] (p = 0.21 > 0.05)
                        thrpt:  [+0.2002% +3.3113% +9.2753%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe

loss_simulator/should_drop_1pct
                        time:   [2.7834 ns 2.7848 ns 2.7865 ns]
                        thrpt:  [358.87 Melem/s 359.10 Melem/s 359.27 Melem/s]
                 change:
                        time:   [−2.9168% −2.7940% −2.6477%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7197% +2.8743% +3.0044%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
loss_simulator/should_drop_5pct
                        time:   [3.1424 ns 3.1446 ns 3.1474 ns]
                        thrpt:  [317.72 Melem/s 318.00 Melem/s 318.23 Melem/s]
                 change:
                        time:   [−2.8772% −2.7624% −2.6331%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7043% +2.8409% +2.9624%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  11 (11.00%) high severe
loss_simulator/should_drop_10pct
                        time:   [3.6104 ns 3.6133 ns 3.6167 ns]
                        thrpt:  [276.49 Melem/s 276.76 Melem/s 276.98 Melem/s]
                 change:
                        time:   [−1.1654% −0.8765% −0.5833%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5867% +0.8843% +1.1791%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
loss_simulator/should_drop_20pct
                        time:   [4.5585 ns 4.5630 ns 4.5685 ns]
                        thrpt:  [218.89 Melem/s 219.15 Melem/s 219.37 Melem/s]
                 change:
                        time:   [−0.4515% −0.3323% −0.1937%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1941% +0.3334% +0.4536%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
loss_simulator/should_drop_burst
                        time:   [2.9156 ns 2.9177 ns 2.9201 ns]
                        thrpt:  [342.45 Melem/s 342.73 Melem/s 342.98 Melem/s]
                 change:
                        time:   [−0.0811% +0.0847% +0.2861%] (p = 0.36 > 0.05)
                        thrpt:  [−0.2853% −0.0846% +0.0812%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  8 (8.00%) high mild
  4 (4.00%) high severe

circuit_breaker/allow_closed
                        time:   [9.5071 ns 9.5501 ns 9.6014 ns]
                        thrpt:  [104.15 Melem/s 104.71 Melem/s 105.18 Melem/s]
                 change:
                        time:   [+0.0411% +0.2500% +0.4678%] (p = 0.02 < 0.05)
                        thrpt:  [−0.4656% −0.2493% −0.0410%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) high mild
  7 (7.00%) high severe
circuit_breaker/record_success
                        time:   [8.3743 ns 8.3849 ns 8.3958 ns]
                        thrpt:  [119.11 Melem/s 119.26 Melem/s 119.41 Melem/s]
                 change:
                        time:   [−0.2982% +0.0515% +0.3908%] (p = 0.77 > 0.05)
                        thrpt:  [−0.3892% −0.0515% +0.2991%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
circuit_breaker/record_failure
                        time:   [7.4142 ns 7.4207 ns 7.4289 ns]
                        thrpt:  [134.61 Melem/s 134.76 Melem/s 134.88 Melem/s]
                 change:
                        time:   [−7.9461% −2.8560% +0.0586%] (p = 0.26 > 0.05)
                        thrpt:  [−0.0585% +2.9400% +8.6320%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe
circuit_breaker/state   time:   [9.7741 ns 9.7851 ns 9.7983 ns]
                        thrpt:  [102.06 Melem/s 102.20 Melem/s 102.31 Melem/s]
                 change:
                        time:   [−0.0696% +0.1254% +0.3282%] (p = 0.22 > 0.05)
                        thrpt:  [−0.3272% −0.1253% +0.0697%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

recovery_manager/on_failure_with_alternates
                        time:   [248.95 ns 257.22 ns 265.56 ns]
                        thrpt:  [3.7656 Melem/s 3.8877 Melem/s 4.0169 Melem/s]
                 change:
                        time:   [+2.0030% +8.3369% +14.676%] (p = 0.01 < 0.05)
                        thrpt:  [−12.798% −7.6953% −1.9636%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
recovery_manager/on_failure_no_alternates
                        time:   [283.78 ns 314.61 ns 343.36 ns]
                        thrpt:  [2.9124 Melem/s 3.1785 Melem/s 3.5238 Melem/s]
                 change:
                        time:   [+30.290% +47.971% +65.291%] (p = 0.00 < 0.05)
                        thrpt:  [−39.501% −32.419% −23.248%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild
recovery_manager/get_action
                        time:   [37.008 ns 37.030 ns 37.055 ns]
                        thrpt:  [26.987 Melem/s 27.005 Melem/s 27.021 Melem/s]
                 change:
                        time:   [+0.0028% +0.1399% +0.2891%] (p = 0.05 < 0.05)
                        thrpt:  [−0.2883% −0.1397% −0.0028%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
recovery_manager/is_failed
                        time:   [13.769 ns 14.217 ns 14.642 ns]
                        thrpt:  [68.296 Melem/s 70.339 Melem/s 72.628 Melem/s]
                 change:
                        time:   [−7.9117% −4.7417% −1.4115%] (p = 0.01 < 0.05)
                        thrpt:  [+1.4317% +4.9777% +8.5915%]
                        Performance has improved.
recovery_manager/on_recovery
                        time:   [98.771 ns 98.911 ns 99.071 ns]
                        thrpt:  [10.094 Melem/s 10.110 Melem/s 10.124 Melem/s]
                 change:
                        time:   [−2.4314% −1.7649% −1.2137%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2286% +1.7966% +2.4920%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
recovery_manager/stats  time:   [698.28 ps 698.52 ps 698.92 ps]
                        thrpt:  [1.4308 Gelem/s 1.4316 Gelem/s 1.4321 Gelem/s]
                 change:
                        time:   [+0.0284% +0.1144% +0.2016%] (p = 0.01 < 0.05)
                        thrpt:  [−0.2012% −0.1143% −0.0284%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe

failure_scaling/check_all/100
                        time:   [5.2195 µs 5.2377 µs 5.2541 µs]
                        thrpt:  [19.033 Melem/s 19.092 Melem/s 19.159 Melem/s]
                 change:
                        time:   [−6.3780% −5.4413% −4.5577%] (p = 0.00 < 0.05)
                        thrpt:  [+4.7754% +5.7544% +6.8125%]
                        Performance has improved.
Found 39 outliers among 100 measurements (39.00%)
  19 (19.00%) low severe
  1 (1.00%) high mild
  19 (19.00%) high severe
failure_scaling/healthy_nodes/100
                        time:   [2.1214 µs 2.1228 µs 2.1248 µs]
                        thrpt:  [47.063 Melem/s 47.107 Melem/s 47.140 Melem/s]
                 change:
                        time:   [−2.8434% −2.7102% −2.5741%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6421% +2.7857% +2.9267%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
failure_scaling/check_all/500
                        time:   [21.067 µs 21.161 µs 21.240 µs]
                        thrpt:  [23.540 Melem/s 23.629 Melem/s 23.734 Melem/s]
                 change:
                        time:   [−4.2135% −2.3179% −0.7540%] (p = 0.01 < 0.05)
                        thrpt:  [+0.7597% +2.3729% +4.3989%]
                        Change within noise threshold.
Found 43 outliers among 100 measurements (43.00%)
  24 (24.00%) low severe
  1 (1.00%) low mild
  18 (18.00%) high mild
failure_scaling/healthy_nodes/500
                        time:   [5.8125 µs 5.8383 µs 5.8690 µs]
                        thrpt:  [85.193 Melem/s 85.641 Melem/s 86.022 Melem/s]
                 change:
                        time:   [+0.0881% +0.3050% +0.5570%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5539% −0.3041% −0.0881%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe
failure_scaling/check_all/1000
                        time:   [41.585 µs 41.912 µs 42.184 µs]
                        thrpt:  [23.706 Melem/s 23.860 Melem/s 24.047 Melem/s]
                 change:
                        time:   [−0.9108% +1.2481% +3.4528%] (p = 0.27 > 0.05)
                        thrpt:  [−3.3376% −1.2327% +0.9192%]
                        No change in performance detected.
failure_scaling/healthy_nodes/1000
                        time:   [10.576 µs 10.579 µs 10.582 µs]
                        thrpt:  [94.500 Melem/s 94.529 Melem/s 94.555 Melem/s]
                 change:
                        time:   [−0.0131% +0.0961% +0.2217%] (p = 0.13 > 0.05)
                        thrpt:  [−0.2213% −0.0960% +0.0131%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
failure_scaling/check_all/5000
                        time:   [202.56 µs 203.22 µs 203.76 µs]
                        thrpt:  [24.539 Melem/s 24.604 Melem/s 24.684 Melem/s]
                 change:
                        time:   [−0.1564% +0.2025% +0.5721%] (p = 0.29 > 0.05)
                        thrpt:  [−0.5688% −0.2021% +0.1566%]
                        No change in performance detected.
failure_scaling/healthy_nodes/5000
                        time:   [49.210 µs 49.220 µs 49.231 µs]
                        thrpt:  [101.56 Melem/s 101.59 Melem/s 101.61 Melem/s]
                 change:
                        time:   [−14.483% −6.8968% −2.2598%] (p = 0.06 > 0.05)
                        thrpt:  [+2.3121% +7.4077% +16.936%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

failure_concurrent/concurrent_heartbeat/4
                        time:   [196.59 µs 197.13 µs 198.19 µs]
                        thrpt:  [10.092 Melem/s 10.146 Melem/s 10.174 Melem/s]
                 change:
                        time:   [+1.0024% +2.6995% +4.6745%] (p = 0.00 < 0.05)
                        thrpt:  [−4.4657% −2.6285% −0.9925%]
                        Performance has regressed.
Found 5 outliers among 20 measurements (25.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
  3 (15.00%) high severe
failure_concurrent/concurrent_heartbeat/8
                        time:   [259.82 µs 260.46 µs 261.20 µs]
                        thrpt:  [15.314 Melem/s 15.357 Melem/s 15.396 Melem/s]
                 change:
                        time:   [−16.565% −15.601% −14.559%] (p = 0.00 < 0.05)
                        thrpt:  [+17.040% +18.485% +19.854%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
  1 (5.00%) high severe
failure_concurrent/concurrent_heartbeat/16
                        time:   [475.05 µs 475.62 µs 476.38 µs]
                        thrpt:  [16.793 Melem/s 16.820 Melem/s 16.840 Melem/s]
                 change:
                        time:   [−0.9788% −0.6386% −0.2451%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2457% +0.6427% +0.9885%]
                        Change within noise threshold.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe

failure_recovery_cycle/full_cycle
                        time:   [268.64 ns 274.30 ns 278.83 ns]
                        thrpt:  [3.5864 Melem/s 3.6456 Melem/s 3.7225 Melem/s]
                 change:
                        time:   [−10.196% −5.4154% −0.3638%] (p = 0.04 < 0.05)
                        thrpt:  [+0.3651% +5.7255% +11.353%]
                        Change within noise threshold.

capability_set/create   time:   [18.981 µs 19.004 µs 19.026 µs]
                        thrpt:  [52.560 Kelem/s 52.621 Kelem/s 52.683 Kelem/s]
                 change:
                        time:   [−1.4008% −1.1208% −0.8545%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8619% +1.1335% +1.4207%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
capability_set/serialize
                        time:   [65.191 µs 65.226 µs 65.269 µs]
                        thrpt:  [15.321 Kelem/s 15.331 Kelem/s 15.340 Kelem/s]
                 change:
                        time:   [+10.002% +10.260% +10.487%] (p = 0.00 < 0.05)
                        thrpt:  [−9.4915% −9.3052% −9.0929%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
capability_set/deserialize
                        time:   [9.9894 µs 10.002 µs 10.015 µs]
                        thrpt:  [99.850 Kelem/s 99.982 Kelem/s 100.11 Kelem/s]
                 change:
                        time:   [−0.9352% −0.6795% −0.4444%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4464% +0.6842% +0.9441%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
capability_set/roundtrip
                        time:   [75.463 µs 75.492 µs 75.529 µs]
                        thrpt:  [13.240 Kelem/s 13.246 Kelem/s 13.251 Kelem/s]
                 change:
                        time:   [+8.4253% +8.7019% +8.9780%] (p = 0.00 < 0.05)
                        thrpt:  [−8.2383% −8.0053% −7.7706%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high severe
capability_set/serialize_compact
                        time:   [2.7020 µs 2.7051 µs 2.7082 µs]
                        thrpt:  [369.25 Kelem/s 369.67 Kelem/s 370.10 Kelem/s]
                 change:
                        time:   [+1.9851% +2.2327% +2.4834%] (p = 0.00 < 0.05)
                        thrpt:  [−2.4232% −2.1839% −1.9465%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_set/deserialize_compact
                        time:   [7.3978 µs 7.4088 µs 7.4215 µs]
                        thrpt:  [134.74 Kelem/s 134.97 Kelem/s 135.17 Kelem/s]
                 change:
                        time:   [+0.3559% +0.6025% +0.8449%] (p = 0.00 < 0.05)
                        thrpt:  [−0.8379% −0.5989% −0.3546%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
capability_set/roundtrip_compact
                        time:   [9.9316 µs 9.9585 µs 9.9851 µs]
                        thrpt:  [100.15 Kelem/s 100.42 Kelem/s 100.69 Kelem/s]
                 change:
                        time:   [+0.0649% +0.4610% +0.8578%] (p = 0.02 < 0.05)
                        thrpt:  [−0.8505% −0.4589% −0.0648%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
capability_set/has_tag  time:   [58.195 ns 58.212 ns 58.238 ns]
                        thrpt:  [17.171 Melem/s 17.178 Melem/s 17.184 Melem/s]
                 change:
                        time:   [+12.963% +13.188% +13.405%] (p = 0.00 < 0.05)
                        thrpt:  [−11.821% −11.652% −11.475%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
capability_set/has_model
                        time:   [37.862 ns 37.872 ns 37.887 ns]
                        thrpt:  [26.394 Melem/s 26.405 Melem/s 26.412 Melem/s]
                 change:
                        time:   [−3.1121% −2.8452% −2.5884%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6572% +2.9285% +3.2121%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
capability_set/has_tool time:   [34.652 ns 34.802 ns 34.946 ns]
                        thrpt:  [28.616 Melem/s 28.734 Melem/s 28.859 Melem/s]
                 change:
                        time:   [+4.4149% +4.7874% +5.1525%] (p = 0.00 < 0.05)
                        thrpt:  [−4.9000% −4.5687% −4.2282%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild
capability_set/has_gpu  time:   [39.548 ns 39.581 ns 39.623 ns]
                        thrpt:  [25.238 Melem/s 25.264 Melem/s 25.286 Melem/s]
                 change:
                        time:   [−0.1861% +0.0388% +0.2689%] (p = 0.73 > 0.05)
                        thrpt:  [−0.2682% −0.0388% +0.1864%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

capability_announcement/create
                        time:   [3.3187 µs 3.3388 µs 3.3602 µs]
                        thrpt:  [297.60 Kelem/s 299.51 Kelem/s 301.32 Kelem/s]
                 change:
                        time:   [−3.4775% −2.9083% −2.3408%] (p = 0.00 < 0.05)
                        thrpt:  [+2.3969% +2.9955% +3.6028%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_announcement/serialize
                        time:   [63.785 µs 63.859 µs 63.958 µs]
                        thrpt:  [15.635 Kelem/s 15.659 Kelem/s 15.678 Kelem/s]
                 change:
                        time:   [−6.4855% −6.2414% −6.0140%] (p = 0.00 < 0.05)
                        thrpt:  [+6.3989% +6.6569% +6.9353%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
capability_announcement/deserialize
                        time:   [10.284 µs 10.299 µs 10.314 µs]
                        thrpt:  [96.956 Kelem/s 97.095 Kelem/s 97.243 Kelem/s]
                 change:
                        time:   [−0.9933% −0.5909% −0.2427%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2433% +0.5944% +1.0033%]
                        Change within noise threshold.
Found 21 outliers among 100 measurements (21.00%)
  13 (13.00%) low mild
  3 (3.00%) high mild
  5 (5.00%) high severe
capability_announcement/is_expired
                        time:   [25.151 ns 25.160 ns 25.174 ns]
                        thrpt:  [39.724 Melem/s 39.746 Melem/s 39.760 Melem/s]
                 change:
                        time:   [−0.6826% −0.4308% −0.1911%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1915% +0.4327% +0.6873%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe

capability_filter/match_single_tag
                        time:   [43.505 ns 43.674 ns 43.881 ns]
                        thrpt:  [22.789 Melem/s 22.897 Melem/s 22.986 Melem/s]
                 change:
                        time:   [−15.703% −15.518% −15.300%] (p = 0.00 < 0.05)
                        thrpt:  [+18.063% +18.368% +18.629%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  3 (3.00%) high mild
  14 (14.00%) high severe
capability_filter/match_require_gpu
                        time:   [46.698 ns 46.744 ns 46.801 ns]
                        thrpt:  [21.367 Melem/s 21.393 Melem/s 21.414 Melem/s]
                 change:
                        time:   [−0.1789% +0.0112% +0.1912%] (p = 0.91 > 0.05)
                        thrpt:  [−0.1909% −0.0112% +0.1792%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe
capability_filter/match_gpu_vendor
                        time:   [141.61 ns 142.63 ns 143.73 ns]
                        thrpt:  [6.9573 Melem/s 7.0113 Melem/s 7.0616 Melem/s]
                 change:
                        time:   [−9.2598% −8.6473% −8.0574%] (p = 0.00 < 0.05)
                        thrpt:  [+8.7635% +9.4658% +10.205%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
capability_filter/match_min_memory
                        time:   [38.770 ns 38.805 ns 38.853 ns]
                        thrpt:  [25.738 Melem/s 25.770 Melem/s 25.793 Melem/s]
                 change:
                        time:   [+551.39% +553.80% +555.99%] (p = 0.00 < 0.05)
                        thrpt:  [−84.756% −84.705% −84.648%]
                        Performance has regressed.
Found 22 outliers among 100 measurements (22.00%)
  7 (7.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  9 (9.00%) high severe
capability_filter/match_complex
                        time:   [4.6177 µs 4.6443 µs 4.6695 µs]
                        thrpt:  [214.16 Kelem/s 215.32 Kelem/s 216.56 Kelem/s]
                 change:
                        time:   [−3.1545% −2.6281% −2.1008%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1458% +2.6991% +3.2573%]
                        Performance has improved.
capability_filter/match_no_match
                        time:   [83.334 ns 83.356 ns 83.385 ns]
                        thrpt:  [11.993 Melem/s 11.997 Melem/s 12.000 Melem/s]
                 change:
                        time:   [−7.9531% −3.0643% +0.3212%] (p = 0.23 > 0.05)
                        thrpt:  [−0.3201% +3.1612% +8.6402%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

capability_fold_insert/index_nodes/100
                        time:   [4.1265 ms 4.1293 ms 4.1322 ms]
                        thrpt:  [24.200 Kelem/s 24.217 Kelem/s 24.234 Kelem/s]
                 change:
                        time:   [+13.867% +13.982% +14.095%] (p = 0.00 < 0.05)
                        thrpt:  [−12.354% −12.267% −12.178%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
capability_fold_insert/index_nodes/1000
                        time:   [42.037 ms 42.116 ms 42.204 ms]
                        thrpt:  [23.694 Kelem/s 23.744 Kelem/s 23.789 Kelem/s]
                 change:
                        time:   [+13.980% +14.359% +14.745%] (p = 0.00 < 0.05)
                        thrpt:  [−12.850% −12.556% −12.265%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  6 (6.00%) high mild
  10 (10.00%) high severe
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 44.3s, or reduce sample count to 10.
capability_fold_insert/index_nodes/10000
                        time:   [438.65 ms 439.25 ms 439.92 ms]
                        thrpt:  [22.731 Kelem/s 22.766 Kelem/s 22.797 Kelem/s]
                 change:
                        time:   [+13.749% +14.159% +14.530%] (p = 0.00 < 0.05)
                        thrpt:  [−12.687% −12.403% −12.087%]
                        Performance has regressed.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

capability_fold_query/query_single_tag
                        time:   [175.72 µs 176.16 µs 176.64 µs]
                        thrpt:  [5.6612 Kelem/s 5.6768 Kelem/s 5.6908 Kelem/s]
                 change:
                        time:   [−4.0593% −2.8670% −1.8314%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8656% +2.9516% +4.2310%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
capability_fold_query/query_require_gpu
                        time:   [359.28 µs 362.78 µs 366.45 µs]
                        thrpt:  [2.7289 Kelem/s 2.7565 Kelem/s 2.7833 Kelem/s]
                 change:
                        time:   [−2.0772% −1.1444% −0.1960%] (p = 0.02 < 0.05)
                        thrpt:  [+0.1964% +1.1576% +2.1213%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  9 (9.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_gpu_vendor
                        time:   [621.72 µs 646.34 µs 673.47 µs]
                        thrpt:  [1.4848 Kelem/s 1.5472 Kelem/s 1.6084 Kelem/s]
                 change:
                        time:   [+4.8453% +8.7876% +13.591%] (p = 0.00 < 0.05)
                        thrpt:  [−11.965% −8.0778% −4.6214%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  9 (9.00%) high mild
  7 (7.00%) high severe
capability_fold_query/query_min_memory
                        time:   [490.76 µs 509.04 µs 529.28 µs]
                        thrpt:  [1.8894 Kelem/s 1.9645 Kelem/s 2.0377 Kelem/s]
                 change:
                        time:   [+2.9782% +6.3446% +9.7829%] (p = 0.00 < 0.05)
                        thrpt:  [−8.9111% −5.9661% −2.8920%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_fold_query/query_complex
                        time:   [450.73 µs 491.64 µs 533.55 µs]
                        thrpt:  [1.8742 Kelem/s 2.0340 Kelem/s 2.2186 Kelem/s]
                 change:
                        time:   [+23.341% +30.853% +39.391%] (p = 0.00 < 0.05)
                        thrpt:  [−28.260% −23.578% −18.924%]
                        Performance has regressed.
capability_fold_query/query_model
                        time:   [85.759 µs 86.151 µs 86.619 µs]
                        thrpt:  [11.545 Kelem/s 11.607 Kelem/s 11.661 Kelem/s]
                 change:
                        time:   [−2.1071% −1.3182% −0.7775%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7836% +1.3358% +2.1524%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe
capability_fold_query/query_tool
                        time:   [350.76 µs 351.98 µs 353.46 µs]
                        thrpt:  [2.8292 Kelem/s 2.8411 Kelem/s 2.8510 Kelem/s]
                 change:
                        time:   [−9.1220% −6.7667% −5.0977%] (p = 0.00 < 0.05)
                        thrpt:  [+5.3715% +7.2578% +10.038%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_no_results
                        time:   [118.28 ns 119.17 ns 120.13 ns]
                        thrpt:  [8.3241 Melem/s 8.3911 Melem/s 8.4544 Melem/s]
                 change:
                        time:   [+65.857% +66.703% +67.615%] (p = 0.00 < 0.05)
                        thrpt:  [−40.340% −40.013% −39.707%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  9 (9.00%) high mild
  7 (7.00%) high severe

capability_fold_find_best/find_best_simple
                        time:   [384.70 µs 401.55 µs 420.35 µs]
                        thrpt:  [2.3790 Kelem/s 2.4904 Kelem/s 2.5994 Kelem/s]
                 change:
                        time:   [−98.591% −98.547% −98.486%] (p = 0.00 < 0.05)
                        thrpt:  [+6506.9% +6780.0% +6997.5%]
                        Performance has improved.
capability_fold_find_best/find_best_with_prefs
                        time:   [545.94 µs 592.45 µs 636.36 µs]
                        thrpt:  [1.5714 Kelem/s 1.6879 Kelem/s 1.8317 Kelem/s]
                 change:
                        time:   [−95.977% −95.732% −95.501%] (p = 0.00 < 0.05)
                        thrpt:  [+2122.7% +2243.1% +2385.6%]
                        Performance has improved.

capability_fold_scaling/query_tag/1000
                        time:   [16.245 µs 16.254 µs 16.265 µs]
                        thrpt:  [61.482 Kelem/s 61.522 Kelem/s 61.558 Kelem/s]
                 change:
                        time:   [−98.778% −98.756% −98.733%] (p = 0.00 < 0.05)
                        thrpt:  [+7790.5% +7935.4% +8081.9%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
capability_fold_scaling/query_complex/1000
                        time:   [33.888 µs 34.039 µs 34.220 µs]
                        thrpt:  [29.223 Kelem/s 29.378 Kelem/s 29.509 Kelem/s]
                 change:
                        time:   [−97.376% −97.331% −97.285%] (p = 0.00 < 0.05)
                        thrpt:  [+3582.8% +3646.9% +3711.4%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  10 (10.00%) high severe
capability_fold_scaling/query_tag/5000
                        time:   [85.794 µs 85.949 µs 86.194 µs]
                        thrpt:  [11.602 Kelem/s 11.635 Kelem/s 11.656 Kelem/s]
                 change:
                        time:   [−98.869% −98.861% −98.853%] (p = 0.00 < 0.05)
                        thrpt:  [+8619.3% +8682.8% +8743.6%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  5 (5.00%) high severe
capability_fold_scaling/query_complex/5000
                        time:   [260.64 µs 281.46 µs 302.29 µs]
                        thrpt:  [3.3081 Kelem/s 3.5530 Kelem/s 3.8368 Kelem/s]
                 change:
                        time:   [−96.475% −96.236% −96.020%] (p = 0.00 < 0.05)
                        thrpt:  [+2412.7% +2557.1% +2736.9%]
                        Performance has improved.
capability_fold_scaling/query_tag/10000
                        time:   [202.06 µs 215.35 µs 230.16 µs]
                        thrpt:  [4.3448 Kelem/s 4.6436 Kelem/s 4.9490 Kelem/s]
                 change:
                        time:   [−98.580% −98.495% −98.411%] (p = 0.00 < 0.05)
                        thrpt:  [+6193.9% +6543.3% +6943.5%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  15 (15.00%) high mild
capability_fold_scaling/query_complex/10000
                        time:   [500.77 µs 536.43 µs 573.43 µs]
                        thrpt:  [1.7439 Kelem/s 1.8642 Kelem/s 1.9969 Kelem/s]
                 change:
                        time:   [−96.397% −96.181% −95.982%] (p = 0.00 < 0.05)
                        thrpt:  [+2388.8% +2518.6% +2675.3%]
                        Performance has improved.
capability_fold_scaling/query_tag/50000
                        time:   [969.66 µs 972.33 µs 975.84 µs]
                        thrpt:  [1.0248 Kelem/s 1.0285 Kelem/s 1.0313 Kelem/s]
                 change:
                        time:   [−98.723% −98.681% −98.585%] (p = 0.00 < 0.05)
                        thrpt:  [+6968.7% +7483.4% +7729.8%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
capability_fold_scaling/query_complex/50000
                        time:   [2.6930 ms 2.7465 ms 2.7994 ms]
                        thrpt:  [357.22  elem/s 364.10  elem/s 371.33  elem/s]
                 change:
                        time:   [−96.445% −96.369% −96.301%] (p = 0.00 < 0.05)
                        thrpt:  [+2603.7% +2653.8% +2713.3%]
                        Performance has improved.

capability_fold_concurrent/concurrent_index/4
                        time:   [15.333 ms 15.343 ms 15.355 ms]
                        thrpt:  [130.25 Kelem/s 130.35 Kelem/s 130.44 Kelem/s]
                 change:
                        time:   [−0.7212% −0.2815% +0.0870%] (p = 0.19 > 0.05)
                        thrpt:  [−0.0869% +0.2823% +0.7264%]
                        No change in performance detected.
Found 5 outliers among 20 measurements (25.00%)
  2 (10.00%) low mild
  3 (15.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.3s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/4
                        time:   [255.42 ms 262.93 ms 270.66 ms]
                        thrpt:  [7.3892 Kelem/s 7.6066 Kelem/s 7.8302 Kelem/s]
                 change:
                        time:   [−99.388% −99.369% −99.350%] (p = 0.00 < 0.05)
                        thrpt:  [+15280% +15751% +16242%]
                        Performance has improved.
capability_fold_concurrent/concurrent_mixed/4
                        time:   [99.349 ms 101.93 ms 104.98 ms]
                        thrpt:  [19.052 Kelem/s 19.622 Kelem/s 20.131 Kelem/s]
                 change:
                        time:   [−99.389% −99.360% −99.326%] (p = 0.00 < 0.05)
                        thrpt:  [+14747% +15519% +16274%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
capability_fold_concurrent/concurrent_index/8
                        time:   [16.361 ms 16.402 ms 16.450 ms]
                        thrpt:  [243.17 Kelem/s 243.87 Kelem/s 244.48 Kelem/s]
                 change:
                        time:   [−5.4145% −3.7864% −2.1282%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1745% +3.9354% +5.7245%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.0s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/8
                        time:   [224.49 ms 230.12 ms 236.42 ms]
                        thrpt:  [16.919 Kelem/s 17.382 Kelem/s 17.818 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
capability_fold_concurrent/concurrent_mixed/8
                        time:   [115.77 ms 116.40 ms 117.08 ms]
                        thrpt:  [34.165 Kelem/s 34.365 Kelem/s 34.550 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_index/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/16
                        time:   [34.261 ms 34.512 ms 34.784 ms]
                        thrpt:  [229.99 Kelem/s 231.80 Kelem/s 233.50 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.8s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/16
                        time:   [418.84 ms 427.96 ms 437.91 ms]
                        thrpt:  [18.268 Kelem/s 18.693 Kelem/s 19.100 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.7s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_mixed/16
                        time:   [271.23 ms 272.51 ms 273.51 ms]
                        thrpt:  [29.250 Kelem/s 29.357 Kelem/s 29.495 Kelem/s]
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) low severe
  3 (15.00%) low mild

capability_fold_updates/update_higher_version
                        time:   [29.620 µs 29.766 µs 29.902 µs]
                        thrpt:  [33.443 Kelem/s 33.596 Kelem/s 33.761 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_fold_updates/update_same_version
                        time:   [29.603 µs 29.755 µs 29.894 µs]
                        thrpt:  [33.451 Kelem/s 33.608 Kelem/s 33.780 Kelem/s]
capability_fold_updates/remove_and_readd
                        time:   [47.150 µs 47.578 µs 48.069 µs]
                        thrpt:  [20.804 Kelem/s 21.018 Kelem/s 21.209 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  11 (11.00%) high severe

location_info/create    time:   [59.249 ns 59.806 ns 60.387 ns]
                        thrpt:  [16.560 Melem/s 16.721 Melem/s 16.878 Melem/s]
location_info/distance_to
                        time:   [4.3209 ns 4.3223 ns 4.3237 ns]
                        thrpt:  [231.28 Melem/s 231.36 Melem/s 231.43 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe
location_info/same_continent
                        time:   [7.1396 ns 7.1490 ns 7.1674 ns]
                        thrpt:  [139.52 Melem/s 139.88 Melem/s 140.06 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe
location_info/same_continent_cross
                        time:   [310.36 ps 310.43 ps 310.51 ps]
                        thrpt:  [3.2205 Gelem/s 3.2213 Gelem/s 3.2220 Gelem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
location_info/same_region
                        time:   [4.0351 ns 4.0398 ns 4.0474 ns]
                        thrpt:  [247.07 Melem/s 247.54 Melem/s 247.82 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) high severe

topology_hints/create   time:   [3.1631 ns 3.1726 ns 3.1824 ns]
                        thrpt:  [314.23 Melem/s 315.20 Melem/s 316.14 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
topology_hints/connectivity_score
                        time:   [310.56 ps 311.01 ps 311.60 ps]
                        thrpt:  [3.2093 Gelem/s 3.2153 Gelem/s 3.2199 Gelem/s]
Found 19 outliers among 100 measurements (19.00%)
  5 (5.00%) high mild
  14 (14.00%) high severe
topology_hints/average_latency_empty
                        time:   [620.73 ps 620.89 ps 621.09 ps]
                        thrpt:  [1.6101 Gelem/s 1.6106 Gelem/s 1.6110 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
topology_hints/average_latency_100
                        time:   [70.374 ns 70.414 ns 70.463 ns]
                        thrpt:  [14.192 Melem/s 14.202 Melem/s 14.210 Melem/s]

nat_type/difficulty     time:   [310.44 ps 310.59 ps 310.77 ps]
                        thrpt:  [3.2178 Gelem/s 3.2197 Gelem/s 3.2212 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
nat_type/can_connect_direct
                        time:   [310.19 ps 310.33 ps 310.52 ps]
                        thrpt:  [3.2204 Gelem/s 3.2223 Gelem/s 3.2238 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
nat_type/can_connect_symmetric
                        time:   [310.12 ps 310.34 ps 310.67 ps]
                        thrpt:  [3.2189 Gelem/s 3.2223 Gelem/s 3.2246 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe

node_metadata/create_simple
                        time:   [50.814 ns 51.034 ns 51.368 ns]
                        thrpt:  [19.468 Melem/s 19.595 Melem/s 19.679 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
node_metadata/create_full
                        time:   [598.36 ns 600.85 ns 603.28 ns]
                        thrpt:  [1.6576 Melem/s 1.6643 Melem/s 1.6712 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
node_metadata/routing_score
                        time:   [2.8975 ns 2.8989 ns 2.9007 ns]
                        thrpt:  [344.74 Melem/s 344.96 Melem/s 345.13 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
node_metadata/age       time:   [27.416 ns 27.429 ns 27.447 ns]
                        thrpt:  [36.434 Melem/s 36.457 Melem/s 36.475 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
node_metadata/is_stale  time:   [25.753 ns 25.760 ns 25.769 ns]
                        thrpt:  [38.806 Melem/s 38.820 Melem/s 38.831 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
node_metadata/serialize time:   [769.32 ns 779.54 ns 791.30 ns]
                        thrpt:  [1.2637 Melem/s 1.2828 Melem/s 1.2999 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
node_metadata/deserialize
                        time:   [1.6736 µs 1.6869 µs 1.6995 µs]
                        thrpt:  [588.42 Kelem/s 592.81 Kelem/s 597.51 Kelem/s]

metadata_query/match_status
                        time:   [3.4174 ns 3.4206 ns 3.4245 ns]
                        thrpt:  [292.01 Melem/s 292.34 Melem/s 292.62 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  7 (7.00%) high mild
  7 (7.00%) high severe
metadata_query/match_min_tier
                        time:   [3.4152 ns 3.4254 ns 3.4384 ns]
                        thrpt:  [290.83 Melem/s 291.94 Melem/s 292.81 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
metadata_query/match_continent
                        time:   [11.209 ns 11.219 ns 11.231 ns]
                        thrpt:  [89.039 Melem/s 89.133 Melem/s 89.213 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
metadata_query/match_complex
                        time:   [10.567 ns 10.584 ns 10.607 ns]
                        thrpt:  [94.280 Melem/s 94.480 Melem/s 94.638 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
metadata_query/match_no_match
                        time:   [3.4301 ns 3.4387 ns 3.4485 ns]
                        thrpt:  [289.98 Melem/s 290.81 Melem/s 291.53 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  15 (15.00%) high mild
  5 (5.00%) high severe

metadata_store_basic/create
                        time:   [743.67 ns 745.11 ns 746.69 ns]
                        thrpt:  [1.3392 Melem/s 1.3421 Melem/s 1.3447 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
metadata_store_basic/upsert_new
                        time:   [2.0827 µs 2.1025 µs 2.1201 µs]
                        thrpt:  [471.67 Kelem/s 475.62 Kelem/s 480.14 Kelem/s]
metadata_store_basic/upsert_existing
                        time:   [1.3415 µs 1.3477 µs 1.3538 µs]
                        thrpt:  [738.68 Kelem/s 742.02 Kelem/s 745.45 Kelem/s]
metadata_store_basic/get
                        time:   [24.909 ns 25.778 ns 26.689 ns]
                        thrpt:  [37.468 Melem/s 38.792 Melem/s 40.146 Melem/s]
metadata_store_basic/get_miss
                        time:   [24.294 ns 25.052 ns 25.886 ns]
                        thrpt:  [38.631 Melem/s 39.916 Melem/s 41.162 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/len
                        time:   [199.71 ns 199.78 ns 199.89 ns]
                        thrpt:  [5.0028 Melem/s 5.0054 Melem/s 5.0072 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  8 (8.00%) high severe
Benchmarking metadata_store_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 20.1s, or reduce sample count to 20.
metadata_store_basic/stats
                        time:   [197.01 ms 197.28 ms 197.60 ms]
                        thrpt:  [5.0606  elem/s 5.0689  elem/s 5.0758  elem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

metadata_store_query/query_by_status
                        time:   [226.76 µs 236.46 µs 246.91 µs]
                        thrpt:  [4.0501 Kelem/s 4.2291 Kelem/s 4.4100 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
metadata_store_query/query_by_continent
                        time:   [146.43 µs 147.53 µs 148.88 µs]
                        thrpt:  [6.7168 Kelem/s 6.7783 Kelem/s 6.8292 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
metadata_store_query/query_by_tier
                        time:   [500.33 µs 509.67 µs 518.64 µs]
                        thrpt:  [1.9281 Kelem/s 1.9621 Kelem/s 1.9987 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
metadata_store_query/query_accepting_work
                        time:   [585.75 µs 607.89 µs 630.24 µs]
                        thrpt:  [1.5867 Kelem/s 1.6450 Kelem/s 1.7072 Kelem/s]
metadata_store_query/query_with_limit
                        time:   [528.41 µs 547.44 µs 566.57 µs]
                        thrpt:  [1.7650 Kelem/s 1.8267 Kelem/s 1.8925 Kelem/s]
metadata_store_query/query_complex
                        time:   [276.63 µs 277.37 µs 278.15 µs]
                        thrpt:  [3.5951 Kelem/s 3.6053 Kelem/s 3.6149 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe

metadata_store_spatial/find_nearby_100km
                        time:   [326.94 µs 327.17 µs 327.44 µs]
                        thrpt:  [3.0540 Kelem/s 3.0565 Kelem/s 3.0587 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
metadata_store_spatial/find_nearby_1000km
                        time:   [404.82 µs 407.62 µs 411.01 µs]
                        thrpt:  [2.4330 Kelem/s 2.4533 Kelem/s 2.4702 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
metadata_store_spatial/find_nearby_5000km
                        time:   [556.25 µs 565.92 µs 575.10 µs]
                        thrpt:  [1.7388 Kelem/s 1.7670 Kelem/s 1.7978 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
metadata_store_spatial/find_best_for_routing
                        time:   [322.24 µs 343.28 µs 365.68 µs]
                        thrpt:  [2.7347 Kelem/s 2.9131 Kelem/s 3.1032 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_spatial/find_relays
                        time:   [619.93 µs 630.36 µs 640.73 µs]
                        thrpt:  [1.5607 Kelem/s 1.5864 Kelem/s 1.6131 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

metadata_store_scaling/query_status/1000
                        time:   [18.752 µs 18.780 µs 18.808 µs]
                        thrpt:  [53.169 Kelem/s 53.247 Kelem/s 53.329 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  8 (8.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_complex/1000
                        time:   [21.216 µs 21.299 µs 21.393 µs]
                        thrpt:  [46.743 Kelem/s 46.951 Kelem/s 47.135 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
metadata_store_scaling/find_nearby/1000
                        time:   [58.123 µs 58.404 µs 58.717 µs]
                        thrpt:  [17.031 Kelem/s 17.122 Kelem/s 17.205 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  11 (11.00%) high severe
metadata_store_scaling/query_status/5000
                        time:   [97.944 µs 98.071 µs 98.226 µs]
                        thrpt:  [10.181 Kelem/s 10.197 Kelem/s 10.210 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
metadata_store_scaling/query_complex/5000
                        time:   [119.08 µs 119.18 µs 119.27 µs]
                        thrpt:  [8.3840 Kelem/s 8.3909 Kelem/s 8.3976 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/5000
                        time:   [403.57 µs 413.19 µs 424.01 µs]
                        thrpt:  [2.3584 Kelem/s 2.4202 Kelem/s 2.4779 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
metadata_store_scaling/query_status/10000
                        time:   [235.40 µs 252.53 µs 271.88 µs]
                        thrpt:  [3.6781 Kelem/s 3.9599 Kelem/s 4.2480 Kelem/s]
metadata_store_scaling/query_complex/10000
                        time:   [281.65 µs 295.73 µs 311.69 µs]
                        thrpt:  [3.2083 Kelem/s 3.3815 Kelem/s 3.5505 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/10000
                        time:   [611.55 µs 623.72 µs 636.28 µs]
                        thrpt:  [1.5716 Kelem/s 1.6033 Kelem/s 1.6352 Kelem/s]
metadata_store_scaling/query_status/50000
                        time:   [2.5445 ms 2.5527 ms 2.5616 ms]
                        thrpt:  [390.38  elem/s 391.75  elem/s 393.00  elem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
metadata_store_scaling/query_complex/50000
                        time:   [2.8261 ms 2.8365 ms 2.8474 ms]
                        thrpt:  [351.20  elem/s 352.55  elem/s 353.84  elem/s]
Found 17 outliers among 100 measurements (17.00%)
  14 (14.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/50000
                        time:   [3.3064 ms 3.3158 ms 3.3244 ms]
                        thrpt:  [300.80  elem/s 301.59  elem/s 302.45  elem/s]
Found 14 outliers among 100 measurements (14.00%)
  10 (10.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

metadata_store_concurrent/concurrent_upsert/4
                        time:   [1.6148 ms 1.6280 ms 1.6432 ms]
                        thrpt:  [1.2171 Melem/s 1.2285 Melem/s 1.2385 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.7s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/4
                        time:   [278.60 ms 284.50 ms 290.89 ms]
                        thrpt:  [6.8754 Kelem/s 7.0298 Kelem/s 7.1788 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
metadata_store_concurrent/concurrent_mixed/4
                        time:   [185.91 ms 189.45 ms 193.56 ms]
                        thrpt:  [10.333 Kelem/s 10.557 Kelem/s 10.758 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
metadata_store_concurrent/concurrent_upsert/8
                        time:   [4.4655 ms 4.4747 ms 4.4856 ms]
                        thrpt:  [891.75 Kelem/s 893.91 Kelem/s 895.76 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 16.4s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/8
                        time:   [810.98 ms 814.97 ms 819.13 ms]
                        thrpt:  [4.8832 Kelem/s 4.9082 Kelem/s 4.9323 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 17.7s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/8
                        time:   [891.12 ms 899.60 ms 909.14 ms]
                        thrpt:  [4.3997 Kelem/s 4.4464 Kelem/s 4.4887 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
  1 (5.00%) high severe
metadata_store_concurrent/concurrent_upsert/16
                        time:   [10.018 ms 10.030 ms 10.043 ms]
                        thrpt:  [796.56 Kelem/s 797.58 Kelem/s 798.60 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 30.2s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/16
                        time:   [1.6075 s 1.6446 s 1.6912 s]
                        thrpt:  [4.7303 Kelem/s 4.8644 Kelem/s 4.9766 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) low severe
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 34.6s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/16
                        time:   [1.7392 s 1.7485 s 1.7622 s]
                        thrpt:  [4.5398 Kelem/s 4.5754 Kelem/s 4.5997 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

metadata_store_versioning/update_versioned_success
                        time:   [271.15 ns 274.32 ns 277.37 ns]
                        thrpt:  [3.6053 Melem/s 3.6454 Melem/s 3.6880 Melem/s]
metadata_store_versioning/update_versioned_conflict
                        time:   [268.52 ns 270.39 ns 272.43 ns]
                        thrpt:  [3.6707 Melem/s 3.6983 Melem/s 3.7241 Melem/s]

schema_validation/validate_string
                        time:   [3.4177 ns 3.4189 ns 3.4203 ns]
                        thrpt:  [292.38 Melem/s 292.49 Melem/s 292.60 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
schema_validation/validate_integer
                        time:   [3.4142 ns 3.4153 ns 3.4168 ns]
                        thrpt:  [292.68 Melem/s 292.80 Melem/s 292.89 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
schema_validation/validate_object
                        time:   [75.813 ns 75.920 ns 76.038 ns]
                        thrpt:  [13.151 Melem/s 13.172 Melem/s 13.190 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
schema_validation/validate_array_10
                        time:   [36.014 ns 36.051 ns 36.103 ns]
                        thrpt:  [27.699 Melem/s 27.739 Melem/s 27.767 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
schema_validation/validate_complex
                        time:   [201.95 ns 202.28 ns 202.67 ns]
                        thrpt:  [4.9342 Melem/s 4.9437 Melem/s 4.9516 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe

endpoint_matching/match_success
                        time:   [278.74 ns 280.53 ns 282.32 ns]
                        thrpt:  [3.5421 Melem/s 3.5647 Melem/s 3.5875 Melem/s]
endpoint_matching/match_failure
                        time:   [280.01 ns 280.53 ns 281.03 ns]
                        thrpt:  [3.5584 Melem/s 3.5647 Melem/s 3.5714 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
endpoint_matching/match_multi_param
                        time:   [645.72 ns 656.29 ns 667.43 ns]
                        thrpt:  [1.4983 Melem/s 1.5237 Melem/s 1.5487 Melem/s]

api_version/is_compatible_with
                        time:   [310.37 ps 310.44 ps 310.53 ps]
                        thrpt:  [3.2203 Gelem/s 3.2212 Gelem/s 3.2220 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
api_version/parse       time:   [38.675 ns 38.718 ns 38.771 ns]
                        thrpt:  [25.792 Melem/s 25.827 Melem/s 25.856 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
api_version/to_string   time:   [49.677 ns 49.707 ns 49.747 ns]
                        thrpt:  [20.102 Melem/s 20.118 Melem/s 20.130 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe

api_schema/create       time:   [2.1653 µs 2.1713 µs 2.1772 µs]
                        thrpt:  [459.31 Kelem/s 460.56 Kelem/s 461.84 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
api_schema/serialize    time:   [2.0609 µs 2.0683 µs 2.0755 µs]
                        thrpt:  [481.80 Kelem/s 483.48 Kelem/s 485.23 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
api_schema/deserialize  time:   [6.7346 µs 6.7519 µs 6.7685 µs]
                        thrpt:  [147.74 Kelem/s 148.11 Kelem/s 148.49 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
api_schema/find_endpoint
                        time:   [297.18 ns 299.56 ns 302.04 ns]
                        thrpt:  [3.3108 Melem/s 3.3382 Melem/s 3.3649 Melem/s]
api_schema/endpoints_by_tag
                        time:   [120.05 ns 121.10 ns 122.14 ns]
                        thrpt:  [8.1874 Melem/s 8.2578 Melem/s 8.3301 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  9 (9.00%) low mild
  1 (1.00%) high mild

request_validation/validate_full_request
                        time:   [71.940 ns 72.096 ns 72.314 ns]
                        thrpt:  [13.829 Melem/s 13.870 Melem/s 13.901 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
request_validation/validate_path_only
                        time:   [21.419 ns 21.575 ns 21.733 ns]
                        thrpt:  [46.012 Melem/s 46.351 Melem/s 46.688 Melem/s]

api_registry_basic/create
                        time:   [402.21 ns 404.50 ns 406.68 ns]
                        thrpt:  [2.4589 Melem/s 2.4722 Melem/s 2.4863 Melem/s]
api_registry_basic/register_new
                        time:   [4.5513 µs 4.6953 µs 4.8390 µs]
                        thrpt:  [206.66 Kelem/s 212.98 Kelem/s 219.72 Kelem/s]
api_registry_basic/get  time:   [25.910 ns 26.948 ns 27.944 ns]
                        thrpt:  [35.786 Melem/s 37.109 Melem/s 38.595 Melem/s]
api_registry_basic/len  time:   [199.27 ns 199.34 ns 199.44 ns]
                        thrpt:  [5.0140 Melem/s 5.0167 Melem/s 5.0184 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
Benchmarking api_registry_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 18.3s, or reduce sample count to 20.
api_registry_basic/stats
                        time:   [179.81 ms 180.01 ms 180.25 ms]
                        thrpt:  [5.5479  elem/s 5.5553  elem/s 5.5615  elem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe

api_registry_query/query_by_name
                        time:   [93.073 µs 93.744 µs 94.606 µs]
                        thrpt:  [10.570 Kelem/s 10.667 Kelem/s 10.744 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
api_registry_query/query_by_tag
                        time:   [750.84 µs 769.43 µs 788.66 µs]
                        thrpt:  [1.2680 Kelem/s 1.2997 Kelem/s 1.3318 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
api_registry_query/query_with_version
                        time:   [56.555 µs 56.594 µs 56.638 µs]
                        thrpt:  [17.656 Kelem/s 17.670 Kelem/s 17.682 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  11 (11.00%) high mild
  9 (9.00%) high severe
api_registry_query/find_by_endpoint
                        time:   [4.5567 ms 4.6157 ms 4.6802 ms]
                        thrpt:  [213.67  elem/s 216.65  elem/s 219.46  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_registry_query/find_compatible
                        time:   [64.065 µs 64.135 µs 64.223 µs]
                        thrpt:  [15.571 Kelem/s 15.592 Kelem/s 15.609 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) high mild
  16 (16.00%) high severe

api_registry_scaling/query_by_name/1000
                        time:   [7.5146 µs 7.5226 µs 7.5312 µs]
                        thrpt:  [132.78 Kelem/s 132.93 Kelem/s 133.08 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_tag/1000
                        time:   [45.852 µs 46.031 µs 46.207 µs]
                        thrpt:  [21.642 Kelem/s 21.725 Kelem/s 21.809 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
api_registry_scaling/query_by_name/5000
                        time:   [44.448 µs 44.797 µs 45.167 µs]
                        thrpt:  [22.140 Kelem/s 22.323 Kelem/s 22.498 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  8 (8.00%) high mild
  7 (7.00%) high severe
api_registry_scaling/query_by_tag/5000
                        time:   [384.11 µs 396.07 µs 409.17 µs]
                        thrpt:  [2.4439 Kelem/s 2.5248 Kelem/s 2.6034 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_name/10000
                        time:   [89.807 µs 90.224 µs 90.687 µs]
                        thrpt:  [11.027 Kelem/s 11.084 Kelem/s 11.135 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_tag/10000
                        time:   [726.17 µs 747.08 µs 768.60 µs]
                        thrpt:  [1.3011 Kelem/s 1.3385 Kelem/s 1.3771 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 18.4s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/4
                        time:   [904.13 ms 909.25 ms 914.60 ms]
                        thrpt:  [2.1867 Kelem/s 2.1996 Kelem/s 2.2121 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 12.4s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/4
                        time:   [543.93 ms 564.88 ms 586.63 ms]
                        thrpt:  [3.4093 Kelem/s 3.5406 Kelem/s 3.6770 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 24.1s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/8
                        time:   [1.1738 s 1.1895 s 1.2028 s]
                        thrpt:  [3.3257 Kelem/s 3.3628 Kelem/s 3.4077 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 20.5s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/8
                        time:   [1.0239 s 1.0368 s 1.0491 s]
                        thrpt:  [3.8129 Kelem/s 3.8579 Kelem/s 3.9066 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 39.5s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/16
                        time:   [1.9687 s 1.9748 s 1.9813 s]
                        thrpt:  [4.0377 Kelem/s 4.0511 Kelem/s 4.0636 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 37.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/16
                        time:   [1.8633 s 1.8748 s 1.8887 s]
                        thrpt:  [4.2357 Kelem/s 4.2672 Kelem/s 4.2935 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

compare_op/eq           time:   [1.9766 ns 1.9775 ns 1.9788 ns]
                        thrpt:  [505.36 Melem/s 505.69 Melem/s 505.92 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low severe
  6 (6.00%) high mild
  8 (8.00%) high severe
compare_op/gt           time:   [2.0385 ns 2.0422 ns 2.0458 ns]
                        thrpt:  [488.81 Melem/s 489.67 Melem/s 490.55 Melem/s]
compare_op/contains_string
                        time:   [24.870 ns 24.900 ns 24.935 ns]
                        thrpt:  [40.104 Melem/s 40.160 Melem/s 40.209 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
compare_op/in_array     time:   [6.8779 ns 6.9000 ns 6.9263 ns]
                        thrpt:  [144.38 Melem/s 144.93 Melem/s 145.39 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

condition/simple        time:   [56.105 ns 56.206 ns 56.306 ns]
                        thrpt:  [17.760 Melem/s 17.792 Melem/s 17.824 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
condition/nested_field  time:   [919.52 ns 931.09 ns 943.64 ns]
                        thrpt:  [1.0597 Melem/s 1.0740 Melem/s 1.0875 Melem/s]
condition/string_eq     time:   [96.066 ns 97.269 ns 98.511 ns]
                        thrpt:  [10.151 Melem/s 10.281 Melem/s 10.409 Melem/s]

condition_expr/single   time:   [56.496 ns 56.564 ns 56.643 ns]
                        thrpt:  [17.655 Melem/s 17.679 Melem/s 17.700 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
condition_expr/and_2    time:   [114.79 ns 115.27 ns 115.80 ns]
                        thrpt:  [8.6359 Melem/s 8.6749 Melem/s 8.7119 Melem/s]
condition_expr/and_5    time:   [397.97 ns 399.95 ns 401.72 ns]
                        thrpt:  [2.4893 Melem/s 2.5003 Melem/s 2.5128 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
condition_expr/or_3     time:   [226.49 ns 227.82 ns 229.20 ns]
                        thrpt:  [4.3630 Melem/s 4.3894 Melem/s 4.4152 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
condition_expr/nested   time:   [173.43 ns 175.09 ns 176.72 ns]
                        thrpt:  [5.6587 Melem/s 5.7113 Melem/s 5.7660 Melem/s]

rule/create             time:   [563.85 ns 568.04 ns 571.87 ns]
                        thrpt:  [1.7486 Melem/s 1.7604 Melem/s 1.7735 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild
rule/matches            time:   [113.49 ns 113.99 ns 114.51 ns]
                        thrpt:  [8.7329 Melem/s 8.7724 Melem/s 8.8112 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild

rule_context/create     time:   [1.4507 µs 1.4568 µs 1.4625 µs]
                        thrpt:  [683.77 Kelem/s 686.44 Kelem/s 689.32 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
rule_context/get_simple time:   [54.630 ns 54.681 ns 54.740 ns]
                        thrpt:  [18.268 Melem/s 18.288 Melem/s 18.305 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
rule_context/get_nested time:   [930.28 ns 932.65 ns 935.42 ns]
                        thrpt:  [1.0690 Melem/s 1.0722 Melem/s 1.0749 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  9 (9.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
rule_context/get_deep_nested
                        time:   [901.80 ns 907.49 ns 913.75 ns]
                        thrpt:  [1.0944 Melem/s 1.1019 Melem/s 1.1089 Melem/s]

rule_engine_basic/create
                        time:   [8.1436 ns 8.1584 ns 8.1755 ns]
                        thrpt:  [122.32 Melem/s 122.57 Melem/s 122.80 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
rule_engine_basic/add_rule
                        time:   [2.8201 µs 2.9689 µs 3.0944 µs]
                        thrpt:  [323.17 Kelem/s 336.83 Kelem/s 354.59 Kelem/s]
rule_engine_basic/get_rule
                        time:   [20.702 ns 21.642 ns 22.522 ns]
                        thrpt:  [44.401 Melem/s 46.206 Melem/s 48.304 Melem/s]
rule_engine_basic/rules_by_tag
                        time:   [1.1716 µs 1.1761 µs 1.1809 µs]
                        thrpt:  [846.79 Kelem/s 850.24 Kelem/s 853.54 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
rule_engine_basic/stats time:   [8.0224 µs 8.0285 µs 8.0350 µs]
                        thrpt:  [124.46 Kelem/s 124.56 Kelem/s 124.65 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe

rule_engine_evaluate/evaluate_10_rules
                        time:   [3.6204 µs 3.6493 µs 3.6767 µs]
                        thrpt:  [271.98 Kelem/s 274.03 Kelem/s 276.21 Kelem/s]
rule_engine_evaluate/evaluate_first_10_rules
                        time:   [439.02 ns 446.24 ns 454.02 ns]
                        thrpt:  [2.2026 Melem/s 2.2409 Melem/s 2.2778 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild
rule_engine_evaluate/evaluate_100_rules
                        time:   [36.409 µs 36.637 µs 36.857 µs]
                        thrpt:  [27.132 Kelem/s 27.295 Kelem/s 27.466 Kelem/s]
rule_engine_evaluate/evaluate_first_100_rules
                        time:   [439.94 ns 445.47 ns 451.67 ns]
                        thrpt:  [2.2140 Melem/s 2.2448 Melem/s 2.2731 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  8 (8.00%) high mild
  8 (8.00%) high severe
rule_engine_evaluate/evaluate_matching_100_rules
                        time:   [35.913 µs 36.020 µs 36.137 µs]
                        thrpt:  [27.672 Kelem/s 27.763 Kelem/s 27.845 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_1000_rules
                        time:   [552.53 µs 556.03 µs 559.50 µs]
                        thrpt:  [1.7873 Kelem/s 1.7985 Kelem/s 1.8098 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
rule_engine_evaluate/evaluate_first_1000_rules
                        time:   [422.55 ns 430.75 ns 439.82 ns]
                        thrpt:  [2.2736 Melem/s 2.3215 Melem/s 2.3666 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  13 (13.00%) high mild

rule_engine_scaling/evaluate/10
                        time:   [3.5362 µs 3.5486 µs 3.5619 µs]
                        thrpt:  [280.75 Kelem/s 281.80 Kelem/s 282.79 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
rule_engine_scaling/evaluate_first/10
                        time:   [409.62 ns 414.57 ns 419.46 ns]
                        thrpt:  [2.3840 Melem/s 2.4121 Melem/s 2.4413 Melem/s]
rule_engine_scaling/evaluate/50
                        time:   [17.633 µs 17.692 µs 17.755 µs]
                        thrpt:  [56.322 Kelem/s 56.523 Kelem/s 56.711 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
rule_engine_scaling/evaluate_first/50
                        time:   [409.46 ns 413.19 ns 416.77 ns]
                        thrpt:  [2.3994 Melem/s 2.4202 Melem/s 2.4422 Melem/s]
rule_engine_scaling/evaluate/100
                        time:   [35.690 µs 35.846 µs 36.000 µs]
                        thrpt:  [27.778 Kelem/s 27.897 Kelem/s 28.019 Kelem/s]
rule_engine_scaling/evaluate_first/100
                        time:   [394.80 ns 399.66 ns 404.54 ns]
                        thrpt:  [2.4719 Melem/s 2.5021 Melem/s 2.5329 Melem/s]
rule_engine_scaling/evaluate/500
                        time:   [247.13 µs 267.94 µs 288.60 µs]
                        thrpt:  [3.4650 Kelem/s 3.7321 Kelem/s 4.0465 Kelem/s]
rule_engine_scaling/evaluate_first/500
                        time:   [401.49 ns 405.85 ns 410.22 ns]
                        thrpt:  [2.4377 Melem/s 2.4640 Melem/s 2.4907 Melem/s]
rule_engine_scaling/evaluate/1000
                        time:   [552.92 µs 557.06 µs 561.21 µs]
                        thrpt:  [1.7819 Kelem/s 1.7951 Kelem/s 1.8086 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
rule_engine_scaling/evaluate_first/1000
                        time:   [411.06 ns 413.99 ns 416.90 ns]
                        thrpt:  [2.3987 Melem/s 2.4155 Melem/s 2.4327 Melem/s]

rule_set/create         time:   [6.0436 µs 6.0669 µs 6.0939 µs]
                        thrpt:  [164.10 Kelem/s 164.83 Kelem/s 165.46 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_set/load_into_engine
                        time:   [10.499 µs 10.539 µs 10.579 µs]
                        thrpt:  [94.524 Kelem/s 94.885 Kelem/s 95.246 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

trace_id/generate       time:   [537.00 ns 537.98 ns 538.99 ns]
                        thrpt:  [1.8553 Melem/s 1.8588 Melem/s 1.8622 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
trace_id/to_hex         time:   [107.33 ns 107.71 ns 108.10 ns]
                        thrpt:  [9.2510 Melem/s 9.2838 Melem/s 9.3170 Melem/s]
trace_id/from_hex       time:   [23.180 ns 23.222 ns 23.274 ns]
                        thrpt:  [42.966 Melem/s 43.062 Melem/s 43.140 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe

context_operations/create
                        time:   [822.55 ns 825.96 ns 830.11 ns]
                        thrpt:  [1.2047 Melem/s 1.2107 Melem/s 1.2157 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
context_operations/child
                        time:   [282.12 ns 282.59 ns 283.10 ns]
                        thrpt:  [3.5323 Melem/s 3.5386 Melem/s 3.5446 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  8 (8.00%) high mild
  7 (7.00%) high severe
context_operations/for_remote
                        time:   [288.31 ns 289.46 ns 290.60 ns]
                        thrpt:  [3.4411 Melem/s 3.4547 Melem/s 3.4685 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
context_operations/to_traceparent
                        time:   [352.80 ns 357.15 ns 361.18 ns]
                        thrpt:  [2.7687 Melem/s 2.8000 Melem/s 2.8345 Melem/s]
context_operations/from_traceparent
                        time:   [384.56 ns 386.39 ns 388.32 ns]
                        thrpt:  [2.5752 Melem/s 2.5880 Melem/s 2.6004 Melem/s]

baggage/create          time:   [2.0556 ns 2.0620 ns 2.0688 ns]
                        thrpt:  [483.36 Melem/s 484.96 Melem/s 486.48 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
baggage/get             time:   [20.378 ns 20.928 ns 21.427 ns]
                        thrpt:  [46.670 Melem/s 47.784 Melem/s 49.073 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild
baggage/set             time:   [80.082 ns 80.334 ns 80.578 ns]
                        thrpt:  [12.410 Melem/s 12.448 Melem/s 12.487 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
baggage/merge           time:   [1.6293 µs 1.6365 µs 1.6446 µs]
                        thrpt:  [608.03 Kelem/s 611.08 Kelem/s 613.77 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

span/create             time:   [342.19 ns 342.92 ns 343.55 ns]
                        thrpt:  [2.9108 Melem/s 2.9162 Melem/s 2.9223 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low severe
  7 (7.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
span/set_attribute      time:   [72.236 ns 72.684 ns 73.157 ns]
                        thrpt:  [13.669 Melem/s 13.758 Melem/s 13.843 Melem/s]
span/add_event          time:   [45.553 ns 45.724 ns 45.890 ns]
                        thrpt:  [21.791 Melem/s 21.870 Melem/s 21.953 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
span/with_kind          time:   [338.25 ns 339.81 ns 341.30 ns]
                        thrpt:  [2.9300 Melem/s 2.9428 Melem/s 2.9564 Melem/s]

context_store/create_context
                        time:   [979.19 ns 982.19 ns 985.61 ns]
                        thrpt:  [1.0146 Melem/s 1.0181 Melem/s 1.0213 Melem/s]
                 change:
                        time:   [−98.985% −98.982% −98.979%] (p = 0.00 < 0.05)
                        thrpt:  [+9695.1% +9722.4% +9750.7%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
context_store/get_context
                        time:   [50.684 ns 50.741 ns 50.800 ns]
                        thrpt:  [19.685 Melem/s 19.708 Melem/s 19.730 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
context_store/add_span  time:   [395.82 ns 397.10 ns 398.37 ns]
                        thrpt:  [2.5102 Melem/s 2.5183 Melem/s 2.5264 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  11 (11.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

propagation_context/from_context
                        time:   [854.01 ns 857.80 ns 861.29 ns]
                        thrpt:  [1.1610 Melem/s 1.1658 Melem/s 1.1709 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
propagation_context/to_context
                        time:   [919.66 ns 923.37 ns 927.03 ns]
                        thrpt:  [1.0787 Melem/s 1.0830 Melem/s 1.0874 Melem/s]

context_store_concurrent/concurrent_get
                        time:   [58.813 ns 58.860 ns 58.919 ns]
                        thrpt:  [16.973 Melem/s 16.989 Melem/s 17.003 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) high mild
  12 (12.00%) high severe

endpoint/create         time:   [3.0523 ns 3.0531 ns 3.0543 ns]
                        thrpt:  [327.41 Melem/s 327.53 Melem/s 327.63 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
endpoint/create_with_config
                        time:   [107.71 ns 108.54 ns 109.50 ns]
                        thrpt:  [9.1324 Melem/s 9.2129 Melem/s 9.2844 Melem/s]
endpoint/effective_weight
                        time:   [310.31 ps 310.41 ps 310.54 ps]
                        thrpt:  [3.2202 Gelem/s 3.2215 Gelem/s 3.2225 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe

load_metrics/load_score time:   [310.64 ps 311.27 ps 312.09 ps]
                        thrpt:  [3.2042 Gelem/s 3.2126 Gelem/s 3.2192 Gelem/s]
Found 19 outliers among 100 measurements (19.00%)
  2 (2.00%) high mild
  17 (17.00%) high severe
load_metrics/is_overloaded
                        time:   [310.29 ps 310.37 ps 310.48 ps]
                        thrpt:  [3.2208 Gelem/s 3.2219 Gelem/s 3.2228 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe

lb_strategies/round_robin
                        time:   [1.9372 µs 1.9444 µs 1.9519 µs]
                        thrpt:  [512.31 Kelem/s 514.28 Kelem/s 516.21 Kelem/s]
lb_strategies/weighted_round_robin
                        time:   [1.9634 µs 1.9672 µs 1.9713 µs]
                        thrpt:  [507.29 Kelem/s 508.33 Kelem/s 509.33 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
lb_strategies/least_connections
                        time:   [1.9255 µs 1.9278 µs 1.9301 µs]
                        thrpt:  [518.12 Kelem/s 518.74 Kelem/s 519.35 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
lb_strategies/random    time:   [2.2207 µs 2.2242 µs 2.2276 µs]
                        thrpt:  [448.92 Kelem/s 449.60 Kelem/s 450.31 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
lb_strategies/power_of_two
                        time:   [2.5137 µs 2.5171 µs 2.5206 µs]
                        thrpt:  [396.73 Kelem/s 397.29 Kelem/s 397.83 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
lb_strategies/consistent_hash
                        time:   [81.110 µs 86.808 µs 92.756 µs]
                        thrpt:  [10.781 Kelem/s 11.520 Kelem/s 12.329 Kelem/s]
lb_strategies/least_load
                        time:   [2.1424 µs 2.1468 µs 2.1525 µs]
                        thrpt:  [464.58 Kelem/s 465.80 Kelem/s 466.77 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

lb_scaling/select/10    time:   [1.9239 µs 1.9303 µs 1.9365 µs]
                        thrpt:  [516.40 Kelem/s 518.06 Kelem/s 519.78 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
lb_scaling/select/50    time:   [2.4740 µs 2.4754 µs 2.4772 µs]
                        thrpt:  [403.69 Kelem/s 403.97 Kelem/s 404.20 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  2 (2.00%) high mild
  2 (2.00%) high severe
lb_scaling/select/100   time:   [3.0845 µs 3.0908 µs 3.0968 µs]
                        thrpt:  [322.92 Kelem/s 323.54 Kelem/s 324.20 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
lb_scaling/select/500   time:   [5.3662 µs 5.3718 µs 5.3780 µs]
                        thrpt:  [185.94 Kelem/s 186.16 Kelem/s 186.35 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe

lb_zone_aware/zone_match
                        time:   [1.9842 µs 1.9881 µs 1.9925 µs]
                        thrpt:  [501.88 Kelem/s 503.00 Kelem/s 503.99 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  9 (9.00%) high severe
lb_zone_aware/zone_fallback
                        time:   [1.9372 µs 1.9423 µs 1.9470 µs]
                        thrpt:  [513.60 Kelem/s 514.85 Kelem/s 516.21 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

lb_health_updates/update_health
                        time:   [25.936 ns 26.248 ns 26.617 ns]
                        thrpt:  [37.571 Melem/s 38.098 Melem/s 38.557 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe
lb_health_updates/update_metrics
                        time:   [131.47 ns 133.57 ns 135.16 ns]
                        thrpt:  [7.3989 Melem/s 7.4868 Melem/s 7.6062 Melem/s]
Found 28 outliers among 100 measurements (28.00%)
  16 (16.00%) low severe
  1 (1.00%) low mild
  11 (11.00%) high mild

redex_append_inline/heap_file
                        time:   [34.813 ns 34.884 ns 34.963 ns]
                        thrpt:  [28.602 Melem/s 28.666 Melem/s 28.725 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe

redex_append_heap/heap_file/32
                        time:   [40.038 ns 40.557 ns 41.047 ns]
                        thrpt:  [743.48 MiB/s 752.47 MiB/s 762.21 MiB/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  14 (14.00%) high mild
  3 (3.00%) high severe
redex_append_heap/heap_file/256
                        time:   [67.491 ns 68.193 ns 69.025 ns]
                        thrpt:  [3.4541 GiB/s 3.4962 GiB/s 3.5326 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild
redex_append_heap/heap_file/1024
                        time:   [167.83 ns 169.92 ns 172.28 ns]
                        thrpt:  [5.5354 GiB/s 5.6124 GiB/s 5.6824 GiB/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  7 (7.00%) high mild
  1 (1.00%) high severe

redex_append_watcher_paths/no_watchers
                        time:   [67.537 ns 68.244 ns 69.102 ns]
                        thrpt:  [3.4502 GiB/s 3.4936 GiB/s 3.5302 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
redex_append_watcher_paths/with_tail
                        time:   [197.83 ns 199.61 ns 201.18 ns]
                        thrpt:  [1.1851 GiB/s 1.1944 GiB/s 1.2051 GiB/s]

redex_append_batch/batch_64_x_64B
                        time:   [1.5662 µs 1.5744 µs 1.5840 µs]
                        thrpt:  [40.404 Melem/s 40.651 Melem/s 40.864 Melem/s]
Found 24 outliers among 100 measurements (24.00%)
  4 (4.00%) low severe
  13 (13.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe

redex_append_disk/disk_file/32
                        time:   [5.5633 µs 5.5750 µs 5.5897 µs]
                        thrpt:  [5.4596 MiB/s 5.4740 MiB/s 5.4855 MiB/s]
Found 17 outliers among 100 measurements (17.00%)
  5 (5.00%) high mild
  12 (12.00%) high severe
redex_append_disk/disk_file/256
                        time:   [5.8123 µs 5.8286 µs 5.8489 µs]
                        thrpt:  [41.742 MiB/s 41.886 MiB/s 42.004 MiB/s]
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
redex_append_disk/disk_file/1024
                        time:   [6.8236 µs 6.9048 µs 7.0039 µs]
                        thrpt:  [139.43 MiB/s 141.43 MiB/s 143.11 MiB/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe

redex_append_batch_disk/batch_64_x/64
                        time:   [13.723 µs 13.778 µs 13.842 µs]
                        thrpt:  [4.6236 Melem/s 4.6451 Melem/s 4.6638 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) high mild
  12 (12.00%) high severe
redex_append_batch_disk/batch_64_x/1024
                        time:   [37.721 µs 38.098 µs 38.542 µs]
                        thrpt:  [1.6605 Melem/s 1.6799 Melem/s 1.6967 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

redex_append_disk_policies/disk_file_256B/never
                        time:   [5.8669 µs 5.9459 µs 6.0532 µs]
                        thrpt:  [40.332 MiB/s 41.060 MiB/s 41.613 MiB/s]
Found 19 outliers among 100 measurements (19.00%)
  5 (5.00%) high mild
  14 (14.00%) high severe
redex_append_disk_policies/disk_file_256B/every_n_1
                        time:   [1.4786 ms 1.6955 ms 1.9057 ms]
                        thrpt:  [131.19 KiB/s 147.45 KiB/s 169.08 KiB/s]
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) low mild
redex_append_disk_policies/disk_file_256B/every_n_64
                        time:   [206.69 µs 216.07 µs 224.09 µs]
                        thrpt:  [1.0895 MiB/s 1.1299 MiB/s 1.1812 MiB/s]
redex_append_disk_policies/disk_file_256B/interval_50ms
                        time:   [6.6331 µs 6.8998 µs 7.1406 µs]
                        thrpt:  [34.191 MiB/s 35.384 MiB/s 36.806 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
redex_append_disk_policies/disk_file_256B/interval_or_bytes
                        time:   [8.7902 µs 9.0869 µs 9.3763 µs]
                        thrpt:  [26.038 MiB/s 26.867 MiB/s 27.774 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

redex_append_batch_disk_policies/batch_64_x_64B/never
                        time:   [13.807 µs 13.856 µs 13.911 µs]
                        thrpt:  [4.6006 Melem/s 4.6191 Melem/s 4.6353 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe
Benchmarking redex_append_batch_disk_policies/batch_64_x_64B/every_n_1: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.5s, enable flat sampling, or reduce sample count to 60.
redex_append_batch_disk_policies/batch_64_x_64B/every_n_1
                        time:   [1.7668 ms 2.0690 ms 2.3670 ms]
                        thrpt:  [27.038 Kelem/s 30.933 Kelem/s 36.224 Kelem/s]
redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small
                        time:   [2.1750 ms 2.4193 ms 2.6341 ms]
                        thrpt:  [24.297 Kelem/s 26.454 Kelem/s 29.426 Kelem/s]
Found 19 outliers among 100 measurements (19.00%)
  19 (19.00%) low mild

redex_tail/append_to_next
                        time:   [160.23 ns 160.90 ns 161.57 ns]
                        thrpt:  [6.1893 Melem/s 6.2149 Melem/s 6.2410 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

standard_placement_score/baseline_no_custom_filter/100
                        time:   [64.362 µs 64.986 µs 65.573 µs]
                        thrpt:  [1.5250 Melem/s 1.5388 Melem/s 1.5537 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  19 (19.00%) low mild
  3 (3.00%) high mild
standard_placement_score/with_custom_filter_rust_callback/100
                        time:   [66.013 µs 66.559 µs 67.138 µs]
                        thrpt:  [1.4895 Melem/s 1.5024 Melem/s 1.5149 Melem/s]
standard_placement_score/with_custom_filter_predicate/100
                        time:   [117.43 µs 117.80 µs 118.19 µs]
                        thrpt:  [846.11 Kelem/s 848.90 Kelem/s 851.57 Kelem/s]
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) low mild
  7 (7.00%) high mild
  2 (2.00%) high severe
