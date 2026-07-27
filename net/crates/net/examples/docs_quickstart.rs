//! The compiled counterpart of the docs Quickstart page.
//!
//! Every snippet in `web/src/content/docs/start/quickstart.md` appears here
//! verbatim so that CI compiles it. The page shipped for several releases
//! with three snippets that did not build — `AdapterConfig::net()` (never
//! existed), `Filter::new().eq(..)` (wrong argument type), and
//! `println!("{}", event.raw)` (`raw` is `Bytes`) — plus a fourth that
//! built but could not do what the prose claimed: it polled events back
//! through the no-op adapter, which discards them, so the "you'll see both
//! events printed" loop always printed nothing. Nothing linked the prose to
//! the compiler. Change one, change the other.

use std::sync::atomic::Ordering;

use net::{ConsumeRequest, Event, EventBus, EventBusConfig, Filter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bus = EventBus::new(EventBusConfig::default()).await?;

    bus.ingest(Event::from_str(r#"{"token": "hello", "index": 0}"#)?)?;
    bus.ingest(Event::from_str(r#"{"token": "world", "index": 1}"#)?)?;

    // Ingest is non-blocking; flush waits for the batch workers to drain.
    bus.flush().await?;

    let stats = bus.stats();
    println!(
        "ingested={} dispatched={} dropped={}",
        stats.events_ingested.load(Ordering::Relaxed),
        stats.events_dispatched.load(Ordering::Relaxed),
        stats.events_dropped.load(Ordering::Relaxed),
    );

    bus.shutdown().await?;
    Ok(())
}

/// Quickstart § "Reading events back".
///
/// Returns zero events against `EventBusConfig::default()` — the no-op
/// adapter has nothing to hand back. Kept compiled so the *shape* of the
/// call stays honest even though the page tells you it comes back empty.
#[allow(dead_code)]
async fn read_back(bus: &EventBus) -> Result<(), Box<dyn std::error::Error>> {
    let response = bus.poll(ConsumeRequest::new(100)).await?;
    for event in response.events {
        // `raw` is the payload as `Bytes` — the bus never assumes it's text.
        println!("{}", String::from_utf8_lossy(&event.raw));
    }
    Ok(())
}

/// Quickstart § "Add a filter".
#[allow(dead_code)]
async fn filtered(bus: &EventBus) -> Result<(), Box<dyn std::error::Error>> {
    let request = ConsumeRequest::new(100).filter(Filter::eq("token", serde_json::json!("hello")));

    let _response = bus.poll(request).await?;
    Ok(())
}

/// Quickstart § "Switch to the mesh".
#[cfg(feature = "net")]
#[allow(dead_code)]
async fn on_the_mesh() -> Result<(), Box<dyn std::error::Error>> {
    use net::adapter::net::{NetAdapterConfig, StaticKeypair};
    use net::AdapterConfig;

    // Both sides share a pre-shared key; the responder owns a static
    // keypair and the initiator must already know its public half.
    let psk = [0x42u8; 32];
    let responder = StaticKeypair::generate();

    let adapter = NetAdapterConfig::initiator(
        "0.0.0.0:7777".parse()?,  // bind
        "10.0.0.2:7777".parse()?, // peer
        psk,
        responder.public,
    );

    let config = EventBusConfig::builder()
        .adapter(AdapterConfig::Net(Box::new(adapter)))
        .build()?;

    let _bus = EventBus::new(config).await?;
    Ok(())
}
