//! §8's identity at rest: IndexedDB under a non-extractable
//! WebCrypto AES-GCM key, plus the leader generation counter.
//!
//! # The trust boundary, stated exactly
//!
//! What this protects: the scalars are not readable from IndexedDB in
//! the clear, and the wrapping key cannot be exported — it is created
//! with `extractable: false` and stored as a `CryptoKey` handle, so
//! `crypto.subtle.exportKey` on it rejects and a dump of the database
//! yields a ciphertext and an opaque key reference.
//!
//! What it does **not** protect: it does not make the scalars
//! non-extractable. WebCrypto decryption returns plaintext to the
//! calling context, and wasm is not a security boundary against
//! same-origin JavaScript — any script on the origin can open the same
//! database, get the same `CryptoKey`, and call `decrypt` with it. The
//! trust boundary is **the origin**. XSS on the origin owns the
//! identity. A host application that needs stronger custody injects a
//! keypair instead ([`crate::identity::IdentitySecrets::from_hex`],
//! the `MeshNodeConfig::entity_keypair` shape), which is why the
//! custodial path and this one produce the same value and share the
//! same single exit.
//!
//! # Layout
//!
//! One database, two object stores, both keyed out of line:
//!
//! | Store | Key | Value |
//! |---|---|---|
//! | `identity` | `"v1"` | `{ key: CryptoKey, iv: Uint8Array(12), blob: Uint8Array }` |
//! | `leader` | `"generation"` | the generation as a decimal **string** |
//!
//! The generation is a string because it is a `u64`: a JS number
//! rounds above 2^53, and a fence that compared rounded generations
//! would start admitting a stale leader exactly when the counter got
//! large. The same reason every `u64` crosses this crate's
//! JavaScript boundary as text.
//!
//! # Atomicity, and where a write becomes durable
//!
//! Two separate atomic operations, both of which two tabs race for:
//!
//! - **First-run identity creation.** The keypair and the AES-GCM key
//!   are generated *outside* any transaction (WebCrypto is a promise,
//!   and awaiting a non-IndexedDB promise inside a transaction commits
//!   it). The commit then happens in ONE `readwrite` transaction that
//!   re-reads the record first and keeps whatever is already there.
//!   Two tabs booting together therefore converge on one identity
//!   instead of overwriting each other into two node ids.
//! - **The generation.** Read, increment and write inside ONE
//!   `readwrite` transaction, per D2. IndexedDB serialises `readwrite`
//!   transactions over the same store, so two tabs acquiring the lock
//!   in sequence cannot observe the same value — which is the whole
//!   basis of the fence.
//!
//! Both then **await the transaction's `complete`**, and both
//! propagate its `abort`. A `put` request's `success` is not a
//! commit: it says the write was accepted into the transaction, and a
//! transaction can still abort afterwards — on quota, on a browser
//! eviction, on any unhandled request error, or because something
//! called `abort()`. Returning at request success is therefore
//! returning a generation that can roll back and be handed out twice,
//! which is a fence that admits two leaders, or an identity that was
//! never stored, which is a node id that changes on the next load.
//! Neither is a failure a caller can see after the fact, so the
//! commit is awaited before the value is returned.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AesGcmParams, AesKeyGenParams, CryptoKey, IdbDatabase, IdbObjectStore, IdbRequest,
    IdbTransaction, IdbTransactionMode,
};

use crate::error::{LeafError, Result};
use crate::identity::{IdentitySecrets, IDENTITY_BLOB_MAGIC};

/// The default database name. Overridable so a test can have its own
/// origin-scoped storage without evicting the page's.
pub const DEFAULT_DB_NAME: &str = "net-mesh-leaf";

/// The store holding the encrypted identity.
const STORE_IDENTITY: &str = "identity";

/// The store holding the leader generation.
const STORE_LEADER: &str = "leader";

/// The identity record's key.
const KEY_IDENTITY: &str = "v1";

/// The generation record's key.
const KEY_GENERATION: &str = "generation";

/// AES-GCM nonce length. 96 bits is the only IV length AES-GCM is
/// specified for without an extra derivation step.
const IV_LEN: usize = 12;

/// The identity store and the generation counter, on one open
/// database.
#[derive(Debug)]
pub struct IdentityVault {
    db: IdbDatabase,
}

impl IdentityVault {
    /// Open (creating on first use) the vault database.
    pub async fn open(db_name: &str) -> Result<Self> {
        let factory = window()?
            .indexed_db()
            .map_err(|e| storage_err("indexedDB is unavailable", &e))?
            .ok_or_else(|| {
                LeafError::Identity(
                    "this context has no IndexedDB, so an identity cannot be stored; \
                     inject one custodially instead"
                        .into(),
                )
            })?;

        let request = factory
            .open_with_u32(db_name, 1)
            .map_err(|e| storage_err("indexedDB.open", &e))?;

        // The upgrade handler is the only place stores may be
        // created, and it must do so synchronously.
        let upgrade_target = request.clone();
        let onupgrade = Closure::once(move |_e: web_sys::Event| {
            let Ok(result) = upgrade_target.result() else {
                return;
            };
            let Ok(db) = result.dyn_into::<IdbDatabase>() else {
                return;
            };
            for store in [STORE_IDENTITY, STORE_LEADER] {
                // An existing store is not an error: a second tab can
                // race the same upgrade.
                let _ = db.create_object_store(store);
            }
        });
        request.set_onupgradeneeded(Some(onupgrade.as_ref().unchecked_ref()));

        let value = awaited(request.clone().into()).await?;
        drop(onupgrade);

        let db = value
            .dyn_into::<IdbDatabase>()
            .map_err(|_| LeafError::Identity("indexedDB.open did not yield a database".into()))?;
        Ok(Self { db })
    }

    /// The stored identity, or a freshly generated one, stored.
    ///
    /// The one entry point for the storage half of the identity
    /// surface: it returns the same [`IdentitySecrets`] the custodial
    /// path returns, so everything downstream has exactly one shape
    /// to handle.
    pub async fn load_or_create(&self) -> Result<IdentitySecrets> {
        if let Some(secrets) = self.load().await? {
            return Ok(secrets);
        }

        // Generate outside any transaction: both of these are
        // promises that are not IndexedDB requests, and awaiting one
        // inside a transaction ends the transaction.
        let secrets = IdentitySecrets::generate()?;
        let record = self.seal(&secrets).await?;

        // Commit under a re-read, so a tab that got here first wins
        // and this one adopts its identity rather than replacing it.
        let tx = self
            .db
            .transaction_with_str_and_mode(STORE_IDENTITY, IdbTransactionMode::Readwrite)
            .map_err(|e| storage_err("readwrite transaction on identity", &e))?;
        let store = tx
            .object_store(STORE_IDENTITY)
            .map_err(|e| storage_err("identity store", &e))?;
        let existing = awaited(get(&store, KEY_IDENTITY)?).await?;
        if !existing.is_undefined() && !existing.is_null() {
            let theirs = self.unseal(&existing).await?;
            return Ok(theirs);
        }
        awaited(
            store
                .put_with_key(record.as_ref(), &JsValue::from_str(KEY_IDENTITY))
                .map_err(|e| storage_err("put identity", &e))?,
        )
        .await?;
        // The identity is not this node's until the transaction that
        // wrote it commits. Returning at the put's success would hand
        // back a keypair that a later abort erases — and the next
        // load would generate a different one, so the node id would
        // change under a page that had already announced it.
        committed(tx, "the identity was not committed").await?;
        Ok(secrets)
    }

    /// The stored identity, if there is one.
    pub async fn load(&self) -> Result<Option<IdentitySecrets>> {
        let tx = self
            .db
            .transaction_with_str(STORE_IDENTITY)
            .map_err(|e| storage_err("readonly transaction on identity", &e))?;
        let store = tx
            .object_store(STORE_IDENTITY)
            .map_err(|e| storage_err("identity store", &e))?;
        let value = awaited(get(&store, KEY_IDENTITY)?).await?;
        if value.is_undefined() || value.is_null() {
            return Ok(None);
        }
        Ok(Some(self.unseal(&value).await?))
    }

    /// Whether a record for the raw identity key exists — used by the
    /// wasm test to assert that what is on disk is a ciphertext and
    /// not the scalars.
    pub async fn raw_record(&self) -> Result<JsValue> {
        let tx = self
            .db
            .transaction_with_str(STORE_IDENTITY)
            .map_err(|e| storage_err("readonly transaction on identity", &e))?;
        let store = tx
            .object_store(STORE_IDENTITY)
            .map_err(|e| storage_err("identity store", &e))?;
        awaited(get(&store, KEY_IDENTITY)?).await
    }

    /// Read, increment, write **and commit** the generation inside
    /// **one** `readwrite` transaction, and return the new value.
    ///
    /// D2's acquisition step. Both requests are issued on the same
    /// transaction and the second is issued from the first's
    /// completion, which is what keeps the transaction alive across
    /// them; nothing that is not an IndexedDB request is awaited in
    /// between. The commit is then awaited, because the whole basis
    /// of the fence is that no two acquisitions see the same number,
    /// and a generation returned at the put's success can still roll
    /// back and be handed out a second time.
    pub async fn next_generation(&self) -> Result<u64> {
        self.next_generation_observed(|_| {}).await
    }

    /// [`Self::next_generation`], with the live transaction handed to
    /// `inspect` after the put has been accepted and **before** the
    /// commit.
    ///
    /// The seam exists because that window is exactly where the
    /// interesting failure lives and nothing else can reach it: the
    /// production path passes a closure that does nothing, so this
    /// *is* the production path, and a witness passes one that calls
    /// `abort()` — standing in for the quota failure, the eviction or
    /// the foreign `abort()` that would land in the same place on a
    /// real page. A test that opened its own transaction instead
    /// would be testing its own code.
    pub async fn next_generation_observed(
        &self,
        inspect: impl FnOnce(&IdbTransaction),
    ) -> Result<u64> {
        let tx = self
            .db
            .transaction_with_str_and_mode(STORE_LEADER, IdbTransactionMode::Readwrite)
            .map_err(|e| storage_err("readwrite transaction on leader", &e))?;
        let store = tx
            .object_store(STORE_LEADER)
            .map_err(|e| storage_err("leader store", &e))?;

        let current = decode_generation(&awaited(get(&store, KEY_GENERATION)?).await?)?;
        let next = current.checked_add(1).ok_or_else(|| {
            LeafError::Identity("the leader generation counter is exhausted".into())
        })?;
        awaited(
            store
                .put_with_key(
                    &JsValue::from_str(&next.to_string()),
                    &JsValue::from_str(KEY_GENERATION),
                )
                .map_err(|e| storage_err("put generation", &e))?,
        )
        .await?;
        inspect(&tx);
        committed(tx, "the leader generation was not committed").await?;
        Ok(next)
    }

    /// The generation currently recorded. `0` before any leader has
    /// ever held the lock.
    pub async fn current_generation(&self) -> Result<u64> {
        let tx = self
            .db
            .transaction_with_str(STORE_LEADER)
            .map_err(|e| storage_err("readonly transaction on leader", &e))?;
        let store = tx
            .object_store(STORE_LEADER)
            .map_err(|e| storage_err("leader store", &e))?;
        decode_generation(&awaited(get(&store, KEY_GENERATION)?).await?)
    }

    /// The storage half of the fence: refuse to act for a generation
    /// that is not the one the store records.
    ///
    /// D2's "every follower **and the storage layer** reject" — a
    /// suspended tab that resumes still believes it holds generation
    /// *n* while the store has moved to *n+1*, and this is the check
    /// that tells it so, typed.
    pub async fn fence(&self, generation: u64) -> Result<()> {
        let current = self.current_generation().await?;
        if current == generation {
            return Ok(());
        }
        Err(LeafError::NotLeader {
            presented: generation,
            current: Some(current),
        })
    }

    /// Encrypt `secrets` into the stored record shape.
    async fn seal(&self, secrets: &IdentitySecrets) -> Result<Object> {
        let subtle = subtle()?;

        let usages = Array::new();
        usages.push(&JsValue::from_str("encrypt"));
        usages.push(&JsValue::from_str("decrypt"));
        // `extractable: false` is the whole claim this module makes.
        let key: CryptoKey = JsFuture::from(
            subtle
                .generate_key_with_object(
                    AesKeyGenParams::new("AES-GCM", 256).as_ref(),
                    false,
                    usages.as_ref(),
                )
                .map_err(|e| storage_err("generateKey", &e))?,
        )
        .await
        .map_err(|e| storage_err("generateKey", &e))?
        .dyn_into()
        .map_err(|_| LeafError::Identity("generateKey did not yield a CryptoKey".into()))?;

        let mut iv = [0u8; IV_LEN];
        getrandom::fill(&mut iv)
            .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
        let iv_array = Uint8Array::from(&iv[..]);

        let mut plaintext = secrets.encode();
        let cipher = JsFuture::from(
            subtle
                .encrypt_with_object_and_u8_array(gcm_params(&iv_array).as_ref(), &key, &plaintext)
                .map_err(|e| storage_err("encrypt", &e))?,
        )
        .await
        .map_err(|e| storage_err("encrypt", &e))?;
        plaintext.zeroize_in_place();

        let record = Object::new();
        set(&record, "key", &key)?;
        set(&record, "iv", &iv_array)?;
        set(&record, "blob", &Uint8Array::new(&cipher))?;
        Ok(record)
    }

    /// Decrypt a stored record.
    async fn unseal(&self, record: &JsValue) -> Result<IdentitySecrets> {
        let key: CryptoKey = Reflect::get(record, &JsValue::from_str("key"))
            .map_err(|e| storage_err("stored record has no key", &e))?
            .dyn_into()
            .map_err(|_| {
                LeafError::Identity("the stored wrapping key is not a CryptoKey".into())
            })?;
        let iv: Uint8Array = Reflect::get(record, &JsValue::from_str("iv"))
            .map_err(|e| storage_err("stored record has no iv", &e))?
            .dyn_into()
            .map_err(|_| LeafError::Identity("the stored iv is not a Uint8Array".into()))?;
        let blob: Uint8Array = Reflect::get(record, &JsValue::from_str("blob"))
            .map_err(|e| storage_err("stored record has no blob", &e))?
            .dyn_into()
            .map_err(|_| LeafError::Identity("the stored blob is not a Uint8Array".into()))?;

        let cipher = blob.to_vec();
        let plain = JsFuture::from(
            subtle()?
                .decrypt_with_object_and_u8_array(gcm_params(&iv).as_ref(), &key, &cipher)
                .map_err(|e| storage_err("decrypt", &e))?,
        )
        .await
        .map_err(|e| {
            // A GCM tag failure is the honest symptom of a record
            // written under a different key or tampered with.
            storage_err(
                "the stored identity did not decrypt (wrong key, or the record was altered)",
                &e,
            )
        })?;

        let mut bytes = Uint8Array::new(&plain).to_vec();
        let secrets = IdentitySecrets::decode(&bytes);
        bytes.zeroize_in_place();
        secrets
    }

    /// Drop both records. The test path, and the honest answer to "how
    /// does a page forget an identity".
    ///
    /// Committed, for the same reason the writes are: a caller that
    /// was told the identity was forgotten and then finds it back
    /// after a reload was not told the truth.
    pub async fn clear(&self) -> Result<()> {
        for store_name in [STORE_IDENTITY, STORE_LEADER] {
            let tx = self
                .db
                .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readwrite)
                .map_err(|e| storage_err("readwrite transaction", &e))?;
            let store = tx
                .object_store(store_name)
                .map_err(|e| storage_err("object store", &e))?;
            awaited(store.clear().map_err(|e| storage_err("clear", &e))?).await?;
            committed(tx, "the records were not cleared").await?;
        }
        Ok(())
    }

    /// Close the database handle.
    pub fn close(&self) {
        self.db.close();
    }
}

/// Overwrite a plaintext buffer in place.
///
/// A trait rather than a free function so the call sites read as the
/// thing they are doing to the buffer they own.
trait ZeroizeInPlace {
    fn zeroize_in_place(&mut self);
}

impl ZeroizeInPlace for Vec<u8> {
    fn zeroize_in_place(&mut self) {
        use zeroize::Zeroize;
        self.zeroize();
    }
}

/// AES-GCM parameters bound to the identity schema.
///
/// The additional data is the blob magic, so a ciphertext written for
/// some other record under the same key cannot be decrypted as an
/// identity — GCM authenticates the AAD, so a mismatch is a tag
/// failure rather than a misread.
fn gcm_params(iv: &Uint8Array) -> AesGcmParams {
    let params = AesGcmParams::new_with_u8_array("AES-GCM", iv);
    params.set_additional_data_u8_array(&Uint8Array::from(IDENTITY_BLOB_MAGIC));
    params
}

/// `store.get(key)`, with the request typed.
fn get(store: &IdbObjectStore, key: &str) -> Result<IdbRequest> {
    store
        .get(&JsValue::from_str(key))
        .map_err(|e| storage_err("get", &e))
}

/// A generation from its stored decimal string. Absent means zero: no
/// leader has ever held the lock.
fn decode_generation(value: &JsValue) -> Result<u64> {
    if value.is_undefined() || value.is_null() {
        return Ok(0);
    }
    let text = value.as_string().ok_or_else(|| {
        LeafError::Identity("the stored generation is not a string; refusing to guess".into())
    })?;
    text.parse()
        .map_err(|_| LeafError::Identity(format!("the stored generation {text:?} is not a u64")))
}

/// Await one `IdbRequest` and yield its `result`.
///
/// Both handlers are installed before the await and dropped after it,
/// which is what keeps the closures alive for exactly as long as the
/// browser may call them.
async fn awaited(request: IdbRequest) -> Result<JsValue> {
    let (tx, rx) = oneshot::channel::<Result<JsValue>>();
    let slot = Rc::new(RefCell::new(Some(tx)));

    let success_req = request.clone();
    let success_slot = slot.clone();
    let onsuccess = Closure::once(move |_e: web_sys::Event| {
        if let Some(tx) = success_slot.borrow_mut().take() {
            let _ = tx.send(
                success_req
                    .result()
                    .map_err(|e| storage_err("request result", &e)),
            );
        }
    });

    let error_req = request.clone();
    let error_slot = slot;
    let onerror = Closure::once(move |_e: web_sys::Event| {
        if let Some(tx) = error_slot.borrow_mut().take() {
            let detail = error_req
                .error()
                .ok()
                .flatten()
                .map(|e| e.message())
                .unwrap_or_else(|| "unknown".into());
            let _ = tx.send(Err(LeafError::Identity(format!(
                "IndexedDB request failed: {detail}"
            ))));
        }
    });

    request.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
    request.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let outcome = rx.await.unwrap_or_else(|_| {
        Err(LeafError::Identity(
            "the IndexedDB request was dropped before it settled".into(),
        ))
    });

    request.set_onsuccess(None);
    request.set_onerror(None);
    drop(onsuccess);
    drop(onerror);
    outcome
}

/// Await one transaction's commit, and report an abort as a typed
/// failure.
///
/// The boundary a `put` request's `success` is not. Three handlers,
/// because IndexedDB has three ways to end a transaction and only one
/// of them is durable: `complete` is the commit, `abort` is the
/// rollback (quota, an unhandled request error, or an explicit
/// `abort()`), and `error` is a request failure that will be followed
/// by the abort. Whichever fires first settles, and the other two find
/// the slot taken.
async fn committed(tx: IdbTransaction, what: &str) -> Result<()> {
    let (sender, receiver) = oneshot::channel::<Result<()>>();
    let slot = Rc::new(RefCell::new(Some(sender)));

    let done = slot.clone();
    let oncomplete = Closure::once(move |_e: web_sys::Event| {
        if let Some(sender) = done.borrow_mut().take() {
            let _ = sender.send(Ok(()));
        }
    });

    let aborted_tx = tx.clone();
    let aborted = slot.clone();
    let reason = what.to_string();
    let onabort = Closure::once(move |_e: web_sys::Event| {
        if let Some(sender) = aborted.borrow_mut().take() {
            let detail = aborted_tx
                .error()
                .map(|e| e.message())
                .unwrap_or_else(|| "the transaction was aborted".into());
            let _ = sender.send(Err(LeafError::Identity(format!("{reason}: {detail}"))));
        }
    });

    let failed_tx = tx.clone();
    let failed = slot;
    let failure = what.to_string();
    let onerror = Closure::once(move |_e: web_sys::Event| {
        if let Some(sender) = failed.borrow_mut().take() {
            let detail = failed_tx
                .error()
                .map(|e| e.message())
                .unwrap_or_else(|| "the transaction failed".into());
            let _ = sender.send(Err(LeafError::Identity(format!("{failure}: {detail}"))));
        }
    });

    tx.set_oncomplete(Some(oncomplete.as_ref().unchecked_ref()));
    tx.set_onabort(Some(onabort.as_ref().unchecked_ref()));
    tx.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let outcome = receiver.await.unwrap_or_else(|_| {
        Err(LeafError::Identity(
            "the IndexedDB transaction was dropped before it settled".into(),
        ))
    });

    tx.set_oncomplete(None);
    tx.set_onabort(None);
    tx.set_onerror(None);
    drop(oncomplete);
    drop(onabort);
    drop(onerror);
    outcome
}

fn set(object: &Object, key: &str, value: &impl AsRef<JsValue>) -> Result<()> {
    Reflect::set(object, &JsValue::from_str(key), value.as_ref())
        .map(|_| ())
        .map_err(|e| storage_err("building the stored record", &e))
}

fn window() -> Result<web_sys::Window> {
    web_sys::window().ok_or_else(|| {
        LeafError::Identity("no `window`: the leaf runs on the main thread (S0b)".into())
    })
}

fn subtle() -> Result<web_sys::SubtleCrypto> {
    Ok(window()?
        .crypto()
        .map_err(|e| storage_err("window.crypto", &e))?
        .subtle())
}

/// A `JsValue` failure as a typed identity failure, with the browser's
/// own words kept.
fn storage_err(what: &str, error: &JsValue) -> LeafError {
    let detail = error
        .as_string()
        .or_else(|| {
            error
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{error:?}"));
    LeafError::Identity(format!("{what}: {detail}"))
}
