//! OSDK-L X1 — the cross-language error-vocabulary fixture.
//!
//! `#[doc(hidden)]`: this is build/test tooling, not part of the facade's five
//! concepts. It is `pub` only so `examples/gen_org_error_fixtures.rs` and the
//! drift guard can both reach it.

use super::error::{
    OrgCredentialError, OrgDiscoveryError, OrgErrorDomain, OrgSdkError, ERR_ORG_PREFIX,
};
use super::types::{CoarseAdmissionReason, GrantAudienceInstallError};

/// One representative error per kind, so every token in the vocabulary appears
/// in the fixture with its real rendering.
fn samples() -> Vec<OrgSdkError> {
    let bad_ttl = super::types::OrgMembershipCert::try_issue(
        &super::types::OrgKeypair::from_bytes([0x11u8; 32]),
        net::adapter::net::identity::EntityKeypair::from_bytes([0x22u8; 32])
            .entity_id()
            .clone(),
        1,
        u64::MAX,
    )
    .expect_err("ttl ceiling yields a canonical OrgError");

    let entity = |b: u8| {
        net::adapter::net::identity::EntityKeypair::from_bytes([b; 32])
            .entity_id()
            .clone()
    };
    let org = |b: u8| super::types::OrgKeypair::from_bytes([b; 32]).org_id();
    let hex = "ab".repeat(32);

    vec![
        // ---- credentials (local; nothing was sent) ----
        OrgCredentialError::PersistentIdentityRequired.into(),
        OrgCredentialError::NodeAuthorityRequired.into(),
        OrgCredentialError::NodeAuthorityOrgMismatch {
            authority_org: org(0x31),
            membership_org: org(0x32),
        }
        .into(),
        OrgCredentialError::MemberBindingMismatch {
            expected: entity(0x41),
            credential: entity(0x42),
        }
        .into(),
        OrgCredentialError::SignatureInvalid {
            credential: "membership".to_string(),
            source: bad_ttl,
        }
        .into(),
        OrgCredentialError::DispatcherBindingMismatch {
            dispatcher: entity(0x51),
            member: entity(0x52),
        }
        .into(),
        OrgCredentialError::ActingOrgMismatch {
            membership_org: org(0x61),
            dispatcher_org: org(0x62),
        }
        .into(),
        OrgCredentialError::GrantNotForActingOrg {
            grant_id: hex.clone(),
            grantee_org: org(0x71),
        }
        .into(),
        OrgCredentialError::DuplicateGrant {
            grant_id: hex.clone(),
        }
        .into(),
        OrgCredentialError::AudienceSecretMismatch {
            grant_id: hex.clone(),
        }
        .into(),
        OrgCredentialError::AudienceInstallRefused {
            grant_id: hex.clone(),
            source: GrantAudienceInstallError::GrantNotCurrent,
        }
        .into(),
        OrgCredentialError::AudienceSecretFile {
            path: "/etc/net/grants/example.audience".to_string(),
            detail: "audience secret is not a regular file".to_string(),
        }
        .into(),
        OrgCredentialError::NotCurrentlyValid {
            credential: "membership".to_string(),
            source: super::types::OrgError::InvalidFormat,
        }
        .into(),
        OrgCredentialError::DispatcherScopeExcludesCapability {
            capability: hex.clone(),
        }
        .into(),
        OrgCredentialError::MissingCapabilityGrant {
            capability: hex.clone(),
        }
        .into(),
        OrgCredentialError::AmbiguousCapabilityGrant {
            capability: hex.clone(),
            grant_ids: vec![hex.clone(), "cd".repeat(32)],
        }
        .into(),
        // ---- discovery (local; nothing was sent) ----
        OrgDiscoveryError::NoAuthorizedProvider {
            capability: hex.clone(),
            considered: 3,
        }
        .into(),
        OrgDiscoveryError::ProviderNotDirect {
            provider: entity(0x81),
        }
        .into(),
        // ---- admission denied (REMOTE) ----
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied),
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::NotSupported),
        OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Unavailable),
        // ---- rpc (transport / non-admission server failure) ----
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::Timeout { elapsed_ms: 5000 }),
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::NoRoute {
            target: 0xDEAD,
            reason: "no path".to_string(),
        }),
        OrgSdkError::Rpc(net::adapter::net::mesh_rpc::RpcError::Cancelled),
    ]
}

/// Render the canonical `error_vectors.json` content.
///
/// Lives in the library so the generator example and the drift guard share ONE
/// implementation — a fixture whose generator and checker could disagree would
/// guard nothing.
pub fn render_error_vectors() -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(
        "  \"description\": \"OSDK-L X1 — the canonical `org:` error vocabulary every language \
         binding parses. Each vector's `wire` string is what Rust emits; a binding MUST recover \
         `domain` and `kind` from it, and MUST use `is_local` to decide whether the request left \
         the process. GENERATED — do not hand-edit; run `cargo run -p net-mesh-sdk --features \
         net,cortex --example gen_org_error_fixtures`.\",\n",
    );
    out.push_str(&format!(
        "  \"version\": 1,\n  \"prefix\": {ERR_ORG_PREFIX:?},\n"
    ));

    // The domain vocabulary, with the load-bearing local/remote split.
    out.push_str("  \"domains\": [\n");
    let domains = [
        OrgErrorDomain::Credentials,
        OrgErrorDomain::Discovery,
        OrgErrorDomain::AdmissionDenied,
        OrgErrorDomain::Rpc,
        OrgErrorDomain::Unclassified,
    ];
    for (i, d) in domains.iter().enumerate() {
        out.push_str(&format!(
            "    {{ \"token\": {:?}, \"is_local\": {} }}{}\n",
            d.as_wire(),
            d.is_local(),
            if i + 1 == domains.len() { "" } else { "," }
        ));
    }
    out.push_str("  ],\n");

    out.push_str("  \"vectors\": [\n");
    let samples = samples();
    for (i, e) in samples.iter().enumerate() {
        let domain = e.domain();
        out.push_str(&format!(
            "    {{ \"wire\": {:?}, \"domain\": {:?}, \"kind\": {:?}, \"is_local\": {} }}{}\n",
            e.to_wire(),
            domain.as_wire(),
            e.wire_kind(),
            domain.is_local(),
            if i + 1 == samples.len() { "" } else { "," }
        ));
    }
    out.push_str("  ],\n");

    // The row Kyra's §D5a exists for: a binding meeting a domain this build
    // does not define must classify it as `unknown` — NEVER as one of the four,
    // because claiming `admission_denied` would assert a remote evaluation that
    // may never have happened.
    out.push_str(
        "  \"unclassified_cases\": [\n\
         \x20   { \"wire\": \"org:frobnicate:whatever: detail\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"org:\", \"expect_domain\": \"unknown\", \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"org:credentials\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false },\n\
         \x20   { \"wire\": \"not-an-org-error\", \"expect_domain\": \"unknown\", \
         \"expect_is_local\": false }\n\
         \x20 ],\n",
    );
    out.push_str(
        "  \"notes\": [\n\
         \x20   \"A binding MUST NOT report `admission_denied` for a string it could not parse: \
         that asserts a request reached a provider and its admission engine evaluated it.\",\n\
         \x20   \"`admission_denied` vectors carry the coarse bucket and NOTHING else — a precise \
         remote reason would be a credential oracle (OA2-E2).\",\n\
         \x20   \"`org:rpc:` reuses the frozen nRPC kind vocabulary rather than minting second \
         names for the same conditions.\"\n\
         \x20 ]\n",
    );
    out.push_str("}\n");
    out
}
