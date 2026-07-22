//! OSDK §1 — [`OrgCredentials`]: the closed credential collection the facade
//! binds to a mesh.
//!
//! # The three-stage validity contract
//!
//! ```text
//! OrgCredentials::new   structural relationships and signatures ONLY
//! Mesh::org             identity/authority relation + operational
//!                       installability of DISCOVER audiences
//! OrgClient::call       per-call temporal recheck of every credential used
//! ```
//!
//! Construction deliberately does NOT check validity windows: credentials are
//! routinely assembled before the window they will be used in. Binding checks
//! what must be true to operate NOW (the node's identity relation, and whether
//! each DISCOVER audience can actually be installed). Calls recheck windows
//! because a long-lived client crosses expiry.
//!
//! # No keypair, no mutation
//!
//! The collection holds no signing key — the mesh's configured durable identity
//! signs. It is closed at construction: there is no `install_grant` /
//! `install_audience_secret`. Changing credentials means constructing a new
//! `OrgCredentials` and binding again.

use net::adapter::net::identity::EntityId;

use super::error::{hex32, OrgCredentialError};
use super::types::{
    OrgAudienceSecret, OrgCapabilityGrant, OrgDispatcherGrant, OrgId, OrgMembershipCert,
};

/// A validated organization credential set: who you belong to, what you may
/// dispatch, and which cross-org grants you hold.
///
/// Construction verifies every signature and every structural relationship the
/// provider's admission engine will later re-verify remotely, so a set that
/// builds is one whose *shape* cannot be refused — what remains is authority
/// the provider owns (floors, live policy) and time.
///
/// Deliberately not `Clone`: it owns [`OrgAudienceSecret`]s, which are
/// non-serializable and zeroized on drop. Deliberately not `Serialize` /
/// `Deserialize` — asserted at compile time below.
pub struct OrgCredentials {
    membership: OrgMembershipCert,
    dispatcher: OrgDispatcherGrant,
    grants: Vec<OrgCapabilityGrant>,
    audience_secrets: Vec<OrgAudienceSecret>,
}

/// Type-level assertion mirroring [`OrgAudienceSecret`]'s: the credential
/// container must never gain `Serialize`. If it ever does, the blanket impl
/// below becomes ambiguous with the `()` impl and this constant fails to
/// compile. Secrets must not acquire a serialization path by being wrapped.
const _: fn() = || {
    trait AmbiguousIfSerialize<A> {
        fn guard() {}
    }
    impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
    #[allow(dead_code)]
    struct IsSerialize;
    impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<IsSerialize> for T {}
    let _ = <OrgCredentials as AmbiguousIfSerialize<_>>::guard;
};

impl OrgCredentials {
    /// Validate and assemble a credential set.
    ///
    /// Checks, in order — each mirrors a relation `verify_org_admission` will
    /// re-check remotely, so a local refusal here is a call the provider was
    /// certain to deny:
    ///
    /// 1. the membership and dispatcher grant signatures verify;
    /// 2. the dispatcher grant empowers the entity the membership vouches for
    ///    (admission step 7);
    /// 3. both name the same acting org (admission step 5);
    /// 4. every capability grant's signature verifies (a reserved zero grant id
    ///    fails here, inside `verify`);
    /// 5. every capability grant names the acting org as grantee — a wallet
    ///    holds only grants issued TO its own org;
    /// 6. no two grants share a grant id;
    /// 7. every audience secret is the out-of-band key of exactly one held
    ///    grant (`matches_grant`: grant id AND key commitment).
    ///
    /// Validity windows are NOT checked — see the module docs.
    pub fn new(
        membership: OrgMembershipCert,
        dispatcher: OrgDispatcherGrant,
        grants: Vec<OrgCapabilityGrant>,
        audience_secrets: Vec<OrgAudienceSecret>,
    ) -> Result<Self, OrgCredentialError> {
        membership
            .verify()
            .map_err(|source| OrgCredentialError::SignatureInvalid {
                credential: "membership".to_string(),
                source,
            })?;
        dispatcher
            .verify()
            .map_err(|source| OrgCredentialError::SignatureInvalid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;

        if dispatcher.dispatcher != membership.member {
            return Err(OrgCredentialError::DispatcherBindingMismatch {
                dispatcher: dispatcher.dispatcher.clone(),
                member: membership.member.clone(),
            });
        }
        if dispatcher.org_id != membership.org_id {
            return Err(OrgCredentialError::ActingOrgMismatch {
                membership_org: membership.org_id,
                dispatcher_org: dispatcher.org_id,
            });
        }

        for grant in &grants {
            grant
                .verify()
                .map_err(|source| OrgCredentialError::SignatureInvalid {
                    credential: format!("capability grant {}", hex32(&grant.grant_id)),
                    source,
                })?;
            if grant.grantee_org != membership.org_id {
                return Err(OrgCredentialError::GrantNotForActingOrg {
                    grant_id: hex32(&grant.grant_id),
                    grantee_org: grant.grantee_org,
                });
            }
        }
        for (i, grant) in grants.iter().enumerate() {
            if grants[..i].iter().any(|g| g.grant_id == grant.grant_id) {
                return Err(OrgCredentialError::DuplicateGrant {
                    grant_id: hex32(&grant.grant_id),
                });
            }
        }

        // Every secret must be the key of exactly one held grant. Grant ids are
        // unique (checked above), so "matches at least one" is "matches exactly
        // one"; `matches_grant` compares the grant id AND the key commitment,
        // so a stale secret for a re-issued grant is refused here rather than
        // silently failing to decrypt later.
        for secret in &audience_secrets {
            if !grants.iter().any(|g| secret.matches_grant(g)) {
                return Err(OrgCredentialError::AudienceSecretMismatch {
                    grant_id: hex32(&secret.grant_id),
                });
            }
        }

        Ok(Self {
            membership,
            dispatcher,
            grants,
            audience_secrets,
        })
    }

    /// Assemble from canonical wire bytes and audience-secret **file paths**
    /// (OSDK-L R2) — the constructor every language binding uses.
    ///
    /// # Why the asymmetry
    ///
    /// The three signed credentials are public objects designed to transit, so
    /// they cross an FFI boundary as their canonical wire encodings. The
    /// audience secret is the raw discovery key: handing it to a
    /// garbage-collected runtime as a buffer would put it in memory that is
    /// never zeroized, freely copied by the collector, and visible in a heap
    /// dump — undoing at the last hop what the substrate protects everywhere
    /// else. So a binding supplies a PATH, and the key's whole lifetime stays
    /// in Rust: loaded by
    /// [`load_grant_audience_secret`](net::adapter::net::behavior::org_authority::load_grant_audience_secret),
    /// which validates the opened object, reads into scrub-on-drop storage, and
    /// never returns the bytes to anyone.
    ///
    /// There is deliberately **no bytes variant of this constructor**, in Rust
    /// or in any binding. Adding one would reopen the language-SDK plan's first
    /// locked decision.
    pub fn from_parts(
        membership: &[u8],
        dispatcher: &[u8],
        grants: &[Vec<u8>],
        audience_secret_paths: &[std::path::PathBuf],
    ) -> Result<Self, OrgCredentialError> {
        let membership = OrgMembershipCert::from_bytes(membership).map_err(|source| {
            OrgCredentialError::SignatureInvalid {
                credential: "membership".to_string(),
                source,
            }
        })?;
        let dispatcher = OrgDispatcherGrant::from_bytes(dispatcher).map_err(|source| {
            OrgCredentialError::SignatureInvalid {
                credential: "dispatcher grant".to_string(),
                source,
            }
        })?;
        let mut decoded_grants = Vec::with_capacity(grants.len());
        for (i, raw) in grants.iter().enumerate() {
            decoded_grants.push(OrgCapabilityGrant::from_bytes(raw).map_err(|source| {
                OrgCredentialError::SignatureInvalid {
                    credential: format!("capability grant #{i}"),
                    source,
                }
            })?);
        }

        let mut secrets = Vec::with_capacity(audience_secret_paths.len());
        for path in audience_secret_paths {
            secrets.push(
                net::adapter::net::behavior::org_authority::load_grant_audience_secret(path)
                    .map_err(|e| OrgCredentialError::AudienceSecretFile {
                        path: path.display().to_string(),
                        detail: e.to_string(),
                    })?,
            );
        }

        // Everything after this is the same validation an in-process caller
        // gets — the loading path adds no authority and skips no check.
        Self::new(membership, dispatcher, decoded_grants, secrets)
    }

    /// The organization this actor acts for (named by the membership; the
    /// dispatcher grant agrees, checked at construction).
    pub fn acting_org(&self) -> OrgId {
        self.membership.org_id
    }

    /// The entity the membership vouches for — must equal the binding mesh's
    /// identity.
    pub fn member(&self) -> &EntityId {
        &self.membership.member
    }

    /// The membership certificate.
    pub fn membership(&self) -> &OrgMembershipCert {
        &self.membership
    }

    /// The dispatcher grant.
    pub fn dispatcher(&self) -> &OrgDispatcherGrant {
        &self.dispatcher
    }

    /// The held cross-org capability grants.
    pub fn grants(&self) -> &[OrgCapabilityGrant] {
        &self.grants
    }

    /// Split into parts for binding. Consuming is deliberate: the audience
    /// secrets move into the node's consumer registry, which is the only place
    /// they are needed (they open inbound envelopes; they never ride a call
    /// proof).
    pub(crate) fn into_parts(
        self,
    ) -> (
        OrgMembershipCert,
        OrgDispatcherGrant,
        Vec<OrgCapabilityGrant>,
        Vec<OrgAudienceSecret>,
    ) {
        (
            self.membership,
            self.dispatcher,
            self.grants,
            self.audience_secrets,
        )
    }
}

impl std::fmt::Debug for OrgCredentials {
    /// Redacted: counts and public ids only. The contained
    /// [`OrgAudienceSecret`]s redact their own key material, but the container
    /// does not invite a reader to print them at all.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgCredentials")
            .field("acting_org", &self.membership.org_id)
            .field("member", &self.membership.member)
            .field("grants", &self.grants.len())
            .field("audience_secrets", &self.audience_secrets.len())
            .finish()
    }
}
