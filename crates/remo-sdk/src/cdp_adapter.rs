//! Bridges `remo-sdk`'s real [`CapabilityRegistry`] into `remo-cdp`'s
//! wire-agnostic [`remo_cdp::domain_remo::CapabilityInvoker`] trait — the
//! seam Phase 0 left open specifically so this adapter could be written
//! without `remo-cdp` ever depending on `remo-sdk` (that dependency only
//! runs this direction: `remo-sdk` → `remo-cdp`).
//!
//! `CapabilityRegistry` is already wire-protocol-agnostic (confirmed in
//! Phase 0's planning — it did not change at all), so this file is the
//! entire integration: no changes to `registry.rs` were needed.

use std::sync::Arc;

use async_trait::async_trait;
use remo_cdp::domain_remo::CapabilityInvoker;
use serde_json::Value;

use crate::registry::{CapabilityRegistry, HandlerError, HandlerOutput};

/// Thin wrapper so the adapter impl lives here rather than on
/// `CapabilityRegistry` itself, keeping `registry.rs` free of any CDP
/// awareness — the registry should never need to know a wire format exists.
pub struct RegistryInvoker(pub CapabilityRegistry);

#[async_trait]
impl CapabilityInvoker for RegistryInvoker {
    async fn invoke(&self, name: &str, params: Value) -> Option<Result<Value, String>> {
        let result = self.0.invoke(name, params).await?;
        Some(match result {
            Ok(HandlerOutput::Json(value)) => Ok(value),
            // Deliberate scope decision, not a silent gap: no built-in
            // capability returns binary data anymore (the one that used to,
            // `__screenshot`, was removed once `Page.captureScreenshot` —
            // see `remo-cdp`'s `domain_page` — made it a redundant Track-A
            // duplicate of Track B). Shoehorning binary bytes through a
            // JSON-typed `Remo.invoke` result isn't worth the complexity
            // until some concrete future capability actually needs it.
            Ok(HandlerOutput::Binary { .. }) => Err(
                "this capability returns binary data, which Remo.invoke does not carry — \
                 expose it as a dedicated CDP method instead (see Page.captureScreenshot)"
                    .to_string(),
            ),
            Err(HandlerError::InvalidParams(message)) => Err(format!("invalid params: {message}")),
            Err(HandlerError::Internal(message)) => Err(format!("internal error: {message}")),
        })
    }

    fn list(&self) -> Vec<String> {
        self.0.list()
    }
}

/// Convenience constructor matching `remo_cdp::domain_remo::RemoDomain::new`'s
/// `Arc<I>` requirement.
pub fn remo_domain(
    registry: CapabilityRegistry,
) -> remo_cdp::domain_remo::RemoDomain<RegistryInvoker> {
    remo_cdp::domain_remo::RemoDomain::new(Arc::new(RegistryInvoker(registry)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn json_output_passes_through() {
        let registry = CapabilityRegistry::new();
        registry.register_sync("ping", |_| Ok(serde_json::json!({"pong": true})));
        let invoker = RegistryInvoker(registry);

        let result = invoker.invoke("ping", Value::Null).await;
        assert_eq!(result, Some(Ok(serde_json::json!({"pong": true}))));
    }

    #[tokio::test]
    async fn binary_output_becomes_a_clear_error_not_a_panic() {
        let registry = CapabilityRegistry::new();
        registry.register_sync_raw("dump", |_| {
            Ok(HandlerOutput::Binary {
                metadata: serde_json::json!({}),
                data: vec![1, 2, 3],
            })
        });
        let invoker = RegistryInvoker(registry);

        let result = invoker.invoke("dump", Value::Null).await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Page.captureScreenshot"));
    }

    #[tokio::test]
    async fn unknown_capability_is_none() {
        let registry = CapabilityRegistry::new();
        let invoker = RegistryInvoker(registry);
        assert!(invoker.invoke("nope", Value::Null).await.is_none());
    }

    #[tokio::test]
    async fn list_reflects_registered_names() {
        let registry = CapabilityRegistry::new();
        registry.register_sync("a", |_| Ok(Value::Null));
        registry.register_sync("b", |_| Ok(Value::Null));
        let invoker = RegistryInvoker(registry);

        let mut names = invoker.list();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }
}
