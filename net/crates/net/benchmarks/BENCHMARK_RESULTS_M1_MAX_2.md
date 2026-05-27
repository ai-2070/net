     Running benches/auth_guard.rs (target/release/deps/auth_guard-a16a75d9be35ed91)
Gnuplot not found, using plotters backend
Benchmarking auth_guard_check_fast_hit/single_thread: Collecting 50 samples in estimated 5.00auth_guard_check_fast_hit/single_thread
                        time:   [23.860 ns 23.910 ns 23.963 ns]
                        thrpt:  [41.730 Melem/s 41.823 Melem/s 41.911 Melem/s]
                 change:
                        time:   [+0.7696% +1.2798% +1.7581%] (p = 0.00 < 0.05)
                        thrpt:  [−1.7278% −1.2636% −0.7637%]
                        Change within noise threshold.

Benchmarking auth_guard_check_fast_miss/single_thread: Collecting 50 samples in estimated 5.0auth_guard_check_fast_miss/single_thread
                        time:   [3.8375 ns 3.8454 ns 3.8524 ns]
                        thrpt:  [259.58 Melem/s 260.05 Melem/s 260.58 Melem/s]
                 change:
                        time:   [−0.2628% +0.1681% +0.5755%] (p = 0.45 > 0.05)
                        thrpt:  [−0.5722% −0.1678% +0.2635%]
                        No change in performance detected.
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

Benchmarking auth_guard_check_fast_contended/eight_threads: Collecting 50 samples in estimateauth_guard_check_fast_contended/eight_threads
                        time:   [28.666 ns 28.723 ns 28.774 ns]
                        thrpt:  [34.754 Melem/s 34.815 Melem/s 34.885 Melem/s]
                 change:
                        time:   [−0.9071% −0.5173% +0.0197%] (p = 0.02 < 0.05)
                        thrpt:  [−0.0197% +0.5200% +0.9154%]
                        Change within noise threshold.
Found 11 outliers among 50 measurements (22.00%)
  3 (6.00%) low severe
  4 (8.00%) low mild
  2 (4.00%) high mild
  2 (4.00%) high severe

Benchmarking auth_guard_allow_channel/insert: Collecting 50 samples in estimated 5.0001 s (17auth_guard_allow_channel/insert
                        time:   [168.35 ns 173.90 ns 178.82 ns]
                        thrpt:  [5.5921 Melem/s 5.7506 Melem/s 5.9399 Melem/s]
                 change:
                        time:   [−5.8508% −1.1715% +3.6350%] (p = 0.64 > 0.05)
                        thrpt:  [−3.5075% +1.1854% +6.2144%]
                        No change in performance detected.

Benchmarking auth_guard_hot_hit_ceiling/million_ops: Collecting 50 samples in estimated 7.221auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.8296 ms 2.8326 ms 2.8366 ms]
                        change: [−1.8234% −1.5112% −1.2171%] (p = 0.00 < 0.05)
                        Performance has improved.
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

     Running benches/ingestion.rs (target/release/deps/ingestion-907b36ebc65038c2)
Gnuplot not found, using plotters backend
Benchmarking shard/ingest_raw/1024: Collecting 100 samples in estimated 5.0002 s (108M iteratshard/ingest_raw/1024   time:   [46.530 ns 46.645 ns 46.791 ns]
                        thrpt:  [21.372 Melem/s 21.439 Melem/s 21.491 Melem/s]
                 change:
                        time:   [−0.2776% +0.0384% +0.3536%] (p = 0.82 > 0.05)
                        thrpt:  [−0.3524% −0.0384% +0.2784%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
Benchmarking shard/ingest_raw_pop/1024: Collecting 100 samples in estimated 5.0001 s (115M itshard/ingest_raw_pop/1024
                        time:   [43.444 ns 43.469 ns 43.495 ns]
                        thrpt:  [22.991 Melem/s 23.005 Melem/s 23.018 Melem/s]
                 change:
                        time:   [−0.6111% −0.3041% −0.0931%] (p = 0.01 < 0.05)
                        thrpt:  [+0.0932% +0.3050% +0.6148%]
                        Change within noise threshold.
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking shard/ingest_raw/8192: Collecting 100 samples in estimated 5.0002 s (107M iteratshard/ingest_raw/8192   time:   [46.452 ns 46.482 ns 46.513 ns]
                        thrpt:  [21.499 Melem/s 21.514 Melem/s 21.528 Melem/s]
                 change:
                        time:   [−0.5123% −0.1997% +0.1309%] (p = 0.24 > 0.05)
                        thrpt:  [−0.1308% +0.2001% +0.5149%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking shard/ingest_raw_pop/8192: Collecting 100 samples in estimated 5.0001 s (115M itshard/ingest_raw_pop/8192
                        time:   [43.518 ns 43.544 ns 43.572 ns]
                        thrpt:  [22.950 Melem/s 22.965 Melem/s 22.979 Melem/s]
                 change:
                        time:   [−0.2435% −0.1256% −0.0090%] (p = 0.04 < 0.05)
                        thrpt:  [+0.0090% +0.1258% +0.2441%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking shard/ingest_raw/65536: Collecting 100 samples in estimated 5.0000 s (108M iterashard/ingest_raw/65536  time:   [46.021 ns 46.076 ns 46.122 ns]
                        thrpt:  [21.682 Melem/s 21.703 Melem/s 21.729 Melem/s]
                 change:
                        time:   [−1.2228% −0.0023% +1.2019%] (p = 1.00 > 0.05)
                        thrpt:  [−1.1876% +0.0023% +1.2380%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) low severe
  5 (5.00%) low mild
Benchmarking shard/ingest_raw_pop/65536: Collecting 100 samples in estimated 5.0001 s (115M ishard/ingest_raw_pop/65536
                        time:   [43.641 ns 43.719 ns 43.854 ns]
                        thrpt:  [22.803 Melem/s 22.873 Melem/s 22.914 Melem/s]
                 change:
                        time:   [−0.4335% −0.0474% +0.3310%] (p = 0.81 > 0.05)
                        thrpt:  [−0.3299% +0.0475% +0.4354%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
Benchmarking shard/ingest_raw/1048576: Collecting 100 samples in estimated 5.0002 s (110M iteshard/ingest_raw/1048576
                        time:   [38.779 ns 39.307 ns 39.764 ns]
                        thrpt:  [25.148 Melem/s 25.441 Melem/s 25.787 Melem/s]
                 change:
                        time:   [−1.5140% +0.1905% +2.0640%] (p = 0.84 > 0.05)
                        thrpt:  [−2.0223% −0.1901% +1.5373%]
                        No change in performance detected.
Benchmarking shard/ingest_raw_pop/1048576: Collecting 100 samples in estimated 5.0002 s (114Mshard/ingest_raw_pop/1048576
                        time:   [44.940 ns 45.020 ns 45.116 ns]
                        thrpt:  [22.165 Melem/s 22.213 Melem/s 22.252 Melem/s]
                 change:
                        time:   [−1.2112% −0.5220% +0.0787%] (p = 0.13 > 0.05)
                        thrpt:  [−0.0786% +0.5248% +1.2261%]
                        No change in performance detected.
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe

timestamp/next          time:   [7.4969 ns 7.5019 ns 7.5070 ns]
                        thrpt:  [133.21 Melem/s 133.30 Melem/s 133.39 Melem/s]
                 change:
                        time:   [−0.2583% −0.0908% +0.0761%] (p = 0.29 > 0.05)
                        thrpt:  [−0.0760% +0.0908% +0.2590%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking timestamp/now_raw: Collecting 100 samples in estimated 5.0000 s (8.0B iterationstimestamp/now_raw       time:   [622.82 ps 623.11 ps 623.43 ps]
                        thrpt:  [1.6040 Gelem/s 1.6049 Gelem/s 1.6056 Gelem/s]
                 change:
                        time:   [−0.1367% +0.0104% +0.1825%] (p = 0.90 > 0.05)
                        thrpt:  [−0.1821% −0.0104% +0.1369%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high severe

Benchmarking event/internal_event_new: Collecting 100 samples in estimated 5.0004 s (28M iterevent/internal_event_new
                        time:   [175.19 ns 175.36 ns 175.53 ns]
                        thrpt:  [5.6971 Melem/s 5.7026 Melem/s 5.7081 Melem/s]
                 change:
                        time:   [−0.5369% −0.3361% −0.1138%] (p = 0.00 < 0.05)
                        thrpt:  [+0.1139% +0.3372% +0.5398%]
                        Change within noise threshold.
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking event/json_creation: Collecting 100 samples in estimated 5.0004 s (46M iterationevent/json_creation     time:   [107.01 ns 107.18 ns 107.46 ns]
                        thrpt:  [9.3061 Melem/s 9.3299 Melem/s 9.3446 Melem/s]
                 change:
                        time:   [−0.0504% +0.1180% +0.3010%] (p = 0.20 > 0.05)
                        thrpt:  [−0.3000% −0.1178% +0.0504%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe

Benchmarking batch/pop_batch_steady_state/100: Collecting 100 samples in estimated 5.0141 s (batch/pop_batch_steady_state/100
                        time:   [3.8294 µs 3.8324 µs 3.8358 µs]
                        thrpt:  [26.070 Melem/s 26.093 Melem/s 26.114 Melem/s]
                 change:
                        time:   [−0.1339% +0.1104% +0.4010%] (p = 0.46 > 0.05)
                        thrpt:  [−0.3994% −0.1103% +0.1341%]
                        No change in performance detected.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking batch/pop_batch_steady_state/1000: Collecting 100 samples in estimated 5.0291 s batch/pop_batch_steady_state/1000
                        time:   [38.174 µs 38.192 µs 38.212 µs]
                        thrpt:  [26.170 Melem/s 26.183 Melem/s 26.196 Melem/s]
                 change:
                        time:   [−0.2593% −0.1267% +0.0142%] (p = 0.07 > 0.05)
                        thrpt:  [−0.0142% +0.1268% +0.2600%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
Benchmarking batch/pop_batch_steady_state/10000: Collecting 100 samples in estimated 5.9227 sbatch/pop_batch_steady_state/10000
                        time:   [386.54 µs 389.22 µs 392.93 µs]
                        thrpt:  [25.450 Melem/s 25.692 Melem/s 25.870 Melem/s]
                 change:
                        time:   [+0.7966% +1.1506% +1.5549%] (p = 0.00 < 0.05)
                        thrpt:  [−1.5311% −1.1375% −0.7903%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

     Running benches/mesh.rs (target/release/deps/mesh-ef6612fd04c7b907)
Gnuplot not found, using plotters backend
Benchmarking mesh_reroute/triangle_failure: Collecting 100 samples in estimated 5.0086 s (1.0mesh_reroute/triangle_failure
                        time:   [5.1598 µs 5.2303 µs 5.3007 µs]
                        thrpt:  [188.65 Kelem/s 191.19 Kelem/s 193.81 Kelem/s]
                 change:
                        time:   [+0.5771% +1.8945% +3.3456%] (p = 0.01 < 0.05)
                        thrpt:  [−3.2373% −1.8593% −0.5737%]
                        Change within noise threshold.
Benchmarking mesh_reroute/10_peers_10_routes: Collecting 100 samples in estimated 5.1420 s (1mesh_reroute/10_peers_10_routes
                        time:   [29.191 µs 29.441 µs 29.713 µs]
                        thrpt:  [33.655 Kelem/s 33.966 Kelem/s 34.257 Kelem/s]
                 change:
                        time:   [+2.4173% +3.2513% +4.1676%] (p = 0.00 < 0.05)
                        thrpt:  [−4.0008% −3.1489% −2.3603%]
                        Performance has regressed.
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_reroute/50_peers_100_routes: Collecting 100 samples in estimated 6.5691 s (mesh_reroute/50_peers_100_routes
                        time:   [317.89 µs 318.93 µs 320.02 µs]
                        thrpt:  [3.1248 Kelem/s 3.1354 Kelem/s 3.1457 Kelem/s]
                 change:
                        time:   [+2.5958% +3.0184% +3.4717%] (p = 0.00 < 0.05)
                        thrpt:  [−3.3552% −2.9300% −2.5302%]
                        Performance has regressed.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mesh_proximity/on_pingwave_new: Collecting 100 samples in estimated 5.0009 s (7.mesh_proximity/on_pingwave_new
                        time:   [181.26 ns 188.05 ns 195.19 ns]
                        thrpt:  [5.1233 Melem/s 5.3177 Melem/s 5.5171 Melem/s]
                 change:
                        time:   [−5.7044% +0.3818% +6.1624%] (p = 0.90 > 0.05)
                        thrpt:  [−5.8047% −0.3803% +6.0495%]
                        No change in performance detected.
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_proximity/on_pingwave_dedup: Collecting 100 samples in estimated 5.0003 s (mesh_proximity/on_pingwave_dedup
                        time:   [69.004 ns 69.161 ns 69.313 ns]
                        thrpt:  [14.427 Melem/s 14.459 Melem/s 14.492 Melem/s]
                 change:
                        time:   [+0.0847% +0.5374% +1.1685%] (p = 0.03 < 0.05)
                        thrpt:  [−1.1550% −0.5345% −0.0846%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking mesh_proximity/pingwave_serialize: Collecting 100 samples in estimated 5.0000 s mesh_proximity/pingwave_serialize
                        time:   [1.9753 ns 1.9763 ns 1.9774 ns]
                        thrpt:  [505.72 Melem/s 505.99 Melem/s 506.25 Melem/s]
                 change:
                        time:   [−0.1049% +0.0249% +0.1711%] (p = 0.75 > 0.05)
                        thrpt:  [−0.1708% −0.0249% +0.1050%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking mesh_proximity/pingwave_deserialize: Collecting 100 samples in estimated 5.0000 mesh_proximity/pingwave_deserialize
                        time:   [2.2496 ns 2.2529 ns 2.2569 ns]
                        thrpt:  [443.10 Melem/s 443.87 Melem/s 444.53 Melem/s]
                 change:
                        time:   [−0.3454% −0.0837% +0.1458%] (p = 0.53 > 0.05)
                        thrpt:  [−0.1456% +0.0838% +0.3466%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking mesh_proximity/node_count: Collecting 100 samples in estimated 5.0001 s (25M itemesh_proximity/node_count
                        time:   [200.17 ns 200.32 ns 200.50 ns]
                        thrpt:  [4.9875 Melem/s 4.9919 Melem/s 4.9957 Melem/s]
                 change:
                        time:   [+0.4051% +0.5574% +0.7214%] (p = 0.00 < 0.05)
                        thrpt:  [−0.7163% −0.5543% −0.4035%]
                        Change within noise threshold.
Benchmarking mesh_proximity/all_nodes_100: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 25.1s, or reduce sample count to 10.
Benchmarking mesh_proximity/all_nodes_100: Collecting 100 samples in estimated 25.138 s (100 mesh_proximity/all_nodes_100
                        time:   [252.40 ms 252.91 ms 253.51 ms]
                        thrpt:  [3.9447  elem/s 3.9540  elem/s 3.9620  elem/s]
                 change:
                        time:   [−0.8752% −0.5876% −0.2744%] (p = 0.00 < 0.05)
                        thrpt:  [+0.2751% +0.5911% +0.8830%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking mesh_dispatch/classify_direct: Collecting 100 samples in estimated 5.0000 s (7.9mesh_dispatch/classify_direct
                        time:   [639.68 ps 640.05 ps 640.41 ps]
                        thrpt:  [1.5615 Gelem/s 1.5624 Gelem/s 1.5633 Gelem/s]
                 change:
                        time:   [+2.5472% +2.6844% +2.8051%] (p = 0.00 < 0.05)
                        thrpt:  [−2.7286% −2.6142% −2.4840%]
                        Performance has regressed.
Found 18 outliers among 100 measurements (18.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe
Benchmarking mesh_dispatch/classify_routed: Collecting 100 samples in estimated 5.0000 s (11Bmesh_dispatch/classify_routed
                        time:   [455.66 ps 455.94 ps 456.19 ps]
                        thrpt:  [2.1921 Gelem/s 2.1933 Gelem/s 2.1946 Gelem/s]
                 change:
                        time:   [+2.5707% +2.7114% +2.8591%] (p = 0.00 < 0.05)
                        thrpt:  [−2.7797% −2.6399% −2.5063%]
                        Performance has regressed.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking mesh_dispatch/classify_pingwave: Collecting 100 samples in estimated 5.0000 s (1mesh_dispatch/classify_pingwave
                        time:   [312.78 ps 313.17 ps 313.61 ps]
                        thrpt:  [3.1886 Gelem/s 3.1931 Gelem/s 3.1972 Gelem/s]
                 change:
                        time:   [−0.7551% −0.4631% −0.0904%] (p = 0.00 < 0.05)
                        thrpt:  [+0.0905% +0.4652% +0.7609%]
                        Change within noise threshold.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

Benchmarking mesh_routing/lookup_hit: Collecting 100 samples in estimated 5.0000 s (329M itermesh_routing/lookup_hit time:   [14.881 ns 14.942 ns 14.989 ns]
                        thrpt:  [66.715 Melem/s 66.926 Melem/s 67.201 Melem/s]
                 change:
                        time:   [+0.1280% +0.9955% +2.0092%] (p = 0.04 < 0.05)
                        thrpt:  [−1.9696% −0.9856% −0.1278%]
                        Change within noise threshold.
Found 32 outliers among 100 measurements (32.00%)
  7 (7.00%) low severe
  8 (8.00%) low mild
  13 (13.00%) high mild
  4 (4.00%) high severe
Benchmarking mesh_routing/lookup_miss: Collecting 100 samples in estimated 5.0001 s (312M itemesh_routing/lookup_miss
                        time:   [16.018 ns 16.141 ns 16.248 ns]
                        thrpt:  [61.546 Melem/s 61.955 Melem/s 62.431 Melem/s]
                 change:
                        time:   [+7.0254% +8.0780% +9.1648%] (p = 0.00 < 0.05)
                        thrpt:  [−8.3954% −7.4742% −6.5642%]
                        Performance has regressed.
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) low severe
  4 (4.00%) low mild
Benchmarking mesh_routing/is_local: Collecting 100 samples in estimated 5.0000 s (16B iteratimesh_routing/is_local   time:   [313.83 ps 315.24 ps 316.79 ps]
                        thrpt:  [3.1566 Gelem/s 3.1722 Gelem/s 3.1865 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking mesh_routing/all_routes/10: Collecting 100 samples in estimated 5.0044 s (3.8M imesh_routing/all_routes/10
                        time:   [1.3220 µs 1.3229 µs 1.3238 µs]
                        thrpt:  [755.39 Kelem/s 755.94 Kelem/s 756.45 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking mesh_routing/all_routes/100: Collecting 100 samples in estimated 5.0037 s (2.3M mesh_routing/all_routes/100
                        time:   [2.1825 µs 2.1913 µs 2.2003 µs]
                        thrpt:  [454.48 Kelem/s 456.35 Kelem/s 458.18 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking mesh_routing/all_routes/1000: Collecting 100 samples in estimated 5.0297 s (424kmesh_routing/all_routes/1000
                        time:   [11.813 µs 11.891 µs 11.973 µs]
                        thrpt:  [83.521 Kelem/s 84.097 Kelem/s 84.652 Kelem/s]
Benchmarking mesh_routing/add_route: Collecting 100 samples in estimated 5.0000 s (116M iteramesh_routing/add_route  time:   [43.685 ns 44.336 ns 44.938 ns]
                        thrpt:  [22.253 Melem/s 22.555 Melem/s 22.891 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low severe
  4 (4.00%) low mild

     Running benches/net.rs (target/release/deps/net-d1da8b9a8227075a)
Gnuplot not found, using plotters backend
Benchmarking net_header/serialize: Collecting 100 samples in estimated 5.0000 s (2.3B iteratinet_header/serialize    time:   [2.1972 ns 2.1983 ns 2.1995 ns]
                        thrpt:  [454.66 Melem/s 454.90 Melem/s 455.12 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
Benchmarking net_header/deserialize: Collecting 100 samples in estimated 5.0000 s (2.1B iteranet_header/deserialize  time:   [2.3560 ns 2.3571 ns 2.3584 ns]
                        thrpt:  [424.02 Melem/s 424.24 Melem/s 424.44 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
Benchmarking net_header/roundtrip: Collecting 100 samples in estimated 5.0000 s (2.1B iteratinet_header/roundtrip    time:   [2.3563 ns 2.3572 ns 2.3583 ns]
                        thrpt:  [424.04 Melem/s 424.23 Melem/s 424.40 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

Benchmarking net_event_frame/write_single/64: Collecting 100 samples in estimated 5.0001 s (2net_event_frame/write_single/64
                        time:   [18.286 ns 18.319 ns 18.353 ns]
                        thrpt:  [3.2477 GiB/s 3.2538 GiB/s 3.2595 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  7 (7.00%) high mild
Benchmarking net_event_frame/write_single/256: Collecting 100 samples in estimated 5.0001 s (net_event_frame/write_single/256
                        time:   [47.963 ns 48.477 ns 48.993 ns]
                        thrpt:  [4.8664 GiB/s 4.9182 GiB/s 4.9709 GiB/s]
Benchmarking net_event_frame/write_single/1024: Collecting 100 samples in estimated 5.0000 s net_event_frame/write_single/1024
                        time:   [35.849 ns 35.863 ns 35.879 ns]
                        thrpt:  [26.580 GiB/s 26.592 GiB/s 26.603 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
Benchmarking net_event_frame/write_single/4096: Collecting 100 samples in estimated 5.0004 s net_event_frame/write_single/4096
                        time:   [84.133 ns 84.628 ns 85.164 ns]
                        thrpt:  [44.792 GiB/s 45.076 GiB/s 45.341 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) high mild
  2 (2.00%) high severe
Benchmarking net_event_frame/write_batch/1: Collecting 100 samples in estimated 5.0001 s (275net_event_frame/write_batch/1
                        time:   [18.180 ns 18.228 ns 18.274 ns]
                        thrpt:  [3.2617 GiB/s 3.2700 GiB/s 3.2786 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild
Benchmarking net_event_frame/write_batch/10: Collecting 100 samples in estimated 5.0002 s (70net_event_frame/write_batch/10
                        time:   [69.424 ns 69.577 ns 69.714 ns]
                        thrpt:  [8.5499 GiB/s 8.5667 GiB/s 8.5855 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking net_event_frame/write_batch/50: Collecting 100 samples in estimated 5.0006 s (34net_event_frame/write_batch/50
                        time:   [147.54 ns 147.65 ns 147.76 ns]
                        thrpt:  [20.169 GiB/s 20.184 GiB/s 20.199 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking net_event_frame/write_batch/100: Collecting 100 samples in estimated 5.0004 s (1net_event_frame/write_batch/100
                        time:   [272.27 ns 272.41 ns 272.56 ns]
                        thrpt:  [21.869 GiB/s 21.881 GiB/s 21.892 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking net_event_frame/read_batch_10: Collecting 100 samples in estimated 5.0001 s (35Mnet_event_frame/read_batch_10
                        time:   [137.52 ns 138.79 ns 140.05 ns]
                        thrpt:  [71.402 Melem/s 72.050 Melem/s 72.716 Melem/s]

Benchmarking net_packet_pool/get_return/16: Collecting 100 samples in estimated 5.0001 s (99Mnet_packet_pool/get_return/16
                        time:   [50.781 ns 50.879 ns 50.983 ns]
                        thrpt:  [19.614 Melem/s 19.654 Melem/s 19.693 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking net_packet_pool/get_return/64: Collecting 100 samples in estimated 5.0000 s (100net_packet_pool/get_return/64
                        time:   [50.036 ns 50.131 ns 50.230 ns]
                        thrpt:  [19.909 Melem/s 19.948 Melem/s 19.986 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking net_packet_pool/get_return/256: Collecting 100 samples in estimated 5.0000 s (10net_packet_pool/get_return/256
                        time:   [50.256 ns 50.436 ns 50.643 ns]
                        thrpt:  [19.746 Melem/s 19.827 Melem/s 19.898 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking net_packet_build/build_packet/1: Collecting 100 samples in estimated 5.0019 s (1net_packet_build/build_packet/1
                        time:   [498.48 ns 500.80 ns 503.33 ns]
                        thrpt:  [121.26 MiB/s 121.88 MiB/s 122.44 MiB/s]
Found 18 outliers among 100 measurements (18.00%)
  18 (18.00%) high severe
Benchmarking net_packet_build/build_packet/10: Collecting 100 samples in estimated 5.0031 s (net_packet_build/build_packet/10
                        time:   [1.8550 µs 1.8579 µs 1.8609 µs]
                        thrpt:  [327.99 MiB/s 328.52 MiB/s 329.03 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking net_packet_build/build_packet/50: Collecting 100 samples in estimated 5.0058 s (net_packet_build/build_packet/50
                        time:   [8.1924 µs 8.1989 µs 8.2056 µs]
                        thrpt:  [371.91 MiB/s 372.21 MiB/s 372.51 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

Benchmarking net_encryption/encrypt/64: Collecting 100 samples in estimated 5.0010 s (10M itenet_encryption/encrypt/64
                        time:   [499.31 ns 502.39 ns 506.05 ns]
                        thrpt:  [120.61 MiB/s 121.49 MiB/s 122.24 MiB/s]
Found 21 outliers among 100 measurements (21.00%)
  2 (2.00%) high mild
  19 (19.00%) high severe
Benchmarking net_encryption/encrypt/256: Collecting 100 samples in estimated 5.0023 s (5.4M inet_encryption/encrypt/256
                        time:   [937.78 ns 940.89 ns 944.29 ns]
                        thrpt:  [258.54 MiB/s 259.48 MiB/s 260.34 MiB/s]
Found 14 outliers among 100 measurements (14.00%)
  12 (12.00%) high mild
  2 (2.00%) high severe
Benchmarking net_encryption/encrypt/1024: Collecting 100 samples in estimated 5.0099 s (1.9M net_encryption/encrypt/1024
                        time:   [2.6998 µs 2.7027 µs 2.7059 µs]
                        thrpt:  [360.90 MiB/s 361.33 MiB/s 361.72 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking net_encryption/encrypt/4096: Collecting 100 samples in estimated 5.0353 s (515k net_encryption/encrypt/4096
                        time:   [9.7435 µs 9.7492 µs 9.7553 µs]
                        thrpt:  [400.42 MiB/s 400.67 MiB/s 400.91 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking net_keypair/generate: Collecting 100 samples in estimated 5.0611 s (409k iteratinet_keypair/generate    time:   [12.966 µs 13.116 µs 13.246 µs]
                        thrpt:  [75.496 Kelem/s 76.243 Kelem/s 77.123 Kelem/s]

net_aad/generate        time:   [1.8669 ns 1.8677 ns 1.8685 ns]
                        thrpt:  [535.18 Melem/s 535.41 Melem/s 535.65 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking pool_comparison/shared_pool_get_return: Collecting 100 samples in estimated 5.00pool_comparison/shared_pool_get_return
                        time:   [50.299 ns 50.385 ns 50.470 ns]
                        thrpt:  [19.814 Melem/s 19.847 Melem/s 19.881 Melem/s]
Benchmarking pool_comparison/thread_local_pool_get_return: Collecting 100 samples in estimatepool_comparison/thread_local_pool_get_return
                        time:   [101.41 ns 103.57 ns 105.56 ns]
                        thrpt:  [9.4732 Melem/s 9.6557 Melem/s 9.8614 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  2 (2.00%) high mild
  16 (16.00%) high severe
Benchmarking pool_comparison/shared_pool_10x: Collecting 100 samples in estimated 5.0008 s (1pool_comparison/shared_pool_10x
                        time:   [469.36 ns 469.58 ns 469.81 ns]
                        thrpt:  [2.1285 Melem/s 2.1296 Melem/s 2.1306 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking pool_comparison/thread_local_pool_10x: Collecting 100 samples in estimated 5.005pool_comparison/thread_local_pool_10x
                        time:   [1.2121 µs 1.2179 µs 1.2246 µs]
                        thrpt:  [816.61 Kelem/s 821.11 Kelem/s 825.03 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking cipher_comparison/shared_pool/64: Collecting 100 samples in estimated 5.0022 s (cipher_comparison/shared_pool/64
                        time:   [499.60 ns 501.97 ns 504.59 ns]
                        thrpt:  [120.96 MiB/s 121.59 MiB/s 122.17 MiB/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/64: Collecting 100 samples in estimated 5.0002 scipher_comparison/fast_chacha20/64
                        time:   [547.91 ns 548.37 ns 548.89 ns]
                        thrpt:  [111.20 MiB/s 111.30 MiB/s 111.40 MiB/s]
Found 19 outliers among 100 measurements (19.00%)
  2 (2.00%) high mild
  17 (17.00%) high severe
Benchmarking cipher_comparison/shared_pool/256: Collecting 100 samples in estimated 5.0013 s cipher_comparison/shared_pool/256
                        time:   [933.98 ns 936.61 ns 939.45 ns]
                        thrpt:  [259.88 MiB/s 260.67 MiB/s 261.40 MiB/s]
Found 11 outliers among 100 measurements (11.00%)
  9 (9.00%) high mild
  2 (2.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/256: Collecting 100 samples in estimated 5.0043 cipher_comparison/fast_chacha20/256
                        time:   [975.58 ns 976.14 ns 976.79 ns]
                        thrpt:  [249.94 MiB/s 250.11 MiB/s 250.25 MiB/s]
Found 18 outliers among 100 measurements (18.00%)
  10 (10.00%) high mild
  8 (8.00%) high severe
Benchmarking cipher_comparison/shared_pool/1024: Collecting 100 samples in estimated 5.0007 scipher_comparison/shared_pool/1024
                        time:   [2.7143 µs 2.7176 µs 2.7212 µs]
                        thrpt:  [358.88 MiB/s 359.35 MiB/s 359.79 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/1024: Collecting 100 samples in estimated 5.0028cipher_comparison/fast_chacha20/1024
                        time:   [2.7363 µs 2.7412 µs 2.7460 µs]
                        thrpt:  [355.63 MiB/s 356.26 MiB/s 356.89 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking cipher_comparison/shared_pool/4096: Collecting 100 samples in estimated 5.0453 scipher_comparison/shared_pool/4096
                        time:   [9.7459 µs 9.7511 µs 9.7568 µs]
                        thrpt:  [400.36 MiB/s 400.60 MiB/s 400.81 MiB/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/4096: Collecting 100 samples in estimated 5.0189cipher_comparison/fast_chacha20/4096
                        time:   [9.7410 µs 9.7487 µs 9.7572 µs]
                        thrpt:  [400.35 MiB/s 400.69 MiB/s 401.01 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking adaptive_batcher/optimal_size: Collecting 100 samples in estimated 5.0000 s (5.1adaptive_batcher/optimal_size
                        time:   [984.17 ps 987.69 ps 990.70 ps]
                        thrpt:  [1.0094 Gelem/s 1.0125 Gelem/s 1.0161 Gelem/s]
Benchmarking adaptive_batcher/record: Collecting 100 samples in estimated 5.0000 s (1.3B iteradaptive_batcher/record time:   [3.8656 ns 3.8671 ns 3.8688 ns]
                        thrpt:  [258.48 Melem/s 258.59 Melem/s 258.69 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking adaptive_batcher/full_cycle: Collecting 100 samples in estimated 5.0000 s (1.1B adaptive_batcher/full_cycle
                        time:   [4.3931 ns 4.4107 ns 4.4318 ns]
                        thrpt:  [225.64 Melem/s 226.72 Melem/s 227.63 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

Benchmarking e2e_packet_build/shared_pool_50_events: Collecting 100 samples in estimated 5.01e2e_packet_build/shared_pool_50_events
                        time:   [8.2288 µs 8.2510 µs 8.2767 µs]
                        thrpt:  [368.72 MiB/s 369.87 MiB/s 370.86 MiB/s]
Found 15 outliers among 100 measurements (15.00%)
  12 (12.00%) high mild
  3 (3.00%) high severe
Benchmarking e2e_packet_build/fast_50_events: Collecting 100 samples in estimated 5.0108 s (6e2e_packet_build/fast_50_events
                        time:   [8.1948 µs 8.1980 µs 8.2015 µs]
                        thrpt:  [372.10 MiB/s 372.26 MiB/s 372.40 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.4s, enable flat sampling, or reduce sample count to 50.
Benchmarking multithread_packet_build/shared_pool/8: Collecting 100 samples in estimated 9.44multithread_packet_build/shared_pool/8
                        time:   [1.8647 ms 1.8675 ms 1.8703 ms]
                        thrpt:  [4.2774 Melem/s 4.2839 Melem/s 4.2902 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/8: Collecting 100 samples in estimatemultithread_packet_build/thread_local_pool/8
                        time:   [909.26 µs 916.24 µs 923.05 µs]
                        thrpt:  [8.6669 Melem/s 8.7313 Melem/s 8.7984 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_packet_build/shared_pool/16: Collecting 100 samples in estimated 5.4multithread_packet_build/shared_pool/16
                        time:   [4.5724 ms 4.6544 ms 4.7393 ms]
                        thrpt:  [3.3760 Melem/s 3.4376 Melem/s 3.4993 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking multithread_packet_build/thread_local_pool/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.8s, enable flat sampling, or reduce sample count to 50.
Benchmarking multithread_packet_build/thread_local_pool/16: Collecting 100 samples in estimatmultithread_packet_build/thread_local_pool/16
                        time:   [1.7386 ms 1.7455 ms 1.7546 ms]
                        thrpt:  [9.1191 Melem/s 9.1664 Melem/s 9.2029 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) high mild
  10 (10.00%) high severe
Benchmarking multithread_packet_build/shared_pool/24: Collecting 100 samples in estimated 5.3multithread_packet_build/shared_pool/24
                        time:   [7.1754 ms 7.3716 ms 7.5844 ms]
                        thrpt:  [3.1644 Melem/s 3.2557 Melem/s 3.3448 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/24: Collecting 100 samples in estimatmultithread_packet_build/thread_local_pool/24
                        time:   [2.5366 ms 2.5401 ms 2.5439 ms]
                        thrpt:  [9.4345 Melem/s 9.4484 Melem/s 9.4615 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking multithread_packet_build/shared_pool/32: Collecting 100 samples in estimated 5.1multithread_packet_build/shared_pool/32
                        time:   [10.101 ms 10.478 ms 10.883 ms]
                        thrpt:  [2.9405 Melem/s 3.0541 Melem/s 3.1679 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  10 (10.00%) high mild
Benchmarking multithread_packet_build/thread_local_pool/32: Collecting 100 samples in estimatmultithread_packet_build/thread_local_pool/32
                        time:   [3.3382 ms 3.3456 ms 3.3555 ms]
                        thrpt:  [9.5365 Melem/s 9.5648 Melem/s 9.5861 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, enable flat sampling, or reduce sample count to 50.
Benchmarking multithread_mixed_frames/shared_mixed/8: Collecting 100 samples in estimated 7.1multithread_mixed_frames/shared_mixed/8
                        time:   [1.4163 ms 1.4183 ms 1.4205 ms]
                        thrpt:  [8.4476 Melem/s 8.4607 Melem/s 8.4730 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.5s, enable flat sampling, or reduce sample count to 60.
Benchmarking multithread_mixed_frames/fast_mixed/8: Collecting 100 samples in estimated 5.504multithread_mixed_frames/fast_mixed/8
                        time:   [1.0566 ms 1.0594 ms 1.0623 ms]
                        thrpt:  [11.296 Melem/s 11.327 Melem/s 11.357 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking multithread_mixed_frames/shared_mixed/16: Collecting 100 samples in estimated 5.multithread_mixed_frames/shared_mixed/16
                        time:   [3.0706 ms 3.1186 ms 3.1723 ms]
                        thrpt:  [7.5654 Melem/s 7.6957 Melem/s 7.8162 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/16: Collecting 100 samples in estimated 5.13multithread_mixed_frames/fast_mixed/16
                        time:   [2.0498 ms 2.0537 ms 2.0579 ms]
                        thrpt:  [11.663 Melem/s 11.686 Melem/s 11.708 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking multithread_mixed_frames/shared_mixed/24: Collecting 100 samples in estimated 5.multithread_mixed_frames/shared_mixed/24
                        time:   [4.6461 ms 4.7623 ms 4.8924 ms]
                        thrpt:  [7.3584 Melem/s 7.5594 Melem/s 7.7484 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/24: Collecting 100 samples in estimated 5.13multithread_mixed_frames/fast_mixed/24
                        time:   [3.0152 ms 3.0198 ms 3.0244 ms]
                        thrpt:  [11.903 Melem/s 11.921 Melem/s 11.939 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
Benchmarking multithread_mixed_frames/shared_mixed/32: Collecting 100 samples in estimated 5.multithread_mixed_frames/shared_mixed/32
                        time:   [6.2736 ms 6.4955 ms 6.7353 ms]
                        thrpt:  [7.1266 Melem/s 7.3897 Melem/s 7.6511 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/32: Collecting 100 samples in estimated 5.18multithread_mixed_frames/fast_mixed/32
                        time:   [4.2617 ms 4.3007 ms 4.3380 ms]
                        thrpt:  [11.065 Melem/s 11.161 Melem/s 11.263 Melem/s]

Benchmarking pool_contention/shared_acquire_release/8: Collecting 100 samples in estimated 6.pool_contention/shared_acquire_release/8
                        time:   [20.817 ms 20.881 ms 20.947 ms]
                        thrpt:  [3.8192 Melem/s 3.8312 Melem/s 3.8431 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
Benchmarking pool_contention/fast_acquire_release/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.3s, enable flat sampling, or reduce sample count to 50.
Benchmarking pool_contention/fast_acquire_release/8: Collecting 100 samples in estimated 9.33pool_contention/fast_acquire_release/8
                        time:   [1.3606 ms 1.4019 ms 1.4550 ms]
                        thrpt:  [54.983 Melem/s 57.064 Melem/s 58.798 Melem/s]
Benchmarking pool_contention/shared_acquire_release/16: Collecting 100 samples in estimated 9pool_contention/shared_acquire_release/16
                        time:   [48.372 ms 48.872 ms 49.392 ms]
                        thrpt:  [3.2394 Melem/s 3.2738 Melem/s 3.3077 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking pool_contention/fast_acquire_release/16: Collecting 100 samples in estimated 5.0pool_contention/fast_acquire_release/16
                        time:   [2.5157 ms 2.5251 ms 2.5347 ms]
                        thrpt:  [63.123 Melem/s 63.363 Melem/s 63.600 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking pool_contention/shared_acquire_release/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.6s, or reduce sample count to 70.
Benchmarking pool_contention/shared_acquire_release/24: Collecting 100 samples in estimated 6pool_contention/shared_acquire_release/24
                        time:   [67.499 ms 68.778 ms 70.062 ms]
                        thrpt:  [3.4256 Melem/s 3.4895 Melem/s 3.5556 Melem/s]
Benchmarking pool_contention/fast_acquire_release/24: Collecting 100 samples in estimated 5.0pool_contention/fast_acquire_release/24
                        time:   [3.6291 ms 3.6429 ms 3.6572 ms]
                        thrpt:  [65.624 Melem/s 65.882 Melem/s 66.132 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking pool_contention/shared_acquire_release/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.0s, or reduce sample count to 60.
Benchmarking pool_contention/shared_acquire_release/32: Collecting 100 samples in estimated 8pool_contention/shared_acquire_release/32
                        time:   [81.629 ms 83.510 ms 85.515 ms]
                        thrpt:  [3.7420 Melem/s 3.8319 Melem/s 3.9202 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  9 (9.00%) high mild
  1 (1.00%) high severe
Benchmarking pool_contention/fast_acquire_release/32: Collecting 100 samples in estimated 5.2pool_contention/fast_acquire_release/32
                        time:   [4.8025 ms 4.8448 ms 4.8965 ms]
                        thrpt:  [65.353 Melem/s 66.050 Melem/s 66.632 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high severe

Benchmarking throughput_scaling/fast_pool_scaling/1: Collecting 20 samples in estimated 5.695throughput_scaling/fast_pool_scaling/1
                        time:   [6.7389 ms 6.7471 ms 6.7598 ms]
                        thrpt:  [295.87 Kelem/s 296.42 Kelem/s 296.78 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) high mild
  1 (5.00%) high severe
Benchmarking throughput_scaling/fast_pool_scaling/2: Collecting 20 samples in estimated 5.881throughput_scaling/fast_pool_scaling/2
                        time:   [6.9864 ms 6.9895 ms 6.9931 ms]
                        thrpt:  [572.00 Kelem/s 572.29 Kelem/s 572.54 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking throughput_scaling/fast_pool_scaling/4: Collecting 20 samples in estimated 6.255throughput_scaling/fast_pool_scaling/4
                        time:   [7.4261 ms 7.4355 ms 7.4456 ms]
                        thrpt:  [1.0745 Melem/s 1.0759 Melem/s 1.0773 Melem/s]
Benchmarking throughput_scaling/fast_pool_scaling/8: Collecting 20 samples in estimated 6.439throughput_scaling/fast_pool_scaling/8
                        time:   [7.6532 ms 7.6635 ms 7.6759 ms]
                        thrpt:  [2.0844 Melem/s 2.0878 Melem/s 2.0906 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking throughput_scaling/fast_pool_scaling/16: Collecting 20 samples in estimated 6.42throughput_scaling/fast_pool_scaling/16
                        time:   [15.250 ms 15.314 ms 15.373 ms]
                        thrpt:  [2.0816 Melem/s 2.0895 Melem/s 2.0983 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking throughput_scaling/fast_pool_scaling/24: Collecting 20 samples in estimated 9.55throughput_scaling/fast_pool_scaling/24
                        time:   [22.714 ms 22.761 ms 22.808 ms]
                        thrpt:  [2.1045 Melem/s 2.1089 Melem/s 2.1132 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking throughput_scaling/fast_pool_scaling/32: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 10.
Benchmarking throughput_scaling/fast_pool_scaling/32: Collecting 20 samples in estimated 6.24throughput_scaling/fast_pool_scaling/32
                        time:   [29.903 ms 30.094 ms 30.416 ms]
                        thrpt:  [2.1042 Melem/s 2.1266 Melem/s 2.1402 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild

Benchmarking routing_header/serialize: Collecting 100 samples in estimated 5.0000 s (8.0B iterouting_header/serialize
                        time:   [624.79 ps 625.56 ps 626.39 ps]
                        thrpt:  [1.5965 Gelem/s 1.5986 Gelem/s 1.6005 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking routing_header/deserialize: Collecting 100 samples in estimated 5.0000 s (5.4B irouting_header/deserialize
                        time:   [933.11 ps 933.71 ps 934.49 ps]
                        thrpt:  [1.0701 Gelem/s 1.0710 Gelem/s 1.0717 Gelem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking routing_header/roundtrip: Collecting 100 samples in estimated 5.0000 s (5.4B iterouting_header/roundtrip
                        time:   [932.95 ps 933.25 ps 933.56 ps]
                        thrpt:  [1.0712 Gelem/s 1.0715 Gelem/s 1.0719 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking routing_header/forward: Collecting 100 samples in estimated 5.0000 s (8.8B iterarouting_header/forward  time:   [566.30 ps 567.67 ps 569.03 ps]
                        thrpt:  [1.7574 Gelem/s 1.7616 Gelem/s 1.7658 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild

Benchmarking routing_table/lookup_hit: Collecting 100 samples in estimated 5.0001 s (136M iterouting_table/lookup_hit
                        time:   [36.237 ns 36.630 ns 37.077 ns]
                        thrpt:  [26.971 Melem/s 27.300 Melem/s 27.596 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  7 (7.00%) high mild
  8 (8.00%) high severe
Benchmarking routing_table/lookup_miss: Collecting 100 samples in estimated 5.0000 s (329M itrouting_table/lookup_miss
                        time:   [15.247 ns 15.346 ns 15.439 ns]
                        thrpt:  [64.772 Melem/s 65.165 Melem/s 65.588 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  3 (3.00%) low severe
  3 (3.00%) low mild
  8 (8.00%) high mild
  7 (7.00%) high severe
Benchmarking routing_table/is_local: Collecting 100 samples in estimated 5.0000 s (16B iteratrouting_table/is_local  time:   [313.25 ps 313.63 ps 314.05 ps]
                        thrpt:  [3.1842 Gelem/s 3.1885 Gelem/s 3.1923 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking routing_table/add_route: Collecting 100 samples in estimated 5.0011 s (22M iterarouting_table/add_route time:   [208.26 ns 213.72 ns 218.45 ns]
                        thrpt:  [4.5777 Melem/s 4.6790 Melem/s 4.8018 Melem/s]
Benchmarking routing_table/record_in: Collecting 100 samples in estimated 5.0001 s (92M iterarouting_table/record_in time:   [54.479 ns 54.688 ns 54.987 ns]
                        thrpt:  [18.186 Melem/s 18.286 Melem/s 18.356 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking routing_table/record_out: Collecting 100 samples in estimated 5.0001 s (124M iterouting_table/record_out
                        time:   [39.790 ns 39.978 ns 40.175 ns]
                        thrpt:  [24.891 Melem/s 25.014 Melem/s 25.132 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking routing_table/aggregate_stats: Collecting 100 samples in estimated 5.0096 s (2.4routing_table/aggregate_stats
                        time:   [2.0728 µs 2.0745 µs 2.0762 µs]
                        thrpt:  [481.65 Kelem/s 482.05 Kelem/s 482.45 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

Benchmarking fair_scheduler/creation: Collecting 100 samples in estimated 5.0007 s (17M iterafair_scheduler/creation time:   [292.79 ns 304.15 ns 320.47 ns]
                        thrpt:  [3.1204 Melem/s 3.2878 Melem/s 3.4154 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
Benchmarking fair_scheduler/stream_count_empty: Collecting 100 samples in estimated 5.0004 s fair_scheduler/stream_count_empty
                        time:   [214.62 ns 223.09 ns 233.96 ns]
                        thrpt:  [4.2742 Melem/s 4.4826 Melem/s 4.6593 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
Benchmarking fair_scheduler/total_queued: Collecting 100 samples in estimated 5.0000 s (14B ifair_scheduler/total_queued
                        time:   [326.73 ps 334.91 ps 349.69 ps]
                        thrpt:  [2.8596 Gelem/s 2.9859 Gelem/s 3.0607 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
Benchmarking fair_scheduler/cleanup_empty: Collecting 100 samples in estimated 5.0004 s (24M fair_scheduler/cleanup_empty
                        time:   [212.69 ns 223.92 ns 238.19 ns]
                        thrpt:  [4.1983 Melem/s 4.4659 Melem/s 4.7017 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe

Benchmarking routing_table_concurrent/concurrent_lookup/4: Collecting 100 samples in estimaterouting_table_concurrent/concurrent_lookup/4
                        time:   [189.93 µs 193.73 µs 197.78 µs]
                        thrpt:  [20.224 Melem/s 20.647 Melem/s 21.060 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
Benchmarking routing_table_concurrent/concurrent_stats/4: Collecting 100 samples in estimatedrouting_table_concurrent/concurrent_stats/4
                        time:   [368.65 µs 371.86 µs 375.32 µs]
                        thrpt:  [10.658 Melem/s 10.757 Melem/s 10.850 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking routing_table_concurrent/concurrent_lookup/8: Collecting 100 samples in estimaterouting_table_concurrent/concurrent_lookup/8
                        time:   [272.47 µs 273.96 µs 275.68 µs]
                        thrpt:  [29.019 Melem/s 29.202 Melem/s 29.361 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  4 (4.00%) high severe
Benchmarking routing_table_concurrent/concurrent_stats/8: Collecting 100 samples in estimatedrouting_table_concurrent/concurrent_stats/8
                        time:   [600.09 µs 651.78 µs 721.84 µs]
                        thrpt:  [11.083 Melem/s 12.274 Melem/s 13.331 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) high mild
  6 (6.00%) high severe
Benchmarking routing_table_concurrent/concurrent_lookup/16: Collecting 100 samples in estimatrouting_table_concurrent/concurrent_lookup/16
                        time:   [463.09 µs 487.98 µs 521.41 µs]
                        thrpt:  [30.686 Melem/s 32.789 Melem/s 34.551 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) high mild
  6 (6.00%) high severe
Benchmarking routing_table_concurrent/concurrent_stats/16: Collecting 100 samples in estimaterouting_table_concurrent/concurrent_stats/16
                        time:   [912.45 µs 914.78 µs 916.99 µs]
                        thrpt:  [17.448 Melem/s 17.490 Melem/s 17.535 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild

Benchmarking routing_decision/parse_lookup_forward: Collecting 100 samples in estimated 5.000routing_decision/parse_lookup_forward
                        time:   [37.207 ns 37.562 ns 37.973 ns]
                        thrpt:  [26.335 Melem/s 26.622 Melem/s 26.877 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  6 (6.00%) high mild
  9 (9.00%) high severe
Benchmarking routing_decision/full_with_stats: Collecting 100 samples in estimated 5.0006 s (routing_decision/full_with_stats
                        time:   [130.35 ns 130.51 ns 130.70 ns]
                        thrpt:  [7.6509 Melem/s 7.6620 Melem/s 7.6717 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking stream_multiplexing/lookup_all/10: Collecting 100 samples in estimated 5.0002 s stream_multiplexing/lookup_all/10
                        time:   [295.09 ns 295.26 ns 295.45 ns]
                        thrpt:  [33.847 Melem/s 33.869 Melem/s 33.888 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking stream_multiplexing/stats_all/10: Collecting 100 samples in estimated 5.0002 s (stream_multiplexing/stats_all/10
                        time:   [542.87 ns 550.91 ns 558.74 ns]
                        thrpt:  [17.898 Melem/s 18.152 Melem/s 18.421 Melem/s]
Benchmarking stream_multiplexing/lookup_all/100: Collecting 100 samples in estimated 5.0092 sstream_multiplexing/lookup_all/100
                        time:   [2.9075 µs 2.9103 µs 2.9142 µs]
                        thrpt:  [34.315 Melem/s 34.361 Melem/s 34.394 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking stream_multiplexing/stats_all/100: Collecting 100 samples in estimated 5.0127 s stream_multiplexing/stats_all/100
                        time:   [5.4507 µs 5.5256 µs 5.5999 µs]
                        thrpt:  [17.858 Melem/s 18.098 Melem/s 18.346 Melem/s]
Benchmarking stream_multiplexing/lookup_all/1000: Collecting 100 samples in estimated 5.1335 stream_multiplexing/lookup_all/1000
                        time:   [29.042 µs 29.066 µs 29.098 µs]
                        thrpt:  [34.367 Melem/s 34.404 Melem/s 34.433 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high severe
Benchmarking stream_multiplexing/stats_all/1000: Collecting 100 samples in estimated 5.0503 sstream_multiplexing/stats_all/1000
                        time:   [54.456 µs 55.193 µs 55.925 µs]
                        thrpt:  [17.881 Melem/s 18.118 Melem/s 18.363 Melem/s]
Benchmarking stream_multiplexing/lookup_all/10000: Collecting 100 samples in estimated 5.9006stream_multiplexing/lookup_all/10000
                        time:   [291.71 µs 292.13 µs 292.67 µs]
                        thrpt:  [34.168 Melem/s 34.231 Melem/s 34.281 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking stream_multiplexing/stats_all/10000: Collecting 100 samples in estimated 5.8174 stream_multiplexing/stats_all/10000
                        time:   [563.54 µs 568.58 µs 573.52 µs]
                        thrpt:  [17.436 Melem/s 17.588 Melem/s 17.745 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild

Benchmarking multihop_packet_builder/build/64: Collecting 100 samples in estimated 5.0001 s (multihop_packet_builder/build/64
                        time:   [23.702 ns 23.821 ns 23.933 ns]
                        thrpt:  [2.4904 GiB/s 2.5022 GiB/s 2.5148 GiB/s]
Benchmarking multihop_packet_builder/build_priority/64: Collecting 100 samples in estimated 5multihop_packet_builder/build_priority/64
                        time:   [20.592 ns 20.606 ns 20.620 ns]
                        thrpt:  [2.8907 GiB/s 2.8926 GiB/s 2.8945 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking multihop_packet_builder/build/256: Collecting 100 samples in estimated 5.0001 s multihop_packet_builder/build/256
                        time:   [51.472 ns 52.006 ns 52.570 ns]
                        thrpt:  [4.5353 GiB/s 4.5845 GiB/s 4.6320 GiB/s]
Benchmarking multihop_packet_builder/build_priority/256: Collecting 100 samples in estimated multihop_packet_builder/build_priority/256
                        time:   [49.919 ns 50.565 ns 51.235 ns]
                        thrpt:  [4.6535 GiB/s 4.7151 GiB/s 4.7761 GiB/s]
Benchmarking multihop_packet_builder/build/1024: Collecting 100 samples in estimated 5.0002 smultihop_packet_builder/build/1024
                        time:   [40.904 ns 40.971 ns 41.061 ns]
                        thrpt:  [23.226 GiB/s 23.277 GiB/s 23.315 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
Benchmarking multihop_packet_builder/build_priority/1024: Collecting 100 samples in estimatedmultihop_packet_builder/build_priority/1024
                        time:   [38.254 ns 38.275 ns 38.297 ns]
                        thrpt:  [24.902 GiB/s 24.916 GiB/s 24.930 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking multihop_packet_builder/build/4096: Collecting 100 samples in estimated 5.0004 smultihop_packet_builder/build/4096
                        time:   [80.462 ns 80.874 ns 81.337 ns]
                        thrpt:  [46.900 GiB/s 47.168 GiB/s 47.410 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_packet_builder/build_priority/4096: Collecting 100 samples in estimatedmultihop_packet_builder/build_priority/4096
                        time:   [78.729 ns 79.025 ns 79.397 ns]
                        thrpt:  [48.046 GiB/s 48.272 GiB/s 48.454 GiB/s]
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe

Benchmarking multihop_chain/forward_chain/1: Collecting 100 samples in estimated 5.0002 s (88multihop_chain/forward_chain/1
                        time:   [56.806 ns 57.009 ns 57.237 ns]
                        thrpt:  [17.471 Melem/s 17.541 Melem/s 17.604 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_chain/forward_chain/2: Collecting 100 samples in estimated 5.0006 s (43multihop_chain/forward_chain/2
                        time:   [115.42 ns 115.86 ns 116.32 ns]
                        thrpt:  [8.5970 Melem/s 8.6308 Melem/s 8.6643 Melem/s]
Benchmarking multihop_chain/forward_chain/3: Collecting 100 samples in estimated 5.0008 s (31multihop_chain/forward_chain/3
                        time:   [158.79 ns 159.25 ns 159.73 ns]
                        thrpt:  [6.2605 Melem/s 6.2796 Melem/s 6.2976 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_chain/forward_chain/4: Collecting 100 samples in estimated 5.0005 s (22multihop_chain/forward_chain/4
                        time:   [223.11 ns 225.11 ns 227.21 ns]
                        thrpt:  [4.4013 Melem/s 4.4423 Melem/s 4.4822 Melem/s]
Benchmarking multihop_chain/forward_chain/5: Collecting 100 samples in estimated 5.0002 s (18multihop_chain/forward_chain/5
                        time:   [279.06 ns 281.59 ns 284.27 ns]
                        thrpt:  [3.5178 Melem/s 3.5513 Melem/s 3.5835 Melem/s]

Benchmarking hop_latency/single_hop_process: Collecting 100 samples in estimated 5.0000 s (3.hop_latency/single_hop_process
                        time:   [1.4529 ns 1.4536 ns 1.4543 ns]
                        thrpt:  [687.62 Melem/s 687.94 Melem/s 688.26 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking hop_latency/single_hop_full: Collecting 100 samples in estimated 5.0003 s (88M ihop_latency/single_hop_full
                        time:   [55.702 ns 56.297 ns 56.908 ns]
                        thrpt:  [17.572 Melem/s 17.763 Melem/s 17.953 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking hop_scaling/64B_1hops: Collecting 100 samples in estimated 5.0001 s (168M iterathop_scaling/64B_1hops   time:   [29.683 ns 29.746 ns 29.807 ns]
                        thrpt:  [1.9997 GiB/s 2.0038 GiB/s 2.0081 GiB/s]
Found 18 outliers among 100 measurements (18.00%)
  10 (10.00%) low mild
  7 (7.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/64B_2hops: Collecting 100 samples in estimated 5.0001 s (95M iteratihop_scaling/64B_2hops   time:   [52.520 ns 52.606 ns 52.698 ns]
                        thrpt:  [1.1311 GiB/s 1.1330 GiB/s 1.1349 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking hop_scaling/64B_3hops: Collecting 100 samples in estimated 5.0002 s (66M iteratihop_scaling/64B_3hops   time:   [76.099 ns 76.228 ns 76.361 ns]
                        thrpt:  [799.29 MiB/s 800.69 MiB/s 802.05 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking hop_scaling/64B_4hops: Collecting 100 samples in estimated 5.0005 s (50M iteratihop_scaling/64B_4hops   time:   [99.626 ns 99.820 ns 100.02 ns]
                        thrpt:  [610.21 MiB/s 611.45 MiB/s 612.64 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking hop_scaling/64B_5hops: Collecting 100 samples in estimated 5.0005 s (38M iteratihop_scaling/64B_5hops   time:   [130.06 ns 130.58 ns 131.20 ns]
                        thrpt:  [465.21 MiB/s 467.41 MiB/s 469.27 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking hop_scaling/256B_1hops: Collecting 100 samples in estimated 5.0003 s (83M iterathop_scaling/256B_1hops  time:   [59.944 ns 60.750 ns 61.557 ns]
                        thrpt:  [3.8731 GiB/s 3.9246 GiB/s 3.9773 GiB/s]
Benchmarking hop_scaling/256B_2hops: Collecting 100 samples in estimated 5.0002 s (42M iterathop_scaling/256B_2hops  time:   [118.85 ns 119.65 ns 120.44 ns]
                        thrpt:  [1.9796 GiB/s 1.9927 GiB/s 2.0061 GiB/s]
Benchmarking hop_scaling/256B_3hops: Collecting 100 samples in estimated 5.0001 s (30M iterathop_scaling/256B_3hops  time:   [167.59 ns 169.62 ns 171.68 ns]
                        thrpt:  [1.3888 GiB/s 1.4056 GiB/s 1.4226 GiB/s]
Benchmarking hop_scaling/256B_4hops: Collecting 100 samples in estimated 5.0002 s (21M iterathop_scaling/256B_4hops  time:   [231.39 ns 233.27 ns 235.20 ns]
                        thrpt:  [1.0137 GiB/s 1.0221 GiB/s 1.0304 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking hop_scaling/256B_5hops: Collecting 100 samples in estimated 5.0006 s (17M iterathop_scaling/256B_5hops  time:   [291.00 ns 293.26 ns 295.55 ns]
                        thrpt:  [826.05 MiB/s 832.50 MiB/s 838.98 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking hop_scaling/1024B_1hops: Collecting 100 samples in estimated 5.0001 s (101M iterhop_scaling/1024B_1hops time:   [47.982 ns 48.027 ns 48.067 ns]
                        thrpt:  [19.841 GiB/s 19.857 GiB/s 19.875 GiB/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  16 (16.00%) high severe
Benchmarking hop_scaling/1024B_2hops: Collecting 100 samples in estimated 5.0003 s (44M iterahop_scaling/1024B_2hops time:   [113.13 ns 113.71 ns 114.38 ns]
                        thrpt:  [8.3381 GiB/s 8.3866 GiB/s 8.4295 GiB/s]
Found 14 outliers among 100 measurements (14.00%)
  5 (5.00%) high mild
  9 (9.00%) high severe
Benchmarking hop_scaling/1024B_3hops: Collecting 100 samples in estimated 5.0005 s (32M iterahop_scaling/1024B_3hops time:   [155.87 ns 157.10 ns 158.35 ns]
                        thrpt:  [6.0226 GiB/s 6.0705 GiB/s 6.1186 GiB/s]
Benchmarking hop_scaling/1024B_4hops: Collecting 100 samples in estimated 5.0002 s (23M iterahop_scaling/1024B_4hops time:   [214.97 ns 215.75 ns 216.62 ns]
                        thrpt:  [4.4025 GiB/s 4.4203 GiB/s 4.4362 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking hop_scaling/1024B_5hops: Collecting 100 samples in estimated 5.0004 s (20M iterahop_scaling/1024B_5hops time:   [253.82 ns 255.12 ns 256.40 ns]
                        thrpt:  [3.7195 GiB/s 3.7381 GiB/s 3.7572 GiB/s]

Benchmarking multihop_with_routing/route_and_forward/1: Collecting 100 samples in estimated 5multihop_with_routing/route_and_forward/1
                        time:   [200.95 ns 201.57 ns 202.25 ns]
                        thrpt:  [4.9445 Melem/s 4.9611 Melem/s 4.9764 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_with_routing/route_and_forward/2: Collecting 100 samples in estimated 5multihop_with_routing/route_and_forward/2
                        time:   [399.48 ns 401.09 ns 402.76 ns]
                        thrpt:  [2.4829 Melem/s 2.4932 Melem/s 2.5032 Melem/s]
Benchmarking multihop_with_routing/route_and_forward/3: Collecting 100 samples in estimated 5multihop_with_routing/route_and_forward/3
                        time:   [598.12 ns 601.68 ns 605.47 ns]
                        thrpt:  [1.6516 Melem/s 1.6620 Melem/s 1.6719 Melem/s]
Benchmarking multihop_with_routing/route_and_forward/4: Collecting 100 samples in estimated 5multihop_with_routing/route_and_forward/4
                        time:   [794.49 ns 797.72 ns 801.18 ns]
                        thrpt:  [1.2482 Melem/s 1.2536 Melem/s 1.2587 Melem/s]
Benchmarking multihop_with_routing/route_and_forward/5: Collecting 100 samples in estimated 5multihop_with_routing/route_and_forward/5
                        time:   [996.15 ns 1.0008 µs 1.0057 µs]
                        thrpt:  [994.37 Kelem/s 999.23 Kelem/s 1.0039 Melem/s]

Benchmarking multihop_concurrent/concurrent_forward/4: Collecting 20 samples in estimated 5.1multihop_concurrent/concurrent_forward/4
                        time:   [692.45 µs 694.58 µs 696.74 µs]
                        thrpt:  [5.7410 Melem/s 5.7589 Melem/s 5.7766 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high mild
Benchmarking multihop_concurrent/concurrent_forward/8: Collecting 20 samples in estimated 5.1multihop_concurrent/concurrent_forward/8
                        time:   [1.4446 ms 1.4593 ms 1.4742 ms]
                        thrpt:  [5.4268 Melem/s 5.4821 Melem/s 5.5380 Melem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low severe
  1 (5.00%) low mild
  1 (5.00%) high mild
Benchmarking multihop_concurrent/concurrent_forward/16: Collecting 20 samples in estimated 5.multihop_concurrent/concurrent_forward/16
                        time:   [1.7229 ms 1.7283 ms 1.7332 ms]
                        thrpt:  [9.2314 Melem/s 9.2575 Melem/s 9.2868 Melem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high severe

Benchmarking pingwave/serialize: Collecting 100 samples in estimated 5.0000 s (6.4B iterationpingwave/serialize      time:   [778.72 ps 779.12 ps 779.54 ps]
                        thrpt:  [1.2828 Gelem/s 1.2835 Gelem/s 1.2842 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking pingwave/deserialize: Collecting 100 samples in estimated 5.0000 s (5.3B iteratipingwave/deserialize    time:   [934.54 ps 934.99 ps 935.45 ps]
                        thrpt:  [1.0690 Gelem/s 1.0695 Gelem/s 1.0700 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking pingwave/roundtrip: Collecting 100 samples in estimated 5.0000 s (5.3B iterationpingwave/roundtrip      time:   [934.44 ps 934.90 ps 935.41 ps]
                        thrpt:  [1.0691 Gelem/s 1.0696 Gelem/s 1.0702 Gelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
pingwave/forward        time:   [625.43 ps 626.20 ps 627.02 ps]
                        thrpt:  [1.5948 Gelem/s 1.5969 Gelem/s 1.5989 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking capabilities/serialize_simple: Collecting 100 samples in estimated 5.0001 s (264capabilities/serialize_simple
                        time:   [18.748 ns 18.852 ns 18.948 ns]
                        thrpt:  [52.776 Melem/s 53.044 Melem/s 53.340 Melem/s]
Benchmarking capabilities/deserialize_simple: Collecting 100 samples in estimated 5.0000 s (1capabilities/deserialize_simple
                        time:   [4.7470 ns 4.7538 ns 4.7611 ns]
                        thrpt:  [210.03 Melem/s 210.36 Melem/s 210.66 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking capabilities/serialize_complex: Collecting 100 samples in estimated 5.0001 s (12capabilities/serialize_complex
                        time:   [41.118 ns 41.139 ns 41.161 ns]
                        thrpt:  [24.295 Melem/s 24.308 Melem/s 24.320 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking capabilities/deserialize_complex: Collecting 100 samples in estimated 5.0006 s (capabilities/deserialize_complex
                        time:   [153.85 ns 153.92 ns 153.99 ns]
                        thrpt:  [6.4939 Melem/s 6.4970 Melem/s 6.5000 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking local_graph/create_pingwave: Collecting 100 samples in estimated 5.0000 s (2.4B local_graph/create_pingwave
                        time:   [2.1035 ns 2.1080 ns 2.1123 ns]
                        thrpt:  [473.43 Melem/s 474.39 Melem/s 475.39 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
Benchmarking local_graph/on_pingwave_new: Collecting 100 samples in estimated 5.0000 s (22M ilocal_graph/on_pingwave_new
                        time:   [88.704 ns 96.173 ns 102.54 ns]
                        thrpt:  [9.7522 Melem/s 10.398 Melem/s 11.273 Melem/s]
Benchmarking local_graph/on_pingwave_duplicate: Collecting 100 samples in estimated 5.0007 s local_graph/on_pingwave_duplicate
                        time:   [212.41 ns 212.59 ns 212.81 ns]
                        thrpt:  [4.6991 Melem/s 4.7039 Melem/s 4.7078 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
Benchmarking local_graph/get_node: Collecting 100 samples in estimated 5.0000 s (331M iteratilocal_graph/get_node    time:   [15.117 ns 15.147 ns 15.183 ns]
                        thrpt:  [65.864 Melem/s 66.021 Melem/s 66.152 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  10 (10.00%) high mild
  9 (9.00%) high severe
Benchmarking local_graph/node_count: Collecting 100 samples in estimated 5.0007 s (25M iteratlocal_graph/node_count  time:   [199.96 ns 200.07 ns 200.18 ns]
                        thrpt:  [4.9956 Melem/s 4.9983 Melem/s 5.0010 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking local_graph/stats: Collecting 100 samples in estimated 5.0000 s (8.3M iterationslocal_graph/stats       time:   [599.06 ns 599.41 ns 599.76 ns]
                        thrpt:  [1.6673 Melem/s 1.6683 Melem/s 1.6693 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking graph_scaling/all_nodes/100: Collecting 100 samples in estimated 5.0107 s (2.1M graph_scaling/all_nodes/100
                        time:   [2.4310 µs 2.4414 µs 2.4522 µs]
                        thrpt:  [40.779 Melem/s 40.961 Melem/s 41.135 Melem/s]
Benchmarking graph_scaling/nodes_within_hops/100: Collecting 100 samples in estimated 5.0006 graph_scaling/nodes_within_hops/100
                        time:   [2.7750 µs 2.7886 µs 2.8028 µs]
                        thrpt:  [35.679 Melem/s 35.860 Melem/s 36.036 Melem/s]
Benchmarking graph_scaling/all_nodes/500: Collecting 100 samples in estimated 5.0034 s (641k graph_scaling/all_nodes/500
                        time:   [7.8117 µs 7.8336 µs 7.8581 µs]
                        thrpt:  [63.629 Melem/s 63.827 Melem/s 64.007 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking graph_scaling/nodes_within_hops/500: Collecting 100 samples in estimated 5.0263 graph_scaling/nodes_within_hops/500
                        time:   [9.2127 µs 9.2290 µs 9.2472 µs]
                        thrpt:  [54.071 Melem/s 54.177 Melem/s 54.273 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  8 (8.00%) high mild
  7 (7.00%) high severe
Benchmarking graph_scaling/all_nodes/1000: Collecting 100 samples in estimated 5.1697 s (96k graph_scaling/all_nodes/1000
                        time:   [49.762 µs 54.456 µs 59.420 µs]
                        thrpt:  [16.829 Melem/s 18.363 Melem/s 20.096 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild
Benchmarking graph_scaling/nodes_within_hops/1000: Collecting 100 samples in estimated 5.0512graph_scaling/nodes_within_hops/1000
                        time:   [59.023 µs 63.980 µs 69.033 µs]
                        thrpt:  [14.486 Melem/s 15.630 Melem/s 16.943 Melem/s]
Benchmarking graph_scaling/all_nodes/5000: Collecting 100 samples in estimated 5.2792 s (45k graph_scaling/all_nodes/5000
                        time:   [106.86 µs 114.64 µs 122.15 µs]
                        thrpt:  [40.934 Melem/s 43.615 Melem/s 46.790 Melem/s]
Benchmarking graph_scaling/nodes_within_hops/5000: Collecting 100 samples in estimated 5.2785graph_scaling/nodes_within_hops/5000
                        time:   [130.30 µs 135.63 µs 141.05 µs]
                        thrpt:  [35.449 Melem/s 36.864 Melem/s 38.374 Melem/s]

Benchmarking capability_search/find_with_gpu: Collecting 100 samples in estimated 5.0623 s (2capability_search/find_with_gpu
                        time:   [17.271 µs 17.294 µs 17.315 µs]
                        thrpt:  [57.754 Kelem/s 57.825 Kelem/s 57.901 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_search/find_by_tool_python: Collecting 100 samples in estimated 5.080capability_search/find_by_tool_python
                        time:   [31.265 µs 31.365 µs 31.454 µs]
                        thrpt:  [31.792 Kelem/s 31.883 Kelem/s 31.985 Kelem/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low severe
  11 (11.00%) low mild
  1 (1.00%) high severe
Benchmarking capability_search/find_by_tool_rust: Collecting 100 samples in estimated 5.1265 capability_search/find_by_tool_rust
                        time:   [40.617 µs 40.684 µs 40.746 µs]
                        thrpt:  [24.542 Kelem/s 24.580 Kelem/s 24.620 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking graph_concurrent/concurrent_pingwave/4: Collecting 20 samples in estimated 5.000graph_concurrent/concurrent_pingwave/4
                        time:   [105.66 µs 106.45 µs 107.40 µs]
                        thrpt:  [18.622 Melem/s 18.789 Melem/s 18.928 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking graph_concurrent/concurrent_pingwave/8: Collecting 20 samples in estimated 5.023graph_concurrent/concurrent_pingwave/8
                        time:   [168.35 µs 171.91 µs 176.30 µs]
                        thrpt:  [22.688 Melem/s 23.268 Melem/s 23.760 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking graph_concurrent/concurrent_pingwave/16: Collecting 20 samples in estimated 5.00graph_concurrent/concurrent_pingwave/16
                        time:   [296.83 µs 299.61 µs 302.41 µs]
                        thrpt:  [26.454 Melem/s 26.701 Melem/s 26.952 Melem/s]

Benchmarking path_finding/path_1_hop: Collecting 100 samples in estimated 5.0048 s (3.2M iterpath_finding/path_1_hop time:   [1.5524 µs 1.5545 µs 1.5568 µs]
                        thrpt:  [642.33 Kelem/s 643.29 Kelem/s 644.17 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking path_finding/path_2_hops: Collecting 100 samples in estimated 5.0019 s (3.1M itepath_finding/path_2_hops
                        time:   [1.5962 µs 1.5978 µs 1.5995 µs]
                        thrpt:  [625.18 Kelem/s 625.85 Kelem/s 626.48 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking path_finding/path_4_hops: Collecting 100 samples in estimated 5.0021 s (2.7M itepath_finding/path_4_hops
                        time:   [1.8452 µs 1.8471 µs 1.8491 µs]
                        thrpt:  [540.80 Kelem/s 541.39 Kelem/s 541.96 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking path_finding/path_not_found: Collecting 100 samples in estimated 5.0029 s (2.9M path_finding/path_not_found
                        time:   [1.7375 µs 1.7406 µs 1.7439 µs]
                        thrpt:  [573.42 Kelem/s 574.51 Kelem/s 575.54 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking path_finding/path_complex_graph: Collecting 100 samples in estimated 5.3913 s (2path_finding/path_complex_graph
                        time:   [212.56 µs 213.94 µs 215.30 µs]
                        thrpt:  [4.6447 Kelem/s 4.6743 Kelem/s 4.7046 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking failure_detector/heartbeat_existing: Collecting 100 samples in estimated 5.0000 failure_detector/heartbeat_existing
                        time:   [29.271 ns 29.843 ns 30.525 ns]
                        thrpt:  [32.760 Melem/s 33.508 Melem/s 34.164 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  4 (4.00%) high mild
  18 (18.00%) high severe
Benchmarking failure_detector/heartbeat_new: Collecting 100 samples in estimated 5.0000 s (25failure_detector/heartbeat_new
                        time:   [239.00 ns 242.50 ns 245.71 ns]
                        thrpt:  [4.0699 Melem/s 4.1237 Melem/s 4.1841 Melem/s]
Benchmarking failure_detector/status_check: Collecting 100 samples in estimated 5.0000 s (344failure_detector/status_check
                        time:   [14.212 ns 14.484 ns 14.745 ns]
                        thrpt:  [67.820 Melem/s 69.041 Melem/s 70.365 Melem/s]
Benchmarking failure_detector/check_all: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 35.2s, or reduce sample count to 10.
Benchmarking failure_detector/check_all: Collecting 100 samples in estimated 35.203 s (100 itfailure_detector/check_all
                        time:   [343.94 ms 344.10 ms 344.31 ms]
                        thrpt:  [2.9044  elem/s 2.9061  elem/s 2.9074  elem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking failure_detector/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.1s, or reduce sample count to 60.
Benchmarking failure_detector/stats: Collecting 100 samples in estimated 8.0685 s (100 iteratfailure_detector/stats  time:   [80.668 ms 80.797 ms 80.960 ms]
                        thrpt:  [12.352  elem/s 12.377  elem/s 12.397  elem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe

Benchmarking loss_simulator/should_drop_1pct: Collecting 100 samples in estimated 5.0000 s (1loss_simulator/should_drop_1pct
                        time:   [2.7962 ns 2.8003 ns 2.8068 ns]
                        thrpt:  [356.28 Melem/s 357.11 Melem/s 357.63 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking loss_simulator/should_drop_5pct: Collecting 100 samples in estimated 5.0000 s (1loss_simulator/should_drop_5pct
                        time:   [3.1565 ns 3.1587 ns 3.1611 ns]
                        thrpt:  [316.35 Melem/s 316.59 Melem/s 316.81 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking loss_simulator/should_drop_10pct: Collecting 100 samples in estimated 5.0000 s (loss_simulator/should_drop_10pct
                        time:   [3.6278 ns 3.6301 ns 3.6327 ns]
                        thrpt:  [275.28 Melem/s 275.47 Melem/s 275.65 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking loss_simulator/should_drop_20pct: Collecting 100 samples in estimated 5.0000 s (loss_simulator/should_drop_20pct
                        time:   [4.6085 ns 4.6202 ns 4.6338 ns]
                        thrpt:  [215.81 Melem/s 216.44 Melem/s 216.99 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking loss_simulator/should_drop_burst: Collecting 100 samples in estimated 5.0000 s (loss_simulator/should_drop_burst
                        time:   [3.0105 ns 3.0124 ns 3.0144 ns]
                        thrpt:  [331.74 Melem/s 331.96 Melem/s 332.17 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking circuit_breaker/allow_closed: Collecting 100 samples in estimated 5.0000 s (510Mcircuit_breaker/allow_closed
                        time:   [9.7941 ns 9.8009 ns 9.8076 ns]
                        thrpt:  [101.96 Melem/s 102.03 Melem/s 102.10 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low severe
  4 (4.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking circuit_breaker/record_success: Collecting 100 samples in estimated 5.0000 s (58circuit_breaker/record_success
                        time:   [8.4058 ns 8.4193 ns 8.4330 ns]
                        thrpt:  [118.58 Melem/s 118.77 Melem/s 118.97 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking circuit_breaker/record_failure: Collecting 100 samples in estimated 5.0000 s (67circuit_breaker/record_failure
                        time:   [7.4368 ns 7.4424 ns 7.4480 ns]
                        thrpt:  [134.26 Melem/s 134.37 Melem/s 134.47 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking circuit_breaker/state: Collecting 100 samples in estimated 5.0000 s (524M iteratcircuit_breaker/state   time:   [9.5256 ns 9.5349 ns 9.5447 ns]
                        thrpt:  [104.77 Melem/s 104.88 Melem/s 104.98 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking recovery_manager/on_failure_with_alternates: Collecting 100 samples in estimatedrecovery_manager/on_failure_with_alternates
                        time:   [221.80 ns 225.35 ns 228.51 ns]
                        thrpt:  [4.3761 Melem/s 4.4374 Melem/s 4.5086 Melem/s]
Benchmarking recovery_manager/on_failure_no_alternates: Collecting 100 samples in estimated 5recovery_manager/on_failure_no_alternates
                        time:   [213.48 ns 227.89 ns 243.03 ns]
                        thrpt:  [4.1147 Melem/s 4.3881 Melem/s 4.6843 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking recovery_manager/get_action: Collecting 100 samples in estimated 5.0001 s (128M recovery_manager/get_action
                        time:   [38.796 ns 38.881 ns 38.959 ns]
                        thrpt:  [25.668 Melem/s 25.720 Melem/s 25.776 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) low mild
  1 (1.00%) high mild
Benchmarking recovery_manager/is_failed: Collecting 100 samples in estimated 5.0000 s (360M irecovery_manager/is_failed
                        time:   [13.372 ns 13.734 ns 14.088 ns]
                        thrpt:  [70.984 Melem/s 72.810 Melem/s 74.783 Melem/s]
Benchmarking recovery_manager/on_recovery: Collecting 100 samples in estimated 5.0005 s (43M recovery_manager/on_recovery
                        time:   [120.23 ns 121.16 ns 121.98 ns]
                        thrpt:  [8.1980 Melem/s 8.2534 Melem/s 8.3172 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) low severe
  6 (6.00%) low mild
Benchmarking recovery_manager/stats: Collecting 100 samples in estimated 5.0000 s (7.1B iterarecovery_manager/stats  time:   [701.00 ps 701.49 ps 702.03 ps]
                        thrpt:  [1.4244 Gelem/s 1.4255 Gelem/s 1.4265 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

Benchmarking failure_scaling/check_all/100: Collecting 100 samples in estimated 5.0040 s (1.2failure_scaling/check_all/100
                        time:   [4.7659 µs 4.7875 µs 4.8043 µs]
                        thrpt:  [20.815 Melem/s 20.888 Melem/s 20.982 Melem/s]
Benchmarking failure_scaling/healthy_nodes/100: Collecting 100 samples in estimated 5.0069 s failure_scaling/healthy_nodes/100
                        time:   [1.6990 µs 1.6999 µs 1.7008 µs]
                        thrpt:  [58.797 Melem/s 58.828 Melem/s 58.859 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking failure_scaling/check_all/500: Collecting 100 samples in estimated 5.0529 s (288failure_scaling/check_all/500
                        time:   [20.770 µs 20.854 µs 20.923 µs]
                        thrpt:  [23.897 Melem/s 23.976 Melem/s 24.073 Melem/s]
Benchmarking failure_scaling/healthy_nodes/500: Collecting 100 samples in estimated 5.0245 s failure_scaling/healthy_nodes/500
                        time:   [5.4350 µs 5.4398 µs 5.4458 µs]
                        thrpt:  [91.814 Melem/s 91.915 Melem/s 91.997 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
Benchmarking failure_scaling/check_all/1000: Collecting 100 samples in estimated 5.0759 s (14failure_scaling/check_all/1000
                        time:   [40.759 µs 40.948 µs 41.105 µs]
                        thrpt:  [24.328 Melem/s 24.421 Melem/s 24.535 Melem/s]
Benchmarking failure_scaling/healthy_nodes/1000: Collecting 100 samples in estimated 5.0078 sfailure_scaling/healthy_nodes/1000
                        time:   [10.214 µs 10.219 µs 10.224 µs]
                        thrpt:  [97.810 Melem/s 97.860 Melem/s 97.907 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking failure_scaling/check_all/5000: Collecting 100 samples in estimated 5.2246 s (30failure_scaling/check_all/5000
                        time:   [202.99 µs 203.67 µs 204.24 µs]
                        thrpt:  [24.481 Melem/s 24.549 Melem/s 24.632 Melem/s]
Benchmarking failure_scaling/healthy_nodes/5000: Collecting 100 samples in estimated 5.1834 sfailure_scaling/healthy_nodes/5000
                        time:   [48.837 µs 48.863 µs 48.890 µs]
                        thrpt:  [102.27 Melem/s 102.33 Melem/s 102.38 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking failure_concurrent/concurrent_heartbeat/4: Collecting 20 samples in estimated 5.failure_concurrent/concurrent_heartbeat/4
                        time:   [182.82 µs 183.08 µs 183.28 µs]
                        thrpt:  [10.912 Melem/s 10.924 Melem/s 10.940 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking failure_concurrent/concurrent_heartbeat/8: Collecting 20 samples in estimated 5.failure_concurrent/concurrent_heartbeat/8
                        time:   [252.68 µs 253.05 µs 253.42 µs]
                        thrpt:  [15.784 Melem/s 15.807 Melem/s 15.830 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking failure_concurrent/concurrent_heartbeat/16: Collecting 20 samples in estimated 5failure_concurrent/concurrent_heartbeat/16
                        time:   [463.76 µs 465.78 µs 467.13 µs]
                        thrpt:  [17.126 Melem/s 17.176 Melem/s 17.250 Melem/s]

Benchmarking failure_recovery_cycle/full_cycle: Collecting 100 samples in estimated 5.0015 s failure_recovery_cycle/full_cycle
                        time:   [280.53 ns 286.90 ns 292.58 ns]
                        thrpt:  [3.4178 Melem/s 3.4855 Melem/s 3.5646 Melem/s]

Benchmarking capability_set/create: Collecting 100 samples in estimated 5.0444 s (91k iteraticapability_set/create   time:   [52.308 µs 52.352 µs 52.397 µs]
                        thrpt:  [19.085 Kelem/s 19.102 Kelem/s 19.117 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_set/serialize: Collecting 100 samples in estimated 5.1101 s (116k itecapability_set/serialize
                        time:   [43.944 µs 43.970 µs 43.997 µs]
                        thrpt:  [22.729 Kelem/s 22.743 Kelem/s 22.756 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_set/deserialize: Collecting 100 samples in estimated 5.0065 s (758k icapability_set/deserialize
                        time:   [6.5998 µs 6.6045 µs 6.6093 µs]
                        thrpt:  [151.30 Kelem/s 151.41 Kelem/s 151.52 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_set/roundtrip: Collecting 100 samples in estimated 5.1056 s (101k itecapability_set/roundtrip
                        time:   [50.543 µs 50.589 µs 50.639 µs]
                        thrpt:  [19.747 Kelem/s 19.767 Kelem/s 19.785 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_set/has_tag: Collecting 100 samples in estimated 5.0001 s (152M iteracapability_set/has_tag  time:   [32.814 ns 32.949 ns 33.106 ns]
                        thrpt:  [30.206 Melem/s 30.350 Melem/s 30.475 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  8 (8.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
  7 (7.00%) high severe
Benchmarking capability_set/has_model: Collecting 100 samples in estimated 5.0002 s (8.0M itecapability_set/has_model
                        time:   [620.33 ns 620.70 ns 621.12 ns]
                        thrpt:  [1.6100 Melem/s 1.6111 Melem/s 1.6120 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
Benchmarking capability_set/has_tool: Collecting 100 samples in estimated 5.0013 s (10M iteracapability_set/has_tool time:   [498.28 ns 498.58 ns 498.88 ns]
                        thrpt:  [2.0045 Melem/s 2.0057 Melem/s 2.0069 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking capability_set/has_gpu: Collecting 100 samples in estimated 5.0000 s (146M iteracapability_set/has_gpu  time:   [34.314 ns 34.337 ns 34.360 ns]
                        thrpt:  [29.103 Melem/s 29.123 Melem/s 29.143 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking capability_announcement/create: Collecting 100 samples in estimated 5.0026 s (4.capability_announcement/create
                        time:   [1.1502 µs 1.1516 µs 1.1531 µs]
                        thrpt:  [867.25 Kelem/s 868.36 Kelem/s 869.44 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_announcement/serialize: Collecting 100 samples in estimated 5.0641 s capability_announcement/serialize
                        time:   [45.466 µs 45.559 µs 45.668 µs]
                        thrpt:  [21.897 Kelem/s 21.950 Kelem/s 21.994 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_announcement/deserialize: Collecting 100 samples in estimated 5.0246 capability_announcement/deserialize
                        time:   [6.9878 µs 6.9979 µs 7.0088 µs]
                        thrpt:  [142.68 Kelem/s 142.90 Kelem/s 143.11 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
Benchmarking capability_announcement/is_expired: Collecting 100 samples in estimated 5.0001 scapability_announcement/is_expired
                        time:   [25.242 ns 25.253 ns 25.265 ns]
                        thrpt:  [39.580 Melem/s 39.599 Melem/s 39.617 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking capability_filter/match_single_tag: Collecting 100 samples in estimated 5.0002 scapability_filter/match_single_tag
                        time:   [68.533 ns 68.576 ns 68.616 ns]
                        thrpt:  [14.574 Melem/s 14.582 Melem/s 14.591 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_filter/match_require_gpu: Collecting 100 samples in estimated 5.0001 capability_filter/match_require_gpu
                        time:   [36.846 ns 36.871 ns 36.896 ns]
                        thrpt:  [27.103 Melem/s 27.122 Melem/s 27.140 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_filter/match_gpu_vendor: Collecting 100 samples in estimated 5.0974 scapability_filter/match_gpu_vendor
                        time:   [46.122 µs 46.167 µs 46.217 µs]
                        thrpt:  [21.637 Kelem/s 21.661 Kelem/s 21.682 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking capability_filter/match_min_memory: Collecting 100 samples in estimated 5.0951 scapability_filter/match_min_memory
                        time:   [46.106 µs 46.157 µs 46.214 µs]
                        thrpt:  [21.638 Kelem/s 21.665 Kelem/s 21.689 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking capability_filter/match_complex: Collecting 100 samples in estimated 5.1953 s (1capability_filter/match_complex
                        time:   [46.989 µs 47.036 µs 47.090 µs]
                        thrpt:  [21.236 Kelem/s 21.260 Kelem/s 21.282 Kelem/s]
Benchmarking capability_filter/match_no_match: Collecting 100 samples in estimated 5.0001 s (capability_filter/match_no_match
                        time:   [63.386 ns 63.462 ns 63.578 ns]
                        thrpt:  [15.729 Melem/s 15.758 Melem/s 15.776 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

Benchmarking capability_fold_insert/index_nodes/100: Collecting 100 samples in estimated 5.98capability_fold_insert/index_nodes/100
                        time:   [9.9520 ms 9.9757 ms 10.002 ms]
                        thrpt:  [9.9984 Kelem/s 10.024 Kelem/s 10.048 Kelem/s]
Found 15 outliers among 100 measurements (15.00%)
  13 (13.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_fold_insert/index_nodes/1000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 10.1s, or reduce sample count to 40.
Benchmarking capability_fold_insert/index_nodes/1000: Collecting 100 samples in estimated 10.capability_fold_insert/index_nodes/1000
                        time:   [101.19 ms 101.44 ms 101.71 ms]
                        thrpt:  [9.8315 Kelem/s 9.8578 Kelem/s 9.8828 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 103.4s, or reduce sample count to 10.
Benchmarking capability_fold_insert/index_nodes/10000: Collecting 100 samples in estimated 10capability_fold_insert/index_nodes/10000
                        time:   [1.0325 s 1.0349 s 1.0374 s]
                        thrpt:  [9.6394 Kelem/s 9.6628 Kelem/s 9.6850 Kelem/s]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild

Benchmarking capability_fold_query/query_single_tag: Collecting 100 samples in estimated 5.79capability_fold_query/query_single_tag
                        time:   [8.2619 ms 8.3175 ms 8.3728 ms]
                        thrpt:  [119.43  elem/s 120.23  elem/s 121.04  elem/s]
Benchmarking capability_fold_query/query_require_gpu: Collecting 100 samples in estimated 5.0capability_fold_query/query_require_gpu
                        time:   [16.789 ms 16.895 ms 17.005 ms]
                        thrpt:  [58.806  elem/s 59.188  elem/s 59.563  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_query/query_gpu_vendor: Collecting 100 samples in estimated 5.06capability_fold_query/query_gpu_vendor
                        time:   [16.709 ms 16.818 ms 16.931 ms]
                        thrpt:  [59.064  elem/s 59.460  elem/s 59.847  elem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
Benchmarking capability_fold_query/query_min_memory: Collecting 100 samples in estimated 5.02capability_fold_query/query_min_memory
                        time:   [16.533 ms 16.640 ms 16.749 ms]
                        thrpt:  [59.705  elem/s 60.095  elem/s 60.484  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_query/query_complex: Collecting 100 samples in estimated 5.7247 capability_fold_query/query_complex
                        time:   [8.0838 ms 8.1464 ms 8.2094 ms]
                        thrpt:  [121.81  elem/s 122.75  elem/s 123.70  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_query/query_model: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.5s, or reduce sample count to 50.
Benchmarking capability_fold_query/query_model: Collecting 100 samples in estimated 8.4992 s capability_fold_query/query_model
                        time:   [84.835 ms 85.221 ms 85.622 ms]
                        thrpt:  [11.679  elem/s 11.734  elem/s 11.788  elem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_fold_query/query_tool: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.2s, or reduce sample count to 60.
Benchmarking capability_fold_query/query_tool: Collecting 100 samples in estimated 8.1756 s (capability_fold_query/query_tool
                        time:   [82.418 ms 82.797 ms 83.190 ms]
                        thrpt:  [12.021  elem/s 12.078  elem/s 12.133  elem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking capability_fold_query/query_no_results: Collecting 100 samples in estimated 5.00capability_fold_query/query_no_results
                        time:   [110.69 ns 111.38 ns 111.97 ns]
                        thrpt:  [8.9313 Melem/s 8.9782 Melem/s 9.0341 Melem/s]

Benchmarking capability_fold_find_best/find_best_simple: Collecting 100 samples in estimated capability_fold_find_best/find_best_simple
                        time:   [16.699 ms 16.800 ms 16.903 ms]
                        thrpt:  [59.163  elem/s 59.524  elem/s 59.882  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_find_best/find_best_with_prefs: Collecting 100 samples in estimacapability_fold_find_best/find_best_with_prefs
                        time:   [8.0148 ms 8.0723 ms 8.1300 ms]
                        thrpt:  [123.00  elem/s 123.88  elem/s 124.77  elem/s]

Benchmarking capability_fold_scaling/query_tag/1000: Collecting 100 samples in estimated 7.96capability_fold_scaling/query_tag/1000
                        time:   [783.95 µs 799.88 µs 816.98 µs]
                        thrpt:  [1.2240 Kelem/s 1.2502 Kelem/s 1.2756 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_scaling/query_complex/1000: Collecting 100 samples in estimated capability_fold_scaling/query_complex/1000
                        time:   [795.44 µs 805.10 µs 814.48 µs]
                        thrpt:  [1.2278 Kelem/s 1.2421 Kelem/s 1.2572 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_fold_scaling/query_tag/5000: Collecting 100 samples in estimated 5.23capability_fold_scaling/query_tag/5000
                        time:   [3.9703 ms 4.0139 ms 4.0571 ms]
                        thrpt:  [246.48  elem/s 249.13  elem/s 251.87  elem/s]
Benchmarking capability_fold_scaling/query_complex/5000: Collecting 100 samples in estimated capability_fold_scaling/query_complex/5000
                        time:   [3.9808 ms 4.0242 ms 4.0674 ms]
                        thrpt:  [245.86  elem/s 248.50  elem/s 251.20  elem/s]
Benchmarking capability_fold_scaling/query_tag/10000: Collecting 100 samples in estimated 5.1capability_fold_scaling/query_tag/10000
                        time:   [8.4771 ms 8.5271 ms 8.5792 ms]
                        thrpt:  [116.56  elem/s 117.27  elem/s 117.96  elem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking capability_fold_scaling/query_complex/10000: Collecting 100 samples in estimatedcapability_fold_scaling/query_complex/10000
                        time:   [8.4497 ms 8.4954 ms 8.5417 ms]
                        thrpt:  [117.07  elem/s 117.71  elem/s 118.35  elem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking capability_fold_scaling/query_tag/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.2s, or reduce sample count to 90.
Benchmarking capability_fold_scaling/query_tag/50000: Collecting 100 samples in estimated 5.1capability_fold_scaling/query_tag/50000
                        time:   [50.471 ms 50.733 ms 51.005 ms]
                        thrpt:  [19.606  elem/s 19.711  elem/s 19.813  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_scaling/query_complex/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.1s, or reduce sample count to 90.
Benchmarking capability_fold_scaling/query_complex/50000: Collecting 100 samples in estimatedcapability_fold_scaling/query_complex/50000
                        time:   [52.143 ms 52.402 ms 52.663 ms]
                        thrpt:  [18.989  elem/s 19.083  elem/s 19.178  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking capability_fold_concurrent/concurrent_index/4: Collecting 20 samples in estimatecapability_fold_concurrent/concurrent_index/4
                        time:   [50.263 ms 50.366 ms 50.485 ms]
                        thrpt:  [39.616 Kelem/s 39.709 Kelem/s 39.791 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 356.0s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_query/4: Collecting 20 samples in estimated 356.02 s (20 iterations)
