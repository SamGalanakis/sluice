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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panic_is_an_error_and_a_result_passes_through() {
        let ok = contained("command", async { Ok::<_, PublicError>(7) }).await;
        assert_eq!(ok.unwrap(), 7);
        let failed: Result<(), _> =
            contained("command", async { panic!("broken {}", "work") }).await;
        let Err(PublicError::Storage { message }) = failed else {
            panic!("{failed:?}")
        };
        assert_eq!(message, "the coordinator's command failed: broken work");
        let refused: Result<(), _> = contained("command", async {
            Err(PublicError::BadRequest {
                message: "no".into(),
            })
        })
        .await;
        assert!(matches!(refused, Err(PublicError::BadRequest { .. })));
    }
}
