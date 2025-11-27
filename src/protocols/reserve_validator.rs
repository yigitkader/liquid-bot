use crate::protocols::solend_reserve::SolendReserve;
use crate::solana_client::SolanaClient;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

pub async fn validate_reserve_structure(
    rpc_client: Arc<SolanaClient>,
    reserve_pubkey: &Pubkey,
) -> Result<ValidationResult> {
    log::info!("Validating reserve account structure: {}", reserve_pubkey);

    let account = rpc_client
        .get_account(reserve_pubkey)
        .await
        .context("Failed to fetch reserve account")?;

    if account.data.is_empty() {
        return Err(anyhow::anyhow!("Reserve account data is empty"));
    }

    log::debug!("Reserve account data size: {} bytes", account.data.len());

    const EXPECTED_RESERVE_SIZE: usize = 619;
    if account.data.len() != EXPECTED_RESERVE_SIZE {
        log::warn!(
            "⚠️  Account size mismatch: {} bytes (expected {} bytes). This might indicate a different Reserve version or structure.",
            account.data.len(),
            EXPECTED_RESERVE_SIZE
        );
    }

    match SolendReserve::from_account_data(&account.data) {
        Ok(reserve) => {
            log::info!("✅ Reserve account parsed successfully!");

            let pyth_oracle_raw = reserve.pyth_oracle();
            let switchboard_oracle_raw = reserve.switchboard_oracle();
            
            // Default pubkey olmayan oracle'ı aktif kabul et
            let pyth_oracle = if pyth_oracle_raw != Pubkey::default() {
                Some(pyth_oracle_raw)
            } else {
                None
            };
            let switchboard_oracle = if switchboard_oracle_raw != Pubkey::default() {
                Some(switchboard_oracle_raw)
            } else {
                None
            };

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
                "Reserve details: version={}, lending_market={}, liquidity_mint={}, collateral_mint={}, pyth_oracle={:?}, switchboard_oracle={:?}",
                reserve.version,
                reserve.lending_market,
                reserve.liquidity_mint(),
                reserve.collateral_mint(),
                pyth_oracle,
                switchboard_oracle
            );

            let ltv = reserve.ltv();
            let liquidation_bonus = reserve.liquidation_bonus();
            if ltv < 0.0 || ltv > 1.0 {
                log::warn!("⚠️  Warning: LTV out of expected range [0.0, 1.0]: {}", ltv);
            }
            if liquidation_bonus < 0.0 || liquidation_bonus > 1.0 {
                log::warn!(
                    "⚠️  Warning: Liquidation bonus out of expected range [0.0, 1.0]: {}",
                    liquidation_bonus
                );
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
                    oracle_option: 0, // Solend'in gerçek kodunda oracle_option YOK - placeholder
                    pyth_oracle,
                    switchboard_oracle,
                }),
            })
        }
        Err(e) => {
            log::error!("❌ Failed to parse reserve account: {}", e);
            log::error!("   This indicates the struct structure doesn't match the real Solend IDL");
            log::error!(
                "   Please update src/protocols/solend_reserve.rs with the correct structure"
            );

            // Hex dump ilk 200 byte'ı göster (debug için)
            let hex_dump: String = account
                .data
                .iter()
                .take(200)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::info!(
                "Account data size: {} bytes (expected: {} bytes)",
                account.data.len(),
                EXPECTED_RESERVE_SIZE
            );
            log::info!("First 200 bytes (hex): {}", hex_dump);
            
            log::info!("Account owner: {}", account.owner);
            
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
            log::info!("  Note: oracleOption field does NOT exist in Solend's real code!");
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

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub success: bool,
    pub error: Option<String>,
    pub reserve_info: Option<ReserveInfo>,
}

#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub version: u8,
    pub lending_market: Pubkey,
    pub liquidity_mint: Pubkey,
    pub collateral_mint: Pubkey,
    pub ltv: f64,
    pub liquidation_bonus: f64,
    pub oracle_option: u32,
    pub pyth_oracle: Option<Pubkey>,
    pub switchboard_oracle: Option<Pubkey>,
}

pub mod known_reserves {
    use solana_sdk::pubkey::Pubkey;

    /// Get USDC reserve address from config or fallback to default mainnet address
    pub fn usdc_reserve(config: Option<&crate::config::Config>) -> anyhow::Result<Pubkey> {
        let addr = config
            .and_then(|c| c.usdc_reserve_address.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw"); // Default mainnet USDC reserve
        
        addr.parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("Invalid USDC reserve address {}: {}", addr, e))
    }
    
    /// Get SOL reserve address from config or fallback to default mainnet address
    pub fn sol_reserve(config: Option<&crate::config::Config>) -> anyhow::Result<Pubkey> {
        let addr = config
            .and_then(|c| c.sol_reserve_address.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36"); // Default mainnet SOL reserve
        
        addr.parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("Invalid SOL reserve address {}: {}", addr, e))
    }
    
    /// Legacy function for backward compatibility (uses default addresses)
    #[deprecated(note = "Use usdc_reserve(Some(&config)) instead")]
    pub fn usdc_reserve_legacy() -> anyhow::Result<Pubkey> {
        usdc_reserve(None)
    }
    
    /// Legacy function for backward compatibility (uses default addresses)
    #[deprecated(note = "Use sol_reserve(Some(&config)) instead")]
    pub fn sol_reserve_legacy() -> anyhow::Result<Pubkey> {
        sol_reserve(None)
    }
}
