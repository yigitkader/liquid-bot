use anyhow::{Context, Result};
use solana_sdk::{
    signature::{Keypair, Signer},
    pubkey::Pubkey,
};
use std::fs;
use std::path::Path;

/// Wallet yönetimi - Keypair okuma ve transaction imzalama
pub struct WalletManager {
    keypair: Keypair,
    pubkey: Pubkey,
}

impl WalletManager {
    /// Wallet dosyasından Keypair yükler
    pub fn from_file<P: AsRef<Path>>(wallet_path: P) -> Result<Self> {
        let wallet_data = fs::read_to_string(&wallet_path)
            .with_context(|| format!("Failed to read wallet file: {:?}", wallet_path.as_ref()))?;
        
        // Solana wallet dosyası JSON formatındadır
        // İki format olabilir: array format [1,2,3,...] veya object format
        let keypair = if wallet_data.trim_start().starts_with('[') {
            // Array format: [1,2,3,...]
            let bytes: Vec<u8> = serde_json::from_str(&wallet_data)
                .context("Failed to parse wallet file as JSON array")?;
            Keypair::from_bytes(&bytes)
                .context("Failed to create keypair from bytes")?
        } else {
            // Object format veya başka format - Keypair::read_from_file kullan
            // Ancak bu fonksiyon deprecated, manuel parse edelim
            return Err(anyhow::anyhow!(
                "Unsupported wallet format. Expected JSON array format [1,2,3,...]"
            ));
        };
        
        let pubkey = keypair.pubkey();
        
        Ok(WalletManager { keypair, pubkey })
    }
    
    /// Public key'i döndürür
    pub fn pubkey(&self) -> &Pubkey {
        &self.pubkey
    }
    
    /// Keypair referansını döndürür (transaction imzalama için)
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }
    
    /// Keypair'i consume eder (bazı durumlarda gerekli)
    pub fn into_keypair(self) -> Keypair {
        self.keypair
    }
}

/// Wallet balance kontrolü için helper
pub struct WalletBalanceChecker {
    wallet_pubkey: Pubkey,
    rpc_client: std::sync::Arc<crate::solana_client::SolanaClient>,
}

impl WalletBalanceChecker {
    pub fn new(wallet_pubkey: Pubkey, rpc_client: std::sync::Arc<crate::solana_client::SolanaClient>) -> Self {
        WalletBalanceChecker {
            wallet_pubkey,
            rpc_client,
        }
    }

    /// Wallet'daki SOL balance'ı kontrol et
    pub async fn get_sol_balance(&self) -> Result<u64> {
        let account = self.rpc_client.get_account(&self.wallet_pubkey).await?;
        Ok(account.lamports)
    }

    /// Likidasyon için yeterli sermaye var mı kontrol et
    /// required_capital: Gerekli sermaye (lamports cinsinden)
    /// min_reserve: Minimum rezerv (transaction fee için, lamports cinsinden)
    pub async fn has_sufficient_capital(&self, required_capital: u64, min_reserve: u64) -> Result<bool> {
        let balance = self.get_sol_balance().await?;
        let available = balance.saturating_sub(min_reserve);
        Ok(available >= required_capital)
    }

    /// Wallet'daki mevcut sermayeyi döndür (lamports)
    pub async fn get_available_capital(&self, min_reserve: u64) -> Result<u64> {
        let balance = self.get_sol_balance().await?;
        Ok(balance.saturating_sub(min_reserve))
    }
}

