use async_trait::async_trait;
use anyhow::Result;
use solana_sdk::{
    pubkey::Pubkey,
    instruction::Instruction,
    account::Account,
};
use crate::domain::AccountPosition;

/// Protokol soyutlaması - Her lending protokolü bu trait'i implement eder
#[async_trait]
pub trait Protocol: Send + Sync {
    /// Protokol adı (örn: "Solend", "MarginFi")
    fn id(&self) -> &str;
    
    /// Solana Program ID'si
    fn program_id(&self) -> Pubkey;
    
    /// Raw Solana account verisini AccountPosition'a dönüştürür
    async fn parse_account_position(
        &self,
        account_address: &Pubkey,
        account_data: &Account,
    ) -> Result<Option<AccountPosition>>;
    
    /// Health Factor hesaplar (veya protokolden alır)
    fn calculate_health_factor(&self, position: &AccountPosition) -> Result<f64>;
    
    /// Protokol parametrelerini döndürür (LTV, liquidation bonus, close factor vb.)
    fn get_liquidation_params(&self) -> LiquidationParams;
    
    /// Likidasyon instruction'ını oluşturur
    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
    ) -> Result<Instruction>;
}

/// Likidasyon parametreleri
#[derive(Debug, Clone)]
pub struct LiquidationParams {
    pub liquidation_bonus: f64,      // Likidasyon bonusu (örn: 0.05 = %5)
    pub close_factor: f64,            // Tek seferde likide edilebilecek maksimum oran (örn: 0.5 = %50)
    pub max_liquidation_slippage: f64, // Maksimum slippage toleransı
}

/// Protokol registry - Desteklenen protokolleri tutar
pub struct ProtocolRegistry {
    protocols: Vec<Box<dyn Protocol>>,
}

impl ProtocolRegistry {
    pub fn new() -> Self {
        ProtocolRegistry {
            protocols: Vec::new(),
        }
    }
    
    /// Protokol ekler
    pub fn register(&mut self, protocol: Box<dyn Protocol>) {
        self.protocols.push(protocol);
    }
    
    /// Protokol ID'sine göre bulur
    pub fn find(&self, protocol_id: &str) -> Option<&dyn Protocol> {
        self.protocols.iter()
            .find(|p| p.id() == protocol_id)
            .map(|p| p.as_ref())
    }
    
    /// Tüm protokolleri döndürür
    pub fn all(&self) -> &[Box<dyn Protocol>] {
        &self.protocols
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

