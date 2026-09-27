# BLTP Performance Optimizations Plan

Practical optimizations for BLTP as a component of Blackstream/TheNet.

**Design Constraints**:
- Zero friction for L1/L0
- Maximum portability (macOS, Linux, Windows, ARM, x86)
- Zero-configuration install
- No privilege requirements

---

## Summary

| Phase | Optimization | Expected Gain | Effort | Priority |
|-------|-------------|---------------|--------|----------|
| 1.1 | Adaptive Batch Sizing | +15-30% | 2-3 days | **Core** |
| 1.2 | Precomputed Key Schedule | +20-40% AEAD | 1-2 days | **Core** |
| 1.3 | xxh3 for Routing/Sharding | ~free | 1 day | **Core** |
| 1.4 | Double-Buffered Pool | +10-15% | 2 days | **Core** |
| 1.5 | Portable SIMD Copies | +10-20% | 1-2 days | **Core** |
| 2.1 | CPU Affinity | +15-25% | 2-3 days | Turbo |
| 2.2 | AES-GCM Runtime Selection | +20-50% x86 | 2 days | Turbo |

**Target**: 70-130M events/s, 12+ GB/s frame throughput

---

## Phase 1: Core Performance (Ship This)

### 1.1 Adaptive Batch Sizing

**Current**: Fixed batch at `MAX_PAYLOAD_SIZE` (8112 bytes), greedy packing.

**Problem**: Suboptimal for bursty workloads, no latency budget awareness.

**Solution**: Dynamic batch sizing based on queue pressure and latency target.

#### Implementation

```
File: src/adapter/bltp/batch.rs (new)

pub struct AdaptiveBatcher {
    min_batch_size: usize,      // Default: 1KB
    max_batch_size: usize,      // Default: 8KB  
    target_latency_us: u64,     // Default: 100μs
    
    // Metrics (exponential moving average)
    avg_batch_latency_us: f64,
    queue_depth: usize,
    burst_detected: bool,
}

impl AdaptiveBatcher {
    pub fn optimal_size(&self) -> usize {
        if self.burst_detected || self.queue_depth > 100 {
            // Burst mode: maximize batch size
            self.max_batch_size
        } else if self.avg_batch_latency_us > self.target_latency_us as f64 {
            // Latency pressure: reduce batch size
            (self.min_batch_size + self.max_batch_size) / 2
        } else {
            // Normal: grow toward max
            self.max_batch_size
        }
    }
    
    pub fn record(&mut self, batch_size: usize, latency_us: u64, queue_depth: usize) {
        const ALPHA: f64 = 0.1;
        self.avg_batch_latency_us = ALPHA * latency_us as f64 
            + (1.0 - ALPHA) * self.avg_batch_latency_us;
        self.queue_depth = queue_depth;
        self.burst_detected = queue_depth > 50;
    }
}
```

#### Integration

```
File: src/adapter/bltp/mod.rs

impl BltpAdapter {
    fn on_batch(&self, events: Vec<RawEvent>) -> Result<()> {
        let start = Instant::now();
        let optimal_size = self.batcher.optimal_size();
        
        // ... existing batching logic, use optimal_size instead of MAX_PAYLOAD_SIZE
        
        self.batcher.record(
            bytes_sent,
            start.elapsed().as_micros() as u64,
            self.pending_queue.len(),
        );
        Ok(())
    }
}
```

#### Config

```rust
pub struct BltpAdapterConfig {
    // ... existing fields
    pub adaptive_batching: bool,        // Default: true
    pub min_batch_size: usize,          // Default: 1024
    pub max_batch_size: usize,          // Default: 8112
    pub target_batch_latency_us: u64,   // Default: 100
}
```

---

### 1.2 Precomputed Key Schedule (Counter-Based Nonces)

**Current**: XChaCha20-Poly1305 with random 24-byte nonces. Each packet runs HChaCha20 to derive subkey.

**Problem**: HChaCha20 derivation adds ~20% overhead per packet.

**Solution**: Switch to ChaCha20-Poly1305 with counter-based 12-byte nonces.

#### Nonce Format

```
┌─────────────────┬─────────────────────────┐
│ session_prefix  │       counter           │
│    (4 bytes)    │      (8 bytes)          │
└─────────────────┴─────────────────────────┘

session_prefix = session_id[0..4]  (from handshake)
counter = atomic u64, incremented per packet
```

This is safe because:
- Counter never repeats within session
- Session prefix ensures uniqueness across sessions
- 2^64 packets before rollover (unreachable)

#### Implementation

```
File: src/adapter/bltp/crypto.rs

use chacha20poly1305::{ChaCha20Poly1305, aead::Aead};

pub struct FastPacketCipher {
    cipher: ChaCha20Poly1305,
    session_prefix: [u8; 4],
    nonce_counter: AtomicU64,
}

impl FastPacketCipher {
    pub fn new(key: &[u8; 32], session_id: u64) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            session_prefix: (session_id as u32).to_le_bytes(),
            nonce_counter: AtomicU64::new(0),
        }
    }
    
    fn next_nonce(&self) -> [u8; 12] {
        let counter = self.nonce_counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce = [0u8; 12];
        nonce[0..4].copy_from_slice(&self.session_prefix);
        nonce[4..12].copy_from_slice(&counter.to_le_bytes());
        nonce
    }
    
    pub fn encrypt_in_place(&self, aad: &[u8], buffer: &mut BytesMut) -> [u8; 12] {
        let nonce = self.next_nonce();
        self.cipher.encrypt_in_place(
            &nonce.into(),
            aad,
            buffer,
        ).expect("encryption failed");
        nonce
    }
}
```

#### Header Update

```
File: src/adapter/bltp/protocol.rs

// Change nonce field from 24 bytes to 12 bytes
// Or: keep 24-byte field, zero-pad upper 12 bytes for compatibility

#[repr(C, align(64))]
pub struct BltpHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub flags: u8,
    pub reserved: [u8; 2],
    pub session_id: u64,
    pub stream_id: u64,
    pub sequence: u64,
    pub timestamp: u64,
    pub payload_len: u32,
    pub nonce: [u8; 12],      // Changed from 24
    pub _padding: [u8; 12],   // Maintain 64-byte alignment
}
```

#### Backward Compatibility

Add version flag to negotiate cipher mode:
- Version 1: XChaCha20-Poly1305, random nonces (current)
- Version 2: ChaCha20-Poly1305, counter nonces (new default)

---

### 1.3 xxh3 for Routing/Sharding

**Current**: Various hashing for stream IDs, shard routing.

**Solution**: Use xxh3 everywhere for consistency and speed.

#### Implementation

```
File: Cargo.toml

[dependencies]
xxhash-rust = { version = "0.8", features = ["xxh3"] }
```

```
File: src/adapter/bltp/mod.rs

use xxhash_rust::xxh3::xxh3_64;

fn route_to_shard(event: &[u8], num_shards: u16) -> u16 {
    (xxh3_64(event) % num_shards as u64) as u16
}

fn stream_id_from_data(data: &[u8]) -> u64 {
    xxh3_64(data)
}
```

xxh3 is:
- ~50GB/s on modern CPUs
- Portable (no SIMD requirements, auto-vectorizes)
- Already a dependency via the core Blackstream crate

---

### 1.4 Double-Buffered Packet Pool

**Current**: Single `ArrayQueue<PacketBuilder>` with atomic acquire/release.

**Problem**: Contention under high load.

**Solution**: Thread-local pools with shared fallback.

#### Implementation

```
File: src/adapter/bltp/pool.rs

use std::cell::RefCell;

thread_local! {
    static LOCAL_BUILDERS: RefCell<Vec<PacketBuilder>> = RefCell::new(Vec::new());
}

pub struct ThreadLocalPool {
    shared: ArrayQueue<PacketBuilder>,
    local_capacity: usize,  // Default: 8
    key: [u8; 32],
}

impl ThreadLocalPool {
    pub fn acquire(&self) -> PacketBuilder {
        LOCAL_BUILDERS.with(|pool| {
            let mut pool = pool.borrow_mut();
            
            // Try local first (zero atomics)
            if let Some(mut builder) = pool.pop() {
                builder.clear();
                return builder;
            }
            
            // Refill from shared
            for _ in 0..self.local_capacity {
                if let Some(b) = self.shared.pop() {
                    pool.push(b);
                } else {
                    break;
                }
            }
            
            pool.pop().unwrap_or_else(|| PacketBuilder::new(&self.key))
        })
    }
    
    pub fn release(&self, builder: PacketBuilder) {
        LOCAL_BUILDERS.with(|pool| {
            let mut pool = pool.borrow_mut();
            
            if pool.len() < self.local_capacity * 2 {
                // Keep in local pool
                pool.push(builder);
            } else {
                // Return excess to shared
                let _ = self.shared.push(builder);
            }
        })
    }
}
```

**Benefit**: Hot path has zero atomic operations when local pool is warm.

---

### 1.5 Portable SIMD Copies

**Current**: Standard `copy_from_slice`, relies on compiler auto-vectorization.

**Solution**: Ensure compiler optimizations are enabled, use `wide` crate for explicit SIMD where beneficial.

#### Compiler Flags

```
File: .cargo/config.toml

[build]
rustflags = ["-C", "target-cpu=native"]

[profile.release]
lto = "thin"
codegen-units = 1
```

#### Explicit SIMD for Batch Encoding (Optional)

```
File: Cargo.toml

[dependencies]
wide = { version = "0.7", optional = true }

[features]
simd = ["wide"]
```

```
File: src/adapter/bltp/protocol.rs

#[cfg(feature = "simd")]
use wide::u8x32;

/// Fast buffer clear using SIMD
#[cfg(feature = "simd")]
pub fn clear_buffer_fast(buf: &mut [u8]) {
    let zero = u8x32::ZERO;
    let chunks = buf.len() / 32;
    
    for i in 0..chunks {
        let ptr = buf[i * 32..].as_mut_ptr() as *mut u8x32;
        unsafe { ptr.write(zero); }
    }
    
    // Clear remainder
    for byte in &mut buf[chunks * 32..] {
        *byte = 0;
    }
}
```

**Note**: Only use explicit SIMD where profiling shows benefit. Rust's auto-vectorization handles most cases.

---

## Phase 2: Turbo Mode (Feature-Gated)

These optimizations are enabled via `--features bltp-turbo`.

### 2.1 CPU Affinity

Pin critical threads to dedicated cores for reduced context-switch noise.

```
File: Cargo.toml

[dependencies]
core_affinity = { version = "0.8", optional = true }

[features]
bltp-turbo = ["core_affinity"]
```

```
File: src/adapter/bltp/affinity.rs

#[cfg(feature = "bltp-turbo")]
pub fn pin_to_core(core_id: usize) -> bool {
    core_affinity::get_core_ids()
        .and_then(|cores| cores.get(core_id).cloned())
        .map(|core| core_affinity::set_for_current(core))
        .unwrap_or(false)
}

#[cfg(not(feature = "bltp-turbo"))]
pub fn pin_to_core(_: usize) -> bool { false }
```

**Usage**: Only when explicitly configured via `BltpAdapterConfig::cpu_affinity`.

### 2.2 AES-GCM Runtime Selection

Use AES-GCM on x86 with AES-NI, ChaCha20 elsewhere.

```
File: Cargo.toml

[dependencies]
aes-gcm = { version = "0.10", optional = true }
cpufeatures = "0.2"

[features]
bltp-turbo = ["aes-gcm", "core_affinity"]
```

```
File: src/adapter/bltp/crypto.rs

#[cfg(feature = "bltp-turbo")]
pub fn select_cipher(key: &[u8; 32], session_id: u64) -> Box<dyn PacketCipherTrait> {
    #[cfg(target_arch = "x86_64")]
    if cpufeatures::x86_64::aes() {
        return Box::new(AesGcmCipher::new(key, session_id));
    }
    
    Box::new(FastPacketCipher::new(key, session_id))
}
```

**Note**: ChaCha20 is faster on ARM (M1/M2, Graviton). Only use AES on x86 with hardware support.

---

## Phase 3: Future / TheNet HPC Edition

**NOT for Blackstream core.** Reserved for TheNet when needed:

- io_uring transport (Linux-only, requires privileges)
- Zero-copy mmap buffers
- Direct NIC queue polling
- RDMA integration

These would be separate crates/features for specialized deployments.

---

## The Missing Piece: FFI Boundary Optimization

**The real bottleneck** is not BLTP itself, but the language bridge:

| Binding | Events/sec |
|---------|-----------|
| Node.js | ~2.9M |
| Bun | ~3.37M |
| Rust native | 74M |

**25x gap** due to FFI overhead.

### Priority FFI Optimizations

1. **Batch-oriented FFI surface**
   - `ingest_raw_batch()` already exists
   - Add `ingest_raw_batch_ptr(ptr, lens[], count)` - zero-copy from JS

2. **Avoid UTF-8 validation**
   - Current: String validation on every event
   - Fix: Accept raw bytes, skip validation for trusted sources

3. **Streaming shared-memory buffers**
   - Pre-allocate large buffer in Rust
   - Map into Node/Python address space
   - Write events directly, signal Rust to process

4. **Reduce NAPI callback count**
   - Batch results instead of per-event callbacks
   - Use `napi::threadsafe_function` for async batches

5. **Binary frame preallocation**
   - Reuse event frame buffers across FFI calls
   - Pool of pre-sized buffers for common event sizes

### Example: Zero-Copy Batch Ingest

```rust
// Node.js binding
#[napi]
pub fn ingest_raw_batch_fast(buffers: Vec<Buffer>) -> u32 {
    let events: Vec<RawEvent> = buffers
        .iter()
        .map(|b| RawEvent::from_bytes_unchecked(
            Bytes::copy_from_slice(b.as_ref())
        ))
        .collect();
    
    self.bus.ingest_raw_batch(events) as u32
}
```

---

## Implementation Order

### Week 1: Core Performance
1. Phase 1.2: Counter-based nonces (+20-40% AEAD)
2. Phase 1.3: xxh3 routing (trivial, ~free)
3. Phase 1.4: Thread-local pool (+10-15%)

### Week 2: Adaptive + SIMD
4. Phase 1.1: Adaptive batching (+15-30%)
5. Phase 1.5: Compiler flags + optional SIMD

### Week 3: FFI Optimization
6. Zero-copy batch ingest for Node.js
7. Binary buffer pooling
8. Reduce UTF-8 validation

### Optional: Turbo Mode
9. Phase 2.1: CPU affinity (behind feature flag)
10. Phase 2.2: AES-GCM selection (behind feature flag)

---

## Feature Flags

```toml
[features]
default = []

# Core optimizations (always safe)
bltp = [...]  # Existing

# Performance tier
bltp-turbo = [
    "bltp",
    "core_affinity",
    "aes-gcm",
]
```

---

## Success Metrics

| Metric | Current | After Phase 1 | After FFI Opt |
|--------|---------|---------------|---------------|
| Rust events/sec | 74M | 110-130M | 130M |
| Node events/sec | 2.9M | 3.5M | 8-10M |
| AEAD throughput | ~2 GB/s | ~3 GB/s | ~3 GB/s |
| p99 latency | ~500μs | ~200μs | ~200μs |

The biggest wins come from:
1. Counter-based nonces (easy, high impact)
2. FFI optimization (harder, highest user-facing impact)
3. Adaptive batching (medium, helps bursty workloads)
