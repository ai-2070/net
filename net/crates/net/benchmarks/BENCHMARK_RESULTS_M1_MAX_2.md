     Running unittests src/bin/net-blob.rs (target/release/deps/net_blob-2ed8c4b4ca2435f9)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running benches/auth_guard.rs (target/release/deps/auth_guard-18ac206efb20171e)
Gnuplot not found, using plotters backend
auth_guard_check_fast_hit/single_thread
                        time:   [23.457 ns 23.519 ns 23.638 ns]
                        thrpt:  [42.305 Melem/s 42.519 Melem/s 42.631 Melem/s]
                 change:
                        time:   [−1.5124% −0.7623% −0.1209%] (p = 0.03 < 0.05)
                        thrpt:  [+0.1210% +0.7682% +1.5356%]
                        Change within noise threshold.
Found 5 outliers among 50 measurements (10.00%)
  2 (4.00%) high mild
  3 (6.00%) high severe

auth_guard_check_fast_miss/single_thread
                        time:   [3.7850 ns 3.7869 ns 3.7896 ns]
                        thrpt:  [263.88 Melem/s 264.07 Melem/s 264.20 Melem/s]
                 change:
                        time:   [−0.4625% −0.2196% +0.0135%] (p = 0.08 > 0.05)
                        thrpt:  [−0.0135% +0.2200% +0.4646%]
                        No change in performance detected.
Found 6 outliers among 50 measurements (12.00%)
  2 (4.00%) high mild
  4 (8.00%) high severe

auth_guard_check_fast_contended/eight_threads
                        time:   [29.356 ns 29.461 ns 29.551 ns]
                        thrpt:  [33.839 Melem/s 33.943 Melem/s 34.065 Melem/s]
                 change:
                        time:   [+0.9655% +1.9974% +2.9010%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8193% −1.9583% −0.9563%]
                        Change within noise threshold.
Found 9 outliers among 50 measurements (18.00%)
  4 (8.00%) low mild
  2 (4.00%) high mild
  3 (6.00%) high severe

auth_guard_allow_channel/insert
                        time:   [163.25 ns 168.29 ns 172.60 ns]
                        thrpt:  [5.7939 Melem/s 5.9420 Melem/s 6.1256 Melem/s]
                 change:
                        time:   [−8.6995% −3.5370% +1.6342%] (p = 0.19 > 0.05)
                        thrpt:  [−1.6079% +3.6667% +9.5284%]
                        No change in performance detected.

auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.8210 ms 2.8228 ms 2.8247 ms]
                        change: [−0.6422% −0.0918% +0.5084%] (p = 0.78 > 0.05)
                        No change in performance detected.
Found 5 outliers among 50 measurements (10.00%)
  2 (4.00%) high mild
  3 (6.00%) high severe

     Running benches/cortex.rs (target/release/deps/cortex-a5bd607cd88615c8)
Gnuplot not found, using plotters backend
cortex_ingest/tasks_create
                        time:   [120.40 ns 121.98 ns 123.72 ns]
                        thrpt:  [8.0829 Melem/s 8.1981 Melem/s 8.3054 Melem/s]
                 change:
                        time:   [−6.4619% +2.6144% +12.967%] (p = 0.62 > 0.05)
                        thrpt:  [−11.479% −2.5478% +6.9083%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
cortex_ingest/memories_store
                        time:   [309.30 ns 321.48 ns 335.28 ns]
                        thrpt:  [2.9826 Melem/s 3.1106 Melem/s 3.2331 Melem/s]
                 change:
                        time:   [−7.2248% +5.7361% +20.889%] (p = 0.42 > 0.05)
                        thrpt:  [−17.279% −5.4249% +7.7874%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) high mild
  3 (3.00%) high severe

cortex_fold_barrier/tasks_create_and_wait
                        time:   [5.7040 µs 5.7151 µs 5.7279 µs]
                        thrpt:  [174.59 Kelem/s 174.97 Kelem/s 175.32 Kelem/s]
                 change:
                        time:   [−1.1262% −0.3792% +0.3623%] (p = 0.34 > 0.05)
                        thrpt:  [−0.3610% +0.3806% +1.1391%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
  8 (8.00%) high severe
cortex_fold_barrier/memories_store_and_wait
                        time:   [5.9316 µs 5.9716 µs 6.0225 µs]
                        thrpt:  [166.04 Kelem/s 167.46 Kelem/s 168.59 Kelem/s]
                 change:
                        time:   [−11.147% −7.5310% −4.3334%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5297% +8.1444% +12.545%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe

cortex_query/tasks_find_many/100
                        time:   [2.2066 µs 2.2139 µs 2.2207 µs]
                        thrpt:  [45.032 Melem/s 45.169 Melem/s 45.318 Melem/s]
                 change:
                        time:   [+1.5129% +2.1763% +2.8196%] (p = 0.00 < 0.05)
                        thrpt:  [−2.7423% −2.1299% −1.4904%]
                        Performance has regressed.
Found 19 outliers among 100 measurements (19.00%)
  3 (3.00%) low severe
  10 (10.00%) low mild
  6 (6.00%) high mild
cortex_query/tasks_count_where/100
                        time:   [164.52 ns 164.72 ns 164.96 ns]
                        thrpt:  [606.21 Melem/s 607.10 Melem/s 607.84 Melem/s]
                 change:
                        time:   [+0.8638% +1.2398% +1.6083%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5828% −1.2246% −0.8564%]
                        Change within noise threshold.
Found 19 outliers among 100 measurements (19.00%)
  18 (18.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_find_unique/100
                        time:   [8.9588 ns 9.0053 ns 9.0592 ns]
                        thrpt:  [11.039 Gelem/s 11.105 Gelem/s 11.162 Gelem/s]
                 change:
                        time:   [−1.0983% −0.4134% +0.1908%] (p = 0.23 > 0.05)
                        thrpt:  [−0.1905% +0.4151% +1.1105%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  10 (10.00%) high mild
  5 (5.00%) high severe
cortex_query/memories_find_many_tag/100
                        time:   [1.0962 µs 1.0992 µs 1.1023 µs]
                        thrpt:  [90.719 Melem/s 90.976 Melem/s 91.223 Melem/s]
                 change:
                        time:   [−4.6840% −4.2016% −3.7455%] (p = 0.00 < 0.05)
                        thrpt:  [+3.8913% +4.3859% +4.9142%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
cortex_query/memories_count_where/100
                        time:   [749.52 ns 751.99 ns 754.98 ns]
                        thrpt:  [132.45 Melem/s 132.98 Melem/s 133.42 Melem/s]
                 change:
                        time:   [−5.3923% −5.0938% −4.7747%] (p = 0.00 < 0.05)
                        thrpt:  [+5.0141% +5.3672% +5.6996%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_find_many/1000
                        time:   [19.320 µs 19.344 µs 19.368 µs]
                        thrpt:  [51.630 Melem/s 51.695 Melem/s 51.759 Melem/s]
                 change:
                        time:   [−1.9251% −1.2026% −0.4915%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4939% +1.2172% +1.9629%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_count_where/1000
                        time:   [1.6371 µs 1.6384 µs 1.6399 µs]
                        thrpt:  [609.80 Melem/s 610.36 Melem/s 610.84 Melem/s]
                 change:
                        time:   [−2.1545% −1.3087% −0.6226%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6265% +1.3261% +2.2019%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe
cortex_query/tasks_find_unique/1000
                        time:   [8.9290 ns 8.9839 ns 9.0477 ns]
                        thrpt:  [110.53 Gelem/s 111.31 Gelem/s 111.99 Gelem/s]
                 change:
                        time:   [−1.1515% −0.5085% +0.0731%] (p = 0.12 > 0.05)
                        thrpt:  [−0.0730% +0.5111% +1.1649%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
cortex_query/memories_find_many_tag/1000
                        time:   [12.877 µs 12.903 µs 12.926 µs]
                        thrpt:  [77.364 Melem/s 77.504 Melem/s 77.656 Melem/s]
                 change:
                        time:   [−5.2670% −4.7428% −4.2912%] (p = 0.00 < 0.05)
                        thrpt:  [+4.4836% +4.9790% +5.5599%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
cortex_query/memories_count_where/1000
                        time:   [11.370 µs 11.420 µs 11.482 µs]
                        thrpt:  [87.092 Melem/s 87.565 Melem/s 87.951 Melem/s]
                 change:
                        time:   [+3.7654% +4.3992% +5.1021%] (p = 0.00 < 0.05)
                        thrpt:  [−4.8545% −4.2138% −3.6288%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
cortex_query/tasks_find_many/10000
                        time:   [280.21 µs 293.77 µs 306.23 µs]
                        thrpt:  [32.655 Melem/s 34.041 Melem/s 35.688 Melem/s]
                 change:
                        time:   [+20.752% +26.551% +32.347%] (p = 0.00 < 0.05)
                        thrpt:  [−24.441% −20.981% −17.186%]
                        Performance has regressed.
cortex_query/tasks_count_where/10000
                        time:   [38.303 µs 38.982 µs 39.751 µs]
                        thrpt:  [251.57 Melem/s 256.53 Melem/s 261.08 Melem/s]
                 change:
                        time:   [+27.830% +31.477% +35.185%] (p = 0.00 < 0.05)
                        thrpt:  [−26.027% −23.941% −21.771%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
cortex_query/tasks_find_unique/10000
                        time:   [8.9740 ns 9.0108 ns 9.0511 ns]
                        thrpt:  [1104.8 Gelem/s 1109.8 Gelem/s 1114.3 Gelem/s]
                 change:
                        time:   [−3.8701% −3.2344% −2.6271%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6980% +3.3425% +4.0259%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  7 (7.00%) high mild
  3 (3.00%) high severe
cortex_query/memories_find_many_tag/10000
                        time:   [171.51 µs 172.88 µs 174.37 µs]
                        thrpt:  [57.351 Melem/s 57.844 Melem/s 58.305 Melem/s]
                 change:
                        time:   [+0.9699% +1.9160% +2.9409%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8569% −1.8800% −0.9606%]
                        Change within noise threshold.
cortex_query/memories_count_where/10000
                        time:   [152.14 µs 152.53 µs 153.02 µs]
                        thrpt:  [65.352 Melem/s 65.559 Melem/s 65.728 Melem/s]
                 change:
                        time:   [+0.1122% +1.2996% +2.5113%] (p = 0.03 < 0.05)
                        thrpt:  [−2.4498% −1.2829% −0.1121%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe

cortex_snapshot/tasks_encode/100
                        time:   [3.3028 µs 3.3247 µs 3.3466 µs]
                        thrpt:  [29.881 Melem/s 30.078 Melem/s 30.278 Melem/s]
                 change:
                        time:   [+1.0730% +1.6907% +2.3504%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2964% −1.6626% −1.0616%]
                        Performance has regressed.
cortex_snapshot/memories_encode/100
                        time:   [5.5678 µs 5.5803 µs 5.5939 µs]
                        thrpt:  [17.877 Melem/s 17.920 Melem/s 17.960 Melem/s]
                 change:
                        time:   [−0.7223% −0.4014% −0.0800%] (p = 0.02 < 0.05)
                        thrpt:  [+0.0801% +0.4030% +0.7275%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_3939/100
                        time:   [2.2115 µs 2.2143 µs 2.2170 µs]
                        thrpt:  [45.105 Melem/s 45.162 Melem/s 45.219 Melem/s]
                 change:
                        time:   [−3.3046% −2.9749% −2.6481%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7201% +3.0661% +3.4175%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/netdb_bundle_decode/100
                        time:   [2.2463 µs 2.2502 µs 2.2554 µs]
                        thrpt:  [44.339 Melem/s 44.440 Melem/s 44.518 Melem/s]
                 change:
                        time:   [−0.1537% +0.0586% +0.3089%] (p = 0.63 > 0.05)
                        thrpt:  [−0.3080% −0.0585% +0.1540%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
cortex_snapshot/tasks_encode/1000
                        time:   [30.455 µs 30.547 µs 30.645 µs]
                        thrpt:  [32.631 Melem/s 32.737 Melem/s 32.835 Melem/s]
                 change:
                        time:   [−1.5842% −1.2631% −0.8656%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8732% +1.2793% +1.6097%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/memories_encode/1000
                        time:   [56.310 µs 56.487 µs 56.719 µs]
                        thrpt:  [17.631 Melem/s 17.703 Melem/s 17.759 Melem/s]
                 change:
                        time:   [−0.8840% −0.5723% −0.2602%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2609% +0.5756% +0.8919%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_48274/1000
                        time:   [22.601 µs 22.677 µs 22.754 µs]
                        thrpt:  [43.948 Melem/s 44.097 Melem/s 44.246 Melem/s]
                 change:
                        time:   [−2.2978% −1.5777% −0.9097%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9180% +1.6030% +2.3518%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_decode/1000
                        time:   [26.359 µs 26.388 µs 26.424 µs]
                        thrpt:  [37.844 Melem/s 37.895 Melem/s 37.938 Melem/s]
                 change:
                        time:   [−0.2520% −0.0502% +0.1488%] (p = 0.64 > 0.05)
                        thrpt:  [−0.1486% +0.0502% +0.2527%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
cortex_snapshot/tasks_encode/10000
                        time:   [348.79 µs 359.15 µs 368.57 µs]
                        thrpt:  [27.132 Melem/s 27.844 Melem/s 28.671 Melem/s]
                 change:
                        time:   [+5.6443% +9.8555% +13.883%] (p = 0.00 < 0.05)
                        thrpt:  [−12.190% −8.9713% −5.3428%]
                        Performance has regressed.
cortex_snapshot/memories_encode/10000
                        time:   [645.84 µs 661.64 µs 676.58 µs]
                        thrpt:  [14.780 Melem/s 15.114 Melem/s 15.484 Melem/s]
                 change:
                        time:   [−8.3219% −5.3998% −2.4401%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5011% +5.7080% +9.0773%]
                        Performance has improved.
cortex_snapshot/netdb_bundle_encode_bytes_511774/10000
                        time:   [277.21 µs 291.46 µs 305.39 µs]
                        thrpt:  [32.745 Melem/s 34.310 Melem/s 36.074 Melem/s]
                 change:
                        time:   [−15.827% −10.833% −5.5442%] (p = 0.00 < 0.05)
                        thrpt:  [+5.8696% +12.150% +18.803%]
                        Performance has improved.
cortex_snapshot/netdb_bundle_decode/10000
                        time:   [308.48 µs 319.07 µs 330.32 µs]
                        thrpt:  [30.274 Melem/s 31.341 Melem/s 32.417 Melem/s]
                 change:
                        time:   [+6.8591% +10.250% +13.635%] (p = 0.00 < 0.05)
                        thrpt:  [−11.999% −9.2969% −6.4188%]
                        Performance has regressed.

     Running benches/ingestion.rs (target/release/deps/ingestion-e982afcc41dcc814)
Gnuplot not found, using plotters backend
shard/ingest_raw/1024   time:   [46.658 ns 46.788 ns 46.947 ns]
                        thrpt:  [21.300 Melem/s 21.373 Melem/s 21.433 Melem/s]
                 change:
                        time:   [+1.5522% +1.8820% +2.2210%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1728% −1.8472% −1.5285%]
                        Performance has regressed.
shard/ingest_raw_pop/1024
                        time:   [43.646 ns 43.673 ns 43.713 ns]
                        thrpt:  [22.876 Melem/s 22.898 Melem/s 22.912 Melem/s]
                 change:
                        time:   [−0.2775% −0.0668% +0.1391%] (p = 0.54 > 0.05)
                        thrpt:  [−0.1389% +0.0668% +0.2782%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
shard/ingest_raw/8192   time:   [46.302 ns 46.323 ns 46.346 ns]
                        thrpt:  [21.577 Melem/s 21.588 Melem/s 21.597 Melem/s]
                 change:
                        time:   [−0.3291% +0.0553% +0.3985%] (p = 0.77 > 0.05)
                        thrpt:  [−0.3969% −0.0552% +0.3302%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  3 (3.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
shard/ingest_raw_pop/8192
                        time:   [43.691 ns 43.721 ns 43.759 ns]
                        thrpt:  [22.852 Melem/s 22.872 Melem/s 22.888 Melem/s]
                 change:
                        time:   [−0.6015% −0.2599% +0.0685%] (p = 0.13 > 0.05)
                        thrpt:  [−0.0684% +0.2606% +0.6052%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
shard/ingest_raw/65536  time:   [46.011 ns 46.105 ns 46.220 ns]
                        thrpt:  [21.636 Melem/s 21.690 Melem/s 21.734 Melem/s]
                 change:
                        time:   [−0.6512% +0.5076% +1.6373%] (p = 0.38 > 0.05)
                        thrpt:  [−1.6109% −0.5050% +0.6555%]
                        No change in performance detected.
Found 26 outliers among 100 measurements (26.00%)
  12 (12.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
shard/ingest_raw_pop/65536
                        time:   [43.906 ns 44.019 ns 44.159 ns]
                        thrpt:  [22.646 Melem/s 22.718 Melem/s 22.776 Melem/s]
                 change:
                        time:   [−0.6447% +0.0322% +0.7159%] (p = 0.93 > 0.05)
                        thrpt:  [−0.7108% −0.0321% +0.6488%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe
shard/ingest_raw/1048576
                        time:   [38.667 ns 39.133 ns 39.516 ns]
                        thrpt:  [25.306 Melem/s 25.554 Melem/s 25.862 Melem/s]
                 change:
                        time:   [−1.7124% +0.0340% +1.6775%] (p = 0.97 > 0.05)
                        thrpt:  [−1.6498% −0.0340% +1.7422%]
                        No change in performance detected.
shard/ingest_raw_pop/1048576
                        time:   [45.129 ns 45.203 ns 45.291 ns]
                        thrpt:  [22.079 Melem/s 22.123 Melem/s 22.159 Melem/s]
                 change:
                        time:   [−1.5463% −0.8562% −0.1674%] (p = 0.01 < 0.05)
                        thrpt:  [+0.1677% +0.8636% +1.5706%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

timestamp/next          time:   [7.4745 ns 7.4811 ns 7.4895 ns]
                        thrpt:  [133.52 Melem/s 133.67 Melem/s 133.79 Melem/s]
                 change:
                        time:   [−0.3409% −0.1325% +0.0364%] (p = 0.19 > 0.05)
                        thrpt:  [−0.0364% +0.1327% +0.3420%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
timestamp/now_raw       time:   [620.88 ps 621.32 ps 622.02 ps]
                        thrpt:  [1.6077 Gelem/s 1.6095 Gelem/s 1.6106 Gelem/s]
                 change:
                        time:   [−0.1906% −0.0324% +0.1319%] (p = 0.70 > 0.05)
                        thrpt:  [−0.1317% +0.0325% +0.1910%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe

event/internal_event_new
                        time:   [285.92 ns 287.72 ns 289.69 ns]
                        thrpt:  [3.4520 Melem/s 3.4756 Melem/s 3.4975 Melem/s]
                 change:
                        time:   [+0.9491% +2.2808% +3.4223%] (p = 0.00 < 0.05)
                        thrpt:  [−3.3090% −2.2299% −0.9402%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
event/internal_event_from_bytes
                        time:   [12.580 ns 12.684 ns 12.784 ns]
                        thrpt:  [78.224 Melem/s 78.840 Melem/s 79.491 Melem/s]
                 change:
                        time:   [+0.6755% +1.1446% +1.7316%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7021% −1.1317% −0.6709%]
                        Change within noise threshold.
Found 20 outliers among 100 measurements (20.00%)
  6 (6.00%) high mild
  14 (14.00%) high severe
event/json_creation     time:   [169.50 ns 170.15 ns 170.79 ns]
                        thrpt:  [5.8552 Melem/s 5.8773 Melem/s 5.8996 Melem/s]
                 change:
                        time:   [+2.8375% +3.5500% +4.2933%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1166% −3.4283% −2.7592%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

batch/pop_batch_steady_state/100
                        time:   [3.8055 µs 3.8080 µs 3.8110 µs]
                        thrpt:  [26.240 Melem/s 26.260 Melem/s 26.277 Melem/s]
                 change:
                        time:   [−0.2652% −0.0287% +0.2062%] (p = 0.82 > 0.05)
                        thrpt:  [−0.2058% +0.0287% +0.2659%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
batch/pop_batch_steady_state/1000
                        time:   [37.986 µs 38.021 µs 38.065 µs]
                        thrpt:  [26.271 Melem/s 26.301 Melem/s 26.325 Melem/s]
                 change:
                        time:   [−0.2710% −0.0433% +0.1941%] (p = 0.72 > 0.05)
                        thrpt:  [−0.1937% +0.0433% +0.2717%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
batch/pop_batch_steady_state/10000
                        time:   [382.86 µs 383.56 µs 384.52 µs]
                        thrpt:  [26.006 Melem/s 26.071 Melem/s 26.120 Melem/s]
                 change:
                        time:   [−0.0895% +0.2311% +0.5867%] (p = 0.17 > 0.05)
                        thrpt:  [−0.5832% −0.2306% +0.0896%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  5 (5.00%) high mild
  11 (11.00%) high severe

event_bus_ingest_raw_concurrent/producers/1
                        time:   [528.94 µs 533.91 µs 540.01 µs]
                        thrpt:  [15.170 Melem/s 15.343 Melem/s 15.487 Melem/s]
                 change:
                        time:   [−6.3280% −3.3863% −0.7918%] (p = 0.02 < 0.05)
                        thrpt:  [+0.7981% +3.5050% +6.7555%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
event_bus_ingest_raw_concurrent/producers/2
                        time:   [832.29 µs 842.18 µs 852.19 µs]
                        thrpt:  [9.6129 Melem/s 9.7271 Melem/s 9.8427 Melem/s]
                 change:
                        time:   [−4.4115% −0.5118% +3.4877%] (p = 0.80 > 0.05)
                        thrpt:  [−3.3702% +0.5144% +4.6151%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
event_bus_ingest_raw_concurrent/producers/4
                        time:   [945.33 µs 981.07 µs 1.0231 ms]
                        thrpt:  [8.0073 Melem/s 8.3501 Melem/s 8.6657 Melem/s]
                 change:
                        time:   [−2.4641% +3.9610% +10.305%] (p = 0.27 > 0.05)
                        thrpt:  [−9.3426% −3.8101% +2.5264%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
Benchmarking event_bus_ingest_raw_concurrent/producers/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
event_bus_ingest_raw_concurrent/producers/8
                        time:   [1.4801 ms 1.5058 ms 1.5324 ms]
                        thrpt:  [5.3460 Melem/s 5.4403 Melem/s 5.5347 Melem/s]
                 change:
                        time:   [−7.5802% +2.4035% +11.849%] (p = 0.67 > 0.05)
                        thrpt:  [−10.593% −2.3471% +8.2020%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

     Running benches/mesh.rs (target/release/deps/mesh-a204bf7dd9663c73)
Gnuplot not found, using plotters backend
mesh_reroute/triangle_failure
                        time:   [7.2278 µs 7.3303 µs 7.4315 µs]
                        thrpt:  [134.56 Kelem/s 136.42 Kelem/s 138.35 Kelem/s]
                 change:
                        time:   [−0.1488% +1.3599% +2.9538%] (p = 0.09 > 0.05)
                        thrpt:  [−2.8691% −1.3416% +0.1490%]
                        No change in performance detected.
mesh_reroute/10_peers_10_routes
                        time:   [39.826 µs 40.200 µs 40.618 µs]
                        thrpt:  [24.620 Kelem/s 24.876 Kelem/s 25.110 Kelem/s]
                 change:
                        time:   [−1.7730% −0.7793% +0.2119%] (p = 0.12 > 0.05)
                        thrpt:  [−0.2115% +0.7855% +1.8050%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
mesh_reroute/50_peers_100_routes
                        time:   [416.92 µs 418.26 µs 419.63 µs]
                        thrpt:  [2.3831 Kelem/s 2.3909 Kelem/s 2.3985 Kelem/s]
                 change:
                        time:   [−0.3913% +0.1375% +0.6046%] (p = 0.60 > 0.05)
                        thrpt:  [−0.6009% −0.1373% +0.3929%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild

mesh_proximity/on_pingwave_new
                        time:   [171.34 ns 172.34 ns 173.21 ns]
                        thrpt:  [5.7734 Melem/s 5.8024 Melem/s 5.8364 Melem/s]
                 change:
                        time:   [−4.6360% −0.1633% +4.2620%] (p = 0.95 > 0.05)
                        thrpt:  [−4.0878% +0.1635% +4.8613%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  10 (10.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
mesh_proximity/on_pingwave_dedup
                        time:   [70.426 ns 70.756 ns 71.100 ns]
                        thrpt:  [14.065 Melem/s 14.133 Melem/s 14.199 Melem/s]
                 change:
                        time:   [+0.3636% +0.6912% +1.0455%] (p = 0.00 < 0.05)
                        thrpt:  [−1.0347% −0.6864% −0.3623%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
mesh_proximity/pingwave_serialize
                        time:   [1.9913 ns 1.9959 ns 2.0008 ns]
                        thrpt:  [499.81 Melem/s 501.03 Melem/s 502.18 Melem/s]
                 change:
                        time:   [+0.8393% +1.0572% +1.2775%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2614% −1.0462% −0.8323%]
                        Change within noise threshold.
mesh_proximity/pingwave_deserialize
                        time:   [2.2338 ns 2.2366 ns 2.2396 ns]
                        thrpt:  [446.51 Melem/s 447.10 Melem/s 447.67 Melem/s]
                 change:
                        time:   [−0.6597% −0.0412% +0.4237%] (p = 0.90 > 0.05)
                        thrpt:  [−0.4219% +0.0412% +0.6640%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
mesh_proximity/node_count
                        time:   [310.49 ps 310.80 ps 311.16 ps]
                        thrpt:  [3.2137 Gelem/s 3.2175 Gelem/s 3.2207 Gelem/s]
                 change:
                        time:   [−0.2105% −0.0055% +0.1598%] (p = 0.97 > 0.05)
                        thrpt:  [−0.1595% +0.0055% +0.2109%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
mesh_proximity/all_nodes_100
                        time:   [4.6195 µs 4.6376 µs 4.6560 µs]
                        thrpt:  [214.78 Kelem/s 215.63 Kelem/s 216.47 Kelem/s]
                 change:
                        time:   [−1.5599% −0.9380% −0.3163%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3173% +0.9469% +1.5846%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

mesh_dispatch/classify_direct
                        time:   [620.89 ps 621.49 ps 622.41 ps]
                        thrpt:  [1.6066 Gelem/s 1.6090 Gelem/s 1.6106 Gelem/s]
                 change:
                        time:   [−0.6612% −0.4135% −0.1915%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1919% +0.4152% +0.6656%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
mesh_dispatch/classify_routed
                        time:   [443.28 ps 445.48 ps 448.04 ps]
                        thrpt:  [2.2320 Gelem/s 2.2448 Gelem/s 2.2559 Gelem/s]
                 change:
                        time:   [−0.0022% +0.2771% +0.6057%] (p = 0.07 > 0.05)
                        thrpt:  [−0.6021% −0.2763% +0.0022%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  12 (12.00%) high severe
mesh_dispatch/classify_pingwave
                        time:   [311.08 ps 311.52 ps 312.00 ps]
                        thrpt:  [3.2052 Gelem/s 3.2101 Gelem/s 3.2146 Gelem/s]
                 change:
                        time:   [−0.4495% −0.0507% +0.4006%] (p = 0.82 > 0.05)
                        thrpt:  [−0.3990% +0.0507% +0.4515%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe

mesh_routing/lookup_hit time:   [15.060 ns 15.089 ns 15.130 ns]
                        thrpt:  [66.092 Melem/s 66.272 Melem/s 66.400 Melem/s]
                 change:
                        time:   [−0.7322% −0.3217% +0.1693%] (p = 0.17 > 0.05)
                        thrpt:  [−0.1690% +0.3227% +0.7376%]
                        No change in performance detected.
Found 21 outliers among 100 measurements (21.00%)
  5 (5.00%) low severe
  5 (5.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
mesh_routing/lookup_miss
                        time:   [15.060 ns 15.092 ns 15.120 ns]
                        thrpt:  [66.138 Melem/s 66.258 Melem/s 66.401 Melem/s]
                 change:
                        time:   [+5.9364% +6.4957% +7.0645%] (p = 0.00 < 0.05)
                        thrpt:  [−6.5983% −6.0995% −5.6038%]
                        Performance has regressed.
Found 24 outliers among 100 measurements (24.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  15 (15.00%) high severe
mesh_routing/is_local   time:   [312.47 ps 313.88 ps 315.49 ps]
                        thrpt:  [3.1697 Gelem/s 3.1859 Gelem/s 3.2004 Gelem/s]
                 change:
                        time:   [+0.0266% +0.4281% +0.8358%] (p = 0.03 < 0.05)
                        thrpt:  [−0.8289% −0.4263% −0.0266%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
mesh_routing/all_routes/10
                        time:   [1.7485 µs 1.7528 µs 1.7571 µs]
                        thrpt:  [569.13 Kelem/s 570.53 Kelem/s 571.93 Kelem/s]
                 change:
                        time:   [+0.6604% +0.9376% +1.2288%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2139% −0.9288% −0.6561%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
mesh_routing/all_routes/100
                        time:   [2.7161 µs 2.7225 µs 2.7296 µs]
                        thrpt:  [366.36 Kelem/s 367.31 Kelem/s 368.18 Kelem/s]
                 change:
                        time:   [+1.8280% +2.1691% +2.5218%] (p = 0.00 < 0.05)
                        thrpt:  [−2.4598% −2.1230% −1.7952%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low mild
  8 (8.00%) high mild
mesh_routing/all_routes/1000
                        time:   [12.728 µs 12.804 µs 12.878 µs]
                        thrpt:  [77.654 Kelem/s 78.099 Kelem/s 78.565 Kelem/s]
                 change:
                        time:   [+0.8554% +1.6267% +2.3420%] (p = 0.00 < 0.05)
                        thrpt:  [−2.2884% −1.6007% −0.8481%]
                        Change within noise threshold.
mesh_routing/add_route  time:   [45.414 ns 45.876 ns 46.300 ns]
                        thrpt:  [21.598 Melem/s 21.798 Melem/s 22.020 Melem/s]
                 change:
                        time:   [−2.2782% −0.0867% +1.9022%] (p = 0.93 > 0.05)
                        thrpt:  [−1.8667% +0.0868% +2.3314%]
                        No change in performance detected.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low severe
  4 (4.00%) low mild

     Running benches/net.rs (target/release/deps/net-25931e7c46d9a56c)
Gnuplot not found, using plotters backend
net_header/serialize    time:   [2.1908 ns 2.1921 ns 2.1939 ns]
                        thrpt:  [455.82 Melem/s 456.19 Melem/s 456.46 Melem/s]
                 change:
                        time:   [−1.1453% −0.6813% −0.3368%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3380% +0.6860% +1.1586%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
net_header/deserialize  time:   [2.3522 ns 2.3581 ns 2.3653 ns]
                        thrpt:  [422.78 Melem/s 424.08 Melem/s 425.13 Melem/s]
                 change:
                        time:   [−0.8960% −0.3025% +0.1470%] (p = 0.30 > 0.05)
                        thrpt:  [−0.1468% +0.3034% +0.9041%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
net_header/roundtrip    time:   [2.3482 ns 2.3500 ns 2.3523 ns]
                        thrpt:  [425.11 Melem/s 425.53 Melem/s 425.86 Melem/s]
                 change:
                        time:   [−2.3766% −0.8040% +0.1331%] (p = 0.31 > 0.05)
                        thrpt:  [−0.1329% +0.8105% +2.4345%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) high mild
  11 (11.00%) high severe

net_event_frame/write_single/64
                        time:   [21.452 ns 21.469 ns 21.491 ns]
                        thrpt:  [2.7735 GiB/s 2.7763 GiB/s 2.7785 GiB/s]
                 change:
                        time:   [−0.8196% −0.6236% −0.4177%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4194% +0.6275% +0.8264%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single_reused/64
                        time:   [2.5352 ns 2.5438 ns 2.5543 ns]
                        thrpt:  [23.335 GiB/s 23.431 GiB/s 23.511 GiB/s]
                 change:
                        time:   [−0.4223% −0.0973% +0.2093%] (p = 0.53 > 0.05)
                        thrpt:  [−0.2089% +0.0974% +0.4241%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
net_event_frame/write_single/256
                        time:   [45.756 ns 46.377 ns 47.068 ns]
                        thrpt:  [5.0654 GiB/s 5.1409 GiB/s 5.2106 GiB/s]
                 change:
                        time:   [−5.5144% −3.3647% −1.5290%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5527% +3.4819% +5.8362%]
                        Performance has improved.
net_event_frame/write_single_reused/256
                        time:   [5.2775 ns 5.2815 ns 5.2872 ns]
                        thrpt:  [45.094 GiB/s 45.142 GiB/s 45.177 GiB/s]
                 change:
                        time:   [−1.6248% −1.1691% −0.7414%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7470% +1.1830% +1.6517%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single/1024
                        time:   [33.919 ns 33.956 ns 34.000 ns]
                        thrpt:  [28.049 GiB/s 28.085 GiB/s 28.116 GiB/s]
                 change:
                        time:   [−2.4606% −2.1515% −1.8268%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8608% +2.1988% +2.5227%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
net_event_frame/write_single_reused/1024
                        time:   [16.200 ns 17.406 ns 18.716 ns]
                        thrpt:  [50.955 GiB/s 54.790 GiB/s 58.868 GiB/s]
                 change:
                        time:   [+10.668% +16.684% +22.645%] (p = 0.00 < 0.05)
                        thrpt:  [−18.464% −14.299% −9.6396%]
                        Performance has regressed.
net_event_frame/write_single/4096
                        time:   [74.308 ns 75.044 ns 75.961 ns]
                        thrpt:  [50.219 GiB/s 50.833 GiB/s 51.336 GiB/s]
                 change:
                        time:   [−20.627% −18.386% −16.095%] (p = 0.00 < 0.05)
                        thrpt:  [+19.183% +22.528% +25.988%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single_reused/4096
                        time:   [54.201 ns 54.671 ns 55.182 ns]
                        thrpt:  [69.130 GiB/s 69.775 GiB/s 70.381 GiB/s]
                 change:
                        time:   [−24.313% −23.004% −21.764%] (p = 0.00 < 0.05)
                        thrpt:  [+27.819% +29.877% +32.123%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
net_event_frame/write_batch/1
                        time:   [21.522 ns 21.545 ns 21.575 ns]
                        thrpt:  [2.7627 GiB/s 2.7665 GiB/s 2.7694 GiB/s]
                 change:
                        time:   [−1.3309% −1.0537% −0.7671%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7730% +1.0649% +1.3488%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_batch/10
                        time:   [69.769 ns 70.378 ns 71.018 ns]
                        thrpt:  [8.3929 GiB/s 8.4692 GiB/s 8.5432 GiB/s]
                 change:
                        time:   [−0.5095% +0.4546% +1.4262%] (p = 0.36 > 0.05)
                        thrpt:  [−1.4062% −0.4525% +0.5121%]
                        No change in performance detected.
net_event_frame/write_batch/50
                        time:   [145.93 ns 146.20 ns 146.53 ns]
                        thrpt:  [20.339 GiB/s 20.384 GiB/s 20.422 GiB/s]
                 change:
                        time:   [−0.8172% −0.4354% +0.0083%] (p = 0.04 < 0.05)
                        thrpt:  [−0.0083% +0.4373% +0.8240%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_batch/100
                        time:   [270.61 ns 270.98 ns 271.40 ns]
                        thrpt:  [21.962 GiB/s 21.996 GiB/s 22.026 GiB/s]
                 change:
                        time:   [−2.6948% −1.9202% −1.2248%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2400% +1.9578% +2.7695%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  9 (9.00%) high mild
  2 (2.00%) high severe
net_event_frame/read_batch_10
                        time:   [137.14 ns 138.32 ns 139.56 ns]
                        thrpt:  [71.654 Melem/s 72.295 Melem/s 72.916 Melem/s]
                 change:
                        time:   [+2.2557% +3.2479% +4.2054%] (p = 0.00 < 0.05)
                        thrpt:  [−4.0357% −3.1458% −2.2059%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

net_packet_pool/get_return/16
                        time:   [85.281 ns 85.457 ns 85.680 ns]
                        thrpt:  [11.671 Melem/s 11.702 Melem/s 11.726 Melem/s]
                 change:
                        time:   [+69.603% +70.248% +70.941%] (p = 0.00 < 0.05)
                        thrpt:  [−41.500% −41.262% −41.039%]
                        Performance has regressed.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
net_packet_pool/get_return/64
                        time:   [87.720 ns 87.796 ns 87.885 ns]
                        thrpt:  [11.378 Melem/s 11.390 Melem/s 11.400 Melem/s]
                 change:
                        time:   [+73.607% +74.011% +74.410%] (p = 0.00 < 0.05)
                        thrpt:  [−42.664% −42.532% −42.399%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
net_packet_pool/get_return/256
                        time:   [92.382 ns 92.441 ns 92.497 ns]
                        thrpt:  [10.811 Melem/s 10.818 Melem/s 10.825 Melem/s]
                 change:
                        time:   [+82.489% +82.960% +83.434%] (p = 0.00 < 0.05)
                        thrpt:  [−45.485% −45.343% −45.202%]
                        Performance has regressed.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe

net_packet_build/build_packet/1
                        time:   [343.40 ns 344.18 ns 345.04 ns]
                        thrpt:  [176.89 MiB/s 177.34 MiB/s 177.74 MiB/s]
                 change:
                        time:   [−33.260% −32.752% −32.257%] (p = 0.00 < 0.05)
                        thrpt:  [+47.616% +48.704% +49.834%]
                        Performance has improved.
net_packet_build/build_packet/10
                        time:   [757.24 ns 758.03 ns 759.15 ns]
                        thrpt:  [803.99 MiB/s 805.18 MiB/s 806.02 MiB/s]
                 change:
                        time:   [−58.902% −58.684% −58.447%] (p = 0.00 < 0.05)
                        thrpt:  [+140.66% +142.04% +143.32%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) high mild
  5 (5.00%) high severe
net_packet_build/build_packet/50
                        time:   [2.4823 µs 2.4845 µs 2.4876 µs]
                        thrpt:  [1.1981 GiB/s 1.1995 GiB/s 1.2006 GiB/s]
                 change:
                        time:   [−69.759% −69.652% −69.546%] (p = 0.00 < 0.05)
                        thrpt:  [+228.36% +229.51% +230.67%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe

net_encryption/encrypt/64
                        time:   [339.61 ns 340.80 ns 342.26 ns]
                        thrpt:  [178.33 MiB/s 179.10 MiB/s 179.72 MiB/s]
                 change:
                        time:   [−33.643% −33.272% −32.871%] (p = 0.00 < 0.05)
                        thrpt:  [+48.966% +49.862% +50.700%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  3 (3.00%) high mild
  15 (15.00%) high severe
net_encryption/encrypt/256
                        time:   [509.87 ns 510.37 ns 511.06 ns]
                        thrpt:  [477.72 MiB/s 478.36 MiB/s 478.83 MiB/s]
                 change:
                        time:   [−46.320% −46.078% −45.829%] (p = 0.00 < 0.05)
                        thrpt:  [+84.600% +85.452% +86.288%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
net_encryption/encrypt/1024
                        time:   [943.01 ns 943.41 ns 943.88 ns]
                        thrpt:  [1.0104 GiB/s 1.0109 GiB/s 1.0113 GiB/s]
                 change:
                        time:   [−65.129% −65.001% −64.867%] (p = 0.00 < 0.05)
                        thrpt:  [+184.63% +185.72% +186.77%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
net_encryption/encrypt/4096
                        time:   [2.9148 µs 2.9163 µs 2.9183 µs]
                        thrpt:  [1.3072 GiB/s 1.3081 GiB/s 1.3087 GiB/s]
                 change:
                        time:   [−70.048% −69.953% −69.859%] (p = 0.00 < 0.05)
                        thrpt:  [+231.77% +232.82% +233.87%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe
net_encryption/raw_aead/64
                        time:   [352.43 ns 363.48 ns 375.53 ns]
                        thrpt:  [162.53 MiB/s 167.92 MiB/s 173.18 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high severe
net_encryption/raw_aead/256
                        time:   [748.61 ns 758.23 ns 768.86 ns]
                        thrpt:  [317.54 MiB/s 321.99 MiB/s 326.13 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high severe
net_encryption/raw_aead/1024
                        time:   [2.3881 µs 2.4046 µs 2.4236 µs]
                        thrpt:  [402.95 MiB/s 406.12 MiB/s 408.93 MiB/s]
Found 21 outliers among 100 measurements (21.00%)
  1 (1.00%) high mild
  20 (20.00%) high severe
net_encryption/raw_aead/4096
                        time:   [8.8883 µs 8.9072 µs 8.9312 µs]
                        thrpt:  [437.37 MiB/s 438.55 MiB/s 439.48 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
net_encryption/raw_ring/64
                        time:   [236.34 ns 236.49 ns 236.67 ns]
                        thrpt:  [257.90 MiB/s 258.09 MiB/s 258.25 MiB/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
net_encryption/raw_ring/256
                        time:   [294.23 ns 294.62 ns 295.09 ns]
                        thrpt:  [827.34 MiB/s 828.67 MiB/s 829.77 MiB/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe
net_encryption/raw_ring/1024
                        time:   [826.37 ns 828.63 ns 831.56 ns]
                        thrpt:  [1.1469 GiB/s 1.1509 GiB/s 1.1540 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
net_encryption/raw_ring/4096
                        time:   [2.6228 µs 2.6254 µs 2.6289 µs]
                        thrpt:  [1.4511 GiB/s 1.4530 GiB/s 1.4544 GiB/s]
Found 15 outliers among 100 measurements (15.00%)
  7 (7.00%) high mild
  8 (8.00%) high severe

net_keypair/generate    time:   [12.441 µs 12.449 µs 12.456 µs]
                        thrpt:  [80.282 Kelem/s 80.330 Kelem/s 80.377 Kelem/s]
                 change:
                        time:   [−1.4036% −1.0399% −0.6959%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7008% +1.0508% +1.4236%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

net_aad/generate        time:   [1.8632 ns 1.8641 ns 1.8651 ns]
                        thrpt:  [536.15 Melem/s 536.45 Melem/s 536.70 Melem/s]
                 change:
                        time:   [−0.9317% −0.6687% −0.3967%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3983% +0.6732% +0.9404%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe

pool_comparison/shared_pool_get_return
                        time:   [85.400 ns 85.511 ns 85.642 ns]
                        thrpt:  [11.676 Melem/s 11.694 Melem/s 11.710 Melem/s]
                 change:
                        time:   [+18.336% +22.985% +27.889%] (p = 0.00 < 0.05)
                        thrpt:  [−21.807% −18.689% −15.495%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
pool_comparison/thread_local_pool_get_return
                        time:   [146.55 ns 147.08 ns 147.64 ns]
                        thrpt:  [6.7732 Melem/s 6.7992 Melem/s 6.8236 Melem/s]
                 change:
                        time:   [+49.737% +50.298% +50.896%] (p = 0.00 < 0.05)
                        thrpt:  [−33.729% −33.465% −33.216%]
                        Performance has regressed.
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  15 (15.00%) high severe
pool_comparison/shared_pool_10x
                        time:   [818.99 ns 820.15 ns 821.42 ns]
                        thrpt:  [1.2174 Melem/s 1.2193 Melem/s 1.2210 Melem/s]
                 change:
                        time:   [+74.110% +74.827% +75.536%] (p = 0.00 < 0.05)
                        thrpt:  [−43.032% −42.801% −42.565%]
                        Performance has regressed.
pool_comparison/thread_local_pool_10x
                        time:   [1.7758 µs 1.7785 µs 1.7815 µs]
                        thrpt:  [561.32 Kelem/s 562.27 Kelem/s 563.13 Kelem/s]
                 change:
                        time:   [+33.349% +35.061% +36.781%] (p = 0.00 < 0.05)
                        thrpt:  [−26.890% −25.959% −25.009%]
                        Performance has regressed.
Found 26 outliers among 100 measurements (26.00%)
  8 (8.00%) low mild
  6 (6.00%) high mild
  12 (12.00%) high severe

cipher_comparison/shared_pool/64
                        time:   [338.84 ns 339.01 ns 339.19 ns]
                        thrpt:  [179.94 MiB/s 180.04 MiB/s 180.13 MiB/s]
                 change:
                        time:   [−33.931% −33.564% −33.199%] (p = 0.00 < 0.05)
                        thrpt:  [+49.699% +50.521% +51.357%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/64
                        time:   [399.48 ns 399.72 ns 399.99 ns]
                        thrpt:  [152.59 MiB/s 152.69 MiB/s 152.79 MiB/s]
                 change:
                        time:   [−27.994% −27.724% −27.444%] (p = 0.00 < 0.05)
                        thrpt:  [+37.825% +38.358% +38.877%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe
cipher_comparison/shared_pool/256
                        time:   [509.19 ns 511.91 ns 517.60 ns]
                        thrpt:  [471.68 MiB/s 476.92 MiB/s 479.47 MiB/s]
                 change:
                        time:   [−46.379% −46.066% −45.701%] (p = 0.00 < 0.05)
                        thrpt:  [+84.166% +85.413% +86.495%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
cipher_comparison/fast_chacha20/256
                        time:   [570.07 ns 570.42 ns 570.82 ns]
                        thrpt:  [427.70 MiB/s 428.00 MiB/s 428.27 MiB/s]
                 change:
                        time:   [−43.668% −43.248% −42.935%] (p = 0.00 < 0.05)
                        thrpt:  [+75.238% +76.206% +77.520%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
cipher_comparison/shared_pool/1024
                        time:   [944.04 ns 944.57 ns 945.24 ns]
                        thrpt:  [1.0089 GiB/s 1.0096 GiB/s 1.0102 GiB/s]
                 change:
                        time:   [−65.401% −65.189% −65.011%] (p = 0.00 < 0.05)
                        thrpt:  [+185.80% +187.27% +189.02%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
cipher_comparison/fast_chacha20/1024
                        time:   [1.0015 µs 1.0043 µs 1.0076 µs]
                        thrpt:  [969.24 MiB/s 972.37 MiB/s 975.09 MiB/s]
                 change:
                        time:   [−64.367% −64.152% −63.940%] (p = 0.00 < 0.05)
                        thrpt:  [+177.32% +178.95% +180.64%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
cipher_comparison/shared_pool/4096
                        time:   [2.9290 µs 2.9422 µs 2.9580 µs]
                        thrpt:  [1.2896 GiB/s 1.2966 GiB/s 1.3024 GiB/s]
                 change:
                        time:   [−69.958% −69.844% −69.730%] (p = 0.00 < 0.05)
                        thrpt:  [+230.36% +231.61% +232.87%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) high mild
  14 (14.00%) high severe
cipher_comparison/fast_chacha20/4096
                        time:   [2.9544 µs 2.9560 µs 2.9584 µs]
                        thrpt:  [1.2895 GiB/s 1.2905 GiB/s 1.2912 GiB/s]
                 change:
                        time:   [−69.905% −69.732% −69.600%] (p = 0.00 < 0.05)
                        thrpt:  [+228.94% +230.38% +232.28%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe

adaptive_batcher/optimal_size
                        time:   [970.58 ps 971.08 ps 971.65 ps]
                        thrpt:  [1.0292 Gelem/s 1.0298 Gelem/s 1.0303 Gelem/s]
                 change:
                        time:   [−1.2293% −0.9349% −0.6522%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6565% +0.9437% +1.2446%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
adaptive_batcher/record time:   [3.8587 ns 3.8605 ns 3.8630 ns]
                        thrpt:  [258.87 Melem/s 259.03 Melem/s 259.15 Melem/s]
                 change:
                        time:   [−1.4431% −1.1609% −0.8790%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8868% +1.1745% +1.4642%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) high mild
  8 (8.00%) high severe
adaptive_batcher/full_cycle
                        time:   [4.3715 ns 4.3742 ns 4.3778 ns]
                        thrpt:  [228.42 Melem/s 228.61 Melem/s 228.76 Melem/s]
                 change:
                        time:   [−1.2470% −0.9546% −0.6748%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6794% +0.9638% +1.2627%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

e2e_packet_build/shared_pool_50_events
                        time:   [2.4773 µs 2.4796 µs 2.4824 µs]
                        thrpt:  [1.2005 GiB/s 1.2019 GiB/s 1.2030 GiB/s]
                 change:
                        time:   [−69.933% −69.821% −69.711%] (p = 0.00 < 0.05)
                        thrpt:  [+230.15% +231.36% +232.59%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe
e2e_packet_build/fast_50_events
                        time:   [2.5196 µs 2.5215 µs 2.5240 µs]
                        thrpt:  [1.1808 GiB/s 1.1819 GiB/s 1.1828 GiB/s]
                 change:
                        time:   [−69.468% −69.374% −69.277%] (p = 0.00 < 0.05)
                        thrpt:  [+225.49% +226.52% +227.53%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

multithread_packet_build/shared_pool/8
                        time:   [2.0078 ms 2.0108 ms 2.0153 ms]
                        thrpt:  [3.9697 Melem/s 3.9785 Melem/s 3.9846 Melem/s]
                 change:
                        time:   [−3.8558% −1.2581% +1.2684%] (p = 0.34 > 0.05)
                        thrpt:  [−1.2525% +1.2741% +4.0105%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
multithread_packet_build/thread_local_pool/8
                        time:   [672.98 µs 675.91 µs 680.79 µs]
                        thrpt:  [11.751 Melem/s 11.836 Melem/s 11.887 Melem/s]
                 change:
                        time:   [−37.966% −36.030% −33.950%] (p = 0.00 < 0.05)
                        thrpt:  [+51.401% +56.323% +61.202%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
multithread_packet_build/shared_pool/16
                        time:   [4.8291 ms 4.9507 ms 5.0826 ms]
                        thrpt:  [3.1480 Melem/s 3.2318 Melem/s 3.3132 Melem/s]
                 change:
                        time:   [−2.8643% +0.6018% +3.9544%] (p = 0.74 > 0.05)
                        thrpt:  [−3.8040% −0.5982% +2.9488%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.3s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/16
                        time:   [1.4363 ms 1.4381 ms 1.4401 ms]
                        thrpt:  [11.110 Melem/s 11.126 Melem/s 11.140 Melem/s]
                 change:
                        time:   [−21.251% −19.604% −18.145%] (p = 0.00 < 0.05)
                        thrpt:  [+22.168% +24.385% +26.986%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
multithread_packet_build/shared_pool/24
                        time:   [7.6961 ms 7.9436 ms 8.2113 ms]
                        thrpt:  [2.9228 Melem/s 3.0213 Melem/s 3.1185 Melem/s]
                 change:
                        time:   [+1.1886% +5.9046% +10.991%] (p = 0.02 < 0.05)
                        thrpt:  [−9.9027% −5.5754% −1.1746%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
multithread_packet_build/thread_local_pool/24
                        time:   [2.1493 ms 2.1522 ms 2.1552 ms]
                        thrpt:  [11.136 Melem/s 11.152 Melem/s 11.166 Melem/s]
                 change:
                        time:   [−18.409% −17.479% −16.566%] (p = 0.00 < 0.05)
                        thrpt:  [+19.855% +21.181% +22.563%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe
multithread_packet_build/shared_pool/32
                        time:   [11.194 ms 11.660 ms 12.143 ms]
                        thrpt:  [2.6352 Melem/s 2.7444 Melem/s 2.8587 Melem/s]
                 change:
                        time:   [+3.9958% +9.4671% +15.443%] (p = 0.00 < 0.05)
                        thrpt:  [−13.377% −8.6483% −3.8423%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multithread_packet_build/thread_local_pool/32
                        time:   [2.8678 ms 2.8713 ms 2.8758 ms]
                        thrpt:  [11.127 Melem/s 11.145 Melem/s 11.158 Melem/s]
                 change:
                        time:   [−17.541% −16.703% −15.879%] (p = 0.00 < 0.05)
                        thrpt:  [+18.877% +20.052% +21.273%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.9s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/shared_mixed/8
                        time:   [1.1694 ms 1.1838 ms 1.2006 ms]
                        thrpt:  [9.9949 Melem/s 10.137 Melem/s 10.262 Melem/s]
                 change:
                        time:   [−32.465% −30.016% −27.691%] (p = 0.00 < 0.05)
                        thrpt:  [+38.295% +42.890% +48.072%]
                        Performance has improved.
Found 22 outliers among 100 measurements (22.00%)
  7 (7.00%) high mild
  15 (15.00%) high severe
multithread_mixed_frames/fast_mixed/8
                        time:   [673.39 µs 675.14 µs 677.66 µs]
                        thrpt:  [17.708 Melem/s 17.774 Melem/s 17.820 Melem/s]
                 change:
                        time:   [−51.539% −50.207% −48.749%] (p = 0.00 < 0.05)
                        thrpt:  [+95.119% +100.83% +106.35%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  6 (6.00%) high severe
multithread_mixed_frames/shared_mixed/16
                        time:   [2.9858 ms 3.0939 ms 3.2060 ms]
                        thrpt:  [7.4860 Melem/s 7.7572 Melem/s 8.0379 Melem/s]
                 change:
                        time:   [−10.356% −6.4399% −2.6098%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6798% +6.8832% +11.553%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking multithread_mixed_frames/fast_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.5s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/fast_mixed/16
                        time:   [1.2965 ms 1.3186 ms 1.3453 ms]
                        thrpt:  [17.840 Melem/s 18.202 Melem/s 18.512 Melem/s]
                 change:
                        time:   [−43.596% −41.908% −40.176%] (p = 0.00 < 0.05)
                        thrpt:  [+67.158% +72.139% +77.292%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
multithread_mixed_frames/shared_mixed/24
                        time:   [4.9315 ms 5.1608 ms 5.3963 ms]
                        thrpt:  [6.6713 Melem/s 6.9757 Melem/s 7.3000 Melem/s]
                 change:
                        time:   [−3.1877% +2.6248% +8.8065%] (p = 0.39 > 0.05)
                        thrpt:  [−8.0937% −2.5577% +3.2927%]
                        No change in performance detected.
Benchmarking multithread_mixed_frames/fast_mixed/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.6s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/fast_mixed/24
                        time:   [2.0482 ms 2.0739 ms 2.0951 ms]
                        thrpt:  [17.183 Melem/s 17.359 Melem/s 17.577 Melem/s]
                 change:
                        time:   [−41.264% −40.224% −39.117%] (p = 0.00 < 0.05)
                        thrpt:  [+64.249% +67.291% +70.254%]
                        Performance has improved.
multithread_mixed_frames/shared_mixed/32
                        time:   [7.4001 ms 7.7826 ms 8.1992 ms]
                        thrpt:  [5.8542 Melem/s 6.1676 Melem/s 6.4864 Melem/s]
                 change:
                        time:   [+2.9340% +10.396% +18.759%] (p = 0.01 < 0.05)
                        thrpt:  [−15.796% −9.4173% −2.8503%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
multithread_mixed_frames/fast_mixed/32
                        time:   [2.5893 ms 2.6233 ms 2.6588 ms]
                        thrpt:  [18.053 Melem/s 18.298 Melem/s 18.538 Melem/s]
                 change:
                        time:   [−42.222% −40.639% −39.173%] (p = 0.00 < 0.05)
                        thrpt:  [+64.400% +68.461% +73.076%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

pool_contention/shared_acquire_release/8
                        time:   [19.859 ms 19.888 ms 19.917 ms]
                        thrpt:  [4.0167 Melem/s 4.0225 Melem/s 4.0285 Melem/s]
                 change:
                        time:   [+1.9086% +2.9660% +3.9875%] (p = 0.00 < 0.05)
                        thrpt:  [−3.8346% −2.8805% −1.8728%]
                        Performance has regressed.
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) low severe
  5 (5.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
pool_contention/fast_acquire_release/8
                        time:   [1.9533 ms 1.9608 ms 1.9699 ms]
                        thrpt:  [40.611 Melem/s 40.800 Melem/s 40.956 Melem/s]
                 change:
                        time:   [+12.311% +15.727% +19.566%] (p = 0.00 < 0.05)
                        thrpt:  [−16.364% −13.590% −10.961%]
                        Performance has regressed.
Found 19 outliers among 100 measurements (19.00%)
  10 (10.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe
pool_contention/shared_acquire_release/16
                        time:   [41.679 ms 42.179 ms 42.712 ms]
                        thrpt:  [3.7460 Melem/s 3.7934 Melem/s 3.8389 Melem/s]
                 change:
                        time:   [+2.3384% +5.8644% +9.3963%] (p = 0.00 < 0.05)
                        thrpt:  [−8.5893% −5.5395% −2.2850%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
pool_contention/fast_acquire_release/16
                        time:   [3.8546 ms 3.8704 ms 3.8864 ms]
                        thrpt:  [41.170 Melem/s 41.340 Melem/s 41.509 Melem/s]
                 change:
                        time:   [+31.176% +32.277% +33.436%] (p = 0.00 < 0.05)
                        thrpt:  [−25.058% −24.401% −23.766%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking pool_contention/shared_acquire_release/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.4s, or reduce sample count to 70.
pool_contention/shared_acquire_release/24
                        time:   [62.287 ms 63.148 ms 64.130 ms]
                        thrpt:  [3.7424 Melem/s 3.8006 Melem/s 3.8531 Melem/s]
                 change:
                        time:   [−10.078% −7.4089% −4.6192%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8430% +8.0017% +11.207%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
pool_contention/fast_acquire_release/24
                        time:   [5.7048 ms 5.7689 ms 5.8442 ms]
                        thrpt:  [41.066 Melem/s 41.602 Melem/s 42.070 Melem/s]
                 change:
                        time:   [+29.481% +33.034% +36.658%] (p = 0.00 < 0.05)
                        thrpt:  [−26.825% −24.831% −22.769%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
Benchmarking pool_contention/shared_acquire_release/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.7s, or reduce sample count to 50.
pool_contention/shared_acquire_release/32
                        time:   [86.676 ms 88.551 ms 90.643 ms]
                        thrpt:  [3.5303 Melem/s 3.6137 Melem/s 3.6919 Melem/s]
                 change:
                        time:   [−8.1252% −4.7865% −1.3129%] (p = 0.01 < 0.05)
                        thrpt:  [+1.3303% +5.0272% +8.8438%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
pool_contention/fast_acquire_release/32
                        time:   [7.4325 ms 7.4514 ms 7.4721 ms]
                        thrpt:  [42.826 Melem/s 42.945 Melem/s 43.054 Melem/s]
                 change:
                        time:   [+43.701% +45.452% +47.276%] (p = 0.00 < 0.05)
                        thrpt:  [−32.100% −31.249% −30.411%]
                        Performance has regressed.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

throughput_scaling/fast_pool_scaling/1
                        time:   [2.4866 ms 2.4930 ms 2.5024 ms]
                        thrpt:  [799.24 Kelem/s 802.25 Kelem/s 804.30 Kelem/s]
                 change:
                        time:   [−63.874% −63.727% −63.565%] (p = 0.00 < 0.05)
                        thrpt:  [+174.46% +175.69% +176.81%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
throughput_scaling/fast_pool_scaling/2
                        time:   [2.6031 ms 2.6122 ms 2.6208 ms]
                        thrpt:  [1.5263 Melem/s 1.5313 Melem/s 1.5366 Melem/s]
                 change:
                        time:   [−64.079% −63.837% −63.565%] (p = 0.00 < 0.05)
                        thrpt:  [+174.46% +176.52% +178.39%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
throughput_scaling/fast_pool_scaling/4
                        time:   [2.7901 ms 2.8039 ms 2.8228 ms]
                        thrpt:  [2.8341 Melem/s 2.8531 Melem/s 2.8672 Melem/s]
                 change:
                        time:   [−62.449% −62.099% −61.756%] (p = 0.00 < 0.05)
                        thrpt:  [+161.48% +163.85% +166.30%]
                        Performance has improved.
throughput_scaling/fast_pool_scaling/8
                        time:   [3.0607 ms 3.0756 ms 3.0897 ms]
                        thrpt:  [5.1785 Melem/s 5.2023 Melem/s 5.2276 Melem/s]
                 change:
                        time:   [−71.352% −69.657% −67.535%] (p = 0.00 < 0.05)
                        thrpt:  [+208.03% +229.57% +249.06%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
throughput_scaling/fast_pool_scaling/16
                        time:   [6.0413 ms 6.0532 ms 6.0674 ms]
                        thrpt:  [5.2741 Melem/s 5.2865 Melem/s 5.2969 Melem/s]
                 change:
                        time:   [−65.462% −64.834% −64.016%] (p = 0.00 < 0.05)
                        thrpt:  [+177.90% +184.36% +189.53%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) high severe
throughput_scaling/fast_pool_scaling/24
                        time:   [8.9158 ms 8.9258 ms 8.9426 ms]
                        thrpt:  [5.3676 Melem/s 5.3777 Melem/s 5.3837 Melem/s]
                 change:
                        time:   [−65.873% −65.326% −64.868%] (p = 0.00 < 0.05)
                        thrpt:  [+184.64% +188.40% +193.03%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
throughput_scaling/fast_pool_scaling/32
                        time:   [11.864 ms 11.890 ms 11.929 ms]
                        thrpt:  [5.3651 Melem/s 5.3826 Melem/s 5.3944 Melem/s]
                 change:
                        time:   [−64.354% −63.807% −63.170%] (p = 0.00 < 0.05)
                        thrpt:  [+171.52% +176.30% +180.53%]
                        Performance has improved.
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) high mild
  3 (15.00%) high severe

routing_header/serialize
                        time:   [624.05 ps 624.83 ps 625.69 ps]
                        thrpt:  [1.5982 Gelem/s 1.6004 Gelem/s 1.6024 Gelem/s]
                 change:
                        time:   [−3.0543% −2.7426% −2.4416%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5027% +2.8199% +3.1506%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
routing_header/deserialize
                        time:   [933.37 ps 935.87 ps 939.28 ps]
                        thrpt:  [1.0646 Gelem/s 1.0685 Gelem/s 1.0714 Gelem/s]
                 change:
                        time:   [−2.8823% −2.6526% −2.4123%] (p = 0.00 < 0.05)
                        thrpt:  [+2.4719% +2.7248% +2.9678%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) high mild
  6 (6.00%) high severe
routing_header/roundtrip
                        time:   [933.93 ps 937.14 ps 940.88 ps]
                        thrpt:  [1.0628 Gelem/s 1.0671 Gelem/s 1.0707 Gelem/s]
                 change:
                        time:   [−3.4060% −2.8602% −2.4126%] (p = 0.00 < 0.05)
                        thrpt:  [+2.4722% +2.9444% +3.5261%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
routing_header/forward  time:   [570.11 ps 572.32 ps 574.97 ps]
                        thrpt:  [1.7392 Gelem/s 1.7473 Gelem/s 1.7541 Gelem/s]
                 change:
                        time:   [−2.7862% −2.3239% −1.8703%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9059% +2.3792% +2.8661%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild

routing_table/lookup_hit
                        time:   [36.971 ns 37.799 ns 38.703 ns]
                        thrpt:  [25.838 Melem/s 26.456 Melem/s 27.048 Melem/s]
                 change:
                        time:   [−8.6896% −6.0699% −3.4823%] (p = 0.00 < 0.05)
                        thrpt:  [+3.6080% +6.4621% +9.5165%]
                        Performance has improved.
routing_table/lookup_miss
                        time:   [15.053 ns 15.127 ns 15.195 ns]
                        thrpt:  [65.810 Melem/s 66.105 Melem/s 66.431 Melem/s]
                 change:
                        time:   [−3.6097% −2.8589% −2.1301%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1765% +2.9430% +3.7449%]
                        Performance has improved.
Found 27 outliers among 100 measurements (27.00%)
  8 (8.00%) low severe
  2 (2.00%) low mild
  4 (4.00%) high mild
  13 (13.00%) high severe
routing_table/is_local  time:   [311.30 ps 311.74 ps 312.29 ps]
                        thrpt:  [3.2021 Gelem/s 3.2078 Gelem/s 3.2124 Gelem/s]
                 change:
                        time:   [−2.4973% −1.4517% +0.1681%] (p = 0.01 < 0.05)
                        thrpt:  [−0.1678% +1.4731% +2.5612%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
routing_table/add_route time:   [46.152 ns 46.646 ns 47.098 ns]
                        thrpt:  [21.233 Melem/s 21.438 Melem/s 21.668 Melem/s]
                 change:
                        time:   [−3.9751% −2.5353% −1.0453%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0564% +2.6012% +4.1396%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) low severe
  5 (5.00%) low mild
  4 (4.00%) high mild
routing_table/record_in time:   [50.473 ns 51.200 ns 51.845 ns]
                        thrpt:  [19.288 Melem/s 19.531 Melem/s 19.812 Melem/s]
                 change:
                        time:   [+1.0207% +3.0531% +5.0170%] (p = 0.00 < 0.05)
                        thrpt:  [−4.7773% −2.9627% −1.0104%]
                        Performance has regressed.
Found 30 outliers among 100 measurements (30.00%)
  22 (22.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
routing_table/record_out
                        time:   [21.846 ns 22.144 ns 22.397 ns]
                        thrpt:  [44.649 Melem/s 45.160 Melem/s 45.776 Melem/s]
                 change:
                        time:   [−4.6085% −2.9755% −1.4602%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4819% +3.0667% +4.8312%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  11 (11.00%) low severe
  3 (3.00%) low mild
routing_table/aggregate_stats
                        time:   [2.1644 µs 2.1672 µs 2.1707 µs]
                        thrpt:  [460.69 Kelem/s 461.42 Kelem/s 462.02 Kelem/s]
                 change:
                        time:   [−5.3143% −5.0381% −4.7464%] (p = 0.00 < 0.05)
                        thrpt:  [+4.9829% +5.3054% +5.6126%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe

fair_scheduler/creation time:   [376.14 ns 379.73 ns 383.28 ns]
                        thrpt:  [2.6091 Melem/s 2.6335 Melem/s 2.6586 Melem/s]
                 change:
                        time:   [−3.3963% −2.5979% −1.8174%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8511% +2.6671% +3.5157%]
                        Performance has improved.
fair_scheduler/stream_count_empty
                        time:   [199.35 ns 199.46 ns 199.59 ns]
                        thrpt:  [5.0102 Melem/s 5.0136 Melem/s 5.0162 Melem/s]
                 change:
                        time:   [−4.3707% −3.5509% −2.9312%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0197% +3.6816% +4.5704%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
fair_scheduler/total_queued
                        time:   [310.50 ps 310.71 ps 310.98 ps]
                        thrpt:  [3.2157 Gelem/s 3.2184 Gelem/s 3.2206 Gelem/s]
                 change:
                        time:   [−8.2570% −4.8523% −2.9284%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0168% +5.0997% +9.0002%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
fair_scheduler/cleanup_empty
                        time:   [200.34 ns 200.45 ns 200.58 ns]
                        thrpt:  [4.9856 Melem/s 4.9889 Melem/s 4.9915 Melem/s]
                 change:
                        time:   [−6.2954% −4.1413% −2.8780%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9633% +4.3202% +6.7184%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe

routing_table_concurrent/concurrent_lookup/4
                        time:   [144.04 µs 150.19 µs 156.96 µs]
                        thrpt:  [25.484 Melem/s 26.633 Melem/s 27.771 Melem/s]
                 change:
                        time:   [−19.464% −17.232% −15.063%] (p = 0.00 < 0.05)
                        thrpt:  [+17.734% +20.820% +24.167%]
                        Performance has improved.
Found 28 outliers among 100 measurements (28.00%)
  24 (24.00%) low mild
  4 (4.00%) high mild
routing_table_concurrent/concurrent_stats/4
                        time:   [291.04 µs 291.76 µs 292.48 µs]
                        thrpt:  [13.676 Melem/s 13.710 Melem/s 13.744 Melem/s]
                 change:
                        time:   [+0.8494% +1.5554% +2.1642%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1183% −1.5316% −0.8423%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
routing_table_concurrent/concurrent_lookup/8
                        time:   [244.09 µs 245.30 µs 246.14 µs]
                        thrpt:  [32.501 Melem/s 32.613 Melem/s 32.775 Melem/s]
                 change:
                        time:   [−9.3273% −8.5273% −7.8481%] (p = 0.00 < 0.05)
                        thrpt:  [+8.5164% +9.3222% +10.287%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
routing_table_concurrent/concurrent_stats/8
                        time:   [460.15 µs 462.98 µs 466.58 µs]
                        thrpt:  [17.146 Melem/s 17.279 Melem/s 17.386 Melem/s]
                 change:
                        time:   [−6.1763% −4.0834% −1.4633%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4850% +4.2572% +6.5829%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
routing_table_concurrent/concurrent_lookup/16
                        time:   [450.96 µs 452.19 µs 453.83 µs]
                        thrpt:  [35.255 Melem/s 35.383 Melem/s 35.480 Melem/s]
                 change:
                        time:   [−7.0245% −5.9422% −4.8887%] (p = 0.00 < 0.05)
                        thrpt:  [+5.1400% +6.3176% +7.5552%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
routing_table_concurrent/concurrent_stats/16
                        time:   [805.92 µs 808.97 µs 812.73 µs]
                        thrpt:  [19.687 Melem/s 19.778 Melem/s 19.853 Melem/s]
                 change:
                        time:   [−7.3830% −6.7034% −6.0141%] (p = 0.00 < 0.05)
                        thrpt:  [+6.3990% +7.1851% +7.9716%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

routing_decision/parse_lookup_forward
                        time:   [37.875 ns 38.369 ns 38.963 ns]
                        thrpt:  [25.665 Melem/s 26.063 Melem/s 26.403 Melem/s]
                 change:
                        time:   [−4.2757% −2.8453% −1.0712%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0828% +2.9286% +4.4667%]
                        Performance has improved.
Found 19 outliers among 100 measurements (19.00%)
  4 (4.00%) high mild
  15 (15.00%) high severe
routing_decision/full_with_stats
                        time:   [106.15 ns 106.47 ns 106.88 ns]
                        thrpt:  [9.3560 Melem/s 9.3922 Melem/s 9.4204 Melem/s]
                 change:
                        time:   [−3.2155% −2.7124% −2.2260%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2767% +2.7880% +3.3224%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe

stream_multiplexing/lookup_all/10
                        time:   [290.74 ns 291.39 ns 292.41 ns]
                        thrpt:  [34.199 Melem/s 34.319 Melem/s 34.395 Melem/s]
                 change:
                        time:   [−3.3640% −2.9306% −2.6081%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6780% +3.0191% +3.4811%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe
stream_multiplexing/stats_all/10
                        time:   [463.19 ns 466.24 ns 468.73 ns]
                        thrpt:  [21.334 Melem/s 21.448 Melem/s 21.589 Melem/s]
                 change:
                        time:   [−9.9516% −9.0307% −8.0256%] (p = 0.00 < 0.05)
                        thrpt:  [+8.7259% +9.9272% +11.051%]
                        Performance has improved.
Found 33 outliers among 100 measurements (33.00%)
  19 (19.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  8 (8.00%) high severe
stream_multiplexing/lookup_all/100
                        time:   [2.9076 µs 2.9103 µs 2.9140 µs]
                        thrpt:  [34.317 Melem/s 34.361 Melem/s 34.393 Melem/s]
                 change:
                        time:   [−3.1608% −2.9652% −2.7765%] (p = 0.00 < 0.05)
                        thrpt:  [+2.8558% +3.0558% +3.2640%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
stream_multiplexing/stats_all/100
                        time:   [4.5867 µs 4.6186 µs 4.6465 µs]
                        thrpt:  [21.522 Melem/s 21.652 Melem/s 21.802 Melem/s]
                 change:
                        time:   [−12.329% −11.738% −11.150%] (p = 0.00 < 0.05)
                        thrpt:  [+12.549% +13.299% +14.063%]
                        Performance has improved.
Found 28 outliers among 100 measurements (28.00%)
  18 (18.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild
  7 (7.00%) high severe
stream_multiplexing/lookup_all/1000
                        time:   [29.162 µs 29.252 µs 29.362 µs]
                        thrpt:  [34.058 Melem/s 34.185 Melem/s 34.291 Melem/s]
                 change:
                        time:   [−4.3590% −3.4303% −2.6822%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7562% +3.5521% +4.5576%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
stream_multiplexing/stats_all/1000
                        time:   [45.955 µs 46.562 µs 47.420 µs]
                        thrpt:  [21.088 Melem/s 21.477 Melem/s 21.760 Melem/s]
                 change:
                        time:   [−12.204% −11.289% −10.206%] (p = 0.00 < 0.05)
                        thrpt:  [+11.366% +12.726% +13.901%]
                        Performance has improved.
Found 28 outliers among 100 measurements (28.00%)
  16 (16.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe
stream_multiplexing/lookup_all/10000
                        time:   [291.19 µs 291.41 µs 291.74 µs]
                        thrpt:  [34.277 Melem/s 34.316 Melem/s 34.342 Melem/s]
                 change:
                        time:   [−17.060% −16.665% −16.202%] (p = 0.00 < 0.05)
                        thrpt:  [+19.334% +19.997% +20.570%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
stream_multiplexing/stats_all/10000
                        time:   [491.85 µs 494.92 µs 497.56 µs]
                        thrpt:  [20.098 Melem/s 20.205 Melem/s 20.331 Melem/s]
                 change:
                        time:   [−27.013% −26.336% −25.641%] (p = 0.00 < 0.05)
                        thrpt:  [+34.483% +35.751% +37.011%]
                        Performance has improved.
Found 27 outliers among 100 measurements (27.00%)
  15 (15.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  8 (8.00%) high severe

multihop_packet_builder/build/64
                        time:   [25.967 ns 26.012 ns 26.059 ns]
                        thrpt:  [2.2873 GiB/s 2.2915 GiB/s 2.2954 GiB/s]
                 change:
                        time:   [−5.0806% −4.5406% −4.0181%] (p = 0.00 < 0.05)
                        thrpt:  [+4.1863% +4.7566% +5.3525%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build_priority/64
                        time:   [23.935 ns 23.965 ns 24.004 ns]
                        thrpt:  [2.4831 GiB/s 2.4872 GiB/s 2.4903 GiB/s]
                 change:
                        time:   [−5.1537% −4.6286% −4.1231%] (p = 0.00 < 0.05)
                        thrpt:  [+4.3004% +4.8532% +5.4337%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
multihop_packet_builder/build/256
                        time:   [49.910 ns 50.478 ns 51.072 ns]
                        thrpt:  [4.6683 GiB/s 4.7233 GiB/s 4.7769 GiB/s]
                 change:
                        time:   [−9.4800% −7.7835% −6.0269%] (p = 0.00 < 0.05)
                        thrpt:  [+6.4135% +8.4404% +10.473%]
                        Performance has improved.
multihop_packet_builder/build_priority/256
                        time:   [47.695 ns 48.330 ns 49.005 ns]
                        thrpt:  [4.8652 GiB/s 4.9331 GiB/s 4.9988 GiB/s]
                 change:
                        time:   [−4.4497% −3.0968% −1.6002%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6262% +3.1958% +4.6569%]
                        Performance has improved.
multihop_packet_builder/build/1024
                        time:   [39.225 ns 39.242 ns 39.261 ns]
                        thrpt:  [24.290 GiB/s 24.303 GiB/s 24.313 GiB/s]
                 change:
                        time:   [−3.1554% −2.9472% −2.7420%] (p = 0.00 < 0.05)
                        thrpt:  [+2.8194% +3.0366% +3.2582%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) high severe
multihop_packet_builder/build_priority/1024
                        time:   [36.678 ns 36.997 ns 37.331 ns]
                        thrpt:  [25.546 GiB/s 25.777 GiB/s 26.001 GiB/s]
                 change:
                        time:   [−1.6689% −1.2065% −0.7335%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7389% +1.2213% +1.6972%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  11 (11.00%) high mild
  5 (5.00%) high severe
multihop_packet_builder/build/4096
                        time:   [86.251 ns 87.452 ns 88.872 ns]
                        thrpt:  [42.924 GiB/s 43.620 GiB/s 44.228 GiB/s]
                 change:
                        time:   [+1.9011% +4.3253% +6.8979%] (p = 0.00 < 0.05)
                        thrpt:  [−6.4528% −4.1459% −1.8656%]
                        Performance has regressed.
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe
multihop_packet_builder/build_priority/4096
                        time:   [84.678 ns 85.368 ns 86.163 ns]
                        thrpt:  [44.273 GiB/s 44.686 GiB/s 45.050 GiB/s]
                 change:
                        time:   [−2.7040% −0.2354% +2.3028%] (p = 0.85 > 0.05)
                        thrpt:  [−2.2509% +0.2359% +2.7792%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

multihop_chain/forward_chain/1
                        time:   [61.173 ns 61.607 ns 61.980 ns]
                        thrpt:  [16.134 Melem/s 16.232 Melem/s 16.347 Melem/s]
                 change:
                        time:   [+5.1713% +7.0223% +8.8228%] (p = 0.00 < 0.05)
                        thrpt:  [−8.1075% −6.5615% −4.9171%]
                        Performance has regressed.
multihop_chain/forward_chain/2
                        time:   [113.35 ns 113.66 ns 114.01 ns]
                        thrpt:  [8.7711 Melem/s 8.7978 Melem/s 8.8218 Melem/s]
                 change:
                        time:   [+2.9994% +3.8691% +4.7676%] (p = 0.00 < 0.05)
                        thrpt:  [−4.5506% −3.7250% −2.9120%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low severe
  2 (2.00%) high mild
  1 (1.00%) high severe
multihop_chain/forward_chain/3
                        time:   [162.45 ns 164.38 ns 166.19 ns]
                        thrpt:  [6.0173 Melem/s 6.0836 Melem/s 6.1556 Melem/s]
                 change:
                        time:   [+3.1889% +4.2283% +5.2929%] (p = 0.00 < 0.05)
                        thrpt:  [−5.0268% −4.0568% −3.0903%]
                        Performance has regressed.
multihop_chain/forward_chain/4
                        time:   [217.03 ns 218.77 ns 220.42 ns]
                        thrpt:  [4.5367 Melem/s 4.5709 Melem/s 4.6077 Melem/s]
                 change:
                        time:   [+0.9343% +2.1426% +3.3114%] (p = 0.00 < 0.05)
                        thrpt:  [−3.2053% −2.0977% −0.9257%]
                        Change within noise threshold.
multihop_chain/forward_chain/5
                        time:   [271.01 ns 273.49 ns 275.97 ns]
                        thrpt:  [3.6235 Melem/s 3.6564 Melem/s 3.6899 Melem/s]
                 change:
                        time:   [+0.1474% +1.0029% +1.9351%] (p = 0.03 < 0.05)
                        thrpt:  [−1.8984% −0.9930% −0.1472%]
                        Change within noise threshold.

hop_latency/single_hop_process
                        time:   [1.4501 ns 1.4515 ns 1.4532 ns]
                        thrpt:  [688.13 Melem/s 688.92 Melem/s 689.60 Melem/s]
                 change:
                        time:   [−2.9122% −2.7649% −2.6006%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6701% +2.8435% +2.9995%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe
hop_latency/single_hop_full
                        time:   [55.472 ns 56.056 ns 56.608 ns]
                        thrpt:  [17.665 Melem/s 17.839 Melem/s 18.027 Melem/s]
                 change:
                        time:   [−0.2338% +1.4505% +3.1434%] (p = 0.09 > 0.05)
                        thrpt:  [−3.0476% −1.4297% +0.2343%]
                        No change in performance detected.

hop_scaling/64B_1hops   time:   [31.670 ns 31.795 ns 31.931 ns]
                        thrpt:  [1.8667 GiB/s 1.8747 GiB/s 1.8820 GiB/s]
                 change:
                        time:   [−3.2023% −2.7353% −2.2810%] (p = 0.00 < 0.05)
                        thrpt:  [+2.3342% +2.8123% +3.3082%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
hop_scaling/64B_2hops   time:   [78.499 ns 79.310 ns 80.139 ns]
                        thrpt:  [761.62 MiB/s 769.58 MiB/s 777.53 MiB/s]
                 change:
                        time:   [−8.3531% −7.4571% −6.4823%] (p = 0.00 < 0.05)
                        thrpt:  [+6.9316% +8.0580% +9.1144%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
hop_scaling/64B_3hops   time:   [104.97 ns 106.02 ns 107.07 ns]
                        thrpt:  [570.07 MiB/s 575.68 MiB/s 581.43 MiB/s]
                 change:
                        time:   [−9.1382% −8.1536% −7.1781%] (p = 0.00 < 0.05)
                        thrpt:  [+7.7332% +8.8774% +10.057%]
                        Performance has improved.
hop_scaling/64B_4hops   time:   [134.45 ns 135.22 ns 136.09 ns]
                        thrpt:  [448.48 MiB/s 451.39 MiB/s 453.95 MiB/s]
                 change:
                        time:   [−3.9992% −3.4225% −2.8498%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9334% +3.5437% +4.1658%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  8 (8.00%) high mild
  9 (9.00%) high severe
hop_scaling/64B_5hops   time:   [160.19 ns 161.75 ns 163.34 ns]
                        thrpt:  [373.67 MiB/s 377.35 MiB/s 381.01 MiB/s]
                 change:
                        time:   [−8.6751% −7.7657% −6.9003%] (p = 0.00 < 0.05)
                        thrpt:  [+7.4117% +8.4196% +9.4992%]
                        Performance has improved.
hop_scaling/256B_1hops  time:   [58.123 ns 58.959 ns 59.806 ns]
                        thrpt:  [3.9865 GiB/s 4.0438 GiB/s 4.1020 GiB/s]
                 change:
                        time:   [+3.1396% +4.3805% +5.5500%] (p = 0.00 < 0.05)
                        thrpt:  [−5.2581% −4.1966% −3.0441%]
                        Performance has regressed.
hop_scaling/256B_2hops  time:   [111.07 ns 111.91 ns 112.84 ns]
                        thrpt:  [2.1129 GiB/s 2.1305 GiB/s 2.1466 GiB/s]
                 change:
                        time:   [−3.9101% −3.0227% −2.1063%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1516% +3.1169% +4.0692%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
hop_scaling/256B_3hops  time:   [161.89 ns 164.14 ns 166.62 ns]
                        thrpt:  [1.4309 GiB/s 1.4525 GiB/s 1.4727 GiB/s]
                 change:
                        time:   [+4.8136% +5.8758% +7.0099%] (p = 0.00 < 0.05)
                        thrpt:  [−6.5507% −5.5497% −4.5926%]
                        Performance has regressed.
hop_scaling/256B_4hops  time:   [216.18 ns 219.00 ns 222.01 ns]
                        thrpt:  [1.0739 GiB/s 1.0887 GiB/s 1.1029 GiB/s]
                 change:
                        time:   [−1.0051% +0.2201% +1.5288%] (p = 0.74 > 0.05)
                        thrpt:  [−1.5058% −0.2196% +1.0153%]
                        No change in performance detected.
hop_scaling/256B_5hops  time:   [262.32 ns 264.87 ns 267.51 ns]
                        thrpt:  [912.65 MiB/s 921.74 MiB/s 930.68 MiB/s]
                 change:
                        time:   [−2.1367% −1.3187% −0.5237%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5264% +1.3364% +2.1833%]
                        Change within noise threshold.
hop_scaling/1024B_1hops time:   [45.450 ns 45.541 ns 45.626 ns]
                        thrpt:  [20.902 GiB/s 20.941 GiB/s 20.983 GiB/s]
                 change:
                        time:   [−11.484% −11.309% −11.147%] (p = 0.00 < 0.05)
                        thrpt:  [+12.545% +12.751% +12.974%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
hop_scaling/1024B_2hops time:   [108.35 ns 109.65 ns 110.91 ns]
                        thrpt:  [8.5986 GiB/s 8.6978 GiB/s 8.8020 GiB/s]
                 change:
                        time:   [−1.7123% −0.5952% +0.4522%] (p = 0.30 > 0.05)
                        thrpt:  [−0.4501% +0.5988% +1.7421%]
                        No change in performance detected.
hop_scaling/1024B_3hops time:   [145.41 ns 146.92 ns 148.59 ns]
                        thrpt:  [6.4183 GiB/s 6.4913 GiB/s 6.5586 GiB/s]
                 change:
                        time:   [−6.6483% −5.7037% −4.6067%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8291% +6.0487% +7.1217%]
                        Performance has improved.
hop_scaling/1024B_4hops time:   [205.55 ns 206.32 ns 207.17 ns]
                        thrpt:  [4.6034 GiB/s 4.6223 GiB/s 4.6396 GiB/s]
                 change:
                        time:   [−1.6244% −0.8502% −0.1621%] (p = 0.02 < 0.05)
                        thrpt:  [+0.1624% +0.8575% +1.6512%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
hop_scaling/1024B_5hops time:   [239.44 ns 241.49 ns 243.56 ns]
                        thrpt:  [3.9156 GiB/s 3.9491 GiB/s 3.9830 GiB/s]
                 change:
                        time:   [−1.5622% −0.8474% −0.1497%] (p = 0.02 < 0.05)
                        thrpt:  [+0.1499% +0.8547% +1.5870%]
                        Change within noise threshold.

multihop_with_routing/route_and_forward/1
                        time:   [157.28 ns 157.73 ns 158.16 ns]
                        thrpt:  [6.3225 Melem/s 6.3400 Melem/s 6.3580 Melem/s]
                 change:
                        time:   [−1.7033% −1.4306% −1.1551%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1686% +1.4513% +1.7328%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_with_routing/route_and_forward/2
                        time:   [303.52 ns 305.28 ns 307.49 ns]
                        thrpt:  [3.2522 Melem/s 3.2757 Melem/s 3.2946 Melem/s]
                 change:
                        time:   [−3.4206% −2.9210% −2.4036%] (p = 0.00 < 0.05)
                        thrpt:  [+2.4628% +3.0089% +3.5418%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe
multihop_with_routing/route_and_forward/3
                        time:   [458.33 ns 459.30 ns 460.22 ns]
                        thrpt:  [2.1729 Melem/s 2.1772 Melem/s 2.1818 Melem/s]
                 change:
                        time:   [−5.2909% −4.5456% −4.0191%] (p = 0.00 < 0.05)
                        thrpt:  [+4.1874% +4.7621% +5.5864%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
multihop_with_routing/route_and_forward/4
                        time:   [616.84 ns 618.14 ns 619.37 ns]
                        thrpt:  [1.6146 Melem/s 1.6178 Melem/s 1.6212 Melem/s]
                 change:
                        time:   [−2.6546% −2.3444% −2.0361%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0784% +2.4007% +2.7270%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_with_routing/route_and_forward/5
                        time:   [770.24 ns 771.53 ns 772.91 ns]
                        thrpt:  [1.2938 Melem/s 1.2961 Melem/s 1.2983 Melem/s]
                 change:
                        time:   [−4.5999% −3.9951% −3.4374%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5598% +4.1614% +4.8217%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe

multihop_concurrent/concurrent_forward/4
                        time:   [877.10 µs 911.39 µs 959.25 µs]
                        thrpt:  [4.1699 Melem/s 4.3889 Melem/s 4.5605 Melem/s]
                 change:
                        time:   [+4.5476% +9.9856% +16.352%] (p = 0.00 < 0.05)
                        thrpt:  [−14.054% −9.0790% −4.3498%]
                        Performance has regressed.
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) high mild
  3 (15.00%) high severe
multihop_concurrent/concurrent_forward/8
                        time:   [1.7738 ms 1.8262 ms 1.8744 ms]
                        thrpt:  [4.2680 Melem/s 4.3807 Melem/s 4.5102 Melem/s]
                 change:
                        time:   [+10.323% +13.195% +16.095%] (p = 0.00 < 0.05)
                        thrpt:  [−13.864% −11.657% −9.3574%]
                        Performance has regressed.
multihop_concurrent/concurrent_forward/16
                        time:   [2.0837 ms 2.1131 ms 2.1422 ms]
                        thrpt:  [7.4691 Melem/s 7.5717 Melem/s 7.6788 Melem/s]
                 change:
                        time:   [−11.400% −9.9405% −8.4973%] (p = 0.00 < 0.05)
                        thrpt:  [+9.2864% +11.038% +12.866%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low severe
  2 (10.00%) high severe

pingwave/serialize      time:   [799.16 ps 800.11 ps 801.54 ps]
                        thrpt:  [1.2476 Gelem/s 1.2498 Gelem/s 1.2513 Gelem/s]
                 change:
                        time:   [−0.7711% −0.5178% −0.2512%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2518% +0.5205% +0.7771%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  5 (5.00%) high severe
pingwave/deserialize    time:   [958.86 ps 959.23 ps 959.66 ps]
                        thrpt:  [1.0420 Gelem/s 1.0425 Gelem/s 1.0429 Gelem/s]
                 change:
                        time:   [−0.1196% −0.0285% +0.0592%] (p = 0.55 > 0.05)
                        thrpt:  [−0.0592% +0.0285% +0.1197%]
                        No change in performance detected.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
pingwave/roundtrip      time:   [931.58 ps 932.25 ps 933.11 ps]
                        thrpt:  [1.0717 Gelem/s 1.0727 Gelem/s 1.0734 Gelem/s]
                 change:
                        time:   [−2.8794% −2.7162% −2.5417%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6080% +2.7921% +2.9647%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
pingwave/forward        time:   [625.97 ps 627.95 ps 630.16 ps]
                        thrpt:  [1.5869 Gelem/s 1.5925 Gelem/s 1.5975 Gelem/s]
                 change:
                        time:   [−4.1399% −3.7706% −3.3886%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5074% +3.9183% +4.3187%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe

capabilities/serialize_simple
                        time:   [20.827 ns 20.959 ns 21.194 ns]
                        thrpt:  [47.183 Melem/s 47.712 Melem/s 48.015 Melem/s]
                 change:
                        time:   [−6.7387% −4.5363% −3.0929%] (p = 0.00 < 0.05)
                        thrpt:  [+3.1917% +4.7519% +7.2256%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
capabilities/deserialize_simple
                        time:   [5.5874 ns 5.5949 ns 5.6025 ns]
                        thrpt:  [178.49 Melem/s 178.73 Melem/s 178.97 Melem/s]
                 change:
                        time:   [−3.2487% −3.0069% −2.7759%] (p = 0.00 < 0.05)
                        thrpt:  [+2.8551% +3.1001% +3.3578%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
capabilities/serialize_complex
                        time:   [44.412 ns 44.470 ns 44.548 ns]
                        thrpt:  [22.448 Melem/s 22.487 Melem/s 22.516 Melem/s]
                 change:
                        time:   [−2.4183% −2.2120% −2.0099%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0511% +2.2621% +2.4782%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
capabilities/deserialize_complex
                        time:   [373.19 ns 376.21 ns 379.36 ns]
                        thrpt:  [2.6360 Melem/s 2.6581 Melem/s 2.6796 Melem/s]
                 change:
                        time:   [−6.7431% −5.6762% −4.5922%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8132% +6.0178% +7.2306%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

local_graph/create_pingwave
                        time:   [2.1003 ns 2.1050 ns 2.1100 ns]
                        thrpt:  [473.94 Melem/s 475.07 Melem/s 476.12 Melem/s]
                 change:
                        time:   [−4.0581% −3.5887% −3.1592%] (p = 0.00 < 0.05)
                        thrpt:  [+3.2622% +3.7222% +4.2297%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
local_graph/on_pingwave_new
                        time:   [39.681 ns 40.200 ns 40.872 ns]
                        thrpt:  [24.466 Melem/s 24.876 Melem/s 25.201 Melem/s]
                 change:
                        time:   [−31.449% −28.015% −24.659%] (p = 0.00 < 0.05)
                        thrpt:  [+32.729% +38.917% +45.878%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
local_graph/on_pingwave_duplicate
                        time:   [22.835 ns 22.908 ns 22.982 ns]
                        thrpt:  [43.512 Melem/s 43.653 Melem/s 43.793 Melem/s]
                 change:
                        time:   [−4.8084% −4.2113% −3.6498%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7881% +4.3964% +5.0513%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
local_graph/get_node    time:   [15.569 ns 15.627 ns 15.680 ns]
                        thrpt:  [63.777 Melem/s 63.991 Melem/s 64.229 Melem/s]
                 change:
                        time:   [+0.1405% +0.5311% +0.8757%] (p = 0.00 < 0.05)
                        thrpt:  [−0.8681% −0.5283% −0.1403%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
local_graph/node_count  time:   [311.12 ps 312.49 ps 314.05 ps]
                        thrpt:  [3.1842 Gelem/s 3.2001 Gelem/s 3.2142 Gelem/s]
                 change:
                        time:   [−4.2219% −3.9050% −3.5773%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7100% +4.0637% +4.4080%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) high severe
local_graph/stats       time:   [387.99 ps 388.11 ps 388.25 ps]
                        thrpt:  [2.5756 Gelem/s 2.5766 Gelem/s 2.5774 Gelem/s]
                 change:
                        time:   [−2.8911% −2.7223% −2.5293%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5949% +2.7985% +2.9772%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe

graph_scaling/all_nodes/100
                        time:   [2.8722 µs 2.8850 µs 2.8979 µs]
                        thrpt:  [34.507 Melem/s 34.662 Melem/s 34.817 Melem/s]
                 change:
                        time:   [−4.4302% −3.7217% −3.0112%] (p = 0.00 < 0.05)
                        thrpt:  [+3.1047% +3.8655% +4.6355%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
graph_scaling/nodes_within_hops/100
                        time:   [3.1747 µs 3.1860 µs 3.1974 µs]
                        thrpt:  [31.275 Melem/s 31.387 Melem/s 31.499 Melem/s]
                 change:
                        time:   [−3.1211% −2.6036% −2.0793%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1234% +2.6732% +3.2216%]
                        Performance has improved.
graph_scaling/all_nodes/500
                        time:   [8.3115 µs 8.3574 µs 8.4027 µs]
                        thrpt:  [59.505 Melem/s 59.827 Melem/s 60.158 Melem/s]
                 change:
                        time:   [−3.6938% −3.1975% −2.6697%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7429% +3.3031% +3.8354%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
graph_scaling/nodes_within_hops/500
                        time:   [9.6628 µs 9.7174 µs 9.7739 µs]
                        thrpt:  [51.157 Melem/s 51.454 Melem/s 51.745 Melem/s]
                 change:
                        time:   [−4.9431% −4.4164% −3.9044%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0631% +4.6205% +5.2002%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
graph_scaling/all_nodes/1000
                        time:   [47.804 µs 49.036 µs 50.412 µs]
                        thrpt:  [19.836 Melem/s 20.393 Melem/s 20.919 Melem/s]
                 change:
                        time:   [−5.3460% −3.0687% −0.9100%] (p = 0.01 < 0.05)
                        thrpt:  [+0.9184% +3.1659% +5.6479%]
                        Change within noise threshold.
graph_scaling/nodes_within_hops/1000
                        time:   [48.973 µs 50.474 µs 52.045 µs]
                        thrpt:  [19.214 Melem/s 19.812 Melem/s 20.420 Melem/s]
                 change:
                        time:   [+15.253% +24.985% +36.415%] (p = 0.00 < 0.05)
                        thrpt:  [−26.694% −19.990% −13.235%]
                        Performance has regressed.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) low mild
  11 (11.00%) high mild
  1 (1.00%) high severe
graph_scaling/all_nodes/5000
                        time:   [104.31 µs 106.45 µs 108.83 µs]
                        thrpt:  [45.944 Melem/s 46.970 Melem/s 47.936 Melem/s]
                 change:
                        time:   [−24.433% −22.736% −20.924%] (p = 0.00 < 0.05)
                        thrpt:  [+26.460% +29.427% +32.332%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
graph_scaling/nodes_within_hops/5000
                        time:   [117.34 µs 118.28 µs 119.29 µs]
                        thrpt:  [41.916 Melem/s 42.272 Melem/s 42.611 Melem/s]
                 change:
                        time:   [−24.734% −23.356% −21.846%] (p = 0.00 < 0.05)
                        thrpt:  [+27.953% +30.474% +32.862%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe

capability_search/find_with_gpu
                        time:   [27.921 µs 28.008 µs 28.100 µs]
                        thrpt:  [35.588 Kelem/s 35.704 Kelem/s 35.816 Kelem/s]
                 change:
                        time:   [−3.0025% −2.5875% −2.1738%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2221% +2.6562% +3.0955%]
                        Performance has improved.
capability_search/find_by_tool_python
                        time:   [60.943 µs 61.099 µs 61.260 µs]
                        thrpt:  [16.324 Kelem/s 16.367 Kelem/s 16.409 Kelem/s]
                 change:
                        time:   [−4.6557% −3.9548% −3.3554%] (p = 0.00 < 0.05)
                        thrpt:  [+3.4719% +4.1177% +4.8830%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_search/find_by_tool_rust
                        time:   [79.375 µs 79.585 µs 79.809 µs]
                        thrpt:  [12.530 Kelem/s 12.565 Kelem/s 12.598 Kelem/s]
                 change:
                        time:   [−4.9043% −4.5181% −4.0935%] (p = 0.00 < 0.05)
                        thrpt:  [+4.2682% +4.7319% +5.1572%]
                        Performance has improved.

graph_concurrent/concurrent_pingwave/4
                        time:   [112.58 µs 114.02 µs 115.52 µs]
                        thrpt:  [17.313 Melem/s 17.540 Melem/s 17.766 Melem/s]
                 change:
                        time:   [−27.028% −26.087% −25.153%] (p = 0.00 < 0.05)
                        thrpt:  [+33.606% +35.294% +37.039%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
graph_concurrent/concurrent_pingwave/8
                        time:   [179.73 µs 181.81 µs 185.00 µs]
                        thrpt:  [21.621 Melem/s 22.001 Melem/s 22.256 Melem/s]
                 change:
                        time:   [−31.359% −29.736% −28.038%] (p = 0.00 < 0.05)
                        thrpt:  [+38.963% +42.321% +45.686%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe
graph_concurrent/concurrent_pingwave/16
                        time:   [328.73 µs 330.00 µs 331.73 µs]
                        thrpt:  [24.116 Melem/s 24.242 Melem/s 24.336 Melem/s]
                 change:
                        time:   [−19.002% −17.605% −16.306%] (p = 0.00 < 0.05)
                        thrpt:  [+19.483% +21.366% +23.460%]
                        Performance has improved.
Found 4 outliers among 20 measurements (20.00%)
  4 (20.00%) high mild

path_finding/path_1_hop time:   [2.2947 µs 2.3011 µs 2.3071 µs]
                        thrpt:  [433.44 Kelem/s 434.58 Kelem/s 435.79 Kelem/s]
                 change:
                        time:   [−2.4619% −2.0880% −1.7225%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7527% +2.1325% +2.5241%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
path_finding/path_2_hops
                        time:   [2.3427 µs 2.3522 µs 2.3625 µs]
                        thrpt:  [423.29 Kelem/s 425.14 Kelem/s 426.86 Kelem/s]
                 change:
                        time:   [−2.9045% −2.5633% −2.2276%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2783% +2.6307% +2.9913%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
path_finding/path_4_hops
                        time:   [2.6444 µs 2.6512 µs 2.6587 µs]
                        thrpt:  [376.12 Kelem/s 377.19 Kelem/s 378.16 Kelem/s]
                 change:
                        time:   [−4.3171% −4.0855% −3.8534%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0078% +4.2595% +4.5118%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
path_finding/path_not_found
                        time:   [2.4106 µs 2.4163 µs 2.4228 µs]
                        thrpt:  [412.75 Kelem/s 413.86 Kelem/s 414.83 Kelem/s]
                 change:
                        time:   [−4.8677% −4.5979% −4.3385%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5353% +4.8195% +5.1167%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
path_finding/path_complex_graph
                        time:   [260.01 µs 260.47 µs 260.94 µs]
                        thrpt:  [3.8323 Kelem/s 3.8392 Kelem/s 3.8460 Kelem/s]
                 change:
                        time:   [−5.6328% −5.1034% −4.4845%] (p = 0.00 < 0.05)
                        thrpt:  [+4.6950% +5.3779% +5.9690%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

failure_detector/heartbeat_existing
                        time:   [39.962 ns 40.161 ns 40.362 ns]
                        thrpt:  [24.776 Melem/s 24.900 Melem/s 25.024 Melem/s]
                 change:
                        time:   [−3.8363% −2.8348% −1.8766%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9125% +2.9175% +3.9894%]
                        Performance has improved.
Found 19 outliers among 100 measurements (19.00%)
  9 (9.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
failure_detector/heartbeat_new
                        time:   [234.68 ns 238.24 ns 241.85 ns]
                        thrpt:  [4.1348 Melem/s 4.1974 Melem/s 4.2611 Melem/s]
                 change:
                        time:   [−30.287% −27.041% −23.923%] (p = 0.00 < 0.05)
                        thrpt:  [+31.446% +37.064% +43.446%]
                        Performance has improved.
failure_detector/status_check
                        time:   [14.739 ns 14.920 ns 15.104 ns]
                        thrpt:  [66.208 Melem/s 67.022 Melem/s 67.848 Melem/s]
                 change:
                        time:   [+0.4139% +2.6295% +4.9985%] (p = 0.02 < 0.05)
                        thrpt:  [−4.7606% −2.5621% −0.4122%]
                        Change within noise threshold.
failure_detector/check_all
                        time:   [12.123 µs 12.132 µs 12.145 µs]
                        thrpt:  [82.341 Kelem/s 82.424 Kelem/s 82.485 Kelem/s]
                 change:
                        time:   [−3.0094% −2.8592% −2.6420%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7137% +2.9433% +3.1028%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) high mild
  5 (5.00%) high severe
failure_detector/stats  time:   [10.556 µs 10.571 µs 10.593 µs]
                        thrpt:  [94.403 Kelem/s 94.603 Kelem/s 94.737 Kelem/s]
                 change:
                        time:   [−3.1404% −2.9824% −2.8212%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9031% +3.0741% +3.2422%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe

loss_simulator/should_drop_1pct
                        time:   [2.7852 ns 2.7871 ns 2.7893 ns]
                        thrpt:  [358.51 Melem/s 358.80 Melem/s 359.04 Melem/s]
                 change:
                        time:   [−4.3234% −3.9310% −3.5677%] (p = 0.00 < 0.05)
                        thrpt:  [+3.6997% +4.0919% +4.5188%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
loss_simulator/should_drop_5pct
                        time:   [3.1432 ns 3.1456 ns 3.1489 ns]
                        thrpt:  [317.57 Melem/s 317.90 Melem/s 318.15 Melem/s]
                 change:
                        time:   [−2.9031% −2.7607% −2.6197%] (p = 0.00 < 0.05)
                        thrpt:  [+2.6902% +2.8391% +2.9899%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
loss_simulator/should_drop_10pct
                        time:   [3.6087 ns 3.6117 ns 3.6152 ns]
                        thrpt:  [276.61 Melem/s 276.88 Melem/s 277.11 Melem/s]
                 change:
                        time:   [−3.1336% −2.9093% −2.6602%] (p = 0.00 < 0.05)
                        thrpt:  [+2.7329% +2.9965% +3.2349%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
loss_simulator/should_drop_20pct
                        time:   [4.5530 ns 4.5636 ns 4.5784 ns]
                        thrpt:  [218.42 Melem/s 219.12 Melem/s 219.64 Melem/s]
                 change:
                        time:   [−11.456% −5.0202% −1.2328%] (p = 0.11 > 0.05)
                        thrpt:  [+1.2482% +5.2855% +12.938%]
                        No change in performance detected.
Found 17 outliers among 100 measurements (17.00%)
  9 (9.00%) high mild
  8 (8.00%) high severe
loss_simulator/should_drop_burst
                        time:   [2.9238 ns 2.9319 ns 2.9420 ns]
                        thrpt:  [339.91 Melem/s 341.07 Melem/s 342.02 Melem/s]
                 change:
                        time:   [−2.4498% −2.1891% −1.8942%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9307% +2.2381% +2.5113%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

circuit_breaker/allow_closed
                        time:   [9.5066 ns 9.5131 ns 9.5193 ns]
                        thrpt:  [105.05 Melem/s 105.12 Melem/s 105.19 Melem/s]
                 change:
                        time:   [−2.8273% −2.6479% −2.4393%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5003% +2.7199% +2.9096%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
circuit_breaker/record_success
                        time:   [8.3747 ns 8.3864 ns 8.3986 ns]
                        thrpt:  [119.07 Melem/s 119.24 Melem/s 119.41 Melem/s]
                 change:
                        time:   [−0.4814% −0.0829% +0.2914%] (p = 0.69 > 0.05)
                        thrpt:  [−0.2906% +0.0830% +0.4838%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
circuit_breaker/record_failure
                        time:   [7.4123 ns 7.4172 ns 7.4230 ns]
                        thrpt:  [134.72 Melem/s 134.82 Melem/s 134.91 Melem/s]
                 change:
                        time:   [−0.3002% −0.0391% +0.2102%] (p = 0.77 > 0.05)
                        thrpt:  [−0.2098% +0.0391% +0.3011%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
circuit_breaker/state   time:   [9.5005 ns 9.5126 ns 9.5252 ns]
                        thrpt:  [104.99 Melem/s 105.12 Melem/s 105.26 Melem/s]
                 change:
                        time:   [−1.1868% −0.6125% −0.1118%] (p = 0.03 < 0.05)
                        thrpt:  [+0.1119% +0.6162% +1.2011%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

recovery_manager/on_failure_with_alternates
                        time:   [253.26 ns 261.41 ns 268.87 ns]
                        thrpt:  [3.7193 Melem/s 3.8254 Melem/s 3.9484 Melem/s]
                 change:
                        time:   [+1.0152% +5.9648% +11.064%] (p = 0.02 < 0.05)
                        thrpt:  [−9.9618% −5.6291% −1.0050%]
                        Performance has regressed.
recovery_manager/on_failure_no_alternates
                        time:   [273.28 ns 306.33 ns 337.94 ns]
                        thrpt:  [2.9591 Melem/s 3.2645 Melem/s 3.6593 Melem/s]
                 change:
                        time:   [+21.507% +44.528% +66.102%] (p = 0.00 < 0.05)
                        thrpt:  [−39.796% −30.809% −17.700%]
                        Performance has regressed.
Found 15 outliers among 100 measurements (15.00%)
  14 (14.00%) high mild
  1 (1.00%) high severe
recovery_manager/get_action
                        time:   [37.031 ns 37.054 ns 37.082 ns]
                        thrpt:  [26.967 Melem/s 26.987 Melem/s 27.005 Melem/s]
                 change:
                        time:   [+0.0194% +0.1966% +0.3978%] (p = 0.03 < 0.05)
                        thrpt:  [−0.3962% −0.1962% −0.0194%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
recovery_manager/is_failed
                        time:   [14.336 ns 14.708 ns 15.037 ns]
                        thrpt:  [66.504 Melem/s 67.990 Melem/s 69.754 Melem/s]
                 change:
                        time:   [−5.9225% −2.7424% +0.3097%] (p = 0.08 > 0.05)
                        thrpt:  [−0.3088% +2.8198% +6.2953%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
recovery_manager/on_recovery
                        time:   [99.276 ns 99.431 ns 99.617 ns]
                        thrpt:  [10.038 Melem/s 10.057 Melem/s 10.073 Melem/s]
                 change:
                        time:   [−3.2172% −2.5156% −1.8107%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8440% +2.5805% +3.3242%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
recovery_manager/stats  time:   [698.84 ps 699.63 ps 700.64 ps]
                        thrpt:  [1.4273 Gelem/s 1.4293 Gelem/s 1.4309 Gelem/s]
                 change:
                        time:   [−0.0312% +0.1566% +0.4063%] (p = 0.19 > 0.05)
                        thrpt:  [−0.4047% −0.1563% +0.0312%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe

failure_scaling/check_all/100
                        time:   [2.5132 µs 2.6301 µs 2.8724 µs]
                        thrpt:  [34.814 Melem/s 38.022 Melem/s 39.789 Melem/s]
                 change:
                        time:   [+1.5805% +5.5644% +12.778%] (p = 0.04 < 0.05)
                        thrpt:  [−11.330% −5.2711% −1.5559%]
                        Performance has regressed.
Found 33 outliers among 100 measurements (33.00%)
  19 (19.00%) low severe
  8 (8.00%) high mild
  6 (6.00%) high severe
failure_scaling/healthy_nodes/100
                        time:   [2.1585 µs 2.1615 µs 2.1649 µs]
                        thrpt:  [46.191 Melem/s 46.264 Melem/s 46.329 Melem/s]
                 change:
                        time:   [+1.4080% +1.7600% +2.0783%] (p = 0.00 < 0.05)
                        thrpt:  [−2.0359% −1.7296% −1.3885%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
failure_scaling/check_all/500
                        time:   [6.4645 µs 6.5102 µs 6.5508 µs]
                        thrpt:  [76.326 Melem/s 76.802 Melem/s 77.346 Melem/s]
                 change:
                        time:   [−0.6436% +0.3145% +1.3126%] (p = 0.52 > 0.05)
                        thrpt:  [−1.2956% −0.3135% +0.6478%]
                        No change in performance detected.
failure_scaling/healthy_nodes/500
                        time:   [6.1357 µs 6.1415 µs 6.1491 µs]
                        thrpt:  [81.313 Melem/s 81.413 Melem/s 81.490 Melem/s]
                 change:
                        time:   [−0.5263% −0.3349% −0.1153%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1154% +0.3361% +0.5291%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
failure_scaling/check_all/1000
                        time:   [11.785 µs 11.873 µs 11.944 µs]
                        thrpt:  [83.724 Melem/s 84.223 Melem/s 84.853 Melem/s]
                 change:
                        time:   [−5.0519% −4.3868% −3.7381%] (p = 0.00 < 0.05)
                        thrpt:  [+3.8832% +4.5881% +5.3207%]
                        Performance has improved.
failure_scaling/healthy_nodes/1000
                        time:   [10.994 µs 11.009 µs 11.024 µs]
                        thrpt:  [90.710 Melem/s 90.839 Melem/s 90.961 Melem/s]
                 change:
                        time:   [+4.1045% +4.2503% +4.3945%] (p = 0.00 < 0.05)
                        thrpt:  [−4.2096% −4.0770% −3.9427%]
                        Performance has regressed.
failure_scaling/check_all/5000
                        time:   [54.660 µs 55.028 µs 55.312 µs]
                        thrpt:  [90.397 Melem/s 90.863 Melem/s 91.475 Melem/s]
                 change:
                        time:   [−1.3059% −0.1675% +1.0135%] (p = 0.78 > 0.05)
                        thrpt:  [−1.0033% +0.1678% +1.3231%]
                        No change in performance detected.
failure_scaling/healthy_nodes/5000
                        time:   [50.710 µs 50.869 µs 51.036 µs]
                        thrpt:  [97.970 Melem/s 98.292 Melem/s 98.601 Melem/s]
                 change:
                        time:   [−0.5946% −0.2494% +0.0998%] (p = 0.15 > 0.05)
                        thrpt:  [−0.0997% +0.2500% +0.5982%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

failure_concurrent/concurrent_heartbeat/4
                        time:   [197.00 µs 199.25 µs 202.15 µs]
                        thrpt:  [9.8934 Melem/s 10.038 Melem/s 10.152 Melem/s]
                 change:
                        time:   [+1.9996% +2.7616% +3.6333%] (p = 0.00 < 0.05)
                        thrpt:  [−3.5059% −2.6874% −1.9604%]
                        Performance has regressed.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
  1 (5.00%) high severe
failure_concurrent/concurrent_heartbeat/8
                        time:   [260.34 µs 263.93 µs 268.96 µs]
                        thrpt:  [14.872 Melem/s 15.155 Melem/s 15.365 Melem/s]
                 change:
                        time:   [+0.2960% +3.0406% +7.8455%] (p = 0.17 > 0.05)
                        thrpt:  [−7.2748% −2.9509% −0.2951%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
failure_concurrent/concurrent_heartbeat/16
                        time:   [470.14 µs 514.36 µs 601.91 µs]
                        thrpt:  [13.291 Melem/s 15.553 Melem/s 17.016 Melem/s]
                 change:
                        time:   [−12.732% −1.8227% +10.895%] (p = 0.79 > 0.05)
                        thrpt:  [−9.8242% +1.8566% +14.589%]
                        No change in performance detected.
Found 4 outliers among 20 measurements (20.00%)
  2 (10.00%) low mild
  2 (10.00%) high severe

failure_recovery_cycle/full_cycle
                        time:   [282.22 ns 289.38 ns 296.09 ns]
                        thrpt:  [3.3774 Melem/s 3.4557 Melem/s 3.5433 Melem/s]
                 change:
                        time:   [−16.010% −12.340% −8.7033%] (p = 0.00 < 0.05)
                        thrpt:  [+9.5330% +14.077% +19.062%]
                        Performance has improved.

capability_set/create   time:   [18.950 µs 18.963 µs 18.977 µs]
                        thrpt:  [52.696 Kelem/s 52.733 Kelem/s 52.771 Kelem/s]
                 change:
                        time:   [−15.144% −9.1708% −4.3725%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5724% +10.097% +17.847%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
capability_set/serialize
                        time:   [10.604 µs 10.615 µs 10.628 µs]
                        thrpt:  [94.094 Kelem/s 94.208 Kelem/s 94.308 Kelem/s]
                 change:
                        time:   [−1.3126% −1.1007% −0.8919%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9000% +1.1129% +1.3301%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
capability_set/deserialize
                        time:   [10.091 µs 10.263 µs 10.485 µs]
                        thrpt:  [95.372 Kelem/s 97.434 Kelem/s 99.098 Kelem/s]
                 change:
                        time:   [−0.9257% −0.2066% +0.7903%] (p = 0.67 > 0.05)
                        thrpt:  [−0.7841% +0.2070% +0.9343%]
                        No change in performance detected.
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) high mild
  12 (12.00%) high severe
capability_set/roundtrip
                        time:   [21.039 µs 21.171 µs 21.312 µs]
                        thrpt:  [46.922 Kelem/s 47.234 Kelem/s 47.530 Kelem/s]
                 change:
                        time:   [−0.9013% −0.5093% −0.0311%] (p = 0.01 < 0.05)
                        thrpt:  [+0.0311% +0.5119% +0.9095%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
capability_set/serialize_compact
                        time:   [2.6925 µs 2.7019 µs 2.7130 µs]
                        thrpt:  [368.60 Kelem/s 370.10 Kelem/s 371.41 Kelem/s]
                 change:
                        time:   [−0.2763% +0.2211% +0.7424%] (p = 0.40 > 0.05)
                        thrpt:  [−0.7369% −0.2206% +0.2770%]
                        No change in performance detected.
capability_set/deserialize_compact
                        time:   [7.4359 µs 7.9874 µs 9.1661 µs]
                        thrpt:  [109.10 Kelem/s 125.20 Kelem/s 134.48 Kelem/s]
                 change:
                        time:   [−1.1896% +2.9818% +10.802%] (p = 0.65 > 0.05)
                        thrpt:  [−9.7487% −2.8955% +1.2039%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
capability_set/roundtrip_compact
                        time:   [9.8008 µs 9.8210 µs 9.8428 µs]
                        thrpt:  [101.60 Kelem/s 101.82 Kelem/s 102.03 Kelem/s]
                 change:
                        time:   [−3.8809% −3.1541% −2.5324%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5982% +3.2569% +4.0376%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  10 (10.00%) high mild
  1 (1.00%) high severe
capability_set/has_tag  time:   [85.139 ns 86.724 ns 88.897 ns]
                        thrpt:  [11.249 Melem/s 11.531 Melem/s 11.746 Melem/s]
                 change:
                        time:   [+0.2955% +1.1249% +2.3350%] (p = 0.01 < 0.05)
                        thrpt:  [−2.2817% −1.1124% −0.2947%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) high mild
  11 (11.00%) high severe
capability_set/has_model
                        time:   [39.203 ns 39.317 ns 39.464 ns]
                        thrpt:  [25.340 Melem/s 25.434 Melem/s 25.508 Melem/s]
                 change:
                        time:   [+8.7039% +8.9375% +9.1660%] (p = 0.00 < 0.05)
                        thrpt:  [−8.3963% −8.2043% −8.0070%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  6 (6.00%) high mild
  10 (10.00%) high severe
capability_set/has_tool time:   [21.731 ns 21.743 ns 21.757 ns]
                        thrpt:  [45.962 Melem/s 45.992 Melem/s 46.016 Melem/s]
                 change:
                        time:   [−38.113% −37.999% −37.887%] (p = 0.00 < 0.05)
                        thrpt:  [+60.996% +61.287% +61.585%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe
capability_set/has_gpu  time:   [39.599 ns 39.632 ns 39.669 ns]
                        thrpt:  [25.209 Melem/s 25.232 Melem/s 25.253 Melem/s]
                 change:
                        time:   [−2.1101% −1.3133% −0.8164%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8231% +1.3307% +2.1556%]
                        Change within noise threshold.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

capability_announcement/create
                        time:   [3.4142 µs 3.4279 µs 3.4405 µs]
                        thrpt:  [290.65 Kelem/s 291.73 Kelem/s 292.89 Kelem/s]
                 change:
                        time:   [−0.6642% −0.0633% +0.5223%] (p = 0.84 > 0.05)
                        thrpt:  [−0.5196% +0.0634% +0.6687%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
capability_announcement/serialize
                        time:   [11.062 µs 11.090 µs 11.124 µs]
                        thrpt:  [89.896 Kelem/s 90.169 Kelem/s 90.403 Kelem/s]
                 change:
                        time:   [+0.8408% +1.1003% +1.3585%] (p = 0.00 < 0.05)
                        thrpt:  [−1.3403% −1.0883% −0.8338%]
                        Change within noise threshold.
capability_announcement/deserialize
                        time:   [10.334 µs 10.345 µs 10.357 µs]
                        thrpt:  [96.549 Kelem/s 96.661 Kelem/s 96.768 Kelem/s]
                 change:
                        time:   [−0.4363% −0.2587% −0.0880%] (p = 0.00 < 0.05)
                        thrpt:  [+0.0881% +0.2594% +0.4382%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
capability_announcement/is_expired
                        time:   [25.186 ns 25.215 ns 25.251 ns]
                        thrpt:  [39.602 Melem/s 39.658 Melem/s 39.704 Melem/s]
                 change:
                        time:   [+0.0336% +0.1832% +0.3505%] (p = 0.02 < 0.05)
                        thrpt:  [−0.3492% −0.1828% −0.0336%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe

capability_filter/match_single_tag
                        time:   [68.960 ns 69.007 ns 69.067 ns]
                        thrpt:  [14.479 Melem/s 14.491 Melem/s 14.501 Melem/s]
                 change:
                        time:   [+74.773% +74.974% +75.157%] (p = 0.00 < 0.05)
                        thrpt:  [−42.908% −42.849% −42.783%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
capability_filter/match_require_gpu
                        time:   [46.724 ns 46.766 ns 46.814 ns]
                        thrpt:  [21.361 Melem/s 21.383 Melem/s 21.402 Melem/s]
                 change:
                        time:   [+0.3472% +0.7513% +1.1710%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1575% −0.7457% −0.3460%]
                        Change within noise threshold.
Found 14 outliers among 100 measurements (14.00%)
  14 (14.00%) high severe
capability_filter/match_gpu_vendor
                        time:   [145.41 ns 147.20 ns 149.03 ns]
                        thrpt:  [6.7100 Melem/s 6.7936 Melem/s 6.8770 Melem/s]
                 change:
                        time:   [−6.7308% −5.6099% −4.7023%] (p = 0.00 < 0.05)
                        thrpt:  [+4.9343% +5.9433% +7.2165%]
                        Performance has improved.
capability_filter/match_min_memory
                        time:   [30.334 ns 30.843 ns 31.486 ns]
                        thrpt:  [31.760 Melem/s 32.422 Melem/s 32.966 Melem/s]
                 change:
                        time:   [+57.646% +67.055% +73.148%] (p = 0.00 < 0.05)
                        thrpt:  [−42.246% −40.139% −36.567%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
capability_filter/match_complex
                        time:   [4.6572 µs 4.6699 µs 4.6836 µs]
                        thrpt:  [213.51 Kelem/s 214.14 Kelem/s 214.72 Kelem/s]
                 change:
                        time:   [−2.7528% −2.1710% −1.5952%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6211% +2.2192% +2.8307%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_filter/match_no_match
                        time:   [83.171 ns 83.239 ns 83.315 ns]
                        thrpt:  [12.003 Melem/s 12.014 Melem/s 12.023 Melem/s]
                 change:
                        time:   [−3.0653% −2.9786% −2.8742%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9592% +3.0700% +3.1622%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

capability_fold_insert/index_nodes/100
                        time:   [4.0660 ms 4.2703 ms 4.6557 ms]
                        thrpt:  [21.479 Kelem/s 23.417 Kelem/s 24.594 Kelem/s]
                 change:
                        time:   [+0.9042% +6.0210% +15.501%] (p = 0.18 > 0.05)
                        thrpt:  [−13.421% −5.6790% −0.8961%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_insert/index_nodes/1000
                        time:   [40.406 ms 40.508 ms 40.619 ms]
                        thrpt:  [24.619 Kelem/s 24.687 Kelem/s 24.749 Kelem/s]
                 change:
                        time:   [−2.0053% −0.9490% −0.0909%] (p = 0.05 < 0.05)
                        thrpt:  [+0.0910% +0.9581% +2.0463%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 42.3s, or reduce sample count to 10.
capability_fold_insert/index_nodes/10000
                        time:   [424.71 ms 427.56 ms 430.89 ms]
                        thrpt:  [23.208 Kelem/s 23.389 Kelem/s 23.545 Kelem/s]
                 change:
                        time:   [−9.1902% −6.8584% −4.6417%] (p = 0.00 < 0.05)
                        thrpt:  [+4.8677% +7.3634% +10.120%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  5 (5.00%) high mild
  12 (12.00%) high severe

capability_fold_query/query_single_tag
                        time:   [110.00 µs 116.28 µs 129.86 µs]
                        thrpt:  [7.7009 Kelem/s 8.5999 Kelem/s 9.0909 Kelem/s]
                 change:
                        time:   [−4.6351% −0.8760% +5.8338%] (p = 0.83 > 0.05)
                        thrpt:  [−5.5122% +0.8838% +4.8604%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) high mild
  14 (14.00%) high severe
capability_fold_query/query_require_gpu
                        time:   [261.69 µs 262.12 µs 262.66 µs]
                        thrpt:  [3.8073 Kelem/s 3.8151 Kelem/s 3.8213 Kelem/s]
                 change:
                        time:   [−41.526% −37.886% −34.155%] (p = 0.00 < 0.05)
                        thrpt:  [+51.872% +60.994% +71.017%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
capability_fold_query/query_gpu_vendor
                        time:   [353.13 µs 365.96 µs 380.45 µs]
                        thrpt:  [2.6284 Kelem/s 2.7325 Kelem/s 2.8318 Kelem/s]
                 change:
                        time:   [−36.841% −33.042% −29.022%] (p = 0.00 < 0.05)
                        thrpt:  [+40.888% +49.348% +58.331%]
                        Performance has improved.
capability_fold_query/query_min_memory
                        time:   [349.08 µs 362.37 µs 376.62 µs]
                        thrpt:  [2.6552 Kelem/s 2.7596 Kelem/s 2.8647 Kelem/s]
                 change:
                        time:   [−33.265% −30.069% −26.746%] (p = 0.00 < 0.05)
                        thrpt:  [+36.511% +42.999% +49.846%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
capability_fold_query/query_complex
                        time:   [223.08 µs 238.49 µs 255.05 µs]
                        thrpt:  [3.9209 Kelem/s 4.1930 Kelem/s 4.4826 Kelem/s]
                 change:
                        time:   [−53.377% −48.780% −43.675%] (p = 0.00 < 0.05)
                        thrpt:  [+77.541% +95.237% +114.49%]
                        Performance has improved.
Found 21 outliers among 100 measurements (21.00%)
  3 (3.00%) high mild
  18 (18.00%) high severe
capability_fold_query/query_model
                        time:   [71.677 µs 71.715 µs 71.754 µs]
                        thrpt:  [13.936 Kelem/s 13.944 Kelem/s 13.952 Kelem/s]
                 change:
                        time:   [−2.0753% −1.7195% −1.3527%] (p = 0.00 < 0.05)
                        thrpt:  [+1.3713% +1.7495% +2.1193%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
capability_fold_query/query_tool
                        time:   [286.87 µs 305.03 µs 324.08 µs]
                        thrpt:  [3.0857 Kelem/s 3.2784 Kelem/s 3.4859 Kelem/s]
                 change:
                        time:   [−29.381% −25.796% −22.069%] (p = 0.00 < 0.05)
                        thrpt:  [+28.319% +34.763% +41.604%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  17 (17.00%) high severe
capability_fold_query/query_no_results
                        time:   [89.246 ns 90.336 ns 91.415 ns]
                        thrpt:  [10.939 Melem/s 11.070 Melem/s 11.205 Melem/s]
                 change:
                        time:   [+0.4514% +1.5739% +2.6973%] (p = 0.00 < 0.05)
                        thrpt:  [−2.6265% −1.5495% −0.4494%]
                        Change within noise threshold.

capability_fold_find_best/find_best_simple
                        time:   [308.49 µs 324.89 µs 342.54 µs]
                        thrpt:  [2.9194 Kelem/s 3.0780 Kelem/s 3.2416 Kelem/s]
                 change:
                        time:   [−23.192% −18.206% −12.850%] (p = 0.00 < 0.05)
                        thrpt:  [+14.745% +22.259% +30.195%]
                        Performance has improved.
capability_fold_find_best/find_best_with_prefs
                        time:   [489.54 µs 518.29 µs 543.55 µs]
                        thrpt:  [1.8398 Kelem/s 1.9294 Kelem/s 2.0427 Kelem/s]
                 change:
                        time:   [+38.870% +54.739% +72.770%] (p = 0.00 < 0.05)
                        thrpt:  [−42.120% −35.375% −27.990%]
                        Performance has regressed.
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) low mild

capability_fold_scaling/query_tag/1000
                        time:   [9.4210 µs 9.4307 µs 9.4419 µs]
                        thrpt:  [52.955 Melem/s 53.019 Melem/s 53.073 Melem/s]
                 change:
                        time:   [−3.8489% −3.7065% −3.5547%] (p = 0.00 < 0.05)
                        thrpt:  [+3.6857% +3.8492% +4.0030%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
capability_fold_scaling/query_complex/1000
                        time:   [18.570 µs 18.578 µs 18.586 µs]
                        thrpt:  [24.319 Melem/s 24.330 Melem/s 24.340 Melem/s]
                 change:
                        time:   [−3.1966% −3.0322% −2.8563%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9402% +3.1270% +3.3022%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_tag_rare/1000
                        time:   [1.9068 µs 1.9101 µs 1.9143 µs]
                        thrpt:  [52.239 Melem/s 52.353 Melem/s 52.445 Melem/s]
                 change:
                        time:   [−3.3961% −3.2314% −3.0612%] (p = 0.00 < 0.05)
                        thrpt:  [+3.1579% +3.3393% +3.5155%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
capability_fold_scaling/query_tag/5000
                        time:   [55.883 µs 56.791 µs 57.851 µs]
                        thrpt:  [43.214 Melem/s 44.021 Melem/s 44.737 Melem/s]
                 change:
                        time:   [−2.8789% −1.2090% +1.6058%] (p = 0.34 > 0.05)
                        thrpt:  [−1.5805% +1.2238% +2.9642%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
capability_fold_scaling/query_complex/5000
                        time:   [163.76 µs 170.67 µs 178.00 µs]
                        thrpt:  [12.714 Melem/s 13.260 Melem/s 13.819 Melem/s]
                 change:
                        time:   [−31.761% −22.000% −9.6624%] (p = 0.00 < 0.05)
                        thrpt:  [+10.696% +28.206% +46.543%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
capability_fold_scaling/query_tag_rare/5000
                        time:   [1.9883 µs 2.0018 µs 2.0216 µs]
                        thrpt:  [49.467 Melem/s 49.955 Melem/s 50.294 Melem/s]
                 change:
                        time:   [+0.5609% +1.3166% +2.1270%] (p = 0.00 < 0.05)
                        thrpt:  [−2.0827% −1.2995% −0.5578%]
                        Change within noise threshold.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
capability_fold_scaling/query_tag/10000
                        time:   [116.27 µs 118.39 µs 120.85 µs]
                        thrpt:  [41.372 Melem/s 42.233 Melem/s 43.004 Melem/s]
                 change:
                        time:   [−0.0672% +1.4182% +3.3488%] (p = 0.10 > 0.05)
                        thrpt:  [−3.2403% −1.3983% +0.0673%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe
capability_fold_scaling/query_complex/10000
                        time:   [426.65 µs 446.29 µs 464.77 µs]
                        thrpt:  [9.7446 Melem/s 10.148 Melem/s 10.615 Melem/s]
                 change:
                        time:   [−3.4591% +4.5046% +13.513%] (p = 0.30 > 0.05)
                        thrpt:  [−11.904% −4.3104% +3.5830%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_fold_scaling/query_tag_rare/10000
                        time:   [1.9857 µs 1.9994 µs 2.0207 µs]
                        thrpt:  [49.487 Melem/s 50.014 Melem/s 50.360 Melem/s]
                 change:
                        time:   [−0.0236% +0.9424% +1.9155%] (p = 0.06 > 0.05)
                        thrpt:  [−1.8795% −0.9336% +0.0236%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
capability_fold_scaling/query_tag/50000
                        time:   [733.49 µs 738.24 µs 744.85 µs]
                        thrpt:  [33.564 Melem/s 33.864 Melem/s 34.084 Melem/s]
                 change:
                        time:   [−26.431% −24.910% −23.260%] (p = 0.00 < 0.05)
                        thrpt:  [+30.310% +33.173% +35.927%]
                        Performance has improved.
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low mild
  10 (10.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_complex/50000
                        time:   [2.1944 ms 2.2312 ms 2.2682 ms]
                        thrpt:  [9.9877 Melem/s 10.153 Melem/s 10.323 Melem/s]
                 change:
                        time:   [−1.8539% +1.3013% +4.6941%] (p = 0.44 > 0.05)
                        thrpt:  [−4.4836% −1.2846% +1.8890%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
capability_fold_scaling/query_tag_rare/50000
                        time:   [1.9857 µs 1.9921 µs 2.0014 µs]
                        thrpt:  [49.964 Melem/s 50.197 Melem/s 50.361 Melem/s]
                 change:
                        time:   [+1.2041% +1.6205% +2.1189%] (p = 0.00 < 0.05)
                        thrpt:  [−2.0749% −1.5947% −1.1898%]
                        Performance has regressed.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe

capability_fold_concurrent/concurrent_index/4
                        time:   [15.758 ms 15.887 ms 16.057 ms]
                        thrpt:  [124.56 Kelem/s 125.89 Kelem/s 126.92 Kelem/s]
                 change:
                        time:   [+0.9847% +3.5389% +7.9928%] (p = 0.05 > 0.05)
                        thrpt:  [−7.4013% −3.4179% −0.9751%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
capability_fold_concurrent/concurrent_query/4
                        time:   [192.47 ms 198.70 ms 205.06 ms]
                        thrpt:  [9.7532 Kelem/s 10.065 Kelem/s 10.391 Kelem/s]
                 change:
                        time:   [+7.1548% +15.141% +23.523%] (p = 0.00 < 0.05)
                        thrpt:  [−19.043% −13.150% −6.6771%]
                        Performance has regressed.
capability_fold_concurrent/concurrent_mixed/4
                        time:   [61.027 ms 61.532 ms 62.220 ms]
                        thrpt:  [32.144 Kelem/s 32.504 Kelem/s 32.772 Kelem/s]
                 change:
                        time:   [−8.7646% −7.0330% −5.5524%] (p = 0.00 < 0.05)
                        thrpt:  [+5.8789% +7.5650% +9.6066%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high severe
capability_fold_concurrent/concurrent_index/8
                        time:   [16.710 ms 17.114 ms 17.504 ms]
                        thrpt:  [228.51 Kelem/s 233.73 Kelem/s 239.38 Kelem/s]
                 change:
                        time:   [−16.442% −15.125% −13.664%] (p = 0.00 < 0.05)
                        thrpt:  [+15.827% +17.821% +19.678%]
                        Performance has improved.
capability_fold_concurrent/concurrent_query/8
                        time:   [163.91 ms 172.78 ms 184.89 ms]
                        thrpt:  [21.635 Kelem/s 23.151 Kelem/s 24.404 Kelem/s]
                 change:
                        time:   [−17.810% −13.507% −7.6667%] (p = 0.00 < 0.05)
                        thrpt:  [+8.3033% +15.616% +21.669%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_mixed/8
                        time:   [78.260 ms 79.007 ms 79.810 ms]
                        thrpt:  [50.119 Kelem/s 50.629 Kelem/s 51.111 Kelem/s]
                 change:
                        time:   [−19.467% −16.763% −14.266%] (p = 0.00 < 0.05)
                        thrpt:  [+16.640% +20.139% +24.172%]
                        Performance has improved.
Benchmarking capability_fold_concurrent/concurrent_index/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 8.0s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/16
                        time:   [36.334 ms 36.655 ms 37.081 ms]
                        thrpt:  [215.75 Kelem/s 218.25 Kelem/s 220.18 Kelem/s]
                 change:
                        time:   [−9.5353% −4.5805% −0.8273%] (p = 0.05 < 0.05)
                        thrpt:  [+0.8342% +4.8004% +10.540%]
                        Change within noise threshold.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 6.1s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/16
                        time:   [292.64 ms 308.30 ms 325.30 ms]
                        thrpt:  [24.593 Kelem/s 25.949 Kelem/s 27.338 Kelem/s]
                 change:
                        time:   [−28.201% −23.548% −19.229%] (p = 0.00 < 0.05)
                        thrpt:  [+23.807% +30.801% +39.278%]
                        Performance has improved.
capability_fold_concurrent/concurrent_mixed/16
                        time:   [187.54 ms 190.00 ms 192.83 ms]
                        thrpt:  [41.488 Kelem/s 42.105 Kelem/s 42.658 Kelem/s]
                 change:
                        time:   [−7.2591% −5.8168% −3.9185%] (p = 0.00 < 0.05)
                        thrpt:  [+4.0783% +6.1760% +7.8273%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

capability_fold_updates/update_higher_version
                        time:   [30.904 µs 31.222 µs 31.692 µs]
                        thrpt:  [31.554 Kelem/s 32.029 Kelem/s 32.359 Kelem/s]
                 change:
                        time:   [+2.2877% +3.1286% +4.0263%] (p = 0.00 < 0.05)
                        thrpt:  [−3.8705% −3.0337% −2.2366%]
                        Performance has regressed.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low mild
  11 (11.00%) high mild
  1 (1.00%) high severe
capability_fold_updates/update_same_version
                        time:   [31.062 µs 31.412 µs 31.885 µs]
                        thrpt:  [31.363 Kelem/s 31.835 Kelem/s 32.194 Kelem/s]
                 change:
                        time:   [+3.7948% +4.7666% +5.8293%] (p = 0.00 < 0.05)
                        thrpt:  [−5.5082% −4.5497% −3.6561%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
capability_fold_updates/remove_and_readd
                        time:   [51.045 µs 51.634 µs 52.301 µs]
                        thrpt:  [19.120 Kelem/s 19.367 Kelem/s 19.590 Kelem/s]
                 change:
                        time:   [+7.6124% +9.2416% +10.922%] (p = 0.00 < 0.05)
                        thrpt:  [−9.8464% −8.4598% −7.0739%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe

location_info/create    time:   [58.287 ns 58.438 ns 58.598 ns]
                        thrpt:  [17.065 Melem/s 17.112 Melem/s 17.156 Melem/s]
                 change:
                        time:   [−4.0154% −3.5587% −3.0930%] (p = 0.00 < 0.05)
                        thrpt:  [+3.1917% +3.6900% +4.1833%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
location_info/distance_to
                        time:   [4.3046 ns 4.3094 ns 4.3146 ns]
                        thrpt:  [231.77 Melem/s 232.05 Melem/s 232.31 Melem/s]
                 change:
                        time:   [−2.0428% −1.7859% −1.5211%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5446% +1.8184% +2.0854%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
location_info/same_continent
                        time:   [7.3224 ns 7.3988 ns 7.5147 ns]
                        thrpt:  [133.07 Melem/s 135.16 Melem/s 136.57 Melem/s]
                 change:
                        time:   [−0.4472% +0.7990% +2.5632%] (p = 0.35 > 0.05)
                        thrpt:  [−2.4992% −0.7926% +0.4492%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
location_info/same_continent_cross
                        time:   [317.14 ps 319.39 ps 323.84 ps]
                        thrpt:  [3.0880 Gelem/s 3.1310 Gelem/s 3.1532 Gelem/s]
                 change:
                        time:   [−1.0898% −0.5979% +0.1975%] (p = 0.07 > 0.05)
                        thrpt:  [−0.1971% +0.6015% +1.1019%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
location_info/same_region
                        time:   [4.1218 ns 4.1488 ns 4.2027 ns]
                        thrpt:  [237.94 Melem/s 241.03 Melem/s 242.61 Melem/s]
                 change:
                        time:   [−2.4878% −1.9083% −0.8615%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8690% +1.9454% +2.5513%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe

topology_hints/create   time:   [3.1807 ns 3.2070 ns 3.2603 ns]
                        thrpt:  [306.72 Melem/s 311.82 Melem/s 314.40 Melem/s]
                 change:
                        time:   [−2.1768% −1.4766% −0.4564%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4585% +1.4987% +2.2252%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
topology_hints/connectivity_score
                        time:   [311.24 ps 313.08 ps 316.15 ps]
                        thrpt:  [3.1631 Gelem/s 3.1941 Gelem/s 3.2130 Gelem/s]
                 change:
                        time:   [−2.6378% −1.7379% −0.3040%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3049% +1.7687% +2.7093%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
topology_hints/average_latency_empty
                        time:   [620.91 ps 621.63 ps 622.71 ps]
                        thrpt:  [1.6059 Gelem/s 1.6087 Gelem/s 1.6105 Gelem/s]
                 change:
                        time:   [−4.7054% −4.5158% −4.3121%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5065% +4.7294% +4.9377%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  9 (9.00%) high mild
  7 (7.00%) high severe
topology_hints/average_latency_100
                        time:   [70.483 ns 70.597 ns 70.727 ns]
                        thrpt:  [14.139 Melem/s 14.165 Melem/s 14.188 Melem/s]
                 change:
                        time:   [−4.0977% −3.7256% −3.4021%] (p = 0.00 < 0.05)
                        thrpt:  [+3.5220% +3.8698% +4.2727%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  6 (6.00%) high mild
  12 (12.00%) high severe

nat_type/difficulty     time:   [310.50 ps 310.83 ps 311.36 ps]
                        thrpt:  [3.2117 Gelem/s 3.2172 Gelem/s 3.2207 Gelem/s]
                 change:
                        time:   [−2.9661% −1.8534% +1.2779%] (p = 0.04 < 0.05)
                        thrpt:  [−1.2618% +1.8883% +3.0568%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
nat_type/can_connect_direct
                        time:   [310.51 ps 310.76 ps 311.08 ps]
                        thrpt:  [3.2146 Gelem/s 3.2179 Gelem/s 3.2205 Gelem/s]
                 change:
                        time:   [−0.1154% +0.0059% +0.1391%] (p = 0.93 > 0.05)
                        thrpt:  [−0.1389% −0.0059% +0.1156%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high severe
nat_type/can_connect_symmetric
                        time:   [310.49 ps 310.68 ps 310.90 ps]
                        thrpt:  [3.2165 Gelem/s 3.2188 Gelem/s 3.2207 Gelem/s]
                 change:
                        time:   [−4.3138% −3.1634% −2.2923%] (p = 0.00 < 0.05)
                        thrpt:  [+2.3460% +3.2667% +4.5083%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe

node_metadata/create_simple
                        time:   [51.022 ns 51.351 ns 51.704 ns]
                        thrpt:  [19.341 Melem/s 19.474 Melem/s 19.600 Melem/s]
                 change:
                        time:   [−2.8829% −2.5718% −2.1715%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2197% +2.6397% +2.9684%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
node_metadata/create_full
                        time:   [598.20 ns 599.82 ns 601.62 ns]
                        thrpt:  [1.6622 Melem/s 1.6672 Melem/s 1.6717 Melem/s]
                 change:
                        time:   [−1.9104% −1.5728% −1.2153%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2303% +1.5979% +1.9476%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
node_metadata/routing_score
                        time:   [2.9004 ns 2.9159 ns 2.9464 ns]
                        thrpt:  [339.40 Melem/s 342.94 Melem/s 344.78 Melem/s]
                 change:
                        time:   [−4.4445% −3.9750% −3.1279%] (p = 0.00 < 0.05)
                        thrpt:  [+3.2289% +4.1395% +4.6512%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  13 (13.00%) high mild
  2 (2.00%) high severe
node_metadata/age       time:   [27.434 ns 27.449 ns 27.468 ns]
                        thrpt:  [36.407 Melem/s 36.432 Melem/s 36.451 Melem/s]
                 change:
                        time:   [−4.3031% −4.1661% −4.0248%] (p = 0.00 < 0.05)
                        thrpt:  [+4.1936% +4.3472% +4.4966%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  8 (8.00%) high mild
  8 (8.00%) high severe
node_metadata/is_stale  time:   [25.773 ns 25.811 ns 25.878 ns]
                        thrpt:  [38.643 Melem/s 38.743 Melem/s 38.800 Melem/s]
                 change:
                        time:   [−3.0400% −2.7578% −2.4560%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5178% +2.8360% +3.1353%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
node_metadata/serialize time:   [775.30 ns 785.23 ns 796.76 ns]
                        thrpt:  [1.2551 Melem/s 1.2735 Melem/s 1.2898 Melem/s]
                 change:
                        time:   [−5.2581% −4.4782% −3.6925%] (p = 0.00 < 0.05)
                        thrpt:  [+3.8341% +4.6881% +5.5499%]
                        Performance has improved.
Found 25 outliers among 100 measurements (25.00%)
  10 (10.00%) low mild
  3 (3.00%) high mild
  12 (12.00%) high severe
node_metadata/deserialize
                        time:   [1.7248 µs 1.7375 µs 1.7496 µs]
                        thrpt:  [571.56 Kelem/s 575.53 Kelem/s 579.77 Kelem/s]
                 change:
                        time:   [−2.3786% −1.1171% −0.0690%] (p = 0.05 > 0.05)
                        thrpt:  [+0.0690% +1.1297% +2.4365%]
                        No change in performance detected.

metadata_query/match_status
                        time:   [3.7247 ns 3.7270 ns 3.7298 ns]
                        thrpt:  [268.11 Melem/s 268.31 Melem/s 268.48 Melem/s]
                 change:
                        time:   [+5.7330% +5.8360% +5.9310%] (p = 0.00 < 0.05)
                        thrpt:  [−5.5990% −5.5142% −5.4221%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
metadata_query/match_min_tier
                        time:   [3.4158 ns 3.4183 ns 3.4214 ns]
                        thrpt:  [292.28 Melem/s 292.55 Melem/s 292.76 Melem/s]
                 change:
                        time:   [−3.0420% −2.5047% −1.9953%] (p = 0.00 < 0.05)
                        thrpt:  [+2.0360% +2.5691% +3.1374%]
                        Performance has improved.
Found 23 outliers among 100 measurements (23.00%)
  2 (2.00%) high mild
  21 (21.00%) high severe
metadata_query/match_continent
                        time:   [11.613 ns 11.700 ns 11.797 ns]
                        thrpt:  [84.769 Melem/s 85.469 Melem/s 86.108 Melem/s]
                 change:
                        time:   [+0.0305% +1.2824% +3.1990%] (p = 0.10 > 0.05)
                        thrpt:  [−3.0998% −1.2662% −0.0305%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
metadata_query/match_complex
                        time:   [10.864 ns 10.869 ns 10.874 ns]
                        thrpt:  [91.961 Melem/s 92.004 Melem/s 92.043 Melem/s]
                 change:
                        time:   [+2.4786% +2.6937% +2.8993%] (p = 0.00 < 0.05)
                        thrpt:  [−2.8176% −2.6230% −2.4187%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
metadata_query/match_no_match
                        time:   [3.4163 ns 3.4194 ns 3.4235 ns]
                        thrpt:  [292.10 Melem/s 292.45 Melem/s 292.71 Melem/s]
                 change:
                        time:   [−1.5246% −0.9261% −0.4111%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4128% +0.9348% +1.5482%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe

metadata_store_basic/create
                        time:   [747.05 ns 748.25 ns 749.55 ns]
                        thrpt:  [1.3341 Melem/s 1.3365 Melem/s 1.3386 Melem/s]
                 change:
                        time:   [−2.4693% −1.6169% −0.9336%] (p = 0.00 < 0.05)
                        thrpt:  [+0.9424% +1.6435% +2.5318%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
metadata_store_basic/upsert_new
                        time:   [2.1249 µs 2.1493 µs 2.1769 µs]
                        thrpt:  [459.37 Kelem/s 465.26 Kelem/s 470.60 Kelem/s]
                 change:
                        time:   [−16.743% −13.771% −10.564%] (p = 0.00 < 0.05)
                        thrpt:  [+11.812% +15.971% +20.109%]
                        Performance has improved.
metadata_store_basic/upsert_existing
                        time:   [1.3202 µs 1.3277 µs 1.3379 µs]
                        thrpt:  [747.44 Kelem/s 753.16 Kelem/s 757.44 Kelem/s]
                 change:
                        time:   [−2.6026% −2.0752% −1.4444%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4655% +2.1192% +2.6721%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/get
                        time:   [24.322 ns 25.238 ns 26.198 ns]
                        thrpt:  [38.171 Melem/s 39.623 Melem/s 41.115 Melem/s]
                 change:
                        time:   [−5.2266% −2.0839% +1.1269%] (p = 0.20 > 0.05)
                        thrpt:  [−1.1143% +2.1283% +5.5149%]
                        No change in performance detected.
Found 20 outliers among 100 measurements (20.00%)
  20 (20.00%) high mild
metadata_store_basic/get_miss
                        time:   [25.391 ns 26.301 ns 27.299 ns]
                        thrpt:  [36.631 Melem/s 38.022 Melem/s 39.384 Melem/s]
                 change:
                        time:   [−4.2756% −1.0347% +2.3147%] (p = 0.54 > 0.05)
                        thrpt:  [−2.2623% +1.0455% +4.4666%]
                        No change in performance detected.
metadata_store_basic/len
                        time:   [310.75 ps 312.23 ps 314.91 ps]
                        thrpt:  [3.1755 Gelem/s 3.2028 Gelem/s 3.2180 Gelem/s]
                 change:
                        time:   [−4.8493% −4.1516% −3.3015%] (p = 0.00 < 0.05)
                        thrpt:  [+3.4142% +4.3314% +5.0964%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
metadata_store_basic/stats
                        time:   [5.3914 µs 5.3997 µs 5.4083 µs]
                        thrpt:  [184.90 Kelem/s 185.19 Kelem/s 185.48 Kelem/s]
                 change:
                        time:   [−1.4781% −1.2710% −1.0631%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0745% +1.2874% +1.5003%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

metadata_store_query/query_by_status
                        time:   [342.49 µs 358.64 µs 373.59 µs]
                        thrpt:  [2.6767 Kelem/s 2.7883 Kelem/s 2.9198 Kelem/s]
                 change:
                        time:   [−36.818% −32.504% −27.943%] (p = 0.00 < 0.05)
                        thrpt:  [+38.778% +48.157% +58.272%]
                        Performance has improved.
metadata_store_query/query_by_continent
                        time:   [146.05 µs 146.64 µs 147.40 µs]
                        thrpt:  [6.7842 Kelem/s 6.8196 Kelem/s 6.8470 Kelem/s]
                 change:
                        time:   [−6.2114% −4.1581% −2.2176%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2679% +4.3384% +6.6227%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) high mild
  13 (13.00%) high severe
metadata_store_query/query_by_tier
                        time:   [585.05 µs 597.69 µs 611.71 µs]
                        thrpt:  [1.6348 Kelem/s 1.6731 Kelem/s 1.7093 Kelem/s]
                 change:
                        time:   [+23.505% +27.357% +31.683%] (p = 0.00 < 0.05)
                        thrpt:  [−24.060% −21.481% −19.032%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
metadata_store_query/query_accepting_work
                        time:   [651.67 µs 684.18 µs 719.00 µs]
                        thrpt:  [1.3908 Kelem/s 1.4616 Kelem/s 1.5345 Kelem/s]
                 change:
                        time:   [−15.322% −9.8541% −4.3612%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5601% +10.931% +18.094%]
                        Performance has improved.
metadata_store_query/query_with_limit
                        time:   [409.42 µs 426.44 µs 443.56 µs]
                        thrpt:  [2.2545 Kelem/s 2.3450 Kelem/s 2.4425 Kelem/s]
                 change:
                        time:   [−29.033% −25.016% −20.639%] (p = 0.00 < 0.05)
                        thrpt:  [+26.007% +33.361% +40.911%]
                        Performance has improved.
metadata_store_query/query_complex
                        time:   [278.71 µs 279.69 µs 280.76 µs]
                        thrpt:  [3.5617 Kelem/s 3.5754 Kelem/s 3.5879 Kelem/s]
                 change:
                        time:   [−44.471% −43.301% −41.870%] (p = 0.00 < 0.05)
                        thrpt:  [+72.029% +76.369% +80.085%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild

metadata_store_spatial/find_nearby_100km
                        time:   [326.90 µs 327.67 µs 328.60 µs]
                        thrpt:  [3.0432 Kelem/s 3.0519 Kelem/s 3.0590 Kelem/s]
                 change:
                        time:   [−27.964% −27.398% −26.798%] (p = 0.00 < 0.05)
                        thrpt:  [+36.608% +37.738% +38.820%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) high mild
  16 (16.00%) high severe
metadata_store_spatial/find_nearby_1000km
                        time:   [403.61 µs 405.50 µs 408.65 µs]
                        thrpt:  [2.4471 Kelem/s 2.4661 Kelem/s 2.4776 Kelem/s]
                 change:
                        time:   [−21.474% −20.979% −20.371%] (p = 0.00 < 0.05)
                        thrpt:  [+25.582% +26.549% +27.347%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
metadata_store_spatial/find_nearby_5000km
                        time:   [653.08 µs 664.23 µs 674.74 µs]
                        thrpt:  [1.4820 Kelem/s 1.5055 Kelem/s 1.5312 Kelem/s]
                 change:
                        time:   [−5.8062% −3.1461% −0.6324%] (p = 0.02 < 0.05)
                        thrpt:  [+0.6364% +3.2483% +6.1641%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  13 (13.00%) low mild
metadata_store_spatial/find_best_for_routing
                        time:   [280.31 µs 287.67 µs 296.81 µs]
                        thrpt:  [3.3691 Kelem/s 3.4762 Kelem/s 3.5675 Kelem/s]
                 change:
                        time:   [−8.4846% −3.2027% +2.1707%] (p = 0.26 > 0.05)
                        thrpt:  [−2.1246% +3.3086% +9.2713%]
                        No change in performance detected.
Found 19 outliers among 100 measurements (19.00%)
  4 (4.00%) high mild
  15 (15.00%) high severe
metadata_store_spatial/find_relays
                        time:   [767.47 µs 782.73 µs 796.39 µs]
                        thrpt:  [1.2557 Kelem/s 1.2776 Kelem/s 1.3030 Kelem/s]
                 change:
                        time:   [+60.059% +62.650% +65.218%] (p = 0.00 < 0.05)
                        thrpt:  [−39.474% −38.518% −37.523%]
                        Performance has regressed.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low severe
  7 (7.00%) low mild
  2 (2.00%) high mild

metadata_store_scaling/query_status/1000
                        time:   [18.974 µs 19.020 µs 19.076 µs]
                        thrpt:  [52.422 Kelem/s 52.577 Kelem/s 52.704 Kelem/s]
                 change:
                        time:   [+0.7486% +1.2332% +1.7070%] (p = 0.00 < 0.05)
                        thrpt:  [−1.6784% −1.2182% −0.7430%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
metadata_store_scaling/query_complex/1000
                        time:   [21.030 µs 21.257 µs 21.595 µs]
                        thrpt:  [46.306 Kelem/s 47.044 Kelem/s 47.552 Kelem/s]
                 change:
                        time:   [+2.6539% +3.3708% +4.3959%] (p = 0.00 < 0.05)
                        thrpt:  [−4.2108% −3.2609% −2.5853%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
metadata_store_scaling/find_nearby/1000
                        time:   [57.132 µs 57.209 µs 57.291 µs]
                        thrpt:  [17.455 Kelem/s 17.480 Kelem/s 17.503 Kelem/s]
                 change:
                        time:   [−1.4458% −1.2644% −1.0827%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0946% +1.2806% +1.4670%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_status/5000
                        time:   [98.130 µs 98.217 µs 98.311 µs]
                        thrpt:  [10.172 Kelem/s 10.182 Kelem/s 10.191 Kelem/s]
                 change:
                        time:   [−1.2942% −0.1276% +0.7573%] (p = 0.84 > 0.05)
                        thrpt:  [−0.7516% +0.1278% +1.3112%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_complex/5000
                        time:   [119.33 µs 119.53 µs 119.81 µs]
                        thrpt:  [8.3462 Kelem/s 8.3662 Kelem/s 8.3803 Kelem/s]
                 change:
                        time:   [+0.4853% +0.9133% +1.3281%] (p = 0.00 < 0.05)
                        thrpt:  [−1.3106% −0.9051% −0.4830%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
metadata_store_scaling/find_nearby/5000
                        time:   [359.58 µs 377.32 µs 395.37 µs]
                        thrpt:  [2.5293 Kelem/s 2.6503 Kelem/s 2.7810 Kelem/s]
                 change:
                        time:   [+0.5328% +9.5618% +19.165%] (p = 0.03 < 0.05)
                        thrpt:  [−16.083% −8.7273% −0.5300%]
                        Change within noise threshold.
metadata_store_scaling/query_status/10000
                        time:   [241.07 µs 254.26 µs 267.88 µs]
                        thrpt:  [3.7330 Kelem/s 3.9329 Kelem/s 4.1482 Kelem/s]
                 change:
                        time:   [−4.8219% +1.6455% +8.4565%] (p = 0.63 > 0.05)
                        thrpt:  [−7.7971% −1.6189% +5.0662%]
                        No change in performance detected.
metadata_store_scaling/query_complex/10000
                        time:   [327.75 µs 344.45 µs 359.95 µs]
                        thrpt:  [2.7782 Kelem/s 2.9031 Kelem/s 3.0511 Kelem/s]
                 change:
                        time:   [+15.569% +23.313% +30.995%] (p = 0.00 < 0.05)
                        thrpt:  [−23.661% −18.905% −13.472%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/10000
                        time:   [696.95 µs 712.51 µs 727.98 µs]
                        thrpt:  [1.3737 Kelem/s 1.4035 Kelem/s 1.4348 Kelem/s]
                 change:
                        time:   [+24.421% +27.655% +30.800%] (p = 0.00 < 0.05)
                        thrpt:  [−23.547% −21.664% −19.628%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  8 (8.00%) high mild
metadata_store_scaling/query_status/50000
                        time:   [2.5543 ms 2.5831 ms 2.6160 ms]
                        thrpt:  [382.26  elem/s 387.13  elem/s 391.50  elem/s]
                 change:
                        time:   [+9.3163% +10.550% +12.144%] (p = 0.00 < 0.05)
                        thrpt:  [−10.829% −9.5431% −8.5224%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/query_complex/50000
                        time:   [2.7341 ms 2.7644 ms 2.7945 ms]
                        thrpt:  [357.84  elem/s 361.74  elem/s 365.75  elem/s]
                 change:
                        time:   [−1.7852% +0.5095% +2.7062%] (p = 0.66 > 0.05)
                        thrpt:  [−2.6349% −0.5069% +1.8177%]
                        No change in performance detected.
metadata_store_scaling/find_nearby/50000
                        time:   [3.4256 ms 3.4695 ms 3.5194 ms]
                        thrpt:  [284.14  elem/s 288.22  elem/s 291.92  elem/s]
                 change:
                        time:   [+4.5391% +6.3870% +8.4470%] (p = 0.00 < 0.05)
                        thrpt:  [−7.7891% −6.0036% −4.3420%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
  6 (6.00%) high severe

metadata_store_concurrent/concurrent_upsert/4
                        time:   [1.6364 ms 1.6545 ms 1.6704 ms]
                        thrpt:  [1.1973 Melem/s 1.2089 Melem/s 1.2222 Melem/s]
                 change:
                        time:   [−45.490% −44.662% −43.806%] (p = 0.00 < 0.05)
                        thrpt:  [+77.954% +80.708% +83.454%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.5s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/4
                        time:   [348.63 ms 376.18 ms 406.86 ms]
                        thrpt:  [4.9157 Kelem/s 5.3166 Kelem/s 5.7367 Kelem/s]
                 change:
                        time:   [−44.463% −39.923% −34.529%] (p = 0.00 < 0.05)
                        thrpt:  [+52.740% +66.454% +80.062%]
                        Performance has improved.
metadata_store_concurrent/concurrent_mixed/4
                        time:   [175.76 ms 180.41 ms 185.28 ms]
                        thrpt:  [10.794 Kelem/s 11.086 Kelem/s 11.379 Kelem/s]
                 change:
                        time:   [−65.479% −60.068% −53.556%] (p = 0.00 < 0.05)
                        thrpt:  [+115.31% +150.43% +189.68%]
                        Performance has improved.
metadata_store_concurrent/concurrent_upsert/8
                        time:   [4.4595 ms 4.4723 ms 4.4930 ms]
                        thrpt:  [890.28 Kelem/s 894.39 Kelem/s 896.96 Kelem/s]
                 change:
                        time:   [−19.539% −18.955% −18.361%] (p = 0.00 < 0.05)
                        thrpt:  [+22.490% +23.388% +24.283%]
                        Performance has improved.
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) high mild
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 16.0s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/8
                        time:   [797.52 ms 801.72 ms 805.92 ms]
                        thrpt:  [4.9633 Kelem/s 4.9892 Kelem/s 5.0156 Kelem/s]
                 change:
                        time:   [−8.2355% −7.5598% −6.9057%] (p = 0.00 < 0.05)
                        thrpt:  [+7.4180% +8.1780% +8.9746%]
                        Performance has improved.
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 17.6s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/8
                        time:   [884.30 ms 890.61 ms 897.13 ms]
                        thrpt:  [4.4587 Kelem/s 4.4913 Kelem/s 4.5233 Kelem/s]
                 change:
                        time:   [−10.472% −8.5792% −6.7011%] (p = 0.00 < 0.05)
                        thrpt:  [+7.1824% +9.3843% +11.697%]
                        Performance has improved.
metadata_store_concurrent/concurrent_upsert/16
                        time:   [9.6562 ms 9.7041 ms 9.7511 ms]
                        thrpt:  [820.42 Kelem/s 824.40 Kelem/s 828.49 Kelem/s]
                 change:
                        time:   [−11.925% −10.194% −8.8239%] (p = 0.00 < 0.05)
                        thrpt:  [+9.6778% +11.352% +13.539%]
                        Performance has improved.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) low mild
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 30.2s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/16
                        time:   [1.5005 s 1.5099 s 1.5204 s]
                        thrpt:  [5.2616 Kelem/s 5.2983 Kelem/s 5.3315 Kelem/s]
                 change:
                        time:   [−9.9287% −8.6309% −7.3756%] (p = 0.00 < 0.05)
                        thrpt:  [+7.9629% +9.4462% +11.023%]
                        Performance has improved.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 34.9s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/16
                        time:   [1.7573 s 1.7673 s 1.7774 s]
                        thrpt:  [4.5010 Kelem/s 4.5268 Kelem/s 4.5525 Kelem/s]
                 change:
                        time:   [−0.6397% +1.0761% +2.5339%] (p = 0.21 > 0.05)
                        thrpt:  [−2.4713% −1.0646% +0.6438%]
                        No change in performance detected.

metadata_store_versioning/update_versioned_success
                        time:   [283.82 ns 284.56 ns 285.39 ns]
                        thrpt:  [3.5039 Melem/s 3.5141 Melem/s 3.5234 Melem/s]
                 change:
                        time:   [+3.4345% +4.2863% +5.1583%] (p = 0.00 < 0.05)
                        thrpt:  [−4.9053% −4.1101% −3.3204%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
metadata_store_versioning/update_versioned_conflict
                        time:   [285.82 ns 286.77 ns 287.82 ns]
                        thrpt:  [3.4744 Melem/s 3.4871 Melem/s 3.4987 Melem/s]
                 change:
                        time:   [+5.4391% +6.6523% +7.6313%] (p = 0.00 < 0.05)
                        thrpt:  [−7.0902% −6.2374% −5.1585%]
                        Performance has regressed.

schema_validation/validate_string
                        time:   [3.4828 ns 3.4882 ns 3.4952 ns]
                        thrpt:  [286.11 Melem/s 286.68 Melem/s 287.12 Melem/s]
                 change:
                        time:   [+0.6039% +0.9090% +1.2144%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1998% −0.9009% −0.6003%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  8 (8.00%) high severe
schema_validation/validate_integer
                        time:   [3.4742 ns 3.4758 ns 3.4774 ns]
                        thrpt:  [287.57 Melem/s 287.70 Melem/s 287.83 Melem/s]
                 change:
                        time:   [+0.7435% +0.9464% +1.1540%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1409% −0.9375% −0.7381%]
                        Change within noise threshold.
Found 18 outliers among 100 measurements (18.00%)
  3 (3.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
schema_validation/validate_object
                        time:   [74.452 ns 74.766 ns 75.258 ns]
                        thrpt:  [13.288 Melem/s 13.375 Melem/s 13.432 Melem/s]
                 change:
                        time:   [−1.3751% −0.7895% +0.0226%] (p = 0.02 < 0.05)
                        thrpt:  [−0.0226% +0.7958% +1.3943%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
schema_validation/validate_array_10
                        time:   [36.187 ns 36.260 ns 36.332 ns]
                        thrpt:  [27.524 Melem/s 27.579 Melem/s 27.634 Melem/s]
                 change:
                        time:   [−0.2598% −0.0046% +0.2291%] (p = 0.97 > 0.05)
                        thrpt:  [−0.2286% +0.0046% +0.2605%]
                        No change in performance detected.
schema_validation/validate_complex
                        time:   [200.75 ns 202.02 ns 203.82 ns]
                        thrpt:  [4.9063 Melem/s 4.9500 Melem/s 4.9814 Melem/s]
                 change:
                        time:   [−1.0348% −0.3256% +0.6267%] (p = 0.47 > 0.05)
                        thrpt:  [−0.6228% +0.3267% +1.0456%]
                        No change in performance detected.
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe

endpoint_matching/match_success
                        time:   [280.21 ns 281.16 ns 282.14 ns]
                        thrpt:  [3.5443 Melem/s 3.5567 Melem/s 3.5688 Melem/s]
                 change:
                        time:   [+0.7500% +1.4634% +2.1672%] (p = 0.00 < 0.05)
                        thrpt:  [−2.1212% −1.4423% −0.7444%]
                        Change within noise threshold.
endpoint_matching/match_failure
                        time:   [280.85 ns 282.02 ns 283.46 ns]
                        thrpt:  [3.5278 Melem/s 3.5459 Melem/s 3.5606 Melem/s]
                 change:
                        time:   [−0.2022% +1.1915% +2.6956%] (p = 0.12 > 0.05)
                        thrpt:  [−2.6248% −1.1774% +0.2026%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
endpoint_matching/match_multi_param
                        time:   [679.08 ns 683.26 ns 686.87 ns]
                        thrpt:  [1.4559 Melem/s 1.4636 Melem/s 1.4726 Melem/s]
                 change:
                        time:   [+1.4635% +2.6207% +3.8432%] (p = 0.00 < 0.05)
                        thrpt:  [−3.7010% −2.5538% −1.4424%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) low severe
  5 (5.00%) low mild

api_version/is_compatible_with
                        time:   [310.40 ps 310.55 ps 310.77 ps]
                        thrpt:  [3.2178 Gelem/s 3.2201 Gelem/s 3.2217 Gelem/s]
                 change:
                        time:   [−0.2423% −0.1140% −0.0048%] (p = 0.07 > 0.05)
                        thrpt:  [+0.0048% +0.1141% +0.2429%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
api_version/parse       time:   [38.712 ns 38.739 ns 38.771 ns]
                        thrpt:  [25.792 Melem/s 25.814 Melem/s 25.832 Melem/s]
                 change:
                        time:   [−1.9605% −0.7772% −0.1175%] (p = 0.14 > 0.05)
                        thrpt:  [+0.1177% +0.7832% +1.9997%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
api_version/to_string   time:   [49.670 ns 49.705 ns 49.749 ns]
                        thrpt:  [20.101 Melem/s 20.119 Melem/s 20.133 Melem/s]
                 change:
                        time:   [−0.2937% −0.1364% +0.0075%] (p = 0.07 > 0.05)
                        thrpt:  [−0.0075% +0.1366% +0.2946%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe

api_schema/create       time:   [2.1997 µs 2.2047 µs 2.2099 µs]
                        thrpt:  [452.51 Kelem/s 453.58 Kelem/s 454.62 Kelem/s]
                 change:
                        time:   [−1.0171% −0.7547% −0.4996%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5021% +0.7605% +1.0276%]
                        Change within noise threshold.
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
api_schema/serialize    time:   [2.0629 µs 2.0680 µs 2.0733 µs]
                        thrpt:  [482.33 Kelem/s 483.57 Kelem/s 484.74 Kelem/s]
                 change:
                        time:   [−2.7807% −2.4234% −2.0640%] (p = 0.00 < 0.05)
                        thrpt:  [+2.1075% +2.4836% +2.8603%]
                        Performance has improved.
api_schema/deserialize  time:   [6.8516 µs 6.8922 µs 6.9398 µs]
                        thrpt:  [144.10 Kelem/s 145.09 Kelem/s 145.95 Kelem/s]
                 change:
                        time:   [−1.7588% −1.4070% −1.0214%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0319% +1.4271% +1.7902%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
api_schema/find_endpoint
                        time:   [133.03 ns 133.23 ns 133.47 ns]
                        thrpt:  [7.4923 Melem/s 7.5058 Melem/s 7.5174 Melem/s]
                 change:
                        time:   [−3.4647% −2.9073% −2.3186%] (p = 0.00 < 0.05)
                        thrpt:  [+2.3736% +2.9943% +3.5891%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) high mild
  12 (12.00%) high severe
api_schema/endpoints_by_tag
                        time:   [114.59 ns 115.72 ns 116.87 ns]
                        thrpt:  [8.5563 Melem/s 8.6416 Melem/s 8.7270 Melem/s]
                 change:
                        time:   [−1.9865% −0.9341% +0.1290%] (p = 0.09 > 0.05)
                        thrpt:  [−0.1289% +0.9429% +2.0268%]
                        No change in performance detected.

request_validation/validate_full_request
                        time:   [70.699 ns 70.977 ns 71.438 ns]
                        thrpt:  [13.998 Melem/s 14.089 Melem/s 14.144 Melem/s]
                 change:
                        time:   [−0.1350% +0.1802% +0.6276%] (p = 0.38 > 0.05)
                        thrpt:  [−0.6237% −0.1799% +0.1352%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
request_validation/validate_path_only
                        time:   [21.266 ns 21.401 ns 21.528 ns]
                        thrpt:  [46.451 Melem/s 46.727 Melem/s 47.024 Melem/s]
                 change:
                        time:   [−3.5566% −2.7473% −1.9382%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9766% +2.8249% +3.6877%]
                        Performance has improved.

api_registry_basic/create
                        time:   [412.13 ns 413.06 ns 414.14 ns]
                        thrpt:  [2.4147 Melem/s 2.4209 Melem/s 2.4264 Melem/s]
                 change:
                        time:   [+0.8827% +1.3329% +1.8153%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7829% −1.3153% −0.8750%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
api_registry_basic/register_new
                        time:   [5.3503 µs 5.5078 µs 5.6536 µs]
                        thrpt:  [176.88 Kelem/s 181.56 Kelem/s 186.91 Kelem/s]
                 change:
                        time:   [+15.360% +19.328% +23.408%] (p = 0.00 < 0.05)
                        thrpt:  [−18.968% −16.197% −13.315%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
api_registry_basic/get  time:   [25.932 ns 26.844 ns 27.724 ns]
                        thrpt:  [36.070 Melem/s 37.252 Melem/s 38.562 Melem/s]
                 change:
                        time:   [+3.9218% +7.8255% +11.955%] (p = 0.00 < 0.05)
                        thrpt:  [−10.678% −7.2575% −3.7738%]
                        Performance has regressed.
api_registry_basic/len  time:   [310.36 ps 310.48 ps 310.62 ps]
                        thrpt:  [3.2194 Gelem/s 3.2208 Gelem/s 3.2220 Gelem/s]
                 change:
                        time:   [−0.3733% −0.1617% +0.0073%] (p = 0.10 > 0.05)
                        thrpt:  [−0.0073% +0.1620% +0.3747%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
api_registry_basic/stats
                        time:   [2.8437 µs 2.8461 µs 2.8486 µs]
                        thrpt:  [351.05 Kelem/s 351.36 Kelem/s 351.66 Kelem/s]
                 change:
                        time:   [+1.8669% +2.1065% +2.3552%] (p = 0.00 < 0.05)
                        thrpt:  [−2.3010% −2.0631% −1.8327%]
                        Performance has regressed.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe

api_registry_query/query_by_name
                        time:   [96.159 µs 97.022 µs 97.885 µs]
                        thrpt:  [10.216 Kelem/s 10.307 Kelem/s 10.399 Kelem/s]
                 change:
                        time:   [−0.1219% +1.0176% +2.1107%] (p = 0.06 > 0.05)
                        thrpt:  [−2.0671% −1.0073% +0.1220%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
api_registry_query/query_by_tag
                        time:   [608.42 µs 617.83 µs 627.24 µs]
                        thrpt:  [1.5943 Kelem/s 1.6186 Kelem/s 1.6436 Kelem/s]
                 change:
                        time:   [−17.060% −14.047% −11.056%] (p = 0.00 < 0.05)
                        thrpt:  [+12.430% +16.343% +20.569%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_registry_query/query_with_version
                        time:   [55.234 µs 55.283 µs 55.331 µs]
                        thrpt:  [18.073 Kelem/s 18.089 Kelem/s 18.105 Kelem/s]
                 change:
                        time:   [+0.2078% +0.3713% +0.5149%] (p = 0.00 < 0.05)
                        thrpt:  [−0.5123% −0.3699% −0.2073%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking api_registry_query/find_by_endpoint: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.7s, enable flat sampling, or reduce sample count to 50.
api_registry_query/find_by_endpoint
                        time:   [1.7613 ms 1.7818 ms 1.8074 ms]
                        thrpt:  [553.27  elem/s 561.23  elem/s 567.77  elem/s]
                 change:
                        time:   [−6.8406% −4.2526% −1.3318%] (p = 0.00 < 0.05)
                        thrpt:  [+1.3498% +4.4415% +7.3429%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
api_registry_query/find_compatible
                        time:   [63.694 µs 63.740 µs 63.794 µs]
                        thrpt:  [15.675 Kelem/s 15.689 Kelem/s 15.700 Kelem/s]
                 change:
                        time:   [−1.3485% −0.9543% −0.5808%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5842% +0.9635% +1.3669%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

api_registry_scaling/query_by_name/1000
                        time:   [7.3605 µs 7.3929 µs 7.4233 µs]
                        thrpt:  [134.71 Kelem/s 135.27 Kelem/s 135.86 Kelem/s]
                 change:
                        time:   [−1.4591% −0.9037% −0.3938%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3953% +0.9119% +1.4807%]
                        Change within noise threshold.
api_registry_scaling/query_by_tag/1000
                        time:   [48.309 µs 48.479 µs 48.716 µs]
                        thrpt:  [20.527 Kelem/s 20.627 Kelem/s 20.700 Kelem/s]
                 change:
                        time:   [−7.0119% −2.7953% +0.1441%] (p = 0.16 > 0.05)
                        thrpt:  [−0.1439% +2.8757% +7.5407%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
api_registry_scaling/query_by_name/5000
                        time:   [44.301 µs 44.477 µs 44.648 µs]
                        thrpt:  [22.397 Kelem/s 22.483 Kelem/s 22.573 Kelem/s]
                 change:
                        time:   [−4.5972% −3.7087% −2.8968%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9832% +3.8515% +4.8187%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_registry_scaling/query_by_tag/5000
                        time:   [305.29 µs 314.05 µs 323.44 µs]
                        thrpt:  [3.0918 Kelem/s 3.1842 Kelem/s 3.2755 Kelem/s]
                 change:
                        time:   [+3.2523% +5.7677% +8.6327%] (p = 0.00 < 0.05)
                        thrpt:  [−7.9467% −5.4532% −3.1499%]
                        Performance has regressed.
Found 20 outliers among 100 measurements (20.00%)
  5 (5.00%) high mild
  15 (15.00%) high severe
api_registry_scaling/query_by_name/10000
                        time:   [95.067 µs 95.917 µs 96.914 µs]
                        thrpt:  [10.318 Kelem/s 10.426 Kelem/s 10.519 Kelem/s]
                 change:
                        time:   [+0.0909% +1.0794% +2.1073%] (p = 0.03 < 0.05)
                        thrpt:  [−2.0638% −1.0679% −0.0908%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  5 (5.00%) low mild
  8 (8.00%) high mild
  3 (3.00%) high severe
api_registry_scaling/query_by_tag/10000
                        time:   [578.43 µs 588.27 µs 600.05 µs]
                        thrpt:  [1.6665 Kelem/s 1.6999 Kelem/s 1.7288 Kelem/s]
                 change:
                        time:   [−0.8767% +1.4085% +3.8232%] (p = 0.24 > 0.05)
                        thrpt:  [−3.6824% −1.3889% +0.8845%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  11 (11.00%) high mild
  2 (2.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 16.7s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/4
                        time:   [800.15 ms 828.97 ms 858.43 ms]
                        thrpt:  [2.3298 Kelem/s 2.4126 Kelem/s 2.4995 Kelem/s]
                 change:
                        time:   [−12.272% −8.9695% −5.5661%] (p = 0.00 < 0.05)
                        thrpt:  [+5.8941% +9.8533% +13.989%]
                        Performance has improved.
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 9.6s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/4
                        time:   [421.46 ms 447.03 ms 478.11 ms]
                        thrpt:  [4.1832 Kelem/s 4.4740 Kelem/s 4.7454 Kelem/s]
                 change:
                        time:   [−31.125% −26.689% −21.846%] (p = 0.00 < 0.05)
                        thrpt:  [+27.952% +36.406% +45.191%]
                        Performance has improved.
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) high mild
  3 (15.00%) high severe
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 22.6s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/8
                        time:   [1.1558 s 1.1730 s 1.1895 s]
                        thrpt:  [3.3629 Kelem/s 3.4099 Kelem/s 3.4609 Kelem/s]
                 change:
                        time:   [+3.2366% +4.9510% +6.7251%] (p = 0.00 < 0.05)
                        thrpt:  [−6.3014% −4.7174% −3.1352%]
                        Performance has regressed.
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 18.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/8
                        time:   [896.01 ms 903.75 ms 913.80 ms]
                        thrpt:  [4.3773 Kelem/s 4.4260 Kelem/s 4.4642 Kelem/s]
                 change:
                        time:   [+1.7630% +2.9536% +4.2003%] (p = 0.00 < 0.05)
                        thrpt:  [−4.0310% −2.8688% −1.7325%]
                        Performance has regressed.
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high severe
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 37.8s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/16
                        time:   [1.8729 s 1.8826 s 1.8913 s]
                        thrpt:  [4.2300 Kelem/s 4.2494 Kelem/s 4.2715 Kelem/s]
                 change:
                        time:   [−0.2217% +1.4868% +2.9816%] (p = 0.08 > 0.05)
                        thrpt:  [−2.8953% −1.4650% +0.2222%]
                        No change in performance detected.
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) low mild
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 35.1s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/16
                        time:   [1.7051 s 1.7193 s 1.7360 s]
                        thrpt:  [4.6083 Kelem/s 4.6532 Kelem/s 4.6919 Kelem/s]
                 change:
                        time:   [−1.9535% −0.8631% +0.2363%] (p = 0.14 > 0.05)
                        thrpt:  [−0.2357% +0.8707% +1.9924%]
                        No change in performance detected.
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

compare_op/eq           time:   [1.9760 ns 1.9766 ns 1.9772 ns]
                        thrpt:  [505.75 Melem/s 505.92 Melem/s 506.07 Melem/s]
                 change:
                        time:   [−1.0592% −0.6745% −0.4053%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4070% +0.6791% +1.0706%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
compare_op/gt           time:   [2.0229 ns 2.0275 ns 2.0338 ns]
                        thrpt:  [491.70 Melem/s 493.22 Melem/s 494.34 Melem/s]
                 change:
                        time:   [−1.1917% −0.9713% −0.7205%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7257% +0.9808% +1.2060%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  12 (12.00%) high mild
  1 (1.00%) high severe
compare_op/contains_string
                        time:   [24.844 ns 24.885 ns 24.938 ns]
                        thrpt:  [40.099 Melem/s 40.185 Melem/s 40.251 Melem/s]
                 change:
                        time:   [−1.7559% −1.2021% −0.7833%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7894% +1.2167% +1.7873%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe
compare_op/in_array     time:   [6.8279 ns 6.8325 ns 6.8380 ns]
                        thrpt:  [146.24 Melem/s 146.36 Melem/s 146.46 Melem/s]
                 change:
                        time:   [−1.0436% −0.9190% −0.7988%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8053% +0.9275% +1.0546%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

condition/simple        time:   [55.543 ns 55.661 ns 55.778 ns]
                        thrpt:  [17.928 Melem/s 17.966 Melem/s 18.004 Melem/s]
                 change:
                        time:   [−0.8814% −0.6067% −0.3209%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3220% +0.6104% +0.8893%]
                        Change within noise threshold.
Found 24 outliers among 100 measurements (24.00%)
  3 (3.00%) low severe
  5 (5.00%) low mild
  6 (6.00%) high mild
  10 (10.00%) high severe
condition/nested_field  time:   [900.22 ns 904.04 ns 908.03 ns]
                        thrpt:  [1.1013 Melem/s 1.1061 Melem/s 1.1108 Melem/s]
                 change:
                        time:   [−5.1548% −4.6973% −4.2315%] (p = 0.00 < 0.05)
                        thrpt:  [+4.4184% +4.9288% +5.4350%]
                        Performance has improved.
condition/string_eq     time:   [94.257 ns 95.256 ns 96.341 ns]
                        thrpt:  [10.380 Melem/s 10.498 Melem/s 10.609 Melem/s]
                 change:
                        time:   [+1.1654% +2.2373% +3.2763%] (p = 0.00 < 0.05)
                        thrpt:  [−3.1724% −2.1883% −1.1520%]
                        Performance has regressed.

condition_expr/single   time:   [56.464 ns 56.512 ns 56.567 ns]
                        thrpt:  [17.678 Melem/s 17.695 Melem/s 17.710 Melem/s]
                 change:
                        time:   [−3.2971% −2.3098% −1.4933%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5159% +2.3644% +3.4095%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
condition_expr/and_2    time:   [113.10 ns 113.36 ns 113.64 ns]
                        thrpt:  [8.7999 Melem/s 8.8214 Melem/s 8.8415 Melem/s]
                 change:
                        time:   [−0.5942% −0.3106% +0.0271%] (p = 0.04 < 0.05)
                        thrpt:  [−0.0270% +0.3116% +0.5978%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
condition_expr/and_5    time:   [395.59 ns 397.13 ns 398.55 ns]
                        thrpt:  [2.5091 Melem/s 2.5181 Melem/s 2.5279 Melem/s]
                 change:
                        time:   [−0.8320% −0.2873% +0.2690%] (p = 0.31 > 0.05)
                        thrpt:  [−0.2683% +0.2881% +0.8390%]
                        No change in performance detected.
condition_expr/or_3     time:   [228.23 ns 229.30 ns 230.36 ns]
                        thrpt:  [4.3411 Melem/s 4.3611 Melem/s 4.3816 Melem/s]
                 change:
                        time:   [−2.4869% −1.8194% −1.1593%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1729% +1.8532% +2.5503%]
                        Performance has improved.
condition_expr/nested   time:   [168.41 ns 169.32 ns 170.32 ns]
                        thrpt:  [5.8713 Melem/s 5.9061 Melem/s 5.9381 Melem/s]
                 change:
                        time:   [+0.3544% +0.8264% +1.3002%] (p = 0.00 < 0.05)
                        thrpt:  [−1.2835% −0.8196% −0.3532%]
                        Change within noise threshold.

rule/create             time:   [567.29 ns 570.22 ns 573.27 ns]
                        thrpt:  [1.7444 Melem/s 1.7537 Melem/s 1.7628 Melem/s]
                 change:
                        time:   [+2.9022% +3.6265% +4.3818%] (p = 0.00 < 0.05)
                        thrpt:  [−4.1978% −3.4996% −2.8203%]
                        Performance has regressed.
rule/matches            time:   [113.66 ns 114.03 ns 114.46 ns]
                        thrpt:  [8.7369 Melem/s 8.7697 Melem/s 8.7981 Melem/s]
                 change:
                        time:   [−1.2476% −0.6853% −0.1781%] (p = 0.01 < 0.05)
                        thrpt:  [+0.1784% +0.6900% +1.2634%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe

rule_context/create     time:   [1.4398 µs 1.4460 µs 1.4526 µs]
                        thrpt:  [688.43 Kelem/s 691.58 Kelem/s 694.55 Kelem/s]
                 change:
                        time:   [−3.1727% −2.8190% −2.4606%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5227% +2.9008% +3.2767%]
                        Performance has improved.
rule_context/get_simple time:   [54.884 ns 55.177 ns 55.527 ns]
                        thrpt:  [18.009 Melem/s 18.123 Melem/s 18.220 Melem/s]
                 change:
                        time:   [−0.4698% +0.0008% +0.5436%] (p = 1.00 > 0.05)
                        thrpt:  [−0.5406% −0.0008% +0.4720%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  13 (13.00%) high severe
rule_context/get_nested time:   [884.47 ns 886.37 ns 888.36 ns]
                        thrpt:  [1.1257 Melem/s 1.1282 Melem/s 1.1306 Melem/s]
                 change:
                        time:   [−2.0938% −1.7628% −1.4321%] (p = 0.00 < 0.05)
                        thrpt:  [+1.4529% +1.7944% +2.1386%]
                        Performance has improved.
rule_context/get_deep_nested
                        time:   [890.57 ns 893.35 ns 896.03 ns]
                        thrpt:  [1.1160 Melem/s 1.1194 Melem/s 1.1229 Melem/s]
                 change:
                        time:   [−3.6785% −2.8709% −2.2184%] (p = 0.00 < 0.05)
                        thrpt:  [+2.2687% +2.9558% +3.8190%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

rule_engine_basic/create
                        time:   [8.1293 ns 8.1353 ns 8.1421 ns]
                        thrpt:  [122.82 Melem/s 122.92 Melem/s 123.01 Melem/s]
                 change:
                        time:   [−3.7847% −3.2529% −2.8634%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9479% +3.3623% +3.9336%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
rule_engine_basic/add_rule
                        time:   [2.9317 µs 3.0931 µs 3.2298 µs]
                        thrpt:  [309.61 Kelem/s 323.30 Kelem/s 341.10 Kelem/s]
                 change:
                        time:   [−10.705% +0.2889% +11.611%] (p = 0.95 > 0.05)
                        thrpt:  [−10.403% −0.2880% +11.988%]
                        No change in performance detected.
rule_engine_basic/get_rule
                        time:   [21.679 ns 22.658 ns 23.585 ns]
                        thrpt:  [42.401 Melem/s 44.135 Melem/s 46.128 Melem/s]
                 change:
                        time:   [−0.6100% +4.7456% +10.134%] (p = 0.08 > 0.05)
                        thrpt:  [−9.2018% −4.5306% +0.6138%]
                        No change in performance detected.
rule_engine_basic/rules_by_tag
                        time:   [1.1757 µs 1.1826 µs 1.1890 µs]
                        thrpt:  [841.01 Kelem/s 845.59 Kelem/s 850.52 Kelem/s]
                 change:
                        time:   [+4.7059% +5.3361% +5.9766%] (p = 0.00 < 0.05)
                        thrpt:  [−5.6396% −5.0658% −4.4944%]
                        Performance has regressed.
rule_engine_basic/stats time:   [7.9823 µs 7.9941 µs 8.0117 µs]
                        thrpt:  [124.82 Kelem/s 125.09 Kelem/s 125.28 Kelem/s]
                 change:
                        time:   [−0.8338% −0.5866% −0.3072%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3082% +0.5901% +0.8409%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe

rule_engine_evaluate/evaluate_10_rules
                        time:   [3.5946 µs 3.6177 µs 3.6404 µs]
                        thrpt:  [274.70 Kelem/s 276.42 Kelem/s 278.19 Kelem/s]
                 change:
                        time:   [−2.5534% −1.7974% −1.0617%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0731% +1.8303% +2.6203%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
rule_engine_evaluate/evaluate_first_10_rules
                        time:   [428.00 ns 435.79 ns 444.05 ns]
                        thrpt:  [2.2520 Melem/s 2.2947 Melem/s 2.3364 Melem/s]
                 change:
                        time:   [−0.4529% +1.7656% +4.0480%] (p = 0.12 > 0.05)
                        thrpt:  [−3.8906% −1.7350% +0.4550%]
                        No change in performance detected.
Found 16 outliers among 100 measurements (16.00%)
  15 (15.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_100_rules
                        time:   [36.241 µs 36.497 µs 36.754 µs]
                        thrpt:  [27.208 Kelem/s 27.400 Kelem/s 27.593 Kelem/s]
                 change:
                        time:   [−0.7335% +0.0611% +0.8055%] (p = 0.87 > 0.05)
                        thrpt:  [−0.7991% −0.0610% +0.7389%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
rule_engine_evaluate/evaluate_first_100_rules
                        time:   [417.93 ns 424.74 ns 432.55 ns]
                        thrpt:  [2.3119 Melem/s 2.3544 Melem/s 2.3928 Melem/s]
                 change:
                        time:   [−4.9789% −2.8335% −0.4209%] (p = 0.02 < 0.05)
                        thrpt:  [+0.4227% +2.9162% +5.2398%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  5 (5.00%) high mild
  11 (11.00%) high severe
rule_engine_evaluate/evaluate_matching_100_rules
                        time:   [35.707 µs 35.792 µs 35.885 µs]
                        thrpt:  [27.867 Kelem/s 27.939 Kelem/s 28.005 Kelem/s]
                 change:
                        time:   [−1.6474% −1.2525% −0.8734%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8811% +1.2684% +1.6750%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_1000_rules
                        time:   [364.09 µs 369.90 µs 375.60 µs]
                        thrpt:  [2.6624 Kelem/s 2.7034 Kelem/s 2.7465 Kelem/s]
                 change:
                        time:   [−10.226% −8.5256% −6.6591%] (p = 0.00 < 0.05)
                        thrpt:  [+7.1342% +9.3202% +11.391%]
                        Performance has improved.
rule_engine_evaluate/evaluate_first_1000_rules
                        time:   [428.70 ns 436.06 ns 443.97 ns]
                        thrpt:  [2.2524 Melem/s 2.2932 Melem/s 2.3326 Melem/s]
                 change:
                        time:   [−1.5006% +0.9345% +3.5536%] (p = 0.47 > 0.05)
                        thrpt:  [−3.4317% −0.9258% +1.5235%]
                        No change in performance detected.

rule_engine_scaling/evaluate/10
                        time:   [3.5340 µs 3.5457 µs 3.5606 µs]
                        thrpt:  [280.85 Kelem/s 282.03 Kelem/s 282.96 Kelem/s]
                 change:
                        time:   [−0.4945% −0.2145% +0.0909%] (p = 0.16 > 0.05)
                        thrpt:  [−0.0909% +0.2150% +0.4970%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
rule_engine_scaling/evaluate_first/10
                        time:   [395.75 ns 397.53 ns 399.33 ns]
                        thrpt:  [2.5042 Melem/s 2.5155 Melem/s 2.5269 Melem/s]
                 change:
                        time:   [−3.7386% −3.3333% −2.9164%] (p = 0.00 < 0.05)
                        thrpt:  [+3.0040% +3.4482% +3.8838%]
                        Performance has improved.
rule_engine_scaling/evaluate/50
                        time:   [17.580 µs 17.626 µs 17.675 µs]
                        thrpt:  [56.578 Kelem/s 56.734 Kelem/s 56.883 Kelem/s]
                 change:
                        time:   [−1.5201% −1.1868% −0.8461%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8533% +1.2011% +1.5436%]
                        Change within noise threshold.
rule_engine_scaling/evaluate_first/50
                        time:   [392.87 ns 393.86 ns 394.95 ns]
                        thrpt:  [2.5320 Melem/s 2.5390 Melem/s 2.5454 Melem/s]
                 change:
                        time:   [−5.5902% −4.9404% −4.3115%] (p = 0.00 < 0.05)
                        thrpt:  [+4.5057% +5.1971% +5.9212%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  9 (9.00%) high mild
rule_engine_scaling/evaluate/100
                        time:   [36.142 µs 36.387 µs 36.658 µs]
                        thrpt:  [27.279 Kelem/s 27.482 Kelem/s 27.669 Kelem/s]
                 change:
                        time:   [+1.5031% +1.9295% +2.3868%] (p = 0.00 < 0.05)
                        thrpt:  [−2.3311% −1.8929% −1.4808%]
                        Performance has regressed.
Found 23 outliers among 100 measurements (23.00%)
  12 (12.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
rule_engine_scaling/evaluate_first/100
                        time:   [397.78 ns 400.59 ns 403.34 ns]
                        thrpt:  [2.4793 Melem/s 2.4963 Melem/s 2.5140 Melem/s]
                 change:
                        time:   [−1.0222% −0.0727% +0.8073%] (p = 0.87 > 0.05)
                        thrpt:  [−0.8008% +0.0727% +1.0328%]
                        No change in performance detected.
rule_engine_scaling/evaluate/500
                        time:   [190.29 µs 195.96 µs 201.93 µs]
                        thrpt:  [4.9521 Kelem/s 5.1030 Kelem/s 5.2551 Kelem/s]
                 change:
                        time:   [−9.7231% −6.7417% −3.6079%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7430% +7.2291% +10.770%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) high mild
rule_engine_scaling/evaluate_first/500
                        time:   [408.48 ns 411.80 ns 415.09 ns]
                        thrpt:  [2.4091 Melem/s 2.4284 Melem/s 2.4481 Melem/s]
                 change:
                        time:   [−0.9343% −0.0823% +0.7800%] (p = 0.85 > 0.05)
                        thrpt:  [−0.7740% +0.0824% +0.9431%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
rule_engine_scaling/evaluate/1000
                        time:   [377.33 µs 383.73 µs 389.83 µs]
                        thrpt:  [2.5652 Kelem/s 2.6060 Kelem/s 2.6502 Kelem/s]
                 change:
                        time:   [−1.1753% +0.5633% +2.2800%] (p = 0.53 > 0.05)
                        thrpt:  [−2.2291% −0.5601% +1.1892%]
                        No change in performance detected.
rule_engine_scaling/evaluate_first/1000
                        time:   [393.71 ns 394.93 ns 396.19 ns]
                        thrpt:  [2.5240 Melem/s 2.5321 Melem/s 2.5399 Melem/s]
                 change:
                        time:   [−3.8974% −3.1510% −2.3954%] (p = 0.00 < 0.05)
                        thrpt:  [+2.4542% +3.2535% +4.0555%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

rule_set/create         time:   [6.0443 µs 6.0592 µs 6.0761 µs]
                        thrpt:  [164.58 Kelem/s 165.04 Kelem/s 165.45 Kelem/s]
                 change:
                        time:   [−0.8351% −0.5990% −0.3346%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3357% +0.6026% +0.8421%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_set/load_into_engine
                        time:   [10.497 µs 10.544 µs 10.599 µs]
                        thrpt:  [94.350 Kelem/s 94.844 Kelem/s 95.268 Kelem/s]
                 change:
                        time:   [−1.0381% −0.6677% −0.2849%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2857% +0.6722% +1.0489%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

trace_id/generate       time:   [541.86 ns 543.26 ns 544.93 ns]
                        thrpt:  [1.8351 Melem/s 1.8407 Melem/s 1.8455 Melem/s]
                 change:
                        time:   [−1.9967% −1.5406% −1.1376%] (p = 0.00 < 0.05)
                        thrpt:  [+1.1507% +1.5648% +2.0373%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
trace_id/to_hex         time:   [131.94 ns 132.52 ns 133.12 ns]
                        thrpt:  [7.5121 Melem/s 7.5459 Melem/s 7.5791 Melem/s]
                 change:
                        time:   [+16.707% +17.667% +18.606%] (p = 0.00 < 0.05)
                        thrpt:  [−15.687% −15.014% −14.316%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
trace_id/from_hex       time:   [23.171 ns 23.184 ns 23.201 ns]
                        thrpt:  [43.102 Melem/s 43.133 Melem/s 43.157 Melem/s]
                 change:
                        time:   [−5.8258% −2.9622% −0.8007%] (p = 0.01 < 0.05)
                        thrpt:  [+0.8072% +3.0526% +6.1862%]
                        Change within noise threshold.
Found 17 outliers among 100 measurements (17.00%)
  9 (9.00%) high mild
  8 (8.00%) high severe

context_operations/create
                        time:   [818.77 ns 820.84 ns 823.43 ns]
                        thrpt:  [1.2144 Melem/s 1.2183 Melem/s 1.2213 Melem/s]
                 change:
                        time:   [−1.6910% −1.2102% −0.7480%] (p = 0.00 < 0.05)
                        thrpt:  [+0.7537% +1.2250% +1.7201%]
                        Change within noise threshold.
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) high mild
  15 (15.00%) high severe
context_operations/child
                        time:   [283.96 ns 285.16 ns 286.52 ns]
                        thrpt:  [3.4901 Melem/s 3.5067 Melem/s 3.5216 Melem/s]
                 change:
                        time:   [−3.0143% −2.3462% −1.7173%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7473% +2.4026% +3.1080%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) high severe
context_operations/for_remote
                        time:   [288.13 ns 290.40 ns 292.93 ns]
                        thrpt:  [3.4138 Melem/s 3.4435 Melem/s 3.4706 Melem/s]
                 change:
                        time:   [−0.6630% +0.1764% +0.9324%] (p = 0.65 > 0.05)
                        thrpt:  [−0.9238% −0.1761% +0.6675%]
                        No change in performance detected.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
context_operations/to_traceparent
                        time:   [336.41 ns 338.26 ns 339.94 ns]
                        thrpt:  [2.9417 Melem/s 2.9563 Melem/s 2.9725 Melem/s]
                 change:
                        time:   [−7.1768% −6.5576% −5.9643%] (p = 0.00 < 0.05)
                        thrpt:  [+6.3426% +7.0178% +7.7317%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low severe
  5 (5.00%) low mild
context_operations/from_traceparent
                        time:   [379.83 ns 380.80 ns 382.09 ns]
                        thrpt:  [2.6172 Melem/s 2.6260 Melem/s 2.6327 Melem/s]
                 change:
                        time:   [−1.1203% −0.7726% −0.4490%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4511% +0.7786% +1.1330%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

baggage/create          time:   [2.0356 ns 2.0364 ns 2.0374 ns]
                        thrpt:  [490.82 Melem/s 491.06 Melem/s 491.26 Melem/s]
                 change:
                        time:   [−0.9019% −0.7116% −0.5365%] (p = 0.00 < 0.05)
                        thrpt:  [+0.5394% +0.7167% +0.9101%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
baggage/get             time:   [17.098 ns 17.927 ns 18.782 ns]
                        thrpt:  [53.243 Melem/s 55.781 Melem/s 58.486 Melem/s]
                 change:
                        time:   [−4.3779% +0.2339% +4.6856%] (p = 0.92 > 0.05)
                        thrpt:  [−4.4759% −0.2334% +4.5784%]
                        No change in performance detected.
baggage/set             time:   [80.673 ns 81.161 ns 81.706 ns]
                        thrpt:  [12.239 Melem/s 12.321 Melem/s 12.396 Melem/s]
                 change:
                        time:   [−2.9038% −2.2868% −1.7079%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7376% +2.3403% +2.9907%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
baggage/merge           time:   [1.6166 µs 1.6185 µs 1.6203 µs]
                        thrpt:  [617.16 Kelem/s 617.86 Kelem/s 618.59 Kelem/s]
                 change:
                        time:   [−0.9898% −0.7245% −0.4580%] (p = 0.00 < 0.05)
                        thrpt:  [+0.4601% +0.7298% +0.9996%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

span/create             time:   [332.73 ns 333.99 ns 335.38 ns]
                        thrpt:  [2.9817 Melem/s 2.9941 Melem/s 3.0055 Melem/s]
                 change:
                        time:   [−2.5731% −2.0700% −1.6186%] (p = 0.00 < 0.05)
                        thrpt:  [+1.6452% +2.1138% +2.6410%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
span/set_attribute      time:   [74.259 ns 74.683 ns 75.093 ns]
                        thrpt:  [13.317 Melem/s 13.390 Melem/s 13.466 Melem/s]
                 change:
                        time:   [−1.8326% −0.9025% +0.0364%] (p = 0.06 > 0.05)
                        thrpt:  [−0.0364% +0.9107% +1.8668%]
                        No change in performance detected.
span/add_event          time:   [46.946 ns 47.523 ns 48.356 ns]
                        thrpt:  [20.680 Melem/s 21.042 Melem/s 21.301 Melem/s]
                 change:
                        time:   [−1.4674% −0.3048% +1.0154%] (p = 0.64 > 0.05)
                        thrpt:  [−1.0052% +0.3058% +1.4893%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
span/with_kind          time:   [331.32 ns 332.05 ns 332.82 ns]
                        thrpt:  [3.0046 Melem/s 3.0116 Melem/s 3.0182 Melem/s]
                 change:
                        time:   [−3.3593% −2.8935% −2.4445%] (p = 0.00 < 0.05)
                        thrpt:  [+2.5058% +2.9798% +3.4760%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

context_store/create_context
                        time:   [976.78 ns 984.44 ns 992.19 ns]
                        thrpt:  [1.0079 Melem/s 1.0158 Melem/s 1.0238 Melem/s]
                 change:
                        time:   [−4.5158% −3.8616% −3.1796%] (p = 0.00 < 0.05)
                        thrpt:  [+3.2840% +4.0167% +4.7293%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
context_store/get_context
                        time:   [50.624 ns 50.704 ns 50.783 ns]
                        thrpt:  [19.691 Melem/s 19.722 Melem/s 19.753 Melem/s]
                 change:
                        time:   [−3.3836% −3.1145% −2.8557%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9396% +3.2146% +3.5021%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) low mild
  3 (3.00%) high mild
context_store/add_span  time:   [387.05 ns 389.03 ns 391.70 ns]
                        thrpt:  [2.5530 Melem/s 2.5705 Melem/s 2.5836 Melem/s]
                 change:
                        time:   [−2.7477% −2.1808% −1.5119%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5351% +2.2295% +2.8254%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe

propagation_context/from_context
                        time:   [862.11 ns 868.23 ns 874.01 ns]
                        thrpt:  [1.1442 Melem/s 1.1518 Melem/s 1.1599 Melem/s]
                 change:
                        time:   [−0.2396% +0.5114% +1.2640%] (p = 0.18 > 0.05)
                        thrpt:  [−1.2482% −0.5088% +0.2402%]
                        No change in performance detected.
propagation_context/to_context
                        time:   [921.74 ns 925.37 ns 928.87 ns]
                        thrpt:  [1.0766 Melem/s 1.0806 Melem/s 1.0849 Melem/s]
                 change:
                        time:   [−3.0292% −2.4990% −1.9472%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9858% +2.5630% +3.1238%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

context_store_concurrent/concurrent_get
                        time:   [58.719 ns 58.755 ns 58.793 ns]
                        thrpt:  [17.009 Melem/s 17.020 Melem/s 17.030 Melem/s]
                 change:
                        time:   [−5.4397% −4.4117% −3.5888%] (p = 0.00 < 0.05)
                        thrpt:  [+3.7224% +4.6153% +5.7526%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

endpoint/create         time:   [3.0542 ns 3.0560 ns 3.0584 ns]
                        thrpt:  [326.96 Melem/s 327.23 Melem/s 327.42 Melem/s]
                 change:
                        time:   [−2.5253% −2.0209% −1.5531%] (p = 0.00 < 0.05)
                        thrpt:  [+1.5776% +2.0626% +2.5907%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
endpoint/create_with_config
                        time:   [111.80 ns 112.97 ns 114.07 ns]
                        thrpt:  [8.7663 Melem/s 8.8519 Melem/s 8.9445 Melem/s]
                 change:
                        time:   [+2.0097% +3.1896% +4.4364%] (p = 0.00 < 0.05)
                        thrpt:  [−4.2479% −3.0910% −1.9701%]
                        Performance has regressed.
endpoint/effective_weight
                        time:   [310.45 ps 310.77 ps 311.16 ps]
                        thrpt:  [3.2138 Gelem/s 3.2178 Gelem/s 3.2211 Gelem/s]
                 change:
                        time:   [−3.7224% −1.9188% −0.6010%] (p = 0.00 < 0.05)
                        thrpt:  [+0.6046% +1.9563% +3.8663%]
                        Change within noise threshold.
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe

load_metrics/load_score time:   [310.36 ps 310.57 ps 310.87 ps]
                        thrpt:  [3.2168 Gelem/s 3.2198 Gelem/s 3.2221 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) high mild
  10 (10.00%) high severe
load_metrics/is_overloaded
                        time:   [310.47 ps 310.65 ps 310.88 ps]
                        thrpt:  [3.2166 Gelem/s 3.2191 Gelem/s 3.2210 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe

lb_strategies/round_robin
                        time:   [281.98 ns 285.54 ns 289.55 ns]
                        thrpt:  [3.4537 Melem/s 3.5021 Melem/s 3.5464 Melem/s]
lb_strategies/weighted_round_robin
                        time:   [320.06 ns 323.90 ns 327.55 ns]
                        thrpt:  [3.0529 Melem/s 3.0874 Melem/s 3.1244 Melem/s]
lb_strategies/least_connections
                        time:   [280.21 ns 282.57 ns 285.70 ns]
                        thrpt:  [3.5002 Melem/s 3.5390 Melem/s 3.5688 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
lb_strategies/random    time:   [576.22 ns 579.82 ns 583.49 ns]
                        thrpt:  [1.7138 Melem/s 1.7247 Melem/s 1.7354 Melem/s]
lb_strategies/power_of_two
                        time:   [848.17 ns 853.48 ns 859.03 ns]
                        thrpt:  [1.1641 Melem/s 1.1717 Melem/s 1.1790 Melem/s]
lb_strategies/consistent_hash
                        time:   [45.466 µs 47.256 µs 49.088 µs]
                        thrpt:  [20.371 Kelem/s 21.162 Kelem/s 21.994 Kelem/s]
lb_strategies/least_load
                        time:   [495.07 ns 497.80 ns 500.46 ns]
                        thrpt:  [1.9981 Melem/s 2.0088 Melem/s 2.0199 Melem/s]

lb_scaling/select/10    time:   [300.36 ns 303.95 ns 307.50 ns]
                        thrpt:  [3.2521 Melem/s 3.2901 Melem/s 3.3294 Melem/s]
lb_scaling/select/50    time:   [648.80 ns 654.21 ns 659.47 ns]
                        thrpt:  [1.5164 Melem/s 1.5286 Melem/s 1.5413 Melem/s]
lb_scaling/select/100   time:   [1.0402 µs 1.0464 µs 1.0519 µs]
                        thrpt:  [950.68 Kelem/s 955.65 Kelem/s 961.31 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  13 (13.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
lb_scaling/select/500   time:   [2.1955 µs 2.2048 µs 2.2144 µs]
                        thrpt:  [451.59 Kelem/s 453.57 Kelem/s 455.48 Kelem/s]

lb_zone_aware/zone_match
                        time:   [360.15 ns 364.83 ns 369.77 ns]
                        thrpt:  [2.7044 Melem/s 2.7410 Melem/s 2.7766 Melem/s]
lb_zone_aware/zone_fallback
                        time:   [295.59 ns 299.00 ns 302.19 ns]
                        thrpt:  [3.3092 Melem/s 3.3444 Melem/s 3.3831 Melem/s]

lb_health_updates/update_health
                        time:   [26.015 ns 26.282 ns 26.578 ns]
                        thrpt:  [37.624 Melem/s 38.049 Melem/s 38.440 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
lb_health_updates/update_metrics
                        time:   [133.60 ns 135.09 ns 136.35 ns]
                        thrpt:  [7.3342 Melem/s 7.4025 Melem/s 7.4848 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  9 (9.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

     Running benches/origin_cache_bench.rs (target/release/deps/origin_cache_bench-bb2e62dfee6bad84)
Gnuplot not found, using plotters backend
origin_cache_hit/dashmap
                        time:   [12.628 ns 13.092 ns 13.692 ns]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe
origin_cache_hit/mutex_lru
                        time:   [11.183 ns 11.196 ns 11.214 ns]
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe

origin_cache_insert_256/dashmap
                        time:   [11.836 µs 11.885 µs 11.936 µs]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
origin_cache_insert_256/mutex_lru
                        time:   [13.429 µs 13.911 µs 14.352 µs]

     Running benches/parallel.rs (target/release/deps/parallel-c4e61420d9af7bab)
Gnuplot not found, using plotters backend
shard_manager/ingest_json/1
                        time:   [362.46 ns 367.74 ns 372.76 ns]
                        thrpt:  [2.6827 Melem/s 2.7193 Melem/s 2.7589 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
shard_manager/ingest_json/4
                        time:   [363.81 ns 370.16 ns 376.58 ns]
                        thrpt:  [2.6555 Melem/s 2.7015 Melem/s 2.7487 Melem/s]
shard_manager/ingest_json/8
                        time:   [355.53 ns 361.29 ns 367.26 ns]
                        thrpt:  [2.7228 Melem/s 2.7679 Melem/s 2.8127 Melem/s]
shard_manager/ingest_json/16
                        time:   [369.52 ns 376.66 ns 383.69 ns]
                        thrpt:  [2.6063 Melem/s 2.6549 Melem/s 2.7062 Melem/s]
shard_manager/ingest_raw/1
                        time:   [46.503 ns 46.572 ns 46.666 ns]
                        thrpt:  [21.429 Melem/s 21.472 Melem/s 21.504 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe
shard_manager/ingest_raw/4
                        time:   [46.426 ns 46.465 ns 46.513 ns]
                        thrpt:  [21.500 Melem/s 21.522 Melem/s 21.540 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
shard_manager/ingest_raw/8
                        time:   [46.554 ns 46.756 ns 46.987 ns]
                        thrpt:  [21.282 Melem/s 21.388 Melem/s 21.480 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) high mild
  11 (11.00%) high severe
shard_manager/ingest_raw/16
                        time:   [46.392 ns 46.423 ns 46.462 ns]
                        thrpt:  [21.523 Melem/s 21.541 Melem/s 21.556 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe

event_size/small_50b_json
                        time:   [289.57 ns 296.46 ns 303.43 ns]
                        thrpt:  [3.2956 Melem/s 3.3732 Melem/s 3.4534 Melem/s]
event_size/small_50b_raw
                        time:   [46.091 ns 46.125 ns 46.165 ns]
                        thrpt:  [21.661 Melem/s 21.680 Melem/s 21.696 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
event_size/medium_200b_json
                        time:   [817.81 ns 826.64 ns 835.13 ns]
                        thrpt:  [1.1974 Melem/s 1.2097 Melem/s 1.2228 Melem/s]
event_size/medium_200b_raw
                        time:   [46.131 ns 46.169 ns 46.212 ns]
                        thrpt:  [21.639 Melem/s 21.660 Melem/s 21.678 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
event_size/large_1kb_json
                        time:   [2.6770 µs 2.6957 µs 2.7138 µs]
                        thrpt:  [368.49 Kelem/s 370.96 Kelem/s 373.55 Kelem/s]
event_size/large_1kb_raw
                        time:   [46.440 ns 46.463 ns 46.492 ns]
                        thrpt:  [21.509 Melem/s 21.522 Melem/s 21.533 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

Benchmarking parallel/threads/1: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
parallel/threads/1      time:   [1.4968 ms 1.4985 ms 1.5007 ms]
                        thrpt:  [6.6634 Melem/s 6.6734 Melem/s 6.6810 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe
parallel/threads/2      time:   [2.1950 ms 2.2014 ms 2.2085 ms]
                        thrpt:  [9.0558 Melem/s 9.0852 Melem/s 9.1117 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) high mild
  2 (2.00%) high severe
parallel/threads/4      time:   [2.9585 ms 3.0269 ms 3.1327 ms]
                        thrpt:  [12.769 Melem/s 13.215 Melem/s 13.520 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
parallel/threads/8      time:   [9.7392 ms 9.7640 ms 9.7897 ms]
                        thrpt:  [8.1719 Melem/s 8.1934 Melem/s 8.2142 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

     Running benches/placement.rs (target/release/deps/placement-721b5c4ac2cb9da8)
Gnuplot not found, using plotters backend
standard_placement_score/baseline_no_custom_filter/100
                        time:   [59.530 µs 60.179 µs 60.881 µs]
                        thrpt:  [1.6425 Melem/s 1.6617 Melem/s 1.6798 Melem/s]
standard_placement_score/with_custom_filter_rust_callback/100
                        time:   [65.364 µs 65.810 µs 66.276 µs]
                        thrpt:  [1.5088 Melem/s 1.5195 Melem/s 1.5299 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
standard_placement_score/with_custom_filter_predicate/100
                        time:   [115.45 µs 115.89 µs 116.32 µs]
                        thrpt:  [859.70 Kelem/s 862.90 Kelem/s 866.20 Kelem/s]

     Running benches/redex.rs (target/release/deps/redex-bcf441be9d6b3a36)
Gnuplot not found, using plotters backend
redex_append_inline/heap_file
                        time:   [35.091 ns 35.172 ns 35.247 ns]
                        thrpt:  [28.371 Melem/s 28.432 Melem/s 28.498 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

redex_append_heap/heap_file/32
                        time:   [39.786 ns 40.347 ns 40.883 ns]
                        thrpt:  [746.46 MiB/s 756.37 MiB/s 767.04 MiB/s]
Found 15 outliers among 100 measurements (15.00%)
  14 (14.00%) high mild
  1 (1.00%) high severe
redex_append_heap/heap_file/256
                        time:   [74.417 ns 75.599 ns 76.839 ns]
                        thrpt:  [3.1028 GiB/s 3.1537 GiB/s 3.2038 GiB/s]
Found 18 outliers among 100 measurements (18.00%)
  17 (17.00%) low mild
  1 (1.00%) high mild
redex_append_heap/heap_file/1024
                        time:   [198.31 ns 201.22 ns 203.89 ns]
                        thrpt:  [4.6775 GiB/s 4.7394 GiB/s 4.8090 GiB/s]
Found 18 outliers among 100 measurements (18.00%)
  7 (7.00%) low severe
  7 (7.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

redex_append_watcher_paths/no_watchers
                        time:   [74.869 ns 76.166 ns 77.630 ns]
                        thrpt:  [3.0712 GiB/s 3.1303 GiB/s 3.1845 GiB/s]
Found 19 outliers among 100 measurements (19.00%)
  17 (17.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
redex_append_watcher_paths/with_tail
                        time:   [203.16 ns 205.42 ns 207.65 ns]
                        thrpt:  [1.1482 GiB/s 1.1607 GiB/s 1.1736 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_batch/batch_64_x_64B
                        time:   [1.6590 µs 1.6793 µs 1.6983 µs]
                        thrpt:  [37.686 Melem/s 38.111 Melem/s 38.577 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

redex_append_disk/disk_file/32
                        time:   [4.1771 µs 4.1930 µs 4.2099 µs]
                        thrpt:  [7.2490 MiB/s 7.2783 MiB/s 7.3059 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) high mild
  12 (12.00%) high severe
redex_append_disk/disk_file/256
                        time:   [4.5004 µs 4.5162 µs 4.5368 µs]
                        thrpt:  [53.813 MiB/s 54.059 MiB/s 54.249 MiB/s]
Found 18 outliers among 100 measurements (18.00%)
  9 (9.00%) high mild
  9 (9.00%) high severe
redex_append_disk/disk_file/1024
                        time:   [5.6776 µs 5.6973 µs 5.7197 µs]
                        thrpt:  [170.74 MiB/s 171.41 MiB/s 172.00 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) high mild
  13 (13.00%) high severe

redex_append_batch_disk/batch_64_x/64
                        time:   [13.391 µs 13.480 µs 13.581 µs]
                        thrpt:  [4.7124 Melem/s 4.7479 Melem/s 4.7794 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  2 (2.00%) high mild
  15 (15.00%) high severe
redex_append_batch_disk/batch_64_x/1024
                        time:   [57.589 µs 58.397 µs 59.196 µs]
                        thrpt:  [1.0812 Melem/s 1.0959 Melem/s 1.1113 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  5 (5.00%) high severe

redex_append_disk_policies/disk_file_256B/never
                        time:   [4.4627 µs 4.4739 µs 4.4880 µs]
                        thrpt:  [54.398 MiB/s 54.570 MiB/s 54.707 MiB/s]
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) high mild
  12 (12.00%) high severe
redex_append_disk_policies/disk_file_256B/every_n_1
                        time:   [1.1666 ms 1.3203 ms 1.4618 ms]
                        thrpt:  [171.02 KiB/s 189.35 KiB/s 214.29 KiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
redex_append_disk_policies/disk_file_256B/every_n_64
                        time:   [204.59 µs 214.77 µs 223.42 µs]
                        thrpt:  [1.0927 MiB/s 1.1367 MiB/s 1.1933 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) low mild
redex_append_disk_policies/disk_file_256B/interval_50ms
                        time:   [5.0886 µs 5.3035 µs 5.5062 µs]
                        thrpt:  [44.339 MiB/s 46.034 MiB/s 47.978 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
redex_append_disk_policies/disk_file_256B/interval_or_bytes
                        time:   [7.9087 µs 8.1082 µs 8.2876 µs]
                        thrpt:  [29.458 MiB/s 30.110 MiB/s 30.870 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

redex_append_batch_disk_policies/batch_64_x_64B/never
                        time:   [13.856 µs 13.988 µs 14.120 µs]
                        thrpt:  [4.5327 Melem/s 4.5752 Melem/s 4.6189 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  14 (14.00%) high severe
Benchmarking redex_append_batch_disk_policies/batch_64_x_64B/every_n_1: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.1s, enable flat sampling, or reduce sample count to 60.
redex_append_batch_disk_policies/batch_64_x_64B/every_n_1
                        time:   [1.7759 ms 1.9648 ms 2.1347 ms]
                        thrpt:  [29.981 Kelem/s 32.573 Kelem/s 36.037 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, enable flat sampling, or reduce sample count to 50.
redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small
                        time:   [1.9466 ms 2.1491 ms 2.3363 ms]
                        thrpt:  [27.394 Kelem/s 29.779 Kelem/s 32.879 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

redex_tail/append_to_next
                        time:   [158.32 ns 158.77 ns 159.19 ns]
                        thrpt:  [6.2820 Melem/s 6.2986 Melem/s 6.3163 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe