# An Event-Sourced Service

This tutorial walks through building a complete event-sourced service: a small order-tracking system where every state change is an immutable event in a durable log, the queryable view is a fold materialized from the log, snapshots handle restarts without replaying from genesis, and the whole thing survives a node going away.

By the end you'll have an end-to-end implementation of the storage stack pattern that Net is built around — RedEX as the log, CortEX as the fold driver, NetDB as the query surface — and a worked example of how the layers compose.

## The shape

```
┌──────────────────────────────────────────────────────┐
│                                                      │
│  Producer ─►  Event  ─►  Channel  ─►  RedEX file     │
│              (Order command)         (append-only)   │
│                                          │           │
│                                          ▼           │
│                                   ┌────────────┐     │
│                                   │  Fold task │     │
│                                   │ (CortEX)   │     │
│                                   └─────┬──────┘     │
│                                         │           │
│                                  ┌──────▼──────┐    │
│                                  │ Order state │    │
│                                  │ (in-memory) │    │
│                                  └──────┬──────┘    │
│                                         │           │
│                                ┌────────┼────────┐  │
│                                ▼        ▼        ▼  │
│                              Query   Watch    Snapshot
│                                                    │
└──────────────────────────────────────────────────────┘
```

Every state change in the system is an event. The event lands in a RedEX log. The fold reads the log, applies each event to an in-memory state, and emits change notifications. Queries read the state; watchers subscribe to state changes; snapshots checkpoint the state for fast restart.

## Designing the events

The first decision is what the events look like. In an event-sourced system, every state change is an event — there's no "update this field" primitive, just commands that produce new events:

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
enum OrderEvent {
    Placed     { order_id: u64, customer: String, items: Vec<LineItem>, total_cents: u64 },
    Confirmed  { order_id: u64, confirmed_at: u64 },
    Shipped    { order_id: u64, carrier: String, tracking: String, shipped_at: u64 },
    Delivered  { order_id: u64, delivered_at: u64 },
    Cancelled  { order_id: u64, reason: String, cancelled_at: u64 },
}
```

A few principles:

- **Events are facts.** Once written, they're never modified. An event-sourced system models change by appending new events, not by editing old ones.
- **Events are self-contained.** The producer puts everything the fold needs into the event payload; the fold doesn't reach for external context.
- **Events are at the level of intent.** `Placed` is what happened, not `INSERT INTO orders`. The fold decides what an order *means* at any given moment.

## The fold

The fold reads events one at a time and applies them to the state. The state holds the current view — for orders, that means a map of order ID to its current lifecycle state:

```rust
use net::adapter::net::cortex::{RedexFold, FoldError};
use net::adapter::net::state::CausalEvent;
use std::collections::HashMap;

#[derive(Default, Serialize, Deserialize)]
struct OrderBook {
    orders: HashMap<u64, Order>,
}

#[derive(Serialize, Deserialize)]
struct Order {
    customer:      String,
    items:         Vec<LineItem>,
    total_cents:   u64,
    status:        OrderStatus,
    history:       Vec<HistoryEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
enum OrderStatus {
    Placed,
    Confirmed,
    Shipped { carrier: String, tracking: String },
    Delivered,
    Cancelled { reason: String },
}

struct OrderFold;

impl RedexFold<OrderBook> for OrderFold {
    fn apply(&self, state: &mut OrderBook, event: &CausalEvent) -> Result<(), FoldError> {
        let parsed: OrderEvent = serde_json::from_slice(&event.payload)
            .map_err(|e| FoldError::InvalidPayload(e.to_string()))?;

        match parsed {
            OrderEvent::Placed { order_id, customer, items, total_cents } => {
                state.orders.insert(order_id, Order {
                    customer, items, total_cents,
                    status: OrderStatus::Placed,
                    history: vec![HistoryEntry::Placed { at: event.timestamp() }],
                });
            }
            OrderEvent::Confirmed { order_id, confirmed_at } => {
                if let Some(order) = state.orders.get_mut(&order_id) {
                    order.status = OrderStatus::Confirmed;
                    order.history.push(HistoryEntry::Confirmed { at: confirmed_at });
                }
            }
            OrderEvent::Shipped { order_id, carrier, tracking, shipped_at } => {
                if let Some(order) = state.orders.get_mut(&order_id) {
                    order.status = OrderStatus::Shipped { carrier: carrier.clone(), tracking: tracking.clone() };
                    order.history.push(HistoryEntry::Shipped { at: shipped_at, carrier, tracking });
                }
            }
            OrderEvent::Delivered { order_id, delivered_at } => {
                if let Some(order) = state.orders.get_mut(&order_id) {
                    order.status = OrderStatus::Delivered;
                    order.history.push(HistoryEntry::Delivered { at: delivered_at });
                }
            }
            OrderEvent::Cancelled { order_id, reason, cancelled_at } => {
                if let Some(order) = state.orders.get_mut(&order_id) {
                    order.status = OrderStatus::Cancelled { reason: reason.clone() };
                    order.history.push(HistoryEntry::Cancelled { at: cancelled_at, reason });
                }
            }
        }
        Ok(())
    }
}
```

Some things to note about how the fold is written:

- **It's pure.** Same sequence of events in, same state out. No randomness, no clock reads, no external lookups. This is what makes replay-from-genesis correct.
- **It's defensive.** If an `OrderEvent::Confirmed` arrives for an order that doesn't exist, the fold ignores it. The discipline is on the producer to emit events in valid sequences; the fold doesn't try to enforce ordering rules.
- **History is in-band.** The `history` vector inside each order is built by the fold from the events it sees. The log is the source of truth; the state is the derived view.

## Opening the stack

The full stack — RedEX log, CortEX adapter, NetDB facade — opens with three lines:

```rust
use net::adapter::net::redex::{Redex, RedexFileConfig, FsyncPolicy};
use net::adapter::net::cortex::CortexAdapter;
use std::sync::Arc;

let redex = Arc::new(Redex::with_persistent_dir("/var/lib/orders/redex")?);

let order_cfg = RedexFileConfig::default()
    .with_persistent(true)
    .with_fsync_policy(FsyncPolicy::EveryN(100));

let orders = CortexAdapter::open(
    &redex,
    "orders",
    origin_hash,
    OrderFold,
).await?;
```

The RedEX manager handles the on-disk layout. The CortEX adapter spawns the fold task. Once both are open, the adapter exposes the state for query, the watch stream for reactive updates, and the snapshot primitive for checkpointing.

## Writing events

A new order is one event:

```rust
let event = Event::new(serde_json::to_value(OrderEvent::Placed {
    order_id: 12345,
    customer: "alice@example.com".into(),
    items: vec![LineItem { sku: "WIDGET-1".into(), qty: 2 }],
    total_cents: 4998,
})?);

let seq = orders.append(event.into_raw().bytes()).await?;
```

`append()` writes to the RedEX log atomically. The fold task picks up the event from the tail subscription and applies it to the state. If you need read-your-writes — a UI that wants to render the state including the just-appended event — wait for the fold to catch up:

```rust
orders.wait_for_seq(seq).await;
let state = orders.state().read();
let order = state.orders.get(&12345);  // Guaranteed visible
```

## Reading

The state is in memory, behind an `Arc<RwLock<OrderBook>>`. Queries are read-lock-and-scan:

```rust
let state = orders.state().read();

let count_placed = state.orders.values()
    .filter(|o| matches!(o.status, OrderStatus::Placed))
    .count();

let total_revenue: u64 = state.orders.values()
    .filter(|o| matches!(o.status, OrderStatus::Delivered))
    .map(|o| o.total_cents)
    .sum();
```

For more structured access, define helper methods on the state itself. The fold task only holds the write lock briefly per event, so even under high read load, queries don't block ingestion meaningfully.

## Watching

UIs and dashboards want change notifications. The CortEX adapter exposes a stream that emits the current state whenever it changes:

```rust
use futures::StreamExt;

let mut updates = Box::pin(orders.watch().stream());

while let Some(state) = updates.next().await {
    dashboard.render(&state);
}
```

The stream emits the current state on subscribe, then dedupe-emits on every subsequent change. Renders are deterministic; the dashboard sees every transition without polling.

For consumers that want a snapshot plus deltas (the common UI pattern), `snapshot_and_watch` handles both in one call:

```rust
let (initial, mut deltas) = orders.snapshot_and_watch(orders.watch()).await?;
dashboard.render(&initial);
while let Some(d) = deltas.next().await {
    dashboard.render(&d);
}
```

## Snapshots and restart

Over time the log grows. A daemon restarting against a million-event log replays a million events; that's fine for some workloads, problematic for others. Snapshots checkpoint the state:

```rust
let (bytes, last_seq) = orders.snapshot()?;
write_to_disk("/var/lib/orders/snapshot.bin", &bytes).await?;
write_to_disk("/var/lib/orders/snapshot.seq", last_seq.to_string()).await?;
```

The snapshot is the state's serialized form (postcard, compact) plus the sequence number of the last event folded into it. On restart, the adapter restores from the snapshot and resumes the fold from `last_seq + 1`:

```rust
let snapshot_bytes = read_from_disk("/var/lib/orders/snapshot.bin").await?;
let last_seq: u64 = read_from_disk("/var/lib/orders/snapshot.seq").await?.parse()?;

let orders = CortexAdapter::open_from_snapshot(
    &redex,
    origin_hash,
    OrderFold,
    &snapshot_bytes,
    last_seq,
).await?;
```

Pre-snapshot events never re-fold. The resumed state is byte-identical to where it left off. The cost of a restart is bounded by "snapshot size + post-snapshot event count," which is typically a tiny fraction of the full log.

## Composing with NetDB

If you have multiple folds — orders, customers, inventory — you'd manage them through a NetDB instead of a stack of standalone CortEX adapters:

```rust
use net::adapter::net::netdb::NetDbBuilder;

let db = NetDbBuilder::new(&redex, origin_hash)
    .with_custom("orders", OrderFold)
    .with_custom("customers", CustomerFold)
    .with_custom("inventory", InventoryFold)
    .build()
    .await?;

let bundle = db.snapshot()?;  // Whole-stack snapshot in one call
```

NetDB bundles the folds under one handle and gives you whole-stack snapshot/restore. Adding a new fold is one builder call; the rest of the system doesn't change.

## What this gives you

The pattern is small, but the properties it gets you are substantial:

- **Every state change is auditable.** The log is the source of truth; you can replay any range, time-travel to any point, and prove what happened from cryptographically chained primitives.
- **Restart is fast.** Snapshots bound the recovery time; the log is durable; the fold is deterministic.
- **Read scales independently of write.** Multiple consumers read the same state without affecting each other or affecting the producer.
- **Change is incremental.** Adding a new event variant is a fold update; the log doesn't need migration. Adding a new fold is one builder call; the existing folds don't change.

This is event sourcing as a first-class shape. The pattern doesn't depend on Net specifically — you can do it on any append-only log with a fold runtime on top. What Net gives you is the substrate: identity-bound writes, encrypted transport, capability-aware access, and the ability to compose this single-node design into a distributed one (replicated logs, federated queries, daemon migration) without changing the application code.
