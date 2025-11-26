use async_trait::async_trait;
use anyhow::Result;
use solana_sdk::{
    pubkey::Pubkey,
    instruction::Instruction,
    account::Account,
};
use crate::domain::AccountPosition;
use crate::solana_client::SolanaClient;
use std::sync::Arc;

/// Protokol soyutlaması - Her lending protokolü bu trait'i implement eder
#[async_trait]
pub trait Protocol: Send + Sync {
    /// Protokol adı (örn: "Solend", "MarginFi")
    fn id(&self) -> &str;
    
    /// Solana Program ID'si
    fn program_id(&self) -> Pubkey;
    
    /// Raw Solana account verisini AccountPosition'a dönüştürür
    /// 
    /// todo NOT: RPC client parametresi eklendi çünkü gerçek mint address'lerini almak için
    /// reserve account'larını RPC'den okumak gerekiyor.
    /// 
    /// rpc_client: None ise reserve pubkey'leri mint olarak kullanılır (fallback)
    /// rpc_client: Some(client) ise gerçek mint address'leri reserve'den alınır
    async fn parse_account_position(
        &self,
        account_address: &Pubkey,
        account_data: &Account,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Option<AccountPosition>>;
    
    /// Health Factor hesaplar (veya protokolden alır)
    fn calculate_health_factor(&self, position: &AccountPosition) -> Result<f64>;
    
    /// Protokol parametrelerini döndürür (LTV, liquidation bonus, close factor vb.)
    fn get_liquidation_params(&self) -> LiquidationParams;
    
    /// Likidasyon instruction'ını oluşturur
    /// 
    /// NOT: RPC client parametresi eklendi çünkü gerçek account'ları almak için
    /// reserve account'larını RPC'den okumak gerekiyor.
    /// 
    /// rpc_client: None ise placeholder account'lar kullanılır (dry-run için)
    /// rpc_client: Some(client) ise gerçek account'lar RPC'den alınır
    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction>;
}

/// Likidasyon parametreleri
#[derive(Debug, Clone)]
pub struct LiquidationParams {
    pub liquidation_bonus: f64,      // Likidasyon bonusu (örn: 0.05 = %5)
    pub close_factor: f64,            // Tek seferde likide edilebilecek maksimum oran (örn: 0.5 = %50)
    pub max_liquidation_slippage: f64, // Maksimum slippage toleransı
}


