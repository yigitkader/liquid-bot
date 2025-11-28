use anyhow::{Context, Result};
use solana_sdk::{
    signature::{Keypair, Signer},
    pubkey::Pubkey,
};
use std::fs;
use std::path::Path;
use crate::balance_reservation::BalanceChecker;

pub struct WalletManager {
    keypair: Keypair,
    pubkey: Pubkey,
}

impl WalletManager {
    pub fn from_file<P: AsRef<Path>>(wallet_path: P) -> Result<Self> {
        let wallet_data = fs::read_to_string(&wallet_path)
            .with_context(|| format!("Failed to read wallet file: {:?}", wallet_path.as_ref()))?;
        
        let keypair = if wallet_data.trim_start().starts_with('[') {
            let bytes: Vec<u8> = serde_json::from_str(&wallet_data)
                .context("Failed to parse wallet file as JSON array")?;
            Keypair::from_bytes(&bytes)
                .context("Failed to create keypair from bytes")?
        } else {
            // Try to parse as base58 encoded secret key (alternative format)
            // Some wallets export as base58 string
            match bs58::decode(wallet_data.trim()).into_vec() {
                Ok(bytes) => {
                    if bytes.len() == 64 {
                        Keypair::from_bytes(&bytes)
                            .context("Failed to create keypair from base58 secret key")?
                    } else {
                        return Err(anyhow::anyhow!(
                            "Invalid base58 secret key length: {} bytes (expected 64 bytes)",
                            bytes.len()
                        ));
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Failed to decode base58 wallet format: {}. \
                         Expected one of:\n\
                         - JSON array format: [1,2,3,...]\n\
                         - Base58 encoded secret key (64 bytes)",
                        e
                    ));
                }
            }
        };
        
        let pubkey = keypair.pubkey();
        
        Ok(WalletManager { keypair, pubkey })
    }
    
    pub fn pubkey(&self) -> &Pubkey {
        &self.pubkey
    }
    
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }
    
    pub fn into_keypair(self) -> Keypair {
        self.keypair
    }
}

pub struct WalletBalanceChecker {
    wallet_pubkey: Pubkey,
    rpc_client: std::sync::Arc<crate::solana_client::SolanaClient>,
    config: Option<crate::config::Config>,
}

impl WalletBalanceChecker {
    pub fn new(
        wallet_pubkey: Pubkey,
        rpc_client: std::sync::Arc<crate::solana_client::SolanaClient>,
        config: Option<crate::config::Config>,
    ) -> Self {
        WalletBalanceChecker {
            wallet_pubkey,
            rpc_client,
            config,
        }
    }
    
    pub async fn get_sol_balance(&self) -> Result<u64> {
        let account = self.rpc_client.get_account(&self.wallet_pubkey).await?;
        Ok(account.lamports)
    }
    
    pub async fn has_sufficient_capital(&self, required_capital: u64, min_reserve: u64) -> Result<bool> {
        let balance = self.get_sol_balance().await?;
        let available = balance.saturating_sub(min_reserve);
        Ok(available >= required_capital)
    }
    
    pub async fn get_available_capital(&self, min_reserve: u64) -> Result<u64> {
        let balance = self.get_sol_balance().await?;
        Ok(balance.saturating_sub(min_reserve))
    }

    fn get_associated_token_address(&self, mint: &Pubkey) -> Result<Pubkey> {
        crate::protocols::solend_accounts::get_associated_token_address(&self.wallet_pubkey, mint, self.config.as_ref())
    }
    
    pub async fn get_token_balance(&self, mint: &Pubkey) -> Result<u64> {
        let token_account = self.get_associated_token_address(mint)?;
        
        match self.rpc_client.get_account(&token_account).await {
            Ok(account) => {
                if account.data.is_empty() {
                    return Ok(0);
                }
                
                // SPL Token account state'ini parse et
                // spl-token 4.0'da Account struct'ı Pack trait'ini implement ediyor
                use spl_token::state::Account;
                use solana_sdk::program_pack::Pack;
                
                // Account struct'ını parse et
                // Note: spl-token 4.0'da Account Pack trait'ini implement ediyor, unpack kullanıyoruz
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

    pub async fn has_sufficient_token_balance(&self, mint: &Pubkey, required_amount: u64) -> Result<bool> {
        let balance = self.get_token_balance(mint).await?;
        Ok(balance >= required_amount)
    }

    pub async fn has_sufficient_capital_for_liquidation(
        &self,
        debt_mint: &Pubkey,
        required_debt_amount: u64,
        min_sol_reserve: u64,
    ) -> Result<bool> {
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

    pub async fn ensure_token_account_exists(&self, mint: &Pubkey) -> Result<bool> {
        let ata = self.get_associated_token_address(mint)?;
        
        match self.rpc_client.get_account(&ata).await {
            Ok(account) => {
                Ok(!account.data.is_empty())
            }
            Err(_) => {
                Ok(false)
            }
        }
    }
}

// Implement BalanceChecker trait for WalletBalanceChecker
// This allows it to be used with BalanceReservation::try_reserve_with_check
impl BalanceChecker for WalletBalanceChecker {
    async fn get_token_balance(&self, mint: &Pubkey) -> Result<u64> {
        WalletBalanceChecker::get_token_balance(self, mint).await
    }
}

