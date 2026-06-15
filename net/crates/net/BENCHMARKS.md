# Net Benchmarks

Performance benchmarks for the Net Rust core and Net transport layer.

Benchmarks accurate as of 2026-06-12.

**Test Systems:**
- Apple M1 Max, macOS
- Intel i9-14900K @5GHz, Windows 11

## Net Header Operations

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 2.1909 ns | 456.44 Melem/s | 1.2061 ns | 829.14 Melem/s |
| Deserialize | 2.3487 ns | 425.77 Melem/s | 1.6079 ns | 621.91 Melem/s |
| Roundtrip | 2.3496 ns | 425.61 Melem/s | 1.6096 ns | 621.28 Melem/s |
| AAD generation | 1.8632 ns | 536.70 Melem/s | 1.0641 ns | 939.77 Melem/s |

## Event Frame Serialization

### Single Write

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 64B | 21.494 ns | 2.7731 GiB/s | 35.345 ns | 1.6863 GiB/s |
| 256B | 46.699 ns | 5.1054 GiB/s | 35.330 ns | 6.7483 GiB/s |
| 1KB | 34.028 ns | 28.026 GiB/s | 35.310 ns | 27.008 GiB/s |
| 4KB | 76.349 ns | 49.964 GiB/s | 48.129 ns | 79.260 GiB/s |

### Batch Write (64B events)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 events | 21.556 ns | 2.7651 GiB/s | 27.175 ns | 2.1933 GiB/s |
| 10 events | 68.435 ns | 8.7097 GiB/s | 55.323 ns | 10.774 GiB/s |
| 50 events | 146.33 ns | 20.366 GiB/s | 147.62 ns | 20.189 GiB/s |
| 100 events | 272.47 ns | 21.876 GiB/s | 273.13 ns | 21.822 GiB/s |

### Batch Read

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Read batch (10 events) | 137.77 ns | 72.587 Melem/s | 163.35 ns | 61.220 Melem/s |

## Packet Pool (Zero-Allocation)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool get+return | 47.648 ns | 20.987 Melem/s | 40.935 ns | 24.429 Melem/s |
| Thread-local get+return | 104.99 ns | 9.5243 Melem/s | 60.006 ns | 16.665 Melem/s |

### Pool Comparison (10x cycles)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool 10x | 349.64 ns | 2.8601 Melem/s | 381.92 ns | 2.6183 Melem/s |
| Thread-local 10x | 1.1130 us | 898.47 Kelem/s | 802.06 ns | 1.2468 Melem/s |

## Packet Build

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 event | 299.12 ns | 204.05 MiB/s | 212.20 ns | 287.63 MiB/s |
| 10 events | 708.38 ns | 861.61 MiB/s | 436.52 ns | 1.3655 GiB/s |
| 50 events | 2.4421 us | 1.2203 GiB/s | 1.4282 us | 2.0866 GiB/s |

## Encryption (ChaCha20-Poly1305)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 64B | 301.09 ns | 202.71 MiB/s | 212.81 ns | 286.80 MiB/s |
| 256B | 476.74 ns | 512.10 MiB/s | 277.96 ns | 878.33 MiB/s |
| 1KB | 908.20 ns | 1.0501 GiB/s | 540.50 ns | 1.7644 GiB/s |
| 4KB | 2.8872 us | 1.3212 GiB/s | 1.5262 us | 2.4995 GiB/s |

### End-to-End Packet Build (50 events)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool | 2.4268 us | 1.2281 GiB/s | 1.4289 us | 2.0857 GiB/s |
| Thread-local pool | 2.4609 us | 1.2110 GiB/s | 1.4415 us | 2.0675 GiB/s |

## Adaptive Batcher Overhead

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| optimal_size() | 986.72 ps | 1.0135 Gelem/s | 804.51 ps | 1.2430 Gelem/s |
| record() | 3.8605 ns | 259.03 Melem/s | 9.4878 ns | 105.40 Melem/s |
| full_cycle | 4.3735 ns | 228.65 Melem/s | 8.0509 ns | 124.21 Melem/s |

## Key Generation

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Keypair generate | 12.456 us | 80.285 Kelem/s | 10.851 us | 92.157 Kelem/s |

## Multi-threaded Packet Build (1000 packets/thread)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 1.8532 ms | 4.3170 Melem/s | 1.2241 ms | 6.5356 Melem/s |
| 8 | Thread-local | 686.14 us | 11.659 Melem/s | 563.94 us | 14.186 Melem/s |
| 16 | Shared | 4.3103 ms | 3.7120 Melem/s | 2.5168 ms | 6.3574 Melem/s |
| 16 | Thread-local | 1.3962 ms | 11.460 Melem/s | 929.65 us | 17.211 Melem/s |
| 24 | Shared | 6.7933 ms | 3.5329 Melem/s | 4.0055 ms | 5.9918 Melem/s |
| 24 | Thread-local | 2.1242 ms | 11.298 Melem/s | 1.2289 ms | 19.529 Melem/s |
| 32 | Shared | 9.6371 ms | 3.3205 Melem/s | 5.3050 ms | 6.0321 Melem/s |
| 32 | Thread-local | 2.7898 ms | 11.470 Melem/s | 1.5174 ms | 21.088 Melem/s |

### Pool Contention (10,000 acquire/release per thread)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 17.374 ms | 4.6045 Melem/s | 10.343 ms | 7.7344 Melem/s |
| 8 | Thread-local | 1.1804 ms | 67.775 Melem/s | 1.1486 ms | 69.653 Melem/s |
| 16 | Shared | 37.810 ms | 4.2316 Melem/s | 21.660 ms | 7.3870 Melem/s |
| 16 | Thread-local | 2.2019 ms | 72.666 Melem/s | 1.7565 ms | 91.092 Melem/s |
| 24 | Shared | 61.180 ms | 3.9228 Melem/s | 38.590 ms | 6.2193 Melem/s |
| 24 | Thread-local | 3.3226 ms | 72.233 Melem/s | 2.1261 ms | 112.88 Melem/s |
| 32 | Shared | 82.277 ms | 3.8893 Melem/s | 49.627 ms | 6.4481 Melem/s |
| 32 | Thread-local | 4.5433 ms | 70.434 Melem/s | 2.6460 ms | 120.94 Melem/s |

### Mixed Frame Sizes (64B/256B/1KB rotation)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 1.0381 ms | 11.560 Melem/s | 739.21 us | 16.234 Melem/s |
| 8 | Thread-local | 635.02 us | 18.897 Melem/s | 543.79 us | 22.067 Melem/s |
| 16 | Shared | 2.4291 ms | 9.8802 Melem/s | 1.3792 ms | 17.401 Melem/s |
| 16 | Thread-local | 1.2185 ms | 19.697 Melem/s | 868.66 us | 27.629 Melem/s |
| 24 | Shared | 4.0760 ms | 8.8321 Melem/s | 2.0743 ms | 17.355 Melem/s |
| 24 | Thread-local | 1.8125 ms | 19.862 Melem/s | 1.1795 ms | 30.520 Melem/s |
| 32 | Shared | 5.6080 ms | 8.5592 Melem/s | 2.7998 ms | 17.144 Melem/s |
| 32 | Thread-local | 2.3118 ms | 20.763 Melem/s | 1.4859 ms | 32.303 Melem/s |

### Throughput Scaling (Thread-local Pool)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 threads | 2.4787 ms | 806.86 Kelem/s | 1.4312 ms | 1.3974 Melem/s |
| 2 threads | 2.5825 ms | 1.5489 Melem/s | 1.5071 ms | 2.6541 Melem/s |
| 4 threads | 2.7908 ms | 2.8666 Melem/s | 1.5458 ms | 5.1752 Melem/s |
| 8 threads | 3.0433 ms | 5.2574 Melem/s | 2.0913 ms | 7.6507 Melem/s |
| 16 threads | 5.9857 ms | 5.3461 Melem/s | 3.3343 ms | 9.5973 Melem/s |
| 24 threads | 8.7039 ms | 5.5148 Melem/s | 3.9903 ms | 12.029 Melem/s |
| 32 threads | 12.350 ms | 5.1820 Melem/s | 5.1411 ms | 12.449 Melem/s |

## Routing

### Routing Header

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 624.31 ps | 1.6018 Gelem/s | 508.32 ps | 1.9673 Gelem/s |
| Deserialize | 931.90 ps | 1.0731 Gelem/s | 720.41 ps | 1.3881 Gelem/s |
| Roundtrip | 931.81 ps | 1.0732 Gelem/s | 735.15 ps | 1.3603 Gelem/s |
| Forward | 571.42 ps | 1.7500 Gelem/s | 201.75 ps | 4.9566 Gelem/s |

### Routing Table

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| lookup_hit | 37.730 ns | 26.504 Melem/s | 38.135 ns | 26.223 Melem/s |
| lookup_miss | 15.219 ns | 65.708 Melem/s | 17.706 ns | 56.477 Melem/s |
| is_local | 313.39 ps | 3.1909 Gelem/s | 201.29 ps | 4.9681 Gelem/s |
| add_route | 46.487 ns | 21.511 Melem/s | 37.349 ns | 26.774 Melem/s |
| record_in | 49.694 ns | 20.123 Melem/s | 40.648 ns | 24.602 Melem/s |
| record_out | 22.084 ns | 45.282 Melem/s | 21.174 ns | 47.228 Melem/s |
| aggregate_stats | 2.1356 us | 468.25 Kelem/s | 6.1092 us | 163.69 Kelem/s |

### Concurrent Routing Lookup

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Lookup | 158.46 us | 25.243 Melem/s | 181.94 us | 21.985 Melem/s |
| 4 | Stats | 291.31 us | 13.731 Melem/s | 226.05 us | 17.695 Melem/s |
| 8 | Lookup | 249.46 us | 32.069 Melem/s | 287.24 us | 27.851 Melem/s |
| 8 | Stats | 406.38 us | 19.686 Melem/s | 328.02 us | 24.389 Melem/s |
| 16 | Lookup | 428.00 us | 37.383 Melem/s | 522.62 us | 30.615 Melem/s |
| 16 | Stats | 799.27 us | 20.018 Melem/s | 582.35 us | 27.475 Melem/s |

### Decision Pipeline

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| parse + lookup + forward | 37.481 ns | 26.680 Melem/s | 38.471 ns | 25.993 Melem/s |
| full with stats | 106.24 ns | 9.4129 Melem/s | 99.818 ns | 10.018 Melem/s |

## Multi-hop Forwarding

### Packet Builder

| Payload | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 64B | Build | 25.934 ns | 2.2983 GiB/s | 41.097 ns | 1.4503 GiB/s |
| 64B | Build (priority) | 24.004 ns | 2.4831 GiB/s | 29.613 ns | 2.0128 GiB/s |
| 256B | Build | 51.517 ns | 4.6280 GiB/s | 44.460 ns | 5.3626 GiB/s |
| 256B | Build (priority) | 47.969 ns | 4.9703 GiB/s | 32.347 ns | 7.3705 GiB/s |
| 1KB | Build | 39.581 ns | 24.094 GiB/s | 44.189 ns | 21.582 GiB/s |
| 1KB | Build (priority) | 36.372 ns | 26.220 GiB/s | 35.378 ns | 26.957 GiB/s |
| 4KB | Build | 77.966 ns | 48.928 GiB/s | 64.470 ns | 59.170 GiB/s |
| 4KB | Build (priority) | 75.934 ns | 50.237 GiB/s | 53.732 ns | 70.995 GiB/s |

### Chain Scaling (forward_chain)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 hops | 61.656 ns | 16.219 Melem/s | 53.417 ns | 18.721 Melem/s |
| 2 hops | 116.46 ns | 8.5868 Melem/s | 88.642 ns | 11.281 Melem/s |
| 3 hops | 160.03 ns | 6.2489 Melem/s | 121.08 ns | 8.2588 Melem/s |
| 4 hops | 217.27 ns | 4.6026 Melem/s | 156.17 ns | 6.4033 Melem/s |
| 5 hops | 271.11 ns | 3.6886 Melem/s | 189.54 ns | 5.2758 Melem/s |

### Hop Latency

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Single hop process | 1.4653 ns | 682.45 Melem/s | 966.73 ps | 1.0344 Gelem/s |
| Single hop full | 58.018 ns | 17.236 Melem/s | 34.141 ns | 29.291 Melem/s |

### Hop Scaling by Payload Size

| Payload | Hops | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 64B | 1 hops | 31.758 ns | 1.8768 GiB/s | 53.762 ns | 1.1087 GiB/s |
| 64B | 2 hops | 84.664 ns | 720.91 MiB/s | 84.985 ns | 718.19 MiB/s |
| 64B | 3 hops | 111.24 ns | 548.67 MiB/s | 116.24 ns | 525.07 MiB/s |
| 64B | 4 hops | 140.62 ns | 434.03 MiB/s | 148.63 ns | 410.66 MiB/s |
| 64B | 5 hops | 167.47 ns | 364.46 MiB/s | 184.47 ns | 330.86 MiB/s |
| 256B | 1 hops | 58.446 ns | 4.0793 GiB/s | 53.698 ns | 4.4400 GiB/s |
| 256B | 2 hops | 113.84 ns | 2.0943 GiB/s | 87.617 ns | 2.7212 GiB/s |
| 256B | 3 hops | 159.04 ns | 1.4991 GiB/s | 123.25 ns | 1.9344 GiB/s |
| 256B | 4 hops | 214.88 ns | 1.1095 GiB/s | 154.66 ns | 1.5415 GiB/s |
| 256B | 5 hops | 270.25 ns | 903.39 MiB/s | 190.90 ns | 1.2489 GiB/s |
| 1024B | 1 hops | 44.878 ns | 21.251 GiB/s | 54.721 ns | 17.428 GiB/s |
| 1024B | 2 hops | 107.25 ns | 8.8923 GiB/s | 90.824 ns | 10.500 GiB/s |
| 1024B | 3 hops | 150.63 ns | 6.3314 GiB/s | 125.82 ns | 7.5796 GiB/s |
| 1024B | 4 hops | 200.62 ns | 4.7536 GiB/s | 160.85 ns | 5.9288 GiB/s |
| 1024B | 5 hops | 233.58 ns | 4.0828 GiB/s | 203.21 ns | 4.6930 GiB/s |

### Route and Forward (with routing table lookup)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 hops | 157.11 ns | 6.3648 Melem/s | 154.47 ns | 6.4738 Melem/s |
| 2 hops | 310.08 ns | 3.2250 Melem/s | 290.76 ns | 3.4393 Melem/s |
| 3 hops | 462.31 ns | 2.1631 Melem/s | 424.95 ns | 2.3532 Melem/s |
| 4 hops | 620.72 ns | 1.6110 Melem/s | 558.01 ns | 1.7921 Melem/s |
| 5 hops | 775.19 ns | 1.2900 Melem/s | 694.54 ns | 1.4398 Melem/s |

### Concurrent Forwarding

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Forward | 709.80 us | 5.6354 Melem/s | 580.86 us | 6.8864 Melem/s |
| 8 | Forward | 1.1528 ms | 6.9396 Melem/s | 806.99 us | 9.9134 Melem/s |
| 16 | Forward | 1.7385 ms | 9.2036 Melem/s | 1.5263 ms | 10.483 Melem/s |

## Swarm / Discovery

### Pingwave

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 781.32 ps | 1.2799 Gelem/s | 518.93 ps | 1.9270 Gelem/s |
| Deserialize | 931.39 ps | 1.0737 Gelem/s | 636.21 ps | 1.5718 Gelem/s |
| Roundtrip | 931.83 ps | 1.0732 Gelem/s | 637.45 ps | 1.5687 Gelem/s |
| Forward | 625.27 ps | 1.5993 Gelem/s | 518.69 ps | 1.9279 Gelem/s |

### Local Graph

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create_pingwave | 2.1014 ns | 475.88 Melem/s | 5.0430 ns | 198.29 Melem/s |
| on_pingwave_new | 39.492 ns | 25.321 Melem/s | 37.916 ns | 26.374 Melem/s |
| on_pingwave_duplicate | 22.815 ns | 43.831 Melem/s | 17.086 ns | 58.527 Melem/s |
| get_node | 15.127 ns | 66.107 Melem/s | 21.305 ns | 46.936 Melem/s |
| node_count | 319.63 ps | 3.1286 Gelem/s | 278.17 ps | 3.5950 Gelem/s |
| stats | 388.18 ps | 2.5762 Gelem/s | 548.43 ps | 1.8234 Gelem/s |

### Graph Scaling

| Nodes | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 100 | all_nodes | 2.8506 us | 35.081 Melem/s | 13.270 us | 7.5356 Melem/s |
| 100 | nodes_within_hops | 3.1941 us | 31.308 Melem/s | 13.311 us | 7.5126 Melem/s |
| 500 | all_nodes | 8.3259 us | 60.054 Melem/s | 26.063 us | 19.184 Melem/s |
| 500 | nodes_within_hops | 9.7512 us | 51.276 Melem/s | 25.939 us | 19.276 Melem/s |
| 1,000 | all_nodes | 37.378 us | 26.754 Melem/s | 43.105 us | 23.199 Melem/s |
| 1,000 | nodes_within_hops | 64.030 us | 15.618 Melem/s | 41.811 us | 23.917 Melem/s |
| 5,000 | all_nodes | 113.37 us | 44.102 Melem/s | 316.56 us | 15.795 Melem/s |
| 5,000 | nodes_within_hops | 131.58 us | 37.999 Melem/s | 311.17 us | 16.068 Melem/s |

### Path Finding

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| path_1_hop | 2.3039 us | 434.05 Kelem/s | 10.410 us | 96.058 Kelem/s |
| path_2_hops | 2.3335 us | 428.54 Kelem/s | 10.738 us | 93.125 Kelem/s |
| path_4_hops | 2.6945 us | 371.12 Kelem/s | 11.133 us | 89.826 Kelem/s |
| path_not_found | 2.4460 us | 408.83 Kelem/s | 10.879 us | 91.918 Kelem/s |
| path_complex_graph | 260.29 us | 3.8418 Kelem/s | 335.84 us | 2.9776 Kelem/s |

### Concurrent Pingwave Processing

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Pingwave | 112.78 us | 17.733 Melem/s | 311.25 us | 6.4257 Melem/s |
| 8 | Pingwave | 185.60 us | 21.552 Melem/s | 474.19 us | 8.4355 Melem/s |
| 16 | Pingwave | 332.44 us | 24.064 Melem/s | 685.28 us | 11.674 Melem/s |

## Failure Detection

### Failure Detector

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| heartbeat_existing | 39.762 ns | 25.150 Melem/s | 69.269 ns | 14.436 Melem/s |
| heartbeat_new | 237.77 ns | 4.2058 Melem/s | 243.74 ns | 4.1027 Melem/s |
| status_check | 15.102 ns | 66.218 Melem/s | 15.174 ns | 65.903 Melem/s |
| check_all | 12.198 us | 81.978 Kelem/s | 28.012 us | 35.699 Kelem/s |
| stats | 10.749 us | 93.029 Kelem/s | 21.162 us | 47.254 Kelem/s |

> **Note:** `check_all` and `stats` are O(nodes) maintenance scans, not
> per-packet hot-path costs. `check_all` runs once per `heartbeat_interval`
> (default 5 s) and `stats` is observability-only by design. *Failure Scaling*
> below shows `check_all` growing linearly to ~55 us at 5,000 nodes
> (~11 us/s amortized). The genuine per-event costs are `heartbeat_existing` /
> `heartbeat_new` / `status_check` (15–242 ns).

### Circuit Breaker

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| allow_closed | 9.5519 ns | 104.69 Melem/s | 11.113 ns | 89.984 Melem/s |
| record_success | 8.3906 ns | 119.18 Melem/s | 11.360 ns | 88.028 Melem/s |
| record_failure | 7.4171 ns | 134.82 Melem/s | 11.037 ns | 90.601 Melem/s |
| state | 9.5163 ns | 105.08 Melem/s | 12.002 ns | 83.321 Melem/s |

### Recovery Manager

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| on_failure_with_alternates | 257.51 ns | 3.8833 Melem/s | 354.87 ns | 2.8180 Melem/s |
| on_failure_no_alternates | 178.22 ns | 5.6111 Melem/s | 245.16 ns | 4.0790 Melem/s |
| get_action | 37.147 ns | 26.920 Melem/s | 80.773 ns | 12.380 Melem/s |
| is_failed | 13.954 ns | 71.664 Melem/s | 24.647 ns | 40.572 Melem/s |
| on_recovery | 99.187 ns | 10.082 Melem/s | 236.60 ns | 4.2265 Melem/s |
| stats | 699.22 ps | 1.4302 Gelem/s | 1.7008 ns | 587.95 Melem/s |

### Full Recovery Cycle

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Fail + recover cycle | 290.75 ns | 3.4394 Melem/s | 279.63 ns | 3.5761 Melem/s |

### Loss Simulator

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1% | 2.7864 ns | 358.89 Melem/s | 16.562 ns | 60.377 Melem/s |
| 5% | 3.1451 ns | 317.96 Melem/s | 16.981 ns | 58.891 Melem/s |
| 10% | 3.6118 ns | 276.87 Melem/s | 17.517 ns | 57.088 Melem/s |
| 20% | 4.5659 ns | 219.01 Melem/s | 18.658 ns | 53.596 Melem/s |
| burst | 2.9188 ns | 342.60 Melem/s | 17.139 ns | 58.346 Melem/s |

### Failure Scaling (check_all)

| Nodes | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 100 | check_all | 2.4792 us | 40.336 Melem/s | 12.272 us | 8.1487 Melem/s |
| 100 | healthy_nodes | 2.1508 us | 46.494 Melem/s | 11.434 us | 8.7455 Melem/s |
| 500 | check_all | 6.5892 us | 75.882 Melem/s | 20.516 us | 24.371 Melem/s |
| 500 | healthy_nodes | 6.1351 us | 81.498 Melem/s | 15.234 us | 32.822 Melem/s |
| 1,000 | check_all | 12.105 us | 82.610 Melem/s | 30.501 us | 32.786 Melem/s |
| 1,000 | healthy_nodes | 10.616 us | 94.197 Melem/s | 22.350 us | 44.743 Melem/s |
| 5,000 | check_all | 54.931 us | 91.023 Melem/s | 87.514 us | 57.133 Melem/s |
| 5,000 | healthy_nodes | 50.910 us | 98.212 Melem/s | 51.754 us | 96.611 Melem/s |

### Concurrent Heartbeats

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Heartbeat | 197.85 us | 10.109 Melem/s | 215.76 us | 9.2694 Melem/s |
| 8 | Heartbeat | 259.48 us | 15.416 Melem/s | 340.99 us | 11.730 Melem/s |
| 16 | Heartbeat | 486.95 us | 16.429 Melem/s | 604.87 us | 13.226 Melem/s |

## Stream Multiplexing

| Streams | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 10 | Lookup | 292.48 ns | 34.191 Melem/s | 337.21 ns | 29.655 Melem/s |
| 10 | Stats | 465.58 ns | 21.479 Melem/s | 396.54 ns | 25.218 Melem/s |
| 100 | Lookup | 2.9158 us | 34.296 Melem/s | 3.3989 us | 29.421 Melem/s |
| 100 | Stats | 4.5977 us | 21.750 Melem/s | 3.9472 us | 25.334 Melem/s |
| 1,000 | Lookup | 29.094 us | 34.371 Melem/s | 35.773 us | 27.954 Melem/s |
| 1,000 | Stats | 46.384 us | 21.559 Melem/s | 42.918 us | 23.300 Melem/s |
| 10,000 | Lookup | 291.12 us | 34.350 Melem/s | 386.08 us | 25.901 Melem/s |
| 10,000 | Stats | 508.24 us | 19.676 Melem/s | 455.29 us | 21.964 Melem/s |

## Fair Scheduler

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Creation | 391.85 ns | 2.5520 Melem/s | 1.7416 us | 574.19 Kelem/s |
| Stream count (empty) | 199.39 ns | 5.0152 Melem/s | 960.03 ns | 1.0416 Melem/s |
| Total queued | 311.25 ps | 3.2128 Gelem/s | 234.84 ps | 4.2582 Gelem/s |
| Cleanup (empty) | 200.45 ns | 4.9888 Melem/s | 1.2938 us | 772.95 Kelem/s |

## Capability System


Capability state is folded: announcements collapse into one queryable index that scales from a 10K-node mesh to ~2M nodes. The single-element checks below are matches against a fully populated set.

### CapabilitySet

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create | 19.025 us | 52.563 Kelem/s | 13.980 us | 71.529 Kelem/s |
| serialize | 10.787 us | 92.703 Kelem/s | 33.480 us | 29.868 Kelem/s |
| deserialize | 10.078 us | 99.225 Kelem/s | 8.0414 us | 124.36 Kelem/s |
| roundtrip | 21.072 us | 47.456 Kelem/s | 19.062 us | 52.460 Kelem/s |
| has_tag | 46.747 ns | 21.392 Melem/s | 67.864 ns | 14.735 Melem/s |
| has_model | 37.260 ns | 26.839 Melem/s | 24.073 ns | 41.541 Melem/s |
| has_tool | 63.146 ns | 15.836 Melem/s | 35.405 ns | 28.244 Melem/s |
| has_gpu | 40.519 ns | 24.680 Melem/s | 40.587 ns | 24.639 Melem/s |

### Capability Announcement

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create | 3.4149 us | 292.83 Kelem/s | 2.7479 us | 363.92 Kelem/s |
| serialize | 11.132 us | 89.831 Kelem/s | 11.495 us | 86.993 Kelem/s |
| deserialize | 10.354 us | 96.583 Kelem/s | 9.6005 us | 104.16 Kelem/s |
| is_expired | 25.175 ns | 39.722 Melem/s | 21.176 ns | 47.224 Melem/s |

### Capability Serialization (Simple vs Complex)

| Type | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| Simple | Serialize | 20.728 ns | 48.245 Melem/s | 37.083 ns | 26.967 Melem/s |
| Simple | Deserialize | 5.5787 ns | 179.25 Melem/s | 14.790 ns | 67.615 Melem/s |
| Complex | Serialize | 43.985 ns | 22.735 Melem/s | 39.098 ns | 25.577 Melem/s |
| Complex | Deserialize | 378.30 ns | 2.6434 Melem/s | 238.23 ns | 4.1977 Melem/s |

### Capability Filter

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| single_tag | 57.136 ns | 17.502 Melem/s | 59.724 ns | 16.744 Melem/s |
| require_gpu | 46.726 ns | 21.402 Melem/s | 42.801 ns | 23.364 Melem/s |
| gpu_vendor | 152.37 ns | 6.5628 Melem/s | 127.19 ns | 7.8622 Melem/s |
| min_memory | 31.373 ns | 31.874 Melem/s | 23.804 ns | 42.010 Melem/s |
| complex | 4.7046 us | 212.56 Kelem/s | 3.9229 us | 254.91 Kelem/s |
| no_match | 83.412 ns | 11.989 Melem/s | 76.976 ns | 12.991 Melem/s |

### Capability Fold Index (Insert)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 100 nodes | 4.0300 ms | 24.814 Kelem/s | 3.1872 ms | 31.376 Kelem/s |
| 1,000 nodes | 40.637 ms | 24.608 Kelem/s | 30.298 ms | 33.006 Kelem/s |
| 10,000 nodes | 423.46 ms | 23.615 Kelem/s | 307.09 ms | 32.564 Kelem/s |

### Capability Fold Index (Query, 10,000 nodes)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| single_tag | 109.69 us | 9.1163 Kelem/s | 91.039 us | 10.984 Kelem/s |
| require_gpu | 299.35 us | 3.3406 Kelem/s | 232.41 us | 4.3028 Kelem/s |
| gpu_vendor | 438.66 us | 2.2797 Kelem/s | 305.96 us | 3.2684 Kelem/s |
| min_memory | 345.90 us | 2.8910 Kelem/s | 278.98 us | 3.5845 Kelem/s |
| complex | 325.36 us | 3.0735 Kelem/s | 182.19 us | 5.4888 Kelem/s |
| model | 71.391 us | 14.007 Kelem/s | 58.464 us | 17.105 Kelem/s |
| tool | 270.06 us | 3.7028 Kelem/s | 231.50 us | 4.3196 Kelem/s |
| no_results | 81.762 ns | 12.231 Melem/s | 86.584 ns | 11.549 Melem/s |

### Capability Fold Index (Find Best)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Simple | 272.81 us | 3.6656 Kelem/s | 232.59 us | 4.2995 Kelem/s |
| With preferences | 456.30 us | 2.1916 Kelem/s | 168.32 us | 5.9412 Kelem/s |

### Capability Search (1,000 nodes)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| find_with_gpu | 27.648 us | 36.169 Kelem/s | 49.928 us | 20.029 Kelem/s |
| find_by_tool (Python) | 59.701 us | 16.750 Kelem/s | 100.86 us | 9.9150 Kelem/s |
| find_by_tool (Rust) | 78.087 us | 12.806 Kelem/s | 127.51 us | 7.8428 Kelem/s |

### Capability Fold Scaling

| Nodes | Query Type | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 1,000 | Tag (broad) | 9.4969 us | 52.649 Melem/s | 7.9688 us | 62.744 Melem/s |
| 1,000 | Tag (selective) | 1.8962 us | 52.738 Melem/s | 1.5241 us | 65.614 Melem/s |
| 1,000 | Complex | 18.414 us | 24.546 Melem/s | 16.940 us | 26.683 Melem/s |
| 5,000 | Tag (broad) | 52.894 us | 47.265 Melem/s | 80.688 us | 30.984 Melem/s |
| 5,000 | Tag (selective) | 1.8909 us | 52.884 Melem/s | 2.8429 us | 35.175 Melem/s |
| 5,000 | Complex | 202.02 us | 11.202 Melem/s | 167.49 us | 13.511 Melem/s |
| 10,000 | Tag (broad) | 108.44 us | 46.108 Melem/s | 171.89 us | 29.089 Melem/s |
| 10,000 | Tag (selective) | 1.9007 us | 52.612 Melem/s | 2.8508 us | 35.078 Melem/s |
| 10,000 | Complex | 367.38 us | 12.328 Melem/s | 342.91 us | 13.208 Melem/s |
| 50,000 | Tag (broad) | 643.95 us | 38.823 Melem/s | 1.2093 ms | 20.672 Melem/s |
| 50,000 | Tag (selective) | 1.9104 us | 52.345 Melem/s | 2.9360 us | 34.060 Melem/s |
| 50,000 | Complex | 1.6939 ms | 13.374 Melem/s | 2.7328 ms | 8.2897 Melem/s |

> A *selective* tag query stays flat (~2 us) regardless of mesh size, while a *broad* tag scans its match set — the property that lets one fold span millions of nodes.


### Concurrent Capability Index

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Insert | 15.573 ms | 128.43 Kelem/s | 25.569 ms | 78.218 Kelem/s |
| 4 | Query | 161.43 ms | 12.389 Kelem/s | 209.95 ms | 9.5263 Kelem/s |
| 8 | Insert | 16.313 ms | 245.20 Kelem/s | 23.200 ms | 172.41 Kelem/s |
| 8 | Query | 171.05 ms | 23.386 Kelem/s | 226.73 ms | 17.642 Kelem/s |
| 16 | Insert | 34.600 ms | 231.21 Kelem/s | 49.383 ms | 162.00 Kelem/s |
| 16 | Query | 272.98 ms | 29.306 Kelem/s | 515.41 ms | 15.522 Kelem/s |

## Running Benchmarks

```bash
cargo bench --features net --bench net
```

For native CPU optimizations:

```bash
RUSTFLAGS="-C target-cpu=native" cargo bench --features net --bench net
```

To re-parse raw criterion output into structured markdown, use the helper script in `benchmarks/`:

```bash
./benchmarks/parse_criterion.py BENCHMARK_RESULTS_M1_MAX_2.md /tmp/m1_max_parsed.md
```

## Key Insights

1. **Header serialize/deserialize runs at ~620–830M ops/sec** (i9) / ~425–456M ops/sec (M1) — sub-2.5 ns per operation; AAD generation cleared ~940M ops/sec on i9.
2. **Routing header forward at ~5G ops/sec on i9** (~200 ps) — routing header serialize/roundtrip 1.07–1.97G ops/sec across both platforms.
3. **Thread-local pool eliminates contention** — ~18x faster than shared pool at 32 threads (M1: 70.4M vs 3.89M ops/sec; i9: 120.9M vs 6.45M ops/sec).
4. **Encryption got faster** — the upgraded ChaCha20-Poly1305 path does 64B in ~301 ns on M1 / ~213 ns on i9, and 4KB at 1.32 GiB/s (M1) / 2.50 GiB/s (i9).
5. **Circuit breaker checks are ~10 ns** — negligible overhead per packet.
6. **Event frame write scales with payload** — ~1.7–2.8 GiB/s at 64B (single write), ~50 GiB/s on M1 / ~79 GiB/s on i9 at 4KB.
7. **Multi-hop forwarding adds ~50 ns per hop** — linear scaling, no amplification.
8. **Capability state is folded** — announcements collapse into a queryable index sized for large meshes; a selective tag query stays flat at ~2 us regardless of mesh size. Per-node fold insert is ~40 us (M1).
