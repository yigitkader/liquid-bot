use anyhow::{Context, Result};
use solana_sdk::{
    signature::{Keypair, Signer},
    pubkey::Pubkey,
};
use std::fs;
use std::path::Path;
use std::str::FromStr;

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

    /// Associated Token Account (ATA) adresini hesaplar
    fn get_associated_token_address(&self, mint: &Pubkey) -> Result<Pubkey> {
        let associated_token_program_id = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
            .map_err(|_| anyhow::anyhow!("Invalid associated token program ID"))?;
        
        let token_program_id = spl_token::id();
        let seeds = &[
            self.wallet_pubkey.as_ref(),
            token_program_id.as_ref(),
            mint.as_ref(),
        ];
        
        Pubkey::try_find_program_address(seeds, &associated_token_program_id)
            .map(|(pubkey, _)| pubkey)
            .ok_or_else(|| anyhow::anyhow!("Failed to derive associated token address"))
    }

    /// Wallet'daki belirli bir token'ın balance'ını kontrol et
    /// mint: Token mint address'i
    /// Returns: Token balance (native units, decimals'e göre)
    pub async fn get_token_balance(&self, mint: &Pubkey) -> Result<u64> {
        // ATA adresini hesapla
        let token_account = self.get_associated_token_address(mint)?;
        
        // Token account'unu RPC'den oku
        match self.rpc_client.get_account(&token_account).await {
            Ok(account) => {
                // Token account data'sını parse et
                if account.data.is_empty() {
                    // Token account yok, balance = 0
                    return Ok(0);
                }
                
                // SPL Token account state'ini parse et
                // spl-token 4.0'da Account struct'ı Pack trait'ini implement ediyor
                use spl_token::state::Account;
                use solana_sdk::program_pack::Pack;
                
                // Account struct'ını parse et
                // spl-token 4.0'da Account Pack trait'ini implement ediyor, unpack kullanabiliriz
                let token_account_state = Account::unpack(&account.data)
                    .map_err(|e| anyhow::anyhow!("Failed to parse token account data: {:?}", e))?;
                
                Ok(token_account_state.amount)
            }
            Err(_) => {
                // Token account yok, balance = 0
                Ok(0)
            }
        }
    }

    /// Belirli bir token için yeterli balance var mı kontrol et
    /// mint: Token mint address'i
    /// required_amount: Gerekli token amount'u (native units)
    pub async fn has_sufficient_token_balance(&self, mint: &Pubkey, required_amount: u64) -> Result<bool> {
        let balance = self.get_token_balance(mint).await?;
        Ok(balance >= required_amount)
    }

    /// Likidasyon için yeterli sermaye var mı kontrol et (TOKEN CİNSİNDEN)
    /// 
    /// Bu fonksiyon:
    /// 1. Debt token mint'ini kullanarak wallet'taki debt token balance'ını kontrol eder
    /// 2. max_liquidatable_amount (debt token amount'u) ile karşılaştırır
    /// 3. Ayrıca SOL balance'ını da kontrol eder (transaction fee için)
    /// 
    /// debt_mint: Debt token mint address'i
    /// required_debt_amount: Gerekli debt token amount'u (native units)
    /// min_sol_reserve: Minimum SOL rezerv (transaction fee için, lamports)
    pub async fn has_sufficient_capital_for_liquidation(
        &self,
        debt_mint: &Pubkey,
        required_debt_amount: u64,
        min_sol_reserve: u64,
    ) -> Result<bool> {
        // 1. Debt token balance'ını kontrol et
        let debt_token_balance = self.get_token_balance(debt_mint).await?;
        if debt_token_balance < required_debt_amount {
            log::debug!(
                "Insufficient debt token balance: required={}, available={}, mint={}",
                required_debt_amount,
                debt_token_balance,
                debt_mint
            );
            return Ok(false);
        }
        
        // 2. SOL balance'ını kontrol et (transaction fee için)
        let sol_balance = self.get_sol_balance().await?;
        if sol_balance < min_sol_reserve {
            log::debug!(
                "Insufficient SOL balance for transaction fee: required={}, available={}",
                min_sol_reserve,
                sol_balance
            );
            return Ok(false);
        }
        
        Ok(true)
    }

    /// Likidasyon için mevcut sermayeyi döndür (token cinsinden)
    /// 
    /// debt_mint: Debt token mint address'i
    /// min_sol_reserve: Minimum SOL rezerv (transaction fee için, lamports)
    /// 
    /// Returns: (debt_token_balance, sol_balance)
    pub async fn get_available_capital_for_liquidation(
        &self,
        debt_mint: &Pubkey,
        min_sol_reserve: u64,
    ) -> Result<(u64, u64)> {
        let debt_token_balance = self.get_token_balance(debt_mint).await?;
        let sol_balance = self.get_sol_balance().await?;
        let available_sol = sol_balance.saturating_sub(min_sol_reserve);
        
        Ok((debt_token_balance, available_sol))
    }
}

