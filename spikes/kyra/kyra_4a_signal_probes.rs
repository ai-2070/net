
#[tokio::test(flavor="multi_thread",worker_threads=6)]
async fn kyra_unknown_candidates_cannot_own_dialog_budget(){
 let (a,b)=signalling_pair().await;let before=b.rtc_stats().signal_delivered();
 for dialog in 0x701..0x705 {a.send_rtc_signal(b.node_id(),&RtcSignalMsg::Candidate{mid:"0".into(),dialog,candidate:"candidate:1 1 udp 2130706431 127.0.0.1 4444 typ host".into()}).await.unwrap();}
 assert!(wait_for(|| b.rtc_stats().signal_delivered()>=before+4,Duration::from_secs(3)).await,"four frames really received");
 tokio::time::sleep(Duration::from_secs(3)).await;
 let slots=b.open_signal_dialogs(a.node_id());eprintln!("KYRA_UNKNOWN_CANDIDATES received=4 past_ice_deadline=true phantom_budget_slots={slots}");
 assert_eq!(slots,0,"ignored unknown candidates have no engine owner but retain reservations");
}
#[test]
fn kyra_reject_then_late_candidate_does_not_resurrect_reservation(){
 use net::adapter::net::rtc::{SignalBudget,RtcRejectReason};let mut b=SignalBudget::new();let now=std::time::Instant::now();
 b.note_outbound_dialog(7,42);
 b.admit(7,&RtcSignalMsg::Reject{dialog:42,reason:RtcRejectReason::Declined},now);
 assert_eq!(b.open_dialogs(7),0);
 b.admit(7,&RtcSignalMsg::Candidate{mid:"0".into(),dialog:42,candidate:"late".into()},now);
 eprintln!("KYRA_LATE_CANDIDATE rejected_dialog_recreated={}",b.open_dialogs(7));assert_eq!(b.open_dialogs(7),0,"late candidate resurrected retired budget id");
}
