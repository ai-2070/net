# Net Benchmarks

Performance benchmarks for the Net Rust core and Net transport layer.

Benchmarks accurate as of 2026-04-27.

**Test Systems:**
- Apple M1 Max, macOS
- Intel i9-14900K @5GHz, Windows 11

## Net Header Operations

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 1.9795 ns | 505.17 Melem/s | 1.3121 ns | 762.11 Melem/s |
| Deserialize | 2.1051 ns | 475.04 Melem/s | 1.2066 ns | 828.74 Melem/s |
| Roundtrip | 2.1236 ns | 470.89 Melem/s | 1.2073 ns | 828.27 Melem/s |
| AAD generation | 2.0290 ns | 492.84 Melem/s | 1.0993 ns | 909.65 Melem/s |

## Event Frame Serialization

### Single Write

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 64B | 18.313 ns | 3.2547 GiB/s | 35.636 ns | 1.6726 GiB/s |
| 256B | 45.154 ns | 5.2802 GiB/s | 35.975 ns | 6.6273 GiB/s |
| 1KB | 35.994 ns | 26.495 GiB/s | 36.711 ns | 25.978 GiB/s |
| 4KB | 77.377 ns | 49.300 GiB/s | 51.033 ns | 74.749 GiB/s |

### Batch Write (64B events)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 events | 18.358 ns | 3.2468 GiB/s | 26.697 ns | 2.2326 GiB/s |
| 10 events | 70.986 ns | 8.3967 GiB/s | 58.763 ns | 10.143 GiB/s |
| 50 events | 147.66 ns | 20.184 GiB/s | 146.16 ns | 20.390 GiB/s |
| 100 events | 273.70 ns | 21.777 GiB/s | 259.22 ns | 22.994 GiB/s |

### Batch Read

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Read batch (10 events) | 140.85 ns | 70.998 Melem/s | 164.31 ns | 60.861 Melem/s |

## Packet Pool (Zero-Allocation)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool get+return | 38.626 ns | 25.889 Melem/s | 52.956 ns | 18.884 Melem/s |
| Thread-local get+return | 82.768 ns | 12.082 Melem/s | 65.543 ns | 15.257 Melem/s |

### Pool Comparison (10x cycles)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool 10x | 340.51 ns | 2.9368 Melem/s | 526.09 ns | 1.9008 Melem/s |
| Thread-local 10x | 959.35 ns | 1.0424 Melem/s | 819.73 ns | 1.2199 Melem/s |

## Packet Build

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 event | 485.51 ns | 125.71 MiB/s | 1.1361 us | 53.724 MiB/s |
| 10 events | 1.8459 us | 330.66 MiB/s | 1.5017 us | 406.44 MiB/s |
| 50 events | 8.2080 us | 371.80 MiB/s | 2.9299 us | 1.0172 GiB/s |

## Encryption (ChaCha20-Poly1305)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 64B | 483.14 ns | 126.33 MiB/s | 1.1356 us | 53.749 MiB/s |
| 256B | 922.69 ns | 264.60 MiB/s | 1.2027 us | 203.00 MiB/s |
| 1KB | 2.6917 us | 362.80 MiB/s | 1.5793 us | 618.35 MiB/s |
| 4KB | 9.7427 us | 400.94 MiB/s | 3.0992 us | 1.2309 GiB/s |

### End-to-End Packet Build (50 events)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Shared pool | 8.1780 us | 373.17 MiB/s | 2.8706 us | 1.0382 GiB/s |
| Thread-local pool | 8.1857 us | 372.82 MiB/s | 2.8355 us | 1.0510 GiB/s |

## Adaptive Batcher Overhead

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| optimal_size() | 972.95 ps | 1.0278 Gelem/s | 788.45 ps | 1.2683 Gelem/s |
| record() | 3.8946 ns | 256.77 Melem/s | 14.312 ns | 69.870 Melem/s |
| full_cycle | 4.3907 ns | 227.75 Melem/s | 7.9251 ns | 126.18 Melem/s |

## Key Generation

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Keypair generate | 12.426 us | 80.477 Kelem/s | 10.661 us | 93.798 Kelem/s |

## Multi-threaded Packet Build (1000 packets/thread)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 1.8232 ms | 4.3879 Melem/s | 1.6552 ms | 4.8333 Melem/s |
| 8 | Thread-local | 871.34 us | 9.1812 Melem/s | 1.4680 ms | 5.4497 Melem/s |
| 16 | Shared | 4.3219 ms | 3.7021 Melem/s | 2.6261 ms | 6.0926 Melem/s |
| 16 | Thread-local | 1.6980 ms | 9.4230 Melem/s | 1.8989 ms | 8.4260 Melem/s |
| 24 | Shared | 6.5734 ms | 3.6511 Melem/s | 3.7907 ms | 6.3314 Melem/s |
| 24 | Thread-local | 2.4907 ms | 9.6358 Melem/s | 2.7032 ms | 8.8783 Melem/s |
| 32 | Shared | 8.6432 ms | 3.7023 Melem/s | 5.5519 ms | 5.7638 Melem/s |
| 32 | Thread-local | 3.2641 ms | 9.8037 Melem/s | 3.2369 ms | 9.8861 Melem/s |

### Pool Contention (10,000 acquire/release per thread)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 17.757 ms | 4.5052 Melem/s | 9.5961 ms | 8.3367 Melem/s |
| 8 | Thread-local | 1.1039 ms | 72.473 Melem/s | 1.2653 ms | 63.228 Melem/s |
| 16 | Shared | 42.666 ms | 3.7501 Melem/s | 21.895 ms | 7.3077 Melem/s |
| 16 | Thread-local | 2.2715 ms | 70.439 Melem/s | 1.8178 ms | 88.018 Melem/s |
| 24 | Shared | 62.141 ms | 3.8622 Melem/s | 35.778 ms | 6.7079 Melem/s |
| 24 | Thread-local | 3.2728 ms | 73.332 Melem/s | 2.2103 ms | 108.58 Melem/s |
| 32 | Shared | 88.574 ms | 3.6128 Melem/s | 46.426 ms | 6.8927 Melem/s |
| 32 | Thread-local | 4.1638 ms | 76.853 Melem/s | 2.8982 ms | 110.41 Melem/s |

### Mixed Frame Sizes (64B/256B/1KB rotation)

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 8 | Shared | 1.3890 ms | 8.6394 Melem/s | 1.1449 ms | 10.481 Melem/s |
| 8 | Thread-local | 1.0463 ms | 11.469 Melem/s | 1.0572 ms | 11.351 Melem/s |
| 16 | Shared | 3.1078 ms | 7.7225 Melem/s | 1.6026 ms | 14.976 Melem/s |
| 16 | Thread-local | 2.0450 ms | 11.736 Melem/s | 1.3995 ms | 17.149 Melem/s |
| 24 | Shared | 4.7286 ms | 7.6132 Melem/s | 2.1766 ms | 16.539 Melem/s |
| 24 | Thread-local | 2.9936 ms | 12.026 Melem/s | 1.7878 ms | 20.136 Melem/s |
| 32 | Shared | 6.2144 ms | 7.7240 Melem/s | 3.2686 ms | 14.685 Melem/s |
| 32 | Thread-local | 3.9297 ms | 12.215 Melem/s | 2.2510 ms | 21.324 Melem/s |

### Throughput Scaling (Thread-local Pool)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 threads | 6.7210 ms | 297.58 Kelem/s | 3.6389 ms | 549.62 Kelem/s |
| 2 threads | 6.9737 ms | 573.58 Kelem/s | 3.7152 ms | 1.0767 Melem/s |
| 4 threads | 7.4390 ms | 1.0754 Melem/s | 3.7323 ms | 2.1434 Melem/s |
| 8 threads | 7.8256 ms | 2.0446 Melem/s | 4.6415 ms | 3.4472 Melem/s |
| 16 threads | 15.423 ms | 2.0748 Melem/s | 5.4369 ms | 5.8857 Melem/s |
| 24 threads | 22.826 ms | 2.1028 Melem/s | 7.9115 ms | 6.0671 Melem/s |
| 32 threads | 29.946 ms | 2.1372 Melem/s | 10.167 ms | 6.2949 Melem/s |

## Routing

### Routing Header

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 627.06 ps | 1.5947 Gelem/s | 458.66 ps | 2.1803 Gelem/s |
| Deserialize | 936.61 ps | 1.0677 Gelem/s | 720.57 ps | 1.3878 Gelem/s |
| Roundtrip | 936.94 ps | 1.0673 Gelem/s | 711.16 ps | 1.4061 Gelem/s |
| Forward | 572.90 ps | 1.7455 Gelem/s | 197.75 ps | 5.0569 Gelem/s |

### Routing Table

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| lookup_hit | 38.087 ns | 26.256 Melem/s | 37.520 ns | 26.653 Melem/s |
| lookup_miss | 15.194 ns | 65.817 Melem/s | 17.417 ns | 57.416 Melem/s |
| is_local | 314.60 ps | 3.1786 Gelem/s | 200.43 ps | 4.9892 Gelem/s |
| add_route | 227.91 ns | 4.3877 Melem/s | 185.53 ns | 5.3899 Melem/s |
| record_in | 49.706 ns | 20.118 Melem/s | 40.591 ns | 24.636 Melem/s |
| record_out | 20.988 ns | 47.647 Melem/s | 21.100 ns | 47.392 Melem/s |
| aggregate_stats | 2.1182 us | 472.10 Kelem/s | 8.0137 us | 124.79 Kelem/s |

### Concurrent Routing Lookup

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Lookup | 150.82 us | 26.522 Melem/s | 179.22 us | 22.319 Melem/s |
| 4 | Stats | 285.91 us | 13.990 Melem/s | 227.20 us | 17.605 Melem/s |
| 8 | Lookup | 247.27 us | 32.354 Melem/s | 286.48 us | 27.926 Melem/s |
| 8 | Stats | 402.88 us | 19.857 Melem/s | 330.19 us | 24.229 Melem/s |
| 16 | Lookup | 427.39 us | 37.437 Melem/s | 520.51 us | 30.739 Melem/s |
| 16 | Stats | 797.54 us | 20.062 Melem/s | 578.11 us | 27.676 Melem/s |

### Decision Pipeline

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| parse + lookup + forward | 38.888 ns | 25.715 Melem/s | 38.618 ns | 25.894 Melem/s |
| full with stats | 109.96 ns | 9.0945 Melem/s | 100.88 ns | 9.9128 Melem/s |

## Multi-hop Forwarding

### Packet Builder

| Payload | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 64B | Build | 23.215 ns | 2.5675 GiB/s | 40.723 ns | 1.4637 GiB/s |
| 64B | Build (priority) | 20.647 ns | 2.8868 GiB/s | 29.625 ns | 2.0120 GiB/s |
| 256B | Build | 51.113 ns | 4.6646 GiB/s | 43.461 ns | 5.4858 GiB/s |
| 256B | Build (priority) | 50.016 ns | 4.7669 GiB/s | 31.959 ns | 7.4601 GiB/s |
| 1KB | Build | 41.145 ns | 23.178 GiB/s | 43.981 ns | 21.684 GiB/s |
| 1KB | Build (priority) | 38.402 ns | 24.834 GiB/s | 35.206 ns | 27.088 GiB/s |
| 4KB | Build | 82.055 ns | 46.489 GiB/s | 62.592 ns | 60.946 GiB/s |
| 4KB | Build (priority) | 80.475 ns | 47.402 GiB/s | 52.824 ns | 72.215 GiB/s |

### Chain Scaling (forward_chain)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 hops | 59.066 ns | 16.930 Melem/s | 53.369 ns | 18.737 Melem/s |
| 2 hops | 117.32 ns | 8.5239 Melem/s | 86.868 ns | 11.512 Melem/s |
| 3 hops | 163.16 ns | 6.1291 Melem/s | 120.66 ns | 8.2878 Melem/s |
| 4 hops | 216.95 ns | 4.6094 Melem/s | 154.83 ns | 6.4585 Melem/s |
| 5 hops | 273.51 ns | 3.6561 Melem/s | 189.73 ns | 5.2706 Melem/s |

### Hop Latency

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Single hop process | 1.4534 ns | 688.06 Melem/s | 956.42 ps | 1.0456 Gelem/s |
| Single hop full | 57.810 ns | 17.298 Melem/s | 33.033 ns | 30.273 Melem/s |

### Hop Scaling by Payload Size

| Payload | Hops | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 64B | 1 hops | 29.697 ns | 2.0071 GiB/s | 51.991 ns | 1.1464 GiB/s |
| 64B | 2 hops | 52.765 ns | 1.1296 GiB/s | 85.921 ns | 710.37 MiB/s |
| 64B | 3 hops | 76.365 ns | 799.26 MiB/s | 117.58 ns | 519.09 MiB/s |
| 64B | 4 hops | 100.17 ns | 609.34 MiB/s | 148.90 ns | 409.92 MiB/s |
| 64B | 5 hops | 130.59 ns | 467.37 MiB/s | 182.79 ns | 333.92 MiB/s |
| 256B | 1 hops | 58.795 ns | 4.0551 GiB/s | 53.936 ns | 4.4204 GiB/s |
| 256B | 2 hops | 115.71 ns | 2.0604 GiB/s | 89.793 ns | 2.6552 GiB/s |
| 256B | 3 hops | 162.42 ns | 1.4679 GiB/s | 120.47 ns | 1.9791 GiB/s |
| 256B | 4 hops | 221.76 ns | 1.0751 GiB/s | 154.96 ns | 1.5385 GiB/s |
| 256B | 5 hops | 283.72 ns | 860.50 MiB/s | 191.03 ns | 1.2481 GiB/s |
| 1024B | 1 hops | 48.124 ns | 19.817 GiB/s | 54.602 ns | 17.466 GiB/s |
| 1024B | 2 hops | 117.18 ns | 8.1383 GiB/s | 89.531 ns | 10.652 GiB/s |
| 1024B | 3 hops | 159.92 ns | 5.9634 GiB/s | 124.67 ns | 7.6498 GiB/s |
| 1024B | 4 hops | 215.82 ns | 4.4189 GiB/s | 159.07 ns | 5.9952 GiB/s |
| 1024B | 5 hops | 258.70 ns | 3.6864 GiB/s | 197.50 ns | 4.8288 GiB/s |

### Route and Forward (with routing table lookup)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1 hops | 178.04 ns | 5.6166 Melem/s | 152.03 ns | 6.5777 Melem/s |
| 2 hops | 353.15 ns | 2.8317 Melem/s | 284.39 ns | 3.5163 Melem/s |
| 3 hops | 527.53 ns | 1.8956 Melem/s | 415.06 ns | 2.4093 Melem/s |
| 4 hops | 713.90 ns | 1.4007 Melem/s | 559.76 ns | 1.7865 Melem/s |
| 5 hops | 880.92 ns | 1.1352 Melem/s | 695.45 ns | 1.4379 Melem/s |

### Concurrent Forwarding

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Forward | 853.81 us | 4.6849 Melem/s | 564.56 us | 7.0851 Melem/s |
| 8 | Forward | 982.85 us | 8.1396 Melem/s | 803.11 us | 9.9613 Melem/s |
| 16 | Forward | 2.0173 ms | 7.9312 Melem/s | 1.2483 ms | 12.818 Melem/s |

## Swarm / Discovery

### Pingwave

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Serialize | 778.96 ps | 1.2838 Gelem/s | 536.54 ps | 1.8638 Gelem/s |
| Deserialize | 934.41 ps | 1.0702 Gelem/s | 655.20 ps | 1.5262 Gelem/s |
| Roundtrip | 931.54 ps | 1.0735 Gelem/s | 645.41 ps | 1.5494 Gelem/s |
| Forward | 667.72 ps | 1.4976 Gelem/s | 542.53 ps | 1.8432 Gelem/s |

### Local Graph

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create_pingwave | 2.0999 ns | 476.20 Melem/s | 4.9381 ns | 202.51 Melem/s |
| on_pingwave_new | 113.27 ns | 8.8287 Melem/s | 151.82 ns | 6.5867 Melem/s |
| on_pingwave_duplicate | 33.091 ns | 30.219 Melem/s | 16.788 ns | 59.566 Melem/s |
| get_node | 26.430 ns | 37.835 Melem/s | 14.852 ns | 67.330 Melem/s |
| node_count | 199.38 ns | 5.0155 Melem/s | 959.04 ns | 1.0427 Melem/s |
| stats | 597.46 ns | 1.6737 Melem/s | 2.8759 us | 347.72 Kelem/s |

### Graph Scaling

| Nodes | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 100 | all_nodes | 2.5183 us | 39.709 Melem/s | 7.4721 us | 13.383 Melem/s |
| 100 | nodes_within_hops | 2.8790 us | 34.734 Melem/s | 7.5288 us | 13.282 Melem/s |
| 500 | all_nodes | 8.0948 us | 61.768 Melem/s | 16.091 us | 31.074 Melem/s |
| 500 | nodes_within_hops | 9.4634 us | 52.835 Melem/s | 16.272 us | 30.727 Melem/s |
| 1,000 | all_nodes | 132.72 us | 7.5345 Melem/s | 26.862 us | 37.228 Melem/s |
| 1,000 | nodes_within_hops | 60.290 us | 16.587 Melem/s | 26.595 us | 37.600 Melem/s |
| 5,000 | all_nodes | 113.29 us | 44.133 Melem/s | 237.66 us | 21.039 Melem/s |
| 5,000 | nodes_within_hops | 176.48 us | 28.333 Melem/s | 238.64 us | 20.952 Melem/s |

### Path Finding

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| path_1_hop | 1.5412 us | 648.85 Kelem/s | 5.3652 us | 186.39 Kelem/s |
| path_2_hops | 1.5826 us | 631.87 Kelem/s | 5.5363 us | 180.63 Kelem/s |
| path_4_hops | 1.8451 us | 541.99 Kelem/s | 5.8806 us | 170.05 Kelem/s |
| path_not_found | 1.7515 us | 570.93 Kelem/s | 5.7318 us | 174.47 Kelem/s |
| path_complex_graph | 220.18 us | 4.5417 Kelem/s | 177.70 us | 5.6274 Kelem/s |

### Concurrent Pingwave Processing

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Pingwave | 107.57 us | 18.592 Melem/s | 167.26 us | 11.957 Melem/s |
| 8 | Pingwave | 173.56 us | 23.047 Melem/s | 282.09 us | 14.180 Melem/s |
| 16 | Pingwave | 308.13 us | 25.963 Melem/s | 524.87 us | 15.242 Melem/s |

## Failure Detection

### Failure Detector

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| heartbeat_existing | 29.027 ns | 34.451 Melem/s | 35.267 ns | 28.355 Melem/s |
| heartbeat_new | 242.62 ns | 4.1216 Melem/s | 174.10 ns | 5.7438 Melem/s |
| status_check | 14.742 ns | 67.832 Melem/s | 13.418 ns | 74.527 Melem/s |
| check_all | 342.96 ms | 2.9158 elem/s | 331.65 ms | 3.0152 elem/s |
| stats | 80.568 ms | 12.412 elem/s | 100.06 ms | 9.9939 elem/s |

> **⚠️ `check_all` (342 ms) and `stats` (80–100 ms) are benchmark-fixture
> artifacts, NOT hot-path costs — don't chase them.** Both are an O(nodes) scan
> whose cost Criterion reports *per element* against a fixture the
> `heartbeat_new` bench balloons to millions of entries (it inserts a fresh id
> every iteration into the shared detector). On the real hot path:
> `check_all` runs once per `heartbeat_interval` (default 5 s) and costs
> ~204 µs at 5,000 nodes — see *Failure Scaling* below, which grows linearly
> and contradicts the 342 ms figure — i.e. ~40 µs/s amortized; `stats` is
> observability-only by documented design. The genuine per-heartbeat costs are
> `heartbeat_existing` / `heartbeat_new` / `status_check` (14–242 ns). See
> `docs/misc/PERF_AUDIT_2026_06_09_HOT_PATH.md` §14.

### Circuit Breaker

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| allow_closed | 13.439 ns | 74.412 Melem/s | 10.166 ns | 98.371 Melem/s |
| record_success | 9.6538 ns | 103.59 Melem/s | 8.6455 ns | 115.67 Melem/s |
| record_failure | 9.6089 ns | 104.07 Melem/s | 9.9009 ns | 101.00 Melem/s |
| state | 13.437 ns | 74.423 Melem/s | 10.263 ns | 97.435 Melem/s |

### Recovery Manager

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| on_failure_with_alternates | 251.67 ns | 3.9734 Melem/s | 254.20 ns | 3.9339 Melem/s |
| on_failure_no_alternates | 322.50 ns | 3.1008 Melem/s | 222.76 ns | 4.4891 Melem/s |
| get_action | 37.080 ns | 26.968 Melem/s | 37.864 ns | 26.410 Melem/s |
| is_failed | 13.599 ns | 73.536 Melem/s | 12.822 ns | 77.993 Melem/s |
| on_recovery | 106.20 ns | 9.4162 Melem/s | 124.46 ns | 8.0346 Melem/s |
| stats | 701.68 ps | 1.4251 Gelem/s | 1.2060 ns | 829.22 Melem/s |

### Full Recovery Cycle

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Fail + recover cycle | 287.97 ns | 3.4726 Melem/s | 255.40 ns | 3.9154 Melem/s |

### Loss Simulator

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 1% | 2.7977 ns | 357.44 Melem/s | 11.051 ns | 90.492 Melem/s |
| 5% | 3.1565 ns | 316.80 Melem/s | 11.468 ns | 87.198 Melem/s |
| 10% | 3.6260 ns | 275.79 Melem/s | 11.974 ns | 83.518 Melem/s |
| 20% | 4.5804 ns | 218.32 Melem/s | 13.007 ns | 76.884 Melem/s |
| burst | 2.9330 ns | 340.95 Melem/s | 11.540 ns | 86.659 Melem/s |

### Failure Scaling (check_all)

| Nodes | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 100 | check_all | 4.8056 us | 20.809 Melem/s | 8.8337 us | 11.320 Melem/s |
| 100 | healthy_nodes | 1.7158 us | 58.281 Melem/s | 7.0171 us | 14.251 Melem/s |
| 500 | check_all | 20.892 us | 23.933 Melem/s | 23.103 us | 21.642 Melem/s |
| 500 | healthy_nodes | 5.4386 us | 91.936 Melem/s | 9.8395 us | 50.816 Melem/s |
| 1,000 | check_all | 40.976 us | 24.405 Melem/s | 41.088 us | 24.338 Melem/s |
| 1,000 | healthy_nodes | 10.222 us | 97.831 Melem/s | 14.881 us | 67.199 Melem/s |
| 5,000 | check_all | 203.50 us | 24.570 Melem/s | 182.98 us | 27.325 Melem/s |
| 5,000 | healthy_nodes | 48.838 us | 102.38 Melem/s | 53.553 us | 93.365 Melem/s |

### Concurrent Heartbeats

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Heartbeat | 186.32 us | 10.734 Melem/s | 207.56 us | 9.6357 Melem/s |
| 8 | Heartbeat | 254.05 us | 15.745 Melem/s | 323.27 us | 12.374 Melem/s |
| 16 | Heartbeat | 466.77 us | 17.139 Melem/s | 568.92 us | 14.062 Melem/s |

## Stream Multiplexing

| Streams | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 10 | Lookup | 291.86 ns | 34.263 Melem/s | 348.28 ns | 28.713 Melem/s |
| 10 | Stats | 484.33 ns | 20.647 Melem/s | 410.47 ns | 24.362 Melem/s |
| 100 | Lookup | 2.9157 us | 34.297 Melem/s | 3.4340 us | 29.120 Melem/s |
| 100 | Stats | 4.9629 us | 20.149 Melem/s | 4.1103 us | 24.329 Melem/s |
| 1,000 | Lookup | 29.223 us | 34.220 Melem/s | 35.485 us | 28.181 Melem/s |
| 1,000 | Stats | 52.473 us | 19.058 Melem/s | 43.948 us | 22.754 Melem/s |
| 10,000 | Lookup | 292.93 us | 34.138 Melem/s | 386.36 us | 25.882 Melem/s |
| 10,000 | Stats | 569.47 us | 17.560 Melem/s | 462.94 us | 21.601 Melem/s |

## Fair Scheduler

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Creation | 288.21 ns | 3.4697 Melem/s | 1.5589 us | 641.48 Kelem/s |
| Stream count (empty) | 200.77 ns | 4.9809 Melem/s | 947.66 ns | 1.0552 Melem/s |
| Total queued | 312.04 ps | 3.2047 Gelem/s | 200.77 ps | 4.9809 Gelem/s |
| Cleanup (empty) | 201.26 ns | 4.9686 Melem/s | 1.2865 us | 777.29 Kelem/s |

## Capability System

### CapabilitySet

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create | 533.54 ns | 1.8743 Melem/s | 777.18 ns | 1.2867 Melem/s |
| serialize | 930.29 ns | 1.0749 Melem/s | 712.47 ns | 1.4036 Melem/s |
| deserialize | 1.7227 us | 580.48 Kelem/s | 3.0854 us | 324.11 Kelem/s |
| roundtrip | 2.6938 us | 371.22 Kelem/s | 3.9750 us | 251.57 Kelem/s |
| has_tag | 747.24 ps | 1.3383 Gelem/s | 605.35 ps | 1.6519 Gelem/s |
| has_model | 934.44 ps | 1.0702 Gelem/s | 445.66 ps | 2.2438 Gelem/s |
| has_tool | 747.85 ps | 1.3372 Gelem/s | 690.86 ps | 1.4475 Gelem/s |
| has_gpu | 311.50 ps | 3.2103 Gelem/s | 199.80 ps | 5.0051 Gelem/s |

### Capability Announcement

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| create | 374.61 ns | 2.6695 Melem/s | 2.3375 us | 427.81 Kelem/s |
| serialize | 1.2338 us | 810.48 Kelem/s | 1.7309 us | 577.72 Kelem/s |
| deserialize | 2.0844 us | 479.75 Kelem/s | 2.7562 us | 362.82 Kelem/s |
| is_expired | 25.249 ns | 39.605 Melem/s | 21.715 ns | 46.051 Melem/s |

### Capability Serialization (Simple vs Complex)

| Type | Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| Simple | Serialize | 19.123 ns | 52.294 Melem/s | 37.537 ns | 26.640 Melem/s |
| Simple | Deserialize | 4.7538 ns | 210.36 Melem/s | 8.8392 ns | 113.13 Melem/s |
| Complex | Serialize | 40.989 ns | 24.397 Melem/s | 38.401 ns | 26.041 Melem/s |
| Complex | Deserialize | 153.56 ns | 6.5121 Melem/s | 1.0230 us | 977.50 Kelem/s |

### Capability Filter

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| single_tag | 9.9682 ns | 100.32 Melem/s | 3.4333 ns | 291.26 Melem/s |
| require_gpu | 4.0489 ns | 246.98 Melem/s | 1.7827 ns | 560.96 Melem/s |
| gpu_vendor | 3.7378 ns | 267.54 Melem/s | 1.9699 ns | 507.64 Melem/s |
| min_memory | 3.7389 ns | 267.46 Melem/s | 1.7762 ns | 562.99 Melem/s |
| complex | 10.279 ns | 97.282 Melem/s | 5.7069 ns | 175.23 Melem/s |
| no_match | 3.1157 ns | 320.95 Melem/s | 1.7608 ns | 567.91 Melem/s |

### Capability Index (Insert)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| 100 nodes | 111.54 us | 896.51 Kelem/s | 168.55 us | 593.28 Kelem/s |
| 1,000 nodes | 1.1739 ms | 851.86 Kelem/s | 1.3966 ms | 716.05 Kelem/s |
| 10,000 nodes | 17.241 ms | 580.00 Kelem/s | 15.626 ms | 639.96 Kelem/s |

### Capability Index (Query, 10,000 nodes)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| single_tag | 253.51 us | 3.9446 Kelem/s | 175.95 us | 5.6833 Kelem/s |
| require_gpu | 325.43 us | 3.0728 Kelem/s | 180.63 us | 5.5362 Kelem/s |
| gpu_vendor | 827.10 us | 1.2090 Kelem/s | 508.12 us | 1.9681 Kelem/s |
| min_memory | 841.27 us | 1.1887 Kelem/s | 533.36 us | 1.8749 Kelem/s |
| complex | 611.13 us | 1.6363 Kelem/s | 335.95 us | 2.9766 Kelem/s |
| model | 99.107 us | 10.090 Kelem/s | 96.330 us | 10.381 Kelem/s |
| tool | 771.71 us | 1.2958 Kelem/s | 494.54 us | 2.0221 Kelem/s |
| no_results | 22.229 ns | 44.987 Melem/s | 28.914 ns | 34.585 Melem/s |

### Capability Index (Find Best)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| Simple | 313.38 us | 3.1910 Kelem/s | 209.77 us | 4.7670 Kelem/s |
| With preferences | 653.69 us | 1.5298 Kelem/s | 431.18 us | 2.3192 Kelem/s |

### Capability Search (1,000 nodes)

| Operation | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|
| find_with_gpu | 17.250 us | 57.972 Kelem/s | 30.519 us | 32.766 Kelem/s |
| find_by_tool (Python) | 31.243 us | 32.007 Kelem/s | 61.204 us | 16.339 Kelem/s |
| find_by_tool (Rust) | 39.598 us | 25.254 Kelem/s | 76.874 us | 13.008 Kelem/s |

### Capability Index Scaling

| Nodes | Query Type | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 1,000 | Tag | 12.534 us | 79.785 Kelem/s | 10.348 us | 96.634 Kelem/s |
| 1,000 | Complex | 40.487 us | 24.699 Kelem/s | 27.562 us | 36.281 Kelem/s |
| 5,000 | Tag | 70.269 us | 14.231 Kelem/s | 54.408 us | 18.380 Kelem/s |
| 5,000 | Complex | 302.95 us | 3.3009 Kelem/s | 136.51 us | 7.3252 Kelem/s |
| 10,000 | Tag | 154.98 us | 6.4525 Kelem/s | 171.99 us | 5.8142 Kelem/s |
| 10,000 | Complex | 601.95 us | 1.6613 Kelem/s | 338.57 us | 2.9536 Kelem/s |
| 50,000 | Tag | 2.5575 ms | 391.00 elem/s | 1.2324 ms | 811.41 elem/s |
| 50,000 | Complex | 3.8061 ms | 262.73 elem/s | 2.1659 ms | 461.69 elem/s |

### Concurrent Capability Index

| Threads | Pool | M1 Max | M1 Throughput | i9-14900K | i9 Throughput |
|---|---|---|---|---|---|
| 4 | Insert | 393.20 us | 5.0865 Melem/s | 568.09 us | 3.5206 Melem/s |
| 4 | Query | 387.20 ms | 5.1653 Kelem/s | 249.54 ms | 8.0147 Kelem/s |
| 8 | Insert | 446.81 us | 8.9523 Melem/s | 1.1444 ms | 3.4954 Melem/s |
| 8 | Query | 604.06 ms | 6.6218 Kelem/s | 301.72 ms | 13.257 Kelem/s |
| 16 | Insert | 982.61 us | 8.1416 Melem/s | 1.3039 ms | 6.1353 Melem/s |
| 16 | Query | 1.0196 s | 7.8460 Kelem/s | 436.90 ms | 18.311 Kelem/s |

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
./benchmarks/parse_criterion.py BENCHMARK_RESULTS_M1_MAX.md /tmp/m1_max_parsed.md
```

## Key Insights

1. **Header serialize/deserialize runs at ~760–910M ops/sec** (i9) / ~470–505M ops/sec (M1) — sub-2ns per operation; AAD generation cleared 900M ops/sec on i9.
2. **Routing header forward at 5G ops/sec on i9** (~200 ps) — routing header serialize/roundtrip 1.07–2.18G ops/sec across both platforms.
3. **Thread-local pool eliminates contention** — up to ~21x faster than shared pool at 32 threads (M1: 76.9M vs 3.61M ops/sec; i9: 110.4M vs 6.89M ops/sec).
4. **Capability filters run at ~97–568M ops/sec** — fast enough for inline packet decisions.
5. **Circuit breaker checks are ~10ns** — negligible overhead per packet.
6. **Event frame write scales with payload** — ~1.7–3.3 GiB/s at 64B, ~49 GiB/s on M1 / ~75 GiB/s on i9 at 4KB.
7. **Multi-hop forwarding adds ~30–60ns per hop** — linear scaling, no amplification.
8. **CortEX ingestion got materially faster on M1**: `tasks.create` is now 113 ns (8.87M ops/sec, was 266 ns / 3.77M); `tasks_count_where` @ 10K is 6.7 µs (1.5G elem/sec, was 38.2 µs / 262M elem/sec). Snapshot encode @ 10K is ~4× faster on both platforms.
