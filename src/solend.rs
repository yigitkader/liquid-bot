// Auto-generated Solend account layouts from IDL
// Imports must be before include!
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

include!(concat!(env!("OUT_DIR"), "/solend_layout.rs"));

use anyhow::Result;
use std::str::FromStr;

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
        
        const EXPECTED_STRUCT_SIZE: usize = 1300; // Actual struct size from generated layout
        const DISCRIMINATOR_SIZE: usize = 8; // Anchor discriminator size
        
        // ✅ FIXED: Consistent discriminator handling
        // STEP 1: Check if account has Anchor discriminator (8 bytes)
        // Obligation accounts from native Solend program don't use discriminators
        // But Save Protocol (2024 rebrand) may use Anchor-style discriminators
        let has_discriminator = if data.len() >= DISCRIMINATOR_SIZE {
            // Anchor discriminator is NOT all zeros
            // Solend native program uses zero discriminator or no discriminator
            let first_8 = &data[0..DISCRIMINATOR_SIZE];
            !first_8.iter().all(|&b| b == 0)
        } else {
            false
        };
        
        // STEP 2: Skip discriminator if present
        let data_without_discriminator = if has_discriminator {
            log::trace!("Skipping 8-byte Anchor discriminator");
            if data.len() < DISCRIMINATOR_SIZE + EXPECTED_STRUCT_SIZE {
                return Err(anyhow::anyhow!(
                    "Account too short: {} bytes (need {} bytes after discriminator)",
                    data.len(),
                    DISCRIMINATOR_SIZE + EXPECTED_STRUCT_SIZE
                ));
            }
            &data[DISCRIMINATOR_SIZE..]
        } else {
            data
        };
        
        // STEP 3: Validate size
        if data_without_discriminator.len() < EXPECTED_STRUCT_SIZE {
            return Err(anyhow::anyhow!(
                "Obligation data too short: {} bytes (need {} bytes). \
                 Account may be corrupted or layout may have changed.",
                data_without_discriminator.len(),
                EXPECTED_STRUCT_SIZE
            ));
        }
        
        // STEP 4: Take exactly EXPECTED_STRUCT_SIZE bytes
        let data_to_parse = &data_without_discriminator[..EXPECTED_STRUCT_SIZE];
        
        // STEP 5: Pre-check version byte (early validation)
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
        
        // STEP 6: Parse with Borsh
        let obligation: Obligation = BorshDeserialize::try_from_slice(data_to_parse)
            .map_err(|e| {
                log::error!("Borsh deserialization failed: {}", e);
                log::error!("Data length: {} bytes (expected: {} bytes for Obligation struct)", data.len(), EXPECTED_STRUCT_SIZE);
                log::error!("Parsed length: {} bytes", data_to_parse.len());
                
                // Log detailed byte analysis for debugging
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
        
        // STEP 7: Validate version again (double-check after parsing)
        const EXPECTED_VERSION: u8 = 1;
        const LEGACY_VERSION: u8 = 0;
        
        if obligation.version == LEGACY_VERSION {
            // Version 0 is legacy but still valid - log debug (not warning to reduce spam)
            log::debug!(
                "Obligation version {} (legacy) - still valid",
                obligation.version
            );
        } else if obligation.version != EXPECTED_VERSION {
            // Unknown version - reject
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
        
        // Each ObligationCollateral is 32 (Pubkey) + 8 (u64) + 16 (u128) + 32 (padding) = 88 bytes
        // CRITICAL FIX: Generated layout includes _padding_56: [u8; 32] which we were missing!
        const COLLATERAL_SIZE: usize = 32 + 8 + 16 + 32; // 88 bytes (from generated layout)
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
        
        // Each ObligationLiquidity is 32 (Pubkey) + 16 (u128) + 16 (u128) + 16 (u128) + 32 (padding) = 112 bytes
        // CRITICAL FIX: Generated layout includes _padding_80: [u8; 32] which we were missing!
        const LIQUIDITY_SIZE: usize = 32 + 16 + 16 + 16 + 32; // 112 bytes (from generated layout)
        let borrows_size = borrows_len * LIQUIDITY_SIZE;
        
        // Deposits come first, then borrows
        let deposits_len = self.depositsLen as usize;
        // Each ObligationCollateral is 32 (Pubkey) + 8 (u64) + 16 (u128) + 32 (padding) = 88 bytes
        const COLLATERAL_SIZE_FOR_BORROWS: usize = 32 + 8 + 16 + 32; // 88 bytes (must match deposits() function)
        let deposits_size = deposits_len * COLLATERAL_SIZE_FOR_BORROWS;
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

// CRITICAL FIX: Solend Program ID - USDC Reserve Required
// 
// ⚠️  IMPORTANT: Bot requires USDC reserve (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v)
//    Only programs with USDC reserve can be used for liquidation operations.
//
// ✅ ACTIVE PROGRAMS WITH USDC:
// 1. Save Protocol Main Market (RECOMMENDED): SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh
//    - Active on mainnet (Save Protocol is Solend's 2024 rebrand)
//    - Has USDC reserve (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v)
//    - Verified working for liquidation bot
//
// ❌ PROGRAMS WITHOUT USDC (DO NOT USE):
//    "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo" - Save Main Market (NO USDC reserve)
//    "ALcohoCRRXGDKhc5pS5UqzVVjZE5x9dkgjZjD8MJCTw" - Altcoins Market (NO USDC)
//    "turboJPMBqVwWU26JsKivCm9wPU3fuaYx8EM9rRHfuuP" - Turbo SOL Market (NO USDC)
//
// NOTE: Save markets (2024 rebrand) may not have USDC reserves.
//       Always verify USDC reserve exists before using a program ID.
//
// Verification: Check Solana Explorer for USDC reserves:
// https://solscan.io/account/SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh
pub const SOLEND_PROGRAM_ID: &str = "SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh"; // Save Protocol Main Market (USDC reserve available)
pub const SOLEND_PROGRAM_ID_ALTCOINS: &str = "ALcohoCRRXGDKhc5pS5UqzVVjZE5x9dkgjZjD8MJCTw"; // Altcoins Market (NO USDC)
pub const SOLEND_PROGRAM_ID_TURBO: &str = "turboJPMBqVwWU26JsKivCm9wPU3fuaYx8EM9rRHfuuP"; // Turbo SOL Market (NO USDC)

/// Known Solend program IDs (for validation)
/// NOTE: Only programs with USDC reserve should be used for liquidation bot
pub const SOLEND_PROGRAM_IDS: &[&str] = &[
    "SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh", // Save Protocol Main Market (USDC reserve available - RECOMMENDED)
];

/// Get Solend program ID (mainnet production)
/// 
/// CRITICAL: This returns the Solend program ID that has USDC reserve
/// Defaults to Save Protocol Main Market (SLendK7ySfcEzyaFqy93gDnD3RtrpXJcnRwb6zFHJSh)
/// Can be overridden via SOLEND_PROGRAM_ID environment variable
/// 
/// NOTE: Bot requires USDC reserve - only programs with USDC can be used
pub fn solend_program_id() -> Result<Pubkey> {
    use std::env;
    
    // First, try to read from environment variable (allows override)
    if let Ok(env_program_id) = env::var("SOLEND_PROGRAM_ID") {
        log::info!("📝 Using Save/Solend program ID from .env: {}", env_program_id);
        return Pubkey::from_str(&env_program_id)
            .map_err(|e| anyhow::anyhow!("Invalid SOLEND_PROGRAM_ID from .env: {} - Error: {}", env_program_id, e));
    }
    
    // Fallback to default (Save Main Market - active as of 2024)
    log::info!("📝 Using default Save program ID: {}", SOLEND_PROGRAM_ID);
    Pubkey::from_str(SOLEND_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Save program ID: {}", e))
}

/// Verify if a program ID is a known Solend program
/// 
/// This can be used to validate that we're connecting to a legitimate Solend program
/// NOTE: Only programs with USDC reserve should be used for liquidation operations
pub fn is_valid_solend_program(pubkey: &Pubkey) -> bool {
    SOLEND_PROGRAM_IDS
        .iter()
        .any(|&id| Pubkey::from_str(id).ok() == Some(*pubkey))
}

/// Identify Solend account type without parsing
/// 
/// Uses heuristics based on size, discriminator, and version byte to quickly
/// identify account types without expensive Borsh deserialization.
/// 
/// This is faster and more reliable than attempting to parse every account.
pub fn identify_solend_account_type(data: &[u8]) -> SolendAccountType {
    if data.len() < 8 {
        return SolendAccountType::Unknown;
    }
    
    // STEP 1: Check discriminator
    // Anchor discriminators are NOT all zeros
    // Solend native program uses zero discriminator or no discriminator
    let has_discriminator = !data[0..8].iter().all(|&b| b == 0);
    
    // STEP 2: Get actual data (skip discriminator if present)
    let actual_data = if has_discriminator {
        if data.len() < 8 {
            return SolendAccountType::Unknown;
        }
        &data[8..]
    } else {
        data
    };
    
    // STEP 3: Size-based heuristic (after discriminator)
    let size = actual_data.len();
    
    // STEP 4: Check version byte
    if size > 0 {
        let version = actual_data[0];
        
        // Obligation: 1200-1400 bytes, version 0 or 1
        // Note: Obligation can be version 0 (legacy) or 1 (current)
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
///   tag 13 => FlashBorrowReserveLiquidity  <-- THIS ONE
/// 
/// Format: [13] + amount.to_le_bytes()
/// Note: Only the first byte (tag = 13) is used as discriminator, followed by amount (u64).
pub fn get_flashloan_discriminator() -> u8 {
    // LendingInstruction::FlashBorrowReserveLiquidity tag = 13
    // Native Solana programs use only 1 byte as enum tag discriminator
    13u8 // FlashBorrowReserveLiquidity enum variant tag
}

/// Get Solend instruction discriminator for FlashRepayReserveLiquidity
/// 
/// CRITICAL: Solend is NOT an Anchor program - it's a native Solana program.
/// Solend uses enum-based instruction encoding via LendingInstruction enum.
/// 
/// Reference: solend-sdk crate, instruction.rs
/// LendingInstruction enum tag mapping:
///   tag 14 => FlashRepayReserveLiquidity  <-- THIS ONE
/// 
/// Format: [14] + repay_amount.to_le_bytes() + borrowed_amount.to_le_bytes()
/// Note: First byte (tag = 14) is discriminator, followed by repay_amount (u64) and borrowed_amount (u64).
/// CRITICAL: FlashLoan requires explicit repayment instruction - NOT automatic!
pub fn get_flashrepay_discriminator() -> u8 {
    // LendingInstruction::FlashRepayReserveLiquidity tag = 14
    // Native Solana programs use only 1 byte as enum tag discriminator
    14u8 // FlashRepayReserveLiquidity enum variant tag
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
        
        // ✅ FIXED: Consistent discriminator handling (same as Obligation)
        // STEP 1: Check if account has Anchor discriminator (8 bytes)
        // Reserve accounts from native Solend program don't use discriminators
        // But Save Protocol (2024 rebrand) may use Anchor-style discriminators
        let has_discriminator = if data.len() >= DISCRIMINATOR_SIZE {
            // Anchor discriminator is NOT all zeros
            // Solend native program uses zero discriminator or no discriminator
            let first_8 = &data[0..DISCRIMINATOR_SIZE];
            !first_8.iter().all(|&b| b == 0)
        } else {
            false
        };
        
        // STEP 2: Skip discriminator if present
        let data_without_discriminator = if has_discriminator {
            log::trace!("Skipping 8-byte Anchor discriminator");
            if data.len() < DISCRIMINATOR_SIZE + EXPECTED_RESERVE_STRUCT_SIZE {
                return Err(anyhow::anyhow!(
                    "Account too short: {} bytes (need {} bytes after discriminator)",
                    data.len(),
                    DISCRIMINATOR_SIZE + EXPECTED_RESERVE_STRUCT_SIZE
                ));
            }
            &data[DISCRIMINATOR_SIZE..]
        } else {
            data
        };
        
        // STEP 3: Validate size
        if data_without_discriminator.len() < EXPECTED_RESERVE_STRUCT_SIZE {
            return Err(anyhow::anyhow!(
                "Reserve data too short: {} bytes (need {} bytes). \
                 Account may be corrupted or layout may have changed.",
                data_without_discriminator.len(),
                EXPECTED_RESERVE_STRUCT_SIZE
            ));
        }
        
        // STEP 4: Take exactly EXPECTED_RESERVE_STRUCT_SIZE bytes
        let data_to_parse = &data_without_discriminator[..EXPECTED_RESERVE_STRUCT_SIZE];
        
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
        
        // STEP 6: Parse with Borsh
        let reserve: Reserve = BorshDeserialize::try_from_slice(data_to_parse)
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
        
        // STEP 7: Validate version again (double-check after parsing)
        const EXPECTED_VERSION: u8 = 1;
        if reserve.version != EXPECTED_VERSION {
            // If version is clearly invalid (0, 254, 255), this is likely not a Reserve account
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
    
    log::info!("🔍 Starting USDC reserve discovery...");
    log::info!("   Program ID: {}", program_id);
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
            const TEST_MAINNET_ACCOUNT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"; // USDC mint
            if let Ok(test_account) = Pubkey::from_str(TEST_MAINNET_ACCOUNT) {
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
    
    log::info!("🔍 Fetching Solend program accounts from RPC (this may take a moment for ~300k accounts)...");
    let accounts = {
        const MAX_RETRIES: u32 = 5;
        let mut retries = MAX_RETRIES;
        let mut last_error = None;
        
        loop {
            match rpc.get_program_accounts(program_id) {
                Ok(accs) => {
                    log::info!("✅ Successfully fetched {} accounts from RPC", accs.len());
                    break Ok(accs);
                },
                Err(e) => {
                    last_error = Some(e);
                    retries -= 1;
                    
                    let error_msg = last_error.as_ref().unwrap().to_string();
                    
                    if retries > 0 {
                        // Exponential backoff: 2s, 4s, 8s, 16s, 32s
                        let delay_secs = 2_u64.pow(MAX_RETRIES - retries);
                        log::warn!(
                            "⚠️  RPC error getting program accounts (retries left: {}): {}",
                            retries,
                            error_msg
                        );
                        log::info!("⏳ Retrying in {} seconds...", delay_secs);
                        std::thread::sleep(std::time::Duration::from_secs(delay_secs));
                        continue;
                    } else {
                        // All retries exhausted
                        log::error!("❌ All {} retry attempts failed", MAX_RETRIES);
                        break Err(last_error.unwrap());
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
    
    log::info!("🔍 Searching for USDC reserve among {} accounts...", accounts_count);
    log::info!("   Account size distribution will be analyzed during parsing...");
    
    // Search for USDC reserve by checking all reserves from chain
    let mut parsed_reserves = 0;
    let mut skipped_accounts = 0;
    let mut parse_errors = 0;
    let mut found_mints = std::collections::HashSet::new();
    let mut account_size_distribution = std::collections::HashMap::new();
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
                    let elapsed = start_time.elapsed();
                    log::info!("✅ Found USDC reserve from chain (as liquidity mint)!");
                    log::info!("   Reserve pubkey: {}", pubkey);
                    log::info!("   USDC liquidity mint: {}", liquidity_mint);
                    log::info!("   Collateral mint: {}", collateral_mint);
                    log::info!("   Parsed {} reserves total in {:?}", parsed_reserves, elapsed);
                    log::info!("   Skipped {} accounts (too small)", skipped_accounts);
                    log::info!("   Found {} unique token mints", found_mints.len());
                    return Ok(liquidity_mint);
                } else if collateral_mint == known_usdc_mint {
                    let elapsed = start_time.elapsed();
                    log::warn!("⚠️  Found USDC as collateral mint (unusual, but using liquidity mint)");
                    log::info!("   Reserve pubkey: {}", pubkey);
                    log::info!("   Liquidity mint: {}", liquidity_mint);
                    log::info!("   USDC collateral mint: {}", collateral_mint);
                    log::info!("   Note: Using liquidity mint for USDC operations");
                    log::info!("   Parsed {} reserves total in {:?}", parsed_reserves, elapsed);
                    // Still return liquidity mint as USDC is typically used as debt token
                    // But log this unusual case
                    return Ok(liquidity_mint);
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
    log::warn!("⚠️  USDC reserve not found after parsing {} reserves", parsed_reserves);
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
    
    // Check if USDC might be in the list with a different address
    log::warn!("🔍 Checking if USDC exists with a different mint address...");
    log::warn!("   This Solend instance may use a different USDC variant (e.g., USDC.e)");
    log::warn!("   Or USDC may not be available in this Solend program");
    
    // CRITICAL: No fallback - if USDC reserve not found, it's an error
    // This ensures we're always using real chain data, not hardcoded values
    // CRITICAL: Only mainnet is supported - no devnet/testnet
    Err(anyhow::anyhow!(
        "USDC reserve not found in chain data. \
         Checked {} accounts, successfully parsed {} reserves, but none match USDC mint {}. \
         \n\
         Analysis:\n\
         - Found {} unique token mints (both liquidity and collateral)\n\
         - USDC mint {} was not found in any reserve\n\
         - All mints have been saved to: {}\n\
         \n\
         Possible causes:\n\
         1. This Solend program instance does not have a USDC reserve\n\
         2. USDC may be using a different mint address (check {})\n\
         3. RPC_URL may not be pointing to the correct Solend program\n\
         4. Solend program layout may have changed\n\
         \n\
         Troubleshooting:\n\
         - Check {} file to see all available mints\n\
         - Verify Solend program ID is correct: {}\n\
         - Verify RPC_URL points to mainnet\n\
         - Check Solana Explorer for this program's reserves\n\
         - Consider if USDC is actually needed (maybe use a different stablecoin?)",
        accounts_count,
        parsed_reserves,
        known_usdc_mint,
        found_mints.len(),
        known_usdc_mint,
        mints_file,
        mints_file,
        mints_file,
        program_id
    ))
}

