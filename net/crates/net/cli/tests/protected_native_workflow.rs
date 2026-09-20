// SPDX-License-Identifier: MIT OR Apache-2.0
//! Two-node protected native journey; no public discovery or invocation retry.
use net_sdk::{
    capabilities::CapabilitySet,
    identity::Identity,
    mesh_rpc::{CallOptionsTyped, Codec, RpcError},
    org::{
        CapabilityAuthorityId, DispatcherScope, NodeAuthority, OrgAccess, OrgCaller,
        OrgCredentials, OrgDispatcherGrant, OrgKeypair, OrgMembershipCert, OWNER_AUDIENCE_FILE,
    },
    MeshBuilder,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Message {
    text: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_protected_native_denial_then_authorized_typed_call() {
    // One test in this binary: authority directories stay alive through node
    // shutdown; no subsequent adoption can recycle their revocation-lock inode.
    let dirs = tempfile::tempdir().unwrap();
    let owner = OrgKeypair::generate();
    let provider_identity = Identity::generate();
    let caller_identity = Identity::generate();
    let provider = MeshBuilder::new("127.0.0.1:0", &[0x53; 32])
        .unwrap()
        .identity(provider_identity.clone())
        .build()
        .await
        .unwrap();
    let caller = MeshBuilder::new("127.0.0.1:0", &[0x53; 32])
        .unwrap()
        .identity(caller_identity.clone())
        .build()
        .await
        .unwrap();
    assert_ne!(provider.node_id(), caller.node_id());
    let provider_dir = dirs.path().join("provider");
    let caller_dir = dirs.path().join("caller");
    let mut caller_cert = None;
    for (dir, identity) in [
        (&provider_dir, &provider_identity),
        (&caller_dir, &caller_identity),
    ] {
        let cert =
            OrgMembershipCert::try_issue(&owner, identity.entity_id().clone(), 1, 3600).unwrap();
        NodeAuthority::adopt(dir, cert.clone(), identity.entity_id(), 0, None).unwrap();
        if dir == &caller_dir {
            caller_cert = Some(cert);
        }
    }
    // Operator pre-staging: same org, same owner-discovery audience. Overwrite
    // the already secured file, preserving adoption's permissions / Windows ACL.
    std::fs::write(
        caller_dir.join(OWNER_AUDIENCE_FILE),
        std::fs::read(provider_dir.join(OWNER_AUDIENCE_FILE)).unwrap(),
    )
    .unwrap();
    provider.install_org_authority(&provider_dir).unwrap();
    caller.install_org_authority(&caller_dir).unwrap();
    let capability = CapabilityAuthorityId::for_tag("nrpc:protected.echo");
    let dispatcher = OrgDispatcherGrant::try_issue(
        &owner,
        caller_identity.entity_id().clone(),
        DispatcherScope::Exact(capability),
        3600,
    )
    .unwrap();
    let org = caller
        .org(OrgCredentials::new(caller_cert.unwrap(), dispatcher, vec![], vec![]).unwrap())
        .unwrap();

    provider.start();
    caller.start();
    tokio::time::timeout(
        Duration::from_secs(10),
        caller.connect_via(
            &provider.local_addr().to_string(),
            provider.public_key(),
            provider.node_id(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let records = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let observed = records.clone();
    let service = provider
        .serve_org(
            "protected.echo",
            OrgAccess::SameOrg,
            move |who: OrgCaller, request: Message| {
                let observed = observed.clone();
                async move {
                    observed.lock().await.push((who, request.clone()));
                    Ok(request)
                }
            },
        )
        .unwrap();
    // Poll discovery, never the business RPC. Keep production announce timers.
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            provider
                .inner()
                .announce_capabilities(CapabilitySet::new())
                .await
                .unwrap();
            caller
                .inner()
                .announce_capabilities(CapabilitySet::new())
                .await
                .unwrap();
            if caller.inner().peer_entity_id(provider.node_id()).is_some()
                && provider.inner().peer_entity_id(caller.node_id()).is_some()
                && caller
                    .inner()
                    .org_cold_discovery(&capability, &[])
                    .is_ok_and(|capture| !capture.owner_providers().is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("private discovery and authenticated peer identities converge");

    let request = Message {
        text: "authorized native consumer".into(),
    };
    // Possessing transport identity and knowing the exact service is not proof.
    let denied: Result<Message, _> = tokio::time::timeout(
        Duration::from_secs(5),
        caller.call_typed(
            provider.node_id(),
            "protected.echo",
            &request,
            CallOptionsTyped {
                raw: Default::default(),
                codec: Codec::Json,
            },
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(&denied, Err(RpcError::ServerError { status: 0x0009, .. })),
        "{denied:?}"
    );
    assert!(
        records.lock().await.is_empty(),
        "provider denial precedes handler effect"
    );

    let response: Message =
        tokio::time::timeout(Duration::from_secs(5), org.call("protected.echo", &request))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(response, request);
    {
        let records = records.lock().await;
        assert_eq!(records.len(), 1, "one admitted call, no retry");
        let (who, actual) = &records[0];
        assert_eq!(actual, &request);
        assert_eq!(&who.entity, caller_identity.entity_id());
        assert_eq!(&who.provider, provider_identity.entity_id());
        assert_eq!(who.acting_org, owner.org_id());
        assert_eq!(who.provider_org, owner.org_id());
        assert_eq!(who.capability, capability);
        assert!(who.is_same_org());
    }
    drop(service);
    drop(org);
    caller.shutdown().await.unwrap();
    provider.shutdown().await.unwrap();
}
