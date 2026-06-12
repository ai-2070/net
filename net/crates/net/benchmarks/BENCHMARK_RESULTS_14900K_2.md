     Running unittests src\bin\net-blob.rs (target\release\deps\net_blob-c06f4bdada5e9ca5.exe)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running benches\auth_guard.rs (target\release\deps\auth_guard-7199754eae51d21f.exe)
Gnuplot not found, using plotters backend
auth_guard_check_fast_hit/single_thread
                        time:   [17.830 ns 17.883 ns 17.944 ns]
                        thrpt:  [55.729 Melem/s 55.918 Melem/s 56.084 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

auth_guard_check_fast_miss/single_thread
                        time:   [3.0910 ns 3.0989 ns 3.1049 ns]
                        thrpt:  [322.08 Melem/s 322.70 Melem/s 323.52 Melem/s]
Found 7 outliers among 50 measurements (14.00%)
  2 (4.00%) low severe
  2 (4.00%) low mild
  2 (4.00%) high mild
  1 (2.00%) high severe

auth_guard_check_fast_contended/eight_threads
                        time:   [14.433 ns 14.919 ns 15.488 ns]
                        thrpt:  [64.565 Melem/s 67.031 Melem/s 69.285 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  2 (4.00%) high mild

auth_guard_allow_channel/insert
                        time:   [121.88 ns 127.75 ns 133.09 ns]
                        thrpt:  [7.5135 Melem/s 7.8281 Melem/s 8.2049 Melem/s]
Found 2 outliers among 50 measurements (4.00%)
  2 (4.00%) high mild

auth_guard_hot_hit_ceiling/million_ops
                        time:   [2.5570 ms 2.5621 ms 2.5671 ms]

     Running benches\cortex.rs (target\release\deps\cortex-da9a4ffdeb608cd1.exe)
Gnuplot not found, using plotters backend
cortex_ingest/tasks_create
                        time:   [145.45 ns 146.42 ns 147.52 ns]
                        thrpt:  [6.7787 Melem/s 6.8299 Melem/s 6.8753 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe
cortex_ingest/memories_store
                        time:   [202.77 ns 204.16 ns 205.84 ns]
                        thrpt:  [4.8582 Melem/s 4.8982 Melem/s 4.9317 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  4 (4.00%) high mild
  12 (12.00%) high severe

cortex_fold_barrier/tasks_create_and_wait
                        time:   [1.7676 µs 1.8358 µs 1.9194 µs]
                        thrpt:  [521.00 Kelem/s 544.73 Kelem/s 565.73 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
cortex_fold_barrier/memories_store_and_wait
                        time:   [2.2107 µs 2.3366 µs 2.4989 µs]
                        thrpt:  [400.17 Kelem/s 427.97 Kelem/s 452.35 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe

cortex_query/tasks_find_many/100
                        time:   [2.1852 µs 2.1882 µs 2.1917 µs]
                        thrpt:  [45.626 Melem/s 45.699 Melem/s 45.763 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
cortex_query/tasks_count_where/100
                        time:   [131.90 ns 132.68 ns 133.49 ns]
                        thrpt:  [749.10 Melem/s 753.71 Melem/s 758.16 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
cortex_query/tasks_find_unique/100
                        time:   [6.7407 ns 6.7490 ns 6.7577 ns]
                        thrpt:  [14.798 Gelem/s 14.817 Gelem/s 14.835 Gelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_find_many_tag/100
                        time:   [959.63 ns 961.85 ns 964.27 ns]
                        thrpt:  [103.70 Melem/s 103.97 Melem/s 104.21 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
cortex_query/memories_count_where/100
                        time:   [570.52 ns 573.06 ns 575.61 ns]
                        thrpt:  [173.73 Melem/s 174.50 Melem/s 175.28 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
cortex_query/tasks_find_many/1000
                        time:   [20.328 µs 20.358 µs 20.391 µs]
                        thrpt:  [49.042 Melem/s 49.121 Melem/s 49.194 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
cortex_query/tasks_count_where/1000
                        time:   [1.2505 µs 1.2544 µs 1.2583 µs]
                        thrpt:  [794.72 Melem/s 797.21 Melem/s 799.68 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_find_unique/1000
                        time:   [6.7389 ns 6.7501 ns 6.7639 ns]
                        thrpt:  [147.84 Gelem/s 148.15 Gelem/s 148.39 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
cortex_query/memories_find_many_tag/1000
                        time:   [8.7224 µs 8.7677 µs 8.8172 µs]
                        thrpt:  [113.41 Melem/s 114.05 Melem/s 114.65 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
cortex_query/memories_count_where/1000
                        time:   [5.8389 µs 5.8692 µs 5.9026 µs]
                        thrpt:  [169.42 Melem/s 170.38 Melem/s 171.26 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_find_many/10000
                        time:   [175.78 µs 176.02 µs 176.28 µs]
                        thrpt:  [56.727 Melem/s 56.812 Melem/s 56.890 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) high mild
  3 (3.00%) high severe
cortex_query/tasks_count_where/10000
                        time:   [30.866 µs 30.953 µs 31.043 µs]
                        thrpt:  [322.13 Melem/s 323.07 Melem/s 323.98 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/tasks_find_unique/10000
                        time:   [6.7519 ns 6.7620 ns 6.7734 ns]
                        thrpt:  [1476.4 Gelem/s 1478.8 Gelem/s 1481.1 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
cortex_query/memories_find_many_tag/10000
                        time:   [164.75 µs 165.03 µs 165.34 µs]
                        thrpt:  [60.482 Melem/s 60.594 Melem/s 60.699 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
cortex_query/memories_count_where/10000
                        time:   [130.32 µs 130.60 µs 130.87 µs]
                        thrpt:  [76.412 Melem/s 76.570 Melem/s 76.732 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

cortex_snapshot/tasks_encode/100
                        time:   [3.0837 µs 3.0890 µs 3.0948 µs]
                        thrpt:  [32.313 Melem/s 32.373 Melem/s 32.429 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
cortex_snapshot/memories_encode/100
                        time:   [5.3609 µs 5.3660 µs 5.3717 µs]
                        thrpt:  [18.616 Melem/s 18.636 Melem/s 18.654 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_3939/100
                        time:   [2.1057 µs 2.1084 µs 2.1115 µs]
                        thrpt:  [47.360 Melem/s 47.428 Melem/s 47.491 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
cortex_snapshot/netdb_bundle_decode/100
                        time:   [2.5510 µs 2.5563 µs 2.5636 µs]
                        thrpt:  [39.008 Melem/s 39.119 Melem/s 39.201 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
cortex_snapshot/tasks_encode/1000
                        time:   [31.956 µs 31.982 µs 32.012 µs]
                        thrpt:  [31.238 Melem/s 31.267 Melem/s 31.293 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) high mild
  6 (6.00%) high severe
cortex_snapshot/memories_encode/1000
                        time:   [53.091 µs 53.147 µs 53.208 µs]
                        thrpt:  [18.794 Melem/s 18.816 Melem/s 18.836 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  9 (9.00%) high mild
  5 (5.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_48274/1000
                        time:   [24.385 µs 24.412 µs 24.442 µs]
                        thrpt:  [40.914 Melem/s 40.964 Melem/s 41.010 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
cortex_snapshot/netdb_bundle_decode/1000
                        time:   [32.441 µs 32.481 µs 32.526 µs]
                        thrpt:  [30.745 Melem/s 30.787 Melem/s 30.825 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/tasks_encode/10000
                        time:   [327.21 µs 327.57 µs 327.96 µs]
                        thrpt:  [30.491 Melem/s 30.528 Melem/s 30.561 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
cortex_snapshot/memories_encode/10000
                        time:   [587.09 µs 588.10 µs 589.23 µs]
                        thrpt:  [16.971 Melem/s 17.004 Melem/s 17.033 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
cortex_snapshot/netdb_bundle_encode_bytes_511774/10000
                        time:   [250.03 µs 250.34 µs 250.67 µs]
                        thrpt:  [39.893 Melem/s 39.945 Melem/s 39.995 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
cortex_snapshot/netdb_bundle_decode/10000
                        time:   [340.53 µs 340.82 µs 341.16 µs]
                        thrpt:  [29.312 Melem/s 29.341 Melem/s 29.366 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

     Running benches\ingestion.rs (target\release\deps\ingestion-c3005cbf4fae34e6.exe)
Gnuplot not found, using plotters backend
shard/ingest_raw/1024   time:   [103.66 ns 103.81 ns 103.99 ns]
                        thrpt:  [9.6167 Melem/s 9.6330 Melem/s 9.6472 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  7 (7.00%) high severe
shard/ingest_raw_pop/1024
                        time:   [83.416 ns 83.675 ns 83.973 ns]
                        thrpt:  [11.909 Melem/s 11.951 Melem/s 11.988 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
shard/ingest_raw/8192   time:   [103.26 ns 103.42 ns 103.57 ns]
                        thrpt:  [9.6556 Melem/s 9.6696 Melem/s 9.6840 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  7 (7.00%) low severe
  6 (6.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
shard/ingest_raw_pop/8192
                        time:   [83.493 ns 83.615 ns 83.745 ns]
                        thrpt:  [11.941 Melem/s 11.960 Melem/s 11.977 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
shard/ingest_raw/65536  time:   [100.23 ns 100.57 ns 100.85 ns]
                        thrpt:  [9.9159 Melem/s 9.9434 Melem/s 9.9770 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  10 (10.00%) low severe
  3 (3.00%) low mild
shard/ingest_raw_pop/65536
                        time:   [83.843 ns 83.979 ns 84.128 ns]
                        thrpt:  [11.887 Melem/s 11.908 Melem/s 11.927 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  7 (7.00%) high mild
  4 (4.00%) high severe
shard/ingest_raw/1048576
                        time:   [72.869 ns 73.058 ns 73.288 ns]
                        thrpt:  [13.645 Melem/s 13.688 Melem/s 13.723 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  5 (5.00%) high severe
shard/ingest_raw_pop/1048576
                        time:   [87.019 ns 87.182 ns 87.366 ns]
                        thrpt:  [11.446 Melem/s 11.470 Melem/s 11.492 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  6 (6.00%) high mild
  3 (3.00%) high severe

timestamp/next          time:   [16.373 ns 16.392 ns 16.411 ns]
                        thrpt:  [60.935 Melem/s 61.007 Melem/s 61.075 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  4 (4.00%) high mild
  1 (1.00%) high severe
timestamp/now_raw       time:   [6.0375 ns 6.0429 ns 6.0489 ns]
                        thrpt:  [165.32 Melem/s 165.48 Melem/s 165.63 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

event/internal_event_new
                        time:   [220.54 ns 220.96 ns 221.39 ns]
                        thrpt:  [4.5169 Melem/s 4.5257 Melem/s 4.5343 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
event/internal_event_from_bytes
                        time:   [26.459 ns 26.489 ns 26.521 ns]
                        thrpt:  [37.705 Melem/s 37.752 Melem/s 37.795 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
  2 (2.00%) high severe
event/json_creation     time:   [131.82 ns 132.13 ns 132.48 ns]
                        thrpt:  [7.5481 Melem/s 7.5684 Melem/s 7.5858 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

batch/pop_batch_steady_state/100
                        time:   [7.1714 µs 7.1928 µs 7.2170 µs]
                        thrpt:  [13.856 Melem/s 13.903 Melem/s 13.944 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
batch/pop_batch_steady_state/1000
                        time:   [71.627 µs 71.832 µs 72.062 µs]
                        thrpt:  [13.877 Melem/s 13.921 Melem/s 13.961 Melem/s]
batch/pop_batch_steady_state/10000
                        time:   [722.36 µs 724.47 µs 726.85 µs]
                        thrpt:  [13.758 Melem/s 13.803 Melem/s 13.844 Melem/s]

event_bus_ingest_raw_concurrent/producers/1
                        time:   [979.82 µs 981.78 µs 983.96 µs]
                        thrpt:  [8.3255 Melem/s 8.3440 Melem/s 8.3608 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
event_bus_ingest_raw_concurrent/producers/2
                        time:   [823.67 µs 828.22 µs 833.09 µs]
                        thrpt:  [9.8333 Melem/s 9.8910 Melem/s 9.9458 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
event_bus_ingest_raw_concurrent/producers/4
                        time:   [659.04 µs 663.91 µs 669.10 µs]
                        thrpt:  [12.243 Melem/s 12.339 Melem/s 12.430 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
event_bus_ingest_raw_concurrent/producers/8
                        time:   [603.61 µs 616.56 µs 629.87 µs]
                        thrpt:  [13.006 Melem/s 13.287 Melem/s 13.572 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

     Running benches\mesh.rs (target\release\deps\mesh-4c8c7febfcdd619f.exe)
Gnuplot not found, using plotters backend
mesh_reroute/triangle_failure
                        time:   [22.726 µs 23.010 µs 23.327 µs]
                        thrpt:  [42.869 Kelem/s 43.460 Kelem/s 44.002 Kelem/s]
mesh_reroute/10_peers_10_routes
                        time:   [124.91 µs 126.33 µs 127.89 µs]
                        thrpt:  [7.8192 Kelem/s 7.9160 Kelem/s 8.0056 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking mesh_reroute/50_peers_100_routes: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.1s, enable flat sampling, or reduce sample count to 60.
mesh_reroute/50_peers_100_routes
                        time:   [1.1858 ms 1.1890 ms 1.1927 ms]
                        thrpt:  [838.42  elem/s 841.02  elem/s 843.29  elem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

mesh_proximity/on_pingwave_new
                        time:   [164.99 ns 171.69 ns 177.58 ns]
                        thrpt:  [5.6312 Melem/s 5.8245 Melem/s 6.0611 Melem/s]
mesh_proximity/on_pingwave_dedup
                        time:   [49.283 ns 49.366 ns 49.455 ns]
                        thrpt:  [20.221 Melem/s 20.257 Melem/s 20.291 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
mesh_proximity/pingwave_serialize
                        time:   [1.1942 ns 1.2393 ns 1.2822 ns]
                        thrpt:  [779.89 Melem/s 806.90 Melem/s 837.41 Melem/s]
mesh_proximity/pingwave_deserialize
                        time:   [1.4581 ns 1.4796 ns 1.5037 ns]
                        thrpt:  [665.05 Melem/s 675.87 Melem/s 685.81 Melem/s]
mesh_proximity/node_count
                        time:   [200.65 ps 200.86 ps 201.07 ps]
                        thrpt:  [4.9734 Gelem/s 4.9786 Gelem/s 4.9838 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  1 (1.00%) high severe
mesh_proximity/all_nodes_100
                        time:   [10.038 µs 10.058 µs 10.080 µs]
                        thrpt:  [99.211 Kelem/s 99.422 Kelem/s 99.620 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

mesh_dispatch/classify_direct
                        time:   [402.33 ps 402.84 ps 403.35 ps]
                        thrpt:  [2.4792 Gelem/s 2.4823 Gelem/s 2.4855 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  6 (6.00%) high mild
  4 (4.00%) high severe
mesh_dispatch/classify_routed
                        time:   [305.65 ps 307.32 ps 309.19 ps]
                        thrpt:  [3.2343 Gelem/s 3.2540 Gelem/s 3.2717 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
mesh_dispatch/classify_pingwave
                        time:   [201.16 ps 201.34 ps 201.53 ps]
                        thrpt:  [4.9619 Gelem/s 4.9667 Gelem/s 4.9711 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe

mesh_routing/lookup_hit time:   [17.777 ns 17.826 ns 17.907 ns]
                        thrpt:  [55.846 Melem/s 56.098 Melem/s 56.254 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
mesh_routing/lookup_miss
                        time:   [17.758 ns 17.786 ns 17.816 ns]
                        thrpt:  [56.128 Melem/s 56.223 Melem/s 56.314 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
mesh_routing/is_local   time:   [201.02 ps 201.28 ps 201.53 ps]
                        thrpt:  [4.9619 Gelem/s 4.9681 Gelem/s 4.9747 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  1 (1.00%) high mild
  5 (5.00%) high severe
mesh_routing/all_routes/10
                        time:   [5.6469 µs 5.6737 µs 5.7035 µs]
                        thrpt:  [175.33 Kelem/s 176.25 Kelem/s 177.09 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
mesh_routing/all_routes/100
                        time:   [7.4580 µs 7.4873 µs 7.5170 µs]
                        thrpt:  [133.03 Kelem/s 133.56 Kelem/s 134.08 Kelem/s]
mesh_routing/all_routes/1000
                        time:   [23.675 µs 23.746 µs 23.818 µs]
                        thrpt:  [41.986 Kelem/s 42.113 Kelem/s 42.239 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
mesh_routing/add_route  time:   [37.710 ns 37.786 ns 37.855 ns]
                        thrpt:  [26.417 Melem/s 26.465 Melem/s 26.518 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

     Running benches\net.rs (target\release\deps\net-e8d1f2f86e075132.exe)
Gnuplot not found, using plotters backend
net_header/serialize    time:   [1.2023 ns 1.2061 ns 1.2090 ns]
                        thrpt:  [827.15 Melem/s 829.14 Melem/s 831.75 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
net_header/deserialize  time:   [1.6029 ns 1.6079 ns 1.6116 ns]
                        thrpt:  [620.50 Melem/s 621.91 Melem/s 623.86 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  6 (6.00%) high mild
  4 (4.00%) high severe
net_header/roundtrip    time:   [1.6078 ns 1.6096 ns 1.6118 ns]
                        thrpt:  [620.44 Melem/s 621.28 Melem/s 621.97 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe

net_event_frame/write_single/64
                        time:   [35.256 ns 35.345 ns 35.435 ns]
                        thrpt:  [1.6821 GiB/s 1.6863 GiB/s 1.6906 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
net_event_frame/write_single_reused/64
                        time:   [2.2551 ns 2.5284 ns 2.9620 ns]
                        thrpt:  [20.123 GiB/s 23.574 GiB/s 26.431 GiB/s]
Found 16 outliers among 100 measurements (16.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  8 (8.00%) high severe
net_event_frame/write_single/256
                        time:   [35.247 ns 35.330 ns 35.414 ns]
                        thrpt:  [6.7324 GiB/s 6.7483 GiB/s 6.7641 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
net_event_frame/write_single_reused/256
                        time:   [3.6327 ns 4.0036 ns 4.4427 ns]
                        thrpt:  [53.665 GiB/s 59.550 GiB/s 65.630 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high severe
net_event_frame/write_single/1024
                        time:   [35.256 ns 35.310 ns 35.369 ns]
                        thrpt:  [26.964 GiB/s 27.008 GiB/s 27.050 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
net_event_frame/write_single_reused/1024
                        time:   [6.1932 ns 6.2464 ns 6.3006 ns]
                        thrpt:  [151.36 GiB/s 152.68 GiB/s 153.99 GiB/s]
net_event_frame/write_single/4096
                        time:   [48.008 ns 48.129 ns 48.260 ns]
                        thrpt:  [79.045 GiB/s 79.260 GiB/s 79.460 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
net_event_frame/write_single_reused/4096
                        time:   [21.687 ns 21.822 ns 21.975 ns]
                        thrpt:  [173.59 GiB/s 174.81 GiB/s 175.90 GiB/s]
Found 24 outliers among 100 measurements (24.00%)
  24 (24.00%) high severe
net_event_frame/write_batch/1
                        time:   [27.104 ns 27.175 ns 27.250 ns]
                        thrpt:  [2.1873 GiB/s 2.1933 GiB/s 2.1991 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
net_event_frame/write_batch/10
                        time:   [55.175 ns 55.323 ns 55.474 ns]
                        thrpt:  [10.745 GiB/s 10.774 GiB/s 10.803 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
net_event_frame/write_batch/50
                        time:   [147.32 ns 147.62 ns 147.92 ns]
                        thrpt:  [20.148 GiB/s 20.189 GiB/s 20.230 GiB/s]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  9 (9.00%) high mild
  2 (2.00%) high severe
net_event_frame/write_batch/100
                        time:   [272.04 ns 273.13 ns 274.13 ns]
                        thrpt:  [21.743 GiB/s 21.822 GiB/s 21.911 GiB/s]
Found 19 outliers among 100 measurements (19.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  8 (8.00%) high severe
net_event_frame/read_batch_10
                        time:   [163.06 ns 163.35 ns 163.66 ns]
                        thrpt:  [61.103 Melem/s 61.220 Melem/s 61.327 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

net_packet_pool/get_return/16
                        time:   [40.809 ns 40.974 ns 41.131 ns]
                        thrpt:  [24.313 Melem/s 24.406 Melem/s 24.504 Melem/s]
net_packet_pool/get_return/64
                        time:   [40.688 ns 40.917 ns 41.128 ns]
                        thrpt:  [24.314 Melem/s 24.440 Melem/s 24.577 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild
net_packet_pool/get_return/256
                        time:   [41.369 ns 41.595 ns 41.818 ns]
                        thrpt:  [23.913 Melem/s 24.042 Melem/s 24.173 Melem/s]

net_packet_build/build_packet/1
                        time:   [211.76 ns 212.20 ns 212.62 ns]
                        thrpt:  [287.06 MiB/s 287.63 MiB/s 288.22 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
net_packet_build/build_packet/10
                        time:   [435.79 ns 436.52 ns 437.20 ns]
                        thrpt:  [1.3633 GiB/s 1.3655 GiB/s 1.3677 GiB/s]
Found 13 outliers among 100 measurements (13.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  4 (4.00%) high mild
  4 (4.00%) high severe
net_packet_build/build_packet/50
                        time:   [1.4255 µs 1.4282 µs 1.4308 µs]
                        thrpt:  [2.0829 GiB/s 2.0866 GiB/s 2.0907 GiB/s]
Found 18 outliers among 100 measurements (18.00%)
  4 (4.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  9 (9.00%) high severe

net_encryption/encrypt/64
                        time:   [212.49 ns 212.81 ns 213.16 ns]
                        thrpt:  [286.34 MiB/s 286.80 MiB/s 287.24 MiB/s]
                 change:
                        time:   [−4.1985% −4.0258% −3.8429%] (p = 0.00 < 0.05)
                        thrpt:  [+3.9965% +4.1946% +4.3825%]
                        Performance has improved.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
net_encryption/encrypt/256
                        time:   [277.70 ns 277.96 ns 278.23 ns]
                        thrpt:  [877.48 MiB/s 878.33 MiB/s 879.16 MiB/s]
                 change:
                        time:   [−3.5344% −3.3239% −3.1348%] (p = 0.00 < 0.05)
                        thrpt:  [+3.2363% +3.4381% +3.6639%]
                        Performance has improved.
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
net_encryption/encrypt/1024
                        time:   [539.90 ns 540.50 ns 541.12 ns]
                        thrpt:  [1.7624 GiB/s 1.7644 GiB/s 1.7664 GiB/s]
                 change:
                        time:   [−0.7890% −0.5997% −0.3870%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3885% +0.6033% +0.7953%]
                        Change within noise threshold.
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  6 (6.00%) high severe
net_encryption/encrypt/4096
                        time:   [1.5251 µs 1.5262 µs 1.5275 µs]
                        thrpt:  [2.4973 GiB/s 2.4995 GiB/s 2.5013 GiB/s]
                 change:
                        time:   [−0.8665% −0.3094% +0.3664%] (p = 0.45 > 0.05)
                        thrpt:  [−0.3651% +0.3103% +0.8741%]
                        No change in performance detected.
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) high mild
  7 (7.00%) high severe
net_encryption/raw_aead/64
                        time:   [964.60 ns 965.52 ns 966.49 ns]
                        thrpt:  [63.151 MiB/s 63.215 MiB/s 63.275 MiB/s]
                 change:
                        time:   [+0.4149% +0.7569% +1.1476%] (p = 0.00 < 0.05)
                        thrpt:  [−1.1346% −0.7512% −0.4132%]
                        Change within noise threshold.
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
net_encryption/raw_aead/256
                        time:   [1.0274 µs 1.0284 µs 1.0294 µs]
                        thrpt:  [237.16 MiB/s 237.39 MiB/s 237.64 MiB/s]
                 change:
                        time:   [−4.0560% −2.7402% −1.7716%] (p = 0.00 < 0.05)
                        thrpt:  [+1.8035% +2.8174% +4.2275%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high mild
  4 (4.00%) high severe
net_encryption/raw_aead/1024
                        time:   [1.3922 µs 1.3952 µs 1.3976 µs]
                        thrpt:  [698.75 MiB/s 699.96 MiB/s 701.43 MiB/s]
                 change:
                        time:   [−16.869% −6.8885% −0.5732%] (p = 0.21 > 0.05)
                        thrpt:  [+0.5765% +7.3981% +20.292%]
                        No change in performance detected.
Found 15 outliers among 100 measurements (15.00%)
  7 (7.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  5 (5.00%) high severe
net_encryption/raw_aead/4096
                        time:   [2.8695 µs 2.8748 µs 2.8800 µs]
                        thrpt:  [1.3246 GiB/s 1.3269 GiB/s 1.3294 GiB/s]
                 change:
                        time:   [−15.053% −6.6607% −0.0133%] (p = 0.10 > 0.05)
                        thrpt:  [+0.0133% +7.1360% +17.721%]
                        No change in performance detected.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  5 (5.00%) high mild
  4 (4.00%) high severe
net_encryption/raw_ring/64
                        time:   [134.73 ns 134.97 ns 135.19 ns]
                        thrpt:  [451.48 MiB/s 452.20 MiB/s 453.00 MiB/s]
                 change:
                        time:   [−5.0422% −2.1439% −0.1920%] (p = 0.08 > 0.05)
                        thrpt:  [+0.1924% +2.1909% +5.3099%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  9 (9.00%) high mild
  1 (1.00%) high severe
net_encryption/raw_ring/256
                        time:   [196.65 ns 197.07 ns 197.39 ns]
                        thrpt:  [1.2078 GiB/s 1.2098 GiB/s 1.2124 GiB/s]
                 change:
                        time:   [−0.9245% −0.5862% −0.3123%] (p = 0.00 < 0.05)
                        thrpt:  [+0.3132% +0.5897% +0.9332%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
net_encryption/raw_ring/1024
                        time:   [446.96 ns 447.39 ns 447.90 ns]
                        thrpt:  [2.1292 GiB/s 2.1316 GiB/s 2.1337 GiB/s]
                 change:
                        time:   [−15.462% −7.0894% −1.0176%] (p = 0.06 > 0.05)
                        thrpt:  [+1.0281% +7.6304% +18.291%]
                        No change in performance detected.
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
net_encryption/raw_ring/4096
                        time:   [1.4024 µs 1.4043 µs 1.4063 µs]
                        thrpt:  [2.7126 GiB/s 2.7165 GiB/s 2.7201 GiB/s]
                 change:
                        time:   [−0.1081% +0.2060% +0.5782%] (p = 0.24 > 0.05)
                        thrpt:  [−0.5749% −0.2056% +0.1082%]
                        No change in performance detected.
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  3 (3.00%) high mild
  5 (5.00%) high severe

net_keypair/generate    time:   [10.819 µs 10.851 µs 10.879 µs]
                        thrpt:  [91.919 Kelem/s 92.157 Kelem/s 92.434 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high severe

net_aad/generate        time:   [1.0188 ns 1.0641 ns 1.1161 ns]
                        thrpt:  [895.99 Melem/s 939.77 Melem/s 981.58 Melem/s]

pool_comparison/shared_pool_get_return
                        time:   [40.754 ns 40.935 ns 41.123 ns]
                        thrpt:  [24.317 Melem/s 24.429 Melem/s 24.537 Melem/s]
pool_comparison/thread_local_pool_get_return
                        time:   [59.371 ns 60.006 ns 60.673 ns]
                        thrpt:  [16.482 Melem/s 16.665 Melem/s 16.843 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
pool_comparison/shared_pool_10x
                        time:   [379.70 ns 381.92 ns 384.25 ns]
                        thrpt:  [2.6025 Melem/s 2.6183 Melem/s 2.6336 Melem/s]
pool_comparison/thread_local_pool_10x
                        time:   [794.03 ns 802.06 ns 809.12 ns]
                        thrpt:  [1.2359 Melem/s 1.2468 Melem/s 1.2594 Melem/s]

cipher_comparison/shared_pool/64
                        time:   [212.82 ns 213.16 ns 213.53 ns]
                        thrpt:  [285.85 MiB/s 286.34 MiB/s 286.80 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
cipher_comparison/fast_chacha20/64
                        time:   [223.68 ns 223.96 ns 224.28 ns]
                        thrpt:  [272.14 MiB/s 272.52 MiB/s 272.87 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
cipher_comparison/shared_pool/256
                        time:   [277.33 ns 277.68 ns 278.07 ns]
                        thrpt:  [877.97 MiB/s 879.21 MiB/s 880.32 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild
cipher_comparison/fast_chacha20/256
                        time:   [287.35 ns 287.67 ns 288.03 ns]
                        thrpt:  [847.63 MiB/s 848.68 MiB/s 849.63 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
cipher_comparison/shared_pool/1024
                        time:   [540.45 ns 541.01 ns 541.62 ns]
                        thrpt:  [1.7608 GiB/s 1.7628 GiB/s 1.7646 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  2 (2.00%) high severe
cipher_comparison/fast_chacha20/1024
                        time:   [545.10 ns 546.58 ns 547.83 ns]
                        thrpt:  [1.7408 GiB/s 1.7448 GiB/s 1.7496 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) low severe
  2 (2.00%) high mild
  2 (2.00%) high severe
cipher_comparison/shared_pool/4096
                        time:   [1.5293 µs 1.5310 µs 1.5330 µs]
                        thrpt:  [2.4883 GiB/s 2.4916 GiB/s 2.4944 GiB/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
cipher_comparison/fast_chacha20/4096
                        time:   [1.5130 µs 1.5165 µs 1.5195 µs]
                        thrpt:  [2.5106 GiB/s 2.5154 GiB/s 2.5213 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  5 (5.00%) high mild
  1 (1.00%) high severe

adaptive_batcher/optimal_size
                        time:   [803.37 ps 804.51 ps 805.67 ps]
                        thrpt:  [1.2412 Gelem/s 1.2430 Gelem/s 1.2447 Gelem/s]
Found 23 outliers among 100 measurements (23.00%)
  12 (12.00%) low severe
  5 (5.00%) high mild
  6 (6.00%) high severe
adaptive_batcher/record time:   [9.4690 ns 9.4878 ns 9.5038 ns]
                        thrpt:  [105.22 Melem/s 105.40 Melem/s 105.61 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  9 (9.00%) low severe
  6 (6.00%) high mild
  5 (5.00%) high severe
adaptive_batcher/full_cycle
                        time:   [8.0386 ns 8.0509 ns 8.0628 ns]
                        thrpt:  [124.03 Melem/s 124.21 Melem/s 124.40 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  2 (2.00%) low severe
  3 (3.00%) high mild
  9 (9.00%) high severe

e2e_packet_build/shared_pool_50_events
                        time:   [1.4271 µs 1.4289 µs 1.4307 µs]
                        thrpt:  [2.0830 GiB/s 2.0857 GiB/s 2.0882 GiB/s]
Found 22 outliers among 100 measurements (22.00%)
  7 (7.00%) low severe
  2 (2.00%) low mild
  8 (8.00%) high mild
  5 (5.00%) high severe
e2e_packet_build/fast_50_events
                        time:   [1.4349 µs 1.4415 µs 1.4498 µs]
                        thrpt:  [2.0556 GiB/s 2.0675 GiB/s 2.0770 GiB/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low mild
  6 (6.00%) high mild
  4 (4.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.2s, enable flat sampling, or reduce sample count to 60.
multithread_packet_build/shared_pool/8
                        time:   [1.2154 ms 1.2241 ms 1.2320 ms]
                        thrpt:  [6.4936 Melem/s 6.5356 Melem/s 6.5823 Melem/s]
multithread_packet_build/thread_local_pool/8
                        time:   [547.89 µs 563.94 µs 580.98 µs]
                        thrpt:  [13.770 Melem/s 14.186 Melem/s 14.602 Melem/s]
multithread_packet_build/shared_pool/16
                        time:   [2.5082 ms 2.5168 ms 2.5252 ms]
                        thrpt:  [6.3362 Melem/s 6.3574 Melem/s 6.3790 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
multithread_packet_build/thread_local_pool/16
                        time:   [919.14 µs 929.65 µs 939.80 µs]
                        thrpt:  [17.025 Melem/s 17.211 Melem/s 17.408 Melem/s]
multithread_packet_build/shared_pool/24
                        time:   [3.9893 ms 4.0055 ms 4.0205 ms]
                        thrpt:  [5.9694 Melem/s 5.9918 Melem/s 6.0161 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
Benchmarking multithread_packet_build/thread_local_pool/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.3s, enable flat sampling, or reduce sample count to 60.
multithread_packet_build/thread_local_pool/24
                        time:   [1.2245 ms 1.2289 ms 1.2331 ms]
                        thrpt:  [19.464 Melem/s 19.529 Melem/s 19.600 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) low severe
  3 (3.00%) low mild
  4 (4.00%) high mild
multithread_packet_build/shared_pool/32
                        time:   [5.2833 ms 5.3050 ms 5.3267 ms]
                        thrpt:  [6.0075 Melem/s 6.0321 Melem/s 6.0569 Melem/s]
Benchmarking multithread_packet_build/thread_local_pool/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.7s, enable flat sampling, or reduce sample count to 50.
multithread_packet_build/thread_local_pool/32
                        time:   [1.5103 ms 1.5174 ms 1.5246 ms]
                        thrpt:  [20.989 Melem/s 21.088 Melem/s 21.188 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe

multithread_mixed_frames/shared_mixed/8
                        time:   [728.32 µs 739.21 µs 750.25 µs]
                        thrpt:  [15.995 Melem/s 16.234 Melem/s 16.476 Melem/s]
multithread_mixed_frames/fast_mixed/8
                        time:   [530.16 µs 543.79 µs 557.07 µs]
                        thrpt:  [21.541 Melem/s 22.067 Melem/s 22.635 Melem/s]
Benchmarking multithread_mixed_frames/shared_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.0s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/shared_mixed/16
                        time:   [1.3753 ms 1.3792 ms 1.3833 ms]
                        thrpt:  [17.350 Melem/s 17.401 Melem/s 17.450 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
multithread_mixed_frames/fast_mixed/16
                        time:   [856.16 µs 868.66 µs 881.28 µs]
                        thrpt:  [27.233 Melem/s 27.629 Melem/s 28.032 Melem/s]
multithread_mixed_frames/shared_mixed/24
                        time:   [2.0710 ms 2.0743 ms 2.0777 ms]
                        thrpt:  [17.327 Melem/s 17.355 Melem/s 17.383 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking multithread_mixed_frames/fast_mixed/24: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.0s, enable flat sampling, or reduce sample count to 60.
multithread_mixed_frames/fast_mixed/24
                        time:   [1.1686 ms 1.1795 ms 1.1903 ms]
                        thrpt:  [30.244 Melem/s 30.520 Melem/s 30.807 Melem/s]
multithread_mixed_frames/shared_mixed/32
                        time:   [2.7747 ms 2.7998 ms 2.8424 ms]
                        thrpt:  [16.887 Melem/s 17.144 Melem/s 17.299 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/32: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.5s, enable flat sampling, or reduce sample count to 50.
multithread_mixed_frames/fast_mixed/32
                        time:   [1.4769 ms 1.4859 ms 1.4948 ms]
                        thrpt:  [32.111 Melem/s 32.303 Melem/s 32.500 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

pool_contention/shared_acquire_release/8
                        time:   [10.303 ms 10.343 ms 10.383 ms]
                        thrpt:  [7.7051 Melem/s 7.7344 Melem/s 7.7650 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) low mild
  1 (1.00%) high mild
Benchmarking pool_contention/fast_acquire_release/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.1s, enable flat sampling, or reduce sample count to 60.
pool_contention/fast_acquire_release/8
                        time:   [1.1066 ms 1.1486 ms 1.1895 ms]
                        thrpt:  [67.253 Melem/s 69.653 Melem/s 72.295 Melem/s]
pool_contention/shared_acquire_release/16
                        time:   [21.592 ms 21.660 ms 21.729 ms]
                        thrpt:  [7.3634 Melem/s 7.3870 Melem/s 7.4100 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking pool_contention/fast_acquire_release/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.7s, enable flat sampling, or reduce sample count to 50.
pool_contention/fast_acquire_release/16
                        time:   [1.7451 ms 1.7565 ms 1.7685 ms]
                        thrpt:  [90.474 Melem/s 91.092 Melem/s 91.687 Melem/s]
pool_contention/shared_acquire_release/24
                        time:   [38.425 ms 38.590 ms 38.751 ms]
                        thrpt:  [6.1934 Melem/s 6.2193 Melem/s 6.2460 Melem/s]
pool_contention/fast_acquire_release/24
                        time:   [2.1041 ms 2.1261 ms 2.1494 ms]
                        thrpt:  [111.66 Melem/s 112.88 Melem/s 114.06 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
pool_contention/shared_acquire_release/32
                        time:   [49.261 ms 49.627 ms 50.083 ms]
                        thrpt:  [6.3894 Melem/s 6.4481 Melem/s 6.4961 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  1 (1.00%) high severe
pool_contention/fast_acquire_release/32
                        time:   [2.6184 ms 2.6460 ms 2.6731 ms]
                        thrpt:  [119.71 Melem/s 120.94 Melem/s 122.21 Melem/s]

throughput_scaling/fast_pool_scaling/1
                        time:   [1.4235 ms 1.4312 ms 1.4374 ms]
                        thrpt:  [1.3914 Melem/s 1.3974 Melem/s 1.4050 Melem/s]
Found 5 outliers among 20 measurements (25.00%)
  3 (15.00%) low mild
  2 (10.00%) high mild
throughput_scaling/fast_pool_scaling/2
                        time:   [1.5047 ms 1.5071 ms 1.5103 ms]
                        thrpt:  [2.6485 Melem/s 2.6541 Melem/s 2.6584 Melem/s]
throughput_scaling/fast_pool_scaling/4
                        time:   [1.5394 ms 1.5458 ms 1.5509 ms]
                        thrpt:  [5.1584 Melem/s 5.1752 Melem/s 5.1970 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) low mild
throughput_scaling/fast_pool_scaling/8
                        time:   [1.9283 ms 2.0913 ms 2.2680 ms]
                        thrpt:  [7.0546 Melem/s 7.6507 Melem/s 8.2974 Melem/s]
throughput_scaling/fast_pool_scaling/16
                        time:   [3.3139 ms 3.3343 ms 3.3539 ms]
                        thrpt:  [9.5412 Melem/s 9.5973 Melem/s 9.6564 Melem/s]
throughput_scaling/fast_pool_scaling/24
                        time:   [3.8604 ms 3.9903 ms 4.1315 ms]
                        thrpt:  [11.618 Melem/s 12.029 Melem/s 12.434 Melem/s]
throughput_scaling/fast_pool_scaling/32
                        time:   [5.0482 ms 5.1411 ms 5.2359 ms]
                        thrpt:  [12.223 Melem/s 12.449 Melem/s 12.678 Melem/s]

routing_header/serialize
                        time:   [465.63 ps 508.32 ps 555.28 ps]
                        thrpt:  [1.8009 Gelem/s 1.9673 Gelem/s 2.1476 Gelem/s]
routing_header/deserialize
                        time:   [713.27 ps 720.41 ps 729.03 ps]
                        thrpt:  [1.3717 Gelem/s 1.3881 Gelem/s 1.4020 Gelem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
routing_header/roundtrip
                        time:   [722.89 ps 735.15 ps 748.55 ps]
                        thrpt:  [1.3359 Gelem/s 1.3603 Gelem/s 1.3833 Gelem/s]
routing_header/forward  time:   [201.38 ps 201.75 ps 202.20 ps]
                        thrpt:  [4.9456 Gelem/s 4.9566 Gelem/s 4.9658 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  5 (5.00%) high mild
  6 (6.00%) high severe

routing_table/lookup_hit
                        time:   [38.089 ns 38.135 ns 38.183 ns]
                        thrpt:  [26.190 Melem/s 26.223 Melem/s 26.254 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
routing_table/lookup_miss
                        time:   [17.683 ns 17.706 ns 17.731 ns]
                        thrpt:  [56.398 Melem/s 56.477 Melem/s 56.552 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
routing_table/is_local  time:   [201.03 ps 201.29 ps 201.53 ps]
                        thrpt:  [4.9621 Gelem/s 4.9681 Gelem/s 4.9745 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  8 (8.00%) high mild
  4 (4.00%) high severe
routing_table/add_route time:   [37.298 ns 37.349 ns 37.403 ns]
                        thrpt:  [26.736 Melem/s 26.774 Melem/s 26.811 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
routing_table/record_in time:   [40.599 ns 40.648 ns 40.703 ns]
                        thrpt:  [24.568 Melem/s 24.602 Melem/s 24.631 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  5 (5.00%) high severe
routing_table/record_out
                        time:   [21.102 ns 21.174 ns 21.241 ns]
                        thrpt:  [47.078 Melem/s 47.228 Melem/s 47.388 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  6 (6.00%) low severe
  4 (4.00%) low mild
  9 (9.00%) high mild
  3 (3.00%) high severe
routing_table/aggregate_stats
                        time:   [6.1012 µs 6.1092 µs 6.1173 µs]
                        thrpt:  [163.47 Kelem/s 163.69 Kelem/s 163.90 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe

fair_scheduler/creation time:   [1.7386 µs 1.7416 µs 1.7440 µs]
                        thrpt:  [573.39 Kelem/s 574.19 Kelem/s 575.16 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  2 (2.00%) low severe
  6 (6.00%) high mild
  4 (4.00%) high severe
fair_scheduler/stream_count_empty
                        time:   [958.81 ns 960.03 ns 961.27 ns]
                        thrpt:  [1.0403 Melem/s 1.0416 Melem/s 1.0430 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
fair_scheduler/total_queued
                        time:   [229.42 ps 234.84 ps 240.24 ps]
                        thrpt:  [4.1625 Gelem/s 4.2582 Gelem/s 4.3587 Gelem/s]
fair_scheduler/cleanup_empty
                        time:   [1.2924 µs 1.2938 µs 1.2952 µs]
                        thrpt:  [772.10 Kelem/s 772.95 Kelem/s 773.73 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

routing_table_concurrent/concurrent_lookup/4
                        time:   [180.55 µs 181.94 µs 183.26 µs]
                        thrpt:  [21.827 Melem/s 21.985 Melem/s 22.154 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
routing_table_concurrent/concurrent_stats/4
                        time:   [224.57 µs 226.05 µs 227.69 µs]
                        thrpt:  [17.568 Melem/s 17.695 Melem/s 17.812 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  8 (8.00%) high mild
  6 (6.00%) high severe
routing_table_concurrent/concurrent_lookup/8
                        time:   [283.56 µs 287.24 µs 291.11 µs]
                        thrpt:  [27.481 Melem/s 27.851 Melem/s 28.213 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
routing_table_concurrent/concurrent_stats/8
                        time:   [323.26 µs 328.02 µs 333.04 µs]
                        thrpt:  [24.021 Melem/s 24.389 Melem/s 24.748 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
routing_table_concurrent/concurrent_lookup/16
                        time:   [514.18 µs 522.62 µs 531.07 µs]
                        thrpt:  [30.128 Melem/s 30.615 Melem/s 31.117 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
routing_table_concurrent/concurrent_stats/16
                        time:   [572.90 µs 582.35 µs 591.49 µs]
                        thrpt:  [27.050 Melem/s 27.475 Melem/s 27.928 Melem/s]

routing_decision/parse_lookup_forward
                        time:   [38.353 ns 38.471 ns 38.572 ns]
                        thrpt:  [25.925 Melem/s 25.993 Melem/s 26.073 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  9 (9.00%) low severe
  4 (4.00%) low mild
  4 (4.00%) high mild
  3 (3.00%) high severe
routing_decision/full_with_stats
                        time:   [99.669 ns 99.818 ns 99.949 ns]
                        thrpt:  [10.005 Melem/s 10.018 Melem/s 10.033 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
  7 (7.00%) high severe

stream_multiplexing/lookup_all/10
                        time:   [335.12 ns 337.21 ns 339.18 ns]
                        thrpt:  [29.482 Melem/s 29.655 Melem/s 29.840 Melem/s]
stream_multiplexing/stats_all/10
                        time:   [394.19 ns 396.54 ns 398.74 ns]
                        thrpt:  [25.079 Melem/s 25.218 Melem/s 25.368 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  16 (16.00%) low mild
  2 (2.00%) high mild
stream_multiplexing/lookup_all/100
                        time:   [3.3809 µs 3.3989 µs 3.4152 µs]
                        thrpt:  [29.281 Melem/s 29.421 Melem/s 29.578 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
stream_multiplexing/stats_all/100
                        time:   [3.9215 µs 3.9472 µs 3.9722 µs]
                        thrpt:  [25.175 Melem/s 25.334 Melem/s 25.500 Melem/s]
stream_multiplexing/lookup_all/1000
                        time:   [35.588 µs 35.773 µs 35.945 µs]
                        thrpt:  [27.821 Melem/s 27.954 Melem/s 28.099 Melem/s]
stream_multiplexing/stats_all/1000
                        time:   [42.620 µs 42.918 µs 43.220 µs]
                        thrpt:  [23.138 Melem/s 23.300 Melem/s 23.463 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low severe
  8 (8.00%) low mild
stream_multiplexing/lookup_all/10000
                        time:   [383.73 µs 386.08 µs 388.52 µs]
                        thrpt:  [25.739 Melem/s 25.901 Melem/s 26.060 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  3 (3.00%) low severe
  12 (12.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
stream_multiplexing/stats_all/10000
                        time:   [452.81 µs 455.29 µs 457.58 µs]
                        thrpt:  [21.854 Melem/s 21.964 Melem/s 22.084 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  17 (17.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

multihop_packet_builder/build/64
                        time:   [40.879 ns 41.097 ns 41.304 ns]
                        thrpt:  [1.4431 GiB/s 1.4503 GiB/s 1.4581 GiB/s]
multihop_packet_builder/build_priority/64
                        time:   [29.446 ns 29.613 ns 29.764 ns]
                        thrpt:  [2.0026 GiB/s 2.0128 GiB/s 2.0242 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild
multihop_packet_builder/build/256
                        time:   [44.078 ns 44.460 ns 44.810 ns]
                        thrpt:  [5.3206 GiB/s 5.3626 GiB/s 5.4090 GiB/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  16 (16.00%) high mild
multihop_packet_builder/build_priority/256
                        time:   [32.212 ns 32.347 ns 32.490 ns]
                        thrpt:  [7.3383 GiB/s 7.3705 GiB/s 7.4015 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
multihop_packet_builder/build/1024
                        time:   [44.071 ns 44.189 ns 44.313 ns]
                        thrpt:  [21.521 GiB/s 21.582 GiB/s 21.640 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
multihop_packet_builder/build_priority/1024
                        time:   [35.318 ns 35.378 ns 35.443 ns]
                        thrpt:  [26.908 GiB/s 26.957 GiB/s 27.002 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
multihop_packet_builder/build/4096
                        time:   [64.118 ns 64.470 ns 64.858 ns]
                        thrpt:  [58.816 GiB/s 59.170 GiB/s 59.495 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
multihop_packet_builder/build_priority/4096
                        time:   [53.581 ns 53.732 ns 53.890 ns]
                        thrpt:  [70.786 GiB/s 70.995 GiB/s 71.195 GiB/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe

multihop_chain/forward_chain/1
                        time:   [53.291 ns 53.417 ns 53.533 ns]
                        thrpt:  [18.680 Melem/s 18.721 Melem/s 18.765 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low severe
  4 (4.00%) high mild
  1 (1.00%) high severe
multihop_chain/forward_chain/2
                        time:   [88.458 ns 88.642 ns 88.829 ns]
                        thrpt:  [11.258 Melem/s 11.281 Melem/s 11.305 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
multihop_chain/forward_chain/3
                        time:   [120.85 ns 121.08 ns 121.32 ns]
                        thrpt:  [8.2424 Melem/s 8.2588 Melem/s 8.2747 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
multihop_chain/forward_chain/4
                        time:   [155.80 ns 156.17 ns 156.56 ns]
                        thrpt:  [6.3872 Melem/s 6.4033 Melem/s 6.4184 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
multihop_chain/forward_chain/5
                        time:   [189.01 ns 189.54 ns 190.17 ns]
                        thrpt:  [5.2585 Melem/s 5.2758 Melem/s 5.2907 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

hop_latency/single_hop_process
                        time:   [961.72 ps 966.73 ps 972.50 ps]
                        thrpt:  [1.0283 Gelem/s 1.0344 Gelem/s 1.0398 Gelem/s]
hop_latency/single_hop_full
                        time:   [34.064 ns 34.141 ns 34.220 ns]
                        thrpt:  [29.223 Melem/s 29.291 Melem/s 29.356 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

hop_scaling/64B_1hops   time:   [53.587 ns 53.762 ns 53.930 ns]
                        thrpt:  [1.1052 GiB/s 1.1087 GiB/s 1.1123 GiB/s]
hop_scaling/64B_2hops   time:   [84.628 ns 84.985 ns 85.374 ns]
                        thrpt:  [714.91 MiB/s 718.19 MiB/s 721.21 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
hop_scaling/64B_3hops   time:   [115.96 ns 116.24 ns 116.55 ns]
                        thrpt:  [523.69 MiB/s 525.07 MiB/s 526.33 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
hop_scaling/64B_4hops   time:   [148.34 ns 148.63 ns 148.94 ns]
                        thrpt:  [409.81 MiB/s 410.66 MiB/s 411.46 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
hop_scaling/64B_5hops   time:   [184.15 ns 184.47 ns 184.80 ns]
                        thrpt:  [330.27 MiB/s 330.86 MiB/s 331.43 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
hop_scaling/256B_1hops  time:   [53.621 ns 53.698 ns 53.780 ns]
                        thrpt:  [4.4332 GiB/s 4.4400 GiB/s 4.4463 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
hop_scaling/256B_2hops  time:   [87.414 ns 87.617 ns 87.827 ns]
                        thrpt:  [2.7146 GiB/s 2.7212 GiB/s 2.7275 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
hop_scaling/256B_3hops  time:   [122.97 ns 123.25 ns 123.57 ns]
                        thrpt:  [1.9295 GiB/s 1.9344 GiB/s 1.9389 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
hop_scaling/256B_4hops  time:   [154.13 ns 154.66 ns 155.20 ns]
                        thrpt:  [1.5362 GiB/s 1.5415 GiB/s 1.5468 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
hop_scaling/256B_5hops  time:   [190.48 ns 190.90 ns 191.35 ns]
                        thrpt:  [1.2460 GiB/s 1.2489 GiB/s 1.2517 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
hop_scaling/1024B_1hops time:   [54.645 ns 54.721 ns 54.806 ns]
                        thrpt:  [17.401 GiB/s 17.428 GiB/s 17.452 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
hop_scaling/1024B_2hops time:   [90.658 ns 90.824 ns 91.009 ns]
                        thrpt:  [10.479 GiB/s 10.500 GiB/s 10.519 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
hop_scaling/1024B_3hops time:   [125.49 ns 125.82 ns 126.18 ns]
                        thrpt:  [7.5580 GiB/s 7.5796 GiB/s 7.5995 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
hop_scaling/1024B_4hops time:   [160.53 ns 160.85 ns 161.18 ns]
                        thrpt:  [5.9169 GiB/s 5.9288 GiB/s 5.9409 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low severe
hop_scaling/1024B_5hops time:   [202.75 ns 203.21 ns 203.71 ns]
                        thrpt:  [4.6816 GiB/s 4.6930 GiB/s 4.7038 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

multihop_with_routing/route_and_forward/1
                        time:   [154.20 ns 154.47 ns 154.73 ns]
                        thrpt:  [6.4628 Melem/s 6.4738 Melem/s 6.4853 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low severe
  2 (2.00%) high mild
multihop_with_routing/route_and_forward/2
                        time:   [290.39 ns 290.76 ns 291.14 ns]
                        thrpt:  [3.4348 Melem/s 3.4393 Melem/s 3.4436 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high severe
multihop_with_routing/route_and_forward/3
                        time:   [423.93 ns 424.95 ns 426.45 ns]
                        thrpt:  [2.3449 Melem/s 2.3532 Melem/s 2.3589 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
multihop_with_routing/route_and_forward/4
                        time:   [556.16 ns 558.01 ns 559.49 ns]
                        thrpt:  [1.7873 Melem/s 1.7921 Melem/s 1.7981 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
multihop_with_routing/route_and_forward/5
                        time:   [693.76 ns 694.54 ns 695.39 ns]
                        thrpt:  [1.4380 Melem/s 1.4398 Melem/s 1.4414 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

multihop_concurrent/concurrent_forward/4
                        time:   [571.29 µs 580.86 µs 589.54 µs]
                        thrpt:  [6.7849 Melem/s 6.8864 Melem/s 7.0017 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
multihop_concurrent/concurrent_forward/8
                        time:   [755.61 µs 806.99 µs 864.09 µs]
                        thrpt:  [9.2583 Melem/s 9.9134 Melem/s 10.587 Melem/s]
multihop_concurrent/concurrent_forward/16
                        time:   [1.5187 ms 1.5263 ms 1.5317 ms]
                        thrpt:  [10.446 Melem/s 10.483 Melem/s 10.535 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe

pingwave/serialize      time:   [511.78 ps 518.93 ps 527.53 ps]
                        thrpt:  [1.8956 Gelem/s 1.9270 Gelem/s 1.9540 Gelem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  17 (17.00%) high severe
pingwave/deserialize    time:   [618.31 ps 636.21 ps 657.45 ps]
                        thrpt:  [1.5210 Gelem/s 1.5718 Gelem/s 1.6173 Gelem/s]
Found 24 outliers among 100 measurements (24.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  19 (19.00%) high severe
pingwave/roundtrip      time:   [619.92 ps 637.45 ps 658.60 ps]
                        thrpt:  [1.5184 Gelem/s 1.5687 Gelem/s 1.6131 Gelem/s]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
pingwave/forward        time:   [511.63 ps 518.69 ps 527.17 ps]
                        thrpt:  [1.8969 Gelem/s 1.9279 Gelem/s 1.9545 Gelem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) low mild
  17 (17.00%) high severe

capabilities/serialize_simple
                        time:   [36.992 ns 37.083 ns 37.182 ns]
                        thrpt:  [26.895 Melem/s 26.967 Melem/s 27.033 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
capabilities/deserialize_simple
                        time:   [14.328 ns 14.790 ns 15.262 ns]
                        thrpt:  [65.521 Melem/s 67.615 Melem/s 69.792 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  16 (16.00%) high mild
capabilities/serialize_complex
                        time:   [39.008 ns 39.098 ns 39.192 ns]
                        thrpt:  [25.515 Melem/s 25.577 Melem/s 25.636 Melem/s]
capabilities/deserialize_complex
                        time:   [237.47 ns 238.23 ns 238.98 ns]
                        thrpt:  [4.1845 Melem/s 4.1977 Melem/s 4.2111 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild

local_graph/create_pingwave
                        time:   [5.0375 ns 5.0430 ns 5.0492 ns]
                        thrpt:  [198.05 Melem/s 198.29 Melem/s 198.51 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
local_graph/on_pingwave_new
                        time:   [37.382 ns 37.916 ns 38.531 ns]
                        thrpt:  [25.953 Melem/s 26.374 Melem/s 26.751 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
local_graph/on_pingwave_duplicate
                        time:   [16.641 ns 17.086 ns 17.509 ns]
                        thrpt:  [57.115 Melem/s 58.527 Melem/s 60.092 Melem/s]
local_graph/get_node    time:   [19.734 ns 21.305 ns 22.614 ns]
                        thrpt:  [44.221 Melem/s 46.936 Melem/s 50.674 Melem/s]
local_graph/node_count  time:   [276.12 ps 278.17 ps 280.62 ps]
                        thrpt:  [3.5635 Gelem/s 3.5950 Gelem/s 3.6216 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
local_graph/stats       time:   [546.44 ps 548.43 ps 550.83 ps]
                        thrpt:  [1.8155 Gelem/s 1.8234 Gelem/s 1.8300 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe

graph_scaling/all_nodes/100
                        time:   [13.233 µs 13.270 µs 13.309 µs]
                        thrpt:  [7.5135 Melem/s 7.5356 Melem/s 7.5567 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
graph_scaling/nodes_within_hops/100
                        time:   [13.280 µs 13.311 µs 13.344 µs]
                        thrpt:  [7.4938 Melem/s 7.5126 Melem/s 7.5303 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
graph_scaling/all_nodes/500
                        time:   [25.972 µs 26.063 µs 26.152 µs]
                        thrpt:  [19.119 Melem/s 19.184 Melem/s 19.252 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
graph_scaling/nodes_within_hops/500
                        time:   [25.709 µs 25.939 µs 26.301 µs]
                        thrpt:  [19.011 Melem/s 19.276 Melem/s 19.448 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
graph_scaling/all_nodes/1000
                        time:   [42.965 µs 43.105 µs 43.246 µs]
                        thrpt:  [23.124 Melem/s 23.199 Melem/s 23.275 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
graph_scaling/nodes_within_hops/1000
                        time:   [41.541 µs 41.811 µs 42.071 µs]
                        thrpt:  [23.769 Melem/s 23.917 Melem/s 24.072 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
graph_scaling/all_nodes/5000
                        time:   [312.79 µs 316.56 µs 320.99 µs]
                        thrpt:  [15.577 Melem/s 15.795 Melem/s 15.985 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
graph_scaling/nodes_within_hops/5000
                        time:   [308.54 µs 311.17 µs 314.15 µs]
                        thrpt:  [15.916 Melem/s 16.068 Melem/s 16.205 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

capability_search/find_with_gpu
                        time:   [49.812 µs 49.928 µs 50.056 µs]
                        thrpt:  [19.978 Kelem/s 20.029 Kelem/s 20.075 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
capability_search/find_by_tool_python
                        time:   [100.35 µs 100.86 µs 101.44 µs]
                        thrpt:  [9.8584 Kelem/s 9.9150 Kelem/s 9.9654 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_search/find_by_tool_rust
                        time:   [126.90 µs 127.51 µs 128.15 µs]
                        thrpt:  [7.8031 Kelem/s 7.8428 Kelem/s 7.8804 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

graph_concurrent/concurrent_pingwave/4
                        time:   [309.99 µs 311.25 µs 312.41 µs]
                        thrpt:  [6.4019 Melem/s 6.4257 Melem/s 6.4518 Melem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high mild
graph_concurrent/concurrent_pingwave/8
                        time:   [469.96 µs 474.19 µs 478.47 µs]
                        thrpt:  [8.3599 Melem/s 8.4355 Melem/s 8.5113 Melem/s]
graph_concurrent/concurrent_pingwave/16
                        time:   [608.01 µs 685.28 µs 795.54 µs]
                        thrpt:  [10.056 Melem/s 11.674 Melem/s 13.158 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  4 (20.00%) low mild

path_finding/path_1_hop time:   [10.260 µs 10.410 µs 10.522 µs]
                        thrpt:  [95.042 Kelem/s 96.058 Kelem/s 97.465 Kelem/s]
path_finding/path_2_hops
                        time:   [10.717 µs 10.738 µs 10.763 µs]
                        thrpt:  [92.913 Kelem/s 93.125 Kelem/s 93.307 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
path_finding/path_4_hops
                        time:   [11.116 µs 11.133 µs 11.153 µs]
                        thrpt:  [89.664 Kelem/s 89.826 Kelem/s 89.964 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
path_finding/path_not_found
                        time:   [10.868 µs 10.879 µs 10.891 µs]
                        thrpt:  [91.819 Kelem/s 91.918 Kelem/s 92.015 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
path_finding/path_complex_graph
                        time:   [334.85 µs 335.84 µs 336.81 µs]
                        thrpt:  [2.9690 Kelem/s 2.9776 Kelem/s 2.9864 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

failure_detector/heartbeat_existing
                        time:   [69.140 ns 69.269 ns 69.414 ns]
                        thrpt:  [14.406 Melem/s 14.436 Melem/s 14.463 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
failure_detector/heartbeat_new
                        time:   [241.64 ns 243.74 ns 245.87 ns]
                        thrpt:  [4.0671 Melem/s 4.1027 Melem/s 4.1384 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  14 (14.00%) low severe
  3 (3.00%) low mild
  3 (3.00%) high mild
failure_detector/status_check
                        time:   [14.580 ns 15.174 ns 15.949 ns]
                        thrpt:  [62.701 Melem/s 65.903 Melem/s 68.587 Melem/s]
failure_detector/check_all
                        time:   [26.827 µs 28.012 µs 28.923 µs]
                        thrpt:  [34.574 Kelem/s 35.699 Kelem/s 37.276 Kelem/s]
failure_detector/stats  time:   [21.109 µs 21.162 µs 21.222 µs]
                        thrpt:  [47.120 Kelem/s 47.254 Kelem/s 47.374 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

loss_simulator/should_drop_1pct
                        time:   [16.516 ns 16.562 ns 16.613 ns]
                        thrpt:  [60.193 Melem/s 60.377 Melem/s 60.546 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
loss_simulator/should_drop_5pct
                        time:   [16.950 ns 16.981 ns 17.015 ns]
                        thrpt:  [58.773 Melem/s 58.891 Melem/s 58.998 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
loss_simulator/should_drop_10pct
                        time:   [17.499 ns 17.517 ns 17.537 ns]
                        thrpt:  [57.023 Melem/s 57.088 Melem/s 57.146 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
loss_simulator/should_drop_20pct
                        time:   [18.605 ns 18.658 ns 18.717 ns]
                        thrpt:  [53.428 Melem/s 53.596 Melem/s 53.749 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  5 (5.00%) high mild
  10 (10.00%) high severe
loss_simulator/should_drop_burst
                        time:   [17.017 ns 17.139 ns 17.336 ns]
                        thrpt:  [57.683 Melem/s 58.346 Melem/s 58.765 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) high mild
  3 (3.00%) high severe

circuit_breaker/allow_closed
                        time:   [11.093 ns 11.113 ns 11.140 ns]
                        thrpt:  [89.763 Melem/s 89.984 Melem/s 90.148 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe
circuit_breaker/record_success
                        time:   [11.305 ns 11.360 ns 11.428 ns]
                        thrpt:  [87.505 Melem/s 88.028 Melem/s 88.459 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high severe
circuit_breaker/record_failure
                        time:   [10.955 ns 11.037 ns 11.193 ns]
                        thrpt:  [89.340 Melem/s 90.601 Melem/s 91.287 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
circuit_breaker/state   time:   [11.969 ns 12.002 ns 12.034 ns]
                        thrpt:  [83.097 Melem/s 83.321 Melem/s 83.546 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

recovery_manager/on_failure_with_alternates
                        time:   [353.02 ns 354.87 ns 356.74 ns]
                        thrpt:  [2.8032 Melem/s 2.8180 Melem/s 2.8327 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  11 (11.00%) low severe
  3 (3.00%) low mild
recovery_manager/on_failure_no_alternates
                        time:   [233.30 ns 245.16 ns 268.65 ns]
                        thrpt:  [3.7223 Melem/s 4.0790 Melem/s 4.2863 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  9 (9.00%) low severe
  4 (4.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
recovery_manager/get_action
                        time:   [79.806 ns 80.773 ns 81.889 ns]
                        thrpt:  [12.212 Melem/s 12.380 Melem/s 12.530 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  3 (3.00%) high mild
  16 (16.00%) high severe
recovery_manager/is_failed
                        time:   [24.585 ns 24.647 ns 24.715 ns]
                        thrpt:  [40.462 Melem/s 40.572 Melem/s 40.676 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
recovery_manager/on_recovery
                        time:   [220.09 ns 236.60 ns 274.29 ns]
                        thrpt:  [3.6458 Melem/s 4.2265 Melem/s 4.5436 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
recovery_manager/stats  time:   [1.6971 ns 1.7008 ns 1.7049 ns]
                        thrpt:  [586.53 Melem/s 587.95 Melem/s 589.25 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe

failure_scaling/check_all/100
                        time:   [12.230 µs 12.272 µs 12.312 µs]
                        thrpt:  [8.1218 Melem/s 8.1487 Melem/s 8.1764 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
failure_scaling/healthy_nodes/100
                        time:   [11.408 µs 11.434 µs 11.463 µs]
                        thrpt:  [8.7236 Melem/s 8.7455 Melem/s 8.7661 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
failure_scaling/check_all/500
                        time:   [20.449 µs 20.516 µs 20.615 µs]
                        thrpt:  [24.255 Melem/s 24.371 Melem/s 24.451 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  18 (18.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
failure_scaling/healthy_nodes/500
                        time:   [15.204 µs 15.234 µs 15.267 µs]
                        thrpt:  [32.751 Melem/s 32.822 Melem/s 32.886 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe
failure_scaling/check_all/1000
                        time:   [30.202 µs 30.501 µs 30.731 µs]
                        thrpt:  [32.541 Melem/s 32.786 Melem/s 33.110 Melem/s]
failure_scaling/healthy_nodes/1000
                        time:   [22.189 µs 22.350 µs 22.622 µs]
                        thrpt:  [44.204 Melem/s 44.743 Melem/s 45.068 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
failure_scaling/check_all/5000
                        time:   [81.236 µs 87.514 µs 94.724 µs]
                        thrpt:  [52.785 Melem/s 57.133 Melem/s 61.549 Melem/s]
Found 22 outliers among 100 measurements (22.00%)
  20 (20.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
failure_scaling/healthy_nodes/5000
                        time:   [51.546 µs 51.754 µs 51.994 µs]
                        thrpt:  [96.166 Melem/s 96.611 Melem/s 97.000 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

failure_concurrent/concurrent_heartbeat/4
                        time:   [211.86 µs 215.76 µs 219.95 µs]
                        thrpt:  [9.0928 Melem/s 9.2694 Melem/s 9.4400 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
failure_concurrent/concurrent_heartbeat/8
                        time:   [333.44 µs 340.99 µs 349.51 µs]
                        thrpt:  [11.445 Melem/s 11.730 Melem/s 11.996 Melem/s]
failure_concurrent/concurrent_heartbeat/16
                        time:   [590.52 µs 604.87 µs 621.06 µs]
                        thrpt:  [12.881 Melem/s 13.226 Melem/s 13.547 Melem/s]

failure_recovery_cycle/full_cycle
                        time:   [275.80 ns 279.63 ns 283.71 ns]
                        thrpt:  [3.5248 Melem/s 3.5761 Melem/s 3.6258 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low mild
  1 (1.00%) high mild

capability_set/create   time:   [13.963 µs 13.980 µs 14.001 µs]
                        thrpt:  [71.425 Kelem/s 71.529 Kelem/s 71.619 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
capability_set/serialize
                        time:   [33.402 µs 33.480 µs 33.588 µs]
                        thrpt:  [29.772 Kelem/s 29.868 Kelem/s 29.938 Kelem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
capability_set/deserialize
                        time:   [8.0286 µs 8.0414 µs 8.0559 µs]
                        thrpt:  [124.13 Kelem/s 124.36 Kelem/s 124.55 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
capability_set/roundtrip
                        time:   [19.024 µs 19.062 µs 19.106 µs]
                        thrpt:  [52.341 Kelem/s 52.460 Kelem/s 52.566 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
capability_set/serialize_compact
                        time:   [2.0647 µs 2.0675 µs 2.0705 µs]
                        thrpt:  [482.98 Kelem/s 483.68 Kelem/s 484.34 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
capability_set/deserialize_compact
                        time:   [5.4519 µs 5.4600 µs 5.4691 µs]
                        thrpt:  [182.85 Kelem/s 183.15 Kelem/s 183.42 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
capability_set/roundtrip_compact
                        time:   [7.8319 µs 7.8463 µs 7.8639 µs]
                        thrpt:  [127.16 Kelem/s 127.45 Kelem/s 127.68 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
capability_set/has_tag  time:   [67.555 ns 67.864 ns 68.302 ns]
                        thrpt:  [14.641 Melem/s 14.735 Melem/s 14.803 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
capability_set/has_model
                        time:   [23.995 ns 24.073 ns 24.153 ns]
                        thrpt:  [41.403 Melem/s 41.541 Melem/s 41.675 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
capability_set/has_tool time:   [35.235 ns 35.405 ns 35.634 ns]
                        thrpt:  [28.063 Melem/s 28.244 Melem/s 28.381 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
capability_set/has_gpu  time:   [40.394 ns 40.587 ns 40.810 ns]
                        thrpt:  [24.504 Melem/s 24.639 Melem/s 24.756 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

capability_announcement/create
                        time:   [2.7442 µs 2.7479 µs 2.7522 µs]
                        thrpt:  [363.34 Kelem/s 363.92 Kelem/s 364.41 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
capability_announcement/serialize
                        time:   [11.467 µs 11.495 µs 11.527 µs]
                        thrpt:  [86.751 Kelem/s 86.993 Kelem/s 87.206 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
capability_announcement/deserialize
                        time:   [9.5657 µs 9.6005 µs 9.6493 µs]
                        thrpt:  [103.63 Kelem/s 104.16 Kelem/s 104.54 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
capability_announcement/is_expired
                        time:   [21.149 ns 21.176 ns 21.203 ns]
                        thrpt:  [47.164 Melem/s 47.224 Melem/s 47.283 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

capability_filter/match_single_tag
                        time:   [59.533 ns 59.724 ns 59.974 ns]
                        thrpt:  [16.674 Melem/s 16.744 Melem/s 16.797 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
capability_filter/match_require_gpu
                        time:   [42.691 ns 42.801 ns 42.928 ns]
                        thrpt:  [23.295 Melem/s 23.364 Melem/s 23.424 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
capability_filter/match_gpu_vendor
                        time:   [126.96 ns 127.19 ns 127.44 ns]
                        thrpt:  [7.8467 Melem/s 7.8622 Melem/s 7.8762 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  6 (6.00%) high mild
  1 (1.00%) high severe
capability_filter/match_min_memory
                        time:   [23.769 ns 23.804 ns 23.839 ns]
                        thrpt:  [41.948 Melem/s 42.010 Melem/s 42.071 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
capability_filter/match_complex
                        time:   [3.9161 µs 3.9229 µs 3.9304 µs]
                        thrpt:  [254.43 Kelem/s 254.91 Kelem/s 255.35 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  9 (9.00%) high mild
  2 (2.00%) high severe
capability_filter/match_no_match
                        time:   [76.750 ns 76.976 ns 77.278 ns]
                        thrpt:  [12.940 Melem/s 12.991 Melem/s 13.029 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe

capability_fold_insert/index_nodes/100
                        time:   [3.1771 ms 3.1872 ms 3.2002 ms]
                        thrpt:  [31.248 Kelem/s 31.376 Kelem/s 31.475 Kelem/s]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  5 (5.00%) high severe
capability_fold_insert/index_nodes/1000
                        time:   [30.216 ms 30.298 ms 30.410 ms]
                        thrpt:  [32.883 Kelem/s 33.006 Kelem/s 33.095 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 30.7s, or reduce sample count to 10.
capability_fold_insert/index_nodes/10000
                        time:   [306.68 ms 307.09 ms 307.53 ms]
                        thrpt:  [32.518 Kelem/s 32.564 Kelem/s 32.607 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

capability_fold_query/query_single_tag
                        time:   [90.788 µs 91.039 µs 91.339 µs]
                        thrpt:  [10.948 Kelem/s 10.984 Kelem/s 11.015 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
capability_fold_query/query_require_gpu
                        time:   [231.39 µs 232.41 µs 233.96 µs]
                        thrpt:  [4.2743 Kelem/s 4.3028 Kelem/s 4.3218 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe
capability_fold_query/query_gpu_vendor
                        time:   [304.83 µs 305.96 µs 307.18 µs]
                        thrpt:  [3.2554 Kelem/s 3.2684 Kelem/s 3.2805 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
capability_fold_query/query_min_memory
                        time:   [277.77 µs 278.98 µs 280.19 µs]
                        thrpt:  [3.5690 Kelem/s 3.5845 Kelem/s 3.6001 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_complex
                        time:   [181.42 µs 182.19 µs 182.96 µs]
                        thrpt:  [5.4658 Kelem/s 5.4888 Kelem/s 5.5120 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
capability_fold_query/query_model
                        time:   [58.308 µs 58.464 µs 58.629 µs]
                        thrpt:  [17.056 Kelem/s 17.105 Kelem/s 17.150 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_query/query_tool
                        time:   [230.96 µs 231.50 µs 232.04 µs]
                        thrpt:  [4.3095 Kelem/s 4.3196 Kelem/s 4.3298 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
capability_fold_query/query_no_results
                        time:   [86.276 ns 86.584 ns 87.064 ns]
                        thrpt:  [11.486 Melem/s 11.549 Melem/s 11.591 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe

capability_fold_find_best/find_best_simple
                        time:   [232.18 µs 232.59 µs 233.00 µs]
                        thrpt:  [4.2918 Kelem/s 4.2995 Kelem/s 4.3069 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
capability_fold_find_best/find_best_with_prefs
                        time:   [167.71 µs 168.32 µs 169.11 µs]
                        thrpt:  [5.9132 Kelem/s 5.9412 Kelem/s 5.9628 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe

capability_fold_scaling/query_tag/1000
                        time:   [7.9373 µs 7.9688 µs 8.0077 µs]
                        thrpt:  [62.440 Melem/s 62.744 Melem/s 62.994 Melem/s]
                 change:
                        time:   [−23.675% −18.666% −13.296%] (p = 0.00 < 0.05)
                        thrpt:  [+15.335% +22.950% +31.019%]
                        Performance has improved.
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_complex/1000
                        time:   [16.869 µs 16.940 µs 17.027 µs]
                        thrpt:  [26.546 Melem/s 26.683 Melem/s 26.794 Melem/s]
                 change:
                        time:   [+6.1396% +6.4715% +6.8548%] (p = 0.00 < 0.05)
                        thrpt:  [−6.4151% −6.0781% −5.7844%]
                        Performance has regressed.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_scaling/query_tag_rare/1000
                        time:   [1.5217 µs 1.5241 µs 1.5266 µs]
                        thrpt:  [65.504 Melem/s 65.614 Melem/s 65.715 Melem/s]
                 change:
                        time:   [−3.3945% −2.4847% −1.6890%] (p = 0.00 < 0.05)
                        thrpt:  [+1.7180% +2.5480% +3.5138%]
                        Performance has improved.
Found 9 outliers among 100 measurements (9.00%)
  3 (3.00%) low severe
  1 (1.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
capability_fold_scaling/query_tag/5000
                        time:   [80.474 µs 80.688 µs 80.905 µs]
                        thrpt:  [30.900 Melem/s 30.984 Melem/s 31.066 Melem/s]
                 change:
                        time:   [+42.873% +51.557% +60.795%] (p = 0.00 < 0.05)
                        thrpt:  [−37.809% −34.018% −30.008%]
                        Performance has regressed.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
capability_fold_scaling/query_complex/5000
                        time:   [166.19 µs 167.49 µs 169.84 µs]
                        thrpt:  [13.324 Melem/s 13.511 Melem/s 13.617 Melem/s]
                 change:
                        time:   [−1.0513% −0.3047% +0.6180%] (p = 0.51 > 0.05)
                        thrpt:  [−0.6142% +0.3057% +1.0625%]
                        No change in performance detected.
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
capability_fold_scaling/query_tag_rare/5000
                        time:   [2.8365 µs 2.8429 µs 2.8501 µs]
                        thrpt:  [35.087 Melem/s 35.175 Melem/s 35.254 Melem/s]
                 change:
                        time:   [−1.7037% −1.3092% −0.8837%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8916% +1.3266% +1.7332%]
                        Change within noise threshold.
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
capability_fold_scaling/query_tag/10000
                        time:   [171.38 µs 171.89 µs 172.45 µs]
                        thrpt:  [28.994 Melem/s 29.089 Melem/s 29.175 Melem/s]
                 change:
                        time:   [−1.6031% −0.9306% −0.2723%] (p = 0.01 < 0.05)
                        thrpt:  [+0.2731% +0.9394% +1.6293%]
                        Change within noise threshold.
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
capability_fold_scaling/query_complex/10000
                        time:   [340.78 µs 342.91 µs 345.74 µs]
                        thrpt:  [13.099 Melem/s 13.208 Melem/s 13.290 Melem/s]
                 change:
                        time:   [+82.456% +85.211% +89.027%] (p = 0.00 < 0.05)
                        thrpt:  [−47.098% −46.007% −45.192%]
                        Performance has regressed.
Found 11 outliers among 100 measurements (11.00%)
  8 (8.00%) high mild
  3 (3.00%) high severe
capability_fold_scaling/query_tag_rare/10000
                        time:   [2.8432 µs 2.8508 µs 2.8592 µs]
                        thrpt:  [34.975 Melem/s 35.078 Melem/s 35.171 Melem/s]
                 change:
                        time:   [−2.0175% −1.3626% −0.8489%] (p = 0.00 < 0.05)
                        thrpt:  [+0.8561% +1.3815% +2.0591%]
                        Change within noise threshold.
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_fold_scaling/query_tag/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.1s, enable flat sampling, or reduce sample count to 60.
capability_fold_scaling/query_tag/50000
                        time:   [1.2001 ms 1.2093 ms 1.2204 ms]
                        thrpt:  [20.485 Melem/s 20.672 Melem/s 20.831 Melem/s]
                 change:
                        time:   [−4.3484% −2.7850% −1.2603%] (p = 0.00 < 0.05)
                        thrpt:  [+1.2764% +2.8648% +4.5461%]
                        Performance has improved.
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
capability_fold_scaling/query_complex/50000
                        time:   [2.7154 ms 2.7328 ms 2.7532 ms]
                        thrpt:  [8.2284 Melem/s 8.2897 Melem/s 8.3429 Melem/s]
                 change:
                        time:   [+0.7844% +1.6504% +2.4884%] (p = 0.00 < 0.05)
                        thrpt:  [−2.4280% −1.6236% −0.7783%]
                        Change within noise threshold.
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) high mild
  6 (6.00%) high severe
capability_fold_scaling/query_tag_rare/50000
                        time:   [2.9293 µs 2.9360 µs 2.9430 µs]
                        thrpt:  [33.979 Melem/s 34.060 Melem/s 34.137 Melem/s]
                 change:
                        time:   [−2.7613% −2.2968% −1.8816%] (p = 0.00 < 0.05)
                        thrpt:  [+1.9177% +2.3508% +2.8397%]
                        Performance has improved.
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking capability_fold_concurrent/concurrent_index/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.4s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/4
                        time:   [25.379 ms 25.569 ms 25.756 ms]
                        thrpt:  [77.653 Kelem/s 78.218 Kelem/s 78.804 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
capability_fold_concurrent/concurrent_query/4
                        time:   [208.29 ms 209.95 ms 211.94 ms]
                        thrpt:  [9.4366 Kelem/s 9.5263 Kelem/s 9.6020 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_mixed/4
                        time:   [93.666 ms 94.082 ms 94.492 ms]
                        thrpt:  [21.166 Kelem/s 21.258 Kelem/s 21.353 Kelem/s]
Benchmarking capability_fold_concurrent/concurrent_index/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.0s, enable flat sampling, or reduce sample count to 10.
capability_fold_concurrent/concurrent_index/8
                        time:   [22.306 ms 23.200 ms 24.420 ms]
                        thrpt:  [163.80 Kelem/s 172.41 Kelem/s 179.32 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
capability_fold_concurrent/concurrent_query/8
                        time:   [222.67 ms 226.73 ms 231.18 ms]
                        thrpt:  [17.302 Kelem/s 17.642 Kelem/s 17.963 Kelem/s]
capability_fold_concurrent/concurrent_mixed/8
                        time:   [109.67 ms 113.31 ms 117.78 ms]
                        thrpt:  [33.962 Kelem/s 35.301 Kelem/s 36.472 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe
capability_fold_concurrent/concurrent_index/16
                        time:   [48.600 ms 49.383 ms 50.325 ms]
                        thrpt:  [158.97 Kelem/s 162.00 Kelem/s 164.61 Kelem/s]
Found 3 outliers among 20 measurements (15.00%)
  2 (10.00%) high mild
  1 (5.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.4s, or reduce sample count to 10.
capability_fold_concurrent/concurrent_query/16
                        time:   [504.18 ms 515.41 ms 525.42 ms]
                        thrpt:  [15.226 Kelem/s 15.522 Kelem/s 15.867 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
capability_fold_concurrent/concurrent_mixed/16
                        time:   [248.39 ms 254.00 ms 259.91 ms]
                        thrpt:  [30.780 Kelem/s 31.496 Kelem/s 32.207 Kelem/s]

capability_fold_updates/update_higher_version
                        time:   [37.372 µs 37.493 µs 37.627 µs]
                        thrpt:  [26.577 Kelem/s 26.672 Kelem/s 26.758 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
capability_fold_updates/update_same_version
                        time:   [37.399 µs 37.597 µs 37.909 µs]
                        thrpt:  [26.379 Kelem/s 26.598 Kelem/s 26.739 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
capability_fold_updates/remove_and_readd
                        time:   [58.504 µs 58.942 µs 59.645 µs]
                        thrpt:  [16.766 Kelem/s 16.966 Kelem/s 17.093 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe

location_info/create    time:   [115.05 ns 118.32 ns 122.35 ns]
                        thrpt:  [8.1732 Melem/s 8.4520 Melem/s 8.6921 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) high mild
  9 (9.00%) high severe
location_info/distance_to
                        time:   [6.1588 ns 6.2160 ns 6.3160 ns]
                        thrpt:  [158.33 Melem/s 160.87 Melem/s 162.37 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
location_info/same_continent
                        time:   [6.0165 ns 6.0388 ns 6.0641 ns]
                        thrpt:  [164.91 Melem/s 165.60 Melem/s 166.21 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  8 (8.00%) high mild
  1 (1.00%) high severe
location_info/same_continent_cross
                        time:   [408.08 ps 409.23 ps 410.52 ps]
                        thrpt:  [2.4359 Gelem/s 2.4436 Gelem/s 2.4505 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
location_info/same_region
                        time:   [4.9274 ns 4.9667 ns 5.0302 ns]
                        thrpt:  [198.80 Melem/s 201.34 Melem/s 202.95 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

topology_hints/create   time:   [4.9776 ns 5.0485 ns 5.1304 ns]
                        thrpt:  [194.92 Melem/s 198.08 Melem/s 200.90 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  18 (18.00%) high mild
  1 (1.00%) high severe
topology_hints/connectivity_score
                        time:   [272.77 ps 273.97 ps 275.45 ps]
                        thrpt:  [3.6304 Gelem/s 3.6500 Gelem/s 3.6661 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
topology_hints/average_latency_empty
                        time:   [818.52 ps 820.53 ps 822.74 ps]
                        thrpt:  [1.2154 Gelem/s 1.2187 Gelem/s 1.2217 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
topology_hints/average_latency_100
                        time:   [90.134 ns 90.925 ns 92.400 ns]
                        thrpt:  [10.823 Melem/s 10.998 Melem/s 11.095 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe

nat_type/difficulty     time:   [274.96 ps 276.54 ps 278.48 ps]
                        thrpt:  [3.5909 Gelem/s 3.6161 Gelem/s 3.6370 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
nat_type/can_connect_direct
                        time:   [274.08 ps 275.31 ps 276.80 ps]
                        thrpt:  [3.6127 Gelem/s 3.6323 Gelem/s 3.6485 Gelem/s]
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe
nat_type/can_connect_symmetric
                        time:   [274.05 ps 274.99 ps 276.12 ps]
                        thrpt:  [3.6216 Gelem/s 3.6365 Gelem/s 3.6489 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe

node_metadata/create_simple
                        time:   [57.578 ns 57.899 ns 58.255 ns]
                        thrpt:  [17.166 Melem/s 17.271 Melem/s 17.368 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
node_metadata/create_full
                        time:   [846.49 ns 849.67 ns 852.94 ns]
                        thrpt:  [1.1724 Melem/s 1.1769 Melem/s 1.1813 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
node_metadata/routing_score
                        time:   [274.25 ps 276.58 ps 279.45 ps]
                        thrpt:  [3.5785 Gelem/s 3.6156 Gelem/s 3.6463 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
node_metadata/age       time:   [38.270 ns 38.356 ns 38.450 ns]
                        thrpt:  [26.007 Melem/s 26.071 Melem/s 26.130 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  1 (1.00%) high mild
  12 (12.00%) high severe
node_metadata/is_stale  time:   [37.083 ns 37.103 ns 37.129 ns]
                        thrpt:  [26.933 Melem/s 26.952 Melem/s 26.967 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
node_metadata/serialize time:   [1.2012 µs 1.2124 µs 1.2266 µs]
                        thrpt:  [815.26 Kelem/s 824.81 Kelem/s 832.50 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe
node_metadata/deserialize
                        time:   [3.8715 µs 3.8928 µs 3.9260 µs]
                        thrpt:  [254.71 Kelem/s 256.89 Kelem/s 258.30 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe

metadata_query/match_status
                        time:   [5.3494 ns 5.3632 ns 5.3790 ns]
                        thrpt:  [185.91 Melem/s 186.45 Melem/s 186.94 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) high mild
  6 (6.00%) high severe
metadata_query/match_min_tier
                        time:   [5.2887 ns 5.2980 ns 5.3090 ns]
                        thrpt:  [188.36 Melem/s 188.75 Melem/s 189.08 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
metadata_query/match_continent
                        time:   [11.721 ns 11.818 ns 11.993 ns]
                        thrpt:  [83.385 Melem/s 84.619 Melem/s 85.314 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) high mild
  10 (10.00%) high severe
metadata_query/match_complex
                        time:   [11.465 ns 11.489 ns 11.517 ns]
                        thrpt:  [86.832 Melem/s 87.038 Melem/s 87.221 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
metadata_query/match_no_match
                        time:   [2.7101 ns 2.7134 ns 2.7173 ns]
                        thrpt:  [368.02 Melem/s 368.54 Melem/s 368.99 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe

metadata_store_basic/create
                        time:   [6.0266 µs 6.0399 µs 6.0548 µs]
                        thrpt:  [165.16 Kelem/s 165.57 Kelem/s 165.93 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  7 (7.00%) high mild
  1 (1.00%) high severe
metadata_store_basic/upsert_new
                        time:   [3.0578 µs 3.0728 µs 3.0884 µs]
                        thrpt:  [323.80 Kelem/s 325.43 Kelem/s 327.03 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  14 (14.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild
metadata_store_basic/upsert_existing
                        time:   [1.8689 µs 1.8768 µs 1.8856 µs]
                        thrpt:  [530.32 Kelem/s 532.82 Kelem/s 535.07 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
metadata_store_basic/get
                        time:   [44.185 ns 44.291 ns 44.410 ns]
                        thrpt:  [22.517 Melem/s 22.578 Melem/s 22.632 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe
metadata_store_basic/get_miss
                        time:   [44.222 ns 44.326 ns 44.441 ns]
                        thrpt:  [22.502 Melem/s 22.560 Melem/s 22.613 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  7 (7.00%) high mild
  3 (3.00%) high severe
metadata_store_basic/len
                        time:   [274.82 ps 275.91 ps 277.16 ps]
                        thrpt:  [3.6080 Gelem/s 3.6244 Gelem/s 3.6388 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
metadata_store_basic/stats
                        time:   [30.450 µs 30.534 µs 30.624 µs]
                        thrpt:  [32.654 Kelem/s 32.750 Kelem/s 32.840 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe

metadata_store_query/query_by_status
                        time:   [510.52 µs 513.81 µs 517.77 µs]
                        thrpt:  [1.9314 Kelem/s 1.9462 Kelem/s 1.9588 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe
metadata_store_query/query_by_continent
                        time:   [275.15 µs 276.11 µs 277.23 µs]
                        thrpt:  [3.6071 Kelem/s 3.6218 Kelem/s 3.6344 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
metadata_store_query/query_by_tier
                        time:   [729.53 µs 732.86 µs 736.66 µs]
                        thrpt:  [1.3575 Kelem/s 1.3645 Kelem/s 1.3707 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
metadata_store_query/query_accepting_work
                        time:   [815.68 µs 819.73 µs 824.18 µs]
                        thrpt:  [1.2133 Kelem/s 1.2199 Kelem/s 1.2260 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
metadata_store_query/query_with_limit
                        time:   [846.96 µs 855.52 µs 867.19 µs]
                        thrpt:  [1.1532 Kelem/s 1.1689 Kelem/s 1.1807 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
metadata_store_query/query_complex
                        time:   [472.06 µs 477.41 µs 485.91 µs]
                        thrpt:  [2.0580 Kelem/s 2.0946 Kelem/s 2.1184 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

metadata_store_spatial/find_nearby_100km
                        time:   [571.51 µs 574.93 µs 578.88 µs]
                        thrpt:  [1.7275 Kelem/s 1.7393 Kelem/s 1.7497 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
metadata_store_spatial/find_nearby_1000km
                        time:   [651.59 µs 654.47 µs 657.44 µs]
                        thrpt:  [1.5211 Kelem/s 1.5280 Kelem/s 1.5347 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking metadata_store_spatial/find_nearby_5000km: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.1s, enable flat sampling, or reduce sample count to 70.
metadata_store_spatial/find_nearby_5000km
                        time:   [1.0081 ms 1.0203 ms 1.0393 ms]
                        thrpt:  [962.22  elem/s 980.08  elem/s 991.99  elem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
metadata_store_spatial/find_best_for_routing
                        time:   [681.86 µs 684.15 µs 686.58 µs]
                        thrpt:  [1.4565 Kelem/s 1.4617 Kelem/s 1.4666 Kelem/s]
metadata_store_spatial/find_relays
                        time:   [836.37 µs 839.91 µs 843.82 µs]
                        thrpt:  [1.1851 Kelem/s 1.1906 Kelem/s 1.1956 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe

metadata_store_scaling/query_status/1000
                        time:   [44.600 µs 44.816 µs 45.190 µs]
                        thrpt:  [22.129 Kelem/s 22.313 Kelem/s 22.422 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
metadata_store_scaling/query_complex/1000
                        time:   [43.448 µs 43.542 µs 43.652 µs]
                        thrpt:  [22.908 Kelem/s 22.966 Kelem/s 23.016 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  6 (6.00%) high mild
  6 (6.00%) high severe
metadata_store_scaling/find_nearby/1000
                        time:   [92.739 µs 93.050 µs 93.373 µs]
                        thrpt:  [10.710 Kelem/s 10.747 Kelem/s 10.783 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
metadata_store_scaling/query_status/5000
                        time:   [239.02 µs 241.28 µs 245.10 µs]
                        thrpt:  [4.0799 Kelem/s 4.1446 Kelem/s 4.1837 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
metadata_store_scaling/query_complex/5000
                        time:   [238.42 µs 239.21 µs 240.05 µs]
                        thrpt:  [4.1658 Kelem/s 4.1805 Kelem/s 4.1943 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/find_nearby/5000
                        time:   [430.00 µs 431.08 µs 432.32 µs]
                        thrpt:  [2.3131 Kelem/s 2.3198 Kelem/s 2.3256 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_status/10000
                        time:   [506.02 µs 507.74 µs 509.56 µs]
                        thrpt:  [1.9625 Kelem/s 1.9695 Kelem/s 1.9762 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
metadata_store_scaling/query_complex/10000
                        time:   [514.16 µs 516.21 µs 518.58 µs]
                        thrpt:  [1.9283 Kelem/s 1.9372 Kelem/s 1.9449 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/find_nearby/10000
                        time:   [864.89 µs 867.64 µs 870.53 µs]
                        thrpt:  [1.1487 Kelem/s 1.1526 Kelem/s 1.1562 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
metadata_store_scaling/query_status/50000
                        time:   [4.7164 ms 4.8284 ms 4.9557 ms]
                        thrpt:  [201.79  elem/s 207.11  elem/s 212.02  elem/s]
Found 13 outliers among 100 measurements (13.00%)
  2 (2.00%) high mild
  11 (11.00%) high severe
metadata_store_scaling/query_complex/50000
                        time:   [4.6104 ms 4.7154 ms 4.8373 ms]
                        thrpt:  [206.73  elem/s 212.07  elem/s 216.90  elem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe
metadata_store_scaling/find_nearby/50000
                        time:   [5.1030 ms 5.1673 ms 5.2395 ms]
                        thrpt:  [190.86  elem/s 193.53  elem/s 195.96  elem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe

metadata_store_concurrent/concurrent_upsert/4
                        time:   [1.8568 ms 1.8743 ms 1.9038 ms]
                        thrpt:  [1.0505 Melem/s 1.0670 Melem/s 1.0771 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 9.2s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/4
                        time:   [456.96 ms 459.02 ms 461.08 ms]
                        thrpt:  [4.3376 Kelem/s 4.3571 Kelem/s 4.3768 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 9.3s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/4
                        time:   [459.14 ms 462.21 ms 465.19 ms]
                        thrpt:  [4.2994 Kelem/s 4.3270 Kelem/s 4.3559 Kelem/s]
metadata_store_concurrent/concurrent_upsert/8
                        time:   [2.4088 ms 2.4477 ms 2.4919 ms]
                        thrpt:  [1.6052 Melem/s 1.6342 Melem/s 1.6606 Melem/s]
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.3s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/8
                        time:   [507.28 ms 513.10 ms 519.29 ms]
                        thrpt:  [7.7028 Kelem/s 7.7957 Kelem/s 7.8852 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.7s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/8
                        time:   [524.51 ms 537.74 ms 554.68 ms]
                        thrpt:  [7.2114 Kelem/s 7.4385 Kelem/s 7.6262 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
metadata_store_concurrent/concurrent_upsert/16
                        time:   [4.9152 ms 4.9950 ms 5.0963 ms]
                        thrpt:  [1.5698 Melem/s 1.6016 Melem/s 1.6276 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 21.0s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_query/16
                        time:   [1.0367 s 1.0467 s 1.0584 s]
                        thrpt:  [7.5583 Kelem/s 7.6429 Kelem/s 7.7171 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 21.8s, or reduce sample count to 10.
metadata_store_concurrent/concurrent_mixed/16
                        time:   [1.0961 s 1.1094 s 1.1266 s]
                        thrpt:  [7.1009 Kelem/s 7.2113 Kelem/s 7.2986 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

metadata_store_versioning/update_versioned_success
                        time:   [462.91 ns 464.51 ns 466.49 ns]
                        thrpt:  [2.1437 Melem/s 2.1528 Melem/s 2.1603 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
metadata_store_versioning/update_versioned_conflict
                        time:   [461.98 ns 463.34 ns 464.92 ns]
                        thrpt:  [2.1509 Melem/s 2.1583 Melem/s 2.1646 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

schema_validation/validate_string
                        time:   [7.8059 ns 8.0695 ns 8.3408 ns]
                        thrpt:  [119.89 Melem/s 123.92 Melem/s 128.11 Melem/s]
schema_validation/validate_integer
                        time:   [9.0872 ns 9.3787 ns 9.6013 ns]
                        thrpt:  [104.15 Melem/s 106.63 Melem/s 110.04 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  3 (3.00%) high mild
  2 (2.00%) high severe
schema_validation/validate_object
                        time:   [59.022 ns 64.375 ns 70.294 ns]
                        thrpt:  [14.226 Melem/s 15.534 Melem/s 16.943 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
schema_validation/validate_array_10
                        time:   [60.042 ns 60.210 ns 60.418 ns]
                        thrpt:  [16.551 Melem/s 16.608 Melem/s 16.655 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
schema_validation/validate_complex
                        time:   [367.66 ns 373.73 ns 379.54 ns]
                        thrpt:  [2.6347 Melem/s 2.6757 Melem/s 2.7199 Melem/s]

endpoint_matching/match_success
                        time:   [429.01 ns 431.22 ns 433.71 ns]
                        thrpt:  [2.3057 Melem/s 2.3190 Melem/s 2.3310 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
endpoint_matching/match_failure
                        time:   [433.69 ns 436.00 ns 438.67 ns]
                        thrpt:  [2.2796 Melem/s 2.2936 Melem/s 2.3058 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe
endpoint_matching/match_multi_param
                        time:   [935.72 ns 940.27 ns 945.57 ns]
                        thrpt:  [1.0576 Melem/s 1.0635 Melem/s 1.0687 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  5 (5.00%) high mild
  7 (7.00%) high severe

api_version/is_compatible_with
                        time:   [274.04 ps 275.02 ps 276.10 ps]
                        thrpt:  [3.6219 Gelem/s 3.6361 Gelem/s 3.6491 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
api_version/parse       time:   [95.339 ns 96.300 ns 97.378 ns]
                        thrpt:  [10.269 Melem/s 10.384 Melem/s 10.489 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  3 (3.00%) high mild
  16 (16.00%) high severe
api_version/to_string   time:   [111.55 ns 112.83 ns 114.37 ns]
                        thrpt:  [8.7435 Melem/s 8.8627 Melem/s 8.9648 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  4 (4.00%) high mild
  17 (17.00%) high severe

api_schema/create       time:   [5.1167 µs 5.2500 µs 5.4056 µs]
                        thrpt:  [184.99 Kelem/s 190.48 Kelem/s 195.44 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  4 (4.00%) high mild
  7 (7.00%) high severe
api_schema/serialize    time:   [3.1912 µs 3.2092 µs 3.2282 µs]
                        thrpt:  [309.77 Kelem/s 311.60 Kelem/s 313.36 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) high mild
  1 (1.00%) high severe
api_schema/deserialize  time:   [15.131 µs 15.184 µs 15.245 µs]
                        thrpt:  [65.594 Kelem/s 65.858 Kelem/s 66.092 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
api_schema/find_endpoint
                        time:   [209.19 ns 211.44 ns 214.03 ns]
                        thrpt:  [4.6723 Melem/s 4.7294 Melem/s 4.7804 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
api_schema/endpoints_by_tag
                        time:   [217.71 ns 218.51 ns 219.40 ns]
                        thrpt:  [4.5580 Melem/s 4.5766 Melem/s 4.5933 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe

request_validation/validate_full_request
                        time:   [115.83 ns 117.20 ns 118.84 ns]
                        thrpt:  [8.4150 Melem/s 8.5327 Melem/s 8.6334 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
request_validation/validate_path_only
                        time:   [34.518 ns 35.281 ns 36.154 ns]
                        thrpt:  [27.659 Melem/s 28.344 Melem/s 28.971 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

api_registry_basic/create
                        time:   [3.0706 µs 3.0789 µs 3.0882 µs]
                        thrpt:  [323.81 Kelem/s 324.80 Kelem/s 325.67 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
api_registry_basic/register_new
                        time:   [7.1019 µs 7.1296 µs 7.1609 µs]
                        thrpt:  [139.65 Kelem/s 140.26 Kelem/s 140.81 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  10 (10.00%) low severe
  5 (5.00%) high mild
  2 (2.00%) high severe
api_registry_basic/get  time:   [44.145 ns 44.236 ns 44.345 ns]
                        thrpt:  [22.551 Melem/s 22.606 Melem/s 22.653 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe
api_registry_basic/len  time:   [275.96 ps 277.85 ps 280.04 ps]
                        thrpt:  [3.5710 Gelem/s 3.5991 Gelem/s 3.6237 Gelem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
api_registry_basic/stats
                        time:   [11.260 µs 11.306 µs 11.355 µs]
                        thrpt:  [88.068 Kelem/s 88.447 Kelem/s 88.808 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

api_registry_query/query_by_name
                        time:   [84.899 µs 85.270 µs 85.666 µs]
                        thrpt:  [11.673 Kelem/s 11.727 Kelem/s 11.779 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
api_registry_query/query_by_tag
                        time:   [749.86 µs 751.91 µs 754.11 µs]
                        thrpt:  [1.3261 Kelem/s 1.3299 Kelem/s 1.3336 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) high mild
  2 (2.00%) high severe
api_registry_query/query_with_version
                        time:   [40.748 µs 41.167 µs 41.636 µs]
                        thrpt:  [24.018 Kelem/s 24.291 Kelem/s 24.541 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
api_registry_query/find_by_endpoint
                        time:   [1.8480 ms 1.8721 ms 1.8985 ms]
                        thrpt:  [526.74  elem/s 534.17  elem/s 541.14  elem/s]
Found 17 outliers among 100 measurements (17.00%)
  7 (7.00%) high mild
  10 (10.00%) high severe
api_registry_query/find_compatible
                        time:   [60.052 µs 60.232 µs 60.416 µs]
                        thrpt:  [16.552 Kelem/s 16.602 Kelem/s 16.652 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe

api_registry_scaling/query_by_name/1000
                        time:   [7.5072 µs 7.5162 µs 7.5262 µs]
                        thrpt:  [132.87 Kelem/s 133.05 Kelem/s 133.21 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
api_registry_scaling/query_by_tag/1000
                        time:   [39.157 µs 39.203 µs 39.249 µs]
                        thrpt:  [25.478 Kelem/s 25.508 Kelem/s 25.538 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
api_registry_scaling/query_by_name/5000
                        time:   [38.017 µs 38.084 µs 38.162 µs]
                        thrpt:  [26.204 Kelem/s 26.258 Kelem/s 26.304 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
api_registry_scaling/query_by_tag/5000
                        time:   [263.01 µs 264.41 µs 265.86 µs]
                        thrpt:  [3.7614 Kelem/s 3.7819 Kelem/s 3.8021 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
api_registry_scaling/query_by_name/10000
                        time:   [83.795 µs 84.094 µs 84.405 µs]
                        thrpt:  [11.848 Kelem/s 11.891 Kelem/s 11.934 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
api_registry_scaling/query_by_tag/10000
                        time:   [738.04 µs 740.07 µs 742.41 µs]
                        thrpt:  [1.3470 Kelem/s 1.3512 Kelem/s 1.3549 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/4
                        time:   [489.12 ms 493.27 ms 498.08 ms]
                        thrpt:  [4.0154 Kelem/s 4.0546 Kelem/s 4.0890 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 8.4s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/4
                        time:   [419.77 ms 421.37 ms 422.93 ms]
                        thrpt:  [4.7289 Kelem/s 4.7464 Kelem/s 4.7645 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 11.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/8
                        time:   [550.80 ms 557.70 ms 564.57 ms]
                        thrpt:  [7.0850 Kelem/s 7.1723 Kelem/s 7.2621 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/8
                        time:   [487.90 ms 497.84 ms 508.12 ms]
                        thrpt:  [7.8722 Kelem/s 8.0348 Kelem/s 8.1984 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 15.0s, or reduce sample count to 10.
api_registry_concurrent/concurrent_query/16
                        time:   [1.1220 s 1.3369 s 1.5507 s]
                        thrpt:  [5.1590 Kelem/s 5.9838 Kelem/s 7.1299 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 32.6s, or reduce sample count to 10.
api_registry_concurrent/concurrent_mixed/16
                        time:   [1.6684 s 1.6986 s 1.7338 s]
                        thrpt:  [4.6142 Kelem/s 4.7097 Kelem/s 4.7950 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild

compare_op/eq           time:   [3.1853 ns 3.1913 ns 3.1993 ns]
                        thrpt:  [312.57 Melem/s 313.35 Melem/s 313.94 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  7 (7.00%) high mild
  6 (6.00%) high severe
compare_op/gt           time:   [2.8458 ns 2.8516 ns 2.8587 ns]
                        thrpt:  [349.81 Melem/s 350.68 Melem/s 351.40 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  5 (5.00%) high mild
  8 (8.00%) high severe
compare_op/contains_string
                        time:   [31.424 ns 32.040 ns 32.881 ns]
                        thrpt:  [30.413 Melem/s 31.211 Melem/s 31.822 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  4 (4.00%) high mild
  10 (10.00%) high severe
compare_op/in_array     time:   [8.4816 ns 8.6924 ns 8.9469 ns]
                        thrpt:  [111.77 Melem/s 115.04 Melem/s 117.90 Melem/s]
Found 23 outliers among 100 measurements (23.00%)
  8 (8.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  11 (11.00%) high severe

condition/simple        time:   [108.72 ns 110.51 ns 112.80 ns]
                        thrpt:  [8.8654 Melem/s 9.0493 Melem/s 9.1983 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
condition/nested_field  time:   [1.1969 µs 1.2166 µs 1.2411 µs]
                        thrpt:  [805.71 Kelem/s 821.97 Kelem/s 835.48 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
condition/string_eq     time:   [170.07 ns 172.28 ns 174.54 ns]
                        thrpt:  [5.7292 Melem/s 5.8047 Melem/s 5.8801 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

condition_expr/single   time:   [108.20 ns 109.10 ns 110.04 ns]
                        thrpt:  [9.0872 Melem/s 9.1659 Melem/s 9.2422 Melem/s]
condition_expr/and_2    time:   [222.91 ns 226.32 ns 230.21 ns]
                        thrpt:  [4.3440 Melem/s 4.4185 Melem/s 4.4861 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
condition_expr/and_5    time:   [683.11 ns 690.04 ns 697.50 ns]
                        thrpt:  [1.4337 Melem/s 1.4492 Melem/s 1.4639 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
condition_expr/or_3     time:   [176.85 ns 196.99 ns 219.77 ns]
                        thrpt:  [4.5501 Melem/s 5.0764 Melem/s 5.6544 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  3 (3.00%) high mild
  11 (11.00%) high severe
condition_expr/nested   time:   [281.36 ns 283.45 ns 285.65 ns]
                        thrpt:  [3.5007 Melem/s 3.5280 Melem/s 3.5542 Melem/s]

rule/create             time:   [781.36 ns 784.52 ns 788.16 ns]
                        thrpt:  [1.2688 Melem/s 1.2747 Melem/s 1.2798 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  8 (8.00%) high mild
  5 (5.00%) high severe
rule/matches            time:   [219.90 ns 221.55 ns 223.44 ns]
                        thrpt:  [4.4755 Melem/s 4.5137 Melem/s 4.5475 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) high mild

rule_context/create     time:   [2.7571 µs 2.7692 µs 2.7860 µs]
                        thrpt:  [358.94 Kelem/s 361.11 Kelem/s 362.70 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
rule_context/get_simple time:   [107.33 ns 109.32 ns 111.52 ns]
                        thrpt:  [8.9671 Melem/s 9.1474 Melem/s 9.3174 Melem/s]
rule_context/get_nested time:   [1.1812 µs 1.1884 µs 1.1969 µs]
                        thrpt:  [835.49 Kelem/s 841.45 Kelem/s 846.59 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) high mild
  8 (8.00%) high severe
rule_context/get_deep_nested
                        time:   [1.1962 µs 1.2154 µs 1.2413 µs]
                        thrpt:  [805.59 Kelem/s 822.76 Kelem/s 835.98 Kelem/s]
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) high mild
  14 (14.00%) high severe

rule_engine_basic/create
                        time:   [19.585 ns 19.921 ns 20.226 ns]
                        thrpt:  [49.441 Melem/s 50.197 Melem/s 51.061 Melem/s]
rule_engine_basic/add_rule
                        time:   [3.9874 µs 4.2019 µs 4.3941 µs]
                        thrpt:  [227.58 Kelem/s 237.99 Kelem/s 250.79 Kelem/s]
rule_engine_basic/get_rule
                        time:   [28.824 ns 29.260 ns 29.786 ns]
                        thrpt:  [33.573 Melem/s 34.177 Melem/s 34.694 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
rule_engine_basic/rules_by_tag
                        time:   [1.9079 µs 1.9239 µs 1.9512 µs]
                        thrpt:  [512.51 Kelem/s 519.77 Kelem/s 524.15 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
rule_engine_basic/stats time:   [16.516 µs 16.590 µs 16.673 µs]
                        thrpt:  [59.979 Kelem/s 60.278 Kelem/s 60.546 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  6 (6.00%) high mild
  4 (4.00%) high severe

rule_engine_evaluate/evaluate_10_rules
                        time:   [5.8311 µs 5.8572 µs 5.8822 µs]
                        thrpt:  [170.00 Kelem/s 170.73 Kelem/s 171.50 Kelem/s]
rule_engine_evaluate/evaluate_first_10_rules
                        time:   [663.79 ns 668.22 ns 672.94 ns]
                        thrpt:  [1.4860 Melem/s 1.4965 Melem/s 1.5065 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_100_rules
                        time:   [65.869 µs 67.031 µs 68.915 µs]
                        thrpt:  [14.511 Kelem/s 14.919 Kelem/s 15.182 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
rule_engine_evaluate/evaluate_first_100_rules
                        time:   [325.43 ns 327.48 ns 330.73 ns]
                        thrpt:  [3.0237 Melem/s 3.0536 Melem/s 3.0729 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low severe
  2 (2.00%) low mild
  1 (1.00%) high mild
  6 (6.00%) high severe
rule_engine_evaluate/evaluate_matching_100_rules
                        time:   [73.994 µs 74.553 µs 75.382 µs]
                        thrpt:  [13.266 Kelem/s 13.413 Kelem/s 13.515 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_1000_rules
                        time:   [606.15 µs 608.82 µs 611.38 µs]
                        thrpt:  [1.6356 Kelem/s 1.6425 Kelem/s 1.6498 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
rule_engine_evaluate/evaluate_first_1000_rules
                        time:   [662.69 ns 667.79 ns 673.40 ns]
                        thrpt:  [1.4850 Melem/s 1.4975 Melem/s 1.5090 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

rule_engine_scaling/evaluate/10
                        time:   [5.7810 µs 5.8019 µs 5.8239 µs]
                        thrpt:  [171.71 Kelem/s 172.36 Kelem/s 172.98 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_engine_scaling/evaluate_first/10
                        time:   [659.67 ns 663.40 ns 667.36 ns]
                        thrpt:  [1.4984 Melem/s 1.5074 Melem/s 1.5159 Melem/s]
rule_engine_scaling/evaluate/50
                        time:   [32.060 µs 32.189 µs 32.331 µs]
                        thrpt:  [30.931 Kelem/s 31.067 Kelem/s 31.192 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
rule_engine_scaling/evaluate_first/50
                        time:   [662.61 ns 666.98 ns 671.95 ns]
                        thrpt:  [1.4882 Melem/s 1.4993 Melem/s 1.5092 Melem/s]
rule_engine_scaling/evaluate/100
                        time:   [61.584 µs 61.843 µs 62.128 µs]
                        thrpt:  [16.096 Kelem/s 16.170 Kelem/s 16.238 Kelem/s]
rule_engine_scaling/evaluate_first/100
                        time:   [660.94 ns 664.39 ns 668.06 ns]
                        thrpt:  [1.4969 Melem/s 1.5051 Melem/s 1.5130 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
rule_engine_scaling/evaluate/500
                        time:   [308.35 µs 309.66 µs 311.04 µs]
                        thrpt:  [3.2151 Kelem/s 3.2293 Kelem/s 3.2431 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
rule_engine_scaling/evaluate_first/500
                        time:   [661.81 ns 665.53 ns 669.78 ns]
                        thrpt:  [1.4930 Melem/s 1.5026 Melem/s 1.5110 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  9 (9.00%) high mild
  3 (3.00%) high severe
rule_engine_scaling/evaluate/1000
                        time:   [603.14 µs 605.79 µs 608.69 µs]
                        thrpt:  [1.6429 Kelem/s 1.6507 Kelem/s 1.6580 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
rule_engine_scaling/evaluate_first/1000
                        time:   [663.47 ns 667.48 ns 672.03 ns]
                        thrpt:  [1.4880 Melem/s 1.4982 Melem/s 1.5072 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

rule_set/create         time:   [8.8670 µs 8.9039 µs 8.9483 µs]
                        thrpt:  [111.75 Kelem/s 112.31 Kelem/s 112.78 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
rule_set/load_into_engine
                        time:   [17.953 µs 18.009 µs 18.068 µs]
                        thrpt:  [55.346 Kelem/s 55.527 Kelem/s 55.701 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

trace_id/generate       time:   [88.698 ns 88.886 ns 89.103 ns]
                        thrpt:  [11.223 Melem/s 11.250 Melem/s 11.274 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
trace_id/to_hex         time:   [189.98 ns 190.92 ns 192.38 ns]
                        thrpt:  [5.1980 Melem/s 5.2377 Melem/s 5.2638 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  4 (4.00%) high mild
  8 (8.00%) high severe
trace_id/from_hex       time:   [38.068 ns 38.149 ns 38.245 ns]
                        thrpt:  [26.147 Melem/s 26.213 Melem/s 26.269 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  4 (4.00%) high mild
  6 (6.00%) high severe

context_operations/create
                        time:   [151.01 ns 151.50 ns 151.98 ns]
                        thrpt:  [6.5800 Melem/s 6.6007 Melem/s 6.6220 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
context_operations/child
                        time:   [61.921 ns 62.353 ns 63.137 ns]
                        thrpt:  [15.839 Melem/s 16.038 Melem/s 16.150 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
context_operations/for_remote
                        time:   [61.959 ns 62.119 ns 62.297 ns]
                        thrpt:  [16.052 Melem/s 16.098 Melem/s 16.140 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
context_operations/to_traceparent
                        time:   [535.92 ns 537.75 ns 539.78 ns]
                        thrpt:  [1.8526 Melem/s 1.8596 Melem/s 1.8659 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
context_operations/from_traceparent
                        time:   [238.07 ns 239.07 ns 240.15 ns]
                        thrpt:  [4.1640 Melem/s 4.1828 Melem/s 4.2005 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

baggage/create          time:   [4.9608 ns 5.0152 ns 5.1190 ns]
                        thrpt:  [195.35 Melem/s 199.40 Melem/s 201.58 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) high mild
  9 (9.00%) high severe
baggage/get             time:   [18.039 ns 18.749 ns 19.606 ns]
                        thrpt:  [51.005 Melem/s 53.337 Melem/s 55.434 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  1 (1.00%) high mild
  19 (19.00%) high severe
baggage/set             time:   [135.68 ns 136.33 ns 137.10 ns]
                        thrpt:  [7.2941 Melem/s 7.3350 Melem/s 7.3702 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
baggage/merge           time:   [2.8260 µs 2.8465 µs 2.8659 µs]
                        thrpt:  [348.93 Kelem/s 351.31 Kelem/s 353.86 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

span/create             time:   [155.35 ns 156.49 ns 157.73 ns]
                        thrpt:  [6.3399 Melem/s 6.3900 Melem/s 6.4369 Melem/s]
span/set_attribute      time:   [127.79 ns 130.51 ns 133.47 ns]
                        thrpt:  [7.4925 Melem/s 7.6622 Melem/s 7.8252 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
span/add_event          time:   [106.00 ns 154.47 ns 259.97 ns]
                        thrpt:  [3.8466 Melem/s 6.4738 Melem/s 9.4338 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
span/with_kind          time:   [156.50 ns 158.53 ns 160.78 ns]
                        thrpt:  [6.2199 Melem/s 6.3080 Melem/s 6.3900 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

context_store/create_context
                        time:   [348.41 ns 352.17 ns 357.38 ns]
                        thrpt:  [2.7982 Melem/s 2.8395 Melem/s 2.8702 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
context_store/get_context
                        time:   [67.591 ns 68.620 ns 69.839 ns]
                        thrpt:  [14.319 Melem/s 14.573 Melem/s 14.795 Melem/s]
context_store/add_span  time:   [216.43 ns 217.68 ns 219.34 ns]
                        thrpt:  [4.5591 Melem/s 4.5940 Melem/s 4.6204 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

propagation_context/from_context
                        time:   [1.4000 µs 1.4052 µs 1.4113 µs]
                        thrpt:  [708.55 Kelem/s 711.63 Kelem/s 714.31 Kelem/s]
Found 17 outliers among 100 measurements (17.00%)
  6 (6.00%) high mild
  11 (11.00%) high severe
propagation_context/to_context
                        time:   [890.46 ns 900.00 ns 913.19 ns]
                        thrpt:  [1.0951 Melem/s 1.1111 Melem/s 1.1230 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe

context_store_concurrent/concurrent_get
                        time:   [84.165 ns 84.535 ns 84.933 ns]
                        thrpt:  [11.774 Melem/s 11.829 Melem/s 11.881 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

endpoint/create         time:   [2.7401 ns 2.7643 ns 2.7926 ns]
                        thrpt:  [358.09 Melem/s 361.75 Melem/s 364.96 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  19 (19.00%) high severe
endpoint/create_with_config
                        time:   [180.37 ns 182.17 ns 184.19 ns]
                        thrpt:  [5.4292 Melem/s 5.4892 Melem/s 5.5443 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
endpoint/effective_weight
                        time:   [274.76 ps 277.11 ps 280.33 ps]
                        thrpt:  [3.5672 Gelem/s 3.6087 Gelem/s 3.6395 Gelem/s]
Found 13 outliers among 100 measurements (13.00%)
  4 (4.00%) high mild
  9 (9.00%) high severe

load_metrics/load_score time:   [274.74 ps 277.64 ps 281.06 ps]
                        thrpt:  [3.5579 Gelem/s 3.6018 Gelem/s 3.6398 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  3 (3.00%) high mild
  9 (9.00%) high severe
load_metrics/is_overloaded
                        time:   [275.69 ps 277.51 ps 279.56 ps]
                        thrpt:  [3.5770 Gelem/s 3.6035 Gelem/s 3.6272 Gelem/s]
Found 11 outliers among 100 measurements (11.00%)
  6 (6.00%) high mild
  5 (5.00%) high severe

lb_strategies/round_robin
                        time:   [610.18 ns 611.70 ns 613.48 ns]
                        thrpt:  [1.6300 Melem/s 1.6348 Melem/s 1.6389 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
lb_strategies/weighted_round_robin
                        time:   [663.53 ns 709.09 ns 756.06 ns]
                        thrpt:  [1.3226 Melem/s 1.4102 Melem/s 1.5071 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  11 (11.00%) low severe
  1 (1.00%) high mild
  1 (1.00%) high severe
lb_strategies/least_connections
                        time:   [558.27 ns 576.43 ns 590.25 ns]
                        thrpt:  [1.6942 Melem/s 1.7348 Melem/s 1.7913 Melem/s]
lb_strategies/random    time:   [650.11 ns 650.88 ns 651.72 ns]
                        thrpt:  [1.5344 Melem/s 1.5364 Melem/s 1.5382 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
lb_strategies/power_of_two
                        time:   [691.79 ns 692.40 ns 693.10 ns]
                        thrpt:  [1.4428 Melem/s 1.4443 Melem/s 1.4455 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
lb_strategies/consistent_hash
                        time:   [71.214 µs 71.333 µs 71.469 µs]
                        thrpt:  [13.992 Kelem/s 14.019 Kelem/s 14.042 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
lb_strategies/least_load
                        time:   [916.56 ns 921.63 ns 930.35 ns]
                        thrpt:  [1.0749 Melem/s 1.0850 Melem/s 1.0910 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe

lb_scaling/select/10    time:   [613.05 ns 614.38 ns 615.89 ns]
                        thrpt:  [1.6237 Melem/s 1.6277 Melem/s 1.6312 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe
lb_scaling/select/50    time:   [1.7469 µs 1.7516 µs 1.7569 µs]
                        thrpt:  [569.18 Kelem/s 570.92 Kelem/s 572.44 Kelem/s]
Found 10 outliers among 100 measurements (10.00%)
  5 (5.00%) high mild
  5 (5.00%) high severe
lb_scaling/select/100   time:   [3.1539 µs 3.1628 µs 3.1729 µs]
                        thrpt:  [315.17 Kelem/s 316.17 Kelem/s 317.07 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
lb_scaling/select/500   time:   [7.5557 µs 7.5688 µs 7.5831 µs]
                        thrpt:  [131.87 Kelem/s 132.12 Kelem/s 132.35 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe

lb_zone_aware/zone_match
                        time:   [703.27 ns 705.07 ns 707.05 ns]
                        thrpt:  [1.4143 Melem/s 1.4183 Melem/s 1.4219 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
lb_zone_aware/zone_fallback
                        time:   [612.97 ns 614.64 ns 616.45 ns]
                        thrpt:  [1.6222 Melem/s 1.6270 Melem/s 1.6314 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high mild

lb_health_updates/update_health
                        time:   [52.205 ns 52.308 ns 52.417 ns]
                        thrpt:  [19.078 Melem/s 19.117 Melem/s 19.155 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
lb_health_updates/update_metrics
                        time:   [188.23 ns 188.50 ns 188.79 ns]
                        thrpt:  [5.2969 Melem/s 5.3050 Melem/s 5.3125 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

     Running benches\origin_cache_bench.rs (target\release\deps\origin_cache_bench-02c0b05ab2db5544.exe)
Gnuplot not found, using plotters backend
origin_cache_hit/dashmap
                        time:   [30.566 ns 32.572 ns 34.236 ns]
origin_cache_hit/mutex_lru
                        time:   [17.832 ns 18.094 ns 18.454 ns]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

origin_cache_insert_256/dashmap
                        time:   [18.466 µs 18.650 µs 18.815 µs]
origin_cache_insert_256/mutex_lru
                        time:   [14.205 µs 14.379 µs 14.574 µs]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

     Running benches\parallel.rs (target\release\deps\parallel-b2a77491b57e38a9.exe)
Gnuplot not found, using plotters backend
shard_manager/ingest_json/1
                        time:   [459.15 ns 495.06 ns 524.59 ns]
                        thrpt:  [1.9063 Melem/s 2.0200 Melem/s 2.1779 Melem/s]
shard_manager/ingest_json/4
                        time:   [595.87 ns 599.49 ns 603.56 ns]
                        thrpt:  [1.6568 Melem/s 1.6681 Melem/s 1.6782 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
shard_manager/ingest_json/8
                        time:   [592.27 ns 594.86 ns 597.86 ns]
                        thrpt:  [1.6726 Melem/s 1.6811 Melem/s 1.6884 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
shard_manager/ingest_json/16
                        time:   [598.65 ns 605.13 ns 614.60 ns]
                        thrpt:  [1.6271 Melem/s 1.6525 Melem/s 1.6704 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
shard_manager/ingest_raw/1
                        time:   [165.23 ns 165.58 ns 165.96 ns]
                        thrpt:  [6.0254 Melem/s 6.0394 Melem/s 6.0523 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
shard_manager/ingest_raw/4
                        time:   [165.29 ns 165.68 ns 166.12 ns]
                        thrpt:  [6.0199 Melem/s 6.0358 Melem/s 6.0499 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe
shard_manager/ingest_raw/8
                        time:   [165.13 ns 166.54 ns 169.26 ns]
                        thrpt:  [5.9081 Melem/s 6.0047 Melem/s 6.0558 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
shard_manager/ingest_raw/16
                        time:   [165.07 ns 165.38 ns 165.71 ns]
                        thrpt:  [6.0345 Melem/s 6.0467 Melem/s 6.0579 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe

event_size/small_50b_json
                        time:   [507.24 ns 509.09 ns 511.21 ns]
                        thrpt:  [1.9562 Melem/s 1.9643 Melem/s 1.9714 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
event_size/small_50b_raw
                        time:   [164.82 ns 166.33 ns 169.27 ns]
                        thrpt:  [5.9076 Melem/s 6.0123 Melem/s 6.0674 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
event_size/medium_200b_json
                        time:   [1.1440 µs 1.1488 µs 1.1541 µs]
                        thrpt:  [866.47 Kelem/s 870.50 Kelem/s 874.11 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
event_size/medium_200b_raw
                        time:   [164.67 ns 164.91 ns 165.20 ns]
                        thrpt:  [6.0532 Melem/s 6.0639 Melem/s 6.0729 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  8 (8.00%) high mild
  4 (4.00%) high severe
event_size/large_1kb_json
                        time:   [4.5098 µs 4.5250 µs 4.5420 µs]
                        thrpt:  [220.17 Kelem/s 220.99 Kelem/s 221.74 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
event_size/large_1kb_raw
                        time:   [165.16 ns 165.70 ns 166.34 ns]
                        thrpt:  [6.0119 Melem/s 6.0351 Melem/s 6.0546 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe

parallel/threads/1      time:   [3.7427 ms 3.7828 ms 3.8233 ms]
                        thrpt:  [2.6155 Melem/s 2.6436 Melem/s 2.6719 Melem/s]
parallel/threads/2      time:   [2.5397 ms 2.5713 ms 2.6099 ms]
                        thrpt:  [7.6631 Melem/s 7.7783 Melem/s 7.8750 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) high mild
  8 (8.00%) high severe
parallel/threads/4      time:   [5.4490 ms 5.4801 ms 5.5128 ms]
                        thrpt:  [7.2559 Melem/s 7.2992 Melem/s 7.3408 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
parallel/threads/8      time:   [6.9147 ms 7.0040 ms 7.0992 ms]
                        thrpt:  [11.269 Melem/s 11.422 Melem/s 11.570 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

     Running benches\placement.rs (target\release\deps\placement-9ba385ee11556680.exe)
Gnuplot not found, using plotters backend
standard_placement_score/baseline_no_custom_filter/100
                        time:   [50.935 µs 55.435 µs 60.329 µs]
                        thrpt:  [1.6576 Melem/s 1.8039 Melem/s 1.9633 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
standard_placement_score/with_custom_filter_rust_callback/100
                        time:   [88.970 µs 89.276 µs 89.707 µs]
                        thrpt:  [1.1147 Melem/s 1.1201 Melem/s 1.1240 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
standard_placement_score/with_custom_filter_predicate/100
                        time:   [157.26 µs 157.71 µs 158.19 µs]
                        thrpt:  [632.17 Kelem/s 634.09 Kelem/s 635.91 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

     Running benches\redex.rs (target\release\deps\redex-b27d305bfd6f80f1.exe)
Gnuplot not found, using plotters backend
redex_append_inline/heap_file
                        time:   [44.567 ns 45.194 ns 45.862 ns]
                        thrpt:  [21.805 Melem/s 22.127 Melem/s 22.438 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_heap/heap_file/32
                        time:   [50.127 ns 50.519 ns 50.965 ns]
                        thrpt:  [598.79 MiB/s 604.08 MiB/s 608.81 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
redex_append_heap/heap_file/256
                        time:   [110.67 ns 113.49 ns 116.29 ns]
                        thrpt:  [2.0503 GiB/s 2.1008 GiB/s 2.1543 GiB/s]
redex_append_heap/heap_file/1024
                        time:   [314.46 ns 323.16 ns 333.07 ns]
                        thrpt:  [2.8633 GiB/s 2.9511 GiB/s 3.0328 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_watcher_paths/no_watchers
                        time:   [111.55 ns 114.52 ns 117.45 ns]
                        thrpt:  [2.0299 GiB/s 2.0818 GiB/s 2.1373 GiB/s]
redex_append_watcher_paths/with_tail
                        time:   [261.89 ns 267.01 ns 271.77 ns]
                        thrpt:  [898.32 MiB/s 914.34 MiB/s 932.21 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_batch/batch_64_x_64B
                        time:   [2.7632 µs 2.8448 µs 2.9380 µs]
                        thrpt:  [21.783 Melem/s 22.497 Melem/s 23.162 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

redex_append_disk/disk_file/32
                        time:   [4.1864 µs 4.2016 µs 4.2184 µs]
                        thrpt:  [7.2343 MiB/s 7.2634 MiB/s 7.2897 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
redex_append_disk/disk_file/256
                        time:   [4.4488 µs 4.4746 µs 4.5013 µs]
                        thrpt:  [54.238 MiB/s 54.562 MiB/s 54.878 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) high mild
  4 (4.00%) high severe
redex_append_disk/disk_file/1024
                        time:   [5.2898 µs 5.4184 µs 5.5834 µs]
                        thrpt:  [174.90 MiB/s 180.23 MiB/s 184.61 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe

redex_append_batch_disk/batch_64_x/64
                        time:   [12.281 µs 12.562 µs 12.852 µs]
                        thrpt:  [4.9798 Melem/s 5.0945 Melem/s 5.2111 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
redex_append_batch_disk/batch_64_x/1024
                        time:   [47.076 µs 48.514 µs 50.367 µs]
                        thrpt:  [1.2707 Melem/s 1.3192 Melem/s 1.3595 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

redex_append_disk_policies/disk_file_256B/never
                        time:   [4.4668 µs 4.5385 µs 4.6752 µs]
                        thrpt:  [52.221 MiB/s 53.793 MiB/s 54.657 MiB/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  7 (7.00%) high severe
redex_append_disk_policies/disk_file_256B/every_n_1
                        time:   [1.0502 ms 1.0710 ms 1.0952 ms]
                        thrpt:  [228.26 KiB/s 233.43 KiB/s 238.05 KiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low severe
  1 (1.00%) low mild
  3 (3.00%) high mild
redex_append_disk_policies/disk_file_256B/every_n_64
                        time:   [32.710 µs 43.977 µs 54.596 µs]
                        thrpt:  [4.4718 MiB/s 5.5515 MiB/s 7.4637 MiB/s]
Found 17 outliers among 100 measurements (17.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  13 (13.00%) high severe
redex_append_disk_policies/disk_file_256B/interval_50ms
                        time:   [4.7254 µs 4.7994 µs 4.8837 µs]
                        thrpt:  [49.991 MiB/s 50.869 MiB/s 51.665 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
redex_append_disk_policies/disk_file_256B/interval_or_bytes
                        time:   [5.8309 µs 5.9413 µs 6.0978 µs]
                        thrpt:  [40.037 MiB/s 41.092 MiB/s 41.870 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high severe

redex_append_batch_disk_policies/batch_64_x_64B/never
                        time:   [12.980 µs 13.531 µs 14.175 µs]
                        thrpt:  [4.5149 Melem/s 4.7300 Melem/s 4.9308 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
redex_append_batch_disk_policies/batch_64_x_64B/every_n_1
                        time:   [4.1367 ms 4.2456 ms 4.3495 ms]
                        thrpt:  [14.714 Kelem/s 15.074 Kelem/s 15.471 Kelem/s]
Found 11 outliers among 100 measurements (11.00%)
  1 (1.00%) low severe
  5 (5.00%) low mild
  4 (4.00%) high mild
  1 (1.00%) high severe
redex_append_batch_disk_policies/batch_64_x_64B/interval_or_bytes_small
                        time:   [4.1951 ms 4.3436 ms 4.4891 ms]
                        thrpt:  [14.257 Kelem/s 14.734 Kelem/s 15.256 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe

redex_tail/append_to_next
                        time:   [215.72 ns 216.39 ns 217.16 ns]
                        thrpt:  [4.6049 Melem/s 4.6212 Melem/s 4.6357 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  3 (3.00%) high severe