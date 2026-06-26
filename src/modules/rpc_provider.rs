use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use crate::core::error::{SniperError, SniperResult};
use crate::core::types::RpcHealth;

#[async_trait]
pub trait RpcProvider: Send + Sync {
    async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value>;
    fn health(&self) -> RpcHealth;
    fn mark_unhealthy(&self);
    fn mark_healthy(&self);
}

pub struct HttpRpcProvider {
    client: Client,
    url: String,
    label: String,
    health_state: Arc<RwLock<RpcHealth>>,
}

impl HttpRpcProvider {
    pub fn new(label: &str, url: &str) -> Self {
        let health = RpcHealth {
            label: label.to_string(), url: url.to_string(), latency_ms: 0,
            last_429_at: None, consecutive_429s: 0, is_healthy: true, last_check: Utc::now(),
        };
        Self {
            client: Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap(),
            url: url.to_string(), label: label.to_string(),
            health_state: Arc::new(RwLock::new(health)),
        }
    }

    async fn record_latency(&self, ms: u64) {
        let mut h = self.health_state.write().await;
        h.latency_ms = ms; h.consecutive_429s = 0; h.last_check = Utc::now();
        if !h.is_healthy && h.consecutive_429s < 3 { h.is_healthy = true; }
    }

    async fn record_rate_limit(&self) {
        let mut h = self.health_state.write().await;
        h.last_429_at = Some(Utc::now()); h.consecutive_429s += 1;
        if h.consecutive_429s >= 5 { h.is_healthy = false; }
    }
}

#[async_trait]
impl RpcProvider for HttpRpcProvider {
    async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let resp = self.client.post(&self.url).json(&body).send().await
            .map_err(|e| SniperError::RpcError { msg: e.to_string() })?;
        let ms = start.elapsed().as_millis() as u64;
        if resp.status() == 429 {
            self.record_rate_limit().await;
            return Err(SniperError::RateLimited { endpoint: self.url.clone() });
        }
        if !resp.status().is_success() {
            return Err(SniperError::RpcError { msg: format!("HTTP {}", resp.status()) });
        }
        self.record_latency(ms).await;
        let json: serde_json::Value = resp.json().await.map_err(|e| SniperError::RpcError { msg: e.to_string() })?;
        if let Some(err) = json.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("RPC error");
            return Err(SniperError::RpcError { msg: msg.to_string() });
        }
        json.get("result").cloned().ok_or_else(|| SniperError::RpcError { msg: "No result".into() })
    }
    fn health(&self) -> RpcHealth { self.health_state.blocking_read().clone() }
    fn mark_unhealthy(&self) { let mut h = self.health_state.blocking_write(); h.is_healthy = false; h.consecutive_429s = u32::MAX; }
    fn mark_healthy(&self) { let mut h = self.health_state.blocking_write(); h.is_healthy = true; h.consecutive_429s = 0; h.last_check = Utc::now(); }
}

pub struct RpcManager {
    providers: Arc<RwLock<HashMap<String, Arc<HttpRpcProvider>>>>,
    active: Arc<RwLock<Option<String>>>,
}

impl RpcManager {
    pub fn new() -> Self {
        Self { providers: Arc::new(RwLock::new(HashMap::new())), active: Arc::new(RwLock::new(None)) }
    }
    pub async fn add_provider(&self, label: &str, url: &str) {
        let p = Arc::new(HttpRpcProvider::new(label, url));
        self.providers.write().await.insert(label.to_string(), p);
        if self.active.read().await.is_none() { *self.active.write().await = Some(label.to_string()); }
    }
    async fn get_active(&self) -> SniperResult<Arc<HttpRpcProvider>> {
        let label = self.active.read().await.clone()
            .ok_or_else(|| SniperError::ConfigError { msg: "No active RPC".into() })?;
        let providers = self.providers.read().await;
        providers.get(&label).cloned()
            .ok_or_else(|| SniperError::ConfigError { msg: format!("Provider '{}' not found", label) })
    }
    pub async fn rpc_call(&self, method: &str, params: &[serde_json::Value]) -> SniperResult<serde_json::Value> {
        let provider = self.get_active().await?;
        match provider.rpc_call(method, params).await {
            Ok(r) => Ok(r),
            Err(SniperError::RateLimited { .. }) => {
                warn!("Rate limited, failing over...");
                provider.mark_unhealthy();
                self.failover().await?;
                self.get_active().await?.rpc_call(method, params).await
            }
            Err(e) => Err(e),
        }
    }
    async fn failover(&self) -> SniperResult<()> {
        for (label, p) in self.providers.read().await.iter() {
            if p.health().is_healthy { *self.active.write().await = Some(label.clone()); return Ok(()); }
        }
        for p in self.providers.read().await.values() { p.mark_healthy(); }
        if let Some((label, _)) = self.providers.read().await.iter().next() {
            *self.active.write().await = Some(label.clone()); return Ok(());
        }
        Err(SniperError::ConfigError { msg: "No RPC providers".into() })
    }
    pub async fn health_report(&self) -> Vec<RpcHealth> {
        self.providers.read().await.values().map(|p| p.health()).collect()
    }
}
