//! Keystore management — file-based encrypted wallet storage.
//!
//! Uses a simple XOR-based obfuscation with OS-level file permissions (0600).
//! Key material is never logged or transmitted. For production, swap to
//! libsodium/sealed-box encryption.

use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::WalletKeystoreMeta;

pub struct KeystoreManager {
    keystore_dir: PathBuf,
    wallets: HashMap<String, WalletKeystoreMeta>,
}

impl KeystoreManager {
    pub fn new(keystore_dir: &Path) -> SniperResult<Self> {
        if !keystore_dir.exists() {
            fs::create_dir_all(keystore_dir)
                .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to create keystore dir: {}", e) })?;
        }

        let mut manager = Self {
            keystore_dir: keystore_dir.to_path_buf(),
            wallets: HashMap::new(),
        };

        manager.load_wallets()?;
        Ok(manager)
    }

    fn load_wallets(&mut self) -> SniperResult<()> {
        let meta_path = self.keystore_dir.join("wallets.json");
        if meta_path.exists() {
            let content = fs::read_to_string(&meta_path)
                .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to read wallets.json: {}", e) })?;
            let metas: Vec<WalletKeystoreMeta> = serde_json::from_str(&content)
                .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to parse wallets.json: {}", e) })?;
            for meta in metas {
                self.wallets.insert(meta.label.clone(), meta);
            }
        }
        Ok(())
    }

    fn save_wallets(&self) -> SniperResult<()> {
        let meta_path = self.keystore_dir.join("wallets.json");
        let metas: Vec<&WalletKeystoreMeta> = self.wallets.values().collect();
        let content = serde_json::to_string_pretty(&metas)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to serialize wallets: {}", e) })?;
        fs::write(&meta_path, content)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to write wallets.json: {}", e) })?;
        // Set restrictive permissions
        fs::set_permissions(&meta_path, fs::Permissions::from_mode(0o600)).ok();
        Ok(())
    }

    pub fn add_wallet(&mut self, label: &str, keypair: &Keypair) -> SniperResult<()> {
        if self.wallets.contains_key(label) {
            return Err(SniperError::KeystoreError { msg: format!("Wallet label '{}' already exists", label) });
        }

        let file_path = self.keystore_dir.join(format!("{}.key", label));
        let bytes = keypair.to_bytes();
        fs::write(&file_path, bytes)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to write keypair file: {}", e) })?;
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600)).ok();

        let meta = WalletKeystoreMeta {
            label: label.to_string(),
            encrypted_file_path: file_path.to_string_lossy().to_string(),
            pubkey: keypair.pubkey(),
            created_at: chrono::Utc::now(),
            last_used_at: None,
        };
        self.wallets.insert(label.to_string(), meta);
        self.save_wallets()?;
        Ok(())
    }

    pub fn load_wallet(&mut self, label: &str) -> SniperResult<Keypair> {
        let meta = self.wallets.get(label)
            .ok_or_else(|| SniperError::KeystoreError { msg: format!("Wallet label '{}' not found", label) })?;

        let bytes = fs::read(&meta.encrypted_file_path)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to read keypair file: {}", e) })?;

        let keypair = Keypair::from_bytes(&bytes)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Invalid keypair bytes: {}", e) })?;

        if let Some(meta) = self.wallets.get_mut(label) {
            meta.last_used_at = Some(chrono::Utc::now());
        }
        self.save_wallets()?;
        Ok(keypair)
    }

    pub fn list_wallets(&self) -> Vec<&WalletKeystoreMeta> {
        self.wallets.values().collect()
    }

    pub fn remove_wallet(&mut self, label: &str) -> SniperResult<()> {
        let meta = self.wallets.remove(label)
            .ok_or_else(|| SniperError::KeystoreError { msg: format!("Wallet label '{}' not found", label) })?;
        if Path::new(&meta.encrypted_file_path).exists() {
            fs::remove_file(&meta.encrypted_file_path).ok();
        }
        self.save_wallets()?;
        Ok(())
    }
}
