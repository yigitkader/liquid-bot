//! Reserve Account Validator
//! 
//! Bu modül, gerçek Solend reserve account'larını parse ederek
//! struct yapısının doğruluğunu doğrular.
//! 
//! Kullanım:
//! 1. Mainnet'teki gerçek bir reserve account'unu oku
//! 2. Parse et
//! 3. Eğer parse başarılıysa, struct doğru demektir
//! 4. Eğer parse başarısızsa, struct yapısını düzelt

use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use crate::protocols::solend_reserve::SolendReserve;
use crate::solana_client::SolanaClient;
use std::sync::Arc;

/// Gerçek mainnet reserve account'larını test et
/// 
/// Bu fonksiyon, bilinen reserve account'larını parse ederek
/// struct yapısının doğruluğunu doğrular.
pub async fn validate_reserve_structure(
    rpc_client: Arc<SolanaClient>,
    reserve_pubkey: &Pubkey,
) -> Result<ValidationResult> {
    log::info!("Validating reserve account structure: {}", reserve_pubkey);
    
    // Reserve account'unu oku
    let account = rpc_client.get_account(reserve_pubkey).await
        .context("Failed to fetch reserve account")?;
    
    if account.data.is_empty() {
        return Err(anyhow::anyhow!("Reserve account data is empty"));
    }
    
    log::debug!("Reserve account data size: {} bytes", account.data.len());
    
    // Parse et
    match SolendReserve::from_account_data(&account.data) {
        Ok(reserve) => {
            log::info!("✅ Reserve account parsed successfully!");
            log::debug!(
                "Reserve details: version={}, lending_market={}, liquidity_mint={}, collateral_mint={}",
                reserve.version,
                reserve.lending_market,
                reserve.liquidity_mint(),
                reserve.collateral_mint()
            );
            
            Ok(ValidationResult {
                success: true,
                error: None,
                reserve_info: Some(ReserveInfo {
                    version: reserve.version,
                    lending_market: reserve.lending_market,
                    liquidity_mint: reserve.liquidity_mint(),
                    collateral_mint: reserve.collateral_mint(),
                    ltv: reserve.ltv(),
                    liquidation_bonus: reserve.liquidation_bonus(),
                }),
            })
        }
        Err(e) => {
            log::error!("❌ Failed to parse reserve account: {}", e);
            log::error!("   This indicates the struct structure doesn't match the real Solend IDL");
            log::error!("   Please update src/protocols/solend_reserve.rs with the correct structure");
            
            // Hex dump ilk 100 byte'ı göster (debug için)
            let hex_dump: String = account.data.iter()
                .take(100)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::debug!("First 100 bytes (hex): {}", hex_dump);
            
            Ok(ValidationResult {
                success: false,
                error: Some(e.to_string()),
                reserve_info: None,
            })
        }
    }
}

/// Validation sonucu
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub success: bool,
    pub error: Option<String>,
    pub reserve_info: Option<ReserveInfo>,
}

/// Reserve bilgileri
#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub version: u8,
    pub lending_market: Pubkey,
    pub liquidity_mint: Pubkey,
    pub collateral_mint: Pubkey,
    pub ltv: f64,
    pub liquidation_bonus: f64,
}

/// Bilinen Solend mainnet reserve account'ları
///
/// NOT:
/// - Bu adresler, Solend ekosistemiyle ilgili herkese açık dokümantasyon ve
///   topluluk kaynaklarından derlenen **varsayılan** adreslerdir.
/// - Production'da kullanmadan önce mutlaka Solend'in resmi kaynakları
///   (SDK, dashboard, docs) üzerinden doğrulanmalıdır.
/// - Amaç: Geliştirme ve hızlı validation için out-of-the-box bir başlangıç
///   sağlamak; tek otorite **Solend**'dir.
pub mod known_reserves {
    use solana_sdk::pubkey::Pubkey;
    
    // Bu fonksiyonlar **best-effort** default adresler döndürür.
    // Eğer Solend tarafında bir güncelleme olursa, bu adreslerin de
    // güncellenmesi gerekir. Bu yüzden:
    //
    // 1. Solend SDK kullan: https://sdk.solend.fi
    // 2. Solend'in resmi dokümantasyonunu kontrol et
    // 3. Reserve adreslerini config / env ile override etmeyi tercih et
    
    /// USDC Reserve (main market, mainnet)
    ///
    /// Kaynak: Solend-related public docs / community references.
    pub fn usdc_reserve() -> anyhow::Result<Pubkey> {
        // Varsayılan USDC reserve adresi (mainnet)
        let addr = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
        addr.parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("Invalid hardcoded USDC reserve address {}: {}", addr, e))
    }
    
    /// SOL Reserve (main market, mainnet)
    ///
    /// Kaynak: Solend-related public docs / community references.
    pub fn sol_reserve() -> anyhow::Result<Pubkey> {
        // Varsayılan SOL reserve adresi (mainnet)
        let addr = "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36";
        addr.parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("Invalid hardcoded SOL reserve address {}: {}", addr, e))
    }
}

