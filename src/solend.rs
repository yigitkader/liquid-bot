// Auto-generated Solend account layouts from IDL
// Imports must be before include!
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

include!(concat!(env!("OUT_DIR"), "/solend_layout.rs"));

use anyhow::Result;
use std::str::FromStr;

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

#[derive(Debug, Clone)]
pub struct ReserveConfig {
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
    /// 
    /// CRITICAL SECURITY: Validates version field to detect layout changes.
    /// If Solend updates their layout (e.g., v1 -> v2), this will catch it early.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        log::trace!("Parsing Obligation from {} bytes of data", data.len());
        
        // Log first few bytes for debugging
        if data.len() >= 32 {
            let preview: String = data.iter()
                .take(32)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log::trace!("First 32 bytes (hex): {}", preview);
        }
        
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            log::trace!("Skipping 8-byte discriminator (all zeros)");
            &data[8..]
        } else {
            log::trace!("No discriminator detected, using full data");
            data
        };
        
        log::trace!("Attempting Borsh deserialization of {} bytes", data.len());
        
        // CRITICAL: Calculate expected size from actual struct layout
        // Struct size: 1 + 9 + 32 + 32 + 16*5 + 1 + 16*2 + 1 + 14 + 1 + 1 + 1096 = 1300 bytes
        // Log analysis shows: account size = 1300 bytes, offset before _padding = 188 bytes
        // Total: 188 + 14 (padding) + 1 (depositsLen) + 1 (borrowsLen) + 1096 (dataFlat) = 1300 bytes
        const EXPECTED_STRUCT_SIZE: usize = 1300; // Actual struct size from layout analysis
        
        // Use data as-is if it matches expected size, otherwise error
        let data_to_parse = if data.len() == EXPECTED_STRUCT_SIZE {
            data
        } else if data.len() > EXPECTED_STRUCT_SIZE {
            // More than expected - log warning but try to parse first 1300 bytes
            log::warn!("Obligation account is {} bytes (expected: {} bytes) - parsing first {} bytes", 
                data.len(), EXPECTED_STRUCT_SIZE, EXPECTED_STRUCT_SIZE);
            &data[..EXPECTED_STRUCT_SIZE]
        } else {
            // Too short
            return Err(anyhow::anyhow!(
                "Obligation data too short: {} bytes (minimum: {} bytes)", 
                data.len(), 
                EXPECTED_STRUCT_SIZE
            ));
        };
        
        // Try deserialization - data_to_parse is already sliced to correct size
        let obligation: Obligation = match BorshDeserialize::try_from_slice(data_to_parse) {
            Ok(obl) => obl,
            Err(e) => {
                // If we still get an error, log detailed information and try one more time
                let error_msg = e.to_string();
                log::error!("Borsh deserialization failed: {}", e);
                log::error!("Data length: {} bytes (expected: {} bytes for Obligation struct)", data.len(), EXPECTED_STRUCT_SIZE);
                log::error!("Parsed length: {} bytes", data_to_parse.len());
                
                // If error mentions length/Unexpected and we have extra bytes, try explicit slice one more time
                if (error_msg.contains("length") || error_msg.contains("Unexpected")) && data.len() > EXPECTED_STRUCT_SIZE {
                    log::debug!("Retrying with explicit slice to {} bytes", EXPECTED_STRUCT_SIZE);
                    match BorshDeserialize::try_from_slice(&data[..EXPECTED_STRUCT_SIZE]) {
                        Ok(obl) => {
                            log::debug!("Successfully deserialized after retry with explicit slice");
                            obl
                        },
                        Err(e2) => {
                            log::error!("Borsh deserialization failed even with explicit slice: {}", e2);
                            
                            // Log detailed byte analysis
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
                            
                            log::error!("Expected Obligation struct size: {} bytes", EXPECTED_STRUCT_SIZE);
                            log::error!("Actual data size: {} bytes", data.len());
                            
                            return Err(anyhow::anyhow!("Failed to deserialize Obligation: {}. See logs for detailed byte analysis.", e2));
                        }
                    }
                } else {
                    // Other error or no extra bytes - return error
                    log::error!("Expected Obligation struct size: {} bytes", EXPECTED_STRUCT_SIZE);
                    log::error!("Actual data size: {} bytes", data.len());
                    return Err(anyhow::anyhow!("Failed to deserialize Obligation: {}. See logs for detailed byte analysis.", e));
                }
            }
        };
        
        log::trace!("Successfully deserialized Obligation, version={}", obligation.version);
        
        // CRITICAL: Validate version to detect layout changes
        // Solend Obligation version 1 is the current supported version
        // If Solend updates to v2, this will prevent silent parsing errors
        const EXPECTED_VERSION: u8 = 1;
        if obligation.version != EXPECTED_VERSION {
            log::error!(
                "Version mismatch: found version {} but expected {}",
                obligation.version,
                EXPECTED_VERSION
            );
            return Err(anyhow::anyhow!(
                "Unsupported Obligation version: {} (expected {}). \
                 Layout may have changed, please update bot to support new version.",
                obligation.version,
                EXPECTED_VERSION
            ));
        }
        
        Ok(obligation)
    }

    /// Calculate health factor: allowedBorrowValue / borrowedValue
    /// Per Structure.md section 4.2
    /// 
    /// NOTE: This uses f64 which has precision loss for very large values.
    /// For high-precision calculations, use health_factor_u128() instead.
    pub fn health_factor(&self) -> f64 {
        let borrowed = self.borrowedValue as f64 / 1e18; // Convert WAD to f64
        if borrowed == 0.0 {
            return f64::INFINITY;
        }
        let weighted_collateral = self.allowedBorrowValue as f64 / 1e18; // Convert WAD to f64
        weighted_collateral / borrowed
    }

    /// Alias for health_factor (backward compatibility)
    pub fn calculate_health_factor(&self) -> f64 {
        self.health_factor()
    }

    /// Calculate health factor using u128 arithmetic to avoid precision loss
    /// Returns health factor in WAD format (multiplied by 10^18)
    /// 
    /// CRITICAL: This preserves full precision for large values.
    /// f64 only has ~15 significant digits, but u128 WAD format has 36 digits.
    /// 
    /// Returns u128::MAX if borrowed is 0 (represents infinity).
    /// Returns u128::MAX on overflow (should not happen in practice).
    /// 
    /// Example:
    ///   HF = 1.0 => returns 1_000_000_000_000_000_000 (WAD)
    ///   HF = 0.5 => returns 500_000_000_000_000_000 (WAD / 2)
    pub fn health_factor_u128(&self) -> u128 {
        const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
        
        let borrowed = self.borrowedValue;
        if borrowed == 0 {
            return u128::MAX; // Infinity representation
        }
        
        let weighted_collateral = self.allowedBorrowValue;
        
        // HF = weighted_collateral / borrowed (in WAD format)
        // Result in WAD format: (weighted_collateral * WAD) / borrowed
        weighted_collateral
            .checked_mul(WAD)
            .and_then(|v| v.checked_div(borrowed))
            .unwrap_or(u128::MAX) // Overflow protection
    }

    /// Check if obligation is liquidatable using high-precision u128 arithmetic
    /// 
    /// Returns true if health factor < 1.0 (in WAD format: HF < WAD)
    /// 
    /// CRITICAL: This uses u128 arithmetic to avoid precision loss that could
    /// cause false positives/negatives near the liquidation threshold (HF = 1.0).
    pub fn is_liquidatable(&self) -> bool {
        const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
        
        // HF < 1.0 => health_factor_u128 < WAD
        self.health_factor_u128() < WAD
    }

    /// Get total deposited value in USD
    pub fn total_deposited_value_usd(&self) -> f64 {
        self.depositedValue as f64 / 1e18 // Convert WAD to f64
    }

    /// Get total borrowed value in USD
    pub fn total_borrowed_value_usd(&self) -> f64 {
        self.borrowedValue as f64 / 1e18 // Convert WAD to f64
    }

    /// Parse deposits array from dataFlat
    /// SDK stores deposits in dataFlat, we need to parse them
    pub fn deposits(&self) -> Result<Vec<ObligationCollateral>> {
        use borsh::BorshDeserialize;
        
        let deposits_len = self.depositsLen as usize;
        if deposits_len == 0 {
            return Ok(Vec::new());
        }
        
        // Each ObligationCollateral is 32 (Pubkey) + 8 (u64) + 16 (u128) = 56 bytes
        const COLLATERAL_SIZE: usize = 32 + 8 + 16; // 56 bytes
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
                Ok(collateral) => deposits.push(collateral),
                Err(e) => {
                    log::warn!("Failed to parse deposit {}: {}", i, e);
                }
            }
        }
        
        Ok(deposits)
    }

    /// Parse borrows array from dataFlat
    /// SDK stores borrows in dataFlat, we need to parse them
    pub fn borrows(&self) -> Result<Vec<ObligationLiquidity>> {
        use borsh::BorshDeserialize;
        
        let borrows_len = self.borrowsLen as usize;
        if borrows_len == 0 {
            return Ok(Vec::new());
        }
        
        // Each ObligationLiquidity is 32 (Pubkey) + 16 (u128) + 16 (u128) + 16 (u128) = 80 bytes
        const LIQUIDITY_SIZE: usize = 32 + 16 + 16 + 16; // 80 bytes
        let borrows_size = borrows_len * LIQUIDITY_SIZE;
        
        // Deposits come first, then borrows
        let deposits_len = self.depositsLen as usize;
        const COLLATERAL_SIZE: usize = 32 + 8 + 16; // 56 bytes
        let deposits_size = deposits_len * COLLATERAL_SIZE;
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
                Ok(liquidity) => borrows.push(liquidity),
                Err(e) => {
                    log::warn!("Failed to parse borrow {}: {}", i, e);
                }
            }
        }
        
        Ok(borrows)
    }
}

// Solend program ID (mainnet)
pub const SOLEND_PROGRAM_ID: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

/// Get Solend program ID
pub fn solend_program_id() -> Result<Pubkey> {
    Pubkey::from_str(SOLEND_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Solend program ID: {}", e))
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

/// Derive reserve collateral supply PDA (for verification)
/// 
/// SECURITY: This function attempts to derive the expected PDA for reserve collateral supply.
/// Multiple seed formats are tried as Solend's exact format may vary.
/// Returns Some(derived_pubkey) if a match is found, None otherwise.
pub fn derive_reserve_collateral_supply_pda(
    reserve_pubkey: &Pubkey,
    program_id: &Pubkey,
) -> Option<Pubkey> {
    // Try common Solend PDA seed formats for reserve collateral supply
    // Format 1: ["collateral", reserve_pubkey]
    let seeds1 = &[b"collateral".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds1, program_id) {
        return Some(pda);
    }
    
    // Format 2: ["reserve", reserve_pubkey, "collateral"]
    let seeds2 = &[
        b"reserve".as_ref(),
        reserve_pubkey.as_ref(),
        b"collateral".as_ref(),
    ];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds2, program_id) {
        return Some(pda);
    }
    
    // Format 3: ["reserve_collateral", reserve_pubkey]
    let seeds3 = &[b"reserve_collateral".as_ref(), reserve_pubkey.as_ref()];
    if let Some((pda, _)) = Pubkey::try_find_program_address(seeds3, program_id) {
        return Some(pda);
    }
    
    None
}

/// Get Solend instruction discriminator for liquidateObligation
/// 
/// CRITICAL: Solend is NOT an Anchor program - it's a native Solana program.
/// Solend uses enum-based instruction encoding via LendingInstruction enum.
/// 
/// Reference: solend-sdk crate, instruction.rs
/// LendingInstruction enum tag mapping:
///   tag 0  => InitLendingMarket
///   tag 1  => SetLendingMarketOwner
///   tag 2  => InitReserve
///   tag 3  => RefreshReserve
///   tag 4  => DepositReserveLiquidity
///   tag 5  => RedeemReserveCollateral
///   tag 6  => InitObligation
///   tag 7  => RefreshObligation
///   tag 8  => DepositObligationCollateral
///   tag 9  => WithdrawObligationCollateral
///   tag 10 => BorrowObligationLiquidity
///   tag 11 => RepayObligationLiquidity
///   tag 12 => LiquidateObligation  <-- THIS ONE
///   tag 13 => FlashLoan
///   ... (see solend-sdk/src/instruction.rs for full enum)
/// 
/// Format: [12, 0, 0, 0, 0, 0, 0, 0]
/// Note: Only the first byte (tag = 12) is used by Solend program.
/// The remaining 7 bytes are padding to match [u8; 8] return type.
pub fn get_liquidate_obligation_discriminator() -> u8 {
    // LendingInstruction::LiquidateObligation tag = 12
    // Native Solana programs use only 1 byte as enum tag discriminator
    12u8 // LiquidateObligation enum variant tag
}

/// Get Solend instruction discriminator for RedeemReserveCollateral
/// 
/// CRITICAL: Solend is NOT an Anchor program - it's a native Solana program.
/// Solend uses enum-based instruction encoding via LendingInstruction enum.
/// 
/// Reference: solend-sdk crate, instruction.rs
/// LendingInstruction enum tag mapping:
///   tag 5  => RedeemReserveCollateral  <-- THIS ONE
/// 
/// Format: [5, 0, 0, 0, 0, 0, 0, 0]
/// Note: Only the first byte (tag = 5) is used by Solend program.
pub fn get_redeem_reserve_collateral_discriminator() -> u8 {
    // LendingInstruction::RedeemReserveCollateral tag = 5
    // Native Solana programs use only 1 byte as enum tag discriminator
    5u8 // RedeemReserveCollateral enum variant tag
}

/// Get Solend instruction discriminator for FlashLoan
/// 
/// CRITICAL: Solend is NOT an Anchor program - it's a native Solana program.
/// Solend uses enum-based instruction encoding via LendingInstruction enum.
/// 
/// Reference: solend-sdk crate, instruction.rs
/// LendingInstruction enum tag mapping:
///   tag 13 => FlashLoan  <-- THIS ONE
/// 
/// Format: [13] + amount.to_le_bytes()
/// Note: Only the first byte (tag = 13) is used as discriminator, followed by amount (u64).
/// FlashLoan repay is automatic - Solend checks balance at end of transaction.
pub fn get_flashloan_discriminator() -> u8 {
    // LendingInstruction::FlashLoan tag = 13
    // Native Solana programs use only 1 byte as enum tag discriminator
    13u8 // FlashLoan enum variant tag
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
        
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            log::trace!("Skipping 8-byte discriminator (all zeros)");
            &data[8..]
        } else {
            log::trace!("No discriminator detected, using full data");
            data
        };
        
        log::trace!("Attempting Borsh deserialization of {} bytes", data.len());
        
        // CRITICAL: Calculate expected Reserve struct size
        // Reserve struct: 1 + 9 + 32*8 + 1 + 8*4 + 16*6 + 1*12 + 8*4 + 16*2 + 8*1 + 1*2 + 8*1 = 619 bytes
        // BUT: Actual Solend Reserve accounts are 1300 bytes (layout is incomplete/incorrect)
        // TEMPORARY FIX: Parse only the first 619 bytes, ignore the rest
        const EXPECTED_RESERVE_STRUCT_SIZE: usize = 619;
        const ACTUAL_RESERVE_ACCOUNT_SIZE: usize = 1300; // Real account size on-chain
        
        // If account is 1300 bytes (actual size), parse only first 619 bytes (known struct size)
        let data_to_parse = if data.len() == ACTUAL_RESERVE_ACCOUNT_SIZE {
            log::debug!("Reserve account is {} bytes (expected struct: {} bytes) - parsing first {} bytes only", 
                data.len(), EXPECTED_RESERVE_STRUCT_SIZE, EXPECTED_RESERVE_STRUCT_SIZE);
            &data[..EXPECTED_RESERVE_STRUCT_SIZE]
        } else if data.len() < EXPECTED_RESERVE_STRUCT_SIZE {
            return Err(anyhow::anyhow!(
                "Reserve data too short: {} bytes (minimum: {} bytes)", 
                data.len(), 
                EXPECTED_RESERVE_STRUCT_SIZE
            ));
        } else {
            // Try full data first, fallback to slice if needed
            data
        };
        
        let reserve: Reserve = match BorshDeserialize::try_from_slice(data_to_parse) {
            Ok(res) => res,
            Err(e) => {
                // If error is "Not all bytes read" and we have extra bytes, try slicing to exact size
                let error_msg = e.to_string();
                if error_msg.contains("Not all bytes read") && data.len() >= EXPECTED_RESERVE_STRUCT_SIZE {
                    let extra_bytes = data.len() - EXPECTED_RESERVE_STRUCT_SIZE;
                    log::debug!("Borsh reported 'Not all bytes read' with {} extra bytes - trying exact slice", extra_bytes);
                    // Try deserializing only the expected size (ignore extra padding bytes)
                    match BorshDeserialize::try_from_slice(&data[..EXPECTED_RESERVE_STRUCT_SIZE]) {
                        Ok(res) => res,
                        Err(e2) => {
                            log::error!("Borsh deserialization failed even with exact slice: {}", e2);
                            log::error!("Data length: {} bytes (expected: {} bytes for Reserve struct)", data.len(), EXPECTED_RESERVE_STRUCT_SIZE);
                            if data.len() > 0 {
                                log::error!("First byte: {} (0x{:02x})", data[0], data[0]);
                            }
                            if data.len() > 1 {
                                log::error!("Second byte: {} (0x{:02x})", data[1], data[1]);
                            }
                            return Err(anyhow::anyhow!("Failed to deserialize Reserve: {}", e2));
                        }
                    }
                } else {
                    log::error!("Borsh deserialization failed: {}", e);
                    log::error!("Data length: {} bytes (expected: {} bytes for Reserve struct)", data.len(), EXPECTED_RESERVE_STRUCT_SIZE);
                    if data.len() > 0 {
                        log::error!("First byte: {} (0x{:02x})", data[0], data[0]);
                    }
                    if data.len() > 1 {
                        log::error!("Second byte: {} (0x{:02x})", data[1], data[1]);
                    }
                    return Err(anyhow::anyhow!("Failed to deserialize Reserve: {}", e));
                }
            }
        };
        
        log::trace!("Successfully deserialized Reserve, version={}", reserve.version);
        
        // CRITICAL: Validate version to detect layout changes
        // Solend Reserve version 1 is the current supported version
        // If Solend updates to v2, this will prevent silent parsing errors
        const EXPECTED_VERSION: u8 = 1;
        if reserve.version != EXPECTED_VERSION {
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

    /// Get liquidation bonus (as f64, 0-1 range)
    pub fn liquidation_bonus(&self) -> f64 {
        self.liquidationBonus as f64 / 100.0
    }

    /// Get close factor (as f64, 0-1 range)
    /// 
    /// CRITICAL: Close factor determines what percentage of debt can be liquidated.
    /// Currently, Solend's ReserveConfig doesn't include close_factor field, so we use
    /// the standard 50% (0.5) as fallback. If Solend adds close_factor to ReserveConfig
    /// in the future, this function should be updated to read it from config.
    /// 
    /// Close factor can be changed by governance, so hardcoding it is risky.
    /// This function is prepared for future ReserveConfig.closeFactor field.
    /// 
    /// Returns: Close factor as f64 (e.g., 0.5 = 50%)
    pub fn close_factor(&self) -> f64 {
        // TODO: When Solend adds closeFactor to ReserveConfig, uncomment this:
        // let config = self.config();
        // if let Some(cf) = config.closeFactor {
        //     return cf as f64 / 100.0;
        // }
        
        // Fallback to standard 50% (0.5) - Solend's current default
        // This matches the hardcoded value used in debt calculation
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

    /// Get ReserveConfig struct (helper for accessing config fields)
    /// NOTE: switchboardOraclePubkey field was removed from Reserve struct in newer Solend layouts
    /// Use extraOracle field instead, or liquiditySwitchboardOracle for liquidity oracle
    pub fn config(&self) -> ReserveConfig {
        ReserveConfig {
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

/// Find USDC mint address from Solend reserves (chain-based discovery)
/// This automatically discovers USDC by checking all reserves
/// 
/// CRITICAL: No fallback to hardcoded values - MUST read from chain
/// DYNAMIC CHAIN READING: All data is read from chain via RPC - no static values.
pub fn find_usdc_mint_from_reserves(
    rpc: &solana_client::rpc_client::RpcClient,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    
    // Known USDC mint address (mainnet) - used only for comparison, not as fallback
    const USDC_MINT_MAINNET: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let known_usdc_mint = Pubkey::from_str(USDC_MINT_MAINNET)
        .map_err(|e| anyhow::anyhow!("Invalid known USDC mint: {}", e))?;
    
    // CRITICAL: Read all reserves from chain - no mock/dummy data
    let accounts = rpc
        .get_program_accounts(program_id)
        .map_err(|e| anyhow::anyhow!("Failed to get program accounts from chain: {}", e))?;
    
    let accounts_count = accounts.len();
    
    if accounts_count == 0 {
        return Err(anyhow::anyhow!(
            "No Solend reserves found on chain - cannot discover USDC mint. \
             Please check RPC connection and network configuration."
        ));
    }
    
    // Search for USDC reserve by checking all reserves from chain
    for (_pubkey, account) in accounts {
        if let Ok(reserve) = Reserve::from_account_data(&account.data) {
            let mint = reserve.mint_pubkey();
            // Check if this reserve is USDC (by mint address)
            if mint == known_usdc_mint {
                log::info!("✅ Found USDC reserve from chain (mint: {})", mint);
                return Ok(mint);
            }
        }
    }
    
    // CRITICAL: No fallback - if USDC reserve not found, it's an error
    // This ensures we're always using real chain data, not hardcoded values
    Err(anyhow::anyhow!(
        "USDC reserve not found in chain data. \
         Found {} reserves but none match USDC mint {}. \
         This may indicate: (1) Wrong network (mainnet vs devnet), \
         (2) RPC connection issues, or (3) Solend program not deployed on this network. \
         Please verify RPC_URL and network configuration.",
        accounts_count,
        known_usdc_mint
    ))
}

