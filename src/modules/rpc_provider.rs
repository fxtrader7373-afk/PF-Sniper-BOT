//! RPC Provider module — trait-based abstraction over RPC backends.
//!
//! Supports automatic failover on HTTP 429 (rate limits).
//! Free-tier RPCs (Helius free = 1M credits/10 RPS, public endpoints)
//! will hit rate limits often. This layer handles the reality transparently.

use async_trait::async_trait;
use chrono::Utc;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::RpcHealth;

/// The RPC provider trait — all backends must implement this
#[async_trait]
pub trait RpcProvider: Send + Sync {
    /// Make an HTTP POST RPC call
    async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value>;

    /// Get the current health status of this provider
    fn health(&self) -> RpcHealth;

    /// Mark this provider as unhealthy (called on persistent 429s)
    fn mark_unhealthy(&self);

    /// Mark this provider as healthy again
    fn mark_healthy(&self);
}

/// Concrete HTTP-based RPC provider with built-in rate-limit handling
pub struct HttpRpcProvider {
    client: Client,
    url: String,
    label: String,
    health: Arc<RwLock<RpcHealth>>,
}

impl HttpRpcProvider {
    pub fn new(label: &str, url: &str) -> Self {
        let health = RpcHealth {
            label: label.to_string(),
            url: url.to_string(),
            latency_ms: 0,
            last_429_at: None,
            consecutive_429s: 0,
            is_healthy: true,
            last_check: Utc::now(),
        };

        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            url: url.to_string(),
            label: label.to_string(),
            health: Arc::new(RwLock::new(health)),
        }
    }

    /// Update latency after a successful call
    async fn record_latency(&self, latency_ms: u64) {
        let mut h = self.health.write().await;
        h.latency_ms = latency_ms;
        h.consecutive_429s = 0;
        h.last_check = Utc::now();
        if !h.is_healthy && h.consecutive_429s < 3 {
            h.is_healthy = true;
            info!("RPC provider {} marked healthy again", self.label);
        }
    }

    /// Record a rate-limit hit (HTTP 429)
    async fn record_rate_limit(&self) {
        let mut h = self.health.write().await;
        h.last_429_at = Some(Utc::now());
        h.consecutive_429s += 1;

        if h.consecutive_429s >= 5 {
            h.is_healthy = false;
            warn!(
                "RPC provider {} marked UNHEALTHY after {} consecutive 429s",
                self.label, h.consecutive_429s
            );
        }
    }
}

#[async_trait]
impl RpcProvider for HttpRpcProvider {
    async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value> {
        let start = std::time::Instant::now();

        let request_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.url)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })?;

        let latency_ms = start.elapsed().as_millis() as u64;

        if response.status() == 429 {
            self.record_rate_limit().await;
            return Err(SniperError::RateLimited { endpoint: self.url.clone() });
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(SniperError::RpcError {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("HTTP {}: {}", status, body),
                )),
            });
        }

        self.record_latency(latency_ms).await;

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })?;

        // Check for RPC-level error
        if let Some(error) = json.get("error") {
            let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown RPC error");
            return Err(SniperError::RpcError {
                source: Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg)),
            });
        }

        json.get("result")
            .cloned()
            .ok_or_else(|| SniperError::RpcError {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "No result field in RPC response",
                )),
            })
    }

    fn health(&self) -> RpcHealth {
        let h = self.health.blocking_read();
        h.clone()
    }

    fn mark_unhealthy(&self) {
        let mut h = self.health.blocking_write();
        h.is_healthy = false;
        h.last_429_at = Some(Utc::now());
        h.consecutive_429s = u32::MAX;
    }

    fn mark_healthy(&self) {
        let mut h = self.health.blocking_write();
        h.is_healthy = true;
        h.consecutive_429s = 0;
        h.last_check = Utc::now();
    }
}

/// Multi-provider manager that handles failover
pub struct RpcManager {
    providers: Arc<RwLock<HashMap<String, Box<dyn RpcProvider>>>>,
    active_provider: Arc<RwLock<Option<String>>>,
}

impl RpcManager {
    pub fn new() -> Self {
        Self {
            providers: Arc::new(RwLock::new(HashMap::new())),
            active_provider: Arc::new(RwLock::new(None)),
        }
    }

    /// Register a new RPC provider
    pub async fn add_provider(&self, label: &str, url: &str) {
        let provider = HttpRpcProvider::new(label, url);
        let mut providers = self.providers.write().await;
        providers.insert(label.to_string(), Box::new(provider));

        // Auto-select first provider if none active
        let active = self.active_provider.read().await;
        if active.is_none() {
            drop(active);
            let mut active = self.active_provider.write().await;
            *active = Some(label.to_string());
            info!("Auto-selected {} as active RPC provider", label);
        }
    }

    /// Get the active provider
    async fn get_active_provider(&self) -> SniperResult<Arc<dyn RpcProvider>> {
        let active = self.active_provider.read().await;
        let label = active.as_ref()
            .ok_or_else(|| SniperError::ConfigError { msg: "No active RPC provider configured".into() })?;

        let providers = self.providers.read().await;
        let provider = providers.get(label.as_str())
            .ok_or_else(|| SniperError::ConfigError { msg: format!("Active provider '{}' not found", label) })?;

        Ok(Arc::from(provider.as_ref()))
    }

    /// Make an RPC call through the active provider, with automatic failover on 429
    pub async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value> {
        let provider = self.get_active_provider().await?;

        match provider.rpc_call(method, params).await {
            Ok(result) => Ok(result),
            Err(SniperError::RateLimited { .. }) => {
                warn!("Active provider rate-limited, attempting failover...");
                provider.mark_unhealthy();
                self.failover().await?;

                // Retry once with new provider
                let new_provider = self.get_active_provider().await?;
                new_provider.rpc_call(method, params).await
            }
            Err(e) => Err(e),
        }
    }

    /// Failover to the next healthy provider
    async fn failover(&self) -> SniperResult<()> {
        let providers = self.providers.read().await;

        for (label, provider) in providers.iter() {
            if provider.health().is_healthy {
                let mut active = self.active_provider.write().await;
                *active = Some(label.clone());
                info!("Failed over to RPC provider: {}", label);
                return Ok(());
            }
        }

        // All providers unhealthy — reset and try the first one
        warn!("All RPC providers unhealthy, resetting...");
        drop(providers);

        let mut providers = self.providers.write().await;
        for provider in providers.values_mut() {
            provider.mark_healthy();
        }

        if let Some((label, _)) = providers.iter().next() {
            let mut active = self.active_provider.write().await;
            *active = Some(label.clone());
            info!("Reset and selected {} as active provider", label);
            return Ok(());
        }

        Err(SniperError::ConfigError { msg: "No RPC providers configured".into() })
    }

    /// Get health status of all providers
    pub async fn health_report(&self) -> Vec<RpcHealth> {
        let providers = self.providers.read().await;
        let mut report = Vec::new();

        for provider in providers.values() {
            report.push(provider.health());
        }

        report
    }

    /// Get the active provider label
    pub async fn active_provider_label(&self) -> Option<String> {
        self.active_provider.read().await.clone()
    }
}
