  7 (7.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking shard/ingest_raw_pop/8192: Collecting 100 samples in estimated 5.0001 s (37shard/ingest_raw_pop/8192
                        time:   [135.43 ns 135.90 ns 136.41 ns]
                        thrpt:  [7.3310 Melem/s 7.3582 Melem/s 7.3841 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking shard/ingest_raw/65536: Collecting 100 samples in estimated 5.0002 s (29M ishard/ingest_raw/65536  time:   [162.36 ns 163.42 ns 164.35 ns]
                        thrpt:  [6.0846 Melem/s 6.1191 Melem/s 6.1591 Melem/s]
Found 14 outliers among 100 measurements (14.00%)
  1 (1.00%) low severe
  13 (13.00%) low mild
Benchmarking shard/ingest_raw_pop/65536: Collecting 100 samples in estimated 5.0006 s (3shard/ingest_raw_pop/65536
                        time:   [135.71 ns 136.19 ns 136.68 ns]
                        thrpt:  [7.3163 Melem/s 7.3427 Melem/s 7.3685 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking shard/ingest_raw/1048576: Collecting 100 samples in estimated 5.0007 s (31Mshard/ingest_raw/1048576
                        time:   [116.57 ns 117.27 ns 118.02 ns]
                        thrpt:  [8.4729 Melem/s 8.5271 Melem/s 8.5786 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking shard/ingest_raw_pop/1048576: Collecting 100 samples in estimated 5.0002 s shard/ingest_raw_pop/1048576
                        time:   [139.22 ns 139.65 ns 140.10 ns]
                        thrpt:  [7.1377 Melem/s 7.1607 Melem/s 7.1831 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking timestamp/next: Collecting 100 samples in estimated 5.0001 s (157M iteratiotimestamp/next          time:   [31.694 ns 31.820 ns 31.943 ns]
                        thrpt:  [31.306 Melem/s 31.427 Melem/s 31.552 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking timestamp/now_raw: Collecting 100 samples in estimated 5.0000 s (556M iteratimestamp/now_raw       time:   [8.9787 ns 9.0075 ns 9.0382 ns]
                        thrpt:  [110.64 Melem/s 111.02 Melem/s 111.37 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking event/internal_event_new: Collecting 100 samples in estimated 5.0018 s (11Mevent/internal_event_new
                        time:   [434.68 ns 436.95 ns 439.41 ns]
                        thrpt:  [2.2758 Melem/s 2.2886 Melem/s 2.3005 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking event/json_creation: Collecting 100 samples in estimated 5.0004 s (19M iterevent/json_creation     time:   [262.35 ns 263.54 ns 264.79 ns]
                        thrpt:  [3.7765 Melem/s 3.7945 Melem/s 3.8118 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking batch/pop_batch_steady_state/100: Collecting 100 samples in estimated 5.036batch/pop_batch_steady_state/100
                        time:   [11.149 µs 11.196 µs 11.245 µs]
                        thrpt:  [8.8927 Melem/s 8.9317 Melem/s 8.9696 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking batch/pop_batch_steady_state/1000: Collecting 100 samples in estimated 5.08batch/pop_batch_steady_state/1000
                        time:   [111.19 µs 111.66 µs 112.16 µs]
                        thrpt:  [8.9157 Melem/s 8.9560 Melem/s 8.9939 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking batch/pop_batch_steady_state/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.8s, enable flat sampling, or reduce sample count to 60.
Benchmarking batch/pop_batch_steady_state/10000: Collecting 100 samples in estimated 5.7batch/pop_batch_steady_state/10000
                        time:   [1.1390 ms 1.1442 ms 1.1496 ms]
                        thrpt:  [8.6990 Melem/s 8.7400 Melem/s 8.7793 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

     Running benches\mesh.rs (target\release\deps\mesh-6ec41fc9042e357e.exe)
Gnuplot not found, using plotters backend
Benchmarking mesh_reroute/triangle_failure: Collecting 100 samples in estimated 5.1148 smesh_reroute/triangle_failure
                        time:   [26.760 µs 29.292 µs 31.912 µs]
                        thrpt:  [31.336 Kelem/s 34.139 Kelem/s 37.369 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  8 (8.00%) high severe
Benchmarking mesh_reroute/10_peers_10_routes: Collecting 100 samples in estimated 5.0345mesh_reroute/10_peers_10_routes
                        time:   [242.46 µs 244.82 µs 247.36 µs]
                        thrpt:  [4.0427 Kelem/s 4.0847 Kelem/s 4.1244 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking mesh_reroute/50_peers_100_routes: Collecting 100 samples in estimated 5.238mesh_reroute/50_peers_100_routes
                        time:   [2.3219 ms 2.3307 ms 2.3396 ms]
                        thrpt:  [427.42  elem/s 429.06  elem/s 430.69  elem/s]

Benchmarking mesh_proximity/on_pingwave_new: Collecting 100 samples in estimated 5.0003 mesh_proximity/on_pingwave_new
                        time:   [265.57 ns 269.83 ns 273.99 ns]
                        thrpt:  [3.6497 Melem/s 3.7061 Melem/s 3.7655 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
Benchmarking mesh_proximity/on_pingwave_dedup: Collecting 100 samples in estimated 5.000mesh_proximity/on_pingwave_dedup
                        time:   [94.424 ns 94.838 ns 95.250 ns]
                        thrpt:  [10.499 Melem/s 10.544 Melem/s 10.591 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking mesh_proximity/pingwave_serialize: Collecting 100 samples in estimated 5.00mesh_proximity/pingwave_serialize
                        time:   [1.5864 ns 1.6341 ns 1.6873 ns]
                        thrpt:  [592.67 Melem/s 611.95 Melem/s 630.35 Melem/s]
Benchmarking mesh_proximity/pingwave_deserialize: Collecting 100 samples in estimated 5.mesh_proximity/pingwave_deserialize
                        time:   [2.0539 ns 2.0897 ns 2.1290 ns]
                        thrpt:  [469.71 Melem/s 478.53 Melem/s 486.87 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking mesh_proximity/node_count: Collecting 100 samples in estimated 5.0045 s (3.mesh_proximity/node_count
                        time:   [1.4597 µs 1.4655 µs 1.4716 µs]
                        thrpt:  [679.53 Kelem/s 682.37 Kelem/s 685.05 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking mesh_proximity/all_nodes_100: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 45.3s, or reduce sample count to 10.
Benchmarking mesh_proximity/all_nodes_100: Collecting 100 samples in estimated 45.318 s mesh_proximity/all_nodes_100
                        time:   [429.05 ms 430.58 ms 432.14 ms]
                        thrpt:  [2.3140  elem/s 2.3224  elem/s 2.3307  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking mesh_dispatch/classify_direct: Collecting 100 samples in estimated 5.0000 smesh_dispatch/classify_direct
                        time:   [698.82 ps 701.52 ps 704.26 ps]
                        thrpt:  [1.4199 Gelem/s 1.4255 Gelem/s 1.4310 Gelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_dispatch/classify_routed: Collecting 100 samples in estimated 5.0000 smesh_dispatch/classify_routed
                        time:   [568.82 ps 571.75 ps 574.68 ps]
                        thrpt:  [1.7401 Gelem/s 1.7490 Gelem/s 1.7580 Gelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking mesh_dispatch/classify_pingwave: Collecting 100 samples in estimated 5.0000mesh_dispatch/classify_pingwave
                        time:   [357.26 ps 358.44 ps 359.59 ps]
                        thrpt:  [2.7810 Gelem/s 2.7899 Gelem/s 2.7991 Gelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking mesh_routing/lookup_hit: Collecting 100 samples in estimated 5.0000 s (174Mmesh_routing/lookup_hit time:   [29.030 ns 29.298 ns 29.601 ns]
                        thrpt:  [33.783 Melem/s 34.132 Melem/s 34.447 Melem/s]
Found 19 outliers among 100 measurements (19.00%)
  19 (19.00%) high mild
Benchmarking mesh_routing/lookup_miss: Collecting 100 samples in estimated 5.0000 s (174mesh_routing/lookup_miss
                        time:   [29.006 ns 29.284 ns 29.605 ns]
                        thrpt:  [33.778 Melem/s 34.148 Melem/s 34.475 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking mesh_routing/is_local: Collecting 100 samples in estimated 5.0000 s (8.8B imesh_routing/is_local   time:   [566.19 ps 568.66 ps 571.25 ps]
                        thrpt:  [1.7505 Gelem/s 1.7585 Gelem/s 1.7662 Gelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_routing/all_routes/10: Collecting 100 samples in estimated 5.0288 s (4mesh_routing/all_routes/10
                        time:   [10.683 µs 10.723 µs 10.765 µs]
                        thrpt:  [92.898 Kelem/s 93.260 Kelem/s 93.608 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking mesh_routing/all_routes/100: Collecting 100 samples in estimated 5.0191 s (mesh_routing/all_routes/100
                        time:   [13.036 µs 13.083 µs 13.131 µs]
                        thrpt:  [76.154 Kelem/s 76.436 Kelem/s 76.708 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking mesh_routing/all_routes/1000: Collecting 100 samples in estimated 5.1635 s mesh_routing/all_routes/1000
                        time:   [34.647 µs 34.747 µs 34.845 µs]
                        thrpt:  [28.699 Kelem/s 28.780 Kelem/s 28.862 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking mesh_routing/add_route: Collecting 100 samples in estimated 5.0001 s (67M imesh_routing/add_route  time:   [73.838 ns 74.132 ns 74.442 ns]
                        thrpt:  [13.433 Melem/s 13.489 Melem/s 13.543 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

     Running benches\net.rs (target\release\deps\net-2db728c885e61484.exe)
Gnuplot not found, using plotters backend
Benchmarking net_header/serialize: Collecting 100 samples in estimated 5.0000 s (3.9B itnet_header/serialize    time:   [1.5357 ns 1.5787 ns 1.6128 ns]
                        thrpt:  [620.03 Melem/s 633.45 Melem/s 651.15 Melem/s]
Benchmarking net_header/deserialize: Collecting 100 samples in estimated 5.0000 s (2.2B net_header/deserialize  time:   [2.2519 ns 2.2586 ns 2.2654 ns]
                        thrpt:  [441.43 Melem/s 442.76 Melem/s 444.08 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking net_header/roundtrip: Collecting 100 samples in estimated 5.0000 s (2.2B itnet_header/roundtrip    time:   [2.2460 ns 2.2541 ns 2.2628 ns]
                        thrpt:  [441.93 Melem/s 443.63 Melem/s 445.23 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking net_event_frame/write_single/64: Collecting 100 samples in estimated 5.0001net_event_frame/write_single/64
                        time:   [66.523 ns 67.003 ns 67.531 ns]
                        thrpt:  [903.81 MiB/s 910.93 MiB/s 917.51 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking net_event_frame/write_single/256: Collecting 100 samples in estimated 5.000net_event_frame/write_single/256
                        time:   [65.160 ns 65.586 ns 66.049 ns]
                        thrpt:  [3.6097 GiB/s 3.6352 GiB/s 3.6590 GiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking net_event_frame/write_single/1024: Collecting 100 samples in estimated 5.00net_event_frame/write_single/1024
                        time:   [77.107 ns 77.785 ns 78.480 ns]
                        thrpt:  [12.152 GiB/s 12.260 GiB/s 12.368 GiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking net_event_frame/write_single/4096: Collecting 100 samples in estimated 5.00net_event_frame/write_single/4096
                        time:   [123.32 ns 124.04 ns 124.79 ns]
                        thrpt:  [30.568 GiB/s 30.753 GiB/s 30.933 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking net_event_frame/write_batch/1: Collecting 100 samples in estimated 5.0001 snet_event_frame/write_batch/1
                        time:   [64.504 ns 65.063 ns 65.659 ns]
                        thrpt:  [929.58 MiB/s 938.10 MiB/s 946.23 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
Benchmarking net_event_frame/write_batch/10: Collecting 100 samples in estimated 5.0006 net_event_frame/write_batch/10
                        time:   [123.67 ns 124.88 ns 126.17 ns]
                        thrpt:  [4.7240 GiB/s 4.7729 GiB/s 4.8195 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking net_event_frame/write_batch/50: Collecting 100 samples in estimated 5.0010 net_event_frame/write_batch/50
                        time:   [396.06 ns 399.25 ns 402.80 ns]
                        thrpt:  [7.3987 GiB/s 7.4645 GiB/s 7.5248 GiB/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) high mild
  3 (3.00%) high severe
Benchmarking net_event_frame/write_batch/100: Collecting 100 samples in estimated 5.0033net_event_frame/write_batch/100
                        time:   [769.64 ns 781.10 ns 793.43 ns]
                        thrpt:  [7.5123 GiB/s 7.6309 GiB/s 7.7444 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking net_event_frame/read_batch_10: Collecting 100 samples in estimated 5.0011 snet_event_frame/read_batch_10
                        time:   [263.51 ns 264.41 ns 265.25 ns]
                        thrpt:  [37.700 Melem/s 37.821 Melem/s 37.949 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking net_packet_pool/get_return/16: Collecting 100 samples in estimated 5.0000 snet_packet_pool/get_return/16
                        time:   [86.379 ns 87.028 ns 87.733 ns]
                        thrpt:  [11.398 Melem/s 11.491 Melem/s 11.577 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking net_packet_pool/get_return/64: Collecting 100 samples in estimated 5.0000 snet_packet_pool/get_return/64
                        time:   [86.763 ns 87.376 ns 88.027 ns]
                        thrpt:  [11.360 Melem/s 11.445 Melem/s 11.526 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking net_packet_pool/get_return/256: Collecting 100 samples in estimated 5.0003 net_packet_pool/get_return/256
                        time:   [88.011 ns 88.688 ns 89.418 ns]
                        thrpt:  [11.183 Melem/s 11.276 Melem/s 11.362 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe

Benchmarking net_packet_build/build_packet/1: Collecting 100 samples in estimated 5.0016net_packet_build/build_packet/1
                        time:   [1.2429 µs 1.2474 µs 1.2519 µs]
                        thrpt:  [48.753 MiB/s 48.932 MiB/s 49.106 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking net_packet_build/build_packet/10: Collecting 100 samples in estimated 5.001net_packet_build/build_packet/10
                        time:   [2.0497 µs 2.0586 µs 2.0681 µs]
                        thrpt:  [295.13 MiB/s 296.48 MiB/s 297.78 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking net_packet_build/build_packet/50: Collecting 100 samples in estimated 5.026net_packet_build/build_packet/50
                        time:   [5.5992 µs 5.6180 µs 5.6381 µs]
                        thrpt:  [541.27 MiB/s 543.21 MiB/s 545.03 MiB/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe

Benchmarking net_encryption/encrypt/64: Collecting 100 samples in estimated 5.0005 s (4.net_encryption/encrypt/64
                        time:   [1.2429 µs 1.2473 µs 1.2517 µs]
                        thrpt:  [48.761 MiB/s 48.933 MiB/s 49.109 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking net_encryption/encrypt/256: Collecting 100 samples in estimated 5.0035 s (3net_encryption/encrypt/256
                        time:   [1.4040 µs 1.4087 µs 1.4136 µs]
                        thrpt:  [172.71 MiB/s 173.30 MiB/s 173.89 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking net_encryption/encrypt/1024: Collecting 100 samples in estimated 5.0080 s (net_encryption/encrypt/1024
                        time:   [2.3537 µs 2.3620 µs 2.3706 µs]
                        thrpt:  [411.94 MiB/s 413.44 MiB/s 414.91 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking net_encryption/encrypt/4096: Collecting 100 samples in estimated 5.0130 s (net_encryption/encrypt/4096
                        time:   [6.1377 µs 6.1598 µs 6.1823 µs]
                        thrpt:  [631.84 MiB/s 634.16 MiB/s 636.44 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking net_keypair/generate: Collecting 100 samples in estimated 5.0120 s (167k itnet_keypair/generate    time:   [30.041 µs 30.152 µs 30.261 µs]
                        thrpt:  [33.045 Kelem/s 33.166 Kelem/s 33.288 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking net_aad/generate: Collecting 100 samples in estimated 5.0000 s (3.9B iteratnet_aad/generate        time:   [1.4375 ns 1.4966 ns 1.5662 ns]
                        thrpt:  [638.49 Melem/s 668.17 Melem/s 695.66 Melem/s]

Benchmarking pool_comparison/shared_pool_get_return: Collecting 100 samples in estimatedpool_comparison/shared_pool_get_return
                        time:   [96.242 ns 97.430 ns 98.698 ns]
                        thrpt:  [10.132 Melem/s 10.264 Melem/s 10.390 Melem/s]
Benchmarking pool_comparison/thread_local_pool_get_return: Collecting 100 samples in estpool_comparison/thread_local_pool_get_return
                        time:   [123.36 ns 124.40 ns 125.47 ns]
                        thrpt:  [7.9703 Melem/s 8.0387 Melem/s 8.1066 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking pool_comparison/shared_pool_10x: Collecting 100 samples in estimated 5.0010pool_comparison/shared_pool_10x
                        time:   [1.1000 µs 1.1203 µs 1.1394 µs]
                        thrpt:  [877.65 Kelem/s 892.65 Kelem/s 909.06 Kelem/s]
Benchmarking pool_comparison/thread_local_pool_10x: Collecting 100 samples in estimated pool_comparison/thread_local_pool_10x
                        time:   [1.5053 µs 1.5132 µs 1.5216 µs]
                        thrpt:  [657.20 Kelem/s 660.84 Kelem/s 664.31 Kelem/s]

Benchmarking cipher_comparison/shared_pool/64: Collecting 100 samples in estimated 5.004cipher_comparison/shared_pool/64
                        time:   [1.2351 µs 1.2526 µs 1.2837 µs]
                        thrpt:  [47.546 MiB/s 48.726 MiB/s 49.417 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/64: Collecting 100 samples in estimated 5.0cipher_comparison/fast_chacha20/64
                        time:   [1.2628 µs 1.2684 µs 1.2743 µs]
                        thrpt:  [47.898 MiB/s 48.119 MiB/s 48.333 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking cipher_comparison/shared_pool/256: Collecting 100 samples in estimated 5.00cipher_comparison/shared_pool/256
                        time:   [1.4062 µs 1.4126 µs 1.4193 µs]
                        thrpt:  [172.01 MiB/s 172.83 MiB/s 173.62 MiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/256: Collecting 100 samples in estimated 5.cipher_comparison/fast_chacha20/256
                        time:   [1.4401 µs 1.4497 µs 1.4646 µs]
                        thrpt:  [166.70 MiB/s 168.41 MiB/s 169.53 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking cipher_comparison/shared_pool/1024: Collecting 100 samples in estimated 5.0cipher_comparison/shared_pool/1024
                        time:   [2.3544 µs 2.3639 µs 2.3731 µs]
                        thrpt:  [411.51 MiB/s 413.12 MiB/s 414.78 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/1024: Collecting 100 samples in estimated 5cipher_comparison/fast_chacha20/1024
                        time:   [2.3556 µs 2.3648 µs 2.3737 µs]
                        thrpt:  [411.41 MiB/s 412.96 MiB/s 414.56 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking cipher_comparison/shared_pool/4096: Collecting 100 samples in estimated 5.0cipher_comparison/shared_pool/4096
                        time:   [6.1644 µs 6.1843 µs 6.2053 µs]
                        thrpt:  [629.50 MiB/s 631.64 MiB/s 633.68 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking cipher_comparison/fast_chacha20/4096: Collecting 100 samples in estimated 5cipher_comparison/fast_chacha20/4096
                        time:   [6.0374 µs 6.0593 µs 6.0823 µs]
                        thrpt:  [642.23 MiB/s 644.67 MiB/s 647.01 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking adaptive_batcher/optimal_size: Collecting 100 samples in estimated 5.0000 sadaptive_batcher/optimal_size
                        time:   [1.7590 ns 1.7641 ns 1.7693 ns]
                        thrpt:  [565.21 Melem/s 566.85 Melem/s 568.50 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking adaptive_batcher/record: Collecting 100 samples in estimated 5.0000 s (314Madaptive_batcher/record time:   [15.841 ns 15.937 ns 16.033 ns]
                        thrpt:  [62.370 Melem/s 62.748 Melem/s 63.127 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low severe
  5 (5.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking adaptive_batcher/full_cycle: Collecting 100 samples in estimated 5.0000 s (adaptive_batcher/full_cycle
                        time:   [14.928 ns 14.982 ns 15.041 ns]
                        thrpt:  [66.487 Melem/s 66.745 Melem/s 66.990 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe

Benchmarking e2e_packet_build/shared_pool_50_events: Collecting 100 samples in estimatede2e_packet_build/shared_pool_50_events
                        time:   [5.6165 µs 5.6410 µs 5.6664 µs]
                        thrpt:  [538.57 MiB/s 541.00 MiB/s 543.36 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking e2e_packet_build/fast_50_events: Collecting 100 samples in estimated 5.0243e2e_packet_build/fast_50_events
                        time:   [5.5876 µs 5.6075 µs 5.6270 µs]
                        thrpt:  [542.34 MiB/s 544.23 MiB/s 546.17 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking multithread_packet_build/shared_pool/8: Collecting 100 samples in estimatedmultithread_packet_build/shared_pool/8
                        time:   [2.2177 ms 2.3031 ms 2.3951 ms]
                        thrpt:  [3.3402 Melem/s 3.4736 Melem/s 3.6074 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking multithread_packet_build/thread_local_pool/8: Collecting 100 samples in estmultithread_packet_build/thread_local_pool/8
                        time:   [2.3149 ms 2.3927 ms 2.4705 ms]
                        thrpt:  [3.2382 Melem/s 3.3435 Melem/s 3.4559 Melem/s]
Benchmarking multithread_packet_build/shared_pool/16: Collecting 100 samples in estimatemultithread_packet_build/shared_pool/16
                        time:   [4.2216 ms 4.3196 ms 4.4187 ms]
                        thrpt:  [3.6210 Melem/s 3.7041 Melem/s 3.7900 Melem/s]
Benchmarking multithread_packet_build/thread_local_pool/16: Collecting 100 samples in esmultithread_packet_build/thread_local_pool/16
                        time:   [3.8413 ms 3.9630 ms 4.0979 ms]
                        thrpt:  [3.9044 Melem/s 4.0374 Melem/s 4.1653 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking multithread_packet_build/shared_pool/24: Collecting 100 samples in estimatemultithread_packet_build/shared_pool/24
                        time:   [6.0360 ms 6.1582 ms 6.2882 ms]
                        thrpt:  [3.8167 Melem/s 3.8973 Melem/s 3.9762 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking multithread_packet_build/thread_local_pool/24: Collecting 100 samples in esmultithread_packet_build/thread_local_pool/24
                        time:   [5.4747 ms 5.5769 ms 5.6827 ms]
                        thrpt:  [4.2233 Melem/s 4.3034 Melem/s 4.3838 Melem/s]
Benchmarking multithread_packet_build/shared_pool/32: Collecting 100 samples in estimatemultithread_packet_build/shared_pool/32
                        time:   [7.7912 ms 7.9332 ms 8.0942 ms]
                        thrpt:  [3.9534 Melem/s 4.0337 Melem/s 4.1072 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking multithread_packet_build/thread_local_pool/32: Collecting 100 samples in esmultithread_packet_build/thread_local_pool/32
                        time:   [7.3597 ms 7.4653 ms 7.5773 ms]
                        thrpt:  [4.2231 Melem/s 4.2865 Melem/s 4.3480 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking multithread_mixed_frames/shared_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.2s, enable flat sampling, or reduce sample count to 50.
Benchmarking multithread_mixed_frames/shared_mixed/8: Collecting 100 samples in estimatemultithread_mixed_frames/shared_mixed/8
                        time:   [1.6921 ms 1.7647 ms 1.8371 ms]
                        thrpt:  [6.5320 Melem/s 6.8001 Melem/s 7.0919 Melem/s]
Benchmarking multithread_mixed_frames/fast_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 8.5s, enable flat sampling, or reduce sample count to 50.
Benchmarking multithread_mixed_frames/fast_mixed/8: Collecting 100 samples in estimated multithread_mixed_frames/fast_mixed/8
                        time:   [1.5972 ms 1.6671 ms 1.7399 ms]
                        thrpt:  [6.8969 Melem/s 7.1983 Melem/s 7.5132 Melem/s]
Benchmarking multithread_mixed_frames/shared_mixed/16: Collecting 100 samples in estimatmultithread_mixed_frames/shared_mixed/16
                        time:   [3.0488 ms 3.1165 ms 3.1852 ms]
                        thrpt:  [7.5349 Melem/s 7.7010 Melem/s 7.8718 Melem/s]
Benchmarking multithread_mixed_frames/fast_mixed/16: Collecting 100 samples in estimatedmultithread_mixed_frames/fast_mixed/16
                        time:   [2.8971 ms 2.9633 ms 3.0299 ms]
                        thrpt:  [7.9210 Melem/s 8.0991 Melem/s 8.2840 Melem/s]
Benchmarking multithread_mixed_frames/shared_mixed/24: Collecting 100 samples in estimatmultithread_mixed_frames/shared_mixed/24
                        time:   [4.3582 ms 4.4210 ms 4.4843 ms]
                        thrpt:  [8.0280 Melem/s 8.1430 Melem/s 8.2602 Melem/s]
Benchmarking multithread_mixed_frames/fast_mixed/24: Collecting 100 samples in estimatedmultithread_mixed_frames/fast_mixed/24
                        time:   [4.2049 ms 4.2766 ms 4.3489 ms]
                        thrpt:  [8.2780 Melem/s 8.4179 Melem/s 8.5613 Melem/s]
Benchmarking multithread_mixed_frames/shared_mixed/32: Collecting 100 samples in estimatmultithread_mixed_frames/shared_mixed/32
                        time:   [5.5614 ms 5.6748 ms 5.7954 ms]
                        thrpt:  [8.2825 Melem/s 8.4584 Melem/s 8.6309 Melem/s]
Found 15 outliers among 100 measurements (15.00%)
  14 (14.00%) high mild
  1 (1.00%) high severe
Benchmarking multithread_mixed_frames/fast_mixed/32: Collecting 100 samples in estimatedmultithread_mixed_frames/fast_mixed/32
                        time:   [5.3328 ms 5.4028 ms 5.4741 ms]
                        thrpt:  [8.7686 Melem/s 8.8843 Melem/s 9.0009 Melem/s]

Benchmarking pool_contention/shared_acquire_release/8: Collecting 100 samples in estimatpool_contention/shared_acquire_release/8
                        time:   [10.079 ms 10.138 ms 10.215 ms]
                        thrpt:  [7.8318 Melem/s 7.8909 Melem/s 7.9370 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking pool_contention/fast_acquire_release/8: Collecting 100 samples in estimatedpool_contention/fast_acquire_release/8
                        time:   [2.6764 ms 2.7630 ms 2.8529 ms]
                        thrpt:  [28.041 Melem/s 28.955 Melem/s 29.891 Melem/s]
Benchmarking pool_contention/shared_acquire_release/16: Collecting 100 samples in estimapool_contention/shared_acquire_release/16
                        time:   [19.278 ms 19.672 ms 20.289 ms]
                        thrpt:  [7.8860 Melem/s 8.1334 Melem/s 8.2996 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking pool_contention/fast_acquire_release/16: Collecting 100 samples in estimatepool_contention/fast_acquire_release/16
                        time:   [4.7245 ms 4.8063 ms 4.8897 ms]
                        thrpt:  [32.722 Melem/s 33.290 Melem/s 33.866 Melem/s]
Benchmarking pool_contention/shared_acquire_release/24: Collecting 100 samples in estimapool_contention/shared_acquire_release/24
                        time:   [30.303 ms 30.579 ms 30.992 ms]
                        thrpt:  [7.7440 Melem/s 7.8485 Melem/s 7.9200 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking pool_contention/fast_acquire_release/24: Collecting 100 samples in estimatepool_contention/fast_acquire_release/24
                        time:   [6.7681 ms 6.8548 ms 6.9447 ms]
                        thrpt:  [34.559 Melem/s 35.012 Melem/s 35.460 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking pool_contention/shared_acquire_release/32: Collecting 100 samples in estimapool_contention/shared_acquire_release/32
                        time:   [40.242 ms 40.346 ms 40.458 ms]
                        thrpt:  [7.9094 Melem/s 7.9314 Melem/s 7.9519 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking pool_contention/fast_acquire_release/32: Collecting 100 samples in estimatepool_contention/fast_acquire_release/32
                        time:   [8.7568 ms 8.8663 ms 8.9796 ms]
                        thrpt:  [35.636 Melem/s 36.092 Melem/s 36.543 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking throughput_scaling/fast_pool_scaling/1: Collecting 20 samples in estimated throughput_scaling/fast_pool_scaling/1
                        time:   [5.6917 ms 5.7223 ms 5.7540 ms]
                        thrpt:  [347.58 Kelem/s 349.51 Kelem/s 351.39 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) low mild
Benchmarking throughput_scaling/fast_pool_scaling/2: Collecting 20 samples in estimated throughput_scaling/fast_pool_scaling/2
                        time:   [5.8372 ms 5.8592 ms 5.8819 ms]
                        thrpt:  [680.06 Kelem/s 682.69 Kelem/s 685.26 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking throughput_scaling/fast_pool_scaling/4: Collecting 20 samples in estimated throughput_scaling/fast_pool_scaling/4
                        time:   [6.0156 ms 6.0415 ms 6.0650 ms]
                        thrpt:  [1.3191 Melem/s 1.3242 Melem/s 1.3299 Melem/s]
Benchmarking throughput_scaling/fast_pool_scaling/8: Collecting 20 samples in estimated throughput_scaling/fast_pool_scaling/8
                        time:   [7.6580 ms 8.2949 ms 8.9676 ms]
                        thrpt:  [1.7842 Melem/s 1.9289 Melem/s 2.0893 Melem/s]
Benchmarking throughput_scaling/fast_pool_scaling/16: Collecting 20 samples in estimatedthroughput_scaling/fast_pool_scaling/16
                        time:   [13.805 ms 14.511 ms 15.370 ms]
                        thrpt:  [2.0819 Melem/s 2.2052 Melem/s 2.3179 Melem/s]
Benchmarking throughput_scaling/fast_pool_scaling/24: Collecting 20 samples in estimatedthroughput_scaling/fast_pool_scaling/24
                        time:   [19.780 ms 20.709 ms 21.803 ms]
                        thrpt:  [2.2015 Melem/s 2.3178 Melem/s 2.4267 Melem/s]
Benchmarking throughput_scaling/fast_pool_scaling/32: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.8s, enable flat sampling, or reduce sample count to 10.
Benchmarking throughput_scaling/fast_pool_scaling/32: Collecting 20 samples in estimatedthroughput_scaling/fast_pool_scaling/32
                        time:   [26.508 ms 27.505 ms 28.839 ms]
                        thrpt:  [2.2192 Melem/s 2.3268 Melem/s 2.4144 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

Benchmarking routing_header/serialize: Collecting 100 samples in estimated 5.0000 s (8.7routing_header/serialize
                        time:   [655.18 ps 714.61 ps 779.92 ps]
                        thrpt:  [1.2822 Gelem/s 1.3994 Gelem/s 1.5263 Gelem/s]
Benchmarking routing_header/deserialize: Collecting 100 samples in estimated 5.0000 s (4routing_header/deserialize
                        time:   [1.0118 ns 1.0226 ns 1.0353 ns]
                        thrpt:  [965.87 Melem/s 977.89 Melem/s 988.36 Melem/s]
Found 18 outliers among 100 measurements (18.00%)
  14 (14.00%) high mild
  4 (4.00%) high severe
Benchmarking routing_header/roundtrip: Collecting 100 samples in estimated 5.0000 s (5.0routing_header/roundtrip
                        time:   [1.0195 ns 1.0348 ns 1.0517 ns]
                        thrpt:  [950.81 Melem/s 966.38 Melem/s 980.91 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking routing_header/forward: Collecting 100 samples in estimated 5.0000 s (17B irouting_header/forward  time:   [285.31 ps 286.82 ps 288.46 ps]
                        thrpt:  [3.4667 Gelem/s 3.4866 Gelem/s 3.5050 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking routing_table/lookup_hit: Collecting 100 samples in estimated 5.0004 s (59Mrouting_table/lookup_hit
                        time:   [83.475 ns 83.943 ns 84.387 ns]
                        thrpt:  [11.850 Melem/s 11.913 Melem/s 11.980 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking routing_table/lookup_miss: Collecting 100 samples in estimated 5.0000 s (15routing_table/lookup_miss
                        time:   [30.671 ns 31.109 ns 31.526 ns]
                        thrpt:  [31.720 Melem/s 32.145 Melem/s 32.604 Melem/s]
Benchmarking routing_table/is_local: Collecting 100 samples in estimated 5.0000 s (8.7B routing_table/is_local  time:   [569.44 ps 573.12 ps 577.13 ps]
                        thrpt:  [1.7327 Gelem/s 1.7448 Gelem/s 1.7561 Gelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking routing_table/add_route: Collecting 100 samples in estimated 5.0005 s (18M routing_table/add_route time:   [277.19 ns 279.46 ns 281.75 ns]
                        thrpt:  [3.5492 Melem/s 3.5783 Melem/s 3.6077 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high severe
Benchmarking routing_table/record_in: Collecting 100 samples in estimated 5.0001 s (50M routing_table/record_in time:   [100.52 ns 100.91 ns 101.29 ns]
                        thrpt:  [9.8729 Melem/s 9.9095 Melem/s 9.9480 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking routing_table/record_out: Collecting 100 samples in estimated 5.0003 s (79Mrouting_table/record_out
                        time:   [63.315 ns 63.568 ns 63.818 ns]
                        thrpt:  [15.670 Melem/s 15.731 Melem/s 15.794 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking routing_table/aggregate_stats: Collecting 100 samples in estimated 5.0000 srouting_table/aggregate_stats
                        time:   [14.245 µs 14.299 µs 14.357 µs]
                        thrpt:  [69.652 Kelem/s 69.934 Kelem/s 70.200 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking fair_scheduler/creation: Collecting 100 samples in estimated 5.0070 s (1.3Mfair_scheduler/creation time:   [3.7497 µs 3.7654 µs 3.7817 µs]
                        thrpt:  [264.43 Kelem/s 265.57 Kelem/s 266.69 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  1 (1.00%) low mild
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking fair_scheduler/stream_count_empty: Collecting 100 samples in estimated 5.00fair_scheduler/stream_count_empty
                        time:   [1.4622 µs 1.4675 µs 1.4732 µs]
                        thrpt:  [678.81 Kelem/s 681.43 Kelem/s 683.91 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking fair_scheduler/total_queued: Collecting 100 samples in estimated 5.0000 s (fair_scheduler/total_queued
                        time:   [286.91 ps 288.48 ps 290.16 ps]
                        thrpt:  [3.4464 Gelem/s 3.4665 Gelem/s 3.4854 Gelem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) high mild
  5 (5.00%) high severe
Benchmarking fair_scheduler/cleanup_empty: Collecting 100 samples in estimated 5.0020 s fair_scheduler/cleanup_empty
                        time:   [1.4902 µs 1.4959 µs 1.5017 µs]
                        thrpt:  [665.93 Kelem/s 668.47 Kelem/s 671.04 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild

Benchmarking routing_table_concurrent/concurrent_lookup/4: Collecting 100 samples in estrouting_table_concurrent/concurrent_lookup/4
                        time:   [320.65 µs 324.09 µs 329.87 µs]
                        thrpt:  [12.126 Melem/s 12.342 Melem/s 12.475 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking routing_table_concurrent/concurrent_stats/4: Collecting 100 samples in estirouting_table_concurrent/concurrent_stats/4
                        time:   [444.96 µs 448.35 µs 452.78 µs]
                        thrpt:  [8.8343 Melem/s 8.9216 Melem/s 8.9896 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  3 (3.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking routing_table_concurrent/concurrent_lookup/8: Collecting 100 samples in estrouting_table_concurrent/concurrent_lookup/8
                        time:   [484.07 µs 486.52 µs 489.07 µs]
                        thrpt:  [16.357 Melem/s 16.443 Melem/s 16.526 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking routing_table_concurrent/concurrent_stats/8: Collecting 100 samples in estirouting_table_concurrent/concurrent_stats/8
                        time:   [603.00 µs 607.00 µs 611.19 µs]
                        thrpt:  [13.089 Melem/s 13.179 Melem/s 13.267 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking routing_table_concurrent/concurrent_lookup/16: Collecting 100 samples in esrouting_table_concurrent/concurrent_lookup/16
                        time:   [849.32 µs 852.85 µs 856.64 µs]
                        thrpt:  [18.678 Melem/s 18.761 Melem/s 18.839 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking routing_table_concurrent/concurrent_stats/16: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.2s, enable flat sampling, or reduce sample count to 60.
Benchmarking routing_table_concurrent/concurrent_stats/16: Collecting 100 samples in estrouting_table_concurrent/concurrent_stats/16
                        time:   [977.44 µs 988.21 µs 1.0008 ms]
                        thrpt:  [15.987 Melem/s 16.191 Melem/s 16.369 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe

Benchmarking routing_decision/parse_lookup_forward: Collecting 100 samples in estimated routing_decision/parse_lookup_forward
                        time:   [81.022 ns 81.377 ns 81.739 ns]
                        thrpt:  [12.234 Melem/s 12.288 Melem/s 12.342 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking routing_decision/full_with_stats: Collecting 100 samples in estimated 5.000routing_decision/full_with_stats
                        time:   [244.10 ns 245.01 ns 245.94 ns]
                        thrpt:  [4.0661 Melem/s 4.0815 Melem/s 4.0967 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe

Benchmarking stream_multiplexing/lookup_all/10: Collecting 100 samples in estimated 5.00stream_multiplexing/lookup_all/10
                        time:   [698.62 ns 701.12 ns 703.66 ns]
                        thrpt:  [14.211 Melem/s 14.263 Melem/s 14.314 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking stream_multiplexing/stats_all/10: Collecting 100 samples in estimated 5.002stream_multiplexing/stats_all/10
                        time:   [1.0103 µs 1.0144 µs 1.0187 µs]
                        thrpt:  [9.8167 Melem/s 9.8584 Melem/s 9.8981 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking stream_multiplexing/lookup_all/100: Collecting 100 samples in estimated 5.0stream_multiplexing/lookup_all/100
                        time:   [6.9848 µs 7.0145 µs 7.0437 µs]
                        thrpt:  [14.197 Melem/s 14.256 Melem/s 14.317 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking stream_multiplexing/stats_all/100: Collecting 100 samples in estimated 5.02stream_multiplexing/stats_all/100
                        time:   [10.120 µs 10.167 µs 10.214 µs]
                        thrpt:  [9.7904 Melem/s 9.8358 Melem/s 9.8817 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking stream_multiplexing/lookup_all/1000: Collecting 100 samples in estimated 5.stream_multiplexing/lookup_all/1000
                        time:   [71.351 µs 71.668 µs 72.003 µs]
                        thrpt:  [13.888 Melem/s 13.953 Melem/s 14.015 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking stream_multiplexing/stats_all/1000: Collecting 100 samples in estimated 5.1stream_multiplexing/stats_all/1000
                        time:   [102.35 µs 102.73 µs 103.14 µs]
                        thrpt:  [9.6956 Melem/s 9.7341 Melem/s 9.7708 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking stream_multiplexing/lookup_all/10000: Collecting 100 samples in estimated 7stream_multiplexing/lookup_all/10000
                        time:   [736.27 µs 741.72 µs 748.54 µs]
                        thrpt:  [13.359 Melem/s 13.482 Melem/s 13.582 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking stream_multiplexing/stats_all/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.4s, enable flat sampling, or reduce sample count to 60.
Benchmarking stream_multiplexing/stats_all/10000: Collecting 100 samples in estimated 5.stream_multiplexing/stats_all/10000
                        time:   [1.0511 ms 1.0566 ms 1.0624 ms]
                        thrpt:  [9.4122 Melem/s 9.4644 Melem/s 9.5138 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking multihop_packet_builder/build/64: Collecting 100 samples in estimated 5.000multihop_packet_builder/build/64
                        time:   [76.685 ns 77.264 ns 77.869 ns]
                        thrpt:  [783.81 MiB/s 789.96 MiB/s 795.92 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_packet_builder/build_priority/64: Collecting 100 samples in estimamultihop_packet_builder/build_priority/64
                        time:   [68.533 ns 69.041 ns 69.604 ns]
                        thrpt:  [876.89 MiB/s 884.04 MiB/s 890.59 MiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_packet_builder/build/256: Collecting 100 samples in estimated 5.00multihop_packet_builder/build/256
                        time:   [80.878 ns 81.483 ns 82.117 ns]
                        thrpt:  [2.9034 GiB/s 2.9260 GiB/s 2.9479 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_packet_builder/build_priority/256: Collecting 100 samples in estimmultihop_packet_builder/build_priority/256
                        time:   [70.700 ns 71.242 ns 71.800 ns]
                        thrpt:  [3.3206 GiB/s 3.3466 GiB/s 3.3722 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking multihop_packet_builder/build/1024: Collecting 100 samples in estimated 5.0multihop_packet_builder/build/1024
                        time:   [87.213 ns 87.983 ns 88.771 ns]
                        thrpt:  [10.743 GiB/s 10.839 GiB/s 10.935 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking multihop_packet_builder/build_priority/1024: Collecting 100 samples in estimultihop_packet_builder/build_priority/1024
                        time:   [79.507 ns 80.183 ns 80.890 ns]
                        thrpt:  [11.790 GiB/s 11.894 GiB/s 11.995 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_packet_builder/build/4096: Collecting 100 samples in estimated 5.0multihop_packet_builder/build/4096
                        time:   [140.77 ns 141.63 ns 142.56 ns]
                        thrpt:  [26.759 GiB/s 26.935 GiB/s 27.098 GiB/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_packet_builder/build_priority/4096: Collecting 100 samples in estimultihop_packet_builder/build_priority/4096
                        time:   [142.03 ns 144.14 ns 147.21 ns]
                        thrpt:  [25.914 GiB/s 26.465 GiB/s 26.858 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking multihop_chain/forward_chain/1: Collecting 100 samples in estimated 5.0005 multihop_chain/forward_chain/1
                        time:   [98.870 ns 99.490 ns 100.15 ns]
                        thrpt:  [9.9846 Melem/s 10.051 Melem/s 10.114 Melem/s]
Benchmarking multihop_chain/forward_chain/2: Collecting 100 samples in estimated 5.0007 multihop_chain/forward_chain/2
                        time:   [178.00 ns 178.73 ns 179.45 ns]
                        thrpt:  [5.5725 Melem/s 5.5949 Melem/s 5.6180 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_chain/forward_chain/3: Collecting 100 samples in estimated 5.0008 multihop_chain/forward_chain/3
                        time:   [248.16 ns 249.89 ns 251.76 ns]
                        thrpt:  [3.9720 Melem/s 4.0018 Melem/s 4.0296 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_chain/forward_chain/4: Collecting 100 samples in estimated 5.0006 multihop_chain/forward_chain/4
                        time:   [324.51 ns 327.12 ns 329.93 ns]
                        thrpt:  [3.0309 Melem/s 3.0570 Melem/s 3.0816 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_chain/forward_chain/5: Collecting 100 samples in estimated 5.0008 multihop_chain/forward_chain/5
                        time:   [396.48 ns 398.89 ns 401.48 ns]
                        thrpt:  [2.4908 Melem/s 2.5069 Melem/s 2.5222 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe

Benchmarking hop_latency/single_hop_process: Collecting 100 samples in estimated 5.0000 hop_latency/single_hop_process
                        time:   [2.1711 ns 2.1792 ns 2.1875 ns]
                        thrpt:  [457.15 Melem/s 458.88 Melem/s 460.59 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking hop_latency/single_hop_full: Collecting 100 samples in estimated 5.0000 s (hop_latency/single_hop_full
                        time:   [77.548 ns 78.131 ns 78.722 ns]
                        thrpt:  [12.703 Melem/s 12.799 Melem/s 12.895 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking hop_scaling/64B_1hops: Collecting 100 samples in estimated 5.0002 s (52M ithop_scaling/64B_1hops   time:   [96.951 ns 98.191 ns 99.834 ns]
                        thrpt:  [611.37 MiB/s 621.60 MiB/s 629.55 MiB/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) high mild
  5 (5.00%) high severe
Benchmarking hop_scaling/64B_2hops: Collecting 100 samples in estimated 5.0002 s (29M ithop_scaling/64B_2hops   time:   [174.54 ns 175.64 ns 176.76 ns]
                        thrpt:  [345.31 MiB/s 347.51 MiB/s 349.69 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking hop_scaling/64B_3hops: Collecting 100 samples in estimated 5.0003 s (21M ithop_scaling/64B_3hops   time:   [243.07 ns 244.69 ns 246.41 ns]
                        thrpt:  [247.69 MiB/s 249.44 MiB/s 251.10 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking hop_scaling/64B_4hops: Collecting 100 samples in estimated 5.0003 s (16M ithop_scaling/64B_4hops   time:   [313.05 ns 315.22 ns 317.43 ns]
                        thrpt:  [192.28 MiB/s 193.63 MiB/s 194.97 MiB/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/64B_5hops: Collecting 100 samples in estimated 5.0004 s (13M ithop_scaling/64B_5hops   time:   [383.18 ns 385.38 ns 387.65 ns]
                        thrpt:  [157.45 MiB/s 158.38 MiB/s 159.29 MiB/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/256B_1hops: Collecting 100 samples in estimated 5.0003 s (50M ihop_scaling/256B_1hops  time:   [97.419 ns 97.894 ns 98.360 ns]
                        thrpt:  [2.4239 GiB/s 2.4355 GiB/s 2.4473 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/256B_2hops: Collecting 100 samples in estimated 5.0007 s (28M ihop_scaling/256B_2hops  time:   [177.56 ns 178.63 ns 179.70 ns]
                        thrpt:  [1.3267 GiB/s 1.3347 GiB/s 1.3427 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking hop_scaling/256B_3hops: Collecting 100 samples in estimated 5.0003 s (20M ihop_scaling/256B_3hops  time:   [247.11 ns 248.99 ns 250.96 ns]
                        thrpt:  [972.83 MiB/s 980.52 MiB/s 987.98 MiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/256B_4hops: Collecting 100 samples in estimated 5.0015 s (15M ihop_scaling/256B_4hops  time:   [323.53 ns 325.39 ns 327.31 ns]
                        thrpt:  [745.90 MiB/s 750.30 MiB/s 754.61 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking hop_scaling/256B_5hops: Collecting 100 samples in estimated 5.0020 s (13M ihop_scaling/256B_5hops  time:   [396.19 ns 399.32 ns 402.51 ns]
                        thrpt:  [606.55 MiB/s 611.39 MiB/s 616.22 MiB/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking hop_scaling/1024B_1hops: Collecting 100 samples in estimated 5.0001 s (48M hop_scaling/1024B_1hops time:   [103.86 ns 104.40 ns 104.95 ns]
                        thrpt:  [9.0872 GiB/s 9.1352 GiB/s 9.1822 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/1024B_2hops: Collecting 100 samples in estimated 5.0000 s (26M hop_scaling/1024B_2hops time:   [195.94 ns 196.73 ns 197.53 ns]
                        thrpt:  [4.8279 GiB/s 4.8476 GiB/s 4.8673 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking hop_scaling/1024B_3hops: Collecting 100 samples in estimated 5.0006 s (18M hop_scaling/1024B_3hops time:   [274.55 ns 276.25 ns 278.00 ns]
                        thrpt:  [3.4305 GiB/s 3.4522 GiB/s 3.4736 GiB/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/1024B_4hops: Collecting 100 samples in estimated 5.0003 s (14M hop_scaling/1024B_4hops time:   [361.53 ns 363.81 ns 366.06 ns]
                        thrpt:  [2.6052 GiB/s 2.6214 GiB/s 2.6379 GiB/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking hop_scaling/1024B_5hops: Collecting 100 samples in estimated 5.0019 s (11M hop_scaling/1024B_5hops time:   [445.94 ns 449.33 ns 452.78 ns]
                        thrpt:  [2.1063 GiB/s 2.1225 GiB/s 2.1386 GiB/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking multihop_with_routing/route_and_forward/1: Collecting 100 samples in estimamultihop_with_routing/route_and_forward/1
                        time:   [342.64 ns 344.00 ns 345.43 ns]
                        thrpt:  [2.8949 Melem/s 2.9070 Melem/s 2.9185 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking multihop_with_routing/route_and_forward/2: Collecting 100 samples in estimamultihop_with_routing/route_and_forward/2
                        time:   [664.91 ns 668.27 ns 671.74 ns]
                        thrpt:  [1.4887 Melem/s 1.4964 Melem/s 1.5040 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_with_routing/route_and_forward/3: Collecting 100 samples in estimamultihop_with_routing/route_and_forward/3
                        time:   [984.03 ns 987.11 ns 990.18 ns]
                        thrpt:  [1.0099 Melem/s 1.0131 Melem/s 1.0162 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking multihop_with_routing/route_and_forward/4: Collecting 100 samples in estimamultihop_with_routing/route_and_forward/4
                        time:   [1.3186 µs 1.3243 µs 1.3301 µs]
                        thrpt:  [751.84 Kelem/s 755.10 Kelem/s 758.39 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking multihop_with_routing/route_and_forward/5: Collecting 100 samples in estimamultihop_with_routing/route_and_forward/5
                        time:   [1.6400 µs 1.6514 µs 1.6676 µs]
                        thrpt:  [599.65 Kelem/s 605.54 Kelem/s 609.76 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking multihop_concurrent/concurrent_forward/4: Collecting 20 samples in estimatemultihop_concurrent/concurrent_forward/4
                        time:   [1.6743 ms 1.6857 ms 1.6965 ms]
                        thrpt:  [2.3578 Melem/s 2.3729 Melem/s 2.3890 Melem/s]
Benchmarking multihop_concurrent/concurrent_forward/8: Collecting 20 samples in estimatemultihop_concurrent/concurrent_forward/8
                        time:   [2.2098 ms 2.2352 ms 2.2640 ms]
                        thrpt:  [3.5335 Melem/s 3.5792 Melem/s 3.6202 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking multihop_concurrent/concurrent_forward/16: Collecting 20 samples in estimatmultihop_concurrent/concurrent_forward/16
                        time:   [2.6900 ms 2.8075 ms 2.9612 ms]
                        thrpt:  [5.4031 Melem/s 5.6991 Melem/s 5.9480 Melem/s]
Found 4 outliers among 20 measurements (20.00%)
  2 (10.00%) high mild
  2 (10.00%) high severe

Benchmarking pingwave/serialize: Collecting 100 samples in estimated 5.0000 s (7.0B iterpingwave/serialize      time:   [737.44 ps 750.57 ps 764.89 ps]
                        thrpt:  [1.3074 Gelem/s 1.3323 Gelem/s 1.3560 Gelem/s]
Benchmarking pingwave/deserialize: Collecting 100 samples in estimated 5.0000 s (5.8B itpingwave/deserialize    time:   [881.43 ps 900.46 ps 923.64 ps]
                        thrpt:  [1.0827 Gelem/s 1.1105 Gelem/s 1.1345 Gelem/s]
Found 18 outliers among 100 measurements (18.00%)
  1 (1.00%) high mild
  17 (17.00%) high severe
Benchmarking pingwave/roundtrip: Collecting 100 samples in estimated 5.0000 s (5.8B iterpingwave/roundtrip      time:   [880.56 ps 899.31 ps 922.05 ps]
                        thrpt:  [1.0845 Gelem/s 1.1120 Gelem/s 1.1356 Gelem/s]
Found 19 outliers among 100 measurements (19.00%)
  2 (2.00%) high mild
  17 (17.00%) high severe
Benchmarking pingwave/forward: Collecting 100 samples in estimated 5.0000 s (7.0B iteratpingwave/forward        time:   [730.99 ps 747.19 ps 764.86 ps]
                        thrpt:  [1.3074 Gelem/s 1.3383 Gelem/s 1.3680 Gelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking capabilities/serialize_simple: Collecting 100 samples in estimated 5.0002 scapabilities/serialize_simple
                        time:   [67.025 ns 67.379 ns 67.741 ns]
                        thrpt:  [14.762 Melem/s 14.841 Melem/s 14.920 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking capabilities/deserialize_simple: Collecting 100 samples in estimated 5.0001capabilities/deserialize_simple
                        time:   [15.585 ns 15.892 ns 16.176 ns]
                        thrpt:  [61.820 Melem/s 62.924 Melem/s 64.163 Melem/s]
Benchmarking capabilities/serialize_complex: Collecting 100 samples in estimated 5.0003 capabilities/serialize_complex
                        time:   [84.580 ns 85.474 ns 86.374 ns]
                        thrpt:  [11.578 Melem/s 11.699 Melem/s 11.823 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking capabilities/deserialize_complex: Collecting 100 samples in estimated 5.001capabilities/deserialize_complex
                        time:   [511.45 ns 514.56 ns 517.65 ns]
                        thrpt:  [1.9318 Melem/s 1.9434 Melem/s 1.9552 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking local_graph/create_pingwave: Collecting 100 samples in estimated 5.0000 s (local_graph/create_pingwave
                        time:   [6.6996 ns 6.7345 ns 6.7711 ns]
                        thrpt:  [147.69 Melem/s 148.49 Melem/s 149.26 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking local_graph/on_pingwave_new: Collecting 100 samples in estimated 5.0062 s (local_graph/on_pingwave_new
                        time:   [95.297 ns 99.688 ns 104.01 ns]
                        thrpt:  [9.6143 Melem/s 10.031 Melem/s 10.494 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking local_graph/on_pingwave_duplicate: Collecting 100 samples in estimated 5.00local_graph/on_pingwave_duplicate
                        time:   [1.5036 µs 1.5098 µs 1.5165 µs]
                        thrpt:  [659.41 Kelem/s 662.35 Kelem/s 665.05 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking local_graph/get_node: Collecting 100 samples in estimated 5.0001 s (183M itlocal_graph/get_node    time:   [27.232 ns 27.465 ns 27.800 ns]
                        thrpt:  [35.971 Melem/s 36.410 Melem/s 36.722 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) high mild
  4 (4.00%) high severe
Benchmarking local_graph/node_count: Collecting 100 samples in estimated 5.0035 s (3.4M local_graph/node_count  time:   [1.4655 µs 1.4705 µs 1.4756 µs]
                        thrpt:  [677.69 Kelem/s 680.06 Kelem/s 682.36 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking local_graph/stats: Collecting 100 samples in estimated 5.0201 s (1.1M iteralocal_graph/stats       time:   [4.4573 µs 4.4739 µs 4.4910 µs]
                        thrpt:  [222.67 Kelem/s 223.52 Kelem/s 224.35 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking graph_scaling/all_nodes/100: Collecting 100 samples in estimated 5.0252 s (graph_scaling/all_nodes/100
                        time:   [13.726 µs 13.792 µs 13.857 µs]
                        thrpt:  [7.2164 Melem/s 7.2508 Melem/s 7.2852 Melem/s]
Benchmarking graph_scaling/nodes_within_hops/100: Collecting 100 samples in estimated 5.graph_scaling/nodes_within_hops/100
                        time:   [13.758 µs 13.815 µs 13.870 µs]
                        thrpt:  [7.2096 Melem/s 7.2384 Melem/s 7.2687 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking graph_scaling/all_nodes/500: Collecting 100 samples in estimated 5.0970 s (graph_scaling/all_nodes/500
                        time:   [26.991 µs 27.159 µs 27.318 µs]
                        thrpt:  [18.303 Melem/s 18.410 Melem/s 18.524 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking graph_scaling/nodes_within_hops/500: Collecting 100 samples in estimated 5.graph_scaling/nodes_within_hops/500
                        time:   [27.109 µs 27.268 µs 27.424 µs]
                        thrpt:  [18.232 Melem/s 18.337 Melem/s 18.444 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking graph_scaling/all_nodes/1000: Collecting 100 samples in estimated 5.0933 s graph_scaling/all_nodes/1000
                        time:   [43.220 µs 43.468 µs 43.710 µs]
                        thrpt:  [22.878 Melem/s 23.006 Melem/s 23.137 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking graph_scaling/nodes_within_hops/1000: Collecting 100 samples in estimated 5graph_scaling/nodes_within_hops/1000
                        time:   [43.269 µs 43.568 µs 43.861 µs]
                        thrpt:  [22.799 Melem/s 22.952 Melem/s 23.111 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking graph_scaling/all_nodes/5000: Collecting 100 samples in estimated 6.4923 s graph_scaling/all_nodes/5000
                        time:   [319.04 µs 320.62 µs 322.21 µs]
                        thrpt:  [15.518 Melem/s 15.595 Melem/s 15.672 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking graph_scaling/nodes_within_hops/5000: Collecting 100 samples in estimated 6graph_scaling/nodes_within_hops/5000
                        time:   [321.23 µs 323.20 µs 325.23 µs]
                        thrpt:  [15.374 Melem/s 15.470 Melem/s 15.565 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking capability_search/find_with_gpu: Collecting 100 samples in estimated 5.0240capability_search/find_with_gpu
                        time:   [52.354 µs 52.669 µs 53.005 µs]
                        thrpt:  [18.866 Kelem/s 18.986 Kelem/s 19.101 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking capability_search/find_by_tool_python: Collecting 100 samples in estimated capability_search/find_by_tool_python
                        time:   [106.64 µs 107.19 µs 107.75 µs]
                        thrpt:  [9.2805 Kelem/s 9.3293 Kelem/s 9.3773 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
Benchmarking capability_search/find_by_tool_rust: Collecting 100 samples in estimated 5.capability_search/find_by_tool_rust
                        time:   [133.99 µs 134.70 µs 135.42 µs]
                        thrpt:  [7.3843 Kelem/s 7.4239 Kelem/s 7.4635 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking graph_concurrent/concurrent_pingwave/4: Collecting 20 samples in estimated graph_concurrent/concurrent_pingwave/4
                        time:   [316.87 µs 319.93 µs 323.54 µs]
                        thrpt:  [6.1817 Melem/s 6.2514 Melem/s 6.3118 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking graph_concurrent/concurrent_pingwave/8: Collecting 20 samples in estimated graph_concurrent/concurrent_pingwave/8
                        time:   [488.07 µs 491.67 µs 495.85 µs]
                        thrpt:  [8.0669 Melem/s 8.1355 Melem/s 8.1955 Melem/s]
Benchmarking graph_concurrent/concurrent_pingwave/16: Collecting 20 samples in estimatedgraph_concurrent/concurrent_pingwave/16
                        time:   [859.04 µs 863.45 µs 868.52 µs]
                        thrpt:  [9.2111 Melem/s 9.2652 Melem/s 9.3127 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild

Benchmarking path_finding/path_1_hop: Collecting 100 samples in estimated 5.0343 s (449kpath_finding/path_1_hop time:   [11.148 µs 11.195 µs 11.245 µs]
                        thrpt:  [88.929 Kelem/s 89.323 Kelem/s 89.699 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking path_finding/path_2_hops: Collecting 100 samples in estimated 5.0337 s (444path_finding/path_2_hops
                        time:   [11.275 µs 11.322 µs 11.373 µs]
                        thrpt:  [87.925 Kelem/s 88.327 Kelem/s 88.694 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking path_finding/path_4_hops: Collecting 100 samples in estimated 5.0523 s (429path_finding/path_4_hops
                        time:   [11.711 µs 11.756 µs 11.802 µs]
                        thrpt:  [84.731 Kelem/s 85.065 Kelem/s 85.387 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking path_finding/path_not_found: Collecting 100 samples in estimated 5.0354 s (path_finding/path_not_found
                        time:   [11.359 µs 11.404 µs 11.452 µs]
                        thrpt:  [87.319 Kelem/s 87.689 Kelem/s 88.038 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking path_finding/path_complex_graph: Collecting 100 samples in estimated 5.2228path_finding/path_complex_graph
                        time:   [344.94 µs 346.27 µs 347.59 µs]
                        thrpt:  [2.8770 Kelem/s 2.8879 Kelem/s 2.8990 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking failure_detector/heartbeat_existing: Collecting 100 samples in estimated 5.failure_detector/heartbeat_existing
                        time:   [71.294 ns 71.597 ns 71.902 ns]
                        thrpt:  [13.908 Melem/s 13.967 Melem/s 14.026 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking failure_detector/heartbeat_new: Collecting 100 samples in estimated 5.0002 failure_detector/heartbeat_new
                        time:   [264.63 ns 266.98 ns 269.46 ns]
                        thrpt:  [3.7112 Melem/s 3.7456 Melem/s 3.7788 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low severe
  1 (1.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking failure_detector/status_check: Collecting 100 samples in estimated 5.0000 sfailure_detector/status_check
                        time:   [26.176 ns 26.276 ns 26.375 ns]
                        thrpt:  [37.914 Melem/s 38.057 Melem/s 38.202 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking failure_detector/check_all: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 62.4s, or reduce sample count to 10.
Benchmarking failure_detector/check_all: Collecting 100 samples in estimated 62.354 s (1failure_detector/check_all
                        time:   [614.74 ms 616.52 ms 618.29 ms]
                        thrpt:  [1.6174  elem/s 1.6220  elem/s 1.6267  elem/s]
Benchmarking failure_detector/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 12.2s, or reduce sample count to 40.
Benchmarking failure_detector/stats: Collecting 100 samples in estimated 12.170 s (100 ifailure_detector/stats  time:   [122.01 ms 122.51 ms 123.02 ms]
                        thrpt:  [8.1289  elem/s 8.1628  elem/s 8.1958  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking loss_simulator/should_drop_1pct: Collecting 100 samples in estimated 5.0001loss_simulator/should_drop_1pct
                        time:   [17.024 ns 17.075 ns 17.127 ns]
                        thrpt:  [58.386 Melem/s 58.567 Melem/s 58.742 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking loss_simulator/should_drop_5pct: Collecting 100 samples in estimated 5.0000loss_simulator/should_drop_5pct
                        time:   [17.591 ns 17.676 ns 17.765 ns]
                        thrpt:  [56.290 Melem/s 56.573 Melem/s 56.848 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking loss_simulator/should_drop_10pct: Collecting 100 samples in estimated 5.000loss_simulator/should_drop_10pct
                        time:   [18.116 ns 18.180 ns 18.244 ns]
                        thrpt:  [54.811 Melem/s 55.006 Melem/s 55.200 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) high mild
  3 (3.00%) high severe
Benchmarking loss_simulator/should_drop_20pct: Collecting 100 samples in estimated 5.000loss_simulator/should_drop_20pct
                        time:   [19.283 ns 19.503 ns 19.869 ns]
                        thrpt:  [50.329 Melem/s 51.273 Melem/s 51.859 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) high mild
  3 (3.00%) high severe
Benchmarking loss_simulator/should_drop_burst: Collecting 100 samples in estimated 5.000loss_simulator/should_drop_burst
                        time:   [17.532 ns 17.604 ns 17.677 ns]
                        thrpt:  [56.570 Melem/s 56.807 Melem/s 57.039 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe

Benchmarking circuit_breaker/allow_closed: Collecting 100 samples in estimated 5.0000 s circuit_breaker/allow_closed
                        time:   [12.433 ns 12.486 ns 12.539 ns]
                        thrpt:  [79.754 Melem/s 80.090 Melem/s 80.434 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking circuit_breaker/record_success: Collecting 100 samples in estimated 5.0000 circuit_breaker/record_success
                        time:   [11.238 ns 11.279 ns 11.319 ns]
                        thrpt:  [88.344 Melem/s 88.662 Melem/s 88.983 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking circuit_breaker/record_failure: Collecting 100 samples in estimated 5.0000 circuit_breaker/record_failure
                        time:   [11.376 ns 11.422 ns 11.471 ns]
                        thrpt:  [87.175 Melem/s 87.550 Melem/s 87.907 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking circuit_breaker/state: Collecting 100 samples in estimated 5.0000 s (403M icircuit_breaker/state   time:   [12.413 ns 12.468 ns 12.523 ns]
                        thrpt:  [79.852 Melem/s 80.207 Melem/s 80.558 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking recovery_manager/on_failure_with_alternates: Collecting 100 samples in estirecovery_manager/on_failure_with_alternates
                        time:   [382.42 ns 385.03 ns 387.78 ns]
                        thrpt:  [2.5788 Melem/s 2.5972 Melem/s 2.6149 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) low severe
  3 (3.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking recovery_manager/on_failure_no_alternates: Collecting 100 samples in estimarecovery_manager/on_failure_no_alternates
                        time:   [257.99 ns 274.31 ns 305.60 ns]
                        thrpt:  [3.2723 Melem/s 3.6455 Melem/s 3.8761 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) low severe
  5 (5.00%) low mild
  2 (2.00%) high severe
Benchmarking recovery_manager/get_action: Collecting 100 samples in estimated 5.0004 s (recovery_manager/get_action
                        time:   [83.053 ns 83.531 ns 84.036 ns]
                        thrpt:  [11.900 Melem/s 11.972 Melem/s 12.040 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking recovery_manager/is_failed: Collecting 100 samples in estimated 5.0001 s (1recovery_manager/is_failed
                        time:   [25.554 ns 25.711 ns 25.937 ns]
                        thrpt:  [38.555 Melem/s 38.894 Melem/s 39.133 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high severe
Benchmarking recovery_manager/on_recovery: Collecting 100 samples in estimated 5.0000 s recovery_manager/on_recovery
                        time:   [228.17 ns 248.98 ns 294.64 ns]
                        thrpt:  [3.3939 Melem/s 4.0163 Melem/s 4.3827 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking recovery_manager/stats: Collecting 100 samples in estimated 5.0000 s (2.8B recovery_manager/stats  time:   [1.7596 ns 1.7656 ns 1.7716 ns]
                        thrpt:  [564.46 Melem/s 566.38 Melem/s 568.30 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe

Benchmarking failure_scaling/check_all/100: Collecting 100 samples in estimated 5.0135 sfailure_scaling/check_all/100
                        time:   [16.996 µs 17.109 µs 17.242 µs]
                        thrpt:  [5.8000 Melem/s 5.8450 Melem/s 5.8839 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking failure_scaling/healthy_nodes/100: Collecting 100 samples in estimated 5.01failure_scaling/healthy_nodes/100
                        time:   [11.877 µs 11.932 µs 11.988 µs]
                        thrpt:  [8.3419 Melem/s 8.3808 Melem/s 8.4197 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking failure_scaling/check_all/500: Collecting 100 samples in estimated 5.0033 sfailure_scaling/check_all/500
                        time:   [43.692 µs 43.900 µs 44.114 µs]
                        thrpt:  [11.334 Melem/s 11.389 Melem/s 11.444 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking failure_scaling/healthy_nodes/500: Collecting 100 samples in estimated 5.05failure_scaling/healthy_nodes/500
                        time:   [15.860 µs 15.923 µs 15.987 µs]
                        thrpt:  [31.275 Melem/s 31.401 Melem/s 31.526 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking failure_scaling/check_all/1000: Collecting 100 samples in estimated 5.1177 failure_scaling/check_all/1000
                        time:   [78.004 µs 78.306 µs 78.600 µs]
                        thrpt:  [12.723 Melem/s 12.770 Melem/s 12.820 Melem/s]
Found 17 outliers among 100 measurements (17.00%)
  15 (15.00%) low mild
  2 (2.00%) high mild
Benchmarking failure_scaling/healthy_nodes/1000: Collecting 100 samples in estimated 5.0failure_scaling/healthy_nodes/1000
                        time:   [21.387 µs 21.470 µs 21.556 µs]
                        thrpt:  [46.390 Melem/s 46.576 Melem/s 46.757 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking failure_scaling/check_all/5000: Collecting 100 samples in estimated 6.4813 failure_scaling/check_all/5000
                        time:   [347.65 µs 349.10 µs 350.59 µs]
                        thrpt:  [14.262 Melem/s 14.323 Melem/s 14.382 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking failure_scaling/healthy_nodes/5000: Collecting 100 samples in estimated 5.0failure_scaling/healthy_nodes/5000
                        time:   [66.264 µs 66.491 µs 66.723 µs]
                        thrpt:  [74.937 Melem/s 75.199 Melem/s 75.456 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking failure_concurrent/concurrent_heartbeat/4: Collecting 20 samples in estimatfailure_concurrent/concurrent_heartbeat/4
                        time:   [356.88 µs 360.68 µs 363.84 µs]
                        thrpt:  [5.4970 Melem/s 5.5450 Melem/s 5.6041 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking failure_concurrent/concurrent_heartbeat/8: Collecting 20 samples in estimatfailure_concurrent/concurrent_heartbeat/8
                        time:   [523.17 µs 531.68 µs 544.43 µs]
                        thrpt:  [7.3471 Melem/s 7.5234 Melem/s 7.6457 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low mild
  1 (5.00%) high severe
Benchmarking failure_concurrent/concurrent_heartbeat/16: Collecting 20 samples in estimafailure_concurrent/concurrent_heartbeat/16
                        time:   [880.63 µs 889.21 µs 903.58 µs]
                        thrpt:  [8.8537 Melem/s 8.9968 Melem/s 9.0845 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high severe

Benchmarking failure_recovery_cycle/full_cycle: Collecting 100 samples in estimated 5.00failure_recovery_cycle/full_cycle
                        time:   [412.93 ns 416.02 ns 419.19 ns]
                        thrpt:  [2.3856 Melem/s 2.4038 Melem/s 2.4217 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) low severe
  2 (2.00%) low mild

Benchmarking capability_set/create: Collecting 100 samples in estimated 5.2902 s (35k itcapability_set/create   time:   [149.05 µs 149.83 µs 150.58 µs]
                        thrpt:  [6.6408 Kelem/s 6.6741 Kelem/s 6.7092 Kelem/s]
Benchmarking capability_set/serialize: Collecting 100 samples in estimated 7.4674 s (15kcapability_set/serialize
                        time:   [492.65 µs 495.93 µs 500.18 µs]
                        thrpt:  [1.9993 Kelem/s 2.0164 Kelem/s 2.0298 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking capability_set/deserialize: Collecting 100 samples in estimated 5.0230 s (3capability_set/deserialize
                        time:   [15.546 µs 15.661 µs 15.773 µs]
                        thrpt:  [63.400 Kelem/s 63.855 Kelem/s 64.325 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking capability_set/roundtrip: Collecting 100 samples in estimated 5.5876 s (40kcapability_set/roundtrip
                        time:   [137.22 µs 138.03 µs 138.82 µs]
                        thrpt:  [7.2035 Kelem/s 7.2446 Kelem/s 7.2877 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_set/has_tag: Collecting 100 samples in estimated 5.0005 s (41M icapability_set/has_tag  time:   [122.43 ns 123.23 ns 124.08 ns]
                        thrpt:  [8.0592 Melem/s 8.1149 Melem/s 8.1677 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_set/has_model: Collecting 100 samples in estimated 5.0025 s (2.4capability_set/has_model
                        time:   [2.0182 µs 2.0311 µs 2.0443 µs]
                        thrpt:  [489.17 Kelem/s 492.34 Kelem/s 495.48 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking capability_set/has_tool: Collecting 100 samples in estimated 5.0020 s (12M capability_set/has_tool time:   [403.39 ns 405.75 ns 408.20 ns]
                        thrpt:  [2.4498 Melem/s 2.4646 Melem/s 2.4790 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking capability_set/has_gpu: Collecting 100 samples in estimated 5.0003 s (54M icapability_set/has_gpu  time:   [91.220 ns 91.844 ns 92.477 ns]
                        thrpt:  [10.814 Melem/s 10.888 Melem/s 10.962 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild

Benchmarking capability_announcement/create: Collecting 100 samples in estimated 5.0013 capability_announcement/create
                        time:   [5.1403 µs 5.1650 µs 5.1896 µs]
                        thrpt:  [192.69 Kelem/s 193.61 Kelem/s 194.54 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking capability_announcement/serialize: Collecting 100 samples in estimated 5.01capability_announcement/serialize
                        time:   [109.93 µs 110.44 µs 110.94 µs]
                        thrpt:  [9.0135 Kelem/s 9.0550 Kelem/s 9.0965 Kelem/s]
Benchmarking capability_announcement/deserialize: Collecting 100 samples in estimated 5.capability_announcement/deserialize
                        time:   [17.346 µs 17.439 µs 17.535 µs]
                        thrpt:  [57.029 Kelem/s 57.342 Kelem/s 57.650 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_announcement/is_expired: Collecting 100 samples in estimated 5.0capability_announcement/is_expired
                        time:   [34.313 ns 34.438 ns 34.565 ns]
                        thrpt:  [28.931 Melem/s 29.038 Melem/s 29.144 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking capability_filter/match_single_tag: Collecting 100 samples in estimated 5.0capability_filter/match_single_tag
                        time:   [104.15 ns 104.88 ns 105.69 ns]
                        thrpt:  [9.4620 Melem/s 9.5346 Melem/s 9.6016 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking capability_filter/match_require_gpu: Collecting 100 samples in estimated 5.capability_filter/match_require_gpu
                        time:   [98.292 ns 98.948 ns 99.602 ns]
                        thrpt:  [10.040 Melem/s 10.106 Melem/s 10.174 Melem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_filter/match_gpu_vendor: Collecting 100 samples in estimated 5.3capability_filter/match_gpu_vendor
                        time:   [149.87 µs 150.51 µs 151.18 µs]
                        thrpt:  [6.6148 Kelem/s 6.6440 Kelem/s 6.6726 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_filter/match_min_memory: Collecting 100 samples in estimated 5.4capability_filter/match_min_memory
                        time:   [133.96 µs 134.67 µs 135.45 µs]
                        thrpt:  [7.3830 Kelem/s 7.4253 Kelem/s 7.4647 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_filter/match_complex: Collecting 100 samples in estimated 5.4269capability_filter/match_complex
                        time:   [153.27 µs 153.99 µs 154.74 µs]
                        thrpt:  [6.4625 Kelem/s 6.4941 Kelem/s 6.5246 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking capability_filter/match_no_match: Collecting 100 samples in estimated 5.000capability_filter/match_no_match
                        time:   [131.82 ns 132.72 ns 133.68 ns]
                        thrpt:  [7.4806 Melem/s 7.5345 Melem/s 7.5860 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe

Benchmarking capability_fold_insert/index_nodes/100: Collecting 100 samples in estimatedcapability_fold_insert/index_nodes/100
                        time:   [27.142 ms 27.280 ms 27.423 ms]
                        thrpt:  [3.6466 Kelem/s 3.6656 Kelem/s 3.6843 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking capability_fold_insert/index_nodes/1000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 26.8s, or reduce sample count to 10.
Benchmarking capability_fold_insert/index_nodes/1000: Collecting 100 samples in estimatecapability_fold_insert/index_nodes/1000
                        time:   [265.65 ms 266.58 ms 267.58 ms]
                        thrpt:  [3.7372 Kelem/s 3.7512 Kelem/s 3.7644 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking capability_fold_insert/index_nodes/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 267.1s, or reduce sample count to 10.
Benchmarking capability_fold_insert/index_nodes/10000: Collecting 100 samples in estimatcapability_fold_insert/index_nodes/10000
                        time:   [2.7951 s 2.8621 s 2.9345 s]
                        thrpt:  [3.4078 Kelem/s 3.4939 Kelem/s 3.5776 Kelem/s]
Found 20 outliers among 100 measurements (20.00%)
  20 (20.00%) high severe

Benchmarking capability_fold_query/query_single_tag: Collecting 100 samples in estimatedcapability_fold_query/query_single_tag
                        time:   [32.941 ms 33.402 ms 33.870 ms]
                        thrpt:  [29.525  elem/s 29.939  elem/s 30.358  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_query/query_require_gpu: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.7s, or reduce sample count to 70.
Benchmarking capability_fold_query/query_require_gpu: Collecting 100 samples in estimatecapability_fold_query/query_require_gpu
                        time:   [63.970 ms 64.860 ms 65.754 ms]
                        thrpt:  [15.208  elem/s 15.418  elem/s 15.632  elem/s]
Benchmarking capability_fold_query/query_gpu_vendor: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.6s, or reduce sample count to 70.
Benchmarking capability_fold_query/query_gpu_vendor: Collecting 100 samples in estimatedcapability_fold_query/query_gpu_vendor
                        time:   [66.068 ms 67.094 ms 68.138 ms]
                        thrpt:  [14.676  elem/s 14.904  elem/s 15.136  elem/s]
Benchmarking capability_fold_query/query_min_memory: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.7s, or reduce sample count to 70.
Benchmarking capability_fold_query/query_min_memory: Collecting 100 samples in estimatedcapability_fold_query/query_min_memory
                        time:   [65.837 ms 66.762 ms 67.699 ms]
                        thrpt:  [14.771  elem/s 14.979  elem/s 15.189  elem/s]
Benchmarking capability_fold_query/query_complex: Collecting 100 samples in estimated 6.capability_fold_query/query_complex
                        time:   [34.343 ms 34.967 ms 35.606 ms]
                        thrpt:  [28.085  elem/s 28.598  elem/s 29.118  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_query/query_model: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 23.0s, or reduce sample count to 20.
Benchmarking capability_fold_query/query_model: Collecting 100 samples in estimated 22.9capability_fold_query/query_model
                        time:   [249.11 ms 252.23 ms 255.38 ms]
                        thrpt:  [3.9157  elem/s 3.9646  elem/s 4.0143  elem/s]
Benchmarking capability_fold_query/query_tool: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 24.4s, or reduce sample count to 20.
Benchmarking capability_fold_query/query_tool: Collecting 100 samples in estimated 24.37capability_fold_query/query_tool
                        time:   [230.01 ms 233.62 ms 237.24 ms]
                        thrpt:  [4.2152  elem/s 4.2804  elem/s 4.3476  elem/s]
Benchmarking capability_fold_query/query_no_results: Collecting 100 samples in estimatedcapability_fold_query/query_no_results
                        time:   [394.62 ns 402.62 ns 410.40 ns]
                        thrpt:  [2.4367 Melem/s 2.4837 Melem/s 2.5341 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking capability_fold_find_best/find_best_simple: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 7.2s, or reduce sample count to 60.
Benchmarking capability_fold_find_best/find_best_simple: Collecting 100 samples in estimcapability_fold_find_best/find_best_simple
                        time:   [70.314 ms 71.390 ms 72.480 ms]
                        thrpt:  [13.797  elem/s 14.008  elem/s 14.222  elem/s]
Benchmarking capability_fold_find_best/find_best_with_prefs: Collecting 100 samples in ecapability_fold_find_best/find_best_with_prefs
                        time:   [35.268 ms 35.836 ms 36.421 ms]
                        thrpt:  [27.456  elem/s 27.905  elem/s 28.355  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

Benchmarking capability_fold_scaling/query_tag/1000: Collecting 100 samples in estimatedcapability_fold_scaling/query_tag/1000
                        time:   [1.9929 ms 2.0351 ms 2.0782 ms]
                        thrpt:  [481.18  elem/s 491.37  elem/s 501.77  elem/s]
Benchmarking capability_fold_scaling/query_complex/1000: Collecting 100 samples in estimcapability_fold_scaling/query_complex/1000
                        time:   [2.1434 ms 2.1921 ms 2.2413 ms]
                        thrpt:  [446.16  elem/s 456.18  elem/s 466.55  elem/s]
Benchmarking capability_fold_scaling/query_tag/5000: Collecting 100 samples in estimatedcapability_fold_scaling/query_tag/5000
                        time:   [16.083 ms 16.357 ms 16.628 ms]
                        thrpt:  [60.138  elem/s 61.137  elem/s 62.176  elem/s]
Benchmarking capability_fold_scaling/query_complex/5000: Collecting 100 samples in estimcapability_fold_scaling/query_complex/5000
                        time:   [15.983 ms 16.246 ms 16.511 ms]
                        thrpt:  [60.567  elem/s 61.555  elem/s 62.567  elem/s]
Benchmarking capability_fold_scaling/query_tag/10000: Collecting 100 samples in estimatecapability_fold_scaling/query_tag/10000
                        time:   [33.371 ms 33.827 ms 34.287 ms]
                        thrpt:  [29.166  elem/s 29.563  elem/s 29.966  elem/s]
Benchmarking capability_fold_scaling/query_complex/10000: Collecting 100 samples in esticapability_fold_scaling/query_complex/10000
                        time:   [33.839 ms 34.272 ms 34.706 ms]
                        thrpt:  [28.813  elem/s 29.179  elem/s 29.552  elem/s]
Benchmarking capability_fold_scaling/query_tag/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 18.9s, or reduce sample count to 20.
Benchmarking capability_fold_scaling/query_tag/50000: Collecting 100 samples in estimatecapability_fold_scaling/query_tag/50000
                        time:   [178.25 ms 180.60 ms 183.06 ms]
                        thrpt:  [5.4627  elem/s 5.5370  elem/s 5.6101  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking capability_fold_scaling/query_complex/50000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 17.3s, or reduce sample count to 20.
Benchmarking capability_fold_scaling/query_complex/50000: Collecting 100 samples in esticapability_fold_scaling/query_complex/50000
                        time:   [180.51 ms 182.79 ms 185.08 ms]
                        thrpt:  [5.4032  elem/s 5.4708  elem/s 5.5398  elem/s]

Benchmarking capability_fold_concurrent/concurrent_index/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 5.3s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_index/4: Collecting 20 samples in estcapability_fold_concurrent/concurrent_index/4
                        time:   [274.91 ms 285.17 ms 296.00 ms]
                        thrpt:  [6.7567 Kelem/s 7.0135 Kelem/s 7.2751 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 947.4s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_query/4: Collecting 20 samples in estcapability_fold_concurrent/concurrent_query/4
                        time:   [44.658 s 45.447 s 46.139 s]
                        thrpt:  [43.348  elem/s 44.007  elem/s 44.785  elem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) low severe
  1 (5.00%) low mild
Benchmarking capability_fold_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 307.4s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_mixed/4: Collecting 20 samples in estcapability_fold_concurrent/concurrent_mixed/4
                        time:   [15.360 s 15.485 s 15.602 s]
                        thrpt:  [128.19  elem/s 129.16  elem/s 130.21  elem/s]
Benchmarking capability_fold_concurrent/concurrent_index/8: Collecting 20 samples in estcapability_fold_concurrent/concurrent_index/8
                        time:   [209.27 ms 219.30 ms 229.82 ms]
                        thrpt:  [17.405 Kelem/s 18.239 Kelem/s 19.114 Kelem/s]
Benchmarking capability_fold_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 1153.2s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_query/8: Collecting 20 samples in estcapability_fold_concurrent/concurrent_query/8
                        time:   [46.299 s 47.536 s 49.303 s]
                        thrpt:  [81.131  elem/s 84.147  elem/s 86.395  elem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) low mild
  2 (10.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 344.1s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_mixed/8: Collecting 20 samples in estcapability_fold_concurrent/concurrent_mixed/8
                        time:   [17.511 s 17.641 s 17.782 s]
                        thrpt:  [224.94  elem/s 226.75  elem/s 228.43  elem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking capability_fold_concurrent/concurrent_index/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 7.5s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_index/16: Collecting 20 samples in escapability_fold_concurrent/concurrent_index/16
                        time:   [344.95 ms 357.28 ms 369.85 ms]
                        thrpt:  [21.630 Kelem/s 22.392 Kelem/s 23.192 Kelem/s]
Benchmarking capability_fold_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 1830.1s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_query/16: Collecting 20 samples in escapability_fold_concurrent/concurrent_query/16
                        time:   [80.806 s 82.015 s 83.538 s]
                        thrpt:  [95.765  elem/s 97.544  elem/s 99.002  elem/s]
Found 3 outliers among 20 measurements (15.00%)
  1 (5.00%) high mild
  2 (10.00%) high severe
Benchmarking capability_fold_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 682.0s, or reduce sample count to 10.
Benchmarking capability_fold_concurrent/concurrent_mixed/16: Collecting 20 samples in escapability_fold_concurrent/concurrent_mixed/16
                        time:   [32.283 s 34.937 s 37.692 s]
                        thrpt:  [212.25  elem/s 228.98  elem/s 247.81  elem/s]
Found 4 outliers among 20 measurements (20.00%)
  1 (5.00%) low severe
  2 (10.00%) low mild
  1 (5.00%) high severe

Benchmarking capability_fold_updates/update_higher_version: Collecting 100 samples in escapability_fold_updates/update_higher_version
                        time:   [235.54 µs 235.81 µs 236.09 µs]
                        thrpt:  [4.2356 Kelem/s 4.2407 Kelem/s 4.2455 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  1 (1.00%) low mild
  5 (5.00%) high mild
  3 (3.00%) high severe
Benchmarking capability_fold_updates/update_same_version: Collecting 100 samples in esticapability_fold_updates/update_same_version
                        time:   [234.47 µs 234.93 µs 235.43 µs]
                        thrpt:  [4.2475 Kelem/s 4.2566 Kelem/s 4.2649 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking capability_fold_updates/remove_and_readd: Collecting 100 samples in estimatcapability_fold_updates/remove_and_readd
                        time:   [245.29 µs 246.04 µs 246.90 µs]
                        thrpt:  [4.0503 Kelem/s 4.0643 Kelem/s 4.0768 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) high mild
  5 (5.00%) high severe

Benchmarking location_info/create: Collecting 100 samples in estimated 5.0003 s (44M itelocation_info/create    time:   [112.69 ns 113.06 ns 113.73 ns]
                        thrpt:  [8.7924 Melem/s 8.8446 Melem/s 8.8739 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  2 (2.00%) high mild
  7 (7.00%) high severe
Benchmarking location_info/distance_to: Collecting 100 samples in estimated 5.0000 s (81location_info/distance_to
                        time:   [6.0697 ns 6.0847 ns 6.1009 ns]
                        thrpt:  [163.91 Melem/s 164.35 Melem/s 164.75 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking location_info/same_continent: Collecting 100 samples in estimated 5.0000 s location_info/same_continent
                        time:   [5.7259 ns 5.7428 ns 5.7628 ns]
                        thrpt:  [173.53 Melem/s 174.13 Melem/s 174.65 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  9 (9.00%) high mild
  4 (4.00%) high severe
Benchmarking location_info/same_continent_cross: Collecting 100 samples in estimated 5.0location_info/same_continent_cross
                        time:   [406.45 ps 406.94 ps 407.48 ps]
                        thrpt:  [2.4541 Gelem/s 2.4574 Gelem/s 2.4603 Gelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking location_info/same_region: Collecting 100 samples in estimated 5.0000 s (1.location_info/same_region
                        time:   [4.8951 ns 4.9055 ns 4.9174 ns]
                        thrpt:  [203.36 Melem/s 203.85 Melem/s 204.29 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking topology_hints/create: Collecting 100 samples in estimated 5.0000 s (890M itopology_hints/create   time:   [5.0182 ns 5.1377 ns 5.2503 ns]
                        thrpt:  [190.47 Melem/s 194.64 Melem/s 199.27 Melem/s]
Benchmarking topology_hints/connectivity_score: Collecting 100 samples in estimated 5.00topology_hints/connectivity_score
                        time:   [272.93 ps 273.92 ps 275.13 ps]
                        thrpt:  [3.6347 Gelem/s 3.6507 Gelem/s 3.6640 Gelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking topology_hints/average_latency_empty: Collecting 100 samples in estimated 5topology_hints/average_latency_empty
                        time:   [815.53 ps 816.59 ps 817.82 ps]
                        thrpt:  [1.2228 Gelem/s 1.2246 Gelem/s 1.2262 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking topology_hints/average_latency_100: Collecting 100 samples in estimated 5.0topology_hints/average_latency_100
                        time:   [89.878 ns 89.981 ns 90.099 ns]
                        thrpt:  [11.099 Melem/s 11.113 Melem/s 11.126 Melem/s]
Found 13 outliers among 100 measurements (13.00%)
  6 (6.00%) high mild
  7 (7.00%) high severe

Benchmarking nat_type/difficulty: Collecting 100 samples in estimated 5.0000 s (18B iternat_type/difficulty     time:   [273.44 ps 274.15 ps 274.99 ps]
                        thrpt:  [3.6364 Gelem/s 3.6476 Gelem/s 3.6570 Gelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking nat_type/can_connect_direct: Collecting 100 samples in estimated 5.0000 s (nat_type/can_connect_direct
                        time:   [273.71 ps 274.39 ps 275.14 ps]
                        thrpt:  [3.6345 Gelem/s 3.6445 Gelem/s 3.6535 Gelem/s]
Found 9 outliers among 100 measurements (9.00%)
  5 (5.00%) high mild
  4 (4.00%) high severe
Benchmarking nat_type/can_connect_symmetric: Collecting 100 samples in estimated 5.0000 nat_type/can_connect_symmetric
                        time:   [275.22 ps 276.51 ps 278.14 ps]
                        thrpt:  [3.5954 Gelem/s 3.6165 Gelem/s 3.6335 Gelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe

Benchmarking node_metadata/create_simple: Collecting 100 samples in estimated 5.0001 s (node_metadata/create_simple
                        time:   [58.554 ns 59.174 ns 59.786 ns]
                        thrpt:  [16.726 Melem/s 16.899 Melem/s 17.078 Melem/s]
Benchmarking node_metadata/create_full: Collecting 100 samples in estimated 5.0041 s (5.node_metadata/create_full
                        time:   [851.48 ns 857.95 ns 865.28 ns]
                        thrpt:  [1.1557 Melem/s 1.1656 Melem/s 1.1744 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking node_metadata/routing_score: Collecting 100 samples in estimated 5.0000 s (node_metadata/routing_score
                        time:   [272.25 ps 272.97 ps 273.88 ps]
                        thrpt:  [3.6512 Gelem/s 3.6634 Gelem/s 3.6731 Gelem/s]
Found 12 outliers among 100 measurements (12.00%)
  8 (8.00%) high mild
  4 (4.00%) high severe
Benchmarking node_metadata/age: Collecting 100 samples in estimated 5.0000 s (130M iteranode_metadata/age       time:   [38.501 ns 38.533 ns 38.567 ns]
                        thrpt:  [25.929 Melem/s 25.952 Melem/s 25.973 Melem/s]
Found 20 outliers among 100 measurements (20.00%)
  8 (8.00%) high mild
  12 (12.00%) high severe
Benchmarking node_metadata/is_stale: Collecting 100 samples in estimated 5.0002 s (133M node_metadata/is_stale  time:   [37.741 ns 37.803 ns 37.866 ns]
                        thrpt:  [26.409 Melem/s 26.453 Melem/s 26.496 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking node_metadata/serialize: Collecting 100 samples in estimated 5.0057 s (4.2Mnode_metadata/serialize time:   [1.1956 µs 1.2053 µs 1.2152 µs]
                        thrpt:  [822.89 Kelem/s 829.65 Kelem/s 836.42 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking node_metadata/deserialize: Collecting 100 samples in estimated 5.0150 s (1.node_metadata/deserialize
                        time:   [3.9096 µs 3.9257 µs 3.9437 µs]
                        thrpt:  [253.57 Kelem/s 254.73 Kelem/s 255.78 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  5 (5.00%) high mild
  2 (2.00%) high severe

Benchmarking metadata_query/match_status: Collecting 100 samples in estimated 5.0000 s (metadata_query/match_status
                        time:   [5.3253 ns 5.3357 ns 5.3467 ns]
                        thrpt:  [187.03 Melem/s 187.42 Melem/s 187.78 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking metadata_query/match_min_tier: Collecting 100 samples in estimated 5.0000 smetadata_query/match_min_tier
                        time:   [5.2816 ns 5.2922 ns 5.3065 ns]
                        thrpt:  [188.45 Melem/s 188.96 Melem/s 189.34 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  7 (7.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_query/match_continent: Collecting 100 samples in estimated 5.0000 metadata_query/match_continent
                        time:   [11.733 ns 11.775 ns 11.821 ns]
                        thrpt:  [84.592 Melem/s 84.929 Melem/s 85.232 Melem/s]
Found 12 outliers among 100 measurements (12.00%)
  7 (7.00%) high mild
  5 (5.00%) high severe
Benchmarking metadata_query/match_complex: Collecting 100 samples in estimated 5.0000 s metadata_query/match_complex
                        time:   [11.445 ns 11.472 ns 11.502 ns]
                        thrpt:  [86.942 Melem/s 87.172 Melem/s 87.376 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  5 (5.00%) high mild
  6 (6.00%) high severe
Benchmarking metadata_query/match_no_match: Collecting 100 samples in estimated 5.0000 smetadata_query/match_no_match
                        time:   [2.7095 ns 2.7117 ns 2.7142 ns]
                        thrpt:  [368.43 Melem/s 368.77 Melem/s 369.07 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe

Benchmarking metadata_store_basic/create: Collecting 100 samples in estimated 5.0375 s (metadata_store_basic/create
                        time:   [9.7446 µs 10.143 µs 10.775 µs]
                        thrpt:  [92.807 Kelem/s 98.595 Kelem/s 102.62 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe
Benchmarking metadata_store_basic/upsert_new: Collecting 100 samples in estimated 5.0138metadata_store_basic/upsert_new
                        time:   [2.9889 µs 3.0121 µs 3.0372 µs]
                        thrpt:  [329.25 Kelem/s 331.99 Kelem/s 334.57 Kelem/s]
Found 16 outliers among 100 measurements (16.00%)
  3 (3.00%) low severe
  13 (13.00%) low mild
Benchmarking metadata_store_basic/upsert_existing: Collecting 100 samples in estimated 5metadata_store_basic/upsert_existing
                        time:   [1.8852 µs 1.9178 µs 1.9629 µs]
                        thrpt:  [509.45 Kelem/s 521.44 Kelem/s 530.45 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_basic/get: Collecting 100 samples in estimated 5.0000 s (113metadata_store_basic/get
                        time:   [44.357 ns 44.523 ns 44.732 ns]
                        thrpt:  [22.355 Melem/s 22.460 Melem/s 22.544 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  4 (4.00%) high mild
  5 (5.00%) high severe
Benchmarking metadata_store_basic/get_miss: Collecting 100 samples in estimated 5.0001 smetadata_store_basic/get_miss
                        time:   [44.219 ns 44.277 ns 44.343 ns]
                        thrpt:  [22.551 Melem/s 22.585 Melem/s 22.615 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_store_basic/len: Collecting 100 samples in estimated 5.0016 s (3.5metadata_store_basic/len
                        time:   [1.4123 µs 1.4145 µs 1.4171 µs]
                        thrpt:  [705.65 Kelem/s 706.95 Kelem/s 708.06 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) high mild
  3 (3.00%) high severe
Benchmarking metadata_store_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 17.6s, or reduce sample count to 20.
Benchmarking metadata_store_basic/stats: Collecting 100 samples in estimated 17.643 s (1metadata_store_basic/stats
                        time:   [175.53 ms 176.05 ms 176.60 ms]
                        thrpt:  [5.6625  elem/s 5.6802  elem/s 5.6970  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking metadata_store_query/query_by_status: Collecting 100 samples in estimated 5metadata_store_query/query_by_status
                        time:   [584.97 µs 589.90 µs 595.22 µs]
                        thrpt:  [1.6800 Kelem/s 1.6952 Kelem/s 1.7095 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) high mild
  3 (3.00%) high severe
Benchmarking metadata_store_query/query_by_continent: Collecting 100 samples in estimatemetadata_store_query/query_by_continent
                        time:   [303.21 µs 304.51 µs 305.89 µs]
                        thrpt:  [3.2691 Kelem/s 3.2840 Kelem/s 3.2981 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  5 (5.00%) high mild
Benchmarking metadata_store_query/query_by_tier: Collecting 100 samples in estimated 8.5metadata_store_query/query_by_tier
                        time:   [841.41 µs 844.74 µs 848.77 µs]
                        thrpt:  [1.1782 Kelem/s 1.1838 Kelem/s 1.1885 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking metadata_store_query/query_accepting_work: Collecting 100 samples in estimametadata_store_query/query_accepting_work
                        time:   [951.10 µs 955.19 µs 959.73 µs]
                        thrpt:  [1.0420 Kelem/s 1.0469 Kelem/s 1.0514 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_store_query/query_with_limit: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.1s, enable flat sampling, or reduce sample count to 70.
Benchmarking metadata_store_query/query_with_limit: Collecting 100 samples in estimated metadata_store_query/query_with_limit
                        time:   [986.54 µs 991.45 µs 997.33 µs]
                        thrpt:  [1.0027 Kelem/s 1.0086 Kelem/s 1.0136 Kelem/s]
Found 9 outliers among 100 measurements (9.00%)
  7 (7.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_query/query_complex: Collecting 100 samples in estimated 5.4metadata_store_query/query_complex
                        time:   [530.60 µs 533.49 µs 536.61 µs]
                        thrpt:  [1.8636 Kelem/s 1.8745 Kelem/s 1.8846 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) high mild
  4 (4.00%) high severe

Benchmarking metadata_store_spatial/find_nearby_100km: Collecting 100 samples in estimatmetadata_store_spatial/find_nearby_100km
                        time:   [564.05 µs 568.44 µs 573.84 µs]
                        thrpt:  [1.7427 Kelem/s 1.7592 Kelem/s 1.7729 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_store_spatial/find_nearby_1000km: Collecting 100 samples in estimametadata_store_spatial/find_nearby_1000km
                        time:   [656.54 µs 658.46 µs 660.40 µs]
                        thrpt:  [1.5142 Kelem/s 1.5187 Kelem/s 1.5231 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_store_spatial/find_nearby_5000km: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 5.6s, enable flat sampling, or reduce sample count to 60.
Benchmarking metadata_store_spatial/find_nearby_5000km: Collecting 100 samples in estimametadata_store_spatial/find_nearby_5000km
                        time:   [1.1112 ms 1.1155 ms 1.1207 ms]
                        thrpt:  [892.33  elem/s 896.45  elem/s 899.92  elem/s]
Found 8 outliers among 100 measurements (8.00%)
  6 (6.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_spatial/find_best_for_routing: Collecting 100 samples in estmetadata_store_spatial/find_best_for_routing
                        time:   [724.34 µs 727.48 µs 730.83 µs]
                        thrpt:  [1.3683 Kelem/s 1.3746 Kelem/s 1.3806 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking metadata_store_spatial/find_relays: Collecting 100 samples in estimated 9.7metadata_store_spatial/find_relays
                        time:   [958.52 µs 964.15 µs 970.67 µs]
                        thrpt:  [1.0302 Kelem/s 1.0372 Kelem/s 1.0433 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  2 (2.00%) high mild
  6 (6.00%) high severe

Benchmarking metadata_store_scaling/query_status/1000: Collecting 100 samples in estimatmetadata_store_scaling/query_status/1000
                        time:   [46.043 µs 46.163 µs 46.285 µs]
                        thrpt:  [21.605 Kelem/s 21.662 Kelem/s 21.719 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking metadata_store_scaling/query_complex/1000: Collecting 100 samples in estimametadata_store_scaling/query_complex/1000
                        time:   [44.592 µs 44.670 µs 44.751 µs]
                        thrpt:  [22.346 Kelem/s 22.387 Kelem/s 22.425 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) high mild
  1 (1.00%) high severe
Benchmarking metadata_store_scaling/find_nearby/1000: Collecting 100 samples in estimatemetadata_store_scaling/find_nearby/1000
                        time:   [92.433 µs 92.568 µs 92.711 µs]
                        thrpt:  [10.786 Kelem/s 10.803 Kelem/s 10.819 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  3 (3.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_scaling/query_status/5000: Collecting 100 samples in estimatmetadata_store_scaling/query_status/5000
                        time:   [257.12 µs 257.91 µs 258.72 µs]
                        thrpt:  [3.8652 Kelem/s 3.8774 Kelem/s 3.8892 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_scaling/query_complex/5000: Collecting 100 samples in estimametadata_store_scaling/query_complex/5000
                        time:   [256.16 µs 256.97 µs 257.89 µs]
                        thrpt:  [3.8776 Kelem/s 3.8914 Kelem/s 3.9038 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_scaling/find_nearby/5000: Collecting 100 samples in estimatemetadata_store_scaling/find_nearby/5000
                        time:   [430.28 µs 431.21 µs 432.24 µs]
                        thrpt:  [2.3135 Kelem/s 2.3191 Kelem/s 2.3241 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) high mild
  2 (2.00%) high severe
Benchmarking metadata_store_scaling/query_status/10000: Collecting 100 samples in estimametadata_store_scaling/query_status/10000
                        time:   [632.86 µs 651.32 µs 673.13 µs]
                        thrpt:  [1.4856 Kelem/s 1.5353 Kelem/s 1.5801 Kelem/s]
Found 8 outliers among 100 measurements (8.00%)
  4 (4.00%) high mild
  4 (4.00%) high severe
Benchmarking metadata_store_scaling/query_complex/10000: Collecting 100 samples in estimmetadata_store_scaling/query_complex/10000
                        time:   [837.48 µs 868.29 µs 897.17 µs]
                        thrpt:  [1.1146 Kelem/s 1.1517 Kelem/s 1.1941 Kelem/s]
Benchmarking metadata_store_scaling/find_nearby/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 6.5s, enable flat sampling, or reduce sample count to 60.
Benchmarking metadata_store_scaling/find_nearby/10000: Collecting 100 samples in estimatmetadata_store_scaling/find_nearby/10000
                        time:   [1.2451 ms 1.2774 ms 1.3114 ms]
                        thrpt:  [762.57  elem/s 782.85  elem/s 803.16  elem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking metadata_store_scaling/query_status/50000: Collecting 100 samples in estimametadata_store_scaling/query_status/50000
                        time:   [11.398 ms 11.448 ms 11.500 ms]
                        thrpt:  [86.953  elem/s 87.349  elem/s 87.736  elem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking metadata_store_scaling/query_complex/50000: Collecting 100 samples in estimmetadata_store_scaling/query_complex/50000
                        time:   [8.5062 ms 8.8412 ms 9.1624 ms]
                        thrpt:  [109.14  elem/s 113.11  elem/s 117.56  elem/s]
Benchmarking metadata_store_scaling/find_nearby/50000: Collecting 100 samples in estimatmetadata_store_scaling/find_nearby/50000
                        time:   [6.5549 ms 6.7694 ms 6.9877 ms]
                        thrpt:  [143.11  elem/s 147.72  elem/s 152.56  elem/s]

Benchmarking metadata_store_concurrent/concurrent_upsert/4: Collecting 20 samples in estmetadata_store_concurrent/concurrent_upsert/4
                        time:   [1.9929 ms 2.0055 ms 2.0193 ms]
                        thrpt:  [990.43 Kelem/s 997.27 Kelem/s 1.0035 Melem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 11.1s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_query/4: Collecting 20 samples in estimetadata_store_concurrent/concurrent_query/4
                        time:   [556.91 ms 560.41 ms 564.19 ms]
                        thrpt:  [3.5449 Kelem/s 3.5688 Kelem/s 3.5912 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 10.2s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_mixed/4: Collecting 20 samples in estimetadata_store_concurrent/concurrent_mixed/4
                        time:   [499.89 ms 502.42 ms 505.21 ms]
                        thrpt:  [3.9588 Kelem/s 3.9807 Kelem/s 4.0009 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_upsert/8: Collecting 20 samples in estmetadata_store_concurrent/concurrent_upsert/8
                        time:   [2.6249 ms 2.6645 ms 2.7154 ms]
                        thrpt:  [1.4731 Melem/s 1.5012 Melem/s 1.5239 Melem/s]
Found 2 outliers among 20 measurements (10.00%)
  1 (5.00%) high mild
  1 (5.00%) high severe
Benchmarking metadata_store_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 15.2s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_query/8: Collecting 20 samples in estimetadata_store_concurrent/concurrent_query/8
                        time:   [745.87 ms 759.22 ms 774.93 ms]
                        thrpt:  [5.1618 Kelem/s 5.2686 Kelem/s 5.3629 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 11.7s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_mixed/8: Collecting 20 samples in estimetadata_store_concurrent/concurrent_mixed/8
                        time:   [590.15 ms 604.37 ms 620.99 ms]
                        thrpt:  [6.4413 Kelem/s 6.6185 Kelem/s 6.7780 Kelem/s]
Found 1 outliers among 20 measurements (5.00%)
  1 (5.00%) high mild
Benchmarking metadata_store_concurrent/concurrent_upsert/16: Collecting 20 samples in esmetadata_store_concurrent/concurrent_upsert/16
                        time:   [7.0369 ms 7.2027 ms 7.3913 ms]
                        thrpt:  [1.0824 Melem/s 1.1107 Melem/s 1.1369 Melem/s]
Benchmarking metadata_store_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 34.1s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_query/16: Collecting 20 samples in estmetadata_store_concurrent/concurrent_query/16
                        time:   [1.7067 s 1.7358 s 1.7673 s]
                        thrpt:  [4.5267 Kelem/s 4.6087 Kelem/s 4.6875 Kelem/s]
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 28.3s, or reduce sample count to 10.
Benchmarking metadata_store_concurrent/concurrent_mixed/16: Collecting 20 samples in estmetadata_store_concurrent/concurrent_mixed/16
                        time:   [1.4207 s 1.4472 s 1.4760 s]
                        thrpt:  [5.4199 Kelem/s 5.5278 Kelem/s 5.6309 Kelem/s]

Benchmarking metadata_store_versioning/update_versioned_success: Collecting 100 samples metadata_store_versioning/update_versioned_success
                        time:   [595.79 ns 602.86 ns 609.64 ns]
                        thrpt:  [1.6403 Melem/s 1.6588 Melem/s 1.6784 Melem/s]
Found 10 outliers among 100 measurements (10.00%)
  8 (8.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking metadata_store_versioning/update_versioned_conflict: Warming up for 3.0000 Benchmarking metadata_store_versioning/update_versioned_conflict: Collecting 100 samplesmetadata_store_versioning/update_versioned_conflict
                        time:   [616.54 ns 624.27 ns 631.57 ns]
                        thrpt:  [1.5834 Melem/s 1.6019 Melem/s 1.6220 Melem/s]
Found 7 outliers among 100 measurements (7.00%)
  3 (3.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe

Benchmarking schema_validation/validate_string: Collecting 100 samples in estimated 5.00schema_validation/validate_string
                        time:   [10.087 ns 10.379 ns 10.675 ns]
                        thrpt:  [93.676 Melem/s 96.348 Melem/s 99.133 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking schema_validation/validate_integer: Collecting 100 samples in estimated 5.0schema_validation/validate_integer
                        time:   [10.353 ns 10.477 ns 10.611 ns]
                        thrpt:  [94.245 Melem/s 95.447 Melem/s 96.588 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking schema_validation/validate_object: Collecting 100 samples in estimated 5.00schema_validation/validate_object
                        time:   [151.07 ns 154.90 ns 158.53 ns]
                        thrpt:  [6.3078 Melem/s 6.4559 Melem/s 6.6193 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking schema_validation/validate_array_10: Collecting 100 samples in estimated 5.schema_validation/validate_array_10
                        time:   [84.420 ns 85.615 ns 86.744 ns]
                        thrpt:  [11.528 Melem/s 11.680 Melem/s 11.846 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking schema_validation/validate_complex: Collecting 100 samples in estimated 5.0schema_validation/validate_complex
                        time:   [496.11 ns 507.98 ns 520.62 ns]
                        thrpt:  [1.9208 Melem/s 1.9686 Melem/s 2.0157 Melem/s]

Benchmarking endpoint_matching/match_success: Collecting 100 samples in estimated 5.0003endpoint_matching/match_success
                        time:   [615.32 ns 623.49 ns 631.73 ns]
                        thrpt:  [1.5830 Melem/s 1.6039 Melem/s 1.6252 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking endpoint_matching/match_failure: Collecting 100 samples in estimated 5.0008endpoint_matching/match_failure
                        time:   [551.28 ns 559.50 ns 567.99 ns]
                        thrpt:  [1.7606 Melem/s 1.7873 Melem/s 1.8140 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
  1 (1.00%) high severe
Benchmarking endpoint_matching/match_multi_param: Collecting 100 samples in estimated 5.endpoint_matching/match_multi_param
                        time:   [1.2918 µs 1.3077 µs 1.3241 µs]
                        thrpt:  [755.24 Kelem/s 764.71 Kelem/s 774.09 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low severe
  3 (3.00%) low mild

Benchmarking api_version/is_compatible_with: Collecting 100 samples in estimated 5.0000 api_version/is_compatible_with
                        time:   [369.54 ps 376.99 ps 384.51 ps]
                        thrpt:  [2.6007 Gelem/s 2.6526 Gelem/s 2.7060 Gelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  3 (3.00%) high mild
Benchmarking api_version/parse: Collecting 100 samples in estimated 5.0001 s (39M iteratapi_version/parse       time:   [125.99 ns 128.91 ns 131.59 ns]
                        thrpt:  [7.5994 Melem/s 7.7574 Melem/s 7.9374 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking api_version/to_string: Collecting 100 samples in estimated 5.0005 s (33M itapi_version/to_string   time:   [144.29 ns 147.75 ns 151.40 ns]
                        thrpt:  [6.6052 Melem/s 6.7680 Melem/s 6.9303 Melem/s]

Benchmarking api_schema/create: Collecting 100 samples in estimated 5.0024 s (980k iteraapi_schema/create       time:   [6.2604 µs 6.3254 µs 6.3883 µs]
                        thrpt:  [156.54 Kelem/s 158.09 Kelem/s 159.73 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  3 (3.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking api_schema/serialize: Collecting 100 samples in estimated 5.0110 s (1.2M itapi_schema/serialize    time:   [4.1795 µs 4.2346 µs 4.2909 µs]
                        thrpt:  [233.05 Kelem/s 236.15 Kelem/s 239.26 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking api_schema/deserialize: Collecting 100 samples in estimated 5.0917 s (273k api_schema/deserialize  time:   [17.292 µs 17.538 µs 17.770 µs]
                        thrpt:  [56.273 Kelem/s 57.018 Kelem/s 57.829 Kelem/s]
Benchmarking api_schema/find_endpoint: Collecting 100 samples in estimated 5.0031 s (8.2api_schema/find_endpoint
                        time:   [568.73 ns 578.37 ns 589.09 ns]
                        thrpt:  [1.6975 Melem/s 1.7290 Melem/s 1.7583 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking api_schema/endpoints_by_tag: Collecting 100 samples in estimated 5.0002 s (api_schema/endpoints_by_tag
                        time:   [269.90 ns 273.56 ns 277.18 ns]
                        thrpt:  [3.6077 Melem/s 3.6555 Melem/s 3.7050 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  5 (5.00%) low mild
  1 (1.00%) high severe

Benchmarking request_validation/validate_full_request: Collecting 100 samples in estimatrequest_validation/validate_full_request
                        time:   [145.09 ns 146.84 ns 148.75 ns]
                        thrpt:  [6.7229 Melem/s 6.8099 Melem/s 6.8922 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) low mild
Benchmarking request_validation/validate_path_only: Collecting 100 samples in estimated request_validation/validate_path_only
                        time:   [43.581 ns 44.698 ns 45.789 ns]
                        thrpt:  [21.839 Melem/s 22.372 Melem/s 22.946 Melem/s]

Benchmarking api_registry_basic/create: Collecting 100 samples in estimated 5.0236 s (96api_registry_basic/create
                        time:   [5.2757 µs 5.3324 µs 5.3878 µs]
                        thrpt:  [185.61 Kelem/s 187.53 Kelem/s 189.55 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) low mild
  2 (2.00%) high mild
Benchmarking api_registry_basic/register_new: Collecting 100 samples in estimated 5.0395api_registry_basic/register_new
                        time:   [9.4698 µs 9.6233 µs 9.7819 µs]
                        thrpt:  [102.23 Kelem/s 103.91 Kelem/s 105.60 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking api_registry_basic/get: Collecting 100 samples in estimated 5.0001 s (89M iapi_registry_basic/get  time:   [52.723 ns 53.382 ns 54.019 ns]
                        thrpt:  [18.512 Melem/s 18.733 Melem/s 18.967 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking api_registry_basic/len: Collecting 100 samples in estimated 5.0017 s (3.0M api_registry_basic/len  time:   [1.6860 µs 1.7030 µs 1.7191 µs]
                        thrpt:  [581.71 Kelem/s 587.19 Kelem/s 593.12 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking api_registry_basic/stats: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 13.1s, or reduce sample count to 30.
Benchmarking api_registry_basic/stats: Collecting 100 samples in estimated 13.054 s (100api_registry_basic/stats
                        time:   [132.93 ms 133.98 ms 135.04 ms]
                        thrpt:  [7.4051  elem/s 7.4636  elem/s 7.5229  elem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild

Benchmarking api_registry_query/query_by_name: Collecting 100 samples in estimated 5.545api_registry_query/query_by_name
                        time:   [270.48 µs 275.15 µs 279.54 µs]
                        thrpt:  [3.5773 Kelem/s 3.6344 Kelem/s 3.6972 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high severe
Benchmarking api_registry_query/query_by_tag: Collecting 100 samples in estimated 5.0323api_registry_query/query_by_tag
                        time:   [2.3648 ms 2.4492 ms 2.5363 ms]
                        thrpt:  [394.28  elem/s 408.30  elem/s 422.87  elem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high mild
Benchmarking api_registry_query/query_with_version: Collecting 100 samples in estimated api_registry_query/query_with_version
                        time:   [136.96 µs 138.70 µs 140.35 µs]
                        thrpt:  [7.1248 Kelem/s 7.2098 Kelem/s 7.3015 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking api_registry_query/find_by_endpoint: Collecting 100 samples in estimated 5.api_registry_query/find_by_endpoint
                        time:   [13.349 ms 13.531 ms 13.710 ms]
                        thrpt:  [72.942  elem/s 73.907  elem/s 74.913  elem/s]
Benchmarking api_registry_query/find_compatible: Collecting 100 samples in estimated 5.2api_registry_query/find_compatible
                        time:   [176.35 µs 180.10 µs 184.03 µs]
                        thrpt:  [5.4339 Kelem/s 5.5524 Kelem/s 5.6705 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild

Benchmarking api_registry_scaling/query_by_name/1000: Collecting 100 samples in estimateapi_registry_scaling/query_by_name/1000
                        time:   [19.895 µs 20.124 µs 20.341 µs]
                        thrpt:  [49.161 Kelem/s 49.692 Kelem/s 50.264 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  2 (2.00%) high mild
  1 (1.00%) high severe
Benchmarking api_registry_scaling/query_by_tag/1000: Collecting 100 samples in estimatedapi_registry_scaling/query_by_tag/1000
                        time:   [106.85 µs 108.70 µs 110.51 µs]
                        thrpt:  [9.0486 Kelem/s 9.1998 Kelem/s 9.3592 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking api_registry_scaling/query_by_name/5000: Collecting 100 samples in estimateapi_registry_scaling/query_by_name/5000
                        time:   [105.90 µs 107.70 µs 109.49 µs]
                        thrpt:  [9.1333 Kelem/s 9.2850 Kelem/s 9.4428 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking api_registry_scaling/query_by_tag/5000: Collecting 100 samples in estimatedapi_registry_scaling/query_by_tag/5000
                        time:   [815.60 µs 834.07 µs 852.42 µs]
                        thrpt:  [1.1731 Kelem/s 1.1989 Kelem/s 1.2261 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking api_registry_scaling/query_by_name/10000: Collecting 100 samples in estimatapi_registry_scaling/query_by_name/10000
                        time:   [238.83 µs 241.27 µs 243.83 µs]
                        thrpt:  [4.1013 Kelem/s 4.1448 Kelem/s 4.1871 Kelem/s]
Found 12 outliers among 100 measurements (12.00%)
  1 (1.00%) low severe
  5 (5.00%) high mild
  6 (6.00%) high severe
Benchmarking api_registry_scaling/query_by_tag/10000: Warming up for 3.0000 s
Warning: Unable to complete 100 samples in 5.0s. You may wish to increase target time to 9.1s, enable flat sampling, or reduce sample count to 50.
Benchmarking api_registry_scaling/query_by_tag/10000: Collecting 100 samples in estimateapi_registry_scaling/query_by_tag/10000
                        time:   [1.8736 ms 1.9327 ms 1.9969 ms]
                        thrpt:  [500.77  elem/s 517.41  elem/s 533.73  elem/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe

Benchmarking api_registry_concurrent/concurrent_query/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 31.9s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_query/4: Collecting 20 samples in estimaapi_registry_concurrent/concurrent_query/4
                        time:   [1.6835 s 1.6964 s 1.7101 s]
                        thrpt:  [1.1695 Kelem/s 1.1790 Kelem/s 1.1880 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/4: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 23.3s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_mixed/4: Collecting 20 samples in estimaapi_registry_concurrent/concurrent_mixed/4
                        time:   [1.2205 s 1.2380 s 1.2577 s]
                        thrpt:  [1.5902 Kelem/s 1.6155 Kelem/s 1.6387 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 38.6s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_query/8: Collecting 20 samples in estimaapi_registry_concurrent/concurrent_query/8
                        time:   [1.9316 s 1.9527 s 1.9755 s]
                        thrpt:  [2.0248 Kelem/s 2.0484 Kelem/s 2.0708 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_mixed/8: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 31.1s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_mixed/8: Collecting 20 samples in estimaapi_registry_concurrent/concurrent_mixed/8
                        time:   [1.5305 s 1.5548 s 1.5809 s]
                        thrpt:  [2.5302 Kelem/s 2.5727 Kelem/s 2.6135 Kelem/s]
Benchmarking api_registry_concurrent/concurrent_query/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 68.2s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_query/16: Collecting 20 samples in estimapi_registry_concurrent/concurrent_query/16
                        time:   [3.3425 s 3.3940 s 3.4489 s]
                        thrpt:  [2.3196 Kelem/s 2.3571 Kelem/s 2.3934 Kelem/s]
Found 2 outliers among 20 measurements (10.00%)
  2 (10.00%) high mild
Benchmarking api_registry_concurrent/concurrent_mixed/16: Warming up for 3.0000 s
Warning: Unable to complete 20 samples in 5.0s. You may wish to increase target time to 62.8s, or reduce sample count to 10.
Benchmarking api_registry_concurrent/concurrent_mixed/16: Collecting 20 samples in estimapi_registry_concurrent/concurrent_mixed/16
                        time:   [2.9753 s 3.0340 s 3.0974 s]
                        thrpt:  [2.5828 Kelem/s 2.6368 Kelem/s 2.6888 Kelem/s]

Benchmarking compare_op/eq: Collecting 100 samples in estimated 5.0000 s (1.2B iterationcompare_op/eq           time:   [4.2133 ns 4.2778 ns 4.3421 ns]
                        thrpt:  [230.31 Melem/s 233.77 Melem/s 237.35 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
Benchmarking compare_op/gt: Collecting 100 samples in estimated 5.0000 s (1.3B iterationcompare_op/gt           time:   [3.7101 ns 3.7695 ns 3.8263 ns]
                        thrpt:  [261.35 Melem/s 265.29 Melem/s 269.54 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
Benchmarking compare_op/contains_string: Collecting 100 samples in estimated 5.0001 s (1compare_op/contains_string
                        time:   [46.210 ns 46.938 ns 47.655 ns]
                        thrpt:  [20.984 Melem/s 21.305 Melem/s 21.640 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high severe
Benchmarking compare_op/in_array: Collecting 100 samples in estimated 5.0000 s (415M itecompare_op/in_array     time:   [12.081 ns 12.275 ns 12.459 ns]
                        thrpt:  [80.264 Melem/s 81.463 Melem/s 82.772 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild

Benchmarking condition/simple: Collecting 100 samples in estimated 5.0001 s (34M iteraticondition/simple        time:   [142.38 ns 144.72 ns 147.28 ns]
                        thrpt:  [6.7899 Melem/s 6.9100 Melem/s 7.0235 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking condition/nested_field: Collecting 100 samples in estimated 5.0076 s (3.1M condition/nested_field  time:   [1.6200 µs 1.6484 µs 1.6777 µs]
                        thrpt:  [596.06 Kelem/s 606.64 Kelem/s 617.30 Kelem/s]
Found 7 outliers among 100 measurements (7.00%)
  4 (4.00%) low mild
  3 (3.00%) high mild
Benchmarking condition/string_eq: Collecting 100 samples in estimated 5.0009 s (22M itercondition/string_eq     time:   [219.66 ns 223.60 ns 227.77 ns]
                        thrpt:  [4.3904 Melem/s 4.4723 Melem/s 4.5525 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild

Benchmarking condition_expr/single: Collecting 100 samples in estimated 5.0003 s (31M itcondition_expr/single   time:   [138.22 ns 140.96 ns 144.03 ns]
                        thrpt:  [6.9429 Melem/s 7.0942 Melem/s 7.2349 Melem/s]
Benchmarking condition_expr/and_2: Collecting 100 samples in estimated 5.0005 s (16M itecondition_expr/and_2    time:   [276.89 ns 284.13 ns 290.94 ns]
                        thrpt:  [3.4371 Melem/s 3.5195 Melem/s 3.6116 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking condition_expr/and_5: Collecting 100 samples in estimated 5.0031 s (5.7M itcondition_expr/and_5    time:   [860.98 ns 886.57 ns 910.14 ns]
                        thrpt:  [1.0987 Melem/s 1.1279 Melem/s 1.1615 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking condition_expr/or_3: Collecting 100 samples in estimated 5.0026 s (8.7M itecondition_expr/or_3     time:   [530.64 ns 537.22 ns 544.36 ns]
                        thrpt:  [1.8370 Melem/s 1.8614 Melem/s 1.8845 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  1 (1.00%) low severe
  4 (4.00%) low mild
  1 (1.00%) high mild
Benchmarking condition_expr/nested: Collecting 100 samples in estimated 5.0002 s (12M itcondition_expr/nested   time:   [389.81 ns 396.73 ns 403.90 ns]
                        thrpt:  [2.4759 Melem/s 2.5206 Melem/s 2.5653 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

rule/create             time:   [1.0274 µs 1.0437 µs 1.0622 µs]
                        thrpt:  [941.43 Kelem/s 958.12 Kelem/s 973.34 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  2 (2.00%) low mild
  1 (1.00%) high mild
rule/matches            time:   [298.24 ns 302.75 ns 307.29 ns]
                        thrpt:  [3.2543 Melem/s 3.3031 Melem/s 3.3530 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high severe

Benchmarking rule_context/create: Collecting 100 samples in estimated 5.0179 s (1.3M iterule_context/create     time:   [3.7805 µs 3.8578 µs 3.9281 µs]
                        thrpt:  [254.57 Kelem/s 259.22 Kelem/s 264.51 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking rule_context/get_simple: Collecting 100 samples in estimated 5.0005 s (40M rule_context/get_simple time:   [132.04 ns 134.06 ns 136.21 ns]
                        thrpt:  [7.3414 Melem/s 7.4594 Melem/s 7.5734 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking rule_context/get_nested: Collecting 100 samples in estimated 5.0012 s (3.2Mrule_context/get_nested time:   [1.5941 µs 1.6167 µs 1.6373 µs]
                        thrpt:  [610.77 Kelem/s 618.55 Kelem/s 627.30 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  2 (2.00%) low mild
  3 (3.00%) high mild
Benchmarking rule_context/get_deep_nested: Collecting 100 samples in estimated 5.0060 s rule_context/get_deep_nested
                        time:   [1.6500 µs 1.6710 µs 1.6907 µs]
                        thrpt:  [591.48 Kelem/s 598.44 Kelem/s 606.07 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild

Benchmarking rule_engine_basic/create: Collecting 100 samples in estimated 5.0000 s (187rule_engine_basic/create
                        time:   [25.075 ns 25.498 ns 25.968 ns]
                        thrpt:  [38.510 Melem/s 39.219 Melem/s 39.880 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking rule_engine_basic/add_rule: Collecting 100 samples in estimated 5.0292 s (5rule_engine_basic/add_rule
                        time:   [3.5158 µs 3.8798 µs 4.2422 µs]
                        thrpt:  [235.73 Kelem/s 257.75 Kelem/s 284.43 Kelem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) high mild
Benchmarking rule_engine_basic/get_rule: Collecting 100 samples in estimated 5.0002 s (1rule_engine_basic/get_rule
                        time:   [39.357 ns 40.386 ns 41.529 ns]
                        thrpt:  [24.080 Melem/s 24.761 Melem/s 25.408 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
Benchmarking rule_engine_basic/rules_by_tag: Collecting 100 samples in estimated 5.0118 rule_engine_basic/rules_by_tag
                        time:   [2.6154 µs 2.6517 µs 2.6881 µs]
                        thrpt:  [372.01 Kelem/s 377.12 Kelem/s 382.35 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
Benchmarking rule_engine_basic/stats: Collecting 100 samples in estimated 5.0912 s (207krule_engine_basic/stats time:   [24.869 µs 25.368 µs 25.835 µs]
                        thrpt:  [38.707 Kelem/s 39.420 Kelem/s 40.210 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking rule_engine_evaluate/evaluate_10_rules: Collecting 100 samples in estimatedrule_engine_evaluate/evaluate_10_rules
                        time:   [7.9266 µs 8.1033 µs 8.2935 µs]
                        thrpt:  [120.58 Kelem/s 123.41 Kelem/s 126.16 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
  2 (2.00%) high severe
Benchmarking rule_engine_evaluate/evaluate_first_10_rules: Collecting 100 samples in estrule_engine_evaluate/evaluate_first_10_rules
                        time:   [850.92 ns 866.89 ns 881.18 ns]
                        thrpt:  [1.1348 Melem/s 1.1535 Melem/s 1.1752 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking rule_engine_evaluate/evaluate_100_rules: Collecting 100 samples in estimaterule_engine_evaluate/evaluate_100_rules
                        time:   [95.421 µs 96.579 µs 97.681 µs]
                        thrpt:  [10.237 Kelem/s 10.354 Kelem/s 10.480 Kelem/s]
Found 6 outliers among 100 measurements (6.00%)
  6 (6.00%) low mild
Benchmarking rule_engine_evaluate/evaluate_first_100_rules: Collecting 100 samples in esrule_engine_evaluate/evaluate_first_100_rules
                        time:   [879.51 ns 897.58 ns 915.61 ns]
                        thrpt:  [1.0922 Melem/s 1.1141 Melem/s 1.1370 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
Benchmarking rule_engine_evaluate/evaluate_matching_100_rules: Collecting 100 samples inrule_engine_evaluate/evaluate_matching_100_rules
                        time:   [92.308 µs 93.746 µs 95.269 µs]
                        thrpt:  [10.497 Kelem/s 10.667 Kelem/s 10.833 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking rule_engine_evaluate/evaluate_1000_rules: Collecting 100 samples in estimatrule_engine_evaluate/evaluate_1000_rules
                        time:   [1.0436 ms 1.0628 ms 1.0824 ms]
                        thrpt:  [923.90  elem/s 940.91  elem/s 958.20  elem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking rule_engine_evaluate/evaluate_first_1000_rules: Collecting 100 samples in erule_engine_evaluate/evaluate_first_1000_rules
                        time:   [912.81 ns 924.93 ns 935.75 ns]
                        thrpt:  [1.0687 Melem/s 1.0812 Melem/s 1.0955 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  1 (1.00%) low severe
  6 (6.00%) low mild
  1 (1.00%) high severe

Benchmarking rule_engine_scaling/evaluate/10: Collecting 100 samples in estimated 5.0028rule_engine_scaling/evaluate/10
                        time:   [8.1373 µs 8.2873 µs 8.4235 µs]
                        thrpt:  [118.72 Kelem/s 120.67 Kelem/s 122.89 Kelem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) low mild
Benchmarking rule_engine_scaling/evaluate_first/10: Collecting 100 samples in estimated rule_engine_scaling/evaluate_first/10
                        time:   [906.49 ns 922.00 ns 936.54 ns]
                        thrpt:  [1.0678 Melem/s 1.0846 Melem/s 1.1032 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  3 (3.00%) low mild
  1 (1.00%) high mild
Benchmarking rule_engine_scaling/evaluate/50: Collecting 100 samples in estimated 5.0209rule_engine_scaling/evaluate/50
                        time:   [46.254 µs 47.142 µs 48.133 µs]
                        thrpt:  [20.776 Kelem/s 21.212 Kelem/s 21.620 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild
Benchmarking rule_engine_scaling/evaluate_first/50: Collecting 100 samples in estimated rule_engine_scaling/evaluate_first/50
                        time:   [913.24 ns 933.46 ns 955.09 ns]
                        thrpt:  [1.0470 Melem/s 1.0713 Melem/s 1.0950 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  1 (1.00%) low mild
  2 (2.00%) high mild
Benchmarking rule_engine_scaling/evaluate/100: Collecting 100 samples in estimated 5.080rule_engine_scaling/evaluate/100
                        time:   [92.475 µs 94.630 µs 97.019 µs]
                        thrpt:  [10.307 Kelem/s 10.567 Kelem/s 10.814 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking rule_engine_scaling/evaluate_first/100: Collecting 100 samples in estimatedrule_engine_scaling/evaluate_first/100
                        time:   [983.70 ns 1.0007 µs 1.0191 µs]
                        thrpt:  [981.24 Kelem/s 999.29 Kelem/s 1.0166 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  7 (7.00%) low mild
  1 (1.00%) high mild
Benchmarking rule_engine_scaling/evaluate/500: Collecting 100 samples in estimated 7.494rule_engine_scaling/evaluate/500
                        time:   [527.81 µs 541.08 µs 555.08 µs]
                        thrpt:  [1.8015 Kelem/s 1.8482 Kelem/s 1.8946 Kelem/s]
Found 5 outliers among 100 measurements (5.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
Benchmarking rule_engine_scaling/evaluate_first/500: Collecting 100 samples in estimatedrule_engine_scaling/evaluate_first/500
                        time:   [810.17 ns 824.40 ns 837.72 ns]
                        thrpt:  [1.1937 Melem/s 1.2130 Melem/s 1.2343 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  4 (4.00%) high mild
Benchmarking rule_engine_scaling/evaluate/1000: Collecting 100 samples in estimated 9.31rule_engine_scaling/evaluate/1000
                        time:   [942.43 µs 963.26 µs 983.79 µs]
                        thrpt:  [1.0165 Kelem/s 1.0381 Kelem/s 1.0611 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe
Benchmarking rule_engine_scaling/evaluate_first/1000: Collecting 100 samples in estimaterule_engine_scaling/evaluate_first/1000
                        time:   [872.75 ns 889.44 ns 906.79 ns]
                        thrpt:  [1.1028 Melem/s 1.1243 Melem/s 1.1458 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking rule_set/create: Collecting 100 samples in estimated 5.0494 s (318k iteratirule_set/create         time:   [14.982 µs 15.257 µs 15.527 µs]
                        thrpt:  [64.402 Kelem/s 65.543 Kelem/s 66.747 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) low mild
  1 (1.00%) high severe
Benchmarking rule_set/load_into_engine: Collecting 100 samples in estimated 5.0970 s (22rule_set/load_into_engine
                        time:   [24.312 µs 24.799 µs 25.300 µs]
                        thrpt:  [39.525 Kelem/s 40.324 Kelem/s 41.132 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking trace_id/generate: Collecting 100 samples in estimated 5.0003 s (45M iterattrace_id/generate       time:   [108.02 ns 109.09 ns 110.28 ns]
                        thrpt:  [9.0675 Melem/s 9.1665 Melem/s 9.2575 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  1 (1.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking trace_id/to_hex: Collecting 100 samples in estimated 5.0007 s (21M iteratiotrace_id/to_hex         time:   [237.37 ns 240.39 ns 243.37 ns]
                        thrpt:  [4.1090 Melem/s 4.1598 Melem/s 4.2129 Melem/s]
Found 11 outliers among 100 measurements (11.00%)
  2 (2.00%) low severe
  2 (2.00%) low mild
  6 (6.00%) high mild
  1 (1.00%) high severe
Benchmarking trace_id/from_hex: Collecting 100 samples in estimated 5.0002 s (109M iteratrace_id/from_hex       time:   [46.618 ns 47.065 ns 47.488 ns]
                        thrpt:  [21.058 Melem/s 21.247 Melem/s 21.451 Melem/s]
Found 6 outliers among 100 measurements (6.00%)
  4 (4.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe

Benchmarking context_operations/create: Collecting 100 samples in estimated 5.0009 s (25context_operations/create
                        time:   [198.17 ns 200.47 ns 202.73 ns]
                        thrpt:  [4.9326 Melem/s 4.9882 Melem/s 5.0461 Melem/s]
Found 8 outliers among 100 measurements (8.00%)
  3 (3.00%) low severe
  3 (3.00%) low mild
  1 (1.00%) high mild
  1 (1.00%) high severe
Benchmarking context_operations/child: Collecting 100 samples in estimated 5.0003 s (63Mcontext_operations/child
                        time:   [79.899 ns 80.812 ns 81.684 ns]
                        thrpt:  [12.242 Melem/s 12.374 Melem/s 12.516 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild
Benchmarking context_operations/for_remote: Collecting 100 samples in estimated 5.0003 scontext_operations/for_remote
                        time:   [79.388 ns 80.390 ns 81.389 ns]
                        thrpt:  [12.287 Melem/s 12.439 Melem/s 12.596 Melem/s]
Found 4 outliers among 100 measurements (4.00%)
  2 (2.00%) low mild
  2 (2.00%) high mild
Benchmarking context_operations/to_traceparent: Collecting 100 samples in estimated 5.00context_operations/to_traceparent
                        time:   [700.24 ns 712.12 ns 724.39 ns]
                        thrpt:  [1.3805 Melem/s 1.4043 Melem/s 1.4281 Melem/s]
Found 3 outliers among 100 measurements (3.00%)
  3 (3.00%) low mild
Benchmarking context_operations/from_traceparent: Collecting 100 samples in estimated 5.context_operations/from_traceparent
                        time:   [309.71 ns 313.60 ns 317.60 ns]
                        thrpt:  [3.1486 Melem/s 3.1887 Melem/s 3.2288 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) low mild

Benchmarking baggage/create: Collecting 100 samples in estimated 5.0000 s (795M iteratiobaggage/create          time:   [6.3546 ns 6.4362 ns 6.5124 ns]
                        thrpt:  [153.55 Melem/s 155.37 Melem/s 157.37 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
baggage/get             time:   [23.459 ns 24.609 ns 25.981 ns]
                        thrpt:  [38.490 Melem/s 40.635 Melem/s 42.627 Melem/s]
Found 21 outliers among 100 measurements (21.00%)
  1 (1.00%) low mild
  20 (20.00%) high severe
baggage/set             time:   [171.61 ns 175.72 ns 179.48 ns]
                        thrpt:  [5.5716 Melem/s 5.6908 Melem/s 5.8273 Melem/s]
Found 9 outliers among 100 measurements (9.00%)
  6 (6.00%) low mild
  1 (1.00%) high mild
  2 (2.00%) high severe
Benchmarking baggage/merge: Collecting 100 samples in estimated 5.0085 s (2.2M iterationbaggage/merge           time:   [2.0143 µs 2.0674 µs 2.1310 µs]
                        thrpt:  [469.26 Kelem/s 483.69 Kelem/s 496.45 Kelem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild

span/create             time:   [163.15 ns 169.99 ns 176.86 ns]
                        thrpt:  [5.6543 Melem/s 5.8827 Melem/s 6.1294 Melem/s]
Found 16 outliers among 100 measurements (16.00%)
  2 (2.00%) high mild
  14 (14.00%) high severe
Benchmarking span/set_attribute: Collecting 100 samples in estimated 5.0003 s (34M iteraspan/set_attribute      time:   [148.50 ns 150.93 ns 153.58 ns]
                        thrpt:  [6.5111 Melem/s 6.6255 Melem/s 6.7342 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high mild
Benchmarking span/add_event: Collecting 100 samples in estimated 5.0001 s (24M iterationspan/add_event          time:   [144.96 ns 207.08 ns 342.47 ns]
                        thrpt:  [2.9199 Melem/s 4.8291 Melem/s 6.8986 Melem/s]
Found 2 outliers among 100 measurements (2.00%)
  2 (2.00%) high severe
Benchmarking span/with_kind: Collecting 100 samples in estimated 5.0009 s (25M iterationspan/with_kind          time:   [189.98 ns 192.75 ns 195.47 ns]
                        thrpt:  [5.1159 Melem/s 5.1882 Melem/s 5.2637 Melem/s]
Found 1 outliers among 100 measurements (1.00%)
  1 (1.00%) high severe

Benchmarking context_store/create_context: Collecting 100 samples in estimated 5.6751 s context_store/create_context
                        time:   [199.20 µs 201.08 µs 202.83 µs]
                        thrpt:  [4.9303 Kelem/s 4.9731 Kelem/s 5.0202 Kelem/s]
Found 2 outliers among 100 measurements (2.00%)
  1 (1.00%) high mild
  1 (1.00%) high severe

thread 'main' (73500) panicked at benches\net.rs:4223:45:
called `Result::unwrap()` on an `Err` value: CapacityExceeded
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
