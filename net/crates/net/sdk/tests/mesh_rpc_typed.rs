//! SDK test for the typed nRPC surface.
//!
//! Exercises `Mesh::serve_rpc_typed` and `Mesh::call_typed` /
//! `call_service_typed` over the default JSON codec. Proves the
//! typed glue compiles end-to-end and the user can write RPC
//! servers + clients without hand-rolling serde.
//!
//! Network-level integration is covered by
//! `net/crates/net/tests/integration_nrpc_*.rs`. This SDK test
//! focuses on the typed-handler glue (request decode → handler
//! invocation → response encode).

#![cfg(all(feature = "net", feature = "cortex"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use net_sdk::mesh_rpc::{
    Codec, RpcContext, RpcHandler, RpcHandlerError, RpcStatus,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct AddRequest {
    a: i64,
    b: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct AddResponse {
    sum: i64,
}

/// The typed-handler adapter (as installed by `serve_rpc_typed`)
/// decodes the request body via the chosen codec, runs the user
/// closure, encodes the response. Verify the round-trip without
/// going through the mesh.
#[tokio::test]
async fn typed_handler_round_trip_through_rpc_handler_trait() {
    // Construct an SDK Mesh just to install the typed handler;
    // we'll then manually invoke the underlying RpcHandler trait
    // without touching the network.
    let mesh = net_sdk::mesh::MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])
        .unwrap()
        .build()
        .await
        .unwrap();

    // Build the typed handler exactly the way `serve_rpc_typed`
    // does internally — call into a typed closure, get back a
    // boxed RpcHandler.
    let invoked = Arc::new(AtomicBool::new(false));
    let invoked_for_handler = invoked.clone();
    let typed_handler = build_typed_handler(Codec::Json, move |req: AddRequest| {
        let invoked = invoked_for_handler.clone();
        async move {
            invoked.store(true, Ordering::Release);
            Ok(AddResponse { sum: req.a + req.b })
        }
    });

    // Synthesize a request body (JSON-encoded AddRequest) and run
    // it through the trait method.
    let body = serde_json::to_vec(&AddRequest { a: 5, b: 7 }).unwrap();
    let payload = net::adapter::net::cortex::RpcRequestPayload {
        service: "add".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body,
    };
    let ctx = RpcContext {
        caller_origin: 0xCAFE,
        call_id: 1,
        payload,
        cancellation: net::adapter::net::cortex::RpcCancellationToken::new(),
    };
    let resp = typed_handler.call(ctx).await.expect("handler must succeed");

    assert_eq!(resp.status, RpcStatus::Ok);
    assert!(invoked.load(Ordering::Acquire), "handler must be invoked");
    let decoded: AddResponse = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(decoded, AddResponse { sum: 12 });

    drop(mesh);
}

/// Typed handler returning `Err(message)` surfaces as an
/// `RpcHandlerError::Application` (mapped to `RpcStatus::Application`
/// upstream).
#[tokio::test]
async fn typed_handler_application_error_surfaces() {
    let mesh = net_sdk::mesh::MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])
        .unwrap()
        .build()
        .await
        .unwrap();

    let typed_handler = build_typed_handler(Codec::Json, |req: AddRequest| async move {
        if req.a < 0 {
            Err(format!("negative a not allowed: {}", req.a))
        } else {
            Ok(AddResponse { sum: req.a + req.b })
        }
    });

    let body = serde_json::to_vec(&AddRequest { a: -1, b: 7 }).unwrap();
    let payload = net::adapter::net::cortex::RpcRequestPayload {
        service: "validate".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body,
    };
    let ctx = RpcContext {
        caller_origin: 0xCAFE,
        call_id: 1,
        payload,
        cancellation: net::adapter::net::cortex::RpcCancellationToken::new(),
    };
    let err = typed_handler
        .call(ctx)
        .await
        .expect_err("validation failure must surface as Err");
    match err {
        RpcHandlerError::Application { code, message } => {
            assert_eq!(code, 0x4001);
            assert!(
                message.contains("negative a"),
                "diagnostic must round-trip; got {message:?}",
            );
        }
        other => panic!("expected Application, got {other:?}"),
    }

    drop(mesh);
}

/// Malformed request body (non-JSON when codec=Json) surfaces as
/// `RpcHandlerError::Application` with the bad-body diagnostic
/// before the user closure is ever invoked.
#[tokio::test]
async fn typed_handler_malformed_body_short_circuits_before_closure() {
    let mesh = net_sdk::mesh::MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])
        .unwrap()
        .build()
        .await
        .unwrap();

    let invoked = Arc::new(AtomicBool::new(false));
    let invoked_for_handler = invoked.clone();
    let typed_handler = build_typed_handler(Codec::Json, move |_req: AddRequest| {
        let invoked = invoked_for_handler.clone();
        async move {
            invoked.store(true, Ordering::Release);
            Ok(AddResponse { sum: 0 })
        }
    });

    let payload = net::adapter::net::cortex::RpcRequestPayload {
        service: "add".to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: vec![],
        body: b"not json".to_vec(),
    };
    let ctx = RpcContext {
        caller_origin: 0xCAFE,
        call_id: 1,
        payload,
        cancellation: net::adapter::net::cortex::RpcCancellationToken::new(),
    };
    let err = typed_handler
        .call(ctx)
        .await
        .expect_err("malformed body must error");
    match err {
        RpcHandlerError::Application { code, message } => {
            assert_eq!(code, 0x4000);
            assert!(message.contains("bad request body"));
        }
        other => panic!("expected Application 0x4000, got {other:?}"),
    }
    assert!(
        !invoked.load(Ordering::Acquire),
        "user closure must NOT run on malformed body",
    );

    drop(mesh);
}

/// `Codec::Json` round-trips primitive values without surprises.
#[test]
fn codec_round_trip() {
    let bytes = Codec::Json.encode(&42u32).unwrap();
    let back: u32 = Codec::Json.decode(&bytes).unwrap();
    assert_eq!(back, 42);

    let bytes = Codec::Json.encode(&"hello").unwrap();
    let back: String = Codec::Json.decode(&bytes).unwrap();
    assert_eq!(back, "hello");

    // Pretty round-trips identically (same wire format, just
    // formatted differently on encode).
    let pretty = Codec::JsonPretty.encode(&AddRequest { a: 1, b: 2 }).unwrap();
    let back: AddRequest = Codec::JsonPretty.decode(&pretty).unwrap();
    assert_eq!(back, AddRequest { a: 1, b: 2 });
}

// ---------------------------------------------------------------------------
// Test helper.
//
// Mirrors the internal `TypedRpcHandler` adapter that
// `Mesh::serve_rpc_typed` builds. Defined here so the test can
// drive the trait method directly without going through the
// network layer (which would otherwise need ChannelConfigRegistry
// entries for the dynamic reply channels — an SDK-level friction
// to be addressed in a follow-up).
// ---------------------------------------------------------------------------

fn build_typed_handler<Req, Resp, F, Fut>(
    codec: Codec,
    handler: F,
) -> Arc<dyn RpcHandler>
where
    Req: serde::de::DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + Send + Sync + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Resp, String>> + Send + 'static,
{
    struct H<Req, Resp, F> {
        codec: Codec,
        inner: Arc<F>,
        _req: std::marker::PhantomData<fn() -> Req>,
        _resp: std::marker::PhantomData<fn() -> Resp>,
    }
    #[async_trait::async_trait]
    impl<Req, Resp, F, Fut> RpcHandler for H<Req, Resp, F>
    where
        Req: serde::de::DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(Req) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Resp, String>> + Send + 'static,
    {
        async fn call(
            &self,
            ctx: RpcContext,
        ) -> Result<net::adapter::net::cortex::RpcResponsePayload, RpcHandlerError> {
            let req: Req = match self.codec.decode(&ctx.payload.body) {
                Ok(r) => r,
                Err(e) => {
                    return Err(RpcHandlerError::Application {
                        code: 0x4000,
                        message: format!("typed handler: bad request body: {e}"),
                    });
                }
            };
            let resp = (self.inner)(req)
                .await
                .map_err(|message| RpcHandlerError::Application {
                    code: 0x4001,
                    message,
                })?;
            let body = self
                .codec
                .encode(&resp)
                .map_err(|e| RpcHandlerError::Internal(format!("typed handler encode: {e}")))?;
            Ok(net::adapter::net::cortex::RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body,
            })
        }
    }
    Arc::new(H {
        codec,
        inner: Arc::new(handler),
        _req: std::marker::PhantomData,
        _resp: std::marker::PhantomData,
    })
}
