#![cfg(all(feature="webrtc",feature="fixtures",feature="cortex"))]
use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use bytes::Bytes;
use net::adapter::Adapter;
use net::adapter::net::{EntityKeypair,MeshNode,MeshNodeConfig,SocketBufferConfig,PeerAddr};
use net::adapter::net::rtc::{RtcConfig,connect_rtc_loopback,open_rtc_channel,ENROLL_SERVICE};
use net::adapter::net::cortex::{RpcHandler,RpcContext,RpcResponsePayload,RpcHandlerError,RpcStatus};
use net::adapter::net::mesh_rpc::CallOptions;
fn config(anchor:bool)->MeshNodeConfig {
 let mut c=MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(),[0x5c;32]).with_session_timeout(Duration::from_secs(60));
 c.socket_buffers=SocketBufferConfig::for_testing();c.rtc=Some(RtcConfig{serve_bootstrap:anchor,ice_deadline:Duration::from_secs(2),..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().unwrap())});c
}
async fn node(anchor:bool)->Arc<MeshNode>{let n=Arc::new(MeshNode::new(EntityKeypair::generate(),config(anchor)).await.unwrap());n.start_arc();n}
async fn transit(installed:bool)->bool {
 let anchor=node(true).await;let client=node(false).await;
 let (id,_)=if installed {connect_rtc_loopback(&client,&anchor).await.unwrap()}else{open_rtc_channel(&client,&anchor).await.unwrap()};
 assert_eq!(anchor.peer_is_provisional(client.node_id()),installed);
 let sink=tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();let destination=0x772255_u64;
 anchor.router().add_route(destination,PeerAddr::Udp(sink.local_addr().unwrap()));
 let header=net_wire::protocol::NetHeader::new(1,2,3,[0;net_wire::protocol::NONCE_SIZE],0,1,net_wire::protocol::PacketFlags::NONE);
 let routing=net_wire::route_codec::RoutingHeader::new(destination,0x554433,8);
 let mut payload=routing.to_bytes().to_vec();payload.extend_from_slice(&header.to_bytes());payload.extend_from_slice(b"KYRA-TRANSIT-MARKER");
 client.rtc_driver().unwrap().transport().submit(&payload,id).unwrap();
 let mut bytes=[0;8192];let received=tokio::time::timeout(Duration::from_millis(800),sink.recv_from(&mut bytes)).await;
 let forwarded=matches!(&received,Ok(Ok((n,_))) if bytes[..*n].ends_with(b"KYRA-TRANSIT-MARKER"));
 eprintln!("KYRA_TRANSIT installed_provisional={installed} actual_udp_marker={forwarded} refused={}",anchor.rtc_stats().admission_refused_transit());
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();forwarded
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_pre_noise_rtc_cannot_forward(){assert!(!transit(false).await,"unregistered RTC channel forwarded to a third-party UDP route before Noise/enrollment");}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_installed_provisional_transit_control(){assert!(!transit(true).await,"installed provisional must refuse transit");}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_provisional_app_delivery_is_denied(){
 let anchor=node(true).await;let client=node(false).await;connect_rtc_loopback(&client,&anchor).await.unwrap();
 let event=net::event::InternalEvent::from_value(serde_json::json!({"tag":"KYRA-UNENROLLED-APP"}),0,0);
 let batch=net::event::Batch{shard_id:0,events:vec![event],sequence_start:0,process_nonce:net::event::batch_process_nonce()};
 client.send_to_peer_node(anchor.node_id(),&batch).await.unwrap();
 let deadline=tokio::time::Instant::now()+Duration::from_secs(2);let mut seen=false;
 while tokio::time::Instant::now()<deadline && !seen {
  for shard in 0..4 {let result=anchor.poll_shard(shard,None,512).await.unwrap();seen|=result.events.iter().any(|e|String::from_utf8_lossy(e.raw.as_ref()).contains("KYRA-UNENROLLED-APP"));}
  tokio::time::sleep(Duration::from_millis(10)).await;
 }
 eprintln!("KYRA_APP delivered={seen} still_provisional={}",anchor.peer_is_provisional(client.node_id()));
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();assert!(!seen,"ordinary application payload reached provisional anchor delivery");
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_provisional_signal_does_not_allocate_ice(){
 let anchor=node(true).await;let client=node(false).await;connect_rtc_loopback(&client,&anchor).await.unwrap();
 let before=anchor.rtc_stats().ice_attempted();let (_,sdp)=client.rtc_driver().unwrap().create_offer().await.unwrap();
 client.send_rtc_signal(anchor.node_id(),&net::adapter::net::rtc::RtcSignalMsg::Offer{dialog:777,sdp}).await.unwrap();
 let deadline=tokio::time::Instant::now()+Duration::from_secs(2);
 while tokio::time::Instant::now()<deadline && anchor.rtc_stats().ice_attempted()==before{tokio::time::sleep(Duration::from_millis(10)).await;}
 let attempts=anchor.rtc_stats().ice_attempted()-before;
 eprintln!("KYRA_SIGNAL ice_allocations={attempts} delivered={} still_provisional={}",anchor.rtc_stats().signal_delivered(),anchor.peer_is_provisional(client.node_id()));
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();assert_eq!(attempts,0,"provisional signalling reached the ICE engine");
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_normal_close_reclaims_provisional_projection(){
 let anchor=node(true).await;let client=node(false).await;let (_,anchor_id)=connect_rtc_loopback(&client,&anchor).await.unwrap();
 assert_eq!(anchor.provisional_count(),1);anchor.rtc_driver().unwrap().close(anchor_id).await.unwrap();
 let deadline=tokio::time::Instant::now()+Duration::from_secs(2);
 while tokio::time::Instant::now()<deadline && anchor.peer_endpoint(client.node_id()).is_some(){tokio::time::sleep(Duration::from_millis(10)).await;}
 let gone=anchor.peer_endpoint(client.node_id()).is_none();let projection=anchor.provisional_count();
 eprintln!("KYRA_CLOSE peer_gone={gone} provisional_projection={projection}");
 anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();assert!(gone,"precondition actual ordinary eviction");assert_eq!(projection,0,"closed provisional endpoint remains in admission projection");
}
struct BlockedOutcomes {calls:AtomicUsize,entered:tokio::sync::mpsc::UnboundedSender<usize>,old:Arc<tokio::sync::Notify>,new:Arc<tokio::sync::Notify>}
#[async_trait::async_trait]
impl RpcHandler for BlockedOutcomes {
 async fn call(&self,_:RpcContext)->Result<RpcResponsePayload,RpcHandlerError>{
  let i=self.calls.fetch_add(1,Ordering::SeqCst);self.entered.send(i).unwrap();
  if i==0{self.old.notified().await;}else{self.new.notified().await;}
  let mut body=b"NMO1".to_vec();body.push(if i==0{0}else{1});body.extend_from_slice(&0u32.to_le_bytes());if i!=0{body.extend_from_slice(&0u32.to_le_bytes());}
  Ok(RpcResponsePayload{status:RpcStatus::Ok,headers:vec![],body:body.into()})
 }
}
#[tokio::test(flavor="multi_thread",worker_threads=8)]
async fn kyra_old_success_cannot_promote_replacement_request(){
 let anchor=node(true).await;let client=node(false).await;let (c1,a1)=connect_rtc_loopback(&client,&anchor).await.unwrap();
 let old_sid=anchor.peer_session_id(client.node_id()).unwrap();let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel();
 let old=Arc::new(tokio::sync::Notify::new());let new=Arc::new(tokio::sync::Notify::new());
 let _serve=anchor.serve_rpc(ENROLL_SERVICE,Arc::new(BlockedOutcomes{calls:AtomicUsize::new(0),entered:tx,old:old.clone(),new:new.clone()})).unwrap();
 let first={let c=client.clone();let id=anchor.node_id();tokio::spawn(async move{c.call(id,ENROLL_SERVICE,Bytes::from_static(b"old"),CallOptions::default()).await})};
 assert_eq!(tokio::time::timeout(Duration::from_secs(3),rx.recv()).await.unwrap(),Some(0));
 anchor.rtc_driver().unwrap().close(a1).await.unwrap();client.rtc_driver().unwrap().close(c1).await.unwrap();
 let until=tokio::time::Instant::now()+Duration::from_secs(3);
 while tokio::time::Instant::now()<until && (anchor.peer_endpoint(client.node_id()).is_some()||client.peer_endpoint(anchor.node_id()).is_some()){tokio::time::sleep(Duration::from_millis(10)).await;}
 assert!(anchor.peer_endpoint(client.node_id()).is_none()&&client.peer_endpoint(anchor.node_id()).is_none(),"both old peers evicted before reconnection");
 connect_rtc_loopback(&client,&anchor).await.unwrap();let new_sid=anchor.peer_session_id(client.node_id()).unwrap();assert_ne!(old_sid,new_sid);assert!(anchor.peer_is_provisional(client.node_id()));
 let second={let c=client.clone();let id=anchor.node_id();tokio::spawn(async move{c.call(id,ENROLL_SERVICE,Bytes::from_static(b"new"),CallOptions::default()).await})};
 assert_eq!(tokio::time::timeout(Duration::from_secs(3),rx.recv()).await.unwrap(),Some(1),"new request must really reach a separate handler");
 old.notify_one();let until=tokio::time::Instant::now()+Duration::from_secs(2);
 while tokio::time::Instant::now()<until && anchor.peer_is_provisional(client.node_id()){tokio::time::sleep(Duration::from_millis(10)).await;}
 let wrongly_promoted=!anchor.peer_is_provisional(client.node_id());let same_new=anchor.peer_session_id(client.node_id())==Some(new_sid);
 eprintln!("KYRA_PROMOTION two_real_calls=true distinct_sessions=true second_handler_unreleased=true new_session_still_installed={same_new} old_success_promoted_replacement={wrongly_promoted}");
 new.notify_one();first.abort();second.abort();anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();
 assert!(same_new && !wrongly_promoted,"old enrollment success consumed replacement call's pending promotion");
}
struct RejectCounter(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl RpcHandler for RejectCounter {
 async fn call(&self,_:RpcContext)->Result<RpcResponsePayload,RpcHandlerError>{self.0.fetch_add(1,Ordering::SeqCst);let mut body=b"NMO1".to_vec();body.push(1);body.extend_from_slice(&7u32.to_le_bytes());body.extend_from_slice(&0u32.to_le_bytes());Ok(RpcResponsePayload{status:RpcStatus::Ok,headers:vec![],body:body.into()})}
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_fifth_enrollment_request_is_not_executed(){
 let anchor=node(true).await;let client=node(false).await;connect_rtc_loopback(&client,&anchor).await.unwrap();
 let count=Arc::new(AtomicUsize::new(0));let _serve=anchor.serve_rpc(ENROLL_SERVICE,Arc::new(RejectCounter(count.clone()))).unwrap();
 for index in 0..5 {
  let result=tokio::time::timeout(Duration::from_secs(4),client.call(anchor.node_id(),ENROLL_SERVICE,Bytes::from_static(b"join request"),CallOptions::default())).await;
  eprintln!("KYRA_ENROLL index={index} returned_ok={} executions={} provisional={}",matches!(result,Ok(Ok(_))),count.load(Ordering::SeqCst),anchor.peer_is_provisional(client.node_id()));
  if index<4{assert!(matches!(result,Ok(Ok(_))),"four-request positive control must reach service");}
 }
 let executions=count.load(Ordering::SeqCst);anchor.shutdown().await.unwrap();client.shutdown().await.unwrap();
 assert!(executions<=4,"whole provisional session allows initial + 3 retries, executed {executions}");
}
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_engine_must_install_without_loopback_noise_fixture(){
 let a=Arc::new(MeshNode::new(EntityKeypair::generate(),config(false)).await.unwrap());
 let r=Arc::new(MeshNode::new(EntityKeypair::generate(),config(false)).await.unwrap());
 let b=Arc::new(MeshNode::new(EntityKeypair::generate(),config(false)).await.unwrap());
 for (left,right) in [(&a,&r),(&r,&b)] {
  let id=left.node_id();let other=right.clone();let accept=tokio::spawn(async move{other.accept(id).await});
  left.connect(right.local_addr(),right.public_key(),right.node_id()).await.unwrap();accept.await.unwrap().unwrap();
 }
 a.start_arc();r.start_arc();b.start_arc();
 a.connect_via(r.local_addr(),b.public_key(),b.node_id()).await.unwrap();
 assert!(!a.peer_is_direct(b.node_id()));
 let old=a.peer_session_id(b.node_id()).unwrap();a.offer_direct_path(b.node_id()).await.unwrap();
 let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
 while tokio::time::Instant::now()<deadline && !matches!(a.peer_endpoint(b.node_id()),Some(PeerAddr::Rtc(_))){tokio::time::sleep(Duration::from_millis(20)).await;}
 let endpoint=a.peer_endpoint(b.node_id());let same=a.peer_session_id(b.node_id())==Some(old);
 eprintln!("KYRA_ENGINE endpoint={endpoint:?} old_session_unchanged={same} attempted={} relayed={}",a.rtc_stats().ice_attempted(),a.rtc_stats().ice_relayed());
 a.shutdown().await.unwrap();b.shutdown().await.unwrap();assert!(matches!(endpoint,Some(PeerAddr::Rtc(_))),"real signaling did not install RTC; fixture-free native engine completion is missing");
}
