// SPDX-License-Identifier: MIT OR Apache-2.0
//! Organization membership in enrollment (NET_CLI_PLAN_V3 V3-2, org half).
//!
//! An invite with [`Relation::Org`] always requires operator approval: only
//! the offline org root can sign an `OrgMembershipCert`, and it must be for
//! the exact device that claimed the invite. The operator signs at approval,
//! off the node, and hands the certificate to the issuing node, which keeps
//! it in an [`OrgCertStash`] keyed by that exact claim and delivers it when
//! the device redeems ([`super::bundle::MembershipIssuer::with_org_certs`]).
//!
//! [`Relation::Org`]: super::invite::Relation::Org

use std::io::Write as _;
use std::path::{Path, PathBuf};

use net::adapter::net::behavior::org::OrgMembershipCert;

use super::bundle::OrgCertSource;
use super::store::Claimant;

/// Durable, per-claim store of operator-approved org membership
/// certificates. Certificates are not secret; file names are one-way
/// digests of the claim.
#[derive(Clone, Debug)]
pub struct OrgCertStash {
    dir: PathBuf,
}

impl OrgCertStash {
    /// Use `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, claimant: &Claimant) -> PathBuf {
        let mut h = blake3::Hasher::new_derive_key("net-mesh org cert stash v1");
        h.update(claimant.subject.as_bytes());
        h.update(&claimant.intent_digest);
        let key = h.finalize();
        self.dir
            .join(format!("{}.cert", super::hex_lower(&key.as_bytes()[..16])))
    }

    /// Record `cert` for `claimant`. Refuses a certificate that is not for
    /// that claimant's device or does not verify under its own org root.
    /// Durable before returning (written, synced, then renamed into place).
    pub fn put(&self, claimant: &Claimant, cert: &OrgMembershipCert) -> std::io::Result<()> {
        if cert.member != claimant.subject || cert.verify().is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the certificate is not a valid membership for this claim's device",
            ));
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(claimant);
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&cert.to_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    /// The directory this stash writes to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl OrgCertSource for OrgCertStash {
    fn cert_for(&self, claimant: &Claimant) -> Option<OrgMembershipCert> {
        let bytes = std::fs::read(self.path_for(claimant)).ok()?;
        OrgMembershipCert::from_bytes(&bytes)
            .ok()
            .filter(|cert| cert.member == claimant.subject)
    }
}
