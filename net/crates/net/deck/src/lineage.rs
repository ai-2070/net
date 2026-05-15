//! Live-snapshot lineage inference.
//!
//! The substrate's `DaemonSnapshot` doesn't yet carry an
//! explicit `LineageRef` (see `PLAN.md` — that's a follow-up
//! substrate slice). For Phase A the deck infers group
//! membership from the daemon's NAME suffix:
//!
//! - `mikoshi`              → standalone
//! - `gravity#replica`      → ReplicaGroup "gravity"
//! - `anti_entr#standby`    → StandbyGroup "anti_entr"
//! - `drift_corr#fork@42`   → ForkGroup "drift_corr" parent seq 42
//!
//! Daemons sharing the same raw name belong to the same group;
//! ordering within a group is by `daemon_id` (stable across
//! restarts since it's the keypair's `origin_hash`).
//!
//! When the substrate ships explicit lineage on
//! `DaemonSnapshot`, this module flips to read that field
//! directly + drops the name-parsing path.

use std::collections::BTreeMap;

use net_sdk::deck::DaemonSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    /// One daemon, no group.
    Solo,
    /// Stateless replicas — interchangeable members.
    Replica,
    /// Active-passive standbys — first member (by id) is
    /// active, rest are warm.
    Standby,
    /// Forks from a common parent.
    Fork {
        parent_seq: u64,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum MemberRole {
    Solo,
    Replica(u8),
    StandbyActive,
    StandbyWarm(u8),
    Fork(u8),
}

pub struct LiveMember<'a> {
    pub id: u64,
    pub daemon: &'a DaemonSnapshot,
    pub role: MemberRole,
}

pub struct LiveGroup<'a> {
    pub kind: GroupKind,
    /// Human-readable group name, stripped of the `#...` suffix.
    pub display_name: String,
    pub members: Vec<LiveMember<'a>>,
}

/// Group a snapshot's daemons by inferred lineage. Returns
/// groups in a stable order: Solo first, then Replica, Fork,
/// Standby — alphabetic by display name within each kind.
pub fn group_daemons(daemons: &BTreeMap<u64, DaemonSnapshot>) -> Vec<LiveGroup<'_>> {
    let mut buckets: BTreeMap<String, Vec<(u64, &DaemonSnapshot)>> = BTreeMap::new();
    for (id, d) in daemons {
        buckets.entry(d.name.clone()).or_default().push((*id, d));
    }

    let mut groups = Vec::new();
    for (raw_name, mut members) in buckets {
        let (kind, display_name) = parse_name(&raw_name);
        members.sort_by_key(|(id, _)| *id);
        let live_members: Vec<LiveMember<'_>> = members
            .into_iter()
            .enumerate()
            .map(|(i, (id, d))| {
                let role = role_for(kind, i);
                LiveMember { id, daemon: d, role }
            })
            .collect();
        groups.push(LiveGroup {
            kind,
            display_name,
            members: live_members,
        });
    }
    groups.sort_by(|a, b| {
        kind_order(&a.kind)
            .cmp(&kind_order(&b.kind))
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    groups
}

fn parse_name(name: &str) -> (GroupKind, String) {
    if let Some((display, suffix)) = name.split_once('#') {
        if suffix == "replica" {
            return (GroupKind::Replica, display.to_string());
        }
        if suffix == "standby" {
            return (GroupKind::Standby, display.to_string());
        }
        if let Some(seq_str) = suffix.strip_prefix("fork@") {
            if let Ok(seq) = seq_str.parse::<u64>() {
                return (GroupKind::Fork { parent_seq: seq }, display.to_string());
            }
        }
    }
    (GroupKind::Solo, name.to_string())
}

fn role_for(kind: GroupKind, index: usize) -> MemberRole {
    match kind {
        GroupKind::Solo => MemberRole::Solo,
        GroupKind::Replica => MemberRole::Replica(index as u8),
        GroupKind::Fork { .. } => MemberRole::Fork(index as u8),
        GroupKind::Standby => {
            if index == 0 {
                MemberRole::StandbyActive
            } else {
                MemberRole::StandbyWarm((index - 1) as u8)
            }
        }
    }
}

fn kind_order(k: &GroupKind) -> u8 {
    match k {
        GroupKind::Solo => 0,
        GroupKind::Replica => 1,
        GroupKind::Fork { .. } => 2,
        GroupKind::Standby => 3,
    }
}

/// Short tag for the LIST tab's LINEAGE column: `SOLO`,
/// `REP  m[0]`, `STBY active`, `STBY warm`, `FORK f[0]@42`.
pub fn lineage_tag(role: MemberRole, kind: GroupKind) -> String {
    match role {
        MemberRole::Solo => "SOLO".to_string(),
        MemberRole::Replica(i) => format!("REP  m[{i}]"),
        MemberRole::StandbyActive => "STBY active".to_string(),
        MemberRole::StandbyWarm(_) => "STBY warm".to_string(),
        MemberRole::Fork(i) => {
            if let GroupKind::Fork { parent_seq } = kind {
                format!("FORK f[{i}]@{parent_seq}")
            } else {
                format!("FORK f[{i}]")
            }
        }
    }
}
