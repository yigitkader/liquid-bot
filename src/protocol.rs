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

/// Protokol registry - Desteklenen protokolleri tutar
/// 
/// Bu yapı, trait tabanlı mimari sayesinde gelecekte çoklu protokol desteği için hazırdır.
/// Şu an tek protokol (Solend) kullanılıyor, ancak yeni protokol eklemek için
/// sadece `register()` çağrısı yeterlidir.
pub struct ProtocolRegistry {
    protocols: Vec<Box<dyn Protocol>>,
}

impl ProtocolRegistry {
    /// Yeni bir protocol registry oluşturur
    pub fn new() -> Self {
        ProtocolRegistry {
            protocols: Vec::new(),
        }
    }
    
    /// Protokol ekler
    /// 
    /// Gelecekte yeni protokol eklemek için:
    /// ```rust
    /// let marginfi = MarginFiProtocol::new()?;
    /// registry.register(Box::new(marginfi));
    /// ```
    pub fn register(&mut self, protocol: Box<dyn Protocol>) {
        let protocol_id = protocol.id().to_string();
        self.protocols.push(protocol);
        log::debug!("Protocol '{}' registered in registry", protocol_id);
    }
    
    /// Protokol ID'sine göre bulur
    /// 
    /// Gelecekte çoklu protokol desteğinde kullanılacak:
    /// ```rust
    /// let protocol = registry.find("Solend")?;
    /// ```
    pub fn find(&self, protocol_id: &str) -> Option<&dyn Protocol> {
        self.protocols.iter()
            .find(|p| p.id() == protocol_id)
            .map(|p| p.as_ref())
    }
    
    /// Tüm protokolleri döndürür
    /// 
    /// Gelecekte çoklu protokol desteğinde tüm protokolleri iterate etmek için kullanılacak
    pub fn all(&self) -> &[Box<dyn Protocol>] {
        &self.protocols
    }
    
    /// Şu an aktif protokol sayısını döndürür
    pub fn count(&self) -> usize {
        self.protocols.len()
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

