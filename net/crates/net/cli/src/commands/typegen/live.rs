// SPDX-License-Identifier: MIT OR Apache-2.0
//! Provider-attributed discovery and bounded metadata acquisition.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use net::adapter::net::behavior::{tag::Tag, tag_codec::tools_from_tags, ToolCapability};
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec};
use net_sdk::tool::{
    ToolDescriptor, ToolMetadataRequest, ToolMetadataResponse, TOOL_METADATA_FETCH_SERVICE,
};

use crate::deadline::Deadline;
use crate::error::{generic, invalid_args, CliError};

#[derive(Clone)]
struct Advertisement {
    provider: u64,
    descriptor: ToolDescriptor,
    metadata_service: bool,
}

// Capture descriptors and provenance in ONE fold read. list_tools aggregates
// away node IDs and can choose metadata from a different provider on each read.
fn observe(mesh: &net_sdk::Mesh) -> Vec<Advertisement> {
    mesh.inner().capability_fold().with_state(|state| {
        let mut result = Vec::new();
        for ((_, provider), entry) in &state.entries {
            let membership = &entry.payload;
            let tags: Vec<_> = membership
                .tags
                .iter()
                .filter_map(|s| Tag::parse(s).ok())
                .collect();
            for mut cap in tools_from_tags(&tags) {
                cap.input_schema = membership
                    .metadata
                    .get(&ToolCapability::input_schema_metadata_key(&cap.tool_id))
                    .cloned();
                cap.output_schema = membership
                    .metadata
                    .get(&ToolCapability::output_schema_metadata_key(&cap.tool_id))
                    .cloned();
                result.push(Advertisement {
                    provider: *provider,
                    descriptor: ToolDescriptor::from_capability(&cap, &membership.metadata),
                    metadata_service: membership
                        .tags
                        .contains(&format!("nrpc:{TOOL_METADATA_FETCH_SERVICE}")),
                });
            }
        }
        result
    })
}

pub(super) async fn acquire(
    mesh: &net_sdk::Mesh,
    tags: &[String],
    tools: &[String],
    deadline: Option<Deadline>,
) -> Result<Vec<ToolDescriptor>, CliError> {
    // One fallback acquisition limit, not a fresh budget for every RPC.
    let deadline = match deadline {
        Some(d) => d,
        None => Deadline::after(Duration::from_secs(30))?,
    };
    deadline.run(async {
        let mut advertisements = Vec::new();
        let selected = super::discover_with_timeout(|| {
            advertisements = observe(mesh);
            advertisements.iter().map(|a| a.descriptor.clone()).collect()
        }, tags, tools, Duration::from_secs(5)).await?;
        advertisements.retain(|a| selected.contains(&a.descriptor));
        let providers = select(advertisements)?;
        let mut result = Vec::new();
        for (advertisement, count) in providers {
            let mut descriptor = advertisement.descriptor;
            if descriptor.input_schema.is_none()
                || (descriptor.output_schema.is_none() && advertisement.metadata_service)
            {
                let response: ToolMetadataResponse = Box::pin(mesh.call_typed(
                    advertisement.provider,
                    TOOL_METADATA_FETCH_SERVICE,
                    &ToolMetadataRequest { name: descriptor.tool_id.clone() },
                    CallOptionsTyped { raw: Default::default(), codec: Codec::Json },
                )).await.map_err(|e| generic(format!("metadata fetch for `{}` at provider {} failed: {e}; no output was written", descriptor.tool_id, advertisement.provider)))?;
                descriptor = validate_response(&descriptor, response, advertisement.provider)?;
            }
            validate_schema(&descriptor)?;
            descriptor.node_count = count;
            result.push(descriptor);
        }
        Ok(result)
    }).await
}

fn select(mut ads: Vec<Advertisement>) -> Result<Vec<(Advertisement, u32)>, CliError> {
    ads.sort_by_key(|a| a.provider);
    let mut groups: BTreeMap<String, (Advertisement, BTreeSet<u64>)> = BTreeMap::new();
    for ad in ads {
        match groups.entry(ad.descriptor.tool_id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let provider = ad.provider;
                entry.insert((ad, BTreeSet::from([provider])));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (existing, providers) = entry.get_mut();
                if existing.descriptor != ad.descriptor
                    || existing.metadata_service != ad.metadata_service
                {
                    return Err(invalid_args(format!("conflicting live advertisements for `{}` (versions {} / {}, providers {} / {}); narrow the selection or reconcile providers; no output was written", ad.descriptor.tool_id, existing.descriptor.version, ad.descriptor.version, existing.provider, ad.provider)));
                }
                providers.insert(ad.provider);
            }
        }
    }
    Ok(groups
        .into_values()
        .map(|(ad, providers)| (ad, providers.len() as u32))
        .collect())
}

fn validate_response(
    selected: &ToolDescriptor,
    response: ToolMetadataResponse,
    provider: u64,
) -> Result<ToolDescriptor, CliError> {
    let ToolMetadataResponse::Found { descriptor } = response else {
        return Err(generic(format!(
            "metadata provider {provider} returned NotFound for `{}`; no output was written",
            selected.tool_id
        )));
    };
    if descriptor.tool_id != selected.tool_id
        || descriptor.version != selected.version
        || descriptor.tags != selected.tags
        || selected
            .input_schema
            .as_ref()
            .is_some_and(|s| descriptor.input_schema.as_ref() != Some(s))
        || selected
            .output_schema
            .as_ref()
            .is_some_and(|s| descriptor.output_schema.as_ref() != Some(s))
    {
        return Err(generic(format!(
            "metadata mismatch from provider {provider} for `{}@{}`; no output was written",
            selected.tool_id, selected.version
        )));
    }
    Ok(descriptor)
}

fn validate_schema(descriptor: &ToolDescriptor) -> Result<(), CliError> {
    let input = descriptor.input_schema.as_deref().ok_or_else(|| {
        invalid_args(format!(
            "selected tool `{}` has no input schema; no output was written",
            descriptor.tool_id
        ))
    })?;
    for (kind, source) in [
        ("input", Some(input)),
        ("output", descriptor.output_schema.as_deref()),
    ] {
        if let Some(source) = source {
            super::schema::parse(source).map_err(|e| {
                invalid_args(format!(
                    "selected tool `{}` has unusable {kind} schema: {e}; no output was written",
                    descriptor.tool_id
                ))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(provider: u64) -> Advertisement {
        let mut descriptor = super::super::tests::desc("selected");
        descriptor.node_count = 0;
        Advertisement {
            provider,
            descriptor,
            metadata_service: true,
        }
    }

    #[test]
    fn identical_replicas_choose_lowest_provider_and_count_nodes_once() {
        let selected = select(vec![ad(9), ad(2), ad(9)]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.provider, 2);
        assert_eq!(selected[0].1, 2);
    }

    #[test]
    fn versions_and_conflicting_provider_metadata_fail_closed() {
        for field in ["version", "schema", "service"] {
            let mut conflicting = ad(3);
            match field {
                "version" => conflicting.descriptor.version = "2".into(),
                "schema" => conflicting.descriptor.input_schema = Some("{}".into()),
                _ => conflicting.metadata_service = false,
            }
            assert!(select(vec![ad(2), conflicting]).is_err());
        }
    }

    #[test]
    fn fetch_cannot_replace_inline_contract_or_selection_tags() {
        let mut selected = ad(2).descriptor;
        selected.input_schema = Some("{}".into());
        let mut fetched = selected.clone();
        fetched.input_schema = Some(r#"{"type":"string"}"#.into());
        assert!(validate_response(
            &selected,
            ToolMetadataResponse::Found {
                descriptor: fetched
            },
            2
        )
        .is_err());
        let mut fetched = selected.clone();
        fetched.tags.push("changed".into());
        assert!(validate_response(
            &selected,
            ToolMetadataResponse::Found {
                descriptor: fetched
            },
            2
        )
        .is_err());
        assert!(validate_schema(&selected).is_ok(), "output is optional");
    }
}
