//! The settlement signer seam — doctrine 4/7/8 made into an interface.
//!
//! Identity keys ≠ settlement keys. Settlement keys live in the user's
//! wallet / KMS / MPC / licensed provider; Net stores references and
//! policy. This trait is the whole boundary:
//!
//! - **Typed operations in, signatures out.** The one method takes the
//!   standard `eth_signTypedData_v4` document — domain, types, and the
//!   full message — so a policy-bearing signer can inspect the amount
//!   and recipient it is authorizing. **There is no raw-bytes signing
//!   method, and that absence is the invariant** ("no arbitrary signing
//!   oracle"): a prompt-injected agent can at worst ask for a signature
//!   on a logged, typed transfer authorization.
//! - Keys never cross this boundary in either direction. The
//!   [`ExternalSigner`] path hands the document out (KMS/wallet/MPC
//!   compute the digest and sign; the key never enters Net memory).
//!   The [`DevLocalSigner`] exists only behind the loud
//!   `unsafe-dev-signer` feature, for testnet conformance.

use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use serde_json::Value;

/// Signing failure — terminal for the payment attempt (the flow
/// surfaces it as a structured failure; nothing retries a signature).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("settlement signer: {message}")]
pub struct SignerError {
    pub message: String,
}

impl SignerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The settlement signer for scheme payload authoring.
#[async_trait]
pub trait SchemeSigner: Send + Sync {
    /// The payer address this signer controls (`0x…` for eip155,
    /// base58 for solana).
    fn address(&self) -> String;

    /// Sign an EIP-712 typed-data document (`eth_signTypedData_v4`
    /// shape). Returns the 65-byte `r‖s‖v` signature as `0x…` hex.
    async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError>;

    /// Author a **partially-signed** SVM transaction for a typed
    /// transfer intent (exact-SVM: SPL `TransferChecked`, fee payer =
    /// `intent.fee_payer`, recent blockhash fetched by the wallet).
    /// Returns the base64-serialized versioned transaction. The intent
    /// is the whole document — same doctrine as typed data: a
    /// policy-bearing wallet inspects the amount, mint, and recipient
    /// it is authorizing; there is no raw-bytes path.
    ///
    /// Defaulted to a structured refusal: an EVM signer registered
    /// under the wrong namespace fails closed instead of authoring
    /// something it does not understand.
    async fn sign_svm_transfer(
        &self,
        intent: &crate::x402::schemes::exact_svm::SvmTransferIntent,
    ) -> Result<String, SignerError> {
        let _ = intent;
        Err(SignerError::new(
            "this signer does not author solana transactions",
        ))
    }
}

type SignFuture = Pin<Box<dyn Future<Output = Result<String, SignerError>> + Send>>;

/// The production shape: an externally-held key. The host supplies a
/// callback that forwards the typed-data document to its KMS / wallet /
/// MPC provider and returns the signature — the key never enters Net
/// memory, and Net never learns anything but the address and the
/// signatures it asked for.
pub struct ExternalSigner {
    address: String,
    sign: Box<dyn Fn(Value) -> SignFuture + Send + Sync>,
}

impl ExternalSigner {
    pub fn new(
        address: impl Into<String>,
        sign: impl Fn(Value) -> SignFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            address: address.into(),
            sign: Box::new(sign),
        }
    }
}

#[async_trait]
impl SchemeSigner for ExternalSigner {
    fn address(&self) -> String {
        self.address.clone()
    }

    async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError> {
        (self.sign)(typed_data.clone()).await
    }
}

/// The externally-held SVM wallet: the host's callback receives the
/// structured [`SvmTransferIntent`](crate::x402::schemes::exact_svm::SvmTransferIntent)
/// and returns the base64 partially-signed versioned transaction. The
/// wallet owns the key, the SPL transaction machinery, and the RPC
/// connection for the recent blockhash — none of which enter Net.
/// Registered under the `solana` namespace; its EVM method is a
/// structured refusal by construction.
pub struct ExternalSvmSigner {
    address: String,
    sign:
        Box<dyn Fn(crate::x402::schemes::exact_svm::SvmTransferIntent) -> SignFuture + Send + Sync>,
}

impl ExternalSvmSigner {
    pub fn new(
        address: impl Into<String>,
        sign: impl Fn(crate::x402::schemes::exact_svm::SvmTransferIntent) -> SignFuture
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            address: address.into(),
            sign: Box::new(sign),
        }
    }
}

#[async_trait]
impl SchemeSigner for ExternalSvmSigner {
    fn address(&self) -> String {
        self.address.clone()
    }

    async fn sign_typed_data(&self, _typed_data: &Value) -> Result<String, SignerError> {
        Err(SignerError::new(
            "this signer authors solana transactions, not EIP-712 documents",
        ))
    }

    async fn sign_svm_transfer(
        &self,
        intent: &crate::x402::schemes::exact_svm::SvmTransferIntent,
    ) -> Result<String, SignerError> {
        (self.sign)(intent.clone()).await
    }
}

/// DEV/TESTNET ONLY — a local secp256k1 key signing EIP-712 digests in
/// process. Exists for conformance runs against testnet facilitators;
/// the feature name is the warning, and production builds must never
/// enable it.
#[cfg(feature = "unsafe-dev-signer")]
pub mod dev {
    use super::{SchemeSigner, SignerError};
    use async_trait::async_trait;
    use k256::ecdsa::SigningKey;
    use serde_json::Value;
    use sha3::{Digest, Keccak256};

    fn keccak(data: &[u8]) -> [u8; 32] {
        let mut hasher = Keccak256::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    fn decode_address(s: &str) -> Result<[u8; 20], SignerError> {
        let hex_part = s.strip_prefix("0x").unwrap_or(s);
        let bytes =
            hex::decode(hex_part).map_err(|e| SignerError::new(format!("address hex: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| SignerError::new("address must be 20 bytes"))
    }

    fn decode_bytes32(s: &str) -> Result<[u8; 32], SignerError> {
        let hex_part = s.strip_prefix("0x").unwrap_or(s);
        let bytes =
            hex::decode(hex_part).map_err(|e| SignerError::new(format!("bytes32 hex: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| SignerError::new("nonce must be 32 bytes"))
    }

    fn word_u64(v: u64) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[24..].copy_from_slice(&v.to_be_bytes());
        word
    }

    fn word_u128_str(s: &str) -> Result<[u8; 32], SignerError> {
        let v: u128 = s
            .parse()
            .map_err(|e| SignerError::new(format!("uint256 `{s}`: {e}")))?;
        let mut word = [0u8; 32];
        word[16..].copy_from_slice(&v.to_be_bytes());
        Ok(word)
    }

    fn word_addr(addr: &[u8; 20]) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(addr);
        word
    }

    /// The local-key dev signer. Understands exactly the
    /// `TransferWithAuthorization` document `exact_evm::typed_data`
    /// builds — it is a conformance tool, not a general EIP-712 wallet.
    pub struct DevLocalSigner {
        key: SigningKey,
        address: String,
    }

    impl DevLocalSigner {
        /// Build from a 32-byte secp256k1 secret. Testnet keys only.
        pub fn from_secret(secret: [u8; 32]) -> Result<Self, SignerError> {
            let key = SigningKey::from_bytes(&secret.into())
                .map_err(|e| SignerError::new(format!("secret key: {e}")))?;
            let address = Self::derive_address(&key);
            Ok(Self { key, address })
        }

        fn derive_address(key: &SigningKey) -> String {
            let pubkey = key.verifying_key().to_encoded_point(false);
            let hash = keccak(&pubkey.as_bytes()[1..]);
            format!("0x{}", hex::encode(&hash[12..]))
        }

        /// The EIP-712 digest this signer signs — public so conformance
        /// tests can independently recover the signature.
        pub fn eip712_digest(typed_data: &Value) -> Result<[u8; 32], SignerError> {
            Self::digest(typed_data)
        }

        /// EIP-712 digest for the TransferWithAuthorization document.
        fn digest(typed_data: &Value) -> Result<[u8; 32], SignerError> {
            let get = |path: &[&str]| -> Result<&str, SignerError> {
                let mut v = typed_data;
                for key in path {
                    v = v
                        .get(key)
                        .ok_or_else(|| SignerError::new(format!("typed data missing {path:?}")))?;
                }
                v.as_str()
                    .ok_or_else(|| SignerError::new(format!("typed data {path:?} not a string")))
            };
            if typed_data["primaryType"] != "TransferWithAuthorization" {
                return Err(SignerError::new(
                    "dev signer only signs TransferWithAuthorization documents",
                ));
            }

            let domain_typehash = keccak(
                b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
            );
            let chain_id = typed_data["domain"]["chainId"]
                .as_u64()
                .ok_or_else(|| SignerError::new("domain.chainId not a u64"))?;
            let mut domain_enc = Vec::with_capacity(5 * 32);
            domain_enc.extend_from_slice(&domain_typehash);
            domain_enc.extend_from_slice(&keccak(get(&["domain", "name"])?.as_bytes()));
            domain_enc.extend_from_slice(&keccak(get(&["domain", "version"])?.as_bytes()));
            domain_enc.extend_from_slice(&word_u64(chain_id));
            domain_enc.extend_from_slice(&word_addr(&decode_address(get(&[
                "domain",
                "verifyingContract",
            ])?)?));
            let domain_separator = keccak(&domain_enc);

            let transfer_typehash = keccak(
                b"TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)",
            );
            let mut struct_enc = Vec::with_capacity(7 * 32);
            struct_enc.extend_from_slice(&transfer_typehash);
            struct_enc.extend_from_slice(&word_addr(&decode_address(get(&["message", "from"])?)?));
            struct_enc.extend_from_slice(&word_addr(&decode_address(get(&["message", "to"])?)?));
            struct_enc.extend_from_slice(&word_u128_str(get(&["message", "value"])?)?);
            struct_enc.extend_from_slice(&word_u128_str(get(&["message", "validAfter"])?)?);
            struct_enc.extend_from_slice(&word_u128_str(get(&["message", "validBefore"])?)?);
            struct_enc.extend_from_slice(&decode_bytes32(get(&["message", "nonce"])?)?);
            let struct_hash = keccak(&struct_enc);

            let mut preimage = Vec::with_capacity(2 + 64);
            preimage.extend_from_slice(b"\x19\x01");
            preimage.extend_from_slice(&domain_separator);
            preimage.extend_from_slice(&struct_hash);
            Ok(keccak(&preimage))
        }
    }

    #[async_trait]
    impl SchemeSigner for DevLocalSigner {
        fn address(&self) -> String {
            self.address.clone()
        }

        async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError> {
            let digest = Self::digest(typed_data)?;
            let (signature, recovery) = self
                .key
                .sign_prehash_recoverable(&digest)
                .map_err(|e| SignerError::new(format!("sign: {e}")))?;
            let mut out = Vec::with_capacity(65);
            out.extend_from_slice(&signature.to_bytes());
            // EIP-3009 contracts expect the legacy 27/28 recovery byte.
            out.push(27 + recovery.to_byte());
            Ok(format!("0x{}", hex::encode(out)))
        }
    }
}
