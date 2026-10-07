//! Request work on its own task, so a panic becomes an error the caller is told about.
use sluice_model::error::PublicError;

/// Runs one request's work on its own task, so a panic becomes an error reply (and an
/// error log) instead of a dropped connection. Dropping the future aborts the task.
pub(crate) async fn contained<T: Send + 'static>(
    what: &'static str,
    work: impl std::future::Future<Output = Result<T, PublicError>> + Send + 'static,
) -> Result<T, PublicError> {
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let task = tokio::spawn(work);
    let _abort = AbortOnDrop(task.abort_handle());
    match task.await {
        Ok(result) => result,
        Err(error) if error.is_panic() => {
            let panic = error.into_panic();
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a non-text panic".into());
            tracing::error!(%message, "coordinator {what} panicked");
            Err(PublicError::Storage {
                message: format!("the coordinator's {what} failed: {message}"),
            })
        }
        Err(error) => Err(PublicError::Storage {
            message: error.to_string(),
        }),
    }
}
