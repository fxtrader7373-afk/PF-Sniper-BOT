use chrono::Utc;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::PoolCreationEvent;

const PUMP_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub struct WsListener {
    ws_url: String,
    sender: mpsc::Sender<PoolCreationEvent>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl WsListener {
    pub fn new(ws_url: String) -> (Self, mpsc::Receiver<PoolCreationEvent>) {
        let (tx, rx) = mpsc::channel(1000);
        (
            Self {
                ws_url,
                sender: tx,
                running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            rx,
        )
    }

    pub async fn start(&self) -> SniperResult<()> {
        if self.running.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SniperError::Unknown {
                msg: "Already running".into(),
            });
        }
        self.running
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let url = self.ws_url.clone();
        let tx = self.sender.clone();
        let r = self.running.clone();
        tokio::spawn(async move {
            Self::loop_listen(&url, &tx, &r).await;
        });
        info!("WsListener started");
        Ok(())
    }

    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    async fn loop_listen(
        url: &str,
        tx: &mpsc::Sender<PoolCreationEvent>,
        running: &Arc<std::sync::atomic::AtomicBool>,
    ) {
        loop {
            if !running.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            match PubsubClient::new(url).await {
                Ok(client) => {
                    info!("WS connected");
                    let pid = Pubkey::from_str(PUMP_ID).unwrap();
                    let filter = solana_client::rpc_config::RpcTransactionLogsFilter::Mentions(
                        vec![pid.to_string()],
                    );
                    let cfg = solana_client::rpc_config::RpcTransactionLogsConfig {
                        commitment: Some(
                            solana_sdk::commitment_config::CommitmentConfig::confirmed(),
                        ),
                    };
                    match client.logs_subscribe(filter, cfg).await {
                        Ok((mut stream, _unsub)) => {
                            info!("Subscribed");
                            while running.load(std::sync::atomic::Ordering::Relaxed) {
                                if let Some(log) = futures::StreamExt::next(&mut stream).await {
                                    let value = log.value;
                                    for line in &value.logs {
                                        if line.contains("Program log: Instruction: Create") {
                                            debug!("New pool: {:?}", value.signature);
                                            let _ = tx
                                                .send(PoolCreationEvent {
                                                    mint: Pubkey::new_unique(),
                                                    bonding_curve: Pubkey::new_unique(),
                                                    associated_bonding_curve: Pubkey::new_unique(),
                                                    user: Pubkey::new_unique(),
                                                    name: "Token".into(),
                                                    symbol: "TKN".into(),
                                                    uri: "".into(),
                                                    mayhem: false,
                                                    slot: 0,
                                                    timestamp: Utc::now(),
                                                    signature: value.signature.clone(),
                                                })
                                                .await;
                                            break;
                                        }
                                    }
                                } else {
                                    warn!("Stream ended");
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            error!("Subscribe fail: {}", e);
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                Err(e) => {
                    error!("Connect fail: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }
}
