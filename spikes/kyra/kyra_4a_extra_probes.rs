
struct KyraParkedOutcome { entered: tokio::sync::mpsc::UnboundedSender<()>, release: Arc<tokio::sync::Notify> }
#[async_trait::async_trait]
impl RpcHandler for KyraParkedOutcome {
 async fn call(&self,_:RpcContext)->Result<RpcResponsePayload,RpcHandlerError>{
  self.entered.send(()).unwrap();self.release.notified().await;
  Ok(RpcResponsePayload{status:RpcStatus::Ok,headers:vec![],body:Bytes::from_static(b"NMO1\0\0\0\0\0")})
 }
}
#[tokio::test(flavor="multi_thread",worker_threads=8)]
async fn kyra_same_call_id_old_success_must_not_promote_replacement(){
 let anchor=node(true).await;let client=node(false).await;let (c1,a1)=connect_rtc_loopback(&client,&anchor).await.unwrap();
 let old_sid=anchor.peer_session_id(client.node_id()).unwrap();let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel();let release=Arc::new(tokio::sync::Notify::new());
 let _serve=anchor.serve_rpc(ENROLL_SERVICE,Arc::new(KyraParkedOutcome{entered:tx,release:release.clone()})).unwrap();
 let call=0xC001;
 client.kyra_publish_fixed_request(call,anchor.node_id(),ENROLL_SERVICE,Bytes::from_static(b"old")).await.unwrap();
 tokio::time::timeout(Duration::from_secs(3),rx.recv()).await.unwrap().unwrap();
 assert!(anchor.kyra_has_enrollment_reservation(client.node_id(),old_sid,call));
 anchor.rtc_driver().unwrap().close(a1).await.unwrap();client.rtc_driver().unwrap().close(c1).await.unwrap();
 let until=tokio::time::Instant::now()+Duration::from_secs(3);
 while tokio::time::Instant::now()<until && (anchor.peer_endpoint(client.node_id()).is_some()||client.peer_endpoint(anchor.node_id()).is_some()){tokio::time::sleep(Duration::from_millis(10)).await;}
 assert!(anchor.peer_endpoint(client.node_id()).is_none()&&client.peer_endpoint(anchor.node_id()).is_none());
 connect_rtc_loopback(&client,&anchor).await.unwrap();let new_sid=anchor.peer_session_id(client.node_id()).unwrap();assert_ne!(old_sid,new_sid);
 client.kyra_publish_fixed_request(call,anchor.node_id(),ENROLL_SERVICE,Bytes::from_static(b"new")).await.unwrap();
 let until=tokio::time::Instant::now()+Duration::from_secs(3);
 while tokio::time::Instant::now()<until && !anchor.kyra_has_enrollment_reservation(client.node_id(),new_sid,call){tokio::time::sleep(Duration::from_millis(10)).await;}
 assert!(anchor.kyra_has_enrollment_reservation(client.node_id(),new_sid,call),"real replacement REQUEST must reserve before releasing old outcome");
 assert!(anchor.peer_is_provisional(client.node_id()));release.notify_one();
 let until=tokio::time::Instant::now()+Duration::from_secs(2);
 while tokio::time::Instant::now()<until && anchor.peer_is_provisional(client.node_id()){tokio::time::sleep(Duration::from_millis(10)).await;}
 let promoted=!anchor.peer_is_provisional(client.node_id());let same_new=anchor.peer_session_id(client.node_id())==Some(new_sid);
 eprintln!("KYRA_SAME_CALL real_requests=true same_call_id=true distinct_sessions=true successor_still_installed={same_new} old_success_promoted_successor={promoted}");
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();assert!(same_new&&!promoted,"same-call old completion promoted replacement");
}
#[test]
fn kyra_failed_inflight_reservation_does_not_create_an_owner(){
 let mut b=net::adapter::net::rtc::ProvisionalBudget::default();assert!(b.reserve_enrollment().is_ok());assert!(b.reserve_enrollment().is_err());b.release_enrollment();
 eprintln!("KYRA_RESERVE after_one_owner_released={}",b.inflight_enrollments);assert_eq!(b.inflight_enrollments,0,"refused reservation left a phantom owner");
}
struct KyraInternalOutcome(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl RpcHandler for KyraInternalOutcome {
 async fn call(&self,_:RpcContext)->Result<RpcResponsePayload,RpcHandlerError>{self.0.fetch_add(1,Ordering::SeqCst);Err(RpcHandlerError::Internal("deliberate terminal handler error".into()))}
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_terminal_handler_error_releases_enrollment_owner(){
 let anchor=node(true).await;let client=node(false).await;connect_rtc_loopback(&client,&anchor).await.unwrap();
 let handler=Arc::new(KyraInternalOutcome(std::sync::atomic::AtomicUsize::new(0)));let _serve=anchor.serve_rpc(ENROLL_SERVICE,handler.clone()).unwrap();
 let first=tokio::time::timeout(Duration::from_secs(3),client.call(anchor.node_id(),ENROLL_SERVICE,Bytes::from_static(b"one"),CallOptions::default())).await;assert!(first.is_ok(),"actual terminal response must complete, not outer timeout");assert_eq!(handler.0.load(Ordering::SeqCst),1);
 let before=anchor.rtc_stats().admission_refused_deliver();
 client.kyra_publish_fixed_request(0xC003,anchor.node_id(),ENROLL_SERVICE,Bytes::from_static(b"two")).await.unwrap();
 let until=tokio::time::Instant::now()+Duration::from_secs(3);
 while tokio::time::Instant::now()<until && anchor.rtc_stats().admission_refused_deliver()==before && handler.0.load(Ordering::SeqCst)<2 {tokio::time::sleep(Duration::from_millis(10)).await;}
 let calls=handler.0.load(Ordering::SeqCst);let refused=anchor.rtc_stats().admission_refused_deliver()>before;
 assert!(refused||calls==2,"second request must reach the actual gate or handler");eprintln!("KYRA_TERMINAL completed_error_response=true second_request_refused_at_gate={refused} subsequent_handler_calls={calls}");
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();assert_eq!(calls,2,"terminal error stranded enrollment slot");
}
