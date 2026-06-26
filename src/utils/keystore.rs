//! Keystore management — encrypted wallet storage on disk.
//!
//! Security constraint: keys are loaded from encrypted local file at boot,
//! referenced only by label over Telegram. Raw private keys NEVER appear
//! in Telegram messages, chat history, or logs.
//!
//! Encryption: ChaCha20-Poly1305 with Argon2id key derivation from a master passphrase.

use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use chacha20poly1305::aead::{Aead, OsRng};
use rand::rngs::OsRng;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{SniperError, SniperResult};
use crate::core::types::WalletKeystoreMeta;

/// Master keystore manager
pub struct KeystoreManager {
    keystore_dir: PathBuf,
    master_passphrase: String,
    wallets: HashMap<String, WalletKeystoreMeta>,
}

impl KeystoreManager {
    /// Create a new keystore manager
    pub fn new(keystore_dir: &Path, master_passphrase: &str) -> SniperResult<Self> {
        if !keystore_dir.exists() {
            fs::create_dir_all(keystore_dir)
                .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to create keystore dir: {}", e) })?;
        }

        let mut manager = Self {
            keystore_dir: keystore_dir.to_path_buf(),
            master_passphrase: master_passphrase.to_string(),
            wallets: HashMap::new(),
        };

        manager.load_wallets()?;
        Ok(manager)
    }

    /// Load all wallet metadata from the keystore directory
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

    /// Save wallet metadata to disk
    fn save_wallets(&self) -> SniperResult<()> {
        let meta_path = self.keystore_dir.join("wallets.json");
        let metas: Vec<&WalletKeystoreMeta> = self.wallets.values().collect();
        let content = serde_json::to_string_pretty(&metas)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to serialize wallets: {}", e) })?;

        fs::write(meta_path, content)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to write wallets.json: {}", e) })?;

        Ok(())
    }

    /// Add a new wallet from a keypair (encrypts immediately)
    pub fn add_wallet(&mut self, label: &str, keypair: &Keypair) -> SniperResult<()> {
        if self.wallets.contains_key(label) {
            return Err(SniperError::KeystoreError {
                msg: format!("Wallet label '{}' already exists", label),
            });
        }

        let encrypted_data = self.encrypt_keypair(keypair)?;
        let file_path = self.keystore_dir.join(format!("{}.enc", label));

        fs::write(&file_path, encrypted_data)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to write encrypted keypair: {}", e) })?;

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

    /// Load and decrypt a wallet by label
    pub fn load_wallet(&mut self, label: &str) -> SniperResult<Keypair> {
        let meta = self.wallets.get(label)
            .ok_or_else(|| SniperError::KeystoreError { msg: format!("Wallet label '{}' not found", label) })?;

        let encrypted_data = fs::read(&meta.encrypted_file_path)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to read encrypted file: {}", e) })?;

        let keypair = self.decrypt_keypair(&encrypted_data)?;

        // Update last_used_at
        if let Some(meta) = self.wallets.get_mut(label) {
            meta.last_used_at = Some(chrono::Utc::now());
        }
        self.save_wallets()?;

        Ok(keypair)
    }

    /// List all wallet labels (never exposes keys)
    pub fn list_wallets(&self) -> Vec<&WalletKeystoreMeta> {
        self.wallets.values().collect()
    }

    /// Remove a wallet by label
    pub fn remove_wallet(&mut self, label: &str) -> SniperResult<()> {
        let meta = self.wallets.remove(label)
            .ok_or_else(|| SniperError::KeystoreError { msg: format!("Wallet label '{}' not found", label) })?;

        if Path::new(&meta.encrypted_file_path).exists() {
            fs::remove_file(&meta.encrypted_file_path)
                .map_err(|e| SniperError::KeystoreError { msg: format!("Failed to delete encrypted file: {}", e) })?;
        }

        self.save_wallets()?;
        Ok(())
    }

    /// Encrypt a keypair using ChaCha20-Poly1305
    fn encrypt_keypair(&self, keypair: &Keypair) -> SniperResult<Vec<u8>> {
        // Derive key from passphrase using Argon2id
        let salt = rand::random::<[u8; 16]>();
        let key = self.derive_key(&salt)?;

        let cipher = ChaCha20Poly1305::new(&key.into());
        let nonce = Nonce::from(rand::random::<[u8; 12]>());

        let plaintext = keypair.to_bytes();
        let ciphertext = cipher.encrypt(&nonce, plaintext.as_ref())
            .map_err(|e| SniperError::KeystoreError { msg: format!("Encryption failed: {}", e) })?;

        // Prepend salt + nonce to ciphertext
        let mut result = Vec::with_capacity(16 + 12 + ciphertext.len());
        result.extend_from_slice(&salt);
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    /// Decrypt a keypair from encrypted bytes
    fn decrypt_keypair(&self, data: &[u8]) -> SniperResult<Keypair> {
        if data.len() < 28 {
            return Err(SniperError::KeystoreError {
                msg: "Encrypted data too short".into(),
            });
        }

        let salt: [u8; 16] = data[0..16].try_into().unwrap();
        let nonce: [u8; 12] = data[16..28].try_into().unwrap();
        let ciphertext = &data[28..];

        let key = self.derive_key(&salt)?;
        let cipher = ChaCha20Poly1305::new(&key.into());
        let nonce_ref = Nonce::from(nonce);

        let plaintext = cipher.decrypt(&nonce_ref, ciphertext)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Decryption failed: {}", e) })?;

        let bytes: [u8; 64] = plaintext.try_into()
            .map_err(|_| SniperError::KeystoreError { msg: "Decrypted data has wrong length".into() })?;

        let keypair = Keypair::from_bytes(&bytes)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Invalid keypair bytes: {}", e) })?;

        Ok(keypair)
    }

    /// Derive encryption key from passphrase using Argon2id
    fn derive_key(&self, salt: &[u8; 16]) -> SniperResult<[u8; 32]> {
        use argon2::{Argon2, Params, Version};

        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            Params::new(
                64 * 1024,  // 64 MB memory
                3,          // 3 iterations
                1,          // 1 parallelism
                Some(32),   // 32-byte key
            ).map_err(|e| SniperError::KeystoreError { msg: format!("Argon2 params error: {}", e) })?,
        );

        let mut key = [0u8; 32];
        argon2.hash_password_into(self.master_passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| SniperError::KeystoreError { msg: format!("Argon2 hashing failed: {}", e) })?;

        Ok(key)
    }
}
