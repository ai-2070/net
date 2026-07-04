//! Credential-status classification for a wrapped MCP server
//! (`MCP_BRIDGE_PLAN.md` Phase 1, `wrap/credentials.rs`).
//!
//! **Conservative by construction.** The plan's rule is load-bearing:
//! *detection failure must never become a permission bypass* — "unknown is
//! spicy until proven boring." So the only status that is *not* gated
//! ([`CredentialStatus::None`]) can be reached **only** through the explicit
//! forced downward override, never through detection. Everything the
//! classifier is unsure about lands on [`CredentialStatus::Unknown`], which
//! is treated exactly like `Credentialed` for consent.

/// How the wrapper classifies a wrapped tool's credential exposure. The
/// value rides on the announcement as a `credential_status` metadata tag
/// (see [`super::descriptor`]) and gates invocation in the shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    /// Secrets are configured — env additions or secret-shaped variables.
    Credentialed,
    /// A known external-API server (talks to a third-party service).
    ExternalApi,
    /// Could not classify. Treated **exactly** like `Credentialed` for
    /// consent — the spicy default.
    Unknown,
    /// Explicitly declared to carry no credentials. Reachable only via the
    /// forced downward override; never inferred.
    None,
}

impl CredentialStatus {
    /// The wire/tag form carried in the announcement metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialStatus::Credentialed => "credentialed",
            CredentialStatus::ExternalApi => "external_api",
            CredentialStatus::Unknown => "unknown",
            CredentialStatus::None => "none",
        }
    }

    /// Parse the wire/tag form back to a status — the demand side (`net mcp
    /// serve`) reads it off a discovered capability's metadata to decide
    /// consent. Any unrecognised or missing string maps to [`Self::Unknown`]
    /// (the spicy default, never `None`), so a garbled or absent status can
    /// only ever *over*-gate, never bypass consent — the same conservative
    /// rule detection follows.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "credentialed" => CredentialStatus::Credentialed,
            "external_api" => CredentialStatus::ExternalApi,
            "none" => CredentialStatus::None,
            // "unknown" and anything unrecognised — spicy until proven boring.
            _ => CredentialStatus::Unknown,
        }
    }

    /// Does invoking this capability require local consent (an allowlist
    /// entry or an approved pin)? Everything except an explicitly-boring
    /// `None` is gated — credentialed, external, and unknown alike.
    pub fn requires_consent(self) -> bool {
        !matches!(self, CredentialStatus::None)
    }
}

/// Operator override of the detected status (the `net wrap` flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialOverride {
    /// No override — use detection.
    #[default]
    Detect,
    /// `--credentialed`: force `Credentialed`. Upward (more restrictive),
    /// so always allowed.
    Credentialed,
    /// `--no-credentials`: force `None`. Downward (less restrictive), so it
    /// requires `--force` — otherwise it is rejected.
    NoCredentials,
}

/// The inputs the classifier reasons over: the wrapped command and the
/// environment additions passed to it.
#[derive(Debug, Clone, Copy)]
pub struct WrapEnv<'a> {
    /// The server program (`npx`, `uvx`, a path, …).
    pub program: &'a str,
    /// Its arguments (`-y`, `@modelcontextprotocol/server-github`, …).
    pub args: &'a [String],
    /// Environment additions passed to the wrapper — the place a wrapped
    /// tool's secrets are configured. Never transit the mesh.
    pub envs: &'a [(String, String)],
}

/// Why a classification was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClassifyError {
    /// `--no-credentials` lowers the status below what was detected; the
    /// plan requires `--force` to confirm the operator means it.
    #[error("--no-credentials lowers the credential status; pass --force to confirm")]
    DownwardOverrideRequiresForce,
}

/// Substrings that mark an environment variable **key** as secret-shaped.
/// Matched case-insensitively. Deliberately broad — a false positive only
/// over-gates (safe); a false negative could leak (unsafe).
const SECRET_KEY_MARKERS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "APIKEY",
    "API_KEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "AUTH",
    "BEARER",
    "SESSION",
    "COOKIE",
    // Bare "KEY" last so more specific markers report first if we ever
    // surface *which* marker matched.
    "KEY",
];

/// Substrings in the command / args that mark a **known external-API**
/// server. Extensible; the safety of the classifier does not depend on this
/// list being complete — anything unmatched falls to `Unknown`, which is
/// gated just the same.
const KNOWN_EXTERNAL_API_MARKERS: &[&str] = &[
    "server-github",
    "server-gitlab",
    "server-slack",
    "server-brave-search",
    "server-google-maps",
    "server-sentry",
    "server-linear",
    "server-notion",
];

/// Classify a wrapped server's credential status.
///
/// Order: an explicit override wins (upward freely, downward only with
/// `force`); otherwise detection runs and biases toward gated statuses.
pub fn classify(
    env: &WrapEnv,
    over: CredentialOverride,
    force: bool,
) -> Result<CredentialStatus, ClassifyError> {
    match over {
        // Upward override — more restrictive, always allowed.
        CredentialOverride::Credentialed => return Ok(CredentialStatus::Credentialed),
        // Downward override — the ONLY path to the ungated `None`, and only
        // with an explicit `--force`.
        CredentialOverride::NoCredentials => {
            return if force {
                Ok(CredentialStatus::None)
            } else {
                Err(ClassifyError::DownwardOverrideRequiresForce)
            };
        }
        CredentialOverride::Detect => {}
    }

    if env_has_secret(env.envs) {
        return Ok(CredentialStatus::Credentialed);
    }
    if is_known_external_api(env.program, env.args) {
        return Ok(CredentialStatus::ExternalApi);
    }
    // Unsure ⇒ spicy. Gated exactly like `Credentialed`.
    Ok(CredentialStatus::Unknown)
}

/// True if any env-addition key looks secret-shaped.
fn env_has_secret(envs: &[(String, String)]) -> bool {
    envs.iter().any(|(k, _)| {
        let upper = k.to_ascii_uppercase();
        SECRET_KEY_MARKERS.iter().any(|m| upper.contains(m))
    })
}

/// True if the command or any argument names a known external-API server.
fn is_known_external_api(program: &str, args: &[String]) -> bool {
    let hay_matches = |s: &str| {
        let lower = s.to_ascii_lowercase();
        KNOWN_EXTERNAL_API_MARKERS.iter().any(|m| lower.contains(m))
    };
    hay_matches(program) || args.iter().any(|a| hay_matches(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_of(
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
        over: CredentialOverride,
        force: bool,
    ) -> Result<CredentialStatus, ClassifyError> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let envs: Vec<(String, String)> = envs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        classify(
            &WrapEnv {
                program,
                args: &args,
                envs: &envs,
            },
            over,
            force,
        )
    }

    #[test]
    fn secret_shaped_env_is_credentialed() {
        let s = classify_of(
            "npx",
            &["-y", "some-server"],
            &[("GITHUB_TOKEN", "ghp_xxx")],
            CredentialOverride::Detect,
            false,
        )
        .unwrap();
        assert_eq!(s, CredentialStatus::Credentialed);
        assert!(s.requires_consent());
    }

    #[test]
    fn known_external_api_is_external_api() {
        let s = classify_of(
            "npx",
            &["-y", "@modelcontextprotocol/server-github"],
            &[],
            CredentialOverride::Detect,
            false,
        )
        .unwrap();
        assert_eq!(s, CredentialStatus::ExternalApi);
        assert!(s.requires_consent());
    }

    #[test]
    fn unrecognized_server_is_unknown_and_still_gated() {
        let s = classify_of(
            "uvx",
            &["mcp-server-time"],
            &[("TZ", "UTC")], // not secret-shaped
            CredentialOverride::Detect,
            false,
        )
        .unwrap();
        assert_eq!(s, CredentialStatus::Unknown, "spicy default");
        assert!(s.requires_consent(), "unknown is gated like credentialed");
    }

    #[test]
    fn upward_override_needs_no_force() {
        let s = classify_of(
            "uvx",
            &["mcp-server-time"],
            &[],
            CredentialOverride::Credentialed,
            false,
        )
        .unwrap();
        assert_eq!(s, CredentialStatus::Credentialed);
    }

    #[test]
    fn downward_override_requires_force() {
        let err = classify_of(
            "uvx",
            &["mcp-server-time"],
            &[],
            CredentialOverride::NoCredentials,
            false,
        )
        .unwrap_err();
        assert_eq!(err, ClassifyError::DownwardOverrideRequiresForce);

        let ok = classify_of(
            "uvx",
            &["mcp-server-time"],
            &[],
            CredentialOverride::NoCredentials,
            true,
        )
        .unwrap();
        assert_eq!(ok, CredentialStatus::None);
        assert!(!ok.requires_consent(), "explicitly boring ⇒ not gated");
    }

    #[test]
    fn from_wire_round_trips_and_defaults_unknown_to_spicy() {
        for status in [
            CredentialStatus::Credentialed,
            CredentialStatus::ExternalApi,
            CredentialStatus::Unknown,
            CredentialStatus::None,
        ] {
            assert_eq!(CredentialStatus::from_wire(status.as_str()), status);
        }
        // A garbled or absent status is gated like credentialed, never None.
        for garbled in ["", "bogus", "credentialed ", "None", "UNKNOWN"] {
            let parsed = CredentialStatus::from_wire(garbled);
            assert_eq!(parsed, CredentialStatus::Unknown, "{garbled:?}");
            assert!(parsed.requires_consent());
        }
    }

    /// The safety invariant: detection can never yield the ungated `None`.
    /// Only a *forced* downward override can — so a misclassification can
    /// only ever over-gate, never bypass consent.
    #[test]
    fn detection_never_produces_ungated_none() {
        // For representative inputs, detection and the upward override — at
        // both `force` values — never yield the ungated `None`. Only a forced
        // downward override can, so a misclassification can only over-gate.
        let never_none = |program: &str, args: &[&str], envs: &[(&str, &str)]| {
            for over in [CredentialOverride::Detect, CredentialOverride::Credentialed] {
                for force in [false, true] {
                    if let Ok(status) = classify_of(program, args, envs, over, force) {
                        assert_ne!(
                            status,
                            CredentialStatus::None,
                            "program {program:?} reached ungated None without a forced downgrade",
                        );
                    }
                }
            }
        };
        never_none("uvx", &["mcp-server-time"], &[]);
        never_none("npx", &["-y", "server-github"], &[]);
        never_none("x", &["y"], &[("GITHUB_TOKEN", "t")]);
        never_none("random", &[], &[("PATH", "/usr/bin")]);
    }
}
