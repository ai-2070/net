// Parent-authored Stage4B probes. All credentials are ephemeral fixtures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_acme_cache_must_match_the_requested_domain() {
    let anchor = kyra_long_lived_anchor().await;
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key) = issue_localhost_certificate();
    std::fs::write(dir.path().join("bootstrap-cert.pem"), cert).unwrap();
    std::fs::write(dir.path().join("bootstrap-key.pem"), key).unwrap();
    let mut cfg = config(PSK);
    cfg.tls = BootstrapTls::Acme(net_sdk::rtc_bootstrap::AcmeConfig {
        directory_url: "http://127.0.0.1:9/directory".into(),
        domain: "different.example".into(),
        contact_email: "review@example.invalid".into(),
        cache_dir: dir.path().into(),
    });
    let result = serve_bootstrap(Arc::clone(&anchor), cfg).await;
    let accepted_wrong_name = result.is_ok();
    if let Ok(handle) = result {
        handle.shutdown().await;
    }
    anchor.shutdown().await.unwrap();
    assert!(
        !accepted_wrong_name,
        "ACME returned a cached localhost certificate for different.example without ordering"
    );
}

use net::adapter::Adapter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn kyra_offer_debug_must_not_expose_the_encoded_credential() {
    let credential = credential_for(PSK, Duration::from_secs(600));
    let encoded = credential.encode();
    let request = net_sdk::rtc_bootstrap::OfferRequest {
        credential: encoded.clone(),
        node_id: "7".into(),
        sdp: "v=0".into(),
    };
    let exposed = format!("{request:?}").contains(&encoded);
    // Never print fixture credentials, even when this expected failure fires.
    assert!(
        !exposed,
        "OfferRequest Debug includes the complete PSK-bearing credential"
    );
}

async fn kyra_long_lived_anchor() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK);
    cfg.rtc = Some(RtcConfig {
        ice_deadline: Duration::from_secs(30),
        ..rtc_config()
    });
    let node = Arc::new(MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap());
    node.start_arc();
    node
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_uncredentialed_websocket_cannot_retire_another_offer() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(anchor.open_signal_dialogs(offerer.node_id()), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    // A distinct client: no credential, no session cookie, only a guessed
    // sequential dialog and public/known victim node id. Origin is NOT auth.
    let request = format!("GET /rtc/trickle?dialog={}&node_id={} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: AQEBAQEBAQEBAQEBAQEBAQ==\r\nOrigin: {}\r\n\r\n", offered.dialog, offerer.node_id(), addr, ORIGIN);
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !headers.ends_with(b"\r\n\r\n") && headers.len() < 8192 {
            headers.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    let upgraded = String::from_utf8_lossy(&headers).starts_with("HTTP/1.1 101");
    if upgraded {
        socket.write_all(&[0x88, 0x80, 1, 2, 3, 4]).await.unwrap();
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(1);
    while anchor.open_signal_dialogs(offerer.node_id()) == 1 && tokio::time::Instant::now() < until
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let retained = anchor.open_signal_dialogs(offerer.node_id());
    eprintln!("kyra_trickle: uncredentialed_upgrade={upgraded} victim_dialogs_after={retained}");
    drop(socket);
    server.abort();
    let _ = server.await;
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert_eq!(
        retained, 1,
        "a credential-free second client retired the victim's live ICE attempt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_expired_credential_holder_cannot_extend_its_own_deadlines() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let mut credential = credential_for(PSK, Duration::from_secs(600));
    credential.invite.expires_at = 1;
    credential.psk_expires_at = 1;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    let (before, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(before, StatusCode::BAD_REQUEST);
    assert_eq!(refusal_of(&body), "expired_credential");
    // Alter only the two expiry fields; do not possess any issuer key.
    credential.invite.expires_at = u64::MAX;
    credential.psk_expires_at = u64::MAX;
    let (after, _) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    eprintln!("kyra_expiry: original_status={before} edited_status={after}");
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert_ne!(
        after,
        StatusCode::OK,
        "caller-edited lifetimes revive the expired bootstrap credential"
    );
}
