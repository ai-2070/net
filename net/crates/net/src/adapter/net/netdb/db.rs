//! `NetDb` — unified query façade over one or more CortEX models.

use serde::{Deserialize, Serialize};

use super::super::cortex::memories::MemoriesAdapter;
use super::super::cortex::tasks::TasksAdapter;
use super::super::redex::{Redex, RedexFileConfig};
use super::error::NetDbError;

/// Portable, postcard-serialisable bundle of per-model snapshots.
/// Returned by [`NetDb::snapshot`]; consumed by
/// [`NetDbBuilder::build_from_snapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetDbSnapshot {
    /// `Some((bytes, last_seq))` if tasks were included in the DB
    /// and have been snapshotted; `None` otherwise.
    pub tasks: Option<(Vec<u8>, Option<u64>)>,
    /// Same, for memories.
    pub memories: Option<(Vec<u8>, Option<u64>)>,
}

impl NetDbSnapshot {
    /// Serialize the whole bundle into a single postcard blob for
    /// persistence.
    pub fn encode(&self) -> Result<Vec<u8>, NetDbError> {
        postcard::to_allocvec(self).map_err(|e| NetDbError::Snapshot(e.to_string()))
    }

    /// Deserialize from a blob produced by [`Self::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, NetDbError> {
        postcard::from_bytes(bytes).map_err(|e| NetDbError::Snapshot(e.to_string()))
    }
}

/// Unified NetDB handle.
///
/// Bundles one or more CortEX adapters (tasks, memories, …) behind a
/// single handle. Construct via [`NetDb::builder`].
pub struct NetDb {
    redex: Redex,
    tasks: Option<TasksAdapter>,
    memories: Option<MemoriesAdapter>,
}

impl NetDb {
    /// Start building a NetDB.
    pub fn builder(redex: Redex) -> NetDbBuilder {
        NetDbBuilder {
            redex,
            origin_hash: 0,
            persistent: false,
            want_tasks: false,
            want_memories: false,
        }
    }

    /// Access the tasks model. Panics if `with_tasks()` wasn't
    /// called on the builder. Use [`Self::try_tasks`] for a checked
    /// accessor.
    pub fn tasks(&self) -> &TasksAdapter {
        self.tasks
            .as_ref()
            .expect("NetDb: tasks not enabled — call `with_tasks()` on the builder")
    }

    /// Checked tasks accessor. Returns `None` if tasks were not
    /// included at build time.
    pub fn try_tasks(&self) -> Option<&TasksAdapter> {
        self.tasks.as_ref()
    }

    /// Access the memories model. Panics if `with_memories()` wasn't
    /// called.
    pub fn memories(&self) -> &MemoriesAdapter {
        self.memories
            .as_ref()
            .expect("NetDb: memories not enabled — call `with_memories()` on the builder")
    }

    /// Checked memories accessor.
    pub fn try_memories(&self) -> Option<&MemoriesAdapter> {
        self.memories.as_ref()
    }

    /// Borrow the underlying `Redex` manager. Useful for lifecycle
    /// operations (close a specific channel, sweep retention, etc.).
    pub fn redex(&self) -> &Redex {
        &self.redex
    }

    /// Close every enabled adapter. The underlying `Redex` files
    /// stay open on the manager — reopening via another NetDb
    /// against the same `Redex` instance replays or snapshots them.
    /// Idempotent.
    ///
    /// Both closes are attempted regardless of failure and the
    /// FIRST error is surfaced as the function's return; the
    /// SECOND error is logged at `warn` so a double-failure is
    /// observable in tracing without conflating the typed
    /// error surface. Pre-fix the second error was dropped
    /// silently — operators investigating a `close()` failure
    /// from the typed return would never see the second adapter's
    /// error. The dominant double-failure mode is "underlying
    /// redex already closed," which produces the same error
    /// from both adapters and is uninteresting to disambiguate
    /// in the typed return; the warn-log makes it observable
    /// without adding a new error variant.
    pub fn close(&self) -> Result<(), NetDbError> {
        let tasks_result = self.tasks.as_ref().map(|t| t.close()).unwrap_or(Ok(()));
        let memories_result = self.memories.as_ref().map(|m| m.close()).unwrap_or(Ok(()));

        // Surface the first error; if both errored, the tasks one
        // wins by convention (matches the pre-fix shape where tasks
        // ran first). Log the second so it's not invisible.
        match (tasks_result, memories_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), Ok(())) => Err(e.into()),
            (Ok(()), Err(e)) => Err(e.into()),
            (Err(tasks_err), Err(memories_err)) => {
                tracing::warn!(
                    tasks_error = %tasks_err,
                    memories_error = %memories_err,
                    "netdb close: both adapters failed; surfacing tasks error \
                     and logging memories error",
                );
                Err(tasks_err.into())
            }
        }
    }

    /// Capture a snapshot of every enabled model. Each model is
    /// snapshotted under its own state lock (consistent per-model);
    /// there is no cross-model consistency guarantee because each
    /// model is a separate RedEX file.
    pub fn snapshot(&self) -> Result<NetDbSnapshot, NetDbError> {
        let tasks = if let Some(t) = &self.tasks {
            Some(t.snapshot()?)
        } else {
            None
        };
        let memories = if let Some(m) = &self.memories {
            Some(m.snapshot()?)
        } else {
            None
        };
        Ok(NetDbSnapshot { tasks, memories })
    }
}

impl std::fmt::Debug for NetDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetDb")
            .field("tasks", &self.tasks.is_some())
            .field("memories", &self.memories.is_some())
            .finish()
    }
}

/// Builder for [`NetDb`].
pub struct NetDbBuilder {
    redex: Redex,
    origin_hash: u32,
    persistent: bool,
    want_tasks: bool,
    want_memories: bool,
}

impl NetDbBuilder {
    /// Set the producer origin_hash stamped on every `EventMeta` by
    /// the bundled adapters.
    pub fn origin(mut self, origin_hash: u32) -> Self {
        self.origin_hash = origin_hash;
        self
    }

    /// When `true`, every enabled model's underlying RedEX file uses
    /// `persistent: true`. Requires the `Redex` to have been
    /// constructed with `with_persistent_dir(...)`.
    pub fn persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Include the tasks model.
    pub fn with_tasks(mut self) -> Self {
        self.want_tasks = true;
        self
    }

    /// Include the memories model.
    pub fn with_memories(mut self) -> Self {
        self.want_memories = true;
        self
    }

    /// Build the NetDb. Opens each enabled model against the
    /// underlying `Redex`.
    ///
    /// # Failure atomicity
    ///
    /// If the second adapter open fails, the first adapter is closed
    /// before the error propagates so no orphan fold task outlives
    /// the failed build. The `Redex` is dropped with the builder on
    /// the error path — callers who want to retry without losing
    /// shared state should construct a new `Redex` (retry on the
    /// same manager is not available since the builder consumes it
    /// by value).
    ///
    /// The atomicity guarantee itself is code-level: the
    /// close-on-error block below is the authoritative source of
    /// truth. Integration tests exercise the observable error path
    /// but cannot directly observe the closed first-adapter after
    /// the Redex has been dropped.
    pub async fn build(self) -> Result<NetDb, NetDbError> {
        let cfg = self.redex_config();

        let tasks = if self.want_tasks {
            Some(TasksAdapter::open_with_config(&self.redex, self.origin_hash, cfg).await?)
        } else {
            None
        };

        let memories = if self.want_memories {
            match MemoriesAdapter::open_with_config(&self.redex, self.origin_hash, cfg).await {
                Ok(m) => Some(m),
                Err(e) => {
                    if let Some(t) = &tasks {
                        let _ = t.close();
                    }
                    return Err(e.into());
                }
            }
        } else {
            None
        };

        Ok(NetDb {
            redex: self.redex,
            tasks,
            memories,
        })
    }

    /// Like [`Self::build`], but restore each enabled model from the
    /// corresponding entry in `snapshot`. A model enabled via
    /// `with_*()` whose snapshot entry is `None` in the bundle is
    /// opened from scratch via the normal open path (equivalent to
    /// [`Self::build`] for that model).
    ///
    /// Same failure-atomicity guarantee as [`Self::build`] — a
    /// second-adapter failure closes the first before the error
    /// propagates. See `build`'s docs for the caveat that the
    /// failing Redex is dropped with the builder.
    pub async fn build_from_snapshot(self, snapshot: &NetDbSnapshot) -> Result<NetDb, NetDbError> {
        let cfg = self.redex_config();

        let tasks = match (self.want_tasks, &snapshot.tasks) {
            (true, Some((bytes, last_seq))) => Some(
                TasksAdapter::open_from_snapshot_with_config(
                    &self.redex,
                    self.origin_hash,
                    cfg,
                    bytes,
                    *last_seq,
                )
                .await?,
            ),
            (true, None) => {
                Some(TasksAdapter::open_with_config(&self.redex, self.origin_hash, cfg).await?)
            }
            (false, _) => None,
        };

        let memories_result = match (self.want_memories, &snapshot.memories) {
            (true, Some((bytes, last_seq))) => Some(
                MemoriesAdapter::open_from_snapshot_with_config(
                    &self.redex,
                    self.origin_hash,
                    cfg,
                    bytes,
                    *last_seq,
                )
                .await,
            ),
            (true, None) => {
                Some(MemoriesAdapter::open_with_config(&self.redex, self.origin_hash, cfg).await)
            }
            (false, _) => None,
        };
        let memories = match memories_result {
            Some(Ok(m)) => Some(m),
            Some(Err(e)) => {
                if let Some(t) = &tasks {
                    let _ = t.close();
                }
                return Err(e.into());
            }
            None => None,
        };

        Ok(NetDb {
            redex: self.redex,
            tasks,
            memories,
        })
    }

    fn redex_config(&self) -> RedexFileConfig {
        if self.persistent {
            RedexFileConfig::default().with_persistent(true)
        } else {
            RedexFileConfig::default()
        }
    }
}
