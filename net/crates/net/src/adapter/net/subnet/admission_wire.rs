//! Subnet admission on the wire (NET_CLI_PLAN_V3 V3-2 S1): subprotocol
//! [`SUBPROTOCOL_SUBNET_ADMISSION`] (`0x0A02`).
//!
//! Until now a peer's subnet credentials could only be admitted through a
//! local API (`MeshNode::issue_subnet_challenge` /
//! `admit_subnet_session`); nothing carried the exchange between nodes.
//! This is the two-leg session exchange that does, reusing the existing
//! session-, verifier- and nonce-bound [`SubnetAuthPresentation`]:
//!
//! 1. device → verifier `ChallengeRequest { nonce }`;
//!    verifier → device `Challenge { nonce, verifier, session_id, challenge }`
//!    (a fresh one-use challenge bound to the verifier's view of this
//!    session);
//! 2. device → verifier `Present { nonce, presentation, credential set }`;
//!    verifier → device `Verdict { nonce, refusal }`.
//!
//! A device may also WITHDRAW its own admission (V3-2B subnet leave):
//! device → verifier `Withdraw { nonce, authority, attachment }`;
//! verifier → device `Withdrawn { nonce, dropped }`. The verifier drops the
//! sender's context only if it is the named attachment under the named
//! authority — never another scope's admission — and acknowledges either way
//! (`dropped: false` means none was held there). The sender is the
//! AEAD-resolved session peer, so a node can withdraw only itself. This is
//! advisory self-withdrawal, not revocation: the credentials stay valid.
//!
//! `nonce` only correlates a leg with its reply; the security binding is
//! the verifier's challenge, consumed on first use, and the session the
//! frames ride. The verifier runs the unchanged `admit_subnet_session`
//! (credential chain, floors — including subject floors — and the
//! routing-id pin), so admission over the wire is exactly admission.

use super::auth::{SubnetAuthError, SubnetAuthPresentation, SubnetCredentialSet};
use crate::adapter::net::identity::EntityId;

/// Subprotocol id, next to identity proof (`0x0A01`) in the auth family.
pub const SUBPROTOCOL_SUBNET_ADMISSION: u16 = 0x0A02;
/// Upper bound on an encoded credential set carried in `Present`.
const MAX_SET_BYTES: usize = 4096;

const TAG_CHALLENGE_REQUEST: u8 = 1;
const TAG_CHALLENGE: u8 = 2;
const TAG_PRESENT: u8 = 3;
const TAG_VERDICT: u8 = 4;
const TAG_WITHDRAW: u8 = 5;
const TAG_WITHDRAWN: u8 = 6;

/// One leg of the subnet admission exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubnetAdmissionMsg {
    /// Device asks the verifier for a challenge.
    ChallengeRequest {
        /// Correlation id chosen by the device.
        nonce: u64,
    },
    /// Verifier's one-use challenge for this session.
    Challenge {
        /// Echoed correlation id.
        nonce: u64,
        /// The verifier's entity (the presentation must name it).
        verifier: EntityId,
        /// The verifier's view of the session id (the presentation binds it).
        session_id: u64,
        /// The one-use challenge to sign.
        challenge: [u8; 32],
    },
    /// Device's signed presentation and the credentials it covers.
    Present {
        /// Correlation id chosen by the device.
        nonce: u64,
        /// Session-, verifier- and challenge-bound proof.
        presentation: Box<SubnetAuthPresentation>,
        /// The credential set the presentation's hash names.
        set: Box<SubnetCredentialSet>,
    },
    /// Verifier's verdict: `None` admitted, `Some(kind)` refused.
    Verdict {
        /// Echoed correlation id.
        nonce: u64,
        /// Why admission was refused, if it was.
        refusal: Option<SubnetAuthError>,
    },
    /// Device withdraws its own admission at exactly this attachment.
    Withdraw {
        /// Correlation id chosen by the device.
        nonce: u64,
        /// The authority the admission is under.
        authority: EntityId,
        /// The exact admitted attachment (raw topology path).
        attachment: u32,
    },
    /// Verifier's acknowledgement: no admission of the sender remains at
    /// that attachment; `dropped` says whether one was held.
    Withdrawn {
        /// Echoed correlation id.
        nonce: u64,
        /// Whether a matching admission was dropped.
        dropped: bool,
    },
}

impl SubnetAdmissionMsg {
    /// The correlation id.
    pub fn nonce(&self) -> u64 {
        match self {
            Self::ChallengeRequest { nonce }
            | Self::Challenge { nonce, .. }
            | Self::Present { nonce, .. }
            | Self::Verdict { nonce, .. }
            | Self::Withdraw { nonce, .. }
            | Self::Withdrawn { nonce, .. } => *nonce,
        }
    }

    /// Wire form: tag ‖ nonce ‖ body.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::ChallengeRequest { nonce } => {
                out.push(TAG_CHALLENGE_REQUEST);
                out.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::Challenge {
                nonce,
                verifier,
                session_id,
                challenge,
            } => {
                out.push(TAG_CHALLENGE);
                out.extend_from_slice(&nonce.to_le_bytes());
                out.extend_from_slice(verifier.as_bytes());
                out.extend_from_slice(&session_id.to_le_bytes());
                out.extend_from_slice(challenge);
            }
            Self::Present {
                nonce,
                presentation,
                set,
            } => {
                out.push(TAG_PRESENT);
                out.extend_from_slice(&nonce.to_le_bytes());
                out.extend_from_slice(&presentation.to_bytes());
                let set = set.to_bytes();
                out.extend_from_slice(&(set.len() as u16).to_le_bytes());
                out.extend_from_slice(&set);
            }
            Self::Verdict { nonce, refusal } => {
                out.push(TAG_VERDICT);
                out.extend_from_slice(&nonce.to_le_bytes());
                out.push(match refusal {
                    None => 0xFF,
                    Some(e) => SubnetAuthError::ALL
                        .iter()
                        .position(|k| k == e)
                        .map(|i| i as u8)
                        .unwrap_or(0xFE),
                });
            }
            Self::Withdraw {
                nonce,
                authority,
                attachment,
            } => {
                out.push(TAG_WITHDRAW);
                out.extend_from_slice(&nonce.to_le_bytes());
                out.extend_from_slice(authority.as_bytes());
                out.extend_from_slice(&attachment.to_le_bytes());
            }
            Self::Withdrawn { nonce, dropped } => {
                out.push(TAG_WITHDRAWN);
                out.extend_from_slice(&nonce.to_le_bytes());
                out.push(u8::from(*dropped));
            }
        }
        out
    }

    /// Strict decode: unknown tags, wrong lengths and trailing bytes fail.
    pub fn decode(bytes: &[u8]) -> Result<Self, SubnetAuthError> {
        let bad = SubnetAuthError::InvalidFormat;
        if bytes.len() < 9 {
            return Err(bad);
        }
        let tag = bytes[0];
        let nonce = u64::from_le_bytes(bytes[1..9].try_into().map_err(|_| bad)?);
        let body = &bytes[9..];
        Ok(match tag {
            TAG_CHALLENGE_REQUEST if body.is_empty() => Self::ChallengeRequest { nonce },
            TAG_CHALLENGE if body.len() == 32 + 8 + 32 => Self::Challenge {
                nonce,
                verifier: EntityId::from_bytes(body[..32].try_into().map_err(|_| bad)?),
                session_id: u64::from_le_bytes(body[32..40].try_into().map_err(|_| bad)?),
                challenge: body[40..72].try_into().map_err(|_| bad)?,
            },
            TAG_PRESENT => {
                let p = SubnetAuthPresentation::WIRE_SIZE;
                if body.len() < p + 2 {
                    return Err(bad);
                }
                let presentation = Box::new(SubnetAuthPresentation::from_bytes(&body[..p])?);
                let len = u16::from_le_bytes([body[p], body[p + 1]]) as usize;
                if len > MAX_SET_BYTES || body.len() != p + 2 + len {
                    return Err(bad);
                }
                let set = Box::new(SubnetCredentialSet::from_bytes(&body[p + 2..])?);
                Self::Present {
                    nonce,
                    presentation,
                    set,
                }
            }
            TAG_VERDICT if body.len() == 1 => Self::Verdict {
                nonce,
                refusal: match body[0] {
                    0xFF => None,
                    i => Some(*SubnetAuthError::ALL.get(i as usize).ok_or(bad)?),
                },
            },
            TAG_WITHDRAW if body.len() == 32 + 4 => Self::Withdraw {
                nonce,
                authority: EntityId::from_bytes(body[..32].try_into().map_err(|_| bad)?),
                attachment: u32::from_le_bytes(body[32..36].try_into().map_err(|_| bad)?),
            },
            TAG_WITHDRAWN if body.len() == 1 && body[0] <= 1 => Self::Withdrawn {
                nonce,
                dropped: body[0] == 1,
            },
            _ => return Err(bad),
        })
    }
}

/// Why presenting credentials to a verifier did not end in admission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubnetAdmissionError {
    /// The verifier refused, with its reason.
    #[error("subnet admission refused: subnet:{0}")]
    Refused(SubnetAuthError),
    /// No session to the verifier.
    #[error("no session to the verifier")]
    NoSession,
    /// No reply within the bound (including a verifier that predates
    /// wire admission).
    #[error("no admission reply from the verifier")]
    Timeout,
    /// Local failure building or sending the exchange.
    #[error("subnet admission: {0}")]
    Local(String),
}
