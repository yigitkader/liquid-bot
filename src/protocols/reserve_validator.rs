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
    
    // Validate account size matches expected Reserve size
    // Official SDK: RESERVE_SIZE = ReserveLayout.span (typically 619 bytes)
    const EXPECTED_RESERVE_SIZE: usize = 619;
    if account.data.len() != EXPECTED_RESERVE_SIZE {
        log::warn!(
            "⚠️  Account size mismatch: {} bytes (expected {} bytes). This might indicate a different Reserve version or structure.",
            account.data.len(),
            EXPECTED_RESERVE_SIZE
        );
    }
    
    // Parse et
    match SolendReserve::from_account_data(&account.data) {
        Ok(reserve) => {
            log::info!("✅ Reserve account parsed successfully!");
            
            // Validate critical fields are not default/empty
            let pyth_oracle = reserve.pyth_oracle();
            let switchboard_oracle = reserve.switchboard_oracle();
            
            // Additional validation: Check that critical fields are valid
            if reserve.lending_market == Pubkey::default() {
                log::warn!("⚠️  Warning: lending_market is default/empty - this might indicate parsing error");
            }
            if reserve.liquidity_mint() == Pubkey::default() {
                log::warn!("⚠️  Warning: liquidity_mint is default/empty - this might indicate parsing error");
            }
            if reserve.collateral_mint() == Pubkey::default() {
                log::warn!("⚠️  Warning: collateral_mint is default/empty - this might indicate parsing error");
            }
            
            log::debug!(
                "Reserve details: version={}, lending_market={}, liquidity_mint={}, collateral_mint={}, pyth_oracle={}, switchboard_oracle={}",
                reserve.version,
                reserve.lending_market,
                reserve.liquidity_mint(),
                reserve.collateral_mint(),
                pyth_oracle,
                switchboard_oracle
            );
            
            // Validate LTV and liquidation bonus are in reasonable ranges
            let ltv = reserve.ltv();
            let liquidation_bonus = reserve.liquidation_bonus();
            if ltv < 0.0 || ltv > 1.0 {
                log::warn!("⚠️  Warning: LTV out of expected range [0.0, 1.0]: {}", ltv);
            }
            if liquidation_bonus < 0.0 || liquidation_bonus > 1.0 {
                log::warn!("⚠️  Warning: Liquidation bonus out of expected range [0.0, 1.0]: {}", liquidation_bonus);
            }
            
            Ok(ValidationResult {
                success: true,
                error: None,
                reserve_info: Some(ReserveInfo {
                    version: reserve.version,
                    lending_market: reserve.lending_market,
                    liquidity_mint: reserve.liquidity_mint(),
                    collateral_mint: reserve.collateral_mint(),
                    ltv,
                    liquidation_bonus,
                    pyth_oracle: if pyth_oracle != Pubkey::default() { Some(pyth_oracle) } else { None },
                    switchboard_oracle: if switchboard_oracle != Pubkey::default() { Some(switchboard_oracle) } else { None },
                }),
            })
        }
        Err(e) => {
            log::error!("❌ Failed to parse reserve account: {}", e);
            log::error!("   This indicates the struct structure doesn't match the real Solend IDL");
            log::error!("   Please update src/protocols/solend_reserve.rs with the correct structure");
            
            // Hex dump ilk 200 byte'ı göster (debug için)
            let hex_dump: String = account.data.iter()
                .take(200)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::info!("Account data size: {} bytes (expected: {} bytes)", account.data.len(), EXPECTED_RESERVE_SIZE);
            log::info!("First 200 bytes (hex): {}", hex_dump);
            
            // Account owner'ı kontrol et
            log::info!("Account owner: {}", account.owner);
            
            // Field offset analysis for debugging
            log::info!("Field offset analysis (for debugging):");
            log::info!("  Offset 0: version (u8)");
            log::info!("  Offset 1-8: lastUpdate.slot (u64)");
            log::info!("  Offset 9: lastUpdate.stale (u8)");
            log::info!("  Offset 10-41: lendingMarket (Pubkey, 32 bytes)");
            log::info!("  Offset 42-73: liquidity.mintPubkey (Pubkey, 32 bytes)");
            log::info!("  Offset 74: liquidity.mintDecimals (u8)");
            log::info!("  Offset 75-106: liquidity.supplyPubkey (Pubkey, 32 bytes)");
            log::info!("  Offset 107-138: liquidity.pythOracle (Pubkey, 32 bytes)");
            log::info!("  Offset 139-170: liquidity.switchboardOracle (Pubkey, 32 bytes)");
            log::info!("  ... (see official SDK for complete layout)");
            log::info!("");
            log::info!("Reference: https://github.com/solendprotocol/solend-sdk/blob/master/src/state/reserve.ts");
            
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
    pub pyth_oracle: Option<Pubkey>,
    pub switchboard_oracle: Option<Pubkey>,
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

