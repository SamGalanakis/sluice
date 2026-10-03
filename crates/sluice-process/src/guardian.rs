use sluice_model::{
    error::PublicError,
    rpc::{FnInvocation, JsonMap},
};

/// The composition root injects fn dispatch without a process-to-agents dependency.
pub trait FnHost: Send + Sync {
    fn invoke(
        &self,
        invocation: FnInvocation,
    ) -> impl std::future::Future<Output = Result<JsonMap, PublicError>> + Send;
}

pub struct UnimplementedFnHost;
impl FnHost for UnimplementedFnHost {
    async fn invoke(&self, _invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        Err(PublicError::not_implemented("fn dispatch"))
    }
}
