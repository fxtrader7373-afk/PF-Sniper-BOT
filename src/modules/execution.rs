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
use tracing::{info, warn};
use crate::core::error::{SniperError, SniperResult};
use crate::config::TradingConfig;

pub const JITO_ENDPOINTS: &[(&str, &str)] = &[
    ("mainnet", "https://mainnet.block-engine.jito.wtf/api/v1/bundles"),
    ("amsterdam", "https://amsterdam.mainnet.block-engine.jito.wtf/api/v1/bundles"),
];
pub const JITO_TIP: &str = "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5";
pub const PUMP_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub struct ExecutionEngine {
    rpc_client: RpcClient,
    wallet: Keypair,
    config: TradingConfig,
    paper_mode: bool,
}

impl ExecutionEngine {
    pub fn new(config: TradingConfig, rpc_url: &str, wallet: Keypair) -> Self {
        Self {
            rpc_client: RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed()),
            wallet,
            config: config.clone(),
            paper_mode: config.paper_mode,
        }
    }

    pub async fn execute_buy(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, amount_sol: f64, _max_slippage_pct: f64) -> SniperResult<Option<String>> {
        if self.paper_mode { info!("[PAPER] Would buy {} SOL of {}", amount_sol, mint); return Ok(None); }
        let amount_lamports = (amount_sol * 1e9) as u64;
        match self.submit_jito_buy(mint, bonding_curve, associated_bonding_curve, amount_lamports).await {
            Ok(sig) => { info!("Jito OK: {}", sig); Ok(Some(sig)) }
            Err(e) => { warn!("Jito failed: {}", e); self.submit_direct_buy(mint, bonding_curve, associated_bonding_curve, amount_lamports).await }
        }
    }

    pub async fn execute_sell(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, token_amount: u64, _min_sol_output: f64) -> SniperResult<Option<String>> {
        if self.paper_mode { info!("[PAPER] Would sell {} of {}", token_amount, mint); return Ok(None); }
        self.submit_jito_sell(mint, bonding_curve, associated_bonding_curve, token_amount).await
    }

    async fn submit_jito_buy(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, amount_lamports: u64) -> SniperResult<String> {
        let tip = system_instruction::transfer(&self.wallet.pubkey(), &Pubkey::from_str(JITO_TIP).unwrap(), self.config.jito_tip_lamports);
        let buy = self.build_buy_ix(mint, bonding_curve, associated_bonding_curve, amount_lamports);
        let cb = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let pf = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);
        let bh = self.rpc_client.get_latest_blockhash().await.map_err(|e| SniperError::RpcError { msg: e.to_string() })?;
        let msg = MessageV0::try_compile(&self.wallet.pubkey(), &[tip, cb, pf, buy], &[], bh).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        let tx = VersionedTransaction::try_new(solana_sdk::message::VersionedMessage::V0(msg), &[&self.wallet]).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        self.send_jito(&tx).await
    }

    async fn submit_jito_sell(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, token_amount: u64) -> SniperResult<Option<String>> {
        let tip = system_instruction::transfer(&self.wallet.pubkey(), &Pubkey::from_str(JITO_TIP).unwrap(), self.config.jito_tip_lamports);
        let sell = self.build_sell_ix(mint, bonding_curve, associated_bonding_curve, token_amount);
        let cb = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let pf = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);
        let bh = self.rpc_client.get_latest_blockhash().await.map_err(|e| SniperError::RpcError { msg: e.to_string() })?;
        let msg = MessageV0::try_compile(&self.wallet.pubkey(), &[tip, cb, pf, sell], &[], bh).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        let tx = VersionedTransaction::try_new(solana_sdk::message::VersionedMessage::V0(msg), &[&self.wallet]).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        self.send_jito(&tx).await.map(Some)
    }

    async fn submit_direct_buy(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, amount_lamports: u64) -> SniperResult<Option<String>> {
        let buy = self.build_buy_ix(mint, bonding_curve, associated_bonding_curve, amount_lamports);
        let cb = ComputeBudgetInstruction::set_compute_unit_limit(self.config.compute_unit_limit);
        let pf = ComputeBudgetInstruction::set_compute_unit_price(self.config.priority_fee_micro_lamports);
        let bh = self.rpc_client.get_latest_blockhash().await.map_err(|e| SniperError::RpcError { msg: e.to_string() })?;
        let msg = MessageV0::try_compile(&self.wallet.pubkey(), &[cb, pf, buy], &[], bh).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        let tx = VersionedTransaction::try_new(solana_sdk::message::VersionedMessage::V0(msg), &[&self.wallet]).map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        let sig = self.rpc_client.send_transaction(&tx).await.map_err(|e| SniperError::TxSubmissionFailed { msg: e.to_string(), sig: None })?;
        info!("Direct RPC: {}", sig); Ok(Some(sig.to_string()))
    }

    async fn send_jito(&self, tx: &VersionedTransaction) -> SniperResult<String> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let encoded = STANDARD.encode(tx.message.serialize());
        let payload = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendBundle","params":[[encoded]]});
        for (region, ep) in JITO_ENDPOINTS {
            if let Ok(resp) = reqwest::Client::new().post(ep.to_string()).json(&payload).send().await {
                if resp.status().is_success() {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        if let Some(r) = body.get("result").and_then(|v| v.as_str()) {
                            info!("Jito {} bundle_id={}", region, r);
                            return Ok(r.to_string());
                        }
                    }
                }
            }
        }
        Err(SniperError::BundleRejected { reason: "All Jito failed".into() })
    }

    fn build_buy_ix(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, _amount_lamports: u64) -> Instruction {
        let p = Pubkey::from_str(PUMP_ID).unwrap();
        let ea = Pubkey::find_program_address(&[b"event_authority"], &p).0;
        Instruction { program_id: p, accounts: vec![
            solana_sdk::instruction::AccountMeta::new(bonding_curve, false),
            solana_sdk::instruction::AccountMeta::new(associated_bonding_curve, false),
            solana_sdk::instruction::AccountMeta::new_readonly(mint, false),
            solana_sdk::instruction::AccountMeta::new_readonly(self.wallet.pubkey(), true),
            solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(spl_token::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::sysvar::rent::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(ea, false),
            solana_sdk::instruction::AccountMeta::new_readonly(p, false),
        ], data: vec![] }
    }

    fn build_sell_ix(&self, mint: Pubkey, bonding_curve: Pubkey, associated_bonding_curve: Pubkey, _token_amount: u64) -> Instruction {
        let p = Pubkey::from_str(PUMP_ID).unwrap();
        let ea = Pubkey::find_program_address(&[b"event_authority"], &p).0;
        Instruction { program_id: p, accounts: vec![
            solana_sdk::instruction::AccountMeta::new(bonding_curve, false),
            solana_sdk::instruction::AccountMeta::new(associated_bonding_curve, false),
            solana_sdk::instruction::AccountMeta::new_readonly(mint, false),
            solana_sdk::instruction::AccountMeta::new_readonly(self.wallet.pubkey(), true),
            solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(spl_token::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::sysvar::rent::id(), false),
            solana_sdk::instruction::AccountMeta::new_readonly(ea, false),
            solana_sdk::instruction::AccountMeta::new_readonly(p, false),
        ], data: vec![] }
    }

    pub async fn get_balance(&self) -> SniperResult<u64> {
        if self.paper_mode { return Ok(1_000_000_000); }
        self.rpc_client.get_balance(&self.wallet.pubkey()).await.map_err(|e| SniperError::RpcError { msg: e.to_string() })
    }
}
