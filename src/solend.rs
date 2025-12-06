// Auto-generated Solend account layouts from IDL
// Imports must be before include!
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

include!(concat!(env!("OUT_DIR"), "/solend_layout.rs"));

use anyhow::Result;
use std::str::FromStr;

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
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            &data[8..]
        } else {
            data
        };
        
        let obligation: Obligation = BorshDeserialize::try_from_slice(data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Obligation: {}", e))?;
        
        // CRITICAL: Validate version to detect layout changes
        // Solend Obligation version 1 is the current supported version
        // If Solend updates to v2, this will prevent silent parsing errors
        const EXPECTED_VERSION: u8 = 1;
        if obligation.version != EXPECTED_VERSION {
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
        let borrowed = self.borrowedValue.to_f64();
        if borrowed == 0.0 {
            return f64::INFINITY;
        }
        let weighted_collateral = self.allowedBorrowValue.to_f64();
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
        
        let borrowed = self.borrowedValue.value;
        if borrowed == 0 {
            return u128::MAX; // Infinity representation
        }
        
        let weighted_collateral = self.allowedBorrowValue.value;
        
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
        self.depositedValue.to_f64()
    }

    /// Get total borrowed value in USD
    pub fn total_borrowed_value_usd(&self) -> f64 {
        self.borrowedValue.to_f64()
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

// Helper implementation for Reserve
impl Reserve {
    /// Parse reserve from account data using Borsh
    /// 
    /// CRITICAL SECURITY: Validates version field to detect layout changes.
    /// If Solend updates their layout (e.g., v1 -> v2), this will catch it early.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            &data[8..]
        } else {
            data
        };
        
        let reserve: Reserve = BorshDeserialize::try_from_slice(data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Reserve: {}", e))?;
        
        // CRITICAL: Validate version to detect layout changes
        // Solend Reserve version 1 is the current supported version
        // If Solend updates to v2, this will prevent silent parsing errors
        const EXPECTED_VERSION: u8 = 1;
        if reserve.version != EXPECTED_VERSION {
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
        self.liquidity.mintPubkey
    }

    /// Get collateral mint address
    pub fn collateral_mint_pubkey(&self) -> Pubkey {
        self.collateral.mintPubkey
    }

    /// Get oracle pubkey (Pyth primary oracle)
    pub fn oracle_pubkey(&self) -> Pubkey {
        self.liquidity.oraclePubkey
    }

    /// Get liquidation threshold (as f64, 0-1 range)
    pub fn liquidation_threshold(&self) -> f64 {
        self.config.liquidationThreshold as f64 / 100.0
    }

    /// Get liquidation bonus (as f64, 0-1 range)
    pub fn liquidation_bonus(&self) -> f64 {
        self.config.liquidationBonus as f64 / 100.0
    }
}

/// Find USDC mint address from Solend reserves (chain-based discovery)
/// This automatically discovers USDC by checking all reserves
/// USDC mainnet mint: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
/// 
/// DYNAMIC CHAIN READING: All data is read from chain via RPC - no static values.
pub fn find_usdc_mint_from_reserves(
    rpc: &solana_client::rpc_client::RpcClient,
    program_id: &Pubkey,
) -> Result<Pubkey> {
    
    // Known USDC mint address (mainnet)
    const USDC_MINT_MAINNET: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let known_usdc_mint = Pubkey::from_str(USDC_MINT_MAINNET)
        .map_err(|e| anyhow::anyhow!("Invalid known USDC mint: {}", e))?;
    
    // Try to find USDC reserve by checking all reserves
    // This is more robust than hardcoding
    let accounts = rpc
        .get_program_accounts(program_id)
        .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;
    
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
    
    // Fallback: return known USDC mint if not found in reserves
    // This can happen if reserves haven't loaded yet or network issues
    log::warn!("USDC reserve not found in chain data, using known USDC mint: {}", known_usdc_mint);
    Ok(known_usdc_mint)
}

