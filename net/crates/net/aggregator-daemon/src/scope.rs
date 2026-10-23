//! The daemon's publication scope: one source subnet per daemon.
//!
//! Slice Q1 of `docs/internal/plans/AGGREGATOR_MULTI_HOST_PLAN.md` (D0).
//!
//! Channel visibility is enforced against the publishing node's own subnet
//! (`MeshNode::set_channel_configs` builds the gateway from
//! `local_subnet`). A daemon's summary channels therefore have one
//! publication boundary, whatever subnet a summary says it is about. So
//! the daemon serves exactly one source subnet:
//! - its node's subnet is that subnet;
//! - every group and template must summarize it;
//! - at most one group per fold kind runs on it, since two would read the
//!   same fold and publish the same stream.
//!
//! [`resolve`] checks all of that at boot, before the node exists, and
//! normalizes every group's and template's `source_subnet` to the
//! daemon's. Code downstream of boot can then rely on it.
//!
//! It also refuses a subnet policy that would leave the summaries with no
//! classified audience, which would otherwise show up only as summaries
//! reaching nobody and queries refused, one warn log at a time:
//! - a global daemon with `[[subnet_rule]]`s under a non-global
//!   visibility (a global publisher's subnet-local or parent-visible
//!   summaries reach only peers with no subnet, so every peer the rules
//!   classify is cut off);
//! - a subnet daemon whose rules can never classify a peer into the part
//!   of its subnet the visibility admits.

use std::collections::HashMap;

use net::adapter::net::{SubnetId, Visibility};

use super::subnet_policy::SubnetRuleConfig;
use super::{Config, DaemonError};

/// The resolved scope every group on this daemon shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Scope {
    /// The daemon node's subnet, and the source subnet of every group.
    pub subnet: SubnetId,
    /// Visibility of every summary channel this daemon publishes on.
    pub summary_visibility: Visibility,
}

/// Default when `summary_visibility` is absent. Matches
/// `AggregatorConfig::new`, so a config without the key behaves as before.
const DEFAULT_SUMMARY_VISIBILITY: Visibility = Visibility::ParentVisible;

/// Resolve and check the scope, normalizing `config` in place: every
/// `[[group]]` and `[[template]]` ends up with `source_subnet` set to the
/// daemon's subnet.
pub(crate) fn resolve(config: &mut Config) -> Result<Scope, DaemonError> {
    let subnet = match config.subnet.as_deref() {
        None => SubnetId::GLOBAL,
        Some(raw) => parse(raw)?,
    };
    if !subnet.is_global() && config.subnet_rules.is_empty() {
        return Err(DaemonError::MissingSubnetPolicy {
            subnet: subnet.to_string(),
        });
    }
    let summary_visibility = match config.summary_visibility.as_deref() {
        None => DEFAULT_SUMMARY_VISIBILITY,
        Some(raw) => parse_visibility(raw)?,
    };
    check_audience(subnet, summary_visibility, &config.subnet_rules)?;
    let canonical = subnet.to_string();

    for group in &mut config.groups {
        group.source_subnet = check_source(&group.source_subnet, subnet, "group", &group.name)?;
    }
    for template in &mut config.templates {
        template.source_subnet =
            check_source(&template.source_subnet, subnet, "template", &template.name)?;
    }
    debug_assert!(config.groups.iter().all(|g| g.source_subnet == canonical));

    // One group per fold kind among the static groups. Templates are
    // shapes, not running groups, so they may share kinds; Spawn checks
    // the running set (`occupied_kind`).
    let mut owner: HashMap<u16, &str> = HashMap::new();
    for group in &config.groups {
        for kind in &group.fold_kinds {
            if let Some(first) = owner.insert(*kind, &group.name) {
                if first != group.name {
                    return Err(DaemonError::FoldKindOccupied {
                        kind: *kind,
                        first: first.to_string(),
                        second: group.name.clone(),
                    });
                }
            }
        }
    }

    Ok(Scope {
        subnet,
        summary_visibility,
    })
}

/// Refuse a policy under which the daemon's summaries (and `fold.query`,
/// which uses the same rule) can reach no classified peer.
///
/// The rule is `MeshNode::subnet_visible` from the daemon's subnet `S`. A
/// peer is recorded with the subnet the rules assign it, `GLOBAL` when none
/// matches. The rules can assign any combination of their per-level values,
/// since a peer may carry any tags.
/// - `global` visibility reaches everyone: nothing to check.
/// - `S` global: subnet-local and parent-visible reach only `GLOBAL`
///   peers, so rules (which exist to classify peers away from `GLOBAL`)
///   cut off everyone they classify.
/// - subnet-local: the rules must be able to assign exactly `S`.
/// - parent-visible: `S` and its ancestors are admitted. Unclassified
///   (`GLOBAL`) peers always are, so require that the rules can classify
///   a peer at least into `S`'s top level, i.e. into `S`'s lineage.
fn check_audience(
    subnet: SubnetId,
    visibility: Visibility,
    rules: &[SubnetRuleConfig],
) -> Result<(), DaemonError> {
    if visibility == Visibility::Global {
        return Ok(());
    }
    if subnet.is_global() {
        if rules.is_empty() {
            return Ok(());
        }
        return Err(DaemonError::GlobalDaemonWithSubnetRules {
            visibility: visibility_name(visibility),
        });
    }
    // Levels the rules must be able to write: all of `S` for subnet-local,
    // its top level for parent-visible.
    let needed = match visibility {
        Visibility::SubnetLocal => subnet.depth(),
        _ => 1,
    };
    for level in 0..needed {
        let value = subnet.level(level);
        if value == 0 {
            // "No restriction at this level": every peer has it.
            continue;
        }
        let assignable = rules.iter().any(|rule| {
            rule.level == level
                && rule.values.iter().any(|(tag_value, v)| {
                    // A mapping matching only the empty tag never fires
                    // (`SubnetPolicy::can_assign_non_global`).
                    *v == value && !(rule.tag_prefix.is_empty() && tag_value.is_empty())
                })
        });
        if !assignable {
            return Err(DaemonError::SubnetUnreachable {
                subnet: subnet.to_string(),
                level,
                value,
                visibility: visibility_name(visibility),
            });
        }
    }
    Ok(())
}

/// The config spelling of `visibility`, for error messages.
fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::SubnetLocal => "subnet-local",
        Visibility::ParentVisible => "parent-visible",
        Visibility::Global => "global",
        Visibility::Exported => "exported",
    }
}

/// An absent (`""`) source subnet inherits the daemon's; a present one
/// must equal it. Returns the canonical string the rest of the daemon
/// parses.
fn check_source(
    raw: &str,
    daemon_subnet: SubnetId,
    what: &'static str,
    name: &str,
) -> Result<String, DaemonError> {
    if raw.trim().is_empty() {
        return Ok(daemon_subnet.to_string());
    }
    let source = parse(raw)?;
    if source != daemon_subnet {
        return Err(DaemonError::SourceSubnetMismatch {
            what,
            name: name.to_string(),
            source_subnet: source.to_string(),
            daemon_subnet: daemon_subnet.to_string(),
        });
    }
    Ok(daemon_subnet.to_string())
}

fn parse(raw: &str) -> Result<SubnetId, DaemonError> {
    raw.trim()
        .parse::<SubnetId>()
        .map_err(|e| DaemonError::SubnetInvalid {
            raw: raw.to_string(),
            error: format!("{e:?}"),
        })
}

fn parse_visibility(raw: &str) -> Result<Visibility, DaemonError> {
    match raw.trim() {
        "subnet-local" => Ok(Visibility::SubnetLocal),
        "parent-visible" => Ok(Visibility::ParentVisible),
        "global" => Ok(Visibility::Global),
        "exported" => Err(DaemonError::SummaryVisibilityInvalid {
            raw: raw.to_string(),
            hint: " (\"exported\" needs explicit export targets, which the \
                   daemon config does not support yet)",
        }),
        _ => Err(DaemonError::SummaryVisibilityInvalid {
            raw: raw.to_string(),
            hint: "",
        }),
    }
}

/// The running group, if any, that already aggregates one of `kinds`.
///
/// Spawn calls this under its serialization lock, so two concurrent
/// spawns cannot both pass it.
pub(crate) async fn occupied_kind(
    registry: &net::adapter::net::behavior::aggregator::AggregatorRegistry,
    kinds: &[u16],
) -> Option<(u16, String)> {
    for entry in registry.entries() {
        let Some(replica) = entry.first_replica().await else {
            continue;
        };
        if let Some(kind) = replica
            .config()
            .fold_kinds
            .iter()
            .find(|k| kinds.contains(k))
        {
            return Some((*kind, entry.name.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(src: &str) -> Config {
        let base = format!(
            "listen = \"127.0.0.1:0\"\npsk_hex = \"{}\"\n{src}",
            "42".repeat(32)
        );
        toml::from_str(&base).expect("config parses")
    }

    const RULES: &str = r#"
        [[subnet_rule]]
        tag_prefix = "region:"
        level = 0
        values = { "eu" = 3 }
    "#;

    /// AG-3: rules on a global daemon cut off every peer they classify
    /// unless summaries are global.
    #[test]
    fn a_global_daemon_with_rules_needs_global_summaries() {
        for visibility in ["", "summary_visibility = \"parent-visible\"\n"] {
            let mut c = config(&format!("{visibility}{RULES}"));
            match resolve(&mut c) {
                Err(e @ DaemonError::GlobalDaemonWithSubnetRules { .. }) => {
                    assert!(
                        e.to_string().contains("summary_visibility = \"global\""),
                        "{e}"
                    );
                }
                other => panic!("expected GlobalDaemonWithSubnetRules, got {other:?}"),
            }
        }
        let mut c = config(&format!("summary_visibility = \"subnet-local\"\n{RULES}"));
        assert!(matches!(
            resolve(&mut c),
            Err(DaemonError::GlobalDaemonWithSubnetRules {
                visibility: "subnet-local"
            })
        ));
        let mut c = config(&format!("summary_visibility = \"global\"\n{RULES}"));
        assert_eq!(
            resolve(&mut c)
                .expect("global summaries reach everyone")
                .subnet,
            SubnetId::GLOBAL
        );
    }

    /// AG-5: the rules must be able to classify a peer into the part of
    /// the daemon's subnet its visibility admits.
    #[test]
    fn rules_that_cannot_reach_the_daemon_subnet_are_refused() {
        const TWO_LEVELS: &str = r#"
            [[subnet_rule]]
            tag_prefix = "region:"
            level = 0
            values = { "eu" = 3, "us" = 4 }

            [[subnet_rule]]
            tag_prefix = "rack:"
            level = 1
            values = { "r1" = 1 }
        "#;
        let outcome = |subnet: &str, visibility: &str| {
            let mut c = config(&format!(
                "subnet = \"{subnet}\"\nsummary_visibility = \"{visibility}\"\n{TWO_LEVELS}"
            ));
            resolve(&mut c).map(|_| ())
        };
        // Exactly reachable.
        outcome("3.1", "subnet-local").expect("3.1 is assignable");
        outcome("4", "subnet-local").expect("4 is assignable");
        // Level 1 has no 7: no peer is ever in 3.7.
        match outcome("3.7", "subnet-local") {
            Err(DaemonError::SubnetUnreachable {
                level: 1, value: 7, ..
            }) => {}
            other => panic!("expected 3.7 unreachable at level 1, got {other:?}"),
        }
        // Parent-visible reaches 3.7's lineage through level 0 alone.
        outcome("3.7", "parent-visible").expect("peers in 3 see 3.7's summaries");
        // No rule writes 5 at level 0: outside every classified lineage.
        match outcome("5.1", "parent-visible") {
            Err(DaemonError::SubnetUnreachable {
                level: 0, value: 5, ..
            }) => {}
            other => panic!("expected 5.1 unreachable at level 0, got {other:?}"),
        }
        // Global summaries reach everyone, so nothing is checked.
        outcome("5.1", "global").expect("global summaries");
    }

    #[test]
    fn absent_subnet_is_global_with_parent_visible_summaries() {
        let mut c = config("");
        let scope = resolve(&mut c).expect("resolves");
        assert_eq!(scope.subnet, SubnetId::GLOBAL);
        assert_eq!(scope.summary_visibility, Visibility::ParentVisible);
    }

    #[test]
    fn groups_inherit_the_daemon_subnet_and_matching_ones_are_kept() {
        let mut c = config(&format!(
            r#"
            subnet = "3"
            {RULES}
            [[group]]
            name = "inherits"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50

            [[group]]
            name = "explicit"
            source_subnet = "3"
            fold_kinds = [2]
            replica_count = 1
            summary_interval_ms = 50

            [[template]]
            name = "t"
            fold_kinds = [1]
            summary_interval_ms = 50
            "#
        ));
        let scope = resolve(&mut c).expect("resolves");
        assert_eq!(scope.subnet, SubnetId::new(&[3]));
        assert!(c.groups.iter().all(|g| g.source_subnet == "3"));
        assert_eq!(c.templates[0].source_subnet, "3");
    }

    /// The upgrade case decided for Q1: a pre-Q1 config that labels a
    /// group with a non-global subnet but sets no daemon `subnet` is
    /// refused, with an error that names the fix.
    #[test]
    fn a_pre_q1_config_with_a_labelled_group_is_refused() {
        let mut c = config(
            r#"
            [[group]]
            name = "alpha"
            source_subnet = "3.7"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50
            "#,
        );
        let err = resolve(&mut c).expect_err("refused");
        assert!(
            matches!(err, DaemonError::SourceSubnetMismatch { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("set `subnet = \"3.7\"`"), "{err}");
    }

    #[test]
    fn a_non_global_subnet_without_rules_is_refused() {
        let mut c = config(r#"subnet = "3.7""#);
        assert!(matches!(
            resolve(&mut c),
            Err(DaemonError::MissingSubnetPolicy { .. })
        ));
    }

    #[test]
    fn a_mismatched_template_is_refused() {
        let mut c = config(&format!(
            r#"
            subnet = "3"
            {RULES}
            [[template]]
            name = "elsewhere"
            source_subnet = "4"
            fold_kinds = [1]
            summary_interval_ms = 50
            "#
        ));
        assert!(matches!(
            resolve(&mut c),
            Err(DaemonError::SourceSubnetMismatch {
                what: "template",
                ..
            })
        ));
    }

    #[test]
    fn two_groups_of_one_fold_kind_are_refused() {
        let mut c = config(
            r#"
            [[group]]
            name = "a"
            fold_kinds = [1]
            replica_count = 1
            summary_interval_ms = 50

            [[group]]
            name = "b"
            fold_kinds = [2, 1]
            replica_count = 1
            summary_interval_ms = 50
            "#,
        );
        match resolve(&mut c) {
            Err(DaemonError::FoldKindOccupied {
                kind: 1,
                first,
                second,
            }) => {
                assert_eq!((first.as_str(), second.as_str()), ("a", "b"));
            }
            other => panic!("expected FoldKindOccupied, got {other:?}"),
        }
    }

    #[test]
    fn visibility_values() {
        for (raw, want) in [
            ("subnet-local", Visibility::SubnetLocal),
            ("parent-visible", Visibility::ParentVisible),
            ("global", Visibility::Global),
        ] {
            let mut c = config(&format!("summary_visibility = \"{raw}\""));
            assert_eq!(resolve(&mut c).expect(raw).summary_visibility, want);
        }
        for raw in ["exported", "public"] {
            let mut c = config(&format!("summary_visibility = \"{raw}\""));
            assert!(matches!(
                resolve(&mut c),
                Err(DaemonError::SummaryVisibilityInvalid { .. })
            ));
        }
    }
}
