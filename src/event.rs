use crate::domain::{AccountPosition, LiquidationOpportunity};

/// Sistem genelinde kullanılan event enum'u
#[derive(Debug, Clone)]
pub enum Event {
    /// Hesap pozisyonu güncellendi
    AccountUpdated(AccountPosition),
    
    /// Potansiyel likidasyon fırsatı bulundu
    PotentiallyLiquidatable(LiquidationOpportunity),
    
    /// Likidasyon işlemi yürütülmeli
    ExecuteLiquidation(LiquidationOpportunity),
    
    /// Transaction sonucu
    TxResult {
        opportunity: LiquidationOpportunity,
        success: bool,
        signature: Option<String>,
        error: Option<String>,
    },
}

