//! Side-effect-free public views of the resolution consumed by CLI dispatch.
use crate::commands::aggregator::RemoteAttachArgs;
use crate::context::RemoteAttach;
use crate::error::{generic, CliError};
use crate::output::{emit_value, OutputFormat};
use serde::Serialize;

pub(crate) fn has_profile_target(profile: &crate::config::Profile) -> bool {
    profile.node_addr.is_some()
        || profile.node_pubkey.is_some()
        || profile.node_id.is_some()
        || profile.psk_hex.is_some()
}

#[derive(Serialize)]
pub(crate) struct TargetInspection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_pattern: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_node_id: Option<u64>,
    mode: &'static str,
    target: Option<PublicTarget>,
    bind: Option<String>,
    identity: InspectionIdentity,
    provenance: std::collections::BTreeMap<&'static str, &'static str>,
    ignored_profile_remote_defaults: bool,
    authorization: &'static str,
}

#[derive(Serialize)]
struct PublicTarget {
    address: String,
    node_id: u64,
    peer_key_fingerprint: String,
}

#[derive(Serialize)]
struct InspectionIdentity {
    state: &'static str,
    fingerprint: Option<String>,
    reason: Option<&'static str>,
}

pub(crate) async fn inspect(
    profile: &crate::config::Profile,
    args: &RemoteAttachArgs,
    identity_override: Option<&std::path::Path>,
    remote: Option<&RemoteAttach>,
    mode: &'static str,
) -> Result<TargetInspection, CliError> {
    let source = |flag: bool, configured: bool| {
        if flag {
            "flag"
        } else if configured {
            "profile"
        } else {
            "default"
        }
    };
    let mut provenance = std::collections::BTreeMap::new();
    provenance.insert(
        "mode",
        if mode == "temporary_supervisor" {
            "flag"
        } else {
            source(
                args.node_addr.is_some()
                    || args.node_pubkey.is_some()
                    || args.remote_node_id.is_some()
                    || args.psk_hex.is_some(),
                has_profile_target(profile),
            )
        },
    );
    for (name, flag, configured) in [
        (
            "node_addr",
            args.node_addr.is_some(),
            profile.node_addr.is_some(),
        ),
        (
            "node_pubkey",
            args.node_pubkey.is_some(),
            profile.node_pubkey.is_some(),
        ),
        (
            "node_id",
            args.remote_node_id.is_some(),
            profile.node_id.is_some(),
        ),
        ("psk", args.psk_hex.is_some(), profile.psk_hex.is_some()),
    ] {
        provenance.insert(
            name,
            if remote.is_some() {
                source(flag, configured)
            } else {
                "unused"
            },
        );
    }
    provenance.insert(
        "identity",
        source(identity_override.is_some(), profile.identity.is_some()),
    );
    provenance.insert(
        "bind",
        if remote.is_some() {
            source(args.bind.is_some(), profile.bind.is_some())
        } else {
            "unused"
        },
    );
    let identity = match identity_override.or(profile.identity.as_deref()) {
        Some(path) => {
            let keypair = crate::context::load_identity_keypair(path).await?;
            InspectionIdentity {
                state: "configured",
                fingerprint: Some(public_fingerprint(keypair.entity_id().as_bytes())),
                reason: None,
            }
        }
        None => InspectionIdentity {
            state: "unavailable",
            fingerprint: None,
            reason: Some(if mode == "hosted_service" {
                "execution requires a configured identity"
            } else {
                "no configured identity; execution would generate an ephemeral identity"
            }),
        },
    };
    Ok(TargetInspection {
        destination_pattern: None,
        subject_fingerprint: None,
        store: None,
        source: None,
        destination: None,
        provider_node_id: None,
        mode,
        target: remote.map(|remote| PublicTarget {
            address: remote.addr.to_string(),
            node_id: remote.node_id,
            peer_key_fingerprint: public_fingerprint(&remote.public_key),
        }),
        bind: remote.map(|remote| remote.bind.to_string()),
        identity,
        provenance,
        ignored_profile_remote_defaults: remote.is_none()
            && (has_profile_target(profile) || profile.bind.is_some()),
        authorization: "not_checked",
    })
}

pub(crate) fn public_fingerprint(key: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key)
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl TargetInspection {
    pub(crate) fn listener_bind(
        &mut self,
        bind: String,
        bind_source: &'static str,
        psk_source: &'static str,
    ) {
        self.bind = Some(bind);
        self.ignored_profile_remote_defaults = false;
        self.provenance("mode", "flag");
        self.provenance("bind", bind_source);
        self.provenance("psk", psk_source);
    }
    #[cfg(feature = "rtc-bootstrap")]
    pub(crate) fn standalone_service(profile: &crate::config::Profile, bind: String) -> Self {
        let mut view = Self::local(profile, "hosted_service");
        view.bind = Some(bind);
        view.identity = InspectionIdentity {
            state: "unavailable",
            fingerprint: None,
            reason: Some("execution generates an ephemeral identity; profile identity is unused"),
        };
        view.provenance("identity", "default");
        view
    }

    /// Local operations here do not consume a signing identity. In particular,
    /// an unrelated profile identity must not be read or generated to inspect.
    pub(crate) fn local(profile: &crate::config::Profile, mode: &'static str) -> Self {
        Self {
            destination_pattern: None,
            subject_fingerprint: None,
            store: None,
            source: None,
            destination: None,
            provider_node_id: None,
            mode,
            target: None,
            bind: None,
            identity: InspectionIdentity {
                state: "unused",
                fingerprint: None,
                reason: Some("this operation does not use a signing identity"),
            },
            provenance: [
                ("mode", "command"),
                ("identity", "unused"),
                ("bind", "unused"),
                ("node_addr", "unused"),
                ("node_pubkey", "unused"),
                ("node_id", "unused"),
                ("psk", "unused"),
            ]
            .into_iter()
            .collect(),
            ignored_profile_remote_defaults: has_profile_target(profile) || profile.bind.is_some(),
            authorization: "not_checked",
        }
    }

    pub(crate) fn provenance(&mut self, field: &'static str, source: &'static str) {
        self.provenance.insert(field, source);
    }

    pub(crate) fn unavailable_identity(&mut self, reason: &'static str) {
        self.identity = InspectionIdentity {
            state: "unavailable",
            fingerprint: None,
            reason: Some(reason),
        };
    }

    pub(crate) fn configured_identity(&mut self, public_key: &[u8]) {
        self.identity = InspectionIdentity {
            state: "configured",
            fingerprint: Some(public_fingerprint(public_key)),
            reason: None,
        };
    }

    pub(crate) fn explicit_mode(&mut self) {
        self.provenance.insert("mode", "flag");
    }

    pub(crate) fn emit(&self, output: Option<OutputFormat>) -> Result<(), CliError> {
        emit_value(OutputFormat::resolve_oneshot(output), self)
            .map_err(|e| generic(format!("write target inspection: {e}")))
    }
}
