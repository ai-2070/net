// SPDX-License-Identifier: MIT OR Apache-2.0
//! Two-node, two-org protected journey through a CLI-generated client.
//!
//! The cross-org twin of `protected_native_workflow.rs`: org B's provider
//! serves `protected.echo` under `OrgAccess::Granted`, announced only inside
//! the audience of a capability grant B issued to org A. A's caller discovers
//! it privately through that grant. A call without an org proof is denied
//! before any handler effect; the CLI-generated Python client is denied on
//! its uncredentialed attempt and admitted on its credentialed one, each
//! exactly once; every admitted call is attributed to the caller acting for
//! A under B's grant on B's provider. No invocation is retried.
use net_sdk::{
    capabilities::CapabilitySet,
    identity::Identity,
    mesh_rpc::{CallOptionsTyped, Codec, RpcError},
    org::{
        CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, NodeAuthority,
        OrgAccess, OrgAudienceSecret, OrgCaller, OrgCapabilityGrant, OrgCredentials,
        OrgDispatcherGrant, OrgKeypair, OrgMembershipCert,
    },
    MeshBuilder,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

async fn output(cmd: &mut Command) -> std::process::Output {
    let result = tokio::time::timeout(Duration::from_secs(20), cmd.kill_on_drop(true).output())
        .await
        .expect("bounded fixture subprocess")
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Message {
    text: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_org_granted_native_denial_then_authorized_generated_call() {
    // One test in this binary: authority directories stay alive through node
    // shutdown; no later adoption can recycle their revocation-lock inode.
    let dirs = tempfile::tempdir().unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    output(Command::new(python).args([
        "-c",
        "import pydantic; assert int(pydantic.__version__.split('.')[0]) == 2",
    ]))
    .await;
    // The generated client, from an explicit offline descriptor.
    let schema = json!({"type":"object", "properties":{"text":{"type":"string"}},
        "required":["text"], "additionalProperties":false})
    .to_string();
    let snapshot = dirs.path().join("protected.snapshot.json");
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&json!({
            "format_version":1, "captured_at":"2026-10-17T00:00:00Z",
            "source_query":{"tags":[],"tools":[]},
            "descriptors":[{"tool_id":"protected.echo", "name":"Protected echo", "version":"1.0.0",
                "description":"Cross-org typed echo", "input_schema":schema, "output_schema":schema,
                "requires":[], "estimated_time_ms":0, "stateless":true, "streaming":false,
                "tags":[], "node_count":1}]
        }))
        .unwrap(),
    )
    .unwrap();
    let config = dirs.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    output(
        Command::new(assert_cmd::cargo::cargo_bin("net-mesh"))
            .env_remove("NET_MESH_CONFIG")
            .env_remove("NET_MESH_PROFILE")
            .arg("--config")
            .arg(&config)
            .args([
                "typegen",
                "generate",
                "--language",
                "python",
                "--from-snapshot",
            ])
            .arg(&snapshot)
            .arg("--out")
            .arg(dirs.path().join("generated")),
    )
    .await;

    // Two orgs: A (the caller's) and B (the provider's). Each node adopts into
    // its own org; no audience is shared.
    let org_a = OrgKeypair::generate();
    let org_b = OrgKeypair::generate();
    let provider_identity = Identity::generate();
    let caller_identity = Identity::generate();
    let provider = MeshBuilder::new("127.0.0.1:0", &[0x54; 32])
        .unwrap()
        .identity(provider_identity.clone())
        .build()
        .await
        .unwrap();
    let caller = MeshBuilder::new("127.0.0.1:0", &[0x54; 32])
        .unwrap()
        .identity(caller_identity.clone())
        .build()
        .await
        .unwrap();
    let provider_dir = dirs.path().join("provider");
    let caller_dir = dirs.path().join("caller");
    let provider_cert =
        OrgMembershipCert::try_issue(&org_b, provider_identity.entity_id().clone(), 1, 3600)
            .unwrap();
    NodeAuthority::adopt(
        &provider_dir,
        provider_cert,
        provider_identity.entity_id(),
        0,
        None,
    )
    .unwrap();
    let caller_cert =
        OrgMembershipCert::try_issue(&org_a, caller_identity.entity_id().clone(), 1, 3600).unwrap();
    NodeAuthority::adopt(
        &caller_dir,
        caller_cert.clone(),
        caller_identity.entity_id(),
        0,
        None,
    )
    .unwrap();
    provider.install_org_authority(&provider_dir).unwrap();
    caller.install_org_authority(&caller_dir).unwrap();

    // B grants A discover + invoke on `protected.echo`, on any B-owned node,
    // with a fresh audience. A dispatches for itself.
    let capability = CapabilityAuthorityId::for_tag("nrpc:protected.echo");
    let (grant, secret) = OrgCapabilityGrant::try_issue(
        &org_b,
        org_a.org_id(),
        capability,
        GrantRights::DISCOVER.union(GrantRights::INVOKE),
        GrantTargetScope::AnyNodeOwnedBy(org_b.org_id()),
        3600,
    )
    .unwrap();
    let secret = secret.expect("a DISCOVER grant mints its audience");
    let provider_secret = OrgAudienceSecret::decode_config(&secret.encode_config()).unwrap();
    let dispatcher = OrgDispatcherGrant::try_issue(
        &org_a,
        caller_identity.entity_id().clone(),
        DispatcherScope::Exact(capability),
        3600,
    )
    .unwrap();

    provider.start().expect("mesh start");
    caller.start().expect("mesh start");
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
    // Registration first, then the grant audience (the provisioning contract).
    let service = provider
        .serve_org(
            "protected.echo",
            OrgAccess::Granted,
            move |who: OrgCaller, request: Message| {
                let observed = observed.clone();
                async move {
                    observed.lock().await.push((who, request.clone()));
                    Ok(request)
                }
            },
        )
        .unwrap();
    provider
        .inner()
        .install_provider_grant_audience(grant.clone(), provider_secret)
        .unwrap();
    let org = caller
        .org(
            OrgCredentials::new(caller_cert, dispatcher, vec![grant.clone()], vec![secret])
                .unwrap(),
        )
        .unwrap();

    // Poll private discovery through the grant, never the business RPC.
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
                    .org_cold_discovery(&capability, &[grant.grant_id])
                    .is_ok_and(|capture| !capture.granted_providers(&grant.grant_id).is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("granted private discovery and peer identities converge");

    let request = Message {
        text: "authorized cross-org consumer".into(),
    };
    // Knowing the service and holding a transport session is not authority.
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut consumer = Command::new(python);
    consumer
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/protected_consumer.py"
        ))
        .arg(dirs.path())
        .arg(listener.local_addr().unwrap().to_string());
    let generated_request = Message {
        text: "generated protected consumer".into(),
    };
    let forward = async {
        tokio::time::timeout(Duration::from_secs(15), async {
            // The fixture owns the sequence; Python never holds credentials.
            for authorized in [false, true] {
                let (socket, _) = listener.accept().await.unwrap();
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                let wire: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(wire["tool_id"], "protected.echo");
                let input: Message = serde_json::from_value(wire["input"].clone()).unwrap();
                assert_eq!(input, generated_request);
                let reply = if authorized {
                    let response: Message = tokio::time::timeout(
                        Duration::from_secs(5),
                        org.call("protected.echo", &input),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    json!({"result":response})
                } else {
                    let result: Result<Message, _> = tokio::time::timeout(
                        Duration::from_secs(5),
                        caller.call_typed(
                            provider.node_id(),
                            "protected.echo",
                            &input,
                            CallOptionsTyped {
                                raw: Default::default(),
                                codec: Codec::Json,
                            },
                        ),
                    )
                    .await
                    .unwrap();
                    assert!(
                        matches!(&result, Err(RpcError::ServerError { status: 0x0009, .. })),
                        "{result:?}"
                    );
                    assert_eq!(
                        records.lock().await.len(),
                        1,
                        "the generated denial has no handler effect"
                    );
                    json!({"error":{"status":0x0009}})
                };
                write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        })
        .await
        .expect("bounded generated cross-org journey");
    };
    let (generated, ()) = tokio::join!(output(&mut consumer), forward);
    assert_eq!(
        serde_json::from_slice::<Message>(&generated.stdout).unwrap(),
        generated_request
    );
    {
        let records = records.lock().await;
        assert_eq!(records.len(), 2, "native and generated admitted once each");
        assert_eq!(records[0].1, request);
        assert_eq!(records[1].1, generated_request);
        for (who, _) in records.iter() {
            assert_eq!(&who.entity, caller_identity.entity_id());
            assert_eq!(&who.provider, provider_identity.entity_id());
            assert_eq!(who.acting_org, org_a.org_id(), "acting for A");
            assert_eq!(
                who.provider_org,
                org_b.org_id(),
                "under B's grant, on B's node"
            );
            assert_eq!(who.capability, capability);
            assert!(!who.is_same_org(), "a cross-org call, never same-org");
        }
    }
    drop(service);
    drop(org);
    caller.shutdown().await.unwrap();
    provider.shutdown().await.unwrap();
}
