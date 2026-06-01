     Running benches\auth_guard.rs (target\release\deps\auth_guard-1a970f27d1b1ff03.exe)
Gnuplot not found, using plotters backend
auth_guard_check_fast_hit/single_thread
                        time:   [17.535 ns 17.582 ns 17.643 ns]
                        thrpt:  [56.678 Melem/s 56.877 Melem/s 57.028 Melem/s]
                 change:
                        time:   [−36.042% −28.751% −20.001%] (p = 0.00 < 0.05)
                        thrpt:  [+25.002% +40.354% +56.353%]
                        Performance has improved.
Found 9 outliers among 50 measurements (18.00%)
  4 (8.00%) high mild
  5 (10.00%) high severe

auth_guard_check_fast_miss/single_thread
                        time:   [3.0609 ns 3.0708 ns 3.0835 ns]
                        thrpt:  [324.31 Melem/s 325.64 Melem/s 326.70 Melem/s]
                 change:
                        time:   [−47.467% −47.204% −46.921%] (p = 0.00 < 0.05)
                        thrpt:  [+88.398% +89.409% +90.356%]
                        Performance has improved.
Found 11 outliers among 50 measurements (22.00%)
  2 (4.00%) low severe
  2 (4.00%) low mild
  2 (4.00%) high mild
  5 (10.00%) high severe

auth_guard_check_fast_contended/eight_threads
                        time:   [14.189 ns 14.622 ns 15.116 ns]
                        thrpt:  [66.154 Melem/s 68.389 Melem/s 70.477 Melem/s]
                 change:
                        time:   [−43.356% −39.960% −36.463%] (p = 0.00 < 0.05)
                        thrpt:  [+57.388% +66.557% +76.542%]
                        Performance has improved.
Found 5 outliers among 50 measurements (10.00%)
  1 (2.00%) high mild
  4 (8.00%) high severe

auth_guard_allow_channel/insert
                        time:   [119.54 ns 125.20 ns 130.10 ns]
                        thrpt:  [7.6862 Melem/s 7.9873 Melem/s 8.3651 Melem/s]
                 change:
                        time:   [−46.276% −44.181% −42.016%] (p = 0.00 < 0.05)
                        thrpt:  [+72.462% +79.151% +86.136%]
                        Performance has improved.

auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.5222 ms 2.5243 ms 2.5267 ms]
                        change: [−48.681% −48.616% −48.517%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 6 outliers among 50 measurements (12.00%)
  4 (8.00%) high mild
  2 (4.00%) high severe

     Running benches\cortex.rs (target\release\deps\cortex-d014489fa8cbca44.exe)
Gnuplot not found, using plotters backend
cortex_ingest/tasks_create
                        time:   [213.53 ns 214.71 ns 216.13 ns]
                        thrpt:  [4.6269 Melem/s 4.6574 Melem/s 4.6832 Melem/s]
                 change:
                        time:   [−16.581% −10.226% −3.2976%] (p = 0.01 < 0.05)
                        thrpt:  [+3.4101% +11.390% +19.877%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
cortex_ingest/memories_store
                        time:   [436.79 ns 451.20 ns 467.72 ns]
                        thrpt:  [2.1380 Melem/s 2.2163 Melem/s 2.2894 Melem/s]
                 change:
                        time:   [−48.686% −45.045% −41.145%] (p = 0.00 < 0.05)
                        thrpt:  [+69.909% +81.967% +94.877%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

cortex_fold_barrier/tasks_create_and_wait
                        time:   [1.9635 µs 2.0733 µs 2.1968 µs]
                        thrpt:  [455.20 Kelem/s 482.32 Kelem/s 509.30 Kelem/s]
                 change:
                        time:   [−91.713% −91.364% −90.958%] (p = 0.00 < 0.05)
                        thrpt:  [+1005.9% +1058.0% +1106.7%]
                        Performance has improved.
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
cortex_fold_barrier/memories_store_and_wait
                        time:   [2.6923 µs 3.0817 µs 3.5574 µs]
                        thrpt:  [281.10 Kelem/s 324.49 Kelem/s 371.42 Kelem/s]
                 change:
                        time:   [−88.979% −87.493% −85.912%] (p = 0.00 < 0.05)
                        thrpt:  [+609.82% +699.53% +807.39%]
                        Performance has improved.
Found 21 outliers among 100 measurements (21.00%)
  21 (21.00%) high severe

cortex_query/tasks_find_many/100
                        time:   [2.0242 µs 2.0268 µs 2.0296 µs]
                        thrpt:  [49.270 Melem/s 49.339 Melem/s 49.401 Melem/s]
                 change:
                        time:   [−43.366% −43.045% −42.732%] (p = 0.00 < 0.05)
                        thrpt:  [+74.619% +75.578% +76.571%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
cortex_query/tasks_count_where/100
                        time:   [97.520 ns 98.141 ns 98.767 ns]
                        thrpt:  [1.0125 Gelem/s 1.0189 Gelem/s 1.0254 Gelem/s]
                 change:
                        time:   [−51.074% −50.852% −50.586%] (p = 0.00 < 0.05)
                        thrpt:  [+102.37% +103.47% +104.39%]
                        Performance has improved.
cortex_query/tasks_find_unique/100
                        time:   [6.7364 ns 6.7410 ns 6.7467 ns]
                        thrpt:  [14.822 Gelem/s 14.835 Gelem/s 14.845 Gelem/s]
                 change:
                        time:   [−53.536% −53.272% −53.017%] (p = 0.00 < 0.05)
                        thrpt:  [+112.84% +114.01% +115.22%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) high mild
  14 (14.00%) high severe
cortex_query/memories_find_many_tag/100
                        time:   [882.93 ns 884.86 ns 887.06 ns]
                        thrpt:  [112.73 Melem/s 113.01 Melem/s 113.26 Melem/s]
                 change:
                        time:   [−50.690% −50.533% −50.389%] (p = 0.00 < 0.05)
                        thrpt:  [+101.57% +102.16% +102.80%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_count_where/100
                        time:   [524.20 ns 527.06 ns 529.99 ns]
                        thrpt:  [188.68 Melem/s 189.73 Melem/s 190.77 Melem/s]
                 change:
                        time:   [−50.196% −49.913% −49.611%] (p = 0.00 < 0.05)
                        thrpt:  [+98.455% +99.651% +100.79%]
                        Performance has improved.
cortex_query/tasks_find_many/1000
                        time:   [19.217 µs 19.242 µs 19.272 µs]
                        thrpt:  [51.889 Melem/s 51.969 Melem/s 52.038 Melem/s]
                 change:
                        time:   [−44.836% −44.459% −44.097%] (p = 0.00 < 0.05)
                        thrpt:  [+78.880% +80.046% +81.278%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
cortex_query/tasks_count_where/1000
                        time:   [903.74 ns 909.26 ns 914.75 ns]
                        thrpt:  [1.0932 Gelem/s 1.0998 Gelem/s 1.1065 Gelem/s]
                 change:
                        time:   [−58.548% −58.311% −58.069%] (p = 0.00 < 0.05)
                        thrpt:  [+138.49% +139.87% +141.24%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  5 (5.00%) high mild
cortex_query/tasks_find_unique/1000
                        time:   [6.7332 ns 6.7388 ns 6.7448 ns]
                        thrpt:  [148.26 Gelem/s 148.39 Gelem/s 148.52 Gelem/s]
                 change:
                        time:   [−54.121% −53.855% −53.590%] (p = 0.00 < 0.05)
                        thrpt:  [+115.47% +116.71% +117.96%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  11 (11.00%) high severe
cortex_query/memories_find_many_tag/1000
                        time:   [8.0567 µs 8.0839 µs 8.1122 µs]
                        thrpt:  [123.27 Melem/s 123.70 Melem/s 124.12 Melem/s]
                 change:
                        time:   [−63.238% −63.082% −62.930%] (p = 0.00 < 0.05)
                        thrpt:  [+169.76% +170.87% +172.02%]
                        Performance has improved.
cortex_query/memories_count_where/1000
                        time:   [7.3627 µs 7.3892 µs 7.4136 µs]
                        thrpt:  [134.89 Melem/s 135.33 Melem/s 135.82 Melem/s]
                 change:
                        time:   [−55.170% −54.981% −54.795%] (p = 0.00 < 0.05)
                        thrpt:  [+121.22% +122.13% +123.07%]
                        Performance has improved.
cortex_query/tasks_find_many/10000
                        time:   [172.53 µs 172.97 µs 173.47 µs]
                        thrpt:  [57.647 Melem/s 57.812 Melem/s 57.962 Melem/s]
                 change:
                        time:   [−41.958% −41.493% −41.035%] (p = 0.00 < 0.05)
                        thrpt:  [+69.591% +70.919% +72.288%]
                        Performance has improved.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
cortex_query/tasks_count_where/10000
                        time:   [30.585 µs 30.635 µs 30.688 µs]
                        thrpt:  [325.86 Melem/s 326.42 Melem/s 326.96 Melem/s]
                 change:
                        time:   [−37.197% −36.731% −36.310%] (p = 0.00 < 0.05)
                        thrpt:  [+57.010% +58.055% +59.227%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
cortex_query/tasks_find_unique/10000
                        time:   [6.7324 ns 6.7397 ns 6.7472 ns]
                        thrpt:  [1482.1 Gelem/s 1483.7 Gelem/s 1485.3 Gelem/s]
                 change:
                        time:   [−53.462% −53.288% −53.110%] (p = 0.00 < 0.05)
                        thrpt:  [+113.27% +114.08% +114.88%]
                        Performance has improved.
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  9 (9.00%) high severe
cortex_query/memories_find_many_tag/10000
                        time:   [159.21 µs 159.44 µs 159.67 µs]
                        thrpt:  [62.630 Melem/s 62.721 Melem/s 62.809 Melem/s]
                 change:
                        time:   [−41.430% −40.857% −40.335%] (p = 0.00 < 0.05)
                        thrpt:  [+67.603% +69.083% +70.735%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_count_where/10000
                        time:   [127.45 µs 127.75 µs 128.07 µs]
                        thrpt:  [78.082 Melem/s 78.275 Melem/s 78.465 Melem/s]
                 change:
                        time:   [−34.195% −33.755% −33.327%] (p = 0.00 < 0.05)
                        thrpt:  [+49.986% +50.955% +51.964%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe

cortex_snapshot/tasks_encode/100
                        time:   [3.0826 µs 3.0855 µs 3.0885 µs]
                        thrpt:  [32.378 Melem/s 32.410 Melem/s 32.440 Melem/s]
                 change:
                        time:   [−46.012% −45.266% −44.543%] (p = 0.00 < 0.05)
                        thrpt:  [+80.321% +82.701% +85.226%]
                        Performance has improved.
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
cortex_snapshot/memories_encode/100
                        time:   [5.2376 µs 5.2434 µs 5.2492 µs]
                        thrpt:  [19.050 Melem/s 19.072 Melem/s 19.093 Melem/s]
                 change:
                        time:   [−47.043% −46.114% −45.363%] (p = 0.00 < 0.05)
                        thrpt:  [+83.026% +85.577% +88.832%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_3939/100
                        time:   [2.0924 µs 2.0949 µs 2.0977 µs]
                        thrpt:  [47.671 Melem/s 47.734 Melem/s 47.792 Melem/s]
                 change:
                        time:   [−41.227% −40.957% −40.586%] (p = 0.00 < 0.05)
                        thrpt:  [+68.310% +69.367% +70.146%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
cortex_snapshot/netdb_bundle_decode/100
                        time:   [2.5594 µs 2.5617 µs 2.5642 µs]
                        thrpt:  [38.999 Melem/s 39.036 Melem/s 39.071 Melem/s]
                 change:
                        time:   [−34.463% −34.367% −34.269%] (p = 0.00 < 0.05)
                        thrpt:  [+52.136% +52.362% +52.586%]
                        Performance has improved.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
cortex_snapshot/tasks_encode/1000
                        time:   [31.588 µs 31.611 µs 31.636 µs]
                        thrpt:  [31.609 Melem/s 31.635 Melem/s 31.658 Melem/s]
                 change:
                        time:   [−39.888% −39.654% −39.448%] (p = 0.00 < 0.05)
                        thrpt:  [+65.148% +65.711% +66.355%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  10 (10.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/memories_encode/1000
                        time:   [51.791 µs 51.820 µs 51.853 µs]
                        thrpt:  [19.285 Melem/s 19.298 Melem/s 19.309 Melem/s]
                 change:
                        time:   [−44.688% −43.880% −43.167%] (p = 0.00 < 0.05)
                        thrpt:  [+75.954% +78.191% +80.793%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  4 (4.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_48274/1000
                        time:   [24.677 µs 24.702 µs 24.729 µs]
                        thrpt:  [40.438 Melem/s 40.483 Melem/s 40.523 Melem/s]
                 change:
                        time:   [−35.186% −35.106% −35.025%] (p = 0.00 < 0.05)
                        thrpt:  [+53.906% +54.097% +54.288%]
                        Performance has improved.
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) high mild
  10 (10.00%) high severe
cortex_snapshot/netdb_bundle_decode/1000
                        time:   [32.223 µs 32.239 µs 32.257 µs]
                        thrpt:  [31.001 Melem/s 31.019 Melem/s 31.033 Melem/s]
                 change:
                        time:   [−30.785% −30.706% −30.620%] (p = 0.00 < 0.05)
                        thrpt:  [+44.134% +44.313% +44.478%]
                        Performance has improved.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  6 (6.00%) high severe
cortex_snapshot/tasks_encode/10000
                        time:   [328.59 µs 328.97 µs 329.39 µs]
                        thrpt:  [30.359 Melem/s 30.398 Melem/s 30.433 Melem/s]
                 change:
                        time:   [−37.879% −36.295% −34.871%] (p = 0.00 < 0.05)
                        thrpt:  [+53.541% +56.973% +60.975%]
                        Performance has improved.
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low severe
  7 (7.00%) high mild
  7 (7.00%) high severe
cortex_snapshot/memories_encode/10000
                        time:   [568.67 µs 569.44 µs 570.21 µs]
                        thrpt:  [17.537 Melem/s 17.561 Melem/s 17.585 Melem/s]
                 change:
                        time:   [−45.399% −43.736% −42.169%] (p = 0.00 < 0.05)
                        thrpt:  [+72.918% +77.732% +83.147%]
                        Performance has improved.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_511774/10000
                        time:   [251.10 µs 251.40 µs 251.85 µs]
                        thrpt:  [39.706 Melem/s 39.777 Melem/s 39.825 Melem/s]
                 change:
                        time:   [−32.422% −32.264% −32.102%] (p = 0.00 < 0.05)
                        thrpt:  [+47.279% +47.631% +47.977%]
                        Performance has improved.
Found 20 outliers among 100 measurements (20.00%)
  3 (3.00%) high mild
  17 (17.00%) high severe
cortex_snapshot/netdb_bundle_decode/10000
                        time:   [340.41 µs 340.59 µs 340.81 µs]
                        thrpt:  [29.342 Melem/s 29.361 Melem/s 29.376 Melem/s]
                 change:
                        time:   [−30.221% −30.139% −30.060%] (p = 0.00 < 0.05)
                        thrpt:  [+42.980% +43.142% +43.309%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe

     Running benches\ingestion.rs (target\release\deps\ingestion-6f9a606533c02b39.exe)
Gnuplot not found, using plotters backend
shard/ingest_raw/1024   time:   [104.08 ns 104.31 ns 104.63 ns]
                        thrpt:  [9.5579 Melem/s 9.5871 Melem/s 9.6084 Melem/s]
                 change:
                        time:   [−1.1998% −0.6535% −0.1464%] (p = 0.02 < 0.05)
                        thrpt:  [+0.1466% +0.6578% +1.2143%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
shard/ingest_raw_pop/1024
                        time:   [83.572 ns 83.709 ns 83.856 ns]
                        thrpt:  [11.925 Melem/s 11.946 Melem/s 11.966 Melem/s]
                 change:
                        time:   [−0.3928% −0.1610% +0.0567%] (p = 0.15 > 0.05)
                        thrpt:  [−0.0567% +0.1612% +0.3944%]
                        No change in performance detected.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
shard/ingest_raw/8192   time:   [103.77 ns 103.93 ns 104.10 ns]
                        thrpt:  [9.6058 Melem/s 9.6219 Melem/s 9.6364 Melem/s]
                 change:
                        time:   [−2.6864% −1.5239% −0.2977%] (p = 0.01 < 0.05)
                        thrpt:  [+0.2986% +1.5475% +2.7605%]
                        Change within noise threshold.
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) low severe
  4 (4.00%) low mild
  3 (3.00%) high mild
shard/ingest_raw_pop/8192
                        time:   [84.006 ns 84.160 ns 84.331 ns]
                        thrpt:  [11.858 Melem/s 11.882 Melem/s 11.904 Melem/s]
                 change:
                        time:   [−15.630% −12.243% −8.7908%] (p = 0.00 < 0.05)
                        thrpt:  [+9.6381% +13.951% +18.526%]
                        Performance has improved.
shard/ingest_raw/65536  time:   [100.81 ns 101.07 ns 101.28 ns]
                        thrpt:  [9.8734 Melem/s 9.8941 Melem/s 9.9199 Melem/s]
                 change:
                        time:   [−36.245% −34.583% −32.948%] (p = 0.00 < 0.05)
                        thrpt:  [+49.138% +52.866% +56.850%]
                        Performance has improved.
Found 12 outliers among 100 measurements (12.00%)
  8 (8.00%) low severe
  4 (4.00%) low mild
shard/ingest_raw_pop/65536
                        time:   [84.383 ns 84.550 ns 84.738 ns]
                        thrpt:  [11.801 Melem/s 11.827 Melem/s 11.851 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
shard/ingest_raw/1048576
                        time:   [72.946 ns 73.042 ns 73.150 ns]
                        thrpt:  [13.671 Melem/s 13.691 Melem/s 13.709 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
shard/ingest_raw_pop/1048576
                        time:   [87.852 ns 88.006 ns 88.179 ns]
                        thrpt:  [11.341 Melem/s 11.363 Melem/s 11.383 Melem/s]

timestamp/next          time:   [16.381 ns 16.405 ns 16.432 ns]
                        thrpt:  [60.857 Melem/s 60.957 Melem/s 61.045 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
timestamp/now_raw       time:   [6.2349 ns 6.2419 ns 6.2496 ns]
                        thrpt:  [160.01 Melem/s 160.21 Melem/s 160.39 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe

event/internal_event_new
                        time:   [212.44 ns 212.71 ns 213.02 ns]
                        thrpt:  [4.6945 Melem/s 4.7012 Melem/s 4.7073 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
event/internal_event_from_bytes
                        time:   [26.461 ns 26.480 ns 26.501 ns]
                        thrpt:  [37.734 Melem/s 37.764 Melem/s 37.792 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
event/json_creation     time:   [135.26 ns 135.54 ns 135.88 ns]
                        thrpt:  [7.3595 Melem/s 7.3777 Melem/s 7.3932 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe

batch/pop_batch_steady_state/100
                        time:   [7.1163 µs 7.1262 µs 7.1373 µs]
                        thrpt:  [14.011 Melem/s 14.033 Melem/s 14.052 Melem/s]
batch/pop_batch_steady_state/1000
                        time:   [71.589 µs 71.687 µs 71.796 µs]
                        thrpt:  [13.928 Melem/s 13.950 Melem/s 13.969 Melem/s]
batch/pop_batch_steady_state/10000
                        time:   [710.86 µs 713.63 µs 716.13 µs]
                        thrpt:  [13.964 Melem/s 14.013 Melem/s 14.067 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

     Running benches\mesh.rs (target\release\deps\mesh-1a2c00ef42e74036.exe)
Gnuplot not found, using plotters backend
mesh_reroute/triangle_failure
                        time:   [22.839 µs 23.185 µs 23.545 µs]
                        thrpt:  [42.471 Kelem/s 43.132 Kelem/s 43.785 Kelem/s]
mesh_reroute/10_peers_10_routes
                        time:   [122.88 µs 123.73 µs 124.67 µs]
                        thrpt:  [8.0210 Kelem/s 8.0821 Kelem/s 8.1380 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_reroute/50_peers_100_routes: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, enable flat sampling, or reduce sample count to 60.
mesh_reroute/50_peers_100_routes
                        time:   [1.1915 ms 1.1953 ms 1.1992 ms]
                        thrpt:  [833.90  elem/s 836.60  elem/s 839.24  elem/s]
Found 11 outliers among 100 measurements (11.00%)
  8 (8.00%) high mild
  3 (3.00%) high severe

mesh_proximity/on_pingwave_new
                        time:   [167.43 ns 171.04 ns 174.31 ns]
                        thrpt:  [5.7371 Melem/s 5.8466 Melem/s 5.9726 Melem/s]
mesh_proximity/on_pingwave_dedup
                        time:   [49.858 ns 49.901 ns 49.951 ns]
                        thrpt:  [20.020 Melem/s 20.040 Melem/s 20.057 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
mesh_proximity/pingwave_serialize
                        time:   [1.1209 ns 1.1589 ns 1.2011 ns]
                        thrpt:  [832.57 Melem/s 862.87 Melem/s 892.12 Melem/s]
mesh_proximity/pingwave_deserialize
                        time:   [1.4710 ns 1.4950 ns 1.5217 ns]
                        thrpt:  [657.18 Melem/s 668.88 Melem/s 679.81 Melem/s]
mesh_proximity/node_count
                        time:   [957.92 ns 958.79 ns 959.80 ns]
                        thrpt:  [1.0419 Melem/s 1.0430 Melem/s 1.0439 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
mesh_proximity/all_nodes_100
                        time:   [10.073 µs 10.096 µs 10.118 µs]
                        thrpt:  [98.833 Kelem/s 99.047 Kelem/s 99.274 Kelem/s]

mesh_dispatch/classify_direct
                        time:   [401.47 ps 401.92 ps 402.32 ps]
                        thrpt:  [2.4856 Gelem/s 2.4880 Gelem/s 2.4908 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  5 (5.00%) high mild
  1 (1.00%) high severe
mesh_dispatch/classify_routed
                        time:   [301.46 ps 301.75 ps 302.08 ps]
                        thrpt:  [3.3104 Gelem/s 3.3140 Gelem/s 3.3172 Gelem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
mesh_dispatch/classify_pingwave
                        time:   [200.98 ps 201.14 ps 201.31 ps]
                        thrpt:  [4.9676 Gelem/s 4.9717 Gelem/s 4.9755 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild

mesh_routing/lookup_hit time:   [17.765 ns 17.824 ns 17.897 ns]
                        thrpt:  [55.876 Melem/s 56.103 Melem/s 56.289 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
mesh_routing/lookup_miss
                        time:   [17.709 ns 17.738 ns 17.779 ns]
                        thrpt:  [56.245 Melem/s 56.375 Melem/s 56.468 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) high mild
  5 (5.00%) high severe
mesh_routing/is_local   time:   [200.96 ps 201.10 ps 201.27 ps]
                        thrpt:  [4.9685 Gelem/s 4.9726 Gelem/s 4.9760 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
mesh_routing/all_routes/10
                        time:   [5.5654 µs 5.5792 µs 5.5924 µs]
                        thrpt:  [178.82 Kelem/s 179.24 Kelem/s 179.68 Kelem/s]
mesh_routing/all_routes/100
                        time:   [7.3757 µs 7.3910 µs 7.4061 µs]
                        thrpt:  [135.02 Kelem/s 135.30 Kelem/s 135.58 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
mesh_routing/all_routes/1000
                        time:   [23.486 µs 23.521 µs 23.552 µs]
                        thrpt:  [42.459 Kelem/s 42.515 Kelem/s 42.578 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
mesh_routing/add_route  time:   [37.705 ns 37.749 ns 37.794 ns]
                        thrpt:  [26.459 Melem/s 26.491 Melem/s 26.522 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

     Running benches\net.rs (target\release\deps\net-52dbd8485ba434df.exe)
Gnuplot not found, using plotters backend
net_header/serialize    time:   [1.2058 ns 1.2065 ns 1.2072 ns]
                        thrpt:  [828.36 Melem/s 828.88 Melem/s 829.32 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
net_header/deserialize  time:   [1.6085 ns 1.6098 ns 1.6113 ns]
                        thrpt:  [620.60 Melem/s 621.18 Melem/s 621.71 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
net_header/roundtrip    time:   [1.6079 ns 1.6091 ns 1.6104 ns]
                        thrpt:  [620.95 Melem/s 621.46 Melem/s 621.94 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe

net_event_frame/write_single/64
                        time:   [35.452 ns 35.511 ns 35.575 ns]
                        thrpt:  [1.6755 GiB/s 1.6785 GiB/s 1.6813 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
net_event_frame/write_single_reused/64
                        time:   [2.2003 ns 2.2033 ns 2.2065 ns]
                        thrpt:  [27.013 GiB/s 27.052 GiB/s 27.089 GiB/s]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) low severe
  6 (6.00%) low mild
  5 (5.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single/256
                        time:   [35.832 ns 35.891 ns 35.953 ns]
                        thrpt:  [6.6314 GiB/s 6.6428 GiB/s 6.6538 GiB/s]
net_event_frame/write_single_reused/256
                        time:   [3.7086 ns 4.0222 ns 4.3969 ns]
                        thrpt:  [54.225 GiB/s 59.276 GiB/s 64.287 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  12 (12.00%) high severe
net_event_frame/write_single/1024
                        time:   [35.716 ns 35.845 ns 35.979 ns]
                        thrpt:  [26.506 GiB/s 26.605 GiB/s 26.702 GiB/s]
net_event_frame/write_single_reused/1024
                        time:   [6.1749 ns 6.2198 ns 6.2679 ns]
                        thrpt:  [152.15 GiB/s 153.33 GiB/s 154.44 GiB/s]
net_event_frame/write_single/4096
                        time:   [47.661 ns 47.758 ns 47.863 ns]
                        thrpt:  [79.700 GiB/s 79.875 GiB/s 80.039 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
net_event_frame/write_single_reused/4096
                        time:   [21.381 ns 21.399 ns 21.419 ns]
                        thrpt:  [178.10 GiB/s 178.27 GiB/s 178.42 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
net_event_frame/write_batch/1
                        time:   [27.297 ns 27.400 ns 27.512 ns]
                        thrpt:  [2.1665 GiB/s 2.1754 GiB/s 2.1835 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
net_event_frame/write_batch/10
                        time:   [59.207 ns 59.340 ns 59.472 ns]
                        thrpt:  [10.022 GiB/s 10.045 GiB/s 10.067 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
net_event_frame/write_batch/50
                        time:   [147.93 ns 148.11 ns 148.29 ns]
                        thrpt:  [20.097 GiB/s 20.122 GiB/s 20.147 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
net_event_frame/write_batch/100
                        time:   [275.63 ns 275.98 ns 276.36 ns]
                        thrpt:  [21.567 GiB/s 21.597 GiB/s 21.625 GiB/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  13 (13.00%) high mild
  3 (3.00%) high severe
net_event_frame/read_batch_10
                        time:   [163.42 ns 163.67 ns 163.95 ns]
                        thrpt:  [60.993 Melem/s 61.099 Melem/s 61.193 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

net_packet_pool/get_return/16
                        time:   [51.586 ns 51.713 ns 51.831 ns]
                        thrpt:  [19.294 Melem/s 19.337 Melem/s 19.385 Melem/s]
net_packet_pool/get_return/64
                        time:   [51.777 ns 51.880 ns 51.976 ns]
                        thrpt:  [19.240 Melem/s 19.275 Melem/s 19.313 Melem/s]
net_packet_pool/get_return/256
                        time:   [52.702 ns 52.841 ns 52.980 ns]
                        thrpt:  [18.875 Melem/s 18.925 Melem/s 18.975 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

net_packet_build/build_packet/1
                        time:   [1.1318 µs 1.1335 µs 1.1352 µs]
                        thrpt:  [53.767 MiB/s 53.848 MiB/s 53.927 MiB/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
net_packet_build/build_packet/10
                        time:   [1.5010 µs 1.5032 µs 1.5053 µs]
                        thrpt:  [405.47 MiB/s 406.04 MiB/s 406.62 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
net_packet_build/build_packet/50
                        time:   [2.9296 µs 2.9308 µs 2.9319 µs]
                        thrpt:  [1.0165 GiB/s 1.0169 GiB/s 1.0173 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe

net_encryption/encrypt/64
                        time:   [1.1306 µs 1.1314 µs 1.1321 µs]
                        thrpt:  [53.911 MiB/s 53.947 MiB/s 53.984 MiB/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
net_encryption/encrypt/256
                        time:   [1.1985 µs 1.1994 µs 1.2004 µs]
                        thrpt:  [203.38 MiB/s 203.54 MiB/s 203.70 MiB/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
net_encryption/encrypt/1024
                        time:   [1.5733 µs 1.5748 µs 1.5764 µs]
                        thrpt:  [619.49 MiB/s 620.12 MiB/s 620.70 MiB/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
net_encryption/encrypt/4096
                        time:   [3.1298 µs 3.1313 µs 3.1331 µs]
                        thrpt:  [1.2175 GiB/s 1.2182 GiB/s 1.2188 GiB/s]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) high mild
  8 (8.00%) high severe

net_keypair/generate    time:   [10.842 µs 10.859 µs 10.880 µs]
                        thrpt:  [91.908 Kelem/s 92.091 Kelem/s 92.237 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe

net_aad/generate        time:   [1.0188 ns 1.0640 ns 1.1165 ns]
                        thrpt:  [895.66 Melem/s 939.84 Melem/s 981.58 Melem/s]

pool_comparison/shared_pool_get_return
                        time:   [51.624 ns 51.765 ns 51.883 ns]
                        thrpt:  [19.274 Melem/s 19.318 Melem/s 19.371 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high severe
pool_comparison/thread_local_pool_get_return
                        time:   [65.086 ns 65.532 ns 65.955 ns]
                        thrpt:  [15.162 Melem/s 15.260 Melem/s 15.364 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
pool_comparison/shared_pool_10x
                        time:   [445.24 ns 447.21 ns 449.19 ns]
                        thrpt:  [2.2262 Melem/s 2.2361 Melem/s 2.2460 Melem/s]
Found 27 outliers among 100 measurements (27.00%)
  10 (10.00%) low mild
  17 (17.00%) high severe
pool_comparison/thread_local_pool_10x
                        time:   [787.94 ns 797.09 ns 807.12 ns]
                        thrpt:  [1.2390 Melem/s 1.2546 Melem/s 1.2691 Melem/s]

cipher_comparison/shared_pool/64
                        time:   [1.1309 µs 1.1317 µs 1.1326 µs]
                        thrpt:  [53.891 MiB/s 53.934 MiB/s 53.972 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/64
                        time:   [1.1319 µs 1.1327 µs 1.1335 µs]
                        thrpt:  [53.847 MiB/s 53.885 MiB/s 53.923 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
cipher_comparison/shared_pool/256
                        time:   [1.2004 µs 1.2018 µs 1.2033 µs]
                        thrpt:  [202.89 MiB/s 203.14 MiB/s 203.39 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
cipher_comparison/fast_chacha20/256
                        time:   [1.2011 µs 1.2019 µs 1.2029 µs]
                        thrpt:  [202.96 MiB/s 203.12 MiB/s 203.26 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
cipher_comparison/shared_pool/1024
                        time:   [1.5734 µs 1.5750 µs 1.5767 µs]
                        thrpt:  [619.37 MiB/s 620.03 MiB/s 620.68 MiB/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/1024
                        time:   [1.5770 µs 1.5781 µs 1.5795 µs]
                        thrpt:  [618.29 MiB/s 618.81 MiB/s 619.26 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
cipher_comparison/shared_pool/4096
                        time:   [3.1313 µs 3.1346 µs 3.1381 µs]
                        thrpt:  [1.2156 GiB/s 1.2170 GiB/s 1.2182 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/4096
                        time:   [3.0648 µs 3.0675 µs 3.0707 µs]
                        thrpt:  [1.2423 GiB/s 1.2436 GiB/s 1.2447 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe

adaptive_batcher/optimal_size
                        time:   [804.06 ps 804.91 ps 805.94 ps]
                        thrpt:  [1.2408 Gelem/s 1.2424 Gelem/s 1.2437 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
adaptive_batcher/record time:   [9.4417 ns 9.4511 ns 9.4612 ns]
                        thrpt:  [105.69 Melem/s 105.81 Melem/s 105.91 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe
adaptive_batcher/full_cycle
                        time:   [8.0521 ns 8.0591 ns 8.0669 ns]
                        thrpt:  [123.96 Melem/s 124.08 Melem/s 124.19 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe

e2e_packet_build/shared_pool_50_events
                        time:   [2.8591 µs 2.8771 µs 2.8943 µs]
                        thrpt:  [1.0297 GiB/s 1.0358 GiB/s 1.0424 GiB/s]
Found 20 outliers among 100 measurements (20.00%)
  19 (19.00%) low mild
  1 (1.00%) high severe
e2e_packet_build/fast_50_events
                        time:   [2.8433 µs 2.8565 µs 2.8679 µs]
                        thrpt:  [1.0392 GiB/s 1.0433 GiB/s 1.0482 GiB/s]
Found 20 outliers among 100 measurements (20.00%)
  16 (16.00%) low severe
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.4s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/shared_pool/8
                        time:   [1.6764 ms 1.7001 ms 1.7228 ms]
                        thrpt:  [4.6436 Melem/s 4.7057 Melem/s 4.7722 Melem/s]
Benchmarking multithread_packet_build/thread_local_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.6s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/8
                        time:   [1.4950 ms 1.5130 ms 1.5295 ms]
                        thrpt:  [5.2305 Melem/s 5.2876 Melem/s 5.3511 Melem/s]
multithread_packet_build/shared_pool/16
                        time:   [2.5614 ms 2.5712 ms 2.5811 ms]
                        thrpt:  [6.1990 Melem/s 6.2228 Melem/s 6.2465 Melem/s]
Benchmarking multithread_packet_build/thread_local_pool/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.7s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/16
                        time:   [1.9217 ms 1.9272 ms 1.9321 ms]
                        thrpt:  [8.2812 Melem/s 8.3023 Melem/s 8.3260 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
multithread_packet_build/shared_pool/24
                        time:   [3.9170 ms 3.9671 ms 4.0361 ms]
                        thrpt:  [5.9463 Melem/s 6.0498 Melem/s 6.1271 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe
multithread_packet_build/thread_local_pool/24
                        time:   [2.6088 ms 2.6627 ms 2.7136 ms]
                        thrpt:  [8.8443 Melem/s 9.0135 Melem/s 9.1997 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
multithread_packet_build/shared_pool/32
                        time:   [5.2057 ms 5.4986 ms 5.8300 ms]
                        thrpt:  [5.4888 Melem/s 5.8197 Melem/s 6.1471 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  2 (2.00%) high mild
  17 (17.00%) high severe
multithread_packet_build/thread_local_pool/32
                        time:   [3.2733 ms 3.2891 ms 3.3044 ms]
                        thrpt:  [9.6839 Melem/s 9.7290 Melem/s 9.7760 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) low mild

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/shared_mixed/8
                        time:   [1.1417 ms 1.1643 ms 1.1848 ms]
                        thrpt:  [10.128 Melem/s 10.307 Melem/s 10.511 Melem/s]
Benchmarking multithread_mixed_frames/fast_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.6s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/fast_mixed/8
                        time:   [1.0844 ms 1.1024 ms 1.1188 ms]
                        thrpt:  [10.726 Melem/s 10.885 Melem/s 11.067 Melem/s]
Benchmarking multithread_mixed_frames/shared_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.2s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/shared_mixed/16
                        time:   [1.6166 ms 1.6202 ms 1.6237 ms]
                        thrpt:  [14.781 Melem/s 14.813 Melem/s 14.846 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.3s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/fast_mixed/16
                        time:   [1.4486 ms 1.4519 ms 1.4554 ms]
                        thrpt:  [16.491 Melem/s 16.530 Melem/s 16.567 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
multithread_mixed_frames/shared_mixed/24
                        time:   [2.1824 ms 2.2157 ms 2.2581 ms]
                        thrpt:  [15.943 Melem/s 16.248 Melem/s 16.496 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  11 (11.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.2s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/fast_mixed/24
                        time:   [1.7522 ms 1.7751 ms 1.7973 ms]
                        thrpt:  [20.030 Melem/s 20.280 Melem/s 20.546 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
multithread_mixed_frames/shared_mixed/32
                        time:   [2.9661 ms 3.1200 ms 3.2948 ms]
                        thrpt:  [14.568 Melem/s 15.385 Melem/s 16.183 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  2 (2.00%) high mild
  18 (18.00%) high severe
multithread_mixed_frames/fast_mixed/32
                        time:   [2.3091 ms 2.3262 ms 2.3428 ms]
                        thrpt:  [20.488 Melem/s 20.635 Melem/s 20.787 Melem/s]

pool_contention/shared_acquire_release/8
                        time:   [9.7234 ms 9.7509 ms 9.7785 ms]
                        thrpt:  [8.1812 Melem/s 8.2043 Melem/s 8.2275 Melem/s]
Benchmarking pool_contention/fast_acquire_release/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.4s, enable flat sampling, or reduce sample count to 60.
pool_contention/fast_acquire_release/8
                        time:   [1.2564 ms 1.3059 ms 1.3502 ms]
                        thrpt:  [59.252 Melem/s 61.260 Melem/s 63.675 Melem/s]
pool_contention/shared_acquire_release/16
                        time:   [20.575 ms 20.640 ms 20.706 ms]
                        thrpt:  [7.7270 Melem/s 7.7520 Melem/s 7.7766 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking pool_contention/fast_acquire_release/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.0s, enable flat sampling, or reduce sample count to 40.
pool_contention/fast_acquire_release/16
                        time:   [1.9421 ms 1.9614 ms 1.9806 ms]
                        thrpt:  [80.784 Melem/s 81.576 Melem/s 82.383 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
pool_contention/shared_acquire_release/24
                        time:   [35.718 ms 36.210 ms 36.847 ms]
                        thrpt:  [6.5133 Melem/s 6.6280 Melem/s 6.7194 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
pool_contention/fast_acquire_release/24
                        time:   [2.2739 ms 2.2860 ms 2.2987 ms]
                        thrpt:  [104.41 Melem/s 104.99 Melem/s 105.55 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
pool_contention/shared_acquire_release/32
                        time:   [46.489 ms 47.697 ms 49.065 ms]
                        thrpt:  [6.5219 Melem/s 6.7090 Melem/s 6.8833 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  14 (14.00%) high severe
pool_contention/fast_acquire_release/32
                        time:   [2.9446 ms 2.9900 ms 3.0391 ms]
                        thrpt:  [105.29 Melem/s 107.02 Melem/s 108.67 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

throughput_scaling/fast_pool_scaling/1
                        time:   [3.6002 ms 3.6264 ms 3.6490 ms]
                        thrpt:  [548.09 Kelem/s 551.51 Kelem/s 555.52 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
throughput_scaling/fast_pool_scaling/2
                        time:   [3.6925 ms 3.7021 ms 3.7134 ms]
                        thrpt:  [1.0772 Melem/s 1.0805 Melem/s 1.0833 Melem/s]
throughput_scaling/fast_pool_scaling/4
                        time:   [3.7302 ms 3.7452 ms 3.7607 ms]
                        thrpt:  [2.1272 Melem/s 2.1361 Melem/s 2.1446 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
throughput_scaling/fast_pool_scaling/8
                        time:   [4.3312 ms 4.5978 ms 4.8564 ms]
                        thrpt:  [3.2946 Melem/s 3.4800 Melem/s 3.6941 Melem/s]
throughput_scaling/fast_pool_scaling/16
                        time:   [5.8025 ms 5.8326 ms 5.8577 ms]
                        thrpt:  [5.4629 Melem/s 5.4864 Melem/s 5.5149 Melem/s]
throughput_scaling/fast_pool_scaling/24
                        time:   [7.4799 ms 8.0141 ms 8.4630 ms]
                        thrpt:  [5.6718 Melem/s 5.9894 Melem/s 6.4172 Melem/s]
throughput_scaling/fast_pool_scaling/32
                        time:   [10.110 ms 10.194 ms 10.269 ms]
                        thrpt:  [6.2322 Melem/s 6.2781 Melem/s 6.3306 Melem/s]

routing_header/serialize
                        time:   [460.43 ps 501.72 ps 547.20 ps]
                        thrpt:  [1.8275 Gelem/s 1.9932 Gelem/s 2.1719 Gelem/s]
routing_header/deserialize
                        time:   [708.69 ps 714.80 ps 722.03 ps]
                        thrpt:  [1.3850 Gelem/s 1.3990 Gelem/s 1.4110 Gelem/s]
Found 27 outliers among 100 measurements (27.00%)
  8 (8.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  17 (17.00%) high severe
routing_header/roundtrip
                        time:   [719.51 ps 730.35 ps 742.25 ps]
                        thrpt:  [1.3473 Gelem/s 1.3692 Gelem/s 1.3898 Gelem/s]
routing_header/forward  time:   [199.75 ps 200.38 ps 200.91 ps]
                        thrpt:  [4.9773 Gelem/s 4.9904 Gelem/s 5.0062 Gelem/s]
Found 15 outliers among 100 measurements (15.00%)
  8 (8.00%) low severe
  5 (5.00%) high mild
  2 (2.00%) high severe

routing_table/lookup_hit
                        time:   [37.942 ns 38.033 ns 38.119 ns]
                        thrpt:  [26.233 Melem/s 26.293 Melem/s 26.356 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  3 (3.00%) high severe
routing_table/lookup_miss
                        time:   [17.527 ns 17.606 ns 17.676 ns]
                        thrpt:  [56.573 Melem/s 56.798 Melem/s 57.055 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  5 (5.00%) low severe
  10 (10.00%) high mild
  5 (5.00%) high severe
routing_table/is_local  time:   [200.91 ps 201.18 ps 201.42 ps]
                        thrpt:  [4.9647 Gelem/s 4.9707 Gelem/s 4.9774 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  10 (10.00%) high mild
  3 (3.00%) high severe
routing_table/add_route time:   [37.175 ns 37.283 ns 37.374 ns]
                        thrpt:  [26.757 Melem/s 26.822 Melem/s 26.900 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
routing_table/record_in time:   [54.296 ns 54.374 ns 54.447 ns]
                        thrpt:  [18.366 Melem/s 18.391 Melem/s 18.418 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
routing_table/record_out
                        time:   [34.194 ns 34.279 ns 34.358 ns]
                        thrpt:  [29.105 Melem/s 29.172 Melem/s 29.245 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low severe
  7 (7.00%) low mild
  2 (2.00%) high mild
routing_table/aggregate_stats
                        time:   [13.065 µs 13.097 µs 13.122 µs]
                        thrpt:  [76.208 Kelem/s 76.355 Kelem/s 76.541 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe

fair_scheduler/creation time:   [1.4929 µs 1.4984 µs 1.5027 µs]
                        thrpt:  [665.45 Kelem/s 667.40 Kelem/s 669.82 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) low severe
  5 (5.00%) high mild
  3 (3.00%) high severe
fair_scheduler/stream_count_empty
                        time:   [940.31 ns 945.83 ns 951.06 ns]
                        thrpt:  [1.0515 Melem/s 1.0573 Melem/s 1.0635 Melem/s]
Found 31 outliers among 100 measurements (31.00%)
  20 (20.00%) low severe
  1 (1.00%) low mild
  8 (8.00%) high mild
  2 (2.00%) high severe
fair_scheduler/total_queued
                        time:   [199.03 ps 199.73 ps 200.30 ps]
                        thrpt:  [4.9926 Gelem/s 5.0067 Gelem/s 5.0245 Gelem/s]
Found 20 outliers among 100 measurements (20.00%)
  17 (17.00%) low severe
  2 (2.00%) high mild
  1 (1.00%) high severe
fair_scheduler/cleanup_empty
                        time:   [1.2777 µs 1.2812 µs 1.2839 µs]
                        thrpt:  [778.85 Kelem/s 780.52 Kelem/s 782.65 Kelem/s]
Found 22 outliers among 100 measurements (22.00%)
  19 (19.00%) low severe
  1 (1.00%) high mild
  2 (2.00%) high severe

routing_table_concurrent/concurrent_lookup/4
                        time:   [175.05 µs 176.24 µs 177.54 µs]
                        thrpt:  [22.530 Melem/s 22.696 Melem/s 22.850 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
routing_table_concurrent/concurrent_stats/4
                        time:   [249.26 µs 250.99 µs 252.88 µs]
                        thrpt:  [15.818 Melem/s 15.937 Melem/s 16.048 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
routing_table_concurrent/concurrent_lookup/8
                        time:   [290.24 µs 294.70 µs 299.28 µs]
                        thrpt:  [26.731 Melem/s 27.146 Melem/s 27.563 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
routing_table_concurrent/concurrent_stats/8
                        time:   [381.49 µs 387.69 µs 393.35 µs]
                        thrpt:  [20.338 Melem/s 20.635 Melem/s 20.970 Melem/s]
routing_table_concurrent/concurrent_lookup/16
                        time:   [496.54 µs 502.15 µs 509.01 µs]
                        thrpt:  [31.433 Melem/s 31.863 Melem/s 32.223 Melem/s]
routing_table_concurrent/concurrent_stats/16
                        time:   [633.88 µs 644.28 µs 654.26 µs]
                        thrpt:  [24.455 Melem/s 24.834 Melem/s 25.241 Melem/s]

routing_decision/parse_lookup_forward
                        time:   [38.064 ns 38.233 ns 38.369 ns]
                        thrpt:  [26.063 Melem/s 26.156 Melem/s 26.272 Melem/s]
routing_decision/full_with_stats
                        time:   [125.28 ns 125.84 ns 126.31 ns]
                        thrpt:  [7.9168 Melem/s 7.9466 Melem/s 7.9824 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  17 (17.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

stream_multiplexing/lookup_all/10
                        time:   [338.86 ns 340.02 ns 340.98 ns]
                        thrpt:  [29.327 Melem/s 29.410 Melem/s 29.511 Melem/s]
Found 32 outliers among 100 measurements (32.00%)
  19 (19.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  7 (7.00%) high severe
stream_multiplexing/stats_all/10
                        time:   [538.25 ns 540.17 ns 541.75 ns]
                        thrpt:  [18.459 Melem/s 18.513 Melem/s 18.579 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  11 (11.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
stream_multiplexing/lookup_all/100
                        time:   [3.4149 µs 3.4271 µs 3.4371 µs]
                        thrpt:  [29.094 Melem/s 29.179 Melem/s 29.283 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  11 (11.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
stream_multiplexing/stats_all/100
                        time:   [5.3662 µs 5.3884 µs 5.4076 µs]
                        thrpt:  [18.493 Melem/s 18.558 Melem/s 18.635 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) low severe
  1 (1.00%) high mild
  4 (4.00%) high severe
stream_multiplexing/lookup_all/1000
                        time:   [35.290 µs 35.505 µs 35.708 µs]
                        thrpt:  [28.005 Melem/s 28.165 Melem/s 28.336 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  15 (15.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
stream_multiplexing/stats_all/1000
                        time:   [55.008 µs 55.223 µs 55.400 µs]
                        thrpt:  [18.050 Melem/s 18.108 Melem/s 18.179 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
stream_multiplexing/lookup_all/10000
                        time:   [386.10 µs 388.05 µs 389.73 µs]
                        thrpt:  [25.659 Melem/s 25.770 Melem/s 25.900 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low severe
  1 (1.00%) high severe
stream_multiplexing/stats_all/10000
                        time:   [571.54 µs 574.23 µs 576.53 µs]
                        thrpt:  [17.345 Melem/s 17.415 Melem/s 17.496 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  7 (7.00%) low severe
  1 (1.00%) high mild
  2 (2.00%) high severe

multihop_packet_builder/build/64
                        time:   [41.337 ns 41.409 ns 41.488 ns]
                        thrpt:  [1.4367 GiB/s 1.4394 GiB/s 1.4419 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build_priority/64
                        time:   [29.462 ns 29.632 ns 29.799 ns]
                        thrpt:  [2.0002 GiB/s 2.0115 GiB/s 2.0231 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) low mild
  1 (1.00%) high mild
multihop_packet_builder/build/256
                        time:   [42.675 ns 42.885 ns 43.080 ns]
                        thrpt:  [5.5344 GiB/s 5.5595 GiB/s 5.5869 GiB/s]
Found 30 outliers among 100 measurements (30.00%)
  14 (14.00%) low severe
  2 (2.00%) low mild
  13 (13.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build_priority/256
                        time:   [31.536 ns 31.658 ns 31.773 ns]
                        thrpt:  [7.5037 GiB/s 7.5310 GiB/s 7.5603 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) low mild
multihop_packet_builder/build/1024
                        time:   [44.583 ns 44.722 ns 44.843 ns]
                        thrpt:  [21.267 GiB/s 21.324 GiB/s 21.391 GiB/s]
Found 19 outliers among 100 measurements (19.00%)
  12 (12.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
multihop_packet_builder/build_priority/1024
                        time:   [35.204 ns 35.365 ns 35.548 ns]
                        thrpt:  [26.828 GiB/s 26.967 GiB/s 27.090 GiB/s]
Found 24 outliers among 100 measurements (24.00%)
  11 (11.00%) low severe
  5 (5.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
multihop_packet_builder/build/4096
                        time:   [70.662 ns 70.942 ns 71.244 ns]
                        thrpt:  [53.544 GiB/s 53.772 GiB/s 53.985 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build_priority/4096
                        time:   [57.150 ns 57.411 ns 57.684 ns]
                        thrpt:  [66.131 GiB/s 66.446 GiB/s 66.749 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild

multihop_chain/forward_chain/1
                        time:   [53.342 ns 53.449 ns 53.533 ns]
                        thrpt:  [18.680 Melem/s 18.710 Melem/s 18.747 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low severe
  2 (2.00%) high mild
  1 (1.00%) high severe
multihop_chain/forward_chain/2
                        time:   [87.599 ns 87.871 ns 88.202 ns]
                        thrpt:  [11.338 Melem/s 11.380 Melem/s 11.416 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_chain/forward_chain/3
                        time:   [121.96 ns 122.19 ns 122.44 ns]
                        thrpt:  [8.1671 Melem/s 8.1838 Melem/s 8.1993 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
multihop_chain/forward_chain/4
                        time:   [156.67 ns 157.26 ns 157.98 ns]
                        thrpt:  [6.3300 Melem/s 6.3587 Melem/s 6.3828 Melem/s]
multihop_chain/forward_chain/5
                        time:   [195.95 ns 196.35 ns 196.70 ns]
                        thrpt:  [5.0839 Melem/s 5.0930 Melem/s 5.1032 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild

hop_latency/single_hop_process
                        time:   [963.79 ps 968.90 ps 974.36 ps]
                        thrpt:  [1.0263 Gelem/s 1.0321 Gelem/s 1.0376 Gelem/s]
hop_latency/single_hop_full
                        time:   [33.106 ns 33.186 ns 33.263 ns]
                        thrpt:  [30.064 Melem/s 30.133 Melem/s 30.206 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild

hop_scaling/64B_1hops   time:   [51.812 ns 51.865 ns 51.923 ns]
                        thrpt:  [1.1479 GiB/s 1.1492 GiB/s 1.1504 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
hop_scaling/64B_2hops   time:   [84.175 ns 84.354 ns 84.556 ns]
                        thrpt:  [721.83 MiB/s 723.56 MiB/s 725.10 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
hop_scaling/64B_3hops   time:   [116.92 ns 117.08 ns 117.27 ns]
                        thrpt:  [520.45 MiB/s 521.29 MiB/s 522.05 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
hop_scaling/64B_4hops   time:   [148.49 ns 148.78 ns 149.10 ns]
                        thrpt:  [409.37 MiB/s 410.24 MiB/s 411.03 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
hop_scaling/64B_5hops   time:   [192.25 ns 192.47 ns 192.71 ns]
                        thrpt:  [316.72 MiB/s 317.11 MiB/s 317.47 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
hop_scaling/256B_1hops  time:   [53.550 ns 53.610 ns 53.671 ns]
                        thrpt:  [4.4422 GiB/s 4.4473 GiB/s 4.4522 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
hop_scaling/256B_2hops  time:   [89.858 ns 89.995 ns 90.138 ns]
                        thrpt:  [2.6451 GiB/s 2.6493 GiB/s 2.6533 GiB/s]
hop_scaling/256B_3hops  time:   [123.99 ns 124.22 ns 124.49 ns]
                        thrpt:  [1.9152 GiB/s 1.9193 GiB/s 1.9230 GiB/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  12 (12.00%) high mild
  1 (1.00%) high severe
hop_scaling/256B_4hops  time:   [157.67 ns 157.99 ns 158.36 ns]
                        thrpt:  [1.5056 GiB/s 1.5090 GiB/s 1.5121 GiB/s]
Found 20 outliers among 100 measurements (20.00%)
  15 (15.00%) high mild
  5 (5.00%) high severe
hop_scaling/256B_5hops  time:   [191.01 ns 191.79 ns 192.68 ns]
                        thrpt:  [1.2374 GiB/s 1.2432 GiB/s 1.2482 GiB/s]
Found 15 outliers among 100 measurements (15.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  7 (7.00%) high severe
hop_scaling/1024B_1hops time:   [54.967 ns 55.029 ns 55.095 ns]
                        thrpt:  [17.310 GiB/s 17.330 GiB/s 17.350 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low severe
  6 (6.00%) high mild
  1 (1.00%) high severe
hop_scaling/1024B_2hops time:   [90.321 ns 90.485 ns 90.645 ns]
                        thrpt:  [10.521 GiB/s 10.540 GiB/s 10.559 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low severe
  2 (2.00%) high mild
hop_scaling/1024B_3hops time:   [125.91 ns 126.12 ns 126.35 ns]
                        thrpt:  [7.5481 GiB/s 7.5619 GiB/s 7.5743 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
hop_scaling/1024B_4hops time:   [160.28 ns 160.66 ns 161.02 ns]
                        thrpt:  [5.9228 GiB/s 5.9361 GiB/s 5.9499 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low severe
hop_scaling/1024B_5hops time:   [199.88 ns 200.30 ns 200.65 ns]
                        thrpt:  [4.7528 GiB/s 4.7612 GiB/s 4.7711 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild

multihop_with_routing/route_and_forward/1
                        time:   [181.08 ns 181.23 ns 181.40 ns]
                        thrpt:  [5.5127 Melem/s 5.5177 Melem/s 5.5225 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
multihop_with_routing/route_and_forward/2
                        time:   [340.79 ns 342.19 ns 343.56 ns]
                        thrpt:  [2.9107 Melem/s 2.9223 Melem/s 2.9344 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  5 (5.00%) high severe
multihop_with_routing/route_and_forward/3
                        time:   [500.92 ns 501.89 ns 502.69 ns]
                        thrpt:  [1.9893 Melem/s 1.9925 Melem/s 1.9963 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
multihop_with_routing/route_and_forward/4
                        time:   [662.62 ns 665.12 ns 667.17 ns]
                        thrpt:  [1.4989 Melem/s 1.5035 Melem/s 1.5092 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low severe
  2 (2.00%) high mild
  4 (4.00%) high severe
multihop_with_routing/route_and_forward/5
                        time:   [828.40 ns 829.82 ns 831.99 ns]
                        thrpt:  [1.2019 Melem/s 1.2051 Melem/s 1.2071 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

multihop_concurrent/concurrent_forward/4
                        time:   [602.35 µs 609.48 µs 616.06 µs]
                        thrpt:  [6.4928 Melem/s 6.5630 Melem/s 6.6407 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
multihop_concurrent/concurrent_forward/8
                        time:   [1.2507 ms 1.2532 ms 1.2561 ms]
                        thrpt:  [6.3687 Melem/s 6.3837 Melem/s 6.3965 Melem/s]
multihop_concurrent/concurrent_forward/16
                        time:   [1.6023 ms 1.6228 ms 1.6381 ms]
                        thrpt:  [9.7672 Melem/s 9.8597 Melem/s 9.9855 Melem/s]

pingwave/serialize      time:   [507.00 ps 516.54 ps 527.88 ps]
                        thrpt:  [1.8944 Gelem/s 1.9359 Gelem/s 1.9724 Gelem/s]
Found 32 outliers among 100 measurements (32.00%)
  4 (4.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  20 (20.00%) high severe
pingwave/deserialize    time:   [658.41 ps 686.02 ps 712.10 ps]
                        thrpt:  [1.4043 Gelem/s 1.4577 Gelem/s 1.5188 Gelem/s]
pingwave/roundtrip      time:   [658.56 ps 686.57 ps 712.94 ps]
                        thrpt:  [1.4026 Gelem/s 1.4565 Gelem/s 1.5185 Gelem/s]
pingwave/forward        time:   [510.81 ps 519.69 ps 530.26 ps]
                        thrpt:  [1.8859 Gelem/s 1.9242 Gelem/s 1.9577 Gelem/s]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe

capabilities/serialize_simple
                        time:   [37.341 ns 37.441 ns 37.535 ns]
                        thrpt:  [26.642 Melem/s 26.708 Melem/s 26.780 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
capabilities/deserialize_simple
                        time:   [12.791 ns 12.875 ns 12.960 ns]
                        thrpt:  [77.160 Melem/s 77.667 Melem/s 78.181 Melem/s]
capabilities/serialize_complex
                        time:   [38.241 ns 38.313 ns 38.399 ns]
                        thrpt:  [26.042 Melem/s 26.101 Melem/s 26.150 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
capabilities/deserialize_complex
                        time:   [245.14 ns 245.64 ns 246.12 ns]
                        thrpt:  [4.0631 Melem/s 4.0709 Melem/s 4.0793 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild

local_graph/create_pingwave
                        time:   [5.0214 ns 5.0248 ns 5.0296 ns]
                        thrpt:  [198.82 Melem/s 199.01 Melem/s 199.15 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe
local_graph/on_pingwave_new
                        time:   [46.806 ns 47.270 ns 47.743 ns]
                        thrpt:  [20.945 Melem/s 21.155 Melem/s 21.365 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  10 (10.00%) low mild
  4 (4.00%) high severe
local_graph/on_pingwave_duplicate
                        time:   [972.92 ns 973.92 ns 974.94 ns]
                        thrpt:  [1.0257 Melem/s 1.0268 Melem/s 1.0278 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
local_graph/get_node    time:   [14.191 ns 14.222 ns 14.258 ns]
                        thrpt:  [70.136 Melem/s 70.313 Melem/s 70.467 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  13 (13.00%) high mild
  4 (4.00%) high severe
local_graph/node_count  time:   [957.75 ns 958.46 ns 959.28 ns]
                        thrpt:  [1.0424 Melem/s 1.0433 Melem/s 1.0441 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
local_graph/stats       time:   [2.8815 µs 2.8854 µs 2.8898 µs]
                        thrpt:  [346.04 Kelem/s 346.57 Kelem/s 347.04 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe

graph_scaling/all_nodes/100
                        time:   [7.4974 µs 7.5077 µs 7.5181 µs]
                        thrpt:  [13.301 Melem/s 13.320 Melem/s 13.338 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  8 (8.00%) low mild
  8 (8.00%) high mild
  3 (3.00%) high severe
graph_scaling/nodes_within_hops/100
                        time:   [7.5625 µs 7.5808 µs 7.6009 µs]
                        thrpt:  [13.156 Melem/s 13.191 Melem/s 13.223 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
graph_scaling/all_nodes/500
                        time:   [16.295 µs 16.332 µs 16.369 µs]
                        thrpt:  [30.546 Melem/s 30.615 Melem/s 30.684 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
graph_scaling/nodes_within_hops/500
                        time:   [16.109 µs 16.161 µs 16.217 µs]
                        thrpt:  [30.833 Melem/s 30.939 Melem/s 31.038 Melem/s]
graph_scaling/all_nodes/1000
                        time:   [26.895 µs 26.975 µs 27.062 µs]
                        thrpt:  [36.952 Melem/s 37.071 Melem/s 37.182 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
graph_scaling/nodes_within_hops/1000
                        time:   [26.523 µs 26.647 µs 26.783 µs]
                        thrpt:  [37.337 Melem/s 37.528 Melem/s 37.703 Melem/s]
graph_scaling/all_nodes/5000
                        time:   [227.30 µs 228.05 µs 228.89 µs]
                        thrpt:  [21.844 Melem/s 21.925 Melem/s 21.998 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
graph_scaling/nodes_within_hops/5000
                        time:   [224.91 µs 225.53 µs 226.19 µs]
                        thrpt:  [22.105 Melem/s 22.170 Melem/s 22.231 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

capability_search/find_with_gpu
                        time:   [29.344 µs 29.380 µs 29.418 µs]
                        thrpt:  [33.993 Kelem/s 34.036 Kelem/s 34.079 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
capability_search/find_by_tool_python
                        time:   [61.303 µs 61.362 µs 61.433 µs]
                        thrpt:  [16.278 Kelem/s 16.297 Kelem/s 16.312 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
capability_search/find_by_tool_rust
                        time:   [79.410 µs 79.523 µs 79.655 µs]
                        thrpt:  [12.554 Kelem/s 12.575 Kelem/s 12.593 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

graph_concurrent/concurrent_pingwave/4
                        time:   [169.79 µs 171.92 µs 173.48 µs]
                        thrpt:  [11.529 Melem/s 11.634 Melem/s 11.779 Melem/s]
graph_concurrent/concurrent_pingwave/8
                        time:   [268.77 µs 280.22 µs 290.19 µs]
                        thrpt:  [13.784 Melem/s 14.274 Melem/s 14.883 Melem/s]
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) high mild
  1 (5.00%) high severe
graph_concurrent/concurrent_pingwave/16
                        time:   [506.43 µs 520.02 µs 534.36 µs]
                        thrpt:  [14.971 Melem/s 15.384 Melem/s 15.797 Melem/s]

path_finding/path_1_hop time:   [5.6074 µs 5.6182 µs 5.6278 µs]
                        thrpt:  [177.69 Kelem/s 177.99 Kelem/s 178.34 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  10 (10.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
path_finding/path_2_hops
                        time:   [5.9091 µs 5.9432 µs 5.9803 µs]
                        thrpt:  [167.22 Kelem/s 168.26 Kelem/s 169.23 Kelem/s]
Found 19 outliers among 100 measurements (19.00%)
  19 (19.00%) high severe
path_finding/path_4_hops
                        time:   [6.7612 µs 6.7676 µs 6.7745 µs]
                        thrpt:  [147.61 Kelem/s 147.76 Kelem/s 147.90 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) low severe
  4 (4.00%) high mild
path_finding/path_not_found
                        time:   [5.8407 µs 5.8462 µs 5.8520 µs]
                        thrpt:  [170.88 Kelem/s 171.05 Kelem/s 171.21 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
path_finding/path_complex_graph
                        time:   [178.34 µs 178.50 µs 178.71 µs]
                        thrpt:  [5.5957 Kelem/s 5.6021 Kelem/s 5.6074 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

failure_detector/heartbeat_existing
                        time:   [35.652 ns 35.676 ns 35.703 ns]
                        thrpt:  [28.009 Melem/s 28.030 Melem/s 28.049 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  11 (11.00%) high mild
  6 (6.00%) high severe
failure_detector/heartbeat_new
                        time:   [198.39 ns 200.57 ns 202.93 ns]
                        thrpt:  [4.9278 Melem/s 4.9859 Melem/s 5.0406 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  8 (8.00%) low mild
  2 (2.00%) high mild
failure_detector/status_check
                        time:   [13.452 ns 13.463 ns 13.475 ns]
                        thrpt:  [74.212 Melem/s 74.280 Melem/s 74.340 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking failure_detector/check_all: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 68.9s, or reduce sample count to 10.
failure_detector/check_all
                        time:   [668.86 ms 670.50 ms 672.15 ms]
                        thrpt:  [1.4878  elem/s 1.4914  elem/s 1.4951  elem/s]
Benchmarking failure_detector/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 19.9s, or reduce sample count to 20.
failure_detector/stats  time:   [198.03 ms 198.25 ms 198.47 ms]
                        thrpt:  [5.0385  elem/s 5.0441  elem/s 5.0498  elem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  8 (8.00%) high mild
  1 (1.00%) high severe

loss_simulator/should_drop_1pct
                        time:   [11.061 ns 11.072 ns 11.083 ns]
                        thrpt:  [90.225 Melem/s 90.321 Melem/s 90.404 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe
loss_simulator/should_drop_5pct
                        time:   [11.468 ns 11.477 ns 11.487 ns]
                        thrpt:  [87.054 Melem/s 87.131 Melem/s 87.196 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  11 (11.00%) high severe
loss_simulator/should_drop_10pct
                        time:   [11.980 ns 11.989 ns 11.999 ns]
                        thrpt:  [83.338 Melem/s 83.409 Melem/s 83.474 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
loss_simulator/should_drop_20pct
                        time:   [13.009 ns 13.016 ns 13.023 ns]
                        thrpt:  [76.786 Melem/s 76.831 Melem/s 76.867 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
loss_simulator/should_drop_burst
                        time:   [11.525 ns 11.557 ns 11.581 ns]
                        thrpt:  [86.347 Melem/s 86.529 Melem/s 86.766 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  4 (4.00%) low severe
  4 (4.00%) high mild
  7 (7.00%) high severe

circuit_breaker/allow_closed
                        time:   [11.064 ns 11.070 ns 11.077 ns]
                        thrpt:  [90.280 Melem/s 90.338 Melem/s 90.386 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
circuit_breaker/record_success
                        time:   [9.0819 ns 9.0912 ns 9.1017 ns]
                        thrpt:  [109.87 Melem/s 110.00 Melem/s 110.11 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
circuit_breaker/record_failure
                        time:   [7.6909 ns 7.6969 ns 7.7036 ns]
                        thrpt:  [129.81 Melem/s 129.92 Melem/s 130.02 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  8 (8.00%) high mild
  8 (8.00%) high severe
circuit_breaker/state   time:   [11.044 ns 11.050 ns 11.059 ns]
                        thrpt:  [90.427 Melem/s 90.496 Melem/s 90.547 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe

recovery_manager/on_failure_with_alternates
                        time:   [211.01 ns 213.41 ns 215.96 ns]
                        thrpt:  [4.6304 Melem/s 4.6859 Melem/s 4.7392 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  11 (11.00%) low mild
  2 (2.00%) high mild
recovery_manager/on_failure_no_alternates
                        time:   [180.75 ns 189.53 ns 203.33 ns]
                        thrpt:  [4.9180 Melem/s 5.2763 Melem/s 5.5325 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
recovery_manager/get_action
                        time:   [38.706 ns 38.822 ns 38.946 ns]
                        thrpt:  [25.677 Melem/s 25.758 Melem/s 25.836 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  17 (17.00%) high mild
recovery_manager/is_failed
                        time:   [12.837 ns 12.842 ns 12.848 ns]
                        thrpt:  [77.835 Melem/s 77.872 Melem/s 77.902 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
recovery_manager/on_recovery
                        time:   [120.89 ns 121.16 ns 121.52 ns]
                        thrpt:  [8.2291 Melem/s 8.2532 Melem/s 8.2718 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
recovery_manager/stats  time:   [1.2057 ns 1.2069 ns 1.2082 ns]
                        thrpt:  [827.70 Melem/s 828.60 Melem/s 829.40 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe

failure_scaling/check_all/100
                        time:   [8.8478 µs 8.8615 µs 8.8739 µs]
                        thrpt:  [11.269 Melem/s 11.285 Melem/s 11.302 Melem/s]
failure_scaling/healthy_nodes/100
                        time:   [6.2857 µs 6.2927 µs 6.3006 µs]
                        thrpt:  [15.872 Melem/s 15.891 Melem/s 15.909 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
failure_scaling/check_all/500
                        time:   [23.107 µs 23.144 µs 23.185 µs]
                        thrpt:  [21.566 Melem/s 21.604 Melem/s 21.638 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
failure_scaling/healthy_nodes/500
                        time:   [10.826 µs 10.838 µs 10.851 µs]
                        thrpt:  [46.077 Melem/s 46.133 Melem/s 46.186 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
failure_scaling/check_all/1000
                        time:   [40.988 µs 41.055 µs 41.117 µs]
                        thrpt:  [24.321 Melem/s 24.358 Melem/s 24.397 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  17 (17.00%) low severe
  1 (1.00%) high mild
failure_scaling/healthy_nodes/1000
                        time:   [14.854 µs 14.875 µs 14.893 µs]
                        thrpt:  [67.146 Melem/s 67.228 Melem/s 67.320 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  10 (10.00%) high mild
  2 (2.00%) high severe
failure_scaling/check_all/5000
                        time:   [181.28 µs 182.26 µs 183.18 µs]
                        thrpt:  [27.295 Melem/s 27.433 Melem/s 27.581 Melem/s]
failure_scaling/healthy_nodes/5000
                        time:   [52.578 µs 52.850 µs 53.084 µs]
                        thrpt:  [94.191 Melem/s 94.608 Melem/s 95.097 Melem/s]

failure_concurrent/concurrent_heartbeat/4
                        time:   [201.43 µs 204.79 µs 207.36 µs]
                        thrpt:  [9.6450 Melem/s 9.7660 Melem/s 9.9288 Melem/s]
failure_concurrent/concurrent_heartbeat/8
                        time:   [312.28 µs 321.78 µs 331.11 µs]
                        thrpt:  [12.080 Melem/s 12.431 Melem/s 12.809 Melem/s]
failure_concurrent/concurrent_heartbeat/16
                        time:   [579.13 µs 600.45 µs 624.58 µs]
                        thrpt:  [12.809 Melem/s 13.323 Melem/s 13.814 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

failure_recovery_cycle/full_cycle
                        time:   [246.03 ns 248.73 ns 251.56 ns]
                        thrpt:  [3.9751 Melem/s 4.0205 Melem/s 4.0645 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  17 (17.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

capability_set/create   time:   [15.783 µs 15.804 µs 15.823 µs]
                        thrpt:  [63.197 Kelem/s 63.276 Kelem/s 63.358 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_set/serialize
                        time:   [65.084 µs 65.253 µs 65.403 µs]
                        thrpt:  [15.290 Kelem/s 15.325 Kelem/s 15.365 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  6 (6.00%) low severe
  3 (3.00%) low mild
  7 (7.00%) high mild
  2 (2.00%) high severe
capability_set/deserialize
                        time:   [8.2622 µs 8.2718 µs 8.2824 µs]
                        thrpt:  [120.74 Kelem/s 120.89 Kelem/s 121.03 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  3 (3.00%) high severe
capability_set/roundtrip
                        time:   [73.633 µs 73.754 µs 73.875 µs]
                        thrpt:  [13.536 Kelem/s 13.559 Kelem/s 13.581 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_set/serialize_compact
                        time:   [2.0065 µs 2.0086 µs 2.0106 µs]
                        thrpt:  [497.37 Kelem/s 497.85 Kelem/s 498.39 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  5 (5.00%) high severe
capability_set/deserialize_compact
                        time:   [5.5406 µs 5.5644 µs 5.5845 µs]
                        thrpt:  [179.07 Kelem/s 179.71 Kelem/s 180.49 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  10 (10.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_set/roundtrip_compact
                        time:   [7.6231 µs 7.6382 µs 7.6508 µs]
                        thrpt:  [130.71 Kelem/s 130.92 Kelem/s 131.18 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
capability_set/has_tag  time:   [50.657 ns 50.800 ns 50.934 ns]
                        thrpt:  [19.633 Melem/s 19.685 Melem/s 19.740 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
capability_set/has_model
                        time:   [25.907 ns 25.962 ns 26.005 ns]
                        thrpt:  [38.454 Melem/s 38.519 Melem/s 38.600 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
capability_set/has_tool time:   [21.886 ns 21.986 ns 22.084 ns]
                        thrpt:  [45.281 Melem/s 45.483 Melem/s 45.691 Melem/s]
capability_set/has_gpu  time:   [40.427 ns 40.512 ns 40.598 ns]
                        thrpt:  [24.632 Melem/s 24.684 Melem/s 24.736 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild

capability_announcement/create
                        time:   [2.7245 µs 2.7371 µs 2.7487 µs]
                        thrpt:  [363.81 Kelem/s 365.35 Kelem/s 367.04 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
capability_announcement/serialize
                        time:   [71.480 µs 71.705 µs 71.892 µs]
                        thrpt:  [13.910 Kelem/s 13.946 Kelem/s 13.990 Kelem/s]
Found 19 outliers among 100 measurements (19.00%)
  7 (7.00%) low severe
  2 (2.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
capability_announcement/deserialize
                        time:   [9.5354 µs 9.5633 µs 9.5879 µs]
                        thrpt:  [104.30 Kelem/s 104.57 Kelem/s 104.87 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  4 (4.00%) high mild
  3 (3.00%) high severe
capability_announcement/is_expired
                        time:   [21.607 ns 21.681 ns 21.743 ns]
                        thrpt:  [45.991 Melem/s 46.123 Melem/s 46.280 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  8 (8.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild

capability_filter/match_single_tag
                        time:   [54.004 ns 54.230 ns 54.468 ns]
                        thrpt:  [18.360 Melem/s 18.440 Melem/s 18.517 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
capability_filter/match_require_gpu
                        time:   [43.649 ns 43.795 ns 43.922 ns]
                        thrpt:  [22.768 Melem/s 22.833 Melem/s 22.910 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
capability_filter/match_gpu_vendor
                        time:   [129.23 ns 129.52 ns 129.79 ns]
                        thrpt:  [7.7050 Melem/s 7.7210 Melem/s 7.7382 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
capability_filter/match_min_memory
                        time:   [27.421 ns 27.516 ns 27.609 ns]
                        thrpt:  [36.220 Melem/s 36.342 Melem/s 36.469 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high severe
capability_filter/match_complex
                        time:   [3.9752 µs 3.9807 µs 3.9860 µs]
                        thrpt:  [250.88 Kelem/s 251.21 Kelem/s 251.56 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low severe
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_filter/match_no_match
                        time:   [76.661 ns 76.837 ns 77.011 ns]
                        thrpt:  [12.985 Melem/s 13.015 Melem/s 13.044 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe

capability_fold_insert/index_nodes/100
                        time:   [3.2238 ms 3.2293 ms 3.2345 ms]
                        thrpt:  [30.917 Kelem/s 30.967 Kelem/s 31.020 Kelem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low severe
  4 (4.00%) low mild
  6 (6.00%) high mild
capability_fold_insert/index_nodes/1000
                        time:   [30.851 ms 30.916 ms 30.974 ms]
                        thrpt:  [32.285 Kelem/s 32.346 Kelem/s 32.414 Kelem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 31.1s, or reduce sample count to 10.
capability_fold_insert/index_nodes/10000
                        time:   [309.68 ms 310.25 ms 310.80 ms]
                        thrpt:  [32.175 Kelem/s 32.232 Kelem/s 32.291 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
  7 (7.00%) high mild
  1 (1.00%) high severe

capability_fold_query/query_single_tag
                        time:   [150.65 µs 150.90 µs 151.16 µs]
                        thrpt:  [6.6155 Kelem/s 6.6269 Kelem/s 6.6379 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_require_gpu
                        time:   [301.46 µs 302.69 µs 303.80 µs]
                        thrpt:  [3.2916 Kelem/s 3.3037 Kelem/s 3.3172 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  7 (7.00%) low severe
  6 (6.00%) low mild
  7 (7.00%) high mild
capability_fold_query/query_gpu_vendor
                        time:   [491.32 µs 493.06 µs 494.80 µs]
                        thrpt:  [2.0210 Kelem/s 2.0281 Kelem/s 2.0354 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  7 (7.00%) low severe
  3 (3.00%) low mild
  6 (6.00%) high mild
  4 (4.00%) high severe
capability_fold_query/query_min_memory
                        time:   [369.87 µs 370.87 µs 371.75 µs]
                        thrpt:  [2.6900 Kelem/s 2.6964 Kelem/s 2.7037 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  7 (7.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_complex
                        time:   [297.44 µs 298.18 µs 298.79 µs]
                        thrpt:  [3.3468 Kelem/s 3.3537 Kelem/s 3.3620 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  4 (4.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  7 (7.00%) high severe
capability_fold_query/query_model
                        time:   [72.824 µs 73.028 µs 73.207 µs]
                        thrpt:  [13.660 Kelem/s 13.693 Kelem/s 13.732 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
capability_fold_query/query_tool
                        time:   [303.43 µs 304.55 µs 305.43 µs]
                        thrpt:  [3.2740 Kelem/s 3.2836 Kelem/s 3.2956 Kelem/s]
Found 22 outliers among 100 measurements (22.00%)
  12 (12.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_no_results
                        time:   [115.89 ns 116.21 ns 116.48 ns]
                        thrpt:  [8.5849 Melem/s 8.6054 Melem/s 8.6287 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  5 (5.00%) low severe
  11 (11.00%) low mild
  2 (2.00%) high severe

capability_fold_find_best/find_best_simple
                        time:   [302.62 µs 303.03 µs 303.42 µs]
                        thrpt:  [3.2957 Kelem/s 3.3000 Kelem/s 3.3045 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  4 (4.00%) low severe
  6 (6.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
capability_fold_find_best/find_best_with_prefs
                        time:   [285.29 µs 286.61 µs 287.72 µs]
                        thrpt:  [3.4756 Kelem/s 3.4890 Kelem/s 3.5052 Kelem/s]
Found 34 outliers among 100 measurements (34.00%)
  16 (16.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  11 (11.00%) high severe

capability_fold_scaling/query_tag/1000
                        time:   [14.250 µs 14.278 µs 14.308 µs]
                        thrpt:  [69.889 Kelem/s 70.040 Kelem/s 70.174 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_scaling/query_complex/1000
                        time:   [29.194 µs 29.266 µs 29.327 µs]
                        thrpt:  [34.099 Kelem/s 34.170 Kelem/s 34.254 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  5 (5.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_tag/5000
                        time:   [71.904 µs 72.204 µs 72.473 µs]
                        thrpt:  [13.798 Kelem/s 13.850 Kelem/s 13.907 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_fold_scaling/query_complex/5000
                        time:   [145.39 µs 145.91 µs 146.35 µs]
                        thrpt:  [6.8328 Kelem/s 6.8533 Kelem/s 6.8782 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  5 (5.00%) low severe
  9 (9.00%) high mild
  4 (4.00%) high severe
capability_fold_scaling/query_tag/10000
                        time:   [149.35 µs 149.65 µs 149.92 µs]
                        thrpt:  [6.6704 Kelem/s 6.6823 Kelem/s 6.6958 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  6 (6.00%) low severe
  2 (2.00%) low mild
  8 (8.00%) high mild
  2 (2.00%) high severe
capability_fold_scaling/query_complex/10000
                        time:   [299.66 µs 300.86 µs 301.93 µs]
                        thrpt:  [3.3120 Kelem/s 3.3238 Kelem/s 3.3371 Kelem/s]
Found 26 outliers among 100 measurements (26.00%)
  8 (8.00%) low severe
  6 (6.00%) low mild
  4 (4.00%) high mild
  8 (8.00%) high severe
capability_fold_scaling/query_tag/50000
                        time:   [938.20 µs 940.79 µs 944.21 µs]
                        thrpt:  [1.0591 Kelem/s 1.0629 Kelem/s 1.0659 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_complex/50000
                        time:   [2.0319 ms 2.0434 ms 2.0564 ms]
                        thrpt:  [486.29  elem/s 489.37  elem/s 492.14  elem/s]
Found 20 outliers among 100 measurements (20.00%)
  9 (9.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe

capability_fold_concurrent/concurrent_index/4
                        time:   [17.309 ms 17.373 ms 17.454 ms]
                        thrpt:  [114.58 Kelem/s 115.12 Kelem/s 115.55 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high mild
capability_fold_concurrent/concurrent_query/4
                        time:   [146.90 ms 147.32 ms 147.76 ms]
                        thrpt:  [13.536 Kelem/s 13.576 Kelem/s 13.614 Kelem/s]
capability_fold_concurrent/concurrent_mixed/4
                        time:   [82.140 ms 83.198 ms 84.716 ms]
                        thrpt:  [23.608 Kelem/s 24.039 Kelem/s 24.349 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_index/8
                        time:   [20.144 ms 20.532 ms 20.922 ms]
                        thrpt:  [191.18 Kelem/s 194.82 Kelem/s 198.57 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
capability_fold_concurrent/concurrent_query/8
                        time:   [170.51 ms 176.82 ms 182.86 ms]
                        thrpt:  [21.874 Kelem/s 22.622 Kelem/s 23.459 Kelem/s]
capability_fold_concurrent/concurrent_mixed/8
                        time:   [101.55 ms 105.19 ms 108.80 ms]
                        thrpt:  [36.766 Kelem/s 38.026 Kelem/s 39.390 Kelem/s]
Benchmarking capability_fold_concurrent/concurrent_index/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.5s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/16
                        time:   [24.888 ms 25.080 ms 25.233 ms]
                        thrpt:  [317.05 Kelem/s 318.98 Kelem/s 321.44 Kelem/s]
capability_fold_concurrent/concurrent_query/16
                        time:   [239.68 ms 242.53 ms 245.43 ms]
                        thrpt:  [32.596 Kelem/s 32.985 Kelem/s 33.377 Kelem/s]
capability_fold_concurrent/concurrent_mixed/16
                        time:   [191.47 ms 194.63 ms 198.23 ms]
                        thrpt:  [40.357 Kelem/s 41.103 Kelem/s 41.781 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

capability_fold_updates/update_higher_version
                        time:   [21.574 µs 21.603 µs 21.629 µs]
                        thrpt:  [46.235 Kelem/s 46.290 Kelem/s 46.352 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  9 (9.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
capability_fold_updates/update_same_version
                        time:   [35.505 µs 35.566 µs 35.620 µs]
                        thrpt:  [28.074 Kelem/s 28.117 Kelem/s 28.165 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
capability_fold_updates/remove_and_readd
                        time:   [47.043 µs 47.201 µs 47.336 µs]
                        thrpt:  [21.126 Kelem/s 21.186 Kelem/s 21.257 Kelem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe

location_info/create    time:   [53.770 ns 54.154 ns 54.494 ns]
                        thrpt:  [18.351 Melem/s 18.466 Melem/s 18.598 Melem/s]
location_info/distance_to
                        time:   [2.6409 ns 2.6552 ns 2.6685 ns]
                        thrpt:  [374.74 Melem/s 376.61 Melem/s 378.66 Melem/s]
Found 26 outliers among 100 measurements (26.00%)
  12 (12.00%) low severe
  3 (3.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
location_info/same_continent
                        time:   [2.8323 ns 2.8593 ns 2.8854 ns]
                        thrpt:  [346.57 Melem/s 349.74 Melem/s 353.07 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) low mild
  8 (8.00%) high mild
  1 (1.00%) high severe
location_info/same_continent_cross
                        time:   [200.00 ps 200.44 ps 200.78 ps]
                        thrpt:  [4.9805 Gelem/s 4.9891 Gelem/s 5.0001 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
location_info/same_region
                        time:   [1.9846 ns 1.9933 ns 2.0007 ns]
                        thrpt:  [499.83 Melem/s 501.69 Melem/s 503.88 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  11 (11.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild

topology_hints/create   time:   [3.5397 ns 3.5637 ns 3.5856 ns]
                        thrpt:  [278.89 Melem/s 280.61 Melem/s 282.51 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  21 (21.00%) low mild
topology_hints/connectivity_score
                        time:   [199.92 ps 200.45 ps 200.88 ps]
                        thrpt:  [4.9780 Gelem/s 4.9887 Gelem/s 5.0021 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  6 (6.00%) high severe
topology_hints/average_latency_empty
                        time:   [257.67 ps 260.67 ps 263.75 ps]
                        thrpt:  [3.7915 Gelem/s 3.8362 Gelem/s 3.8810 Gelem/s]
topology_hints/average_latency_100
                        time:   [47.260 ns 47.370 ns 47.461 ns]
                        thrpt:  [21.070 Melem/s 21.110 Melem/s 21.160 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  5 (5.00%) low severe
  5 (5.00%) high mild
  11 (11.00%) high severe

nat_type/difficulty     time:   [197.02 ps 198.01 ps 198.90 ps]
                        thrpt:  [5.0278 Gelem/s 5.0504 Gelem/s 5.0755 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  14 (14.00%) low mild
nat_type/can_connect_direct
                        time:   [197.65 ps 198.78 ps 199.88 ps]
                        thrpt:  [5.0031 Gelem/s 5.0307 Gelem/s 5.0596 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) low severe
  5 (5.00%) high mild
  1 (1.00%) high severe
nat_type/can_connect_symmetric
                        time:   [198.36 ps 199.21 ps 199.89 ps]
                        thrpt:  [5.0028 Gelem/s 5.0198 Gelem/s 5.0413 Gelem/s]

node_metadata/create_simple
                        time:   [31.348 ns 31.409 ns 31.473 ns]
                        thrpt:  [31.773 Melem/s 31.838 Melem/s 31.900 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
node_metadata/create_full
                        time:   [432.30 ns 433.69 ns 434.89 ns]
                        thrpt:  [2.2994 Melem/s 2.3058 Melem/s 2.3132 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high severe
node_metadata/routing_score
                        time:   [198.80 ps 199.71 ps 200.50 ps]
                        thrpt:  [4.9875 Gelem/s 5.0072 Gelem/s 5.0303 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) low severe
  4 (4.00%) high mild
  1 (1.00%) high severe
node_metadata/age       time:   [25.012 ns 25.034 ns 25.058 ns]
                        thrpt:  [39.907 Melem/s 39.945 Melem/s 39.981 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
node_metadata/is_stale  time:   [24.310 ns 24.369 ns 24.416 ns]
                        thrpt:  [40.956 Melem/s 41.035 Melem/s 41.135 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
node_metadata/serialize time:   [641.80 ns 644.90 ns 647.61 ns]
                        thrpt:  [1.5441 Melem/s 1.5506 Melem/s 1.5581 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) low severe
  2 (2.00%) high mild
  3 (3.00%) high severe
node_metadata/deserialize
                        time:   [1.6411 µs 1.6503 µs 1.6590 µs]
                        thrpt:  [602.76 Kelem/s 605.95 Kelem/s 609.35 Kelem/s]

metadata_query/match_status
                        time:   [2.3785 ns 2.3922 ns 2.4048 ns]
                        thrpt:  [415.83 Melem/s 418.03 Melem/s 420.44 Melem/s]
Found 30 outliers among 100 measurements (30.00%)
  17 (17.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe
metadata_query/match_min_tier
                        time:   [2.3453 ns 2.3593 ns 2.3742 ns]
                        thrpt:  [421.20 Melem/s 423.86 Melem/s 426.38 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  16 (16.00%) low mild
  2 (2.00%) high severe
metadata_query/match_continent
                        time:   [5.0468 ns 5.1074 ns 5.1724 ns]
                        thrpt:  [193.33 Melem/s 195.79 Melem/s 198.15 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
metadata_query/match_complex
                        time:   [5.2742 ns 5.3615 ns 5.4481 ns]
                        thrpt:  [183.55 Melem/s 186.51 Melem/s 189.60 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
metadata_query/match_no_match
                        time:   [1.4694 ns 1.4931 ns 1.5180 ns]
                        thrpt:  [658.76 Melem/s 669.74 Melem/s 680.53 Melem/s]

metadata_store_basic/create
                        time:   [3.0740 µs 3.0882 µs 3.1002 µs]
                        thrpt:  [322.56 Kelem/s 323.82 Kelem/s 325.30 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  5 (5.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
metadata_store_basic/upsert_new
                        time:   [1.7218 µs 1.7296 µs 1.7377 µs]
                        thrpt:  [575.48 Kelem/s 578.17 Kelem/s 580.78 Kelem/s]
Found 21 outliers among 100 measurements (21.00%)
  10 (10.00%) low severe
  3 (3.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
metadata_store_basic/upsert_existing
                        time:   [994.02 ns 998.60 ns 1.0026 µs]
                        thrpt:  [997.37 Kelem/s 1.0014 Melem/s 1.0060 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/get
                        time:   [23.483 ns 23.552 ns 23.609 ns]
                        thrpt:  [42.356 Melem/s 42.460 Melem/s 42.584 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/get_miss
                        time:   [23.438 ns 23.495 ns 23.545 ns]
                        thrpt:  [42.473 Melem/s 42.561 Melem/s 42.665 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/len
                        time:   [950.86 ns 955.72 ns 960.25 ns]
                        thrpt:  [1.0414 Melem/s 1.0463 Melem/s 1.0517 Melem/s]
Found 28 outliers among 100 measurements (28.00%)
  19 (19.00%) low severe
  1 (1.00%) high mild
  8 (8.00%) high severe
Benchmarking metadata_store_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 17.1s, or reduce sample count to 20.
metadata_store_basic/stats
                        time:   [168.12 ms 168.70 ms 169.35 ms]
                        thrpt:  [5.9050  elem/s 5.9276  elem/s 5.9480  elem/s]
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) high mild
  6 (6.00%) high severe

metadata_store_query/query_by_status
                        time:   [291.45 µs 292.46 µs 293.32 µs]
                        thrpt:  [3.4092 Kelem/s 3.4193 Kelem/s 3.4311 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  7 (7.00%) high severe
metadata_store_query/query_by_continent
                        time:   [141.67 µs 142.28 µs 142.94 µs]
                        thrpt:  [6.9961 Kelem/s 7.0285 Kelem/s 7.0585 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) high mild
  6 (6.00%) high severe
metadata_store_query/query_by_tier
                        time:   [394.18 µs 395.56 µs 396.78 µs]
                        thrpt:  [2.5203 Kelem/s 2.5280 Kelem/s 2.5369 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  4 (4.00%) high mild
  4 (4.00%) high severe
metadata_store_query/query_accepting_work
                        time:   [494.49 µs 495.67 µs 496.97 µs]
                        thrpt:  [2.0122 Kelem/s 2.0175 Kelem/s 2.0223 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
metadata_store_query/query_with_limit
                        time:   [478.49 µs 479.37 µs 480.15 µs]
                        thrpt:  [2.0827 Kelem/s 2.0861 Kelem/s 2.0899 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  7 (7.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe
metadata_store_query/query_complex
                        time:   [307.55 µs 308.55 µs 309.56 µs]
                        thrpt:  [3.2304 Kelem/s 3.2410 Kelem/s 3.2515 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  3 (3.00%) low mild
  7 (7.00%) high mild
  5 (5.00%) high severe

metadata_store_spatial/find_nearby_100km
                        time:   [325.44 µs 327.11 µs 328.68 µs]
                        thrpt:  [3.0425 Kelem/s 3.0570 Kelem/s 3.0727 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
metadata_store_spatial/find_nearby_1000km
                        time:   [378.06 µs 379.99 µs 381.79 µs]
                        thrpt:  [2.6192 Kelem/s 2.6316 Kelem/s 2.6451 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  10 (10.00%) high mild
  1 (1.00%) high severe
metadata_store_spatial/find_nearby_5000km
                        time:   [535.14 µs 546.03 µs 556.28 µs]
                        thrpt:  [1.7976 Kelem/s 1.8314 Kelem/s 1.8687 Kelem/s]
metadata_store_spatial/find_best_for_routing
                        time:   [382.16 µs 383.67 µs 384.99 µs]
                        thrpt:  [2.5974 Kelem/s 2.6064 Kelem/s 2.6167 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
metadata_store_spatial/find_relays
                        time:   [493.55 µs 494.57 µs 495.69 µs]
                        thrpt:  [2.0174 Kelem/s 2.0220 Kelem/s 2.0261 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe

metadata_store_scaling/query_status/1000
                        time:   [21.660 µs 21.686 µs 21.715 µs]
                        thrpt:  [46.052 Kelem/s 46.112 Kelem/s 46.169 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low severe
  2 (2.00%) high severe
metadata_store_scaling/query_complex/1000
                        time:   [20.863 µs 20.900 µs 20.941 µs]
                        thrpt:  [47.753 Kelem/s 47.848 Kelem/s 47.932 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/1000
                        time:   [50.748 µs 50.879 µs 50.980 µs]
                        thrpt:  [19.615 Kelem/s 19.654 Kelem/s 19.705 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  2 (2.00%) high mild
  7 (7.00%) high severe
metadata_store_scaling/query_status/5000
                        time:   [110.31 µs 111.07 µs 111.85 µs]
                        thrpt:  [8.9407 Kelem/s 9.0033 Kelem/s 9.0650 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
metadata_store_scaling/query_complex/5000
                        time:   [114.45 µs 114.85 µs 115.25 µs]
                        thrpt:  [8.6767 Kelem/s 8.7067 Kelem/s 8.7375 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
metadata_store_scaling/find_nearby/5000
                        time:   [241.03 µs 241.82 µs 242.42 µs]
                        thrpt:  [4.1250 Kelem/s 4.1353 Kelem/s 4.1488 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
metadata_store_scaling/query_status/10000
                        time:   [286.91 µs 288.42 µs 289.87 µs]
                        thrpt:  [3.4498 Kelem/s 3.4672 Kelem/s 3.4854 Kelem/s]
Found 19 outliers among 100 measurements (19.00%)
  4 (4.00%) low severe
  8 (8.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe
metadata_store_scaling/query_complex/10000
                        time:   [300.43 µs 302.12 µs 303.66 µs]
                        thrpt:  [3.2931 Kelem/s 3.3100 Kelem/s 3.3286 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/find_nearby/10000
                        time:   [499.17 µs 500.54 µs 501.72 µs]
                        thrpt:  [1.9931 Kelem/s 1.9979 Kelem/s 2.0033 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  6 (6.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_status/50000
                        time:   [2.0582 ms 2.0787 ms 2.1004 ms]
                        thrpt:  [476.11  elem/s 481.06  elem/s 485.85  elem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
metadata_store_scaling/query_complex/50000
                        time:   [2.1483 ms 2.1747 ms 2.2038 ms]
                        thrpt:  [453.76  elem/s 459.83  elem/s 465.48  elem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
metadata_store_scaling/find_nearby/50000
                        time:   [2.5250 ms 2.5367 ms 2.5484 ms]
                        thrpt:  [392.40  elem/s 394.21  elem/s 396.04  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

metadata_store_concurrent/concurrent_upsert/4
                        time:   [1.2192 ms 1.2257 ms 1.2325 ms]
                        thrpt:  [1.6227 Melem/s 1.6317 Melem/s 1.6404 Melem/s]
metadata_store_concurrent/concurrent_query/4
                        time:   [231.87 ms 233.08 ms 234.30 ms]
                        thrpt:  [8.5360 Kelem/s 8.5808 Kelem/s 8.6254 Kelem/s]
metadata_store_concurrent/concurrent_mixed/4
                        time:   [209.43 ms 210.15 ms 210.88 ms]
                        thrpt:  [9.4839 Kelem/s 9.5169 Kelem/s 9.5498 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
metadata_store_concurrent/concurrent_upsert/8
                        time:   [1.7834 ms 1.8528 ms 1.9231 ms]
                        thrpt:  [2.0800 Melem/s 2.1589 Melem/s 2.2429 Melem/s]
Found 3 outliers among 20 measurements (15.00%)
  3 (15.00%) low mild
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.5s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/8
                        time:   [277.43 ms 284.23 ms 291.64 ms]
                        thrpt:  [13.716 Kelem/s 14.073 Kelem/s 14.418 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.4s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/8
                        time:   [261.60 ms 272.86 ms 285.06 ms]
                        thrpt:  [14.032 Kelem/s 14.659 Kelem/s 15.291 Kelem/s]
metadata_store_concurrent/concurrent_upsert/16
                        time:   [3.4373 ms 3.4645 ms 3.4976 ms]
                        thrpt:  [2.2873 Melem/s 2.3091 Melem/s 2.3274 Melem/s]
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 9.0s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/16
                        time:   [429.98 ms 439.10 ms 450.64 ms]
                        thrpt:  [17.753 Kelem/s 18.219 Kelem/s 18.606 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 9.9s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/16
                        time:   [485.42 ms 492.80 ms 499.99 ms]
                        thrpt:  [16.000 Kelem/s 16.234 Kelem/s 16.481 Kelem/s]

metadata_store_versioning/update_versioned_success
                        time:   [220.85 ns 221.34 ns 221.82 ns]
                        thrpt:  [4.5082 Melem/s 4.5179 Melem/s 4.5279 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
metadata_store_versioning/update_versioned_conflict
                        time:   [222.25 ns 223.03 ns 223.94 ns]
                        thrpt:  [4.4654 Melem/s 4.4836 Melem/s 4.4995 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

schema_validation/validate_string
                        time:   [2.7390 ns 3.0070 ns 3.3326 ns]
                        thrpt:  [300.06 Melem/s 332.56 Melem/s 365.10 Melem/s]
Found 23 outliers among 100 measurements (23.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  20 (20.00%) high severe
schema_validation/validate_integer
                        time:   [2.9289 ns 3.1816 ns 3.4837 ns]
                        thrpt:  [287.05 Melem/s 314.31 Melem/s 341.43 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  17 (17.00%) high severe
schema_validation/validate_object
                        time:   [59.258 ns 59.457 ns 59.656 ns]
                        thrpt:  [16.763 Melem/s 16.819 Melem/s 16.875 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
schema_validation/validate_array_10
                        time:   [35.082 ns 35.211 ns 35.375 ns]
                        thrpt:  [28.268 Melem/s 28.400 Melem/s 28.504 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
schema_validation/validate_complex
                        time:   [153.59 ns 154.29 ns 154.94 ns]
                        thrpt:  [6.4539 Melem/s 6.4815 Melem/s 6.5107 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) low mild
  2 (2.00%) high mild

endpoint_matching/match_success
                        time:   [212.11 ns 212.55 ns 213.01 ns]
                        thrpt:  [4.6947 Melem/s 4.7048 Melem/s 4.7144 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
endpoint_matching/match_failure
                        time:   [207.96 ns 208.28 ns 208.60 ns]
                        thrpt:  [4.7938 Melem/s 4.8011 Melem/s 4.8086 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
endpoint_matching/match_multi_param
                        time:   [469.04 ns 469.59 ns 470.19 ns]
                        thrpt:  [2.1268 Melem/s 2.1295 Melem/s 2.1320 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

api_version/is_compatible_with
                        time:   [200.20 ps 200.26 ps 200.35 ps]
                        thrpt:  [4.9912 Gelem/s 4.9934 Gelem/s 4.9951 Gelem/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
api_version/parse       time:   [41.534 ns 41.580 ns 41.631 ns]
                        thrpt:  [24.021 Melem/s 24.050 Melem/s 24.077 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  4 (4.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
api_version/to_string   time:   [53.006 ns 53.083 ns 53.166 ns]
                        thrpt:  [18.809 Melem/s 18.838 Melem/s 18.866 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild

api_schema/create       time:   [4.1144 µs 4.1188 µs 4.1237 µs]
                        thrpt:  [242.50 Kelem/s 242.79 Kelem/s 243.05 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
api_schema/serialize    time:   [1.7542 µs 1.7565 µs 1.7587 µs]
                        thrpt:  [568.60 Kelem/s 569.33 Kelem/s 570.05 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
api_schema/deserialize  time:   [8.2711 µs 8.2827 µs 8.2953 µs]
                        thrpt:  [120.55 Kelem/s 120.73 Kelem/s 120.90 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
api_schema/find_endpoint
                        time:   [228.16 ns 228.54 ns 228.90 ns]
                        thrpt:  [4.3687 Melem/s 4.3756 Melem/s 4.3828 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
api_schema/endpoints_by_tag
                        time:   [105.44 ns 105.67 ns 105.91 ns]
                        thrpt:  [9.4416 Melem/s 9.4632 Melem/s 9.4843 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

request_validation/validate_full_request
                        time:   [56.761 ns 56.954 ns 57.158 ns]
                        thrpt:  [17.495 Melem/s 17.558 Melem/s 17.618 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
request_validation/validate_path_only
                        time:   [15.752 ns 15.797 ns 15.841 ns]
                        thrpt:  [63.128 Melem/s 63.302 Melem/s 63.484 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

api_registry_basic/create
                        time:   [1.4286 µs 1.4303 µs 1.4322 µs]
                        thrpt:  [698.23 Kelem/s 699.13 Kelem/s 700.00 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
api_registry_basic/register_new
                        time:   [3.8395 µs 3.8513 µs 3.8636 µs]
                        thrpt:  [258.83 Kelem/s 259.65 Kelem/s 260.45 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  12 (12.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
api_registry_basic/get  time:   [32.320 ns 35.095 ns 37.436 ns]
                        thrpt:  [26.713 Melem/s 28.494 Melem/s 30.940 Melem/s]
Found 25 outliers among 100 measurements (25.00%)
  1 (1.00%) high mild
  24 (24.00%) high severe
api_registry_basic/len  time:   [1.4123 µs 1.4158 µs 1.4191 µs]
                        thrpt:  [704.67 Kelem/s 706.31 Kelem/s 708.07 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking api_registry_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 20.1s, or reduce sample count to 20.
api_registry_basic/stats
                        time:   [200.12 ms 201.24 ms 202.55 ms]
                        thrpt:  [4.9371  elem/s 4.9692  elem/s 4.9971  elem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

api_registry_query/query_by_name
                        time:   [83.850 µs 83.987 µs 84.148 µs]
                        thrpt:  [11.884 Kelem/s 11.907 Kelem/s 11.926 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  9 (9.00%) high severe
api_registry_query/query_by_tag
                        time:   [780.11 µs 781.73 µs 783.35 µs]
                        thrpt:  [1.2766 Kelem/s 1.2792 Kelem/s 1.2819 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low severe
  3 (3.00%) high mild
  1 (1.00%) high severe
api_registry_query/query_with_version
                        time:   [48.452 µs 55.062 µs 61.518 µs]
                        thrpt:  [16.255 Kelem/s 18.161 Kelem/s 20.639 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
api_registry_query/find_by_endpoint
                        time:   [6.8730 ms 6.9817 ms 7.0975 ms]
                        thrpt:  [140.90  elem/s 143.23  elem/s 145.50  elem/s]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
api_registry_query/find_compatible
                        time:   [117.84 µs 118.17 µs 118.58 µs]
                        thrpt:  [8.4333 Kelem/s 8.4626 Kelem/s 8.4862 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe

api_registry_scaling/query_by_name/1000
                        time:   [14.780 µs 14.817 µs 14.859 µs]
                        thrpt:  [67.299 Kelem/s 67.488 Kelem/s 67.661 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_tag/1000
                        time:   [75.963 µs 76.101 µs 76.249 µs]
                        thrpt:  [13.115 Kelem/s 13.140 Kelem/s 13.164 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
api_registry_scaling/query_by_name/5000
                        time:   [76.468 µs 76.625 µs 76.798 µs]
                        thrpt:  [13.021 Kelem/s 13.051 Kelem/s 13.077 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
api_registry_scaling/query_by_tag/5000
                        time:   [449.25 µs 451.15 µs 453.36 µs]
                        thrpt:  [2.2058 Kelem/s 2.2166 Kelem/s 2.2259 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
api_registry_scaling/query_by_name/10000
                        time:   [174.58 µs 175.17 µs 175.81 µs]
                        thrpt:  [5.6878 Kelem/s 5.7086 Kelem/s 5.7282 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking api_registry_scaling/query_by_tag/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.6s, enable flat sampling, or reduce sample count to 60.
api_registry_scaling/query_by_tag/10000
                        time:   [1.1179 ms 1.1348 ms 1.1522 ms]
                        thrpt:  [867.90  elem/s 881.20  elem/s 894.55  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 22.1s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/4
                        time:   [949.01 ms 988.41 ms 1.0305 s]
                        thrpt:  [1.9408 Kelem/s 2.0234 Kelem/s 2.1075 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 15.2s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/4
                        time:   [757.31 ms 768.65 ms 779.99 ms]
                        thrpt:  [2.5641 Kelem/s 2.6020 Kelem/s 2.6409 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 24.3s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/8
                        time:   [1.0067 s 1.0915 s 1.1642 s]
                        thrpt:  [3.4360 Kelem/s 3.6648 Kelem/s 3.9736 Kelem/s]
Found 4 outliers among 20 measurements (20.00%)
  3 (15.00%) low severe
  1 (5.00%) high mild
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 11.8s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/8
                        time:   [652.45 ms 734.66 ms 825.38 ms]
                        thrpt:  [4.8462 Kelem/s 5.4447 Kelem/s 6.1307 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 39.4s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/16
                        time:   [1.9191 s 1.9646 s 2.0109 s]
                        thrpt:  [3.9783 Kelem/s 4.0722 Kelem/s 4.1687 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 36.9s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/16
                        time:   [1.7168 s 1.7745 s 1.8357 s]
                        thrpt:  [4.3581 Kelem/s 4.5082 Kelem/s 4.6598 Kelem/s]

compare_op/eq           time:   [3.2539 ns 3.2687 ns 3.2849 ns]
                        thrpt:  [304.43 Melem/s 305.93 Melem/s 307.33 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
compare_op/gt           time:   [2.8475 ns 2.8524 ns 2.8581 ns]
                        thrpt:  [349.88 Melem/s 350.58 Melem/s 351.18 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
compare_op/contains_string
                        time:   [30.971 ns 31.518 ns 32.226 ns]
                        thrpt:  [31.030 Melem/s 31.727 Melem/s 32.288 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
compare_op/in_array     time:   [8.3009 ns 8.3547 ns 8.4039 ns]
                        thrpt:  [118.99 Melem/s 119.69 Melem/s 120.47 Melem/s]

condition/simple        time:   [106.16 ns 107.30 ns 108.51 ns]
                        thrpt:  [9.2161 Melem/s 9.3194 Melem/s 9.4196 Melem/s]
condition/nested_field  time:   [1.1924 µs 1.2139 µs 1.2402 µs]
                        thrpt:  [806.30 Kelem/s 823.81 Kelem/s 838.63 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  12 (12.00%) high mild
  5 (5.00%) high severe
condition/string_eq     time:   [165.09 ns 167.60 ns 170.41 ns]
                        thrpt:  [5.8681 Melem/s 5.9667 Melem/s 6.0572 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

condition_expr/single   time:   [108.81 ns 109.96 ns 111.06 ns]
                        thrpt:  [9.0044 Melem/s 9.0942 Melem/s 9.1904 Melem/s]
condition_expr/and_2    time:   [221.69 ns 223.87 ns 225.95 ns]
                        thrpt:  [4.4257 Melem/s 4.4668 Melem/s 4.5107 Melem/s]
condition_expr/and_5    time:   [680.84 ns 687.03 ns 692.89 ns]
                        thrpt:  [1.4432 Melem/s 1.4555 Melem/s 1.4688 Melem/s]
condition_expr/or_3     time:   [386.99 ns 391.46 ns 395.89 ns]
                        thrpt:  [2.5259 Melem/s 2.5546 Melem/s 2.5841 Melem/s]
condition_expr/nested   time:   [288.17 ns 292.06 ns 296.00 ns]
                        thrpt:  [3.3784 Melem/s 3.4239 Melem/s 3.4702 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild

rule/create             time:   [784.04 ns 786.63 ns 789.47 ns]
                        thrpt:  [1.2667 Melem/s 1.2712 Melem/s 1.2754 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
rule/matches            time:   [219.50 ns 220.94 ns 222.62 ns]
                        thrpt:  [4.4920 Melem/s 4.5261 Melem/s 4.5558 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  20 (20.00%) high mild
  2 (2.00%) high severe

rule_context/create     time:   [2.8178 µs 2.8230 µs 2.8282 µs]
                        thrpt:  [353.58 Kelem/s 354.23 Kelem/s 354.88 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
rule_context/get_simple time:   [104.18 ns 105.38 ns 106.74 ns]
                        thrpt:  [9.3690 Melem/s 9.4893 Melem/s 9.5983 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
rule_context/get_nested time:   [1.1989 µs 1.2167 µs 1.2369 µs]
                        thrpt:  [808.48 Kelem/s 821.87 Kelem/s 834.08 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  6 (6.00%) high mild
  14 (14.00%) high severe
rule_context/get_deep_nested
                        time:   [1.1952 µs 1.2081 µs 1.2229 µs]
                        thrpt:  [817.71 Kelem/s 827.72 Kelem/s 836.66 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  10 (10.00%) high mild
  2 (2.00%) high severe

rule_engine_basic/create
                        time:   [19.656 ns 20.118 ns 20.590 ns]
                        thrpt:  [48.567 Melem/s 49.707 Melem/s 50.874 Melem/s]
rule_engine_basic/add_rule
                        time:   [3.3259 µs 3.4687 µs 3.5998 µs]
                        thrpt:  [277.79 Kelem/s 288.30 Kelem/s 300.67 Kelem/s]
rule_engine_basic/get_rule
                        time:   [29.209 ns 29.891 ns 30.648 ns]
                        thrpt:  [32.629 Melem/s 33.455 Melem/s 34.236 Melem/s]
rule_engine_basic/rules_by_tag
                        time:   [1.9817 µs 1.9848 µs 1.9883 µs]
                        thrpt:  [502.95 Kelem/s 503.82 Kelem/s 504.61 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
rule_engine_basic/stats time:   [18.067 µs 18.197 µs 18.334 µs]
                        thrpt:  [54.545 Kelem/s 54.955 Kelem/s 55.349 Kelem/s]
Found 18 outliers among 100 measurements (18.00%)
  8 (8.00%) high mild
  10 (10.00%) high severe

rule_engine_evaluate/evaluate_10_rules
                        time:   [5.8342 µs 5.8601 µs 5.8848 µs]
                        thrpt:  [169.93 Kelem/s 170.64 Kelem/s 171.40 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_first_10_rules
                        time:   [667.84 ns 674.05 ns 679.93 ns]
                        thrpt:  [1.4707 Melem/s 1.4836 Melem/s 1.4974 Melem/s]
rule_engine_evaluate/evaluate_100_rules
                        time:   [64.467 µs 64.753 µs 65.024 µs]
                        thrpt:  [15.379 Kelem/s 15.443 Kelem/s 15.512 Kelem/s]
rule_engine_evaluate/evaluate_first_100_rules
                        time:   [671.87 ns 677.48 ns 683.04 ns]
                        thrpt:  [1.4640 Melem/s 1.4761 Melem/s 1.4884 Melem/s]
rule_engine_evaluate/evaluate_matching_100_rules
                        time:   [65.979 µs 66.177 µs 66.380 µs]
                        thrpt:  [15.065 Kelem/s 15.111 Kelem/s 15.156 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
rule_engine_evaluate/evaluate_1000_rules
                        time:   [614.26 µs 617.46 µs 620.59 µs]
                        thrpt:  [1.6114 Kelem/s 1.6195 Kelem/s 1.6280 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
rule_engine_evaluate/evaluate_first_1000_rules
                        time:   [666.21 ns 671.00 ns 675.80 ns]
                        thrpt:  [1.4797 Melem/s 1.4903 Melem/s 1.5010 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

rule_engine_scaling/evaluate/10
                        time:   [5.8216 µs 5.8521 µs 5.8812 µs]
                        thrpt:  [170.03 Kelem/s 170.88 Kelem/s 171.77 Kelem/s]
rule_engine_scaling/evaluate_first/10
                        time:   [666.19 ns 671.05 ns 676.06 ns]
                        thrpt:  [1.4792 Melem/s 1.4902 Melem/s 1.5011 Melem/s]
rule_engine_scaling/evaluate/50
                        time:   [33.285 µs 33.416 µs 33.538 µs]
                        thrpt:  [29.817 Kelem/s 29.926 Kelem/s 30.043 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_engine_scaling/evaluate_first/50
                        time:   [672.46 ns 676.81 ns 681.03 ns]
                        thrpt:  [1.4684 Melem/s 1.4775 Melem/s 1.4871 Melem/s]
rule_engine_scaling/evaluate/100
                        time:   [63.287 µs 63.539 µs 63.774 µs]
                        thrpt:  [15.680 Kelem/s 15.738 Kelem/s 15.801 Kelem/s]
rule_engine_scaling/evaluate_first/100
                        time:   [321.80 ns 322.34 ns 322.96 ns]
                        thrpt:  [3.0963 Melem/s 3.1023 Melem/s 3.1075 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  3 (3.00%) high severe
rule_engine_scaling/evaluate/500
                        time:   [150.71 µs 150.92 µs 151.17 µs]
                        thrpt:  [6.6149 Kelem/s 6.6261 Kelem/s 6.6353 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
rule_engine_scaling/evaluate_first/500
                        time:   [320.81 ns 321.21 ns 321.68 ns]
                        thrpt:  [3.1087 Melem/s 3.1132 Melem/s 3.1171 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
rule_engine_scaling/evaluate/1000
                        time:   [303.91 µs 305.27 µs 307.04 µs]
                        thrpt:  [3.2569 Kelem/s 3.2758 Kelem/s 3.2904 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
rule_engine_scaling/evaluate_first/1000
                        time:   [662.24 ns 666.98 ns 672.23 ns]
                        thrpt:  [1.4876 Melem/s 1.4993 Melem/s 1.5100 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

rule_set/create         time:   [6.8073 µs 6.8999 µs 7.0255 µs]
                        thrpt:  [142.34 Kelem/s 144.93 Kelem/s 146.90 Kelem/s]
rule_set/load_into_engine
                        time:   [11.998 µs 12.008 µs 12.020 µs]
                        thrpt:  [83.195 Kelem/s 83.277 Kelem/s 83.349 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  6 (6.00%) high severe

trace_id/generate       time:   [47.601 ns 47.688 ns 47.781 ns]
                        thrpt:  [20.929 Melem/s 20.970 Melem/s 21.008 Melem/s]
trace_id/to_hex         time:   [89.750 ns 89.919 ns 90.165 ns]
                        thrpt:  [11.091 Melem/s 11.121 Melem/s 11.142 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  7 (7.00%) high mild
  3 (3.00%) high severe
trace_id/from_hex       time:   [18.208 ns 18.232 ns 18.259 ns]
                        thrpt:  [54.768 Melem/s 54.848 Melem/s 54.920 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  3 (3.00%) high severe

context_operations/create
                        time:   [148.20 ns 150.15 ns 151.45 ns]
                        thrpt:  [6.6028 Melem/s 6.6600 Melem/s 6.7475 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  2 (2.00%) high severe
context_operations/child
                        time:   [36.874 ns 37.135 ns 37.457 ns]
                        thrpt:  [26.697 Melem/s 26.929 Melem/s 27.120 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
context_operations/for_remote
                        time:   [36.537 ns 36.650 ns 36.776 ns]
                        thrpt:  [27.192 Melem/s 27.285 Melem/s 27.369 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
context_operations/to_traceparent
                        time:   [341.95 ns 342.48 ns 343.12 ns]
                        thrpt:  [2.9145 Melem/s 2.9199 Melem/s 2.9244 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
context_operations/from_traceparent
                        time:   [117.99 ns 118.23 ns 118.52 ns]
                        thrpt:  [8.4376 Melem/s 8.4581 Melem/s 8.4754 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe

baggage/create          time:   [4.7524 ns 4.7746 ns 4.7948 ns]
                        thrpt:  [208.56 Melem/s 209.44 Melem/s 210.42 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  14 (14.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe
baggage/get             time:   [7.9671 ns 7.9922 ns 8.0163 ns]
                        thrpt:  [124.75 Melem/s 125.12 Melem/s 125.52 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  10 (10.00%) high severe
baggage/set             time:   [60.745 ns 61.010 ns 61.255 ns]
                        thrpt:  [16.325 Melem/s 16.391 Melem/s 16.462 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) low mild
  5 (5.00%) high mild
baggage/merge           time:   [1.6755 µs 1.6806 µs 1.6854 µs]
                        thrpt:  [593.31 Kelem/s 595.04 Kelem/s 596.82 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  2 (2.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  3 (3.00%) high severe

span/create             time:   [83.476 ns 83.634 ns 83.819 ns]
                        thrpt:  [11.930 Melem/s 11.957 Melem/s 11.979 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
span/set_attribute      time:   [57.747 ns 57.836 ns 57.925 ns]
                        thrpt:  [17.264 Melem/s 17.290 Melem/s 17.317 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
span/add_event          time:   [64.140 ns 64.958 ns 65.840 ns]
                        thrpt:  [15.188 Melem/s 15.395 Melem/s 15.591 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
span/with_kind          time:   [83.177 ns 83.306 ns 83.426 ns]
                        thrpt:  [11.987 Melem/s 12.004 Melem/s 12.023 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe

context_store/create_context
                        time:   [192.59 ns 192.79 ns 192.97 ns]
                        thrpt:  [5.1822 Melem/s 5.1871 Melem/s 5.1924 Melem/s]
                 change:
                        time:   [−2.7014% −1.8597% −1.0575%] (p = 0.00 < 0.05)
                        thrpt:  [+1.0689% +1.8950% +2.7764%]
                        Performance has improved.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
context_store/get_context
                        time:   [34.779 ns 35.024 ns 35.258 ns]
                        thrpt:  [28.363 Melem/s 28.551 Melem/s 28.753 Melem/s]
                 change:
                        time:   [−12.013% −10.979% −9.8930%] (p = 0.00 < 0.05)
                        thrpt:  [+10.979% +12.333% +13.654%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
context_store/add_span  time:   [114.94 ns 115.12 ns 115.31 ns]
                        thrpt:  [8.6722 Melem/s 8.6865 Melem/s 8.7003 Melem/s]
                 change:
                        time:   [−4.3343% −3.6093% −2.8729%] (p = 0.00 < 0.05)
                        thrpt:  [+2.9579% +3.7444% +4.5306%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

propagation_context/from_context
                        time:   [1.4054 µs 1.4116 µs 1.4188 µs]
                        thrpt:  [704.80 Kelem/s 708.41 Kelem/s 711.54 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  8 (8.00%) high mild
  7 (7.00%) high severe
propagation_context/to_context
                        time:   [891.39 ns 896.50 ns 904.49 ns]
                        thrpt:  [1.1056 Melem/s 1.1154 Melem/s 1.1218 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

context_store_concurrent/concurrent_get
                        time:   [75.878 ns 80.538 ns 84.644 ns]
                        thrpt:  [11.814 Melem/s 12.416 Melem/s 13.179 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low severe
  4 (4.00%) high mild
  3 (3.00%) high severe

endpoint/create         time:   [2.0380 ns 2.0593 ns 2.0829 ns]
                        thrpt:  [480.10 Melem/s 485.59 Melem/s 490.68 Melem/s]
endpoint/create_with_config
                        time:   [94.796 ns 95.019 ns 95.236 ns]
                        thrpt:  [10.500 Melem/s 10.524 Melem/s 10.549 Melem/s]
endpoint/effective_weight
                        time:   [200.77 ps 200.85 ps 200.96 ps]
                        thrpt:  [4.9761 Gelem/s 4.9787 Gelem/s 4.9809 Gelem/s]
Found 18 outliers among 100 measurements (18.00%)
  4 (4.00%) low mild
  7 (7.00%) high mild
  7 (7.00%) high severe

load_metrics/load_score time:   [230.60 ps 240.42 ps 248.82 ps]
                        thrpt:  [4.0190 Gelem/s 4.1595 Gelem/s 4.3366 Gelem/s]
Found 24 outliers among 100 measurements (24.00%)
  24 (24.00%) high severe
load_metrics/is_overloaded
                        time:   [274.80 ps 277.00 ps 280.25 ps]
                        thrpt:  [3.5682 Gelem/s 3.6101 Gelem/s 3.6391 Gelem/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe

lb_strategies/round_robin
                        time:   [7.6383 µs 8.2373 µs 8.9171 µs]
                        thrpt:  [112.14 Kelem/s 121.40 Kelem/s 130.92 Kelem/s]
Found 22 outliers among 100 measurements (22.00%)
  19 (19.00%) low severe
  2 (2.00%) high mild
  1 (1.00%) high severe
lb_strategies/weighted_round_robin
                        time:   [5.7090 µs 5.7177 µs 5.7297 µs]
                        thrpt:  [174.53 Kelem/s 174.89 Kelem/s 175.16 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
lb_strategies/least_connections
                        time:   [5.6419 µs 5.6469 µs 5.6527 µs]
                        thrpt:  [176.91 Kelem/s 177.09 Kelem/s 177.25 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
lb_strategies/random    time:   [5.5985 µs 5.6032 µs 5.6086 µs]
                        thrpt:  [178.30 Kelem/s 178.47 Kelem/s 178.62 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
lb_strategies/power_of_two
                        time:   [10.575 µs 10.635 µs 10.704 µs]
                        thrpt:  [93.426 Kelem/s 94.027 Kelem/s 94.560 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
lb_strategies/consistent_hash
                        time:   [50.547 µs 50.628 µs 50.718 µs]
                        thrpt:  [19.717 Kelem/s 19.752 Kelem/s 19.784 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
lb_strategies/least_load
                        time:   [5.7484 µs 5.7537 µs 5.7594 µs]
                        thrpt:  [173.63 Kelem/s 173.80 Kelem/s 173.96 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

lb_scaling/select/10    time:   [5.5805 µs 5.5862 µs 5.5926 µs]
                        thrpt:  [178.81 Kelem/s 179.01 Kelem/s 179.20 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
lb_scaling/select/50    time:   [6.6440 µs 6.6546 µs 6.6647 µs]
                        thrpt:  [150.05 Kelem/s 150.27 Kelem/s 150.51 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
lb_scaling/select/100   time:   [8.0260 µs 8.0355 µs 8.0460 µs]
                        thrpt:  [124.29 Kelem/s 124.45 Kelem/s 124.60 Kelem/s]
lb_scaling/select/500   time:   [11.867 µs 11.875 µs 11.884 µs]
                        thrpt:  [84.148 Kelem/s 84.210 Kelem/s 84.264 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

lb_zone_aware/zone_match
                        time:   [5.6535 µs 5.6600 µs 5.6681 µs]
                        thrpt:  [176.43 Kelem/s 176.68 Kelem/s 176.88 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
lb_zone_aware/zone_fallback
                        time:   [7.9587 µs 8.5891 µs 9.1070 µs]
                        thrpt:  [109.81 Kelem/s 116.43 Kelem/s 125.65 Kelem/s]

lb_health_updates/update_health
                        time:   [53.633 ns 53.792 ns 53.952 ns]
                        thrpt:  [18.535 Melem/s 18.590 Melem/s 18.645 Melem/s]
lb_health_updates/update_metrics
                        time:   [189.33 ns 189.70 ns 190.12 ns]
                        thrpt:  [5.2599 Melem/s 5.2716 Melem/s 5.2818 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
