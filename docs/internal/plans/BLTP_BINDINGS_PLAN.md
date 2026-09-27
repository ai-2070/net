# BLTP Language Bindings Implementation Plan

This document outlines the plan for adding BLTP (Blackstream L0 Transport Protocol) support to the TypeScript, Python, and Go language bindings.

## Overview

BLTP is an encrypted UDP transport that requires cryptographic keypairs and pre-shared keys. Unlike Redis/JetStream which only need URLs, BLTP requires:
- Static keypair generation (X25519)
- Pre-shared key (32 bytes)
- Connection role (initiator vs responder)
- Peer's static public key (for initiator)

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                      Language Bindings                            │
├──────────────────┬──────────────────┬────────────────────────────┤
│   TypeScript     │     Python       │           Go               │
│   (NAPI-RS)      │     (PyO3)       │         (CGO)              │
├──────────────────┴──────────────────┴────────────────────────────┤
│                         C FFI Layer                               │
│  - bltp_generate_keypair()                                        │
│  - parse_config_json() with BLTP support                          │
├──────────────────────────────────────────────────────────────────┤
│                      Rust Core (BLTP)                             │
│  - StaticKeypair                                                  │
│  - BltpAdapterConfig                                              │
└──────────────────────────────────────────────────────────────────┘
```

## Implementation Steps

### Phase 1: FFI Layer Extensions

#### 1.1 Add Keypair Generation FFI Function

Add to `src/ffi/mod.rs`:

```rust
#[cfg(feature = "bltp")]
use crate::adapter::bltp::crypto::StaticKeypair;

/// Generate a new BLTP keypair.
/// 
/// Returns a JSON object with:
/// - `public_key`: hex-encoded 32-byte public key
/// - `secret_key`: hex-encoded 32-byte secret key
/// 
/// The caller must free the returned string with `blackstream_free_string`.
#[cfg(feature = "bltp")]
#[unsafe(no_mangle)]
pub extern "C" fn bltp_generate_keypair() -> *mut c_char {
    let keypair = StaticKeypair::generate();
    let json = serde_json::json!({
        "public_key": hex::encode(keypair.public_key()),
        "secret_key": hex::encode(keypair.secret_key()),
    });
    
    let s = CString::new(json.to_string()).unwrap();
    s.into_raw()
}

/// Free a string returned by BLTP functions.
#[cfg(feature = "bltp")]
#[unsafe(no_mangle)]
pub extern "C" fn blackstream_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)); }
    }
}
```

#### 1.2 Add BLTP Config Parsing

Extend `parse_config_json()` in `src/ffi/mod.rs`:

```rust
#[cfg(feature = "bltp")]
if let Some(bltp) = value.get("bltp") {
    let bind_addr: SocketAddr = bltp.get("bind_addr")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())?;
    
    let peer_addr: SocketAddr = bltp.get("peer_addr")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())?;
    
    let psk: [u8; 32] = bltp.get("psk")
        .and_then(|v| v.as_str())
        .and_then(|s| hex::decode(s).ok())
        .and_then(|v| v.try_into().ok())?;
    
    let role = bltp.get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("initiator");
    
    let bltp_config = match role {
        "initiator" => {
            let peer_pubkey: [u8; 32] = bltp.get("peer_public_key")
                .and_then(|v| v.as_str())
                .and_then(|s| hex::decode(s).ok())
                .and_then(|v| v.try_into().ok())?;
            BltpAdapterConfig::initiator(bind_addr, peer_addr, psk, peer_pubkey)
        }
        "responder" => {
            let secret_key: [u8; 32] = bltp.get("secret_key")
                .and_then(|v| v.as_str())
                .and_then(|s| hex::decode(s).ok())
                .and_then(|v| v.try_into().ok())?;
            let public_key: [u8; 32] = bltp.get("public_key")
                .and_then(|v| v.as_str())
                .and_then(|s| hex::decode(s).ok())
                .and_then(|v| v.try_into().ok())?;
            let keypair = StaticKeypair::from_bytes(secret_key, public_key);
            BltpAdapterConfig::responder(bind_addr, peer_addr, psk, keypair)
        }
        _ => return None,
    };
    
    // Apply optional settings
    let mut config = bltp_config;
    if let Some(reliability) = bltp.get("reliability").and_then(|v| v.as_str()) {
        config = config.with_reliability(match reliability {
            "none" => ReliabilityConfig::None,
            "light" => ReliabilityConfig::Light,
            "full" => ReliabilityConfig::Full,
            _ => ReliabilityConfig::None,
        });
    }
    // ... more optional settings
    
    builder = builder.adapter(AdapterConfig::Bltp(config));
}
```

### Phase 2: TypeScript/Node.js Binding

#### 2.1 Add BLTP Types

In `bindings/node/src/lib.rs`:

```rust
#[cfg(feature = "bltp")]
#[napi(object)]
pub struct BltpKeypair {
    pub public_key: String,  // hex-encoded
    pub secret_key: String,  // hex-encoded
}

#[cfg(feature = "bltp")]
#[napi(object)]
pub struct BltpOptions {
    pub bind_addr: String,
    pub peer_addr: String,
    pub psk: String,              // hex-encoded 32 bytes
    pub role: String,             // "initiator" or "responder"
    pub peer_public_key: Option<String>,  // hex, required for initiator
    pub secret_key: Option<String>,       // hex, required for responder
    pub public_key: Option<String>,       // hex, required for responder
    pub reliability: Option<String>,      // "none", "light", "full"
    pub heartbeat_interval_ms: Option<u32>,
    pub session_timeout_ms: Option<u32>,
    pub batched_io: Option<bool>,
}
```

#### 2.2 Add Keypair Generation Function

```rust
#[cfg(feature = "bltp")]
#[napi]
pub fn generate_bltp_keypair() -> BltpKeypair {
    let keypair = StaticKeypair::generate();
    BltpKeypair {
        public_key: hex::encode(keypair.public_key()),
        secret_key: hex::encode(keypair.secret_key()),
    }
}
```

#### 2.3 Extend EventBusOptions

```rust
#[napi(object)]
pub struct EventBusOptions {
    // ... existing fields ...
    
    #[cfg(feature = "bltp")]
    pub bltp: Option<BltpOptions>,
}
```

#### 2.4 TypeScript Usage Example

```typescript
import { Blackstream, generateBltpKeypair, BltpOptions } from '@anthropic/blackstream';

// Responder side - generates keypair
const responderKeypair = generateBltpKeypair();
const psk = crypto.randomBytes(32).toString('hex');

const responder = new Blackstream({
  numShards: 2,
  bltp: {
    bindAddr: '127.0.0.1:9001',
    peerAddr: '127.0.0.1:9000',
    psk: psk,
    role: 'responder',
    secretKey: responderKeypair.secretKey,
    publicKey: responderKeypair.publicKey,
    reliability: 'light',
  }
});

// Initiator side - knows responder's public key
const initiator = new Blackstream({
  numShards: 2,
  bltp: {
    bindAddr: '127.0.0.1:9000',
    peerAddr: '127.0.0.1:9001',
    psk: psk,
    role: 'initiator',
    peerPublicKey: responderKeypair.publicKey,
  }
});
```

### Phase 3: Python Binding

#### 3.1 Add BLTP Functions and Parameters

In `bindings/python/src/lib.rs`:

```rust
#[cfg(feature = "bltp")]
#[pyfunction]
fn generate_bltp_keypair() -> PyResult<(String, String)> {
    let keypair = StaticKeypair::generate();
    Ok((
        hex::encode(keypair.public_key()),
        hex::encode(keypair.secret_key()),
    ))
}

#[pyclass]
struct Blackstream {
    // ... existing ...
}

#[pymethods]
impl Blackstream {
    #[new]
    #[pyo3(signature = (
        // ... existing parameters ...
        bltp_bind_addr=None,
        bltp_peer_addr=None,
        bltp_psk=None,
        bltp_role=None,
        bltp_peer_public_key=None,
        bltp_secret_key=None,
        bltp_public_key=None,
        bltp_reliability=None,
        bltp_heartbeat_interval_ms=None,
        bltp_session_timeout_ms=None,
        bltp_batched_io=None,
    ))]
    fn new(
        // ... existing parameters ...
        #[cfg(feature = "bltp")] bltp_bind_addr: Option<String>,
        #[cfg(feature = "bltp")] bltp_peer_addr: Option<String>,
        #[cfg(feature = "bltp")] bltp_psk: Option<String>,
        #[cfg(feature = "bltp")] bltp_role: Option<String>,
        #[cfg(feature = "bltp")] bltp_peer_public_key: Option<String>,
        #[cfg(feature = "bltp")] bltp_secret_key: Option<String>,
        #[cfg(feature = "bltp")] bltp_public_key: Option<String>,
        #[cfg(feature = "bltp")] bltp_reliability: Option<String>,
        #[cfg(feature = "bltp")] bltp_heartbeat_interval_ms: Option<u64>,
        #[cfg(feature = "bltp")] bltp_session_timeout_ms: Option<u64>,
        #[cfg(feature = "bltp")] bltp_batched_io: Option<bool>,
    ) -> PyResult<Self> {
        // ... build config ...
    }
}
```

#### 3.2 Python Usage Example

```python
from blackstream import Blackstream, generate_bltp_keypair
import os

# Responder side
public_key, secret_key = generate_bltp_keypair()
psk = os.urandom(32).hex()

responder = Blackstream(
    num_shards=2,
    bltp_bind_addr="127.0.0.1:9001",
    bltp_peer_addr="127.0.0.1:9000",
    bltp_psk=psk,
    bltp_role="responder",
    bltp_secret_key=secret_key,
    bltp_public_key=public_key,
    bltp_reliability="light",
)

# Initiator side
initiator = Blackstream(
    num_shards=2,
    bltp_bind_addr="127.0.0.1:9000",
    bltp_peer_addr="127.0.0.1:9001",
    bltp_psk=psk,
    bltp_role="initiator",
    bltp_peer_public_key=public_key,
)
```

### Phase 4: Go Binding

#### 4.1 Add BLTP Types

In `bindings/go/blackstream/blackstream.go`:

```go
// BltpConfig configures the BLTP encrypted UDP adapter.
type BltpConfig struct {
    BindAddr          string `json:"bind_addr"`
    PeerAddr          string `json:"peer_addr"`
    PSK               string `json:"psk"`                 // hex-encoded 32 bytes
    Role              string `json:"role"`                // "initiator" or "responder"
    PeerPublicKey     string `json:"peer_public_key,omitempty"`  // hex, for initiator
    SecretKey         string `json:"secret_key,omitempty"`       // hex, for responder
    PublicKey         string `json:"public_key,omitempty"`       // hex, for responder
    Reliability       string `json:"reliability,omitempty"`      // "none", "light", "full"
    HeartbeatInterval int64  `json:"heartbeat_interval_ms,omitempty"`
    SessionTimeout    int64  `json:"session_timeout_ms,omitempty"`
    BatchedIO         bool   `json:"batched_io,omitempty"`
}

// BltpKeypair holds a generated keypair for BLTP.
type BltpKeypair struct {
    PublicKey string // hex-encoded
    SecretKey string // hex-encoded
}

type Config struct {
    // ... existing fields ...
    Bltp *BltpConfig `json:"bltp,omitempty"`
}
```

#### 4.2 Add Keypair Generation

```go
// #include "blackstream.h"
import "C"

// GenerateBltpKeypair generates a new X25519 keypair for BLTP.
func GenerateBltpKeypair() (*BltpKeypair, error) {
    result := C.bltp_generate_keypair()
    if result == nil {
        return nil, errors.New("failed to generate keypair")
    }
    defer C.blackstream_free_string(result)
    
    jsonStr := C.GoString(result)
    var keypair BltpKeypair
    if err := json.Unmarshal([]byte(jsonStr), &keypair); err != nil {
        return nil, err
    }
    return &keypair, nil
}
```

#### 4.3 Go Usage Example

```go
package main

import (
    "crypto/rand"
    "encoding/hex"
    "github.com/anthropic/blackstream-go"
)

func main() {
    // Generate keypair for responder
    keypair, _ := blackstream.GenerateBltpKeypair()
    
    // Generate PSK
    psk := make([]byte, 32)
    rand.Read(psk)
    pskHex := hex.EncodeToString(psk)
    
    // Responder
    responder, _ := blackstream.New(&blackstream.Config{
        NumShards: 2,
        Bltp: &blackstream.BltpConfig{
            BindAddr:    "127.0.0.1:9001",
            PeerAddr:    "127.0.0.1:9000",
            PSK:         pskHex,
            Role:        "responder",
            SecretKey:   keypair.SecretKey,
            PublicKey:   keypair.PublicKey,
            Reliability: "light",
        },
    })
    
    // Initiator
    initiator, _ := blackstream.New(&blackstream.Config{
        NumShards: 2,
        Bltp: &blackstream.BltpConfig{
            BindAddr:      "127.0.0.1:9000",
            PeerAddr:      "127.0.0.1:9001",
            PSK:           pskHex,
            Role:          "initiator",
            PeerPublicKey: keypair.PublicKey,
        },
    })
}
```

## File Changes Summary

| File | Changes |
|------|---------|
| `src/ffi/mod.rs` | Add `bltp_generate_keypair()`, `blackstream_free_string()`, BLTP config parsing |
| `src/adapter/bltp/crypto.rs` | Add `StaticKeypair::from_bytes()` if not present |
| `bindings/node/src/lib.rs` | Add `BltpOptions`, `BltpKeypair`, `generate_bltp_keypair()` |
| `bindings/node/index.d.ts` | Add TypeScript type definitions |
| `bindings/python/src/lib.rs` | Add `generate_bltp_keypair()`, `bltp_*` parameters |
| `bindings/go/blackstream/blackstream.go` | Add `BltpConfig`, `BltpKeypair`, `GenerateBltpKeypair()` |
| `Cargo.toml` | Ensure `hex` crate is included |

## Testing Strategy

### Unit Tests
- FFI keypair generation returns valid hex strings
- Config parsing handles all BLTP fields correctly
- Config parsing fails gracefully with invalid hex/addresses

### Integration Tests
Each binding should have tests that:
1. Generate a keypair
2. Create initiator + responder configs
3. Establish connection and exchange events
4. Verify reliable delivery mode works

### Example Test (Node.js)

```typescript
import { describe, it, expect } from 'vitest';
import { Blackstream, generateBltpKeypair } from '@anthropic/blackstream';

describe('BLTP', () => {
  it('should generate valid keypair', () => {
    const kp = generateBltpKeypair();
    expect(kp.publicKey).toHaveLength(64);  // 32 bytes hex
    expect(kp.secretKey).toHaveLength(64);
  });
  
  it('should exchange events over BLTP', async () => {
    const keypair = generateBltpKeypair();
    const psk = crypto.randomBytes(32).toString('hex');
    
    const responder = new Blackstream({
      bltp: {
        bindAddr: '127.0.0.1:19001',
        peerAddr: '127.0.0.1:19000',
        psk,
        role: 'responder',
        secretKey: keypair.secretKey,
        publicKey: keypair.publicKey,
      }
    });
    
    const initiator = new Blackstream({
      bltp: {
        bindAddr: '127.0.0.1:19000',
        peerAddr: '127.0.0.1:19001',
        psk,
        role: 'initiator',
        peerPublicKey: keypair.publicKey,
      }
    });
    
    initiator.ingest({ test: 'event' });
    await initiator.flush();
    
    // ... verify responder receives event
  });
});
```

## Security Considerations

1. **Secret Key Handling**: Secret keys are passed as hex strings through FFI. Language bindings should:
   - Not log secret keys
   - Zero memory after use where possible
   - Document that users should handle keys securely

2. **PSK Management**: PSKs must be:
   - 32 bytes exactly
   - Shared securely between initiator and responder
   - Not reused across deployments

3. **Key Rotation**: Document that:
   - New keypairs should be generated for new deployments
   - Compromised keys require regeneration and redeployment

## Dependencies

- `hex` crate for encoding/decoding (already in Cargo.toml)
- No additional external dependencies required
