//! One absolute budget for a bounded operation, never reset between stages.
use std::future::Future;
use std::time::Duration;

use crate::error::{invalid_args, CliError, ExitCodeKind};

#[derive(Clone, Copy)]
pub(crate) struct Deadline(tokio::time::Instant);

impl Deadline {
    pub(crate) fn after(duration: Duration) -> Result<Self, CliError> {
        tokio::time::Instant::now()
            .checked_add(duration)
            .map(Self)
            .ok_or_else(|| invalid_args("--timeout is too large for this platform"))
    }

    pub(crate) async fn run<T>(
        self,
        operation: impl Future<Output = Result<T, CliError>>,
    ) -> Result<T, CliError> {
        // timeout_at may poll an immediately ready future even when expired.
        // Refuse before polling so a zero/exhausted budget cannot start effects.
        if tokio::time::Instant::now() >= self.0 {
            return Err(expired());
        }
        tokio::time::timeout_at(self.0, operation)
            .await
            .map_err(|_| expired())?
    }
}

fn expired() -> CliError {
    CliError::new(ExitCodeKind::Timeout, "operation exceeded --timeout; a remote effect may have committed. Timeout does not prove cancellation; the CLI did not retry the operation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn expired_budget_does_not_poll_operation() {
        let effects = AtomicUsize::new(0);
        let err = Deadline::after(Duration::ZERO)
            .unwrap()
            .run(async {
                effects.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ExitCodeKind::Timeout);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stages_share_one_budget_and_timeout_does_not_retry_effect() {
        let deadline = Deadline::after(Duration::from_millis(100)).unwrap();
        deadline
            .run(async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(())
            })
            .await
            .unwrap();
        let effects = AtomicUsize::new(0);
        let err = deadline
            .run(async {
                effects.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ExitCodeKind::Timeout);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert!(deadline.run(async { Ok(()) }).await.is_err());
    }
}
