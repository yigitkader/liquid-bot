use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    transaction::Transaction,
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
};
use std::sync::Arc;
use crate::config::Config;
use crate::domain::LiquidationOpportunity;
use crate::wallet::WalletManager;
use crate::protocol::Protocol;
use crate::rate_limiter::RateLimiter;

/// Solana RPC client wrapper
pub struct SolanaClient {
    rpc_client: Arc<RpcClient>,
    rpc_url: String, // RPC URL'yi sakla (Arc için gerekli)
    commitment: CommitmentConfig,
    rate_limiter: Arc<RateLimiter>,
}

impl SolanaClient {
    /// Yeni RPC client oluşturur
    pub fn new(rpc_url: String) -> Result<Self> {
        let rpc_client = Arc::new(RpcClient::new_with_commitment(
            rpc_url.clone(),
            CommitmentConfig::confirmed(), // Confirmed commitment level
        ));
        
        // Rate limiter: minimum 100ms aralıkla request gönder (10 req/s)
        let rate_limiter = Arc::new(RateLimiter::new(100));
        
        Ok(SolanaClient {
            rpc_client,
            rpc_url,
            commitment: CommitmentConfig::confirmed(),
            rate_limiter,
        })
    }
    
    /// RPC URL'yi döndürür
    pub fn get_url(&self) -> &str {
        &self.rpc_url
    }
    
    /// Account bilgisini çeker
    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;
        
        // RpcClient sync, ama async wrapper kullanabiliriz
        let client = Arc::clone(&self.rpc_client);
        let pubkey = *pubkey;
        tokio::task::spawn_blocking(move || {
            client.get_account(&pubkey)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }
    
    /// Program account'larını çeker (getProgramAccounts)
    pub async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;
        
        let client = Arc::clone(&self.rpc_client);
        let program_id = *program_id;
        tokio::task::spawn_blocking(move || {
            client.get_program_accounts(&program_id)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }
    
    /// Transaction gönderir
    pub async fn send_transaction(
        &self,
        transaction: &Transaction,
    ) -> Result<String> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;
        
        let client = Arc::clone(&self.rpc_client);
        let tx = transaction.clone();
        let signature = tokio::task::spawn_blocking(move || {
            client.send_transaction(&tx)
                .map_err(|e| anyhow::anyhow!("Failed to send transaction: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")??;
        
        Ok(signature.to_string())
    }
    
    /// Recent blockhash alır
    pub async fn get_recent_blockhash(&self) -> Result<solana_sdk::hash::Hash> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;
        
        let client = Arc::clone(&self.rpc_client);
        tokio::task::spawn_blocking(move || {
            client.get_latest_blockhash()
                .map_err(|e| anyhow::anyhow!("Failed to get recent blockhash: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }
}

/// Likidasyon transaction'ını oluşturur ve gönderir
pub async fn execute_liquidation(
    opportunity: &LiquidationOpportunity,
    _config: &Config,
    wallet: &WalletManager,
    protocol: &dyn Protocol,
    rpc_client: &SolanaClient,
) -> Result<String> {
    log::info!(
        "Executing liquidation for account: {}",
        opportunity.account_position.account_address
    );
    
    // 1. Recent blockhash al
    let recent_blockhash = rpc_client.get_recent_blockhash().await?;
    
    // 2. Protokol trait'inden liquidation instruction al
    let liquidator_pubkey = wallet.pubkey();
    // RPC client'ı Arc olarak geç (gerçek account'ları almak için)
    let rpc_url = rpc_client.get_url().to_string();
    let rpc_client_arc = Arc::new(SolanaClient::new(rpc_url)
        .context("Failed to create RPC client for liquidation")?);
    let liquidation_ix = protocol.build_liquidation_instruction(
        opportunity, 
        liquidator_pubkey,
        Some(rpc_client_arc),
    )
        .await
        .context("Failed to build liquidation instruction")?;
    
    // 3. Compute budget instruction'larını oluştur (priority fee için)
    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000); // Yeterli compute unit
    let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(1_000); // Priority fee (micro-lamports)
    
    // 4. Tüm instruction'ları birleştir (önce compute budget, sonra liquidation)
    let instructions = vec![
        compute_budget_ix,
        priority_fee_ix,
        liquidation_ix,
    ];
    
    // 5. Transaction oluştur
    let mut transaction = Transaction::new_with_payer(
        &instructions,
        Some(liquidator_pubkey),
    );
    
    transaction.message.recent_blockhash = recent_blockhash;
    
    // 6. Wallet ile imzala
    transaction.sign(&[wallet.keypair()], recent_blockhash);
    
    // 6. RPC'ye gönder
    let signature = rpc_client.send_transaction(&transaction).await?;
    
    log::info!("Liquidation transaction sent: {}", signature);
    
    Ok(signature)
}

