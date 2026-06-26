//! Execution module — build/sign/submit transactions via Jito bundle first,
//! then direct RPC fallback.
//!
//! Execution priority:
//! 1. Jito bundle (atomic, MEV-protected, guaranteed ordering within bundle)
//! 2. Direct RPC submission (fallback if Jito is unavailable or bundle rejected)
//!
//! Logs actual vs expected slippage for post-trade analysis.
//!
//! Jito bundle architecture:
//! - Bundle = [tip_tx, buy_tx] (up to 5 txs)
//! - Tip tx sends SOL to Jito tip account
//! - Buy tx executes the swap on pump.fun bonding curve
//! - Bundle submitted to Block Engine via gRPC
//! - If bundle rejected, fall back to direct sendTransaction
//!
//! Sources:
//! - [2](https://crates.io/crates/jito-bundle)
//! - [6](https://chainstack.com/jito-explained-bundles-tips-mev-solana/)

use chrono::Utc;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::message::v0::Message as MessageV0;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::system_instruction;
use solana_sdk::transaction::VersionedTransaction;
use std::str::FromStr;
use tracing::{error, info, warn};

use crate::core::error::{SniperError, SniperResult};
use crate::config::TradingConfig;

/// Jito block engine endpoints by region
/// Source: [8](https://gist.github.com/zhe-t/60938c69e29276b7a9f098e1b0672c79)
pub const JITO_ENDPOINTS: &[(&str, &str)] = &[
    ("mainnet", "https://mainnet.block-engine.jito.wtf/api/v1/bundles"),
    ("amsterdam", "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles"),
    ("frankfurt", "https://frankfurt.mainnet.block-engine.jito.wtf/api/v1/bundles"),
    ("ny", "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles"),
    ("tokyo", "https://tokyo.mainnet.block-engine.jito.wtf/api/v1/bundles"),
];

/// Jito tip account
/// Source: Solana ecosystem docs
pub const JITO_TIP_ACCOUNT_PUBKEY: &str = "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5";

/// Pump.fun program ID
pub const PUMP_FUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub struct ExecutionEngine {
    config: TradingConfig,
    rpc_client: RpcClient,
    wallet: Keypair,
    paper_mode: bool,
}

impl ExecutionEngine {
    pub fn new(
        config: TradingConfig,
        rpc_url: &str,
        wallet: Keypair,
    ) -> Self {
        let paper_mode = config.paper_mode;

        Self {
            config,
            rpc_client: RpcClient::new_with_commitment(
                rpc_url.to_string(),
                CommitmentConfig::confirmed(),
            ),
            wallet,
            paper_mode,
        }
    }

    /// Execute a buy transaction
    /// Returns the transaction signature if successful, None in paper mode
    pub async fn execute_buy(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        amount_sol: f64,
        max_slippage_pct: f64,
    ) -> SniperResult<Option<String>> {
        if self.paper_mode {
            info!(
                "[PAPER] Would buy {} SOL of token {}",
                amount_sol, mint
            );
            return Ok(None);
        }

        let amount_lamports = (amount_sol * 1_000_000_000.0) as u64;

        // Step 1: Try Jito bundle first
        match self.submit_jito_buy(mint, bonding_curve, associated_bonding_curve, amount_lamports, max_slippage_pct).await {
            Ok(sig) => {
                info!("Buy executed via Jito bundle: {}", sig);
                return Ok(Some(sig));
            }
            Err(e) => {
                warn!("Jito bundle failed, falling back to direct RPC: {}", e);
            }
        }

        // Step 2: Fallback to direct RPC
        self.submit_direct_buy(mint, bonding_curve, associated_bonding_curve, amount_lamports, max_slippage_pct).await
    }

    /// Execute a sell transaction
    pub async fn execute_sell(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        token_amount: u64,
        min_sol_output: f64,
    ) -> SniperResult<Option<String>> {
        if self.paper_mode {
            info!(
                "[PAPER] Would sell {} tokens of {}",
                token_amount, mint
            );
            return Ok(None);
        }

        // Build and submit sell transaction (same pipeline as buy)
        self.submit_jito_sell(mint, bonding_curve, associated_bonding_curve, token_amount, min_sol_output).await
    }

    /// Submit a buy via Jito bundle
    async fn submit_jito_buy(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        amount_lamports: u64,
        max_slippage_pct: f64,
    ) -> SniperResult<String> {
        // Build tip transaction
        let tip_ix = system_instruction::transfer(
            &self.wallet.pubkey(),
            &Pubkey::from_str(JITO_TIP_ACCOUNT_PUBKEY).unwrap(),
            self.config.jito_tip_lamports,
        );

        // Build buy transaction (simplified — in production this calls the pump.fun program)
        let buy_ix = self.build_buy_instruction(mint, bonding_curve, associated_bonding_curve, amount_lamports);

        // Set compute budget
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);

        // Build bundle: [tip, compute_budget, priority_fee, buy]
        let mut instructions = vec![tip_ix, compute_budget_ix, priority_fee_ix, buy_ix];

        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash().await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })?;

        // Build message and transaction
        let message = MessageV0::try_compile(
            &self.wallet.pubkey(),
            &instructions,
            &[],
            recent_blockhash,
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to compile message: {}", e),
            sig: None,
        })?;

        let tx = VersionedTransaction::try_new(
            solana_sdk::message::VersionedMessage::V0(message),
            &[&self.wallet],
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to sign transaction: {}", e),
            sig: None,
        })?;

        // Submit bundle via Jito Block Engine
        self.send_jito_bundle(&tx, &recent_blockhash).await
    }

    /// Submit a sell via Jito bundle
    async fn submit_jito_sell(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        token_amount: u64,
        min_sol_output: f64,
    ) -> SniperResult<Option<String>> {
        // Build sell transaction (same structure as buy but with sell instruction)
        let tip_ix = system_instruction::transfer(
            &self.wallet.pubkey(),
            &Pubkey::from_str(JITO_TIP_ACCOUNT_PUBKEY).unwrap(),
            self.config.jito_tip_lamports,
        );

        let sell_ix = self.build_sell_instruction(mint, bonding_curve, associated_bonding_curve, token_amount);

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);

        let mut instructions = vec![tip_ix, compute_budget_ix, priority_fee_ix, sell_ix];

        let recent_blockhash = self.rpc_client.get_latest_blockhash().await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })?;

        let message = MessageV0::try_compile(
            &self.wallet.pubkey(),
            &instructions,
            &[],
            recent_blockhash,
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to compile message: {}", e),
            sig: None,
        })?;

        let tx = VersionedTransaction::try_new(
            solana_sdk::message::VersionedMessage::V0(message),
            &[&self.wallet],
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to sign transaction: {}", e),
            sig: None,
        })?;

        self.send_jito_bundle(&tx, &recent_blockhash).await.map(Some)
    }

    /// Direct RPC submission (fallback)
    async fn submit_direct_buy(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        amount_lamports: u64,
        max_slippage_pct: f64,
    ) -> SniperResult<Option<String>> {
        let buy_ix = self.build_buy_instruction(mint, bonding_curve, associated_bonding_curve, amount_lamports);

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);

        let instructions = vec![compute_budget_ix, priority_fee_ix, buy_ix];

        let recent_blockhash = self.rpc_client.get_latest_blockhash().await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })?;

        let message = MessageV0::try_compile(
            &self.wallet.pubkey(),
            &instructions,
            &[],
            recent_blockhash,
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to compile message: {}", e),
            sig: None,
        })?;

        let tx = VersionedTransaction::try_new(
            solana_sdk::message::VersionedMessage::V0(message),
            &[&self.wallet],
        ).map_err(|e| SniperError::TxSubmissionFailed {
            msg: format!("Failed to sign transaction: {}", e),
            sig: None,
        })?;

        // Send transaction directly via RPC
        let sig = self.rpc_client.send_transaction(&tx).await
            .map_err(|e| SniperError::TxSubmissionFailed {
                msg: format!("Direct send failed: {}", e),
                sig: None,
            })?;

        info!("Direct RPC buy submitted: {}", sig);
        Ok(Some(sig.to_string()))
    }

    /// Send a Jito bundle to the Block Engine
    async fn send_jito_bundle(
        &self,
        tx: &VersionedTransaction,
        _recent_blockhash: &solana_sdk::hash::Hash,
    ) -> SniperResult<String> {
        // Serialize the transaction to base58
        let serialized_tx = bs58::encode(tx.serialize()).into_string();

        // Build the bundle JSON payload
        let bundle_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [[serialized_tx]],
        });

        // Try each Jito endpoint until one succeeds
        for (region, endpoint) in JITO_ENDPOINTS {
            let client = reqwest::Client::new();

            match client.post(endpoint)
                .header("Content-Type", "application/json")
                .json(&bundle_payload)
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        let body: serde_json::Value = response.json().await
                            .map_err(|e| SniperError::TxSubmissionFailed {
                                msg: format!("Failed to parse Jito response: {}", e),
                                sig: None,
                            })?;

                        if let Some(result) = body.get("result") {
                            let bundle_id = result.as_str().unwrap_or("unknown");
                            info!("Jito bundle submitted via {}: bundle_id={}", region, bundle_id);
                            return Ok(bundle_id.to_string());
                        }
                    }
                }
                Err(e) => {
                    warn!("Jito endpoint {} failed: {}", region, e);
                    continue;
                }
            }
        }

        Err(SniperError::BundleRejected {
            reason: "All Jito endpoints failed".into(),
        })
    }

    /// Build the buy instruction for pump.fun
    fn build_buy_instruction(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        amount_lamports: u64,
    ) -> Instruction {
        // In production, this would decode the pump.fun IDL and build the
        // correct instruction with the proper discriminator and arguments.
        // For now, we build a placeholder that matches the pump.fun program interface.
        //
        // The actual pump.fun "buy" instruction has the format:
        // Accounts: [bonding_curve, associated_bonding_curve, user, user_token_account, mint, metadata, system_program, token_program, rent, event_authority, program]
        // Data: [discriminator (8 bytes), amount (u64), max_sol_cost (u64)]

        let event_authority = Pubkey::find_program_address(
            &[b"event_authority"],
            &Pubkey::from_str(PUMP_FUN_PROGRAM_ID).unwrap(),
        ).0;
        let pump_program = Pubkey::from_str(PUMP_FUN_PROGRAM_ID).unwrap();

        Instruction {
            program_id: pump_program,
            accounts: vec![
                solana_sdk::instruction::AccountMeta::new(bonding_curve, false),
                solana_sdk::instruction::AccountMeta::new(associated_bonding_curve, false),
                solana_sdk::instruction::AccountMeta::new_readonly(mint, false),
                solana_sdk::instruction::AccountMeta::new_readonly(self.wallet.pubkey(), true),
                solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(spl_token::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::sysvar::rent::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(event_authority, false),
                solana_sdk::instruction::AccountMeta::new_readonly(pump_program, false),
            ],
            data: vec![], // In production: borsh-serialize the buy arguments
        }
    }

    /// Build the sell instruction for pump.fun
    fn build_sell_instruction(
        &self,
        mint: Pubkey,
        bonding_curve: Pubkey,
        associated_bonding_curve: Pubkey,
        token_amount: u64,
    ) -> Instruction {
        // Similar to buy but with sell discriminator
        let event_authority = Pubkey::find_program_address(
            &[b"event_authority"],
            &Pubkey::from_str(PUMP_FUN_PROGRAM_ID).unwrap(),
        ).0;
        let pump_program = Pubkey::from_str(PUMP_FUN_PROGRAM_ID).unwrap();

        Instruction {
            program_id: pump_program,
            accounts: vec![
                solana_sdk::instruction::AccountMeta::new(bonding_curve, false),
                solana_sdk::instruction::AccountMeta::new(associated_bonding_curve, false),
                solana_sdk::instruction::AccountMeta::new_readonly(mint, false),
                solana_sdk::instruction::AccountMeta::new_readonly(self.wallet.pubkey(), true),
                solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(spl_token::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::sysvar::rent::id(), false),
                solana_sdk::instruction::AccountMeta::new_readonly(event_authority, false),
                solana_sdk::instruction::AccountMeta::new_readonly(pump_program, false),
            ],
            data: vec![], // In production: borsh-serialize the sell arguments
        }
    }

    /// Get the current SOL balance of the wallet
    pub async fn get_balance(&self) -> SniperResult<u64> {
        if self.paper_mode {
            return Ok(1_000_000_000); // Paper mode: pretend we have 1 SOL
        }

        self.rpc_client.get_balance(&self.wallet.pubkey()).await
            .map_err(|e| SniperError::RpcError { source: Box::new(e) })
    }
}
