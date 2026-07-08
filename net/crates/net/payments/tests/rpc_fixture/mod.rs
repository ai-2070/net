//! Shared test fixture: a minimal one-connection-per-accept HTTP/1.1 JSON
//! server for the chain-checker suites. The accept loop, header scan,
//! content-length parse, bounded body read, and framed write are identical
//! across the eip155 / svm / xrpl fixtures — only the per-chain method
//! dispatch and response envelope differ, and those live in the responder
//! closure each suite passes in. Kept here once instead of three copies.
//!
//! Not a test target (it lives under a subdirectory of `tests/`); each
//! suite pulls it in with `mod rpc_fixture;`.

#![allow(dead_code)]

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A scripted JSON-over-HTTP endpoint. `endpoint` is the base URL to hand
/// the checker; every request is parsed as JSON and answered with
/// `responder(request)` serialized as the response body.
pub struct HttpJsonServer {
    pub endpoint: String,
}

impl HttpJsonServer {
    /// Start the server on an ephemeral port. `responder` is invoked for
    /// each request with the parsed request JSON and returns the full
    /// response JSON (envelope included) — it reads whatever shared state
    /// the suite mutates between calls.
    pub async fn start<F>(responder: F) -> Self
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
        let responder = Arc::new(responder);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let responder = responder.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    let content_length: usize = head_text
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    let mut body = buf[header_end..].to_vec();
                    while body.len() < content_length {
                        let Ok(n) = stream.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        body.extend_from_slice(&tmp[..n]);
                    }
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let response = responder(request).to_string();
                    let head_out = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        response.len()
                    );
                    let _ = stream.write_all(head_out.as_bytes()).await;
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { endpoint }
    }
}
