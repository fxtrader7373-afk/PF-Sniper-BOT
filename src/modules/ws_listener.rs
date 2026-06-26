//! WebSocket Listener module — subscribes to pump.fun program logs in real time.
//!
//! Uses logsSubscribe to monitor the pump.fun program ID for new token creation events.
//! This is the lowest-latency detection mechanism available via standard RPC.
//!
//! Architecture:
//! 1. Subscribe to logs mentioning the pump.fun program ID
//! 2. Filter for "Program log: Instruction: Create" or CreateV2
//! 3. Decode base64 Program data: lines into borsh-serialized structs
//! 4. Extract: name, symbol, uri, mint, bonding_curve, associated_bonding_curve, user, mayhem
//! 5. Emit PoolCreationEvent into downstream processing pipeline
//!
//! Source: [2](https://docs.chainstack.com/docs/solana-listening-to-pumpfun-token-mint-using-only-logssubscribe)

use chrono::Utc;
use futures::StreamExt;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::PoolCreationEvent;

/// The pump.fun program ID we're monitoring
const PUMP_FUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Channel capacity for new pool events
const EVENT_CHANNEL_CAPACITY: usize = 1000;

/// WebSocket listener for pump.fun new-pool detection
pub struct WsListener {
    ws_url: String,
    sender: mpsc::Sender<PoolCreationEvent>,
    is_running: Arc<std::sync::atomic::AtomicBool>,
}

impl WsListener {
    pub fn new(ws_url: String) -> (Self, mpsc::Receiver<PoolCreationEvent>) {
        let (sender, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let listener = Self {
            ws_url,
            sender,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        (listener, receiver)
    }

    /// Start listening for pump.fun new-pool events
    pub async fn start(&self) -> SniperResult<()> {
        if self.is_running.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SniperError::Unknown { msg: "WsListener is already running".into() });
        }

        self.is_running.store(true, std::sync::atomic::Ordering::Relaxed);

        let ws_url = self.ws_url.clone();
        let sender = self.sender.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            Self::listen_loop(&ws_url, &sender, &is_running).await;
        });

        info!("WsListener started — monitoring pump.fun program for new pools");
        Ok(())
    }

    /// Stop listening
    pub fn stop(&self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        info!("WsListener stopped");
    }

    /// Main listen loop
    async fn listen_loop(
        ws_url: &str,
        sender: &mpsc::Sender<PoolCreationEvent>,
        is_running: &Arc<std::sync::atomic::AtomicBool>,
    ) {
        loop {
            if !is_running.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            match PubsubClient::new(ws_url).await {
                Ok(pubsub_client) => {
                    info!("Connected to WebSocket: {}", ws_url);

                    let program_id = Pubkey::from_str(PUMP_FUN_PROGRAM_ID)
                        .expect("Invalid pump.fun program ID");

                    let filter = solana_client::rpc_config::RpcTransactionLogsFilter::Mentions(vec![
                        program_id.to_string(),
                    ]);

                    let config = solana_client::rpc_config::RpcTransactionLogsConfig {
                        commitment: Some(solana_sdk::commitment_config::CommitmentConfig::confirmed()),
                    };

                    match pubsub_client.logs_subscribe(filter, config).await {
                        Ok((_subscription, mut stream)) => {
                            info!("Subscribed to pump.fun logs");

                            while is_running.load(std::sync::atomic::Ordering::Relaxed) {
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    stream.next(),
                                ).await {
                                    Ok(Some(log_notification)) => {
                                        Self::process_log(&log_notification, sender).await;
                                    }
                                    Ok(None) => {
                                        warn!("WebSocket stream ended, reconnecting...");
                                        break;
                                    }
                                    Err(_) => {
                                        debug!("WebSocket heartbeat timeout, reconnecting...");
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("Failed to subscribe to logs: {}", e);
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect to WebSocket: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Process a single log notification, looking for Create/CreateV2 events
    async fn process_log(log: &solana_client::rpc_response::RpcLogsResponse, sender: &mpsc::Sender<PoolCreationEvent>) {
        let logs = &log.logs;

        // Look for the "Program log: Instruction: Create" or CreateV2 marker
        for log_line in logs {
            if log_line.contains("Program log: Instruction: Create") {
                debug!("Detected new pool creation in logs: {:?}", log.signature);

                if let Some(event) = Self::decode_pool_creation(log) {
                    match sender.send(event).await {
                        Ok(_) => debug!("New pool event sent to processing pipeline"),
                        Err(e) => error!("Failed to send pool creation event: {}", e),
                    }
                }
                break; // Only process the first Create instruction per log set
            }
        }
    }

    /// Decode a PoolCreationEvent from raw log data
    fn decode_pool_creation(log: &solana_client::rpc_response::RpcLogsResponse) -> Option<PoolCreationEvent> {
        let logs = &log.logs;

        let mut name = String::new();
        let mut symbol = String::new();
        let mut uri = String::new();
        let mut mint = None;
        let mut bonding_curve = None;
        let mut associated_bonding_curve = None;
        let mut user = None;
        let mut mayhem = false;

        for log_line in logs {
            if log_line.starts_with("Program data: ") {
                let data = &log_line["Program data: ".len()..];

                // Attempt base64 decode
                if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data) {
                    if decoded.len() >= 128 {
                        if let Ok(parsed) = Self::parse_create_instruction(&decoded[8..]) {
                            name = parsed.name;
                            symbol = parsed.symbol;
                            uri = parsed.uri;
                            mint = Some(parsed.mint);
                            bonding_curve = Some(parsed.bonding_curve);
                            associated_bonding_curve = Some(parsed.associated_bonding_curve);
                            user = Some(parsed.user);
                            mayhem = parsed.mayhem;
                        }
                    }
                }
            }
        }

        // All fields must be present
        let mint = mint?;
        let bonding_curve = bonding_curve?;
        let associated_bonding_curve = associated_bonding_curve?;
        let user = user?;

        Some(PoolCreationEvent {
            mint,
            bonding_curve,
            associated_bonding_curve,
            user,
            name,
            symbol,
            uri,
            mayhem,
            slot: 0, // Will be filled by caller
            timestamp: Utc::now(),
            signature: log.signature.clone(),
        })
    }

    /// Parse borsh-serialized Create instruction data
    fn parse_create_instruction(data: &[u8]) -> SniperResult<CreateInstructionData> {
        let mut offset = 0;

        // Read name
        let name_len = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]) as usize;
        offset += 4;
        let name = String::from_utf8(data[offset..offset+name_len].to_vec())
            .map_err(|e| SniperError::DecodeError { msg: format!("Invalid name UTF-8: {}", e) })?;
        offset += name_len;

        // Read symbol
        let symbol_len = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]) as usize;
        offset += 4;
        let symbol = String::from_utf8(data[offset..offset+symbol_len].to_vec())
            .map_err(|e| SniperError::DecodeError { msg: format!("Invalid symbol UTF-8: {}", e) })?;
        offset += symbol_len;

        // Read uri
        let uri_len = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]) as usize;
        offset += 4;
        let uri = String::from_utf8(data[offset..offset+uri_len].to_vec())
            .map_err(|e| SniperError::DecodeError { msg: format!("Invalid uri UTF-8: {}", e) })?;
        offset += uri_len;

        // Read mint
        let mint_bytes: [u8; 32] = data[offset..offset+32].try_into()
            .map_err(|_| SniperError::DecodeError { msg: "Invalid mint pubkey bytes".into() })?;
        let mint = Pubkey::new_from_array(mint_bytes);
        offset += 32;

        // Read bonding_curve
        let bc_bytes: [u8; 32] = data[offset..offset+32].try_into()
            .map_err(|_| SniperError::DecodeError { msg: "Invalid bonding_curve pubkey bytes".into() })?;
        let bonding_curve = Pubkey::new_from_array(bc_bytes);
        offset += 32;

        // Read associated_bonding_curve
        let abc_bytes: [u8; 32] = data[offset..offset+32].try_into()
            .map_err(|_| SniperError::DecodeError { msg: "Invalid associated_bonding_curve pubkey bytes".into() })?;
        let associated_bonding_curve = Pubkey::new_from_array(abc_bytes);
        offset += 32;

        // Read user
        let user_bytes: [u8; 32] = data[offset..offset+32].try_into()
            .map_err(|_| SniperError::DecodeError { msg: "Invalid user pubkey bytes".into() })?;
        let user = Pubkey::new_from_array(user_bytes);
        offset += 32;

        // Read mayhem
        let mayhem = data[offset] != 0;

        Ok(CreateInstructionData {
            name,
            symbol,
            uri,
            mint,
            bonding_curve,
            associated_bonding_curve,
            user,
            mayhem,
        })
    }
}

struct CreateInstructionData {
    name: String,
    symbol: String,
    uri: String,
    mint: Pubkey,
    bonding_curve: Pubkey,
    associated_bonding_curve: Pubkey,
    user: Pubkey,
    mayhem: bool,
}
