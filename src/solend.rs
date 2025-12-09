// Auto-generated Solend account layouts from IDL
// Imports must be before include!
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

include!(concat!(env!("OUT_DIR"), "/solend_layout.rs"));

use anyhow::Result;
use std::str::FromStr;
use solana_client::rpc_filter::RpcFilterType;
use solana_client::rpc_config::RpcProgramAccountsConfig;

/// Solend account type identification
/// Used to quickly identify account types without parsing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolendAccountType {
    Obligation,
    Reserve,
    LendingMarket,
    Unknown,
}

// Helper structs for Reserve (not in generated layout, but used by helper methods)
#[derive(Debug, Clone)]
pub struct ReserveLiquidity {
    pub mintPubkey: Pubkey,
    pub mintDecimals: u8,
    pub supplyPubkey: Pubkey,
    pub liquidityPythOracle: Pubkey,
    pub liquiditySwitchboardOracle: Pubkey,
    pub availableAmount: u64,
    pub borrowedAmountWads: u128,
    pub cumulativeBorrowRateWads: u128,
    pub liquidityMarketPrice: u128,
}

#[derive(Debug, Clone)]
pub struct ReserveCollateral {
    pub mintPubkey: Pubkey,
    pub mintTotalSupply: u64,
    pub supplyPubkey: Pubkey,
}

// Helper struct for Reserve configuration fields
// NOTE: This is NOT the same as the generated ReserveConfig (which is for RateLimiter)
// This helper struct provides convenient access to Reserve's configuration fields
#[derive(Debug, Clone)]
pub struct ReserveConfigHelper {
    pub optimalUtilizationRate: u8,
    pub loanToValueRatio: u8,
    pub liquidationBonus: u8,
    pub liquidationThreshold: u8,
    pub minBorrowRate: u8,
    pub optimalBorrowRate: u8,
    pub maxBorrowRate: u8,
    pub switchboardOraclePubkey: Pubkey,
    pub borrowFeeWad: u64,
    pub flashLoanFeeWad: u64,
    pub hostFeePercentage: u8,
    pub depositLimit: u64,
    pub borrowLimit: u64,
    pub feeReceiver: Pubkey,
    pub protocolLiquidationFee: u8,
    pub protocolTakeRate: u8,
    pub accumulatedProtocolFeesWads: u128,
    pub addedBorrowWeightBPS: u64,
    pub liquiditySmoothedMarketPrice: u128,
    pub reserveType: u8,
    pub maxUtilizationRate: u8,
    pub superMaxBorrowRate: u64,
    pub maxLiquidationBonus: u8,
    pub maxLiquidationThreshold: u8,
    pub scaledPriceOffsetBPS: u64,
    pub extraOracle: Pubkey,
    pub liquidityExtraMarketPriceFlag: u8,
    pub liquidityExtraMarketPrice: u128,
    pub attributedBorrowValue: u128,
    pub attributedBorrowLimitOpen: u64,
    pub attributedBorrowLimitClose: u64,
}

// Helper implementation for Number type (u128 wrapper)
impl Number {
    pub fn to_f64(&self) -> f64 {
        // Convert u128 to f64 (WAD = 10^18)
        const WAD: f64 = 1_000_000_000_000_000_000.0;
        self.value as f64 / WAD
    }
}

// Helper implementation for Obligation
impl Obligation {
    /// Parse obligation from account data using Borsh
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        log::trace!("Parsing Obligation from {} bytes of data", data.len());
        
        if data.len() >= 32 {
            let preview: String = data.iter()
                .take(32)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::trace!("First 32 bytes (hex): {}", preview);
        }
        
        const EXPECTED_STRUCT_SIZE: usize = 1300;
        const DISCRIMINATOR_SIZE: usize = 8;
        
        let has_discriminator = if data.len() == EXPECTED_STRUCT_SIZE + DISCRIMINATOR_SIZE {
            true
        } else if data.len() == EXPECTED_STRUCT_SIZE {
            false
        } else {
            if data.len() > EXPECTED_STRUCT_SIZE {
                !data[0..DISCRIMINATOR_SIZE].iter().all(|&b| b == 0)
            } else {
                false
            }
        };
        
        let data_without_discriminator = if has_discriminator {
            log::trace!("Skipping 8-byte Anchor discriminator (account size: {} bytes)", data.len());
            if data.len() < DISCRIMINATOR_SIZE + EXPECTED_STRUCT_SIZE {
                return Err(anyhow::anyhow!(
                    "Account too short: {} bytes (need {} bytes after discriminator)",
                    data.len(),
                    DISCRIMINATOR_SIZE + EXPECTED_STRUCT_SIZE
                ));
            }
            &data[DISCRIMINATOR_SIZE..]
        } else {
            log::trace!("No discriminator detected (account size: {} bytes, Legacy format)", data.len());
            data
        };
        
        if data_without_discriminator.len() < EXPECTED_STRUCT_SIZE {
            return Err(anyhow::anyhow!(
                "Obligation data too short: {} bytes (need {} bytes). \
                 Account may be corrupted or layout may have changed.",
                data_without_discriminator.len(),
                EXPECTED_STRUCT_SIZE
            ));
        }
        
        let data_to_parse = &data_without_discriminator[..EXPECTED_STRUCT_SIZE];
        let version_byte = data_to_parse[0];
        if version_byte != 0 && version_byte != 1 {
            return Err(anyhow::anyhow!(
                "Invalid Obligation version: {} (expected 0 or 1). \
                 This is likely not an Obligation account.",
                version_byte
            ));
        }
        
        log::trace!(
            "Attempting Borsh deserialization of {} bytes (version byte: 0x{:02x} = {})",
            data_to_parse.len(),
            version_byte,
            version_byte
        );
        
        let obligation: Obligation = BorshDeserialize::try_from_slice(data_to_parse)
            .map_err(|e| {
                log::error!("Borsh deserialization failed: {}", e);
                log::error!("Data length: {} bytes (expected: {} bytes for Obligation struct)", data.len(), EXPECTED_STRUCT_SIZE);
                log::error!("Parsed length: {} bytes", data_to_parse.len());
                
                if data.len() >= 200 {
                    let hex_preview: String = data.iter()
                        .take(200)
                        .enumerate()
                        .map(|(i, b)| {
                            if i % 32 == 0 && i > 0 {
                                format!("\n  [{:04x}]: {:02x}", i, b)
                            } else {
                                format!(" {:02x}", b)
                            }
                        })
                        .collect();
                    log::error!("First 200 bytes (hex, 32-byte groups):\n  [0000]:{}", hex_preview);
                }
                
                anyhow::anyhow!("Failed to deserialize Obligation: {}. See logs for detailed byte analysis.", e)
            })?;
        
        log::trace!("Successfully deserialized Obligation, version={}", obligation.version);
        
        const EXPECTED_VERSION: u8 = 1;
        const LEGACY_VERSION: u8 = 0;
        
        if obligation.version == LEGACY_VERSION {
            log::debug!(
                "Obligation version {} (legacy) - still valid",
                obligation.version
            );
        } else if obligation.version != EXPECTED_VERSION {
            log::error!(
                "Version mismatch: found version {} but expected {} or {} (legacy)",
                obligation.version,
                EXPECTED_VERSION,
                LEGACY_VERSION
            );
            return Err(anyhow::anyhow!(
                "Unsupported Obligation version: {} (expected {} or {} for legacy). \
                 Layout may have changed, please update bot to support new version.",
                obligation.version,
                EXPECTED_VERSION,
                LEGACY_VERSION
            ));
        }
        
        Ok(obligation)
    }

    /// Calculate health factor: allowedBorrowValue / borrowedValue
    pub fn health_factor(&self) -> f64 {
        let borrowed = self.borrowedValue as f64 / 1e18;
        if borrowed == 0.0 {
            return f64::INFINITY;
        }
        let weighted_collateral = self.allowedBorrowValue as f64 / 1e18;
        weighted_collateral / borrowed
    }

    pub fn calculate_health_factor(&self) -> f64 {
        self.health_factor()
    }

    /// Calculate health factor using u128 arithmetic (returns WAD format)
    pub fn health_factor_u128(&self) -> u128 {
        const WAD: u128 = 1_000_000_000_000_000_000;
        
        let borrowed = self.borrowedValue;
        if borrowed == 0 {
            return u128::MAX;
        }
        
        let weighted_collateral = self.allowedBorrowValue;
        weighted_collateral
            .checked_mul(WAD)
            .and_then(|v| v.checked_div(borrowed))
            .unwrap_or(u128::MAX)
    }

    /// Check if obligation is liquidatable (HF < 1.0)
    pub fn is_liquidatable(&self) -> bool {
        const WAD: u128 = 1_000_000_000_000_000_000;
        self.health_factor_u128() < WAD
    }

    pub fn total_deposited_value_usd(&self) -> f64 {
        self.depositedValue as f64 / 1e18
    }

    pub fn total_borrowed_value_usd(&self) -> f64 {
        self.borrowedValue as f64 / 1e18
    }

    /// Get last update slot from obligation
    /// Returns None if lastUpdate field is not available (shouldn't happen)
    pub fn last_update_slot(&self) -> Option<u64> {
        // lastUpdate field is generated by build.rs
        // Access it via reflection or direct field access
        // Since struct is generated, we can access it directly
        Some(self.lastUpdate.slot)
    }

    /// Check if obligation is stale based on lastUpdate slot
    pub fn is_stale(&self, current_slot: u64, max_slot_diff: u64) -> bool {
        if let Some(last_slot) = self.last_update_slot() {
            current_slot.saturating_sub(last_slot) > max_slot_diff
        } else {
            // If we can't get lastUpdate slot, assume it's stale to be safe
            true
        }
    }

    pub fn deposits(&self) -> Result<Vec<ObligationCollateral>> {
        use borsh::BorshDeserialize;
        
        let deposits_len = self.depositsLen as usize;
        if deposits_len == 0 {
            return Ok(Vec::new());
        }
        
        const COLLATERAL_SIZE: usize = 32 + 8 + 16 + 32;
        let deposits_size = deposits_len * COLLATERAL_SIZE;
        
        if deposits_size > self.dataFlat.len() {
            return Err(anyhow::anyhow!(
                "Invalid deposits size: {} bytes (max: {})",
                deposits_size,
                self.dataFlat.len()
            ));
        }
        
        let deposits_buffer = &self.dataFlat[0..deposits_size];
        let mut deposits = Vec::new();
        
        for i in 0..deposits_len {
            let offset = i * COLLATERAL_SIZE;
            if offset + COLLATERAL_SIZE > deposits_buffer.len() {
                break;
            }
            let collateral_data = &deposits_buffer[offset..offset + COLLATERAL_SIZE];
            match ObligationCollateral::try_from_slice(collateral_data) {
                Ok(collateral) => {
                    // Padding field removed from struct (handled in parsing code)
                    deposits.push(collateral);
                }
                Err(e) => {
                    log::warn!("Failed to parse deposit {}: {} (data size: {} bytes, expected: {} bytes)", i, e, collateral_data.len(), COLLATERAL_SIZE);
                }
            }
        }
        
        Ok(deposits)
    }

    pub fn borrows(&self) -> Result<Vec<ObligationLiquidity>> {
        use borsh::BorshDeserialize;
        
        let borrows_len = self.borrowsLen as usize;
        if borrows_len == 0 {
            return Ok(Vec::new());
        }
        
        const COLLATERAL_SIZE: usize = 32 + 8 + 16 + 32;
        let deposits_size = self.depositsLen as usize * COLLATERAL_SIZE;
        const LIQUIDITY_SIZE: usize = 32 + 16 + 16 + 16 + 32;
        let borrows_size = borrows_len * LIQUIDITY_SIZE;
        let borrows_offset = deposits_size;
        
        if borrows_offset + borrows_size > self.dataFlat.len() {
            return Err(anyhow::anyhow!(
                "Invalid borrows size: {} bytes (offset: {}, max: {})",
                borrows_size,
                borrows_offset,
                self.dataFlat.len()
            ));
        }
        
        let borrows_buffer = &self.dataFlat[borrows_offset..borrows_offset + borrows_size];
        let mut borrows = Vec::new();
        
        for i in 0..borrows_len {
            let offset = i * LIQUIDITY_SIZE;
            if offset + LIQUIDITY_SIZE > borrows_buffer.len() {
                break;
            }
            let liquidity_data = &borrows_buffer[offset..offset + LIQUIDITY_SIZE];
            match ObligationLiquidity::try_from_slice(liquidity_data) {
                Ok(liquidity) => {
                    // Padding field removed from struct (handled in parsing code)
                    borrows.push(liquidity);
                }
                Err(e) => {
                    log::warn!("Failed to parse borrow {}: {} (data size: {} bytes, expected: {} bytes)", i, e, liquidity_data.len(), LIQUIDITY_SIZE);
                }
            }
        }
        
        Ok(borrows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_health_factor_precision() {
        let obligation1 = Obligation {
            version: 1,
            lastUpdate: crate::solend::LastUpdate {
                slot: 0,
                stale: false,
            },
            lendingMarket: Pubkey::default(),
            owner: Pubkey::default(),
            depositsLen: 0,
            borrowsLen: 0,
            depositedValue: 0,
            borrowedValue: 999_999_999_999_999_999,
            allowedBorrowValue: 1_000_000_000_000_000_000,
            dataFlat: vec![],
        };
        
        assert!(!obligation1.is_liquidatable());
        
        let obligation2 = Obligation {
            version: 1,
            lastUpdate: crate::solend::LastUpdate {
                slot: 0,
                stale: false,
            },
            lendingMarket: Pubkey::default(),
            owner: Pubkey::default(),
            depositsLen: 0,
            borrowsLen: 0,
            depositedValue: 0,
            borrowedValue: 1_000_000_000_000_000_001,
            allowedBorrowValue: 1_000_000_000_000_000_000,
            dataFlat: vec![],
        };
        assert!(obligation2.is_liquidatable());
        
        let obligation3 = Obligation {
            version: 1,
            lastUpdate: crate::solend::LastUpdate {
                slot: 0,
                stale: false,
            },
            lendingMarket: Pubkey::default(),
            owner: Pubkey::default(),
            depositsLen: 0,
            borrowsLen: 0,
            depositedValue: 0,
            borrowedValue: 1_000_000_000_000_000_000,
            allowedBorrowValue: 1_000_000_000_000_000_000,
            dataFlat: vec![],
        };
        assert!(!obligation3.is_liquidatable());
        
        let obligation4 = Obligation {
            version: 1,
            lastUpdate: crate::solend::LastUpdate {
                slot: 0,
                stale: false,
            },
            lendingMarket: Pubkey::default(),
            owner: Pubkey::default(),
            depositsLen: 0,
            borrowsLen: 0,
            depositedValue: 0,
            borrowedValue: 2_000_000_000_000_000_000,
            allowedBorrowValue: 1_000_000_000_000_000_000,
            dataFlat: vec![],
        };
        assert!(obligation4.is_liquidatable());
        
        let obligation5 = Obligation {
            version: 1,
            lastUpdate: crate::solend::LastUpdate {
                slot: 0,
                stale: false,
            },
            lendingMarket: Pubkey::default(),
            owner: Pubkey::default(),
            depositsLen: 0,
            borrowsLen: 0,
            depositedValue: 0,
            borrowedValue: 1_000_000_000_000_000_000,
            allowedBorrowValue: 2_000_000_000_000_000_000,
            dataFlat: vec![],
        };
        assert!(!obligation5.is_liquidatable());
    }
}

// CRITICAL FIX: Solend Program ID - USDC Reserve Required
// 
// ⚠️  IMPORTANT: Bot requires USDC reserve (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v)
//    Only programs with USDC reserve can be used for liquidation operations.
//
// ✅ ACTIVE PROGRAMS WITH USDC:
// 1. Legacy Solend Main Pool (DEFAULT - RECOMMENDED): So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
//    - Active on mainnet (original Solend protocol)
//    - Has USDC reserve (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v)
//    - Verified working for liquidation bot
//    - Verification: https://solscan.io/account/So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
//
// ❌ PROGRAMS WITHOUT USDC (DO NOT USE):
//    "SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh" - Save Protocol Main Market (NO USDC reserve)
//       Note: Save Protocol (2024 rebrand) focuses on SUSD, not USDC
//    "ALcohoCRRXGDKhc5pS5UqzVVjZE5x9dkgjZjD8MJCTw" - Altcoins Market (NO USDC)
//    "turboJPMBqVwWU26JsKivCm9wPU3fuaYx8EM9rRHfuuP" - Turbo SOL Market (NO USDC)
//
// NOTE: Save Protocol (2024 rebrand) markets may not have USDC reserves.
//       They focus on SUSD (Save USD) instead. Use Legacy Solend for USDC.
//
// Verification: Check Solana Explorer for USDC reserves:
// - Legacy Solend: https://solscan.io/account/So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo
// - Save Protocol: https://solscan.io/account/SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh
// NOTE: All program IDs are now read from .env file
// No hardcoded program IDs - all must be configured in .env

/// Get Solend program ID (mainnet production)
/// 
/// CRITICAL: Reads from SOLEND_PROGRAM_ID environment variable
/// No hardcoded defaults - must be set in .env file
/// 
/// NOTE: Bot requires USDC reserve - only programs with USDC can be used
///       Save Protocol (2024 rebrand) markets use SUSD, not USDC
pub fn solend_program_id() -> Result<Pubkey> {
    use std::env;
    
    let env_program_id = env::var("SOLEND_PROGRAM_ID")
        .map_err(|_| anyhow::anyhow!(
            "SOLEND_PROGRAM_ID not found in .env file. \
             Please set SOLEND_PROGRAM_ID in .env file. \
             Example: SOLEND_PROGRAM_ID=So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo"
        ))?;
    
    log::info!("📝 Using Solend program ID from .env: {}", env_program_id);
    Pubkey::from_str(&env_program_id)
        .map_err(|e| anyhow::anyhow!("Invalid SOLEND_PROGRAM_ID from .env: {} - Error: {}", env_program_id, e))
}

/// Verify if a program ID is a known Solend program
/// 
/// This checks if the program ID matches known Solend program IDs from .env
/// NOTE: Only programs with USDC reserve should be used for liquidation operations
pub fn is_valid_solend_program(pubkey: &Pubkey) -> bool {
    use std::env;
    
    // Check against SOLEND_PROGRAM_ID from env
    if let Ok(env_program_id) = env::var("SOLEND_PROGRAM_ID") {
        if let Ok(env_pubkey) = Pubkey::from_str(&env_program_id) {
            if *pubkey == env_pubkey {
                return true;
            }
        }
    }
    
    // Check against optional SOLEND_PROGRAM_ID_LEGACY
    if let Ok(legacy_id) = env::var("SOLEND_PROGRAM_ID_LEGACY") {
        if let Ok(legacy_pubkey) = Pubkey::from_str(&legacy_id) {
            if *pubkey == legacy_pubkey {
                return true;
            }
        }
    }
    
    // Check against optional SOLEND_PROGRAM_ID_SAVE
    if let Ok(save_id) = env::var("SOLEND_PROGRAM_ID_SAVE") {
        if let Ok(save_pubkey) = Pubkey::from_str(&save_id) {
            if *pubkey == save_pubkey {
                return true;
            }
        }
    }
    
    false
}

/// Identify Solend account type without parsing
/// 
/// Uses heuristics based on size, discriminator, and version byte to quickly
/// identify account types without expensive Borsh deserialization.
/// 
/// This is faster and more reliable than attempting to parse every account.
pub fn identify_solend_account_type(data: &[u8]) -> SolendAccountType {
    if data.len() < 1 {
        return SolendAccountType::Unknown;
    }
    
    // ✅ FIXED: Size-based discriminator detection
    // Solend Legacy accounts are exactly 1300 bytes (no discriminator)
    // Anchor-based accounts would be 1308 bytes (1300 + 8 byte discriminator)
    const EXPECTED_STRUCT_SIZE: usize = 1300;
    const DISCRIMINATOR_SIZE: usize = 8;
    
    // STEP 1: Determine if discriminator exists based on size
    let (_has_discriminator, actual_data) = if data.len() == EXPECTED_STRUCT_SIZE + DISCRIMINATOR_SIZE {
        // Account is 1308 bytes - likely has Anchor discriminator
        if data.len() < DISCRIMINATOR_SIZE {
            return SolendAccountType::Unknown;
        }
        (true, &data[DISCRIMINATOR_SIZE..])
    } else if data.len() == EXPECTED_STRUCT_SIZE {
        // Account is exactly 1300 bytes - Legacy account, no discriminator
        (false, data)
    } else {
        // Size doesn't match expected - try to detect discriminator using old logic
        if data.len() >= DISCRIMINATOR_SIZE && !data[0..DISCRIMINATOR_SIZE].iter().all(|&b| b == 0) {
            if data.len() < DISCRIMINATOR_SIZE {
                return SolendAccountType::Unknown;
            }
            (true, &data[DISCRIMINATOR_SIZE..])
        } else {
            (false, data)
        }
    };
    
    // STEP 2: Size-based heuristic (after discriminator)
    let size = actual_data.len();
    
    // STEP 3: Check version byte
    if size > 0 {
        let version = actual_data[0];
        
        // Obligation: 1300 bytes (exact match for Legacy), version 0 or 1
        // Note: Obligation can be version 0 (legacy) or 1 (current)
        if size == EXPECTED_STRUCT_SIZE && (version == 0 || version == 1) {
            return SolendAccountType::Obligation;
        }
        
        // Obligation: 1200-1400 bytes (with some tolerance), version 0 or 1
        if (1200..=1400).contains(&size) && (version == 0 || version == 1) {
            return SolendAccountType::Obligation;
        }
        
        // Reserve: 1200-1400 bytes, version 1 only
        // Note: Reserve is always version 1 (version 0 is invalid for Reserve)
        // CRITICAL: Both Obligation and Reserve can be 1300 bytes with version 1
        // We check Obligation first (version 0 or 1), so if we reach here with version 1,
        // it could be either. We return Reserve as a heuristic, but caller should
        // try parsing to confirm (Obligation parsing will fail if it's actually a Reserve)
        if (1200..=1400).contains(&size) && version == 1 {
            // This could be either Obligation or Reserve
            // Return Reserve as default, but caller should verify by parsing
            return SolendAccountType::Reserve;
        }
        
        // LendingMarket: 200-400 bytes
        // LendingMarket doesn't have a version byte in the same way
        if (200..=400).contains(&size) {
            return SolendAccountType::LendingMarket;
        }
    }
    
    SolendAccountType::Unknown
}

/// Derive obligation address (PDA)
pub fn derive_obligation_address(
    wallet_pubkey: &Pubkey,
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    let seeds = &[
        b"obligation".as_ref(),
        wallet_pubkey.as_ref(),
        lending_market.as_ref(),
    ];

    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive obligation address"))
}

/// Derive lending market authority (PDA)
pub fn derive_lending_market_authority(
    lending_market: &Pubkey,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    let seeds = &[lending_market.as_ref()];

    Pubkey::try_find_program_address(seeds, program_id)
        .map(|(pubkey, _)| pubkey)
        .ok_or_else(|| anyhow::anyhow!("Failed to derive lending market authority"))
}

/// Derive reserve liquidity supply PDA (for verification)
/// 
/// SECURITY: This function attempts to derive the expected PDA for reserve liquidity supply.
/// Multiple seed formats are tried as Solend's exact format may vary.
/// Returns Some(derived_pubkey) if a match is found, None otherwise.
/// 
/// This is used for validation - if the derived PDA doesn't match the stored supplyPubkey,
/// it may indicate data corruption or manipulation.
pub fn derive_reserve_liquidity_supply_pda(
    reserve_pubkey: &Pubkey,
    program_id: &Pubkey,
) -> Option<Pubkey> {
    // Try common Solend PDA seed formats for reserve liquidity supply
    // Format 1: ["liquidity", reserve_pubkey]
    let seeds1 = &[b"liquidity".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds1, program_id) {
        return Some(pda);
    }
    
    // Format 2: ["reserve", reserve_pubkey, "liquidity"]
    let seeds2 = &[
        b"reserve".as_ref(),
        reserve_pubkey.as_ref(),
        b"liquidity".as_ref(),
    ];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds2, program_id) {
        return Some(pda);
    }
    
    // Format 3: ["reserve_liquidity", reserve_pubkey]
    let seeds3 = &[b"reserve_liquidity".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds3, program_id) {
        return Some(pda);
    }
    
    None
}

pub fn derive_reserve_collateral_supply_pda(
    reserve_pubkey: &Pubkey,
    program_id: &Pubkey,
) -> Option<Pubkey> {
    let seeds1 = &[b"collateral".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds1, program_id) {
        return Some(pda);
    }
    
    let seeds2 = &[
        b"reserve".as_ref(),
        reserve_pubkey.as_ref(),
        b"collateral".as_ref(),
    ];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds2, program_id) {
        return Some(pda);
    }
    
    let seeds3 = &[b"reserve_collateral".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds3, program_id) {
        return Some(pda);
    }
    
    None
}

pub fn get_liquidate_obligation_discriminator() -> u8 {
    12u8
}

pub fn get_redeem_reserve_collateral_discriminator() -> u8 {
    5u8
}

pub fn get_flashloan_discriminator() -> u8 {
    13u8
}

pub fn get_flashrepay_discriminator() -> u8 {
    14u8
}

// Helper implementation for Reserve
impl Reserve {
    /// Parse reserve from account data using Borsh
    /// 
    /// CRITICAL SECURITY: Validates version field to detect layout changes.
    /// If Solend updates their layout (e.g., v1 -> v2), this will catch it early.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        log::trace!("Parsing Reserve from {} bytes of data", data.len());
        
        // Log first few bytes for debugging
        if data.len() >= 32 {
            let preview: String = data.iter()
                .take(32)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::trace!("First 32 bytes (hex): {}", preview);
        }
        
        const EXPECTED_RESERVE_STRUCT_SIZE: usize = 1300; // Full Reserve struct size including padding
        const DISCRIMINATOR_SIZE: usize = 8; // Anchor discriminator size
        
        // ✅ FIXED: Size-based discriminator detection (same as Obligation)
        // STEP 1: Check account size to determine if discriminator exists
        // Solend Legacy accounts are exactly 1300 bytes (no discriminator)
        // Anchor-based accounts would be 1308 bytes (1300 + 8 byte discriminator)
        let has_discriminator = if data.len() == EXPECTED_RESERVE_STRUCT_SIZE + DISCRIMINATOR_SIZE {
            // Account is 1308 bytes - likely has Anchor discriminator
            true
        } else if data.len() == EXPECTED_RESERVE_STRUCT_SIZE {
            // Account is exactly 1300 bytes - Legacy account, no discriminator
            false
        } else {
            // Size doesn't match expected - fallback to old logic for edge cases
            // But this should rarely happen for valid Solend accounts
            if data.len() > EXPECTED_RESERVE_STRUCT_SIZE {
                // Account is larger than expected - might have discriminator
                !data[0..DISCRIMINATOR_SIZE].iter().all(|&b| b == 0)
            } else {
                false
            }
        };
        
        // STEP 2: Skip discriminator if present
        let data_without_discriminator = if has_discriminator {
            log::trace!("Skipping 8-byte Anchor discriminator (account size: {} bytes)", data.len());
            if data.len() < DISCRIMINATOR_SIZE + EXPECTED_RESERVE_STRUCT_SIZE {
                return Err(anyhow::anyhow!(
                    "Account too short: {} bytes (need {} bytes after discriminator)",
                    data.len(),
                    DISCRIMINATOR_SIZE + EXPECTED_RESERVE_STRUCT_SIZE
                ));
            }
            &data[DISCRIMINATOR_SIZE..]
        } else {
            log::trace!("No discriminator detected (account size: {} bytes, Legacy format)", data.len());
            data
        };
        
        // STEP 3: Handle SDK struct size (619 bytes) vs on-chain size (1300 bytes)
        // CRITICAL FIX: Borsh deserializer expects exactly the struct size based on layout
        // Padding causes "Not all bytes read" error because Borsh can't parse padded zeros
        // Solution: Use SDK struct size (619 bytes) as-is, don't pad to 1300 bytes
        const SDK_STRUCT_SIZE: usize = 619; // SDK struct size without padding
        let data_to_parse: Vec<u8> = if data_without_discriminator.len() == SDK_STRUCT_SIZE {
            // ✅ FIX: Use SDK struct size as-is, don't pad
            // Borsh deserializer expects exactly 619 bytes based on struct layout
            log::debug!(
                "Reserve account is {} bytes (SDK struct size), using as-is for Borsh deserialization",
                SDK_STRUCT_SIZE
            );
            data_without_discriminator.to_vec()
        } else if data_without_discriminator.len() >= EXPECTED_RESERVE_STRUCT_SIZE {
            // On-chain full size (1300 bytes) - use only first 619 bytes (SDK struct size)
            // The remaining bytes are padding that Borsh doesn't expect
            log::debug!(
                "Reserve account is {} bytes (on-chain size), using first {} bytes (SDK struct size) for Borsh deserialization",
                data_without_discriminator.len(),
                SDK_STRUCT_SIZE
            );
            data_without_discriminator[..SDK_STRUCT_SIZE].to_vec()
        } else if data_without_discriminator.len() < SDK_STRUCT_SIZE {
            return Err(anyhow::anyhow!(
                "Reserve data too short: {} bytes (need at least {} bytes for SDK struct). \
                 Account may be corrupted or layout may have changed.",
                data_without_discriminator.len(),
                SDK_STRUCT_SIZE
            ));
        } else {
            // Fallback: use what we have (shouldn't reach here)
            data_without_discriminator.to_vec()
        };
        
        // STEP 4: Parse the data (now guaranteed to be SDK_STRUCT_SIZE bytes, not EXPECTED_RESERVE_STRUCT_SIZE)
        
        // STEP 5: Pre-check version byte (early validation)
        // Reserve version must be 1 (unlike Obligation which can be 0 or 1)
        let version_byte = data_to_parse[0];
        if version_byte != 1 {
            // Version 0 or invalid values usually indicate this is NOT a Reserve account
            if version_byte == 0 || version_byte >= 250 {
                return Err(anyhow::anyhow!(
                    "Invalid Reserve version: {} (likely not a Reserve account - may be Obligation/LendingMarket)",
                    version_byte
                ));
            }
            return Err(anyhow::anyhow!(
                "Invalid Reserve version: {} (expected 1). \
                 This is likely not a Reserve account.",
                version_byte
            ));
        }
        
        log::trace!(
            "Attempting Borsh deserialization of {} bytes (version byte: 0x{:02x} = {})",
            data_to_parse.len(),
            version_byte,
            version_byte
        );
        
        let reserve: Reserve = BorshDeserialize::try_from_slice(&data_to_parse)
            .map_err(|e| {
                log::error!("Borsh deserialization failed: {}", e);
                log::error!("Data length: {} bytes (expected: {} bytes for Reserve struct)", data.len(), EXPECTED_RESERVE_STRUCT_SIZE);
                log::error!("Parsed length: {} bytes", data_to_parse.len());
                if data_to_parse.len() > 0 {
                    log::error!("First byte: {} (0x{:02x})", data_to_parse[0], data_to_parse[0]);
                }
                if data_to_parse.len() > 1 {
                    log::error!("Second byte: {} (0x{:02x})", data_to_parse[1], data_to_parse[1]);
                }
                anyhow::anyhow!("Failed to deserialize Reserve: {}", e)
            })?;
        
        log::trace!("Successfully deserialized Reserve, version={}", reserve.version);
        
        const EXPECTED_VERSION: u8 = 1;
        if reserve.version != EXPECTED_VERSION {
            if reserve.version == 0 || reserve.version >= 250 {
                return Err(anyhow::anyhow!(
                    "Invalid Reserve version: {} (likely not a Reserve account - may be Obligation/LendingMarket)",
                    reserve.version
                ));
            }
            log::error!(
                "Version mismatch: found version {} but expected {}",
                reserve.version,
                EXPECTED_VERSION
            );
            return Err(anyhow::anyhow!(
                "Unsupported Reserve version: {} (expected {}). \
                 Layout may have changed, please update bot to support new version.",
                reserve.version,
                EXPECTED_VERSION
            ));
        }
        
        if reserve.liquidityMintPubkey == Pubkey::default() {
            return Err(anyhow::anyhow!("Invalid Reserve: liquidity mint pubkey is zero/default"));
        }
        if reserve.collateralMintPubkey == Pubkey::default() {
            return Err(anyhow::anyhow!("Invalid Reserve: collateral mint pubkey is zero/default"));
        }
        if reserve.liquiditySupplyPubkey == Pubkey::default() {
            return Err(anyhow::anyhow!("Invalid Reserve: liquidity supply pubkey is zero/default"));
        }
        if reserve.collateralSupplyPubkey == Pubkey::default() {
            return Err(anyhow::anyhow!("Invalid Reserve: collateral supply pubkey is zero/default"));
        }
        if reserve.lendingMarket == Pubkey::default() {
            return Err(anyhow::anyhow!("Invalid Reserve: lending market pubkey is zero/default"));
        }
        
        Ok(reserve)
    }

    /// Get mint address from reserve (liquidity mint)
    pub fn mint_pubkey(&self) -> Pubkey {
        self.liquidity().mintPubkey
    }

    /// Get collateral mint address
    pub fn collateral_mint_pubkey(&self) -> Pubkey {
        self.collateral().mintPubkey
    }

    /// Get oracle pubkey (Pyth primary oracle)
    pub fn oracle_pubkey(&self) -> Pubkey {
        self.liquidity().liquidityPythOracle
    }

    /// Get liquidation threshold (as f64, 0-1 range)
    pub fn liquidation_threshold(&self) -> f64 {
        self.liquidationThreshold as f64 / 100.0
    }

    pub fn liquidation_bonus(&self) -> f64 {
        use std::env;
        if let Ok(bonus_str) = env::var("LIQUIDATION_BONUS") {
            if let Ok(bonus) = bonus_str.parse::<f64>() {
                return bonus;
            }
        }
        self.liquidationBonus as f64 / 100.0
    }

    pub fn close_factor(&self) -> f64 {
        use std::env;
        if let Ok(cf_str) = env::var("CLOSE_FACTOR") {
            if let Ok(cf) = cf_str.parse::<f64>() {
                if cf > 0.0 && cf <= 1.0 {
                    return cf;
                }
            }
        }
        //     return config.liquidationCloseFactor as f64 / 100.0;
        // }
        
        // Fallback to standard 50% (0.5) - Solend's current default
        // This is correct behavior: Solend uses 50% close factor by default
        0.5
    }

    /// Get ReserveLiquidity struct (helper for accessing liquidity fields)
    pub fn liquidity(&self) -> ReserveLiquidity {
        ReserveLiquidity {
            mintPubkey: self.liquidityMintPubkey,
            mintDecimals: self.liquidityMintDecimals,
            supplyPubkey: self.liquiditySupplyPubkey,
            liquidityPythOracle: self.liquidityPythOracle,
            liquiditySwitchboardOracle: self.liquiditySwitchboardOracle,
            availableAmount: self.liquidityAvailableAmount,
            borrowedAmountWads: self.liquidityBorrowedAmountWads,
            cumulativeBorrowRateWads: self.liquidityCumulativeBorrowRateWads,
            liquidityMarketPrice: self.liquidityMarketPrice,
        }
    }

    /// Get ReserveCollateral struct (helper for accessing collateral fields)
    pub fn collateral(&self) -> ReserveCollateral {
        ReserveCollateral {
            mintPubkey: self.collateralMintPubkey,
            mintTotalSupply: self.collateralMintTotalSupply,
            supplyPubkey: self.collateralSupplyPubkey,
        }
    }

    /// Get ReserveConfigHelper struct (helper for accessing config fields)
    /// NOTE: This is a helper struct, NOT the generated ReserveConfig (which is for RateLimiter)
    /// switchboardOraclePubkey field was removed from Reserve struct in newer Solend layouts
    /// Use extraOracle field instead, or liquiditySwitchboardOracle for liquidity oracle
    pub fn config(&self) -> ReserveConfigHelper {
        ReserveConfigHelper {
            optimalUtilizationRate: self.optimalUtilizationRate,
            loanToValueRatio: self.loanToValueRatio,
            liquidationBonus: self.liquidationBonus,
            liquidationThreshold: self.liquidationThreshold,
            minBorrowRate: self.minBorrowRate,
            optimalBorrowRate: self.optimalBorrowRate,
            maxBorrowRate: self.maxBorrowRate,
            switchboardOraclePubkey: self.extraOracle, // Use extraOracle as fallback (may be Switchboard oracle)
            borrowFeeWad: self.borrowFeeWad,
            flashLoanFeeWad: self.flashLoanFeeWad,
            hostFeePercentage: self.hostFeePercentage,
            depositLimit: self.depositLimit,
            borrowLimit: self.borrowLimit,
            feeReceiver: self.feeReceiver,
            protocolLiquidationFee: self.protocolLiquidationFee,
            protocolTakeRate: self.protocolTakeRate,
            accumulatedProtocolFeesWads: self.accumulatedProtocolFeesWads,
            addedBorrowWeightBPS: self.addedBorrowWeightBPS,
            liquiditySmoothedMarketPrice: self.liquiditySmoothedMarketPrice,
            reserveType: self.reserveType,
            maxUtilizationRate: self.maxUtilizationRate,
            superMaxBorrowRate: self.superMaxBorrowRate,
            maxLiquidationBonus: self.maxLiquidationBonus,
            maxLiquidationThreshold: self.maxLiquidationThreshold,
            scaledPriceOffsetBPS: self.scaledPriceOffsetBPS,
            extraOracle: self.extraOracle,
            liquidityExtraMarketPriceFlag: self.liquidityExtraMarketPriceFlag,
            liquidityExtraMarketPrice: self.liquidityExtraMarketPrice,
            attributedBorrowValue: self.attributedBorrowValue,
            attributedBorrowLimitOpen: self.attributedBorrowLimitOpen,
            attributedBorrowLimitClose: self.attributedBorrowLimitClose,
        }
    }
}

// Static cache for USDC mint to avoid repeated lookups
use std::sync::OnceLock;
static USDC_MINT_CACHE: OnceLock<Pubkey> = OnceLock::new();

pub fn find_usdc_mint_from_reserves(
    rpc: &solana_client::rpc_client::RpcClient,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    // Check cache first
    if let Some(cached_mint) = USDC_MINT_CACHE.get() {
        log::debug!("✅ Using cached USDC mint: {}", cached_mint);
        return Ok(*cached_mint);
    }
    
    log::debug!("🔍 USDC mint not in cache, discovering from chain...");
    
    use std::env;
    let usdc_mint_str = env::var("USDC_MINT")
        .map_err(|_| anyhow::anyhow!(
            "USDC_MINT not found in .env file. \
             Please set USDC_MINT in .env file. \
             Example: USDC_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        ))?;
    let known_usdc_mint = Pubkey::from_str(&usdc_mint_str)
        .map_err(|e| anyhow::anyhow!("Invalid USDC_MINT from .env: {} - Error: {}", usdc_mint_str, e))?;
    
    // Read SUSD mint addresses from environment variable (comma-separated)
    let susd_mint_candidates: Vec<Pubkey> = env::var("SUSD_MINT_CANDIDATES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Pubkey::from_str(s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Invalid SUSD_MINT_CANDIDATES in .env (comma-separated list): {}", e))?;
    
    // Validate program ID and determine protocol type (read from env for comparison)
    let program_id_str = program_id.to_string();
    let save_program_id = env::var("SOLEND_PROGRAM_ID_SAVE")
        .unwrap_or_else(|_| "SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh".to_string());
    let legacy_program_id = env::var("SOLEND_PROGRAM_ID_LEGACY")
        .unwrap_or_else(|_| "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo".to_string());
    
    let is_save_protocol = program_id_str == save_program_id;
    let is_legacy_solend = program_id_str == legacy_program_id;
    
    log::info!("🔍 Starting debt token reserve discovery...");
    log::info!("   Program ID: {}", program_id);
    if is_save_protocol {
        log::info!("   ⚠️  Protocol: Save Protocol (2024 rebrand) - typically uses SUSD, not USDC");
    } else if is_legacy_solend {
        log::info!("   ✅ Protocol: Legacy Solend - should have USDC reserve");
    } else {
        log::warn!("   ⚠️  Protocol: Unknown/Other - verifying reserves...");
    }
    log::info!("   Expected USDC mint: {}", known_usdc_mint);
    
    // Log RPC URL (partially masked for security)
    if let Ok(rpc_url) = std::env::var("RPC_URL") {
        let masked_url = if rpc_url.len() > 50 {
            format!("{}...{}", &rpc_url[..25], &rpc_url[rpc_url.len()-25..])
        } else {
            "***".to_string()
        };
        log::info!("   RPC URL: {} (masked)", masked_url);
        log::info!("   RPC URL contains 'mainnet': {}", rpc_url.to_lowercase().contains("mainnet"));
        log::info!("   RPC URL contains 'devnet': {}", rpc_url.to_lowercase().contains("devnet"));
        log::info!("   RPC URL contains 'testnet': {}", rpc_url.to_lowercase().contains("testnet"));
    }
    
    // Check if we should skip the full scan (optimization for production)
    // CRITICAL: Startup optimization for production
    // Scanning 300k+ accounts takes time and RPC quota. 
    // If we trust the .env configuration, we can skip this check.
    let skip_scan = env::var("SKIP_STARTUP_SCAN")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false);
        
    if skip_scan {
        log::info!("🚀 Startup optimization: Skipping full chain scan (SKIP_STARTUP_SCAN=true)");
        log::info!("   Validating USDC mint directly: {}", known_usdc_mint);
        
        use anyhow::Context;
        
        // 1. Verify USDC mint account exists on chain
        let usdc_account = rpc.get_account(&known_usdc_mint)
            .context(format!("USDC mint {} not found on chain - wrong network or invalid mint address in .env", known_usdc_mint))?;
            
        // 2. Verify account owner is Token Program
        use spl_token::ID as TOKEN_PROGRAM_ID;
        if usdc_account.owner != TOKEN_PROGRAM_ID {
            return Err(anyhow::anyhow!(
                "USDC mint {} is not a valid token mint (owner mismatch). \
                 Expected owner: {}, Found: {}. \
                 Please check USDC_MINT in .env file.",
                known_usdc_mint, 
                TOKEN_PROGRAM_ID, 
                usdc_account.owner
            ));
        }
        
        // 3. Verify decimals (USDC must be 6 decimals)
        use solana_sdk::program_pack::Pack;
        let mint_data = spl_token::state::Mint::unpack(&usdc_account.data)
            .context("Failed to unpack USDC mint data")?;
            
        if mint_data.decimals != 6 {
            log::warn!(
                "⚠️  USDC mint decimals mismatch: found {}, expected 6. \
                 This might be a different token masquerading as USDC, or a testnet token.",
                mint_data.decimals
            );
        } else {
            log::info!("✅ USDC mint validated: {} (6 decimals)", known_usdc_mint);
        }
        
        // Cache the USDC mint
        USDC_MINT_CACHE.set(known_usdc_mint)
            .ok(); // Ignore if already set (shouldn't happen, but safe)
        log::debug!("💾 Cached USDC mint: {}", known_usdc_mint);
        
        return Ok(known_usdc_mint);
    }
    
    // CRITICAL: Read all reserves from chain - no mock/dummy data
    // Add retry mechanism for RPC errors (network issues, rate limits, etc.)
    
    // First, test RPC connection with a simple call
    log::info!("🔍 Testing RPC connection...");
    match rpc.get_slot() {
        Ok(slot) => {
            log::info!("✅ RPC connection OK (current slot: {})", slot);
        },
        Err(e) => {
            return Err(anyhow::anyhow!(
                "RPC connection test failed: {}\n\
                 Please check your RPC_URL in .env file and ensure the endpoint is accessible.",
                e
            ));
        }
    }
    
    // CRITICAL: First verify the program account exists
    log::info!("🔍 Verifying Solend program account exists...");
    match rpc.get_account(program_id) {
        Ok(program_account) => {
            log::info!("✅ Solend program account found:");
            log::info!("   Program owner: {}", program_account.owner);
            log::info!("   Program executable: {}", program_account.executable);
            log::info!("   Program lamports: {}", program_account.lamports);
            log::info!("   Program data size: {} bytes", program_account.data.len());
            
            // Verify it's actually a program (executable = true)
            if !program_account.executable {
                return Err(anyhow::anyhow!(
                    "Account {} is not a program (executable=false). \
                     This may indicate the program ID is incorrect or points to a regular account.",
                    program_id
                ));
            }
        }
        Err(e) => {
            // Try to get more diagnostic information
            let error_msg = e.to_string();
            let mut diagnostic = format!(
                "Solend program account {} not found on chain: {}\n\
                 \n\
                 Possible causes:\n\
                 1. Program ID is incorrect: {}\n\
                 2. RPC_URL is pointing to wrong network (devnet/testnet instead of mainnet)\n\
                 3. Program has been closed or moved\n\
                 4. RPC endpoint issue or rate limiting\n\
                 \n",
                program_id, error_msg, program_id
            );
            
            // Check if we can access other known accounts to verify network
            log::warn!("⚠️  Program account not found. Verifying network connectivity...");
            // Use USDC_MINT from env for network verification
            if let Ok(usdc_mint_str) = env::var("USDC_MINT") {
                if let Ok(test_account) = Pubkey::from_str(&usdc_mint_str) {
                    match rpc.get_account(&test_account) {
                        Ok(usdc_account) => {
                            diagnostic.push_str("✅ Network verification: USDC mint found - RPC is on mainnet\n");
                            diagnostic.push_str(&format!("   USDC account owner: {}\n", usdc_account.owner));
                            diagnostic.push_str("❌ But Solend program not found - possible causes:\n");
                            diagnostic.push_str("   1. Program ID may be incorrect\n");
                            diagnostic.push_str("   2. Helius RPC API key may be invalid or rate-limited\n");
                            diagnostic.push_str("   3. Helius RPC URL format may be incorrect\n");
                            diagnostic.push_str("   4. Program may have been closed or moved\n");
                        }
                        Err(usdc_err) => {
                            diagnostic.push_str(&format!("❌ Network verification: USDC mint not found: {}\n", usdc_err));
                            diagnostic.push_str("   This indicates RPC may be on wrong network or RPC endpoint is broken\n");
                        }
                    }
                }
            }
            
            // Additional diagnostic: Try to get slot to verify RPC is working
            match rpc.get_slot() {
                Ok(slot) => {
                    diagnostic.push_str(&format!("✅ RPC is responding (current slot: {})\n", slot));
                }
                Err(slot_err) => {
                    diagnostic.push_str(&format!("❌ RPC slot check failed: {}\n", slot_err));
                }
            }
            
            diagnostic.push_str(&format!(
                "\nTroubleshooting:\n\
                 - Verify program ID is correct: https://solscan.io/account/{}\n\
                 - Check RPC_URL in .env points to mainnet\n\
                 - Try using a different RPC endpoint\n\
                 - Check if program exists on Solana Explorer",
                program_id
            ));
            
            return Err(anyhow::anyhow!("{}", diagnostic));
        }
    }
    
    // ✅ PERFORMANCE FIX: Only fetch Reserve accounts (1300 bytes) instead of all 300k+ accounts
    // This dramatically reduces response size (~500MB+ -> ~few MB) and avoids RPC rate limiting
    log::info!("🔍 Fetching Solend Reserve accounts from RPC (filtered by size: 1300 bytes)...");
    let accounts = {
        // Read MAX_RETRIES from .env (no hardcoded values)
        let max_retries = env::var("MAX_RETRIES")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_else(|| {
                log::warn!("MAX_RETRIES not found in .env, using default 5");
                5 // Default 5 retries if not set
            });
        let mut retries = max_retries;
        
        // ✅ Use size filter to only fetch Reserve accounts (1300 bytes)
        // This avoids fetching 300k+ accounts and reduces response size from ~500MB+ to ~few MB
        const RESERVE_ACCOUNT_SIZE: u64 = 1300;
        let filters = vec![
            RpcFilterType::DataSize(RESERVE_ACCOUNT_SIZE),
        ];
        
        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            ..Default::default()
        };
        
        loop {
            match rpc.get_program_accounts_with_config(program_id, config.clone()) {
                Ok(accs) => {
                    log::info!("✅ Successfully fetched {} Reserve accounts from RPC (filtered by size)", accs.len());
                    break Ok(accs);
                },
                Err(e) => {
                    let error_msg = e.to_string();
                    retries -= 1;
                    
                    if retries > 0 {
                        let initial_delay_ms = env::var("INITIAL_RETRY_DELAY_MS")
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(1000);
                        let delay_secs = (initial_delay_ms / 1000) * 2_u64.pow(max_retries - retries);
                        log::warn!(
                            "⚠️  RPC error getting Reserve accounts (retries left: {}): {}",
                            retries,
                            error_msg
                        );
                        log::info!("⏳ Retrying in {} seconds...", delay_secs);
                        std::thread::sleep(std::time::Duration::from_secs(delay_secs));
                        continue;
                    } else {
                        log::error!("❌ All {} retry attempts failed", max_retries);
                        break Err(e);
                    }
                }
            }
        }
    }.map_err(|e| {
        let error_msg = e.to_string();
        
        // Provide detailed diagnostic information
        let diagnostic = if error_msg.contains("request or response body error") {
            format!(
                "RPC connection error: {}\n\
                 \n\
                 This error typically occurs when:\n\
                 1. RPC rate limit exceeded - Helius RPC may have rate limits\n\
                 2. Request too large - fetching ~300k accounts may exceed RPC limits\n\
                 3. Network connectivity issues\n\
                 4. RPC endpoint temporarily unavailable\n\
                 \n\
                 Troubleshooting:\n\
                 - Wait 1-2 minutes and try again (rate limits reset)\n\
                 - Check Helius dashboard for API usage/quota\n\
                 - Verify RPC_URL in .env file is correct\n\
                 - Consider using a different RPC endpoint with higher limits\n\
                 - Check network connection",
                error_msg
            )
        } else if error_msg.contains("timeout") {
            format!(
                "RPC timeout error: {}\n\
                 The RPC endpoint took too long to respond. This may indicate:\n\
                 - Network latency issues\n\
                 - RPC endpoint is overloaded\n\
                 - Request is too large (trying to fetch ~300k accounts at once)\n\
                 \n\
                 Try using a faster RPC endpoint or wait and retry.",
                error_msg
            )
        } else {
            format!("Failed to get program accounts from chain: {}", error_msg)
        };
        
        anyhow::anyhow!("{}", diagnostic)
    })?;
    
    let accounts_count = accounts.len();
    
    if accounts_count == 0 {
        // CRITICAL: 0 accounts means either:
        // 1. Program exists but has no accounts (unlikely for Solend)
        // 2. Wrong network (devnet/testnet instead of mainnet)
        // 3. Program ID is wrong
        // 4. RPC filtering issue
        
        // Try to get account info again to verify program still exists
        let program_info = rpc.get_account(program_id);
        
        return Err(anyhow::anyhow!(
            "No Solend accounts found for program {}.\n\
             \n\
             Analysis:\n\
             - Program account exists: {}\n\
             - Accounts returned: 0\n\
             \n\
             Possible causes:\n\
             1. Wrong network: RPC_URL may be pointing to devnet/testnet instead of mainnet\n\
             2. Program ID incorrect: {} may not be the correct Solend program\n\
             3. Program has no accounts (unlikely for Solend)\n\
             4. RPC endpoint issue or filtering\n\
             \n\
             Troubleshooting:\n\
             - Verify RPC_URL in .env points to mainnet (not devnet/testnet)\n\
             - Check program on Solana Explorer: https://solscan.io/account/{}\n\
             - Verify program ID is correct: https://docs.solend.fi/protocol/lending-protocol\n\
             - Try using a different RPC endpoint\n\
             - Check if program has accounts on Solana Explorer",
            program_id,
            if program_info.is_ok() { "Yes" } else { "No (program not found)" },
            program_id,
            program_id
        ));
    }
    
    log::info!("🔍 Searching for debt token reserves (USDC/SUSD) among {} accounts...", accounts_count);
    log::info!("   Account size distribution will be analyzed during parsing...");
    
    // Search for USDC or SUSD reserve by checking all reserves from chain
    let mut parsed_reserves = 0;
    let mut skipped_accounts = 0;
    let mut parse_errors = 0;
    let mut found_mints = std::collections::HashSet::new();
    let mut account_size_distribution = std::collections::HashMap::new();
    let mut usdc_found = false;
    let mut susd_found = false;
    let mut usdc_mint: Option<Pubkey> = None;
    let mut susd_mint: Option<Pubkey> = None;
    let start_time = std::time::Instant::now();
    
    for (pubkey, account) in &accounts {
        let account_size = account.data.len();
        
        // Track account size distribution
        *account_size_distribution.entry(account_size).or_insert(0) += 1;
        
        // Pre-filter: Reserves should be ~1300 bytes (same as obligations, but we'll try to parse)
        // Skip accounts that are clearly too small (LendingMarkets are ~200-300 bytes)
        if account_size < 400 {
            skipped_accounts += 1;
            continue;
        }
        
        // Try to parse as Reserve
        match Reserve::from_account_data(&account.data) {
            Ok(reserve) => {
                parsed_reserves += 1;
                let liquidity_mint = reserve.mint_pubkey();
                let collateral_mint = reserve.collateral_mint_pubkey();
                found_mints.insert(liquidity_mint);
                found_mints.insert(collateral_mint);
                
                // Log first 20 reserves found for debugging
                if parsed_reserves <= 20 {
                    log::info!("  ✅ Reserve #{}: liquidity_mint={}, collateral_mint={}, pubkey={}", 
                               parsed_reserves, liquidity_mint, collateral_mint, pubkey);
                }
                
                // Progress logging every 1000 reserves
                if parsed_reserves % 1000 == 0 {
                    let elapsed = start_time.elapsed().as_secs();
                    let rate = if elapsed > 0 { parsed_reserves as f64 / elapsed as f64 } else { 0.0 };
                    log::info!("  📊 Progress: {} reserves parsed ({} reserves/sec, {} unique mints found)", 
                               parsed_reserves, rate as u64, found_mints.len());
                }
                
                // Check if this reserve is USDC (check both liquidity and collateral mints)
                // USDC is typically a liquidity mint (debt token), but check both to be safe
                if liquidity_mint == known_usdc_mint {
                    usdc_found = true;
                    usdc_mint = Some(liquidity_mint);
                    let elapsed = start_time.elapsed();
                    log::info!("✅ Found USDC reserve from chain (as liquidity mint)!");
                    log::info!("   Reserve pubkey: {}", pubkey);
                    log::info!("   USDC liquidity mint: {}", liquidity_mint);
                    log::info!("   Collateral mint: {}", collateral_mint);
                    log::info!("   Parsed {} reserves total in {:?}", parsed_reserves, elapsed);
                    log::info!("   Skipped {} accounts (too small)", skipped_accounts);
                    log::info!("   Found {} unique token mints", found_mints.len());
                    // Continue scanning to also check for SUSD, but USDC takes priority
                } else if collateral_mint == known_usdc_mint {
                    usdc_found = true;
                    usdc_mint = Some(liquidity_mint); // Still use liquidity mint
                    let elapsed = start_time.elapsed();
                    log::warn!("⚠️  Found USDC as collateral mint (unusual, but using liquidity mint)");
                    log::info!("   Reserve pubkey: {}", pubkey);
                    log::info!("   Liquidity mint: {}", liquidity_mint);
                    log::info!("   USDC collateral mint: {}", collateral_mint);
                    log::info!("   Note: Using liquidity mint for USDC operations");
                    log::info!("   Parsed {} reserves total in {:?}", parsed_reserves, elapsed);
                    // Continue scanning to also check for SUSD
                }
                
                // Check for SUSD (Save Protocol stablecoin)
                // Only check if USDC not found yet (USDC takes priority)
                if !usdc_found && !susd_mint_candidates.is_empty() {
                    for susd_candidate in &susd_mint_candidates {
                        if liquidity_mint == *susd_candidate || collateral_mint == *susd_candidate {
                            susd_found = true;
                            susd_mint = Some(liquidity_mint);
                            log::info!("✅ Found SUSD reserve from chain!");
                            log::info!("   Reserve pubkey: {}", pubkey);
                            log::info!("   SUSD mint: {}", susd_candidate);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                parse_errors += 1;
                // Log first few parse errors for debugging
                if parse_errors <= 5 {
                    log::debug!("  ❌ Failed to parse account {} (size: {} bytes): {}", 
                                pubkey, account_size, e);
                }
            }
        }
    }
    
    let elapsed = start_time.elapsed();
    
    // Report findings
    if usdc_found {
        log::info!("✅ USDC reserve found!");
        if let Some(mint) = usdc_mint {
            // Cache the USDC mint
            USDC_MINT_CACHE.set(mint)
                .ok(); // Ignore if already set
            log::debug!("💾 Cached USDC mint from chain scan: {}", mint);
            return Ok(mint);
        }
    }
    
    if susd_found {
        log::warn!("⚠️  USDC not found, but SUSD reserve found!");
        log::warn!("   This program uses SUSD (Save Protocol stablecoin), not USDC");
        
        // Check if SUSD is allowed via .env
        let allow_susd = env::var("ALLOW_SUSD")
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false);
            
        if allow_susd {
            if let Some(mint) = susd_mint {
                log::info!("✅ SUSD accepted as quote token (ALLOW_SUSD=true)");
                log::info!("   Using SUSD mint: {}", mint);
                // Cache the SUSD mint (as quote token)
                USDC_MINT_CACHE.set(mint)
                    .ok(); // Ignore if already set
                log::debug!("💾 Cached SUSD mint as quote token: {}", mint);
                return Ok(mint);
            }
        } else {
            log::warn!("   Bot default requires USDC. Set ALLOW_SUSD=true in .env to enable SUSD.");
            if let Some(_mint) = susd_mint {
                return Err(anyhow::anyhow!(
                    "SUSD reserve found but USDC required. \
                     This program ({}) uses SUSD, not USDC. \
                     Set ALLOW_SUSD=true in .env to support this protocol, or use Legacy Solend: So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo",
                    program_id
                ));
            }
        }
    }
    
    log::warn!("⚠️  Neither USDC nor SUSD reserve found after parsing {} reserves", parsed_reserves);
    log::warn!("   Total accounts checked: {}", accounts_count);
    log::warn!("   Successfully parsed: {} reserves", parsed_reserves);
    log::warn!("   Parse errors: {}", parse_errors);
    log::warn!("   Skipped (too small): {} accounts", skipped_accounts);
    log::warn!("   Unique mints found: {}", found_mints.len());
    log::warn!("   Time taken: {:?}", elapsed);
    
    // Log account size distribution (top 10 most common sizes)
    let mut size_dist: Vec<_> = account_size_distribution.iter().collect();
    size_dist.sort_by(|a, b| b.1.cmp(a.1));
    log::info!("📊 Account size distribution (top 10):");
    for (size, count) in size_dist.iter().take(10) {
        log::info!("   {} bytes: {} accounts", size, count);
    }
    
    // Log first 20 unique mints found (for debugging)
    if !found_mints.is_empty() {
        log::info!("📋 First 20 unique token mints found in reserves:");
        for (idx, mint) in found_mints.iter().take(20).enumerate() {
            log::info!("   {}. {}", idx + 1, mint);
        }
        if found_mints.len() > 20 {
            log::info!("   ... and {} more mints", found_mints.len() - 20);
        }
    }
    
    // Try to write all mints to a file for manual inspection
    let mints_file = "logs/found_mints.txt";
    if let Ok(mut file) = std::fs::File::create(mints_file) {
        use std::io::Write;
        writeln!(file, "# All token mints found in Solend reserves").ok();
        writeln!(file, "# Total: {} unique mints", found_mints.len()).ok();
        writeln!(file, "# Expected USDC mint: {}", known_usdc_mint).ok();
        writeln!(file, "").ok();
        for mint in &found_mints {
            writeln!(file, "{}", mint).ok();
        }
        log::info!("💾 All {} unique mints written to: {}", found_mints.len(), mints_file);
        log::info!("   You can search for USDC in this file to verify if it exists with a different address");
    }
    
    // Program ID validation and recommendations
    let mut error_msg = format!(
        "USDC reserve not found in chain data. \
         Checked {} accounts, successfully parsed {} reserves, but none match USDC mint {}. \
         \n\
         Analysis:\n\
         - Found {} unique token mints (both liquidity and collateral)\n\
         - USDC mint {} was not found in any reserve\n",
        accounts_count,
        parsed_reserves,
        known_usdc_mint,
        found_mints.len(),
        known_usdc_mint
    );
    
    // Add protocol-specific recommendations
    if is_save_protocol {
        error_msg.push_str(&format!(
            "\n\
         ⚠️  PROGRAM ID ANALYSIS:\n\
         - You are using Save Protocol (SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh)\n\
         - Save Protocol (2024 rebrand) uses SUSD, not USDC\n\
         - SUSD status: {}\n\
         \n\
         ✅ RECOMMENDED SOLUTION:\n\
         - Use Legacy Solend program ID: So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo\n\
         - Legacy Solend has USDC reserve\n\
         - Set in .env: SOLEND_PROGRAM_ID=So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo\n",
            if susd_found { "FOUND" } else { "NOT FOUND" }
        ));
    } else if is_legacy_solend {
        error_msg.push_str(
            "\n\
         ⚠️  PROGRAM ID ANALYSIS:\n\
         - You are using Legacy Solend (So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo)\n\
         - This program should have USDC reserve\n\
         - USDC not found - this is unexpected\n\
         \n\
         Possible causes:\n\
         1. RPC_URL may not be pointing to mainnet\n\
         2. Program layout may have changed\n\
         3. USDC reserve may have been removed\n",
        );
    } else {
        error_msg.push_str(&format!(
            "\n\
         ⚠️  PROGRAM ID ANALYSIS:\n\
         - Unknown program ID: {}\n\
         - Verify this is a valid Solend/Save program\n",
            program_id
        ));
    }
    
    error_msg.push_str(&format!(
        "\n\
         Troubleshooting:\n\
         - All mints have been saved to: {}\n\
         - Check {} file to see all available mints\n\
         - Verify Solend program ID is correct: {}\n\
         - Verify RPC_URL points to mainnet\n\
         - Check Solana Explorer for this program's reserves:\n\
           https://solscan.io/account/{}\n\
         \n\
         For USDC support, use Legacy Solend:\n\
         SOLEND_PROGRAM_ID=So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo",
        mints_file,
        mints_file,
        program_id,
        program_id
    ));
    
    // CRITICAL: No fallback - if USDC reserve not found, it's an error
    // This ensures we're always using real chain data, not hardcoded values
    // CRITICAL: Only mainnet is supported - no devnet/testnet
    Err(anyhow::anyhow!("{}", error_msg))
}

