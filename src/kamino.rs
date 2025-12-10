// Kamino Lend (klend) account layouts
// Based on: https://github.com/Kamino-Finance/klend
// Program ID (mainnet): KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;
use anyhow::Result;

/// Kamino Lend Program ID (mainnet)
pub const KAMINO_LEND_PROGRAM_ID: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";

/// Anchor discriminator size
const DISCRIMINATOR_SIZE: usize = 8;

/// Scale factor for u128 values (similar to WAD but Kamino uses different precision)
/// Kamino uses "sf" suffix for scale factor fields
pub const SCALE_FACTOR: u128 = 1_000_000_000_000_000; // 10^15

// =============================================================================
// BASIC TYPES
// =============================================================================

/// LastUpdate - tracks when account was last updated
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8,
    pub placeholder: [u8; 7], // padding to 16 bytes
}

/// BigFractionBytes - large fraction representation (used in BorrowRateCurve)
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BigFractionBytes {
    pub value: [u64; 4], // 256-bit value
    pub padding: [u64; 2],
}

/// CurvePoint - point on the borrow rate curve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct CurvePoint {
    pub utilization_rate_bps: u32,
    pub borrow_rate_bps: u32,
}

/// BorrowRateCurve - defines interest rate curve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct BorrowRateCurve {
    pub points: [CurvePoint; 11],
}

/// TokenInfo - token metadata in reserve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct TokenInfo {
    pub name: [u8; 32],
    pub heuristic: PriceHeuristic,
    pub max_twap_divergence_bps: u64,
    pub max_age_price_seconds: u64,
    pub max_age_twap_seconds: u64,
    pub scope_configuration: ScopeConfiguration,
    pub switchboard_configuration: SwitchboardConfiguration,
    pub pyth_configuration: PythConfiguration,
    pub block_price_usage: u8,
    pub reserved: [u8; 7],
    pub padding: [u64; 19],
}

/// PriceHeuristic
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct PriceHeuristic {
    pub lower: u64,
    pub upper: u64,
    pub exp: u64,
}

/// ScopeConfiguration
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ScopeConfiguration {
    pub price_feed: Pubkey,
    pub price_chain: [u16; 4],
    pub twap_chain: [u16; 4],
}

/// SwitchboardConfiguration
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct SwitchboardConfiguration {
    pub price_aggregator: Pubkey,
    pub twap_aggregator: Pubkey,
}

/// PythConfiguration
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct PythConfiguration {
    pub price: Pubkey,
}

/// WithdrawalCaps - limits for withdrawals
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct WithdrawalCaps {
    pub config_capacity: i64,
    pub current_total: i64,
    pub last_interval_start_timestamp: u64,
    pub config_interval_length_seconds: u64,
}

/// ReserveFees
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveFees {
    pub borrow_fee_sf: u64,
    pub flash_loan_fee_sf: u64,
    pub padding: [u8; 8],
}

/// ElevationGroup - risk grouping for reserves
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ElevationGroup {
    pub max_liquidation_bonus_bps: u16,
    pub id: u8,
    pub ltv_pct: u8,
    pub liquidation_threshold_pct: u8,
    pub allow_new_loans: u8,
    pub reserved: [u8; 2],
    pub debt_reserve: Pubkey,
    pub padding: [u64; 4],
}

// =============================================================================
// RESERVE NESTED STRUCTS
// =============================================================================

/// ReserveLiquidity - liquidity state of a reserve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveLiquidity {
    pub mint_pubkey: Pubkey,
    pub supply_vault: Pubkey,
    pub fee_vault: Pubkey,
    pub available_amount: u64,
    pub borrowed_amount_sf: u128,
    pub market_price_sf: u128,
    pub market_price_last_updated_ts: u64,
    pub mint_decimals: u64,
    pub deposit_limit_crossed_slot: u64,
    pub borrow_limit_crossed_slot: u64,
    pub cumulative_borrow_rate_bsf: BigFractionBytes,
    pub accumulated_protocol_fees_sf: u128,
    pub accumulated_referrer_fees_sf: u128,
    pub pending_referrer_fees_sf: u128,
    pub absolute_referral_rate_sf: u128,
    pub token_program: Pubkey,
    pub padding2: [u64; 51],
    pub padding3: [u128; 32],
}

/// ReserveCollateral - collateral state of a reserve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub mint_total_supply: u64,
    pub supply_vault: Pubkey,
    pub padding1: [u128; 32],
    pub padding2: [u128; 32],
}

/// ReserveConfig - configuration parameters for a reserve
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ReserveConfig {
    pub status: u8,
    pub asset_tier: u8,
    pub host_fixed_interest_rate_bps: u16,
    pub reserved2: [u8; 2],
    pub reserved3: [u8; 8],
    
    pub protocol_take_rate_pct: u8,
    pub protocol_liquidation_fee_pct: u8,
    pub loan_to_value_pct: u8,
    pub liquidation_threshold_pct: u8,
    
    pub min_liquidation_bonus_bps: u16,
    pub max_liquidation_bonus_bps: u16,
    pub bad_debt_liquidation_bonus_bps: u16,
    
    pub deleveraging_margin_call_period_secs: u64,
    pub deleveraging_threshold_slots_per_bps: u64,
    
    pub fees: ReserveFees,
    pub borrow_rate_curve: BorrowRateCurve,
    
    pub borrow_factor_pct: u64,
    pub deposit_limit: u64,
    pub borrow_limit: u64,
    
    pub token_info: TokenInfo,
    pub deposit_withdrawal_cap: WithdrawalCaps,
    pub debt_withdrawal_cap: WithdrawalCaps,
    
    pub elevation_groups: [u8; 20],
    pub disable_usage_as_coll_outside_emode: u8,
    pub utilization_limit_block_borrowing_above: u8,
    pub reserved1: [u8; 2],
    
    pub borrow_limit_outside_elevation_group: u64,
    pub borrow_limit_against_this_collateral_in_elevation_group: [u64; 32],
}

// =============================================================================
// OBLIGATION NESTED STRUCTS
// =============================================================================

/// ObligationCollateral - single deposit position in an obligation
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ObligationCollateral {
    pub deposit_reserve: Pubkey,
    pub deposited_amount: u64,
    pub market_value_sf: u128,
    pub padding: [u64; 10],
}

/// ObligationLiquidity - single borrow position in an obligation
#[derive(Debug, Clone, Copy, BorshDeserialize, BorshSerialize)]
pub struct ObligationLiquidity {
    pub borrow_reserve: Pubkey,
    pub cumulative_borrow_rate_bsf: BigFractionBytes,
    pub padding: u64,
    pub borrowed_amount_sf: u128,
    pub market_value_sf: u128,
    pub borrow_factor_adjusted_market_value_sf: u128,
    pub padding2: [u64; 8],
}

// =============================================================================
// MAIN ACCOUNT STRUCTS
// =============================================================================

/// Kamino LendingMarket account
#[derive(Debug, Clone, BorshDeserialize, BorshSerialize)]
pub struct LendingMarket {
    pub version: u64,
    pub bump_seed: u64,
    pub lending_market_owner: Pubkey,
    pub lending_market_owner_cached: Pubkey,
    pub quote_currency: [u8; 32],
    pub referral_fee_bps: u16,
    
    pub emergency_mode: u8,
    pub autodeleverage_enabled: u8,
    pub borrow_disabled: u8,
    pub price_refresh_trigger_to_max_age_pct: u8,
    pub liquidation_max_debt_close_factor_pct: u8,
    pub insolvency_risk_unhealthy_ltv_pct: u8,
    
    pub min_full_liquidation_value_threshold: u64,
    pub max_liquidatable_debt_market_value_at_once: u64,
    pub global_unhealthy_borrow_value: u64,
    pub global_allowed_borrow_value: u64,
    
    pub risk_council: Pubkey,
    pub reserved1: [u8; 8],
    
    pub elevation_groups: [ElevationGroup; 32],
    pub elevation_group_padding: [u64; 90],
    
    pub min_net_value_in_obligation_sf: u128,
    pub min_value_skip_liquidation_ltv_bf_checks: u64,
    
    pub name: [u8; 32],
    pub padding1: [u64; 173],
}

/// Kamino Reserve account
#[derive(Debug, Clone, BorshDeserialize, BorshSerialize)]
pub struct Reserve {
    pub version: u64,
    pub last_update: LastUpdate,
    pub lending_market: Pubkey,
    pub farm_collateral: Pubkey,
    pub farm_debt: Pubkey,
    
    pub liquidity: ReserveLiquidity,
    pub reserve_liquidity_padding: [u64; 150],
    
    pub collateral: ReserveCollateral,
    pub reserve_collateral_padding: [u64; 150],
    
    pub config: ReserveConfig,
    pub config_padding: [u64; 117],
    
    pub borrowed_amount_outside_elevation_group: u64,
    pub borrowed_amounts_against_this_reserve_in_elevation_groups: [u64; 32],
    
    pub padding: [u64; 207],
}

/// Kamino Obligation account
#[derive(Debug, Clone, BorshDeserialize, BorshSerialize)]
pub struct Obligation {
    pub tag: u64,
    pub last_update: LastUpdate,
    pub lending_market: Pubkey,
    pub owner: Pubkey,
    
    pub deposits: [ObligationCollateral; 8],
    pub lowest_reserve_deposit_liquidation_ltv: u64,
    pub deposited_value_sf: u128,
    
    pub borrows: [ObligationLiquidity; 5],
    pub borrow_factor_adjusted_debt_value_sf: u128,
    pub borrowed_assets_market_value_sf: u128,
    pub allowed_borrow_value_sf: u128,
    pub unhealthy_borrow_value_sf: u128,
    
    pub deposits_asset_tiers: [u8; 8],
    pub borrows_asset_tiers: [u8; 5],
    
    pub elevation_group: u8,
    pub num_of_obsolete_reserves: u8,
    pub has_debt: u8,
    
    pub referrer: Pubkey,
    pub borrowing_disabled: u8,
    pub reserved: [u8; 7],
    
    pub highest_borrow_factor_pct: u64,
    pub padding3: [u64; 126],
}

// =============================================================================
// ACCOUNT TYPE DETECTION
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KaminoAccountType {
    Obligation,
    Reserve,
    LendingMarket,
    Unknown,
}

/// Anchor discriminators for Kamino accounts
/// These are the first 8 bytes of sha256("account:<AccountName>")
pub mod discriminators {
    // Pre-computed discriminators
    pub const OBLIGATION: [u8; 8] = [168, 206, 141, 106, 88, 76, 172, 167];
    pub const RESERVE: [u8; 8] = [43, 242, 204, 202, 26, 247, 218, 208];
    pub const LENDING_MARKET: [u8; 8] = [246, 114, 50, 98, 72, 157, 22, 138];
}

/// Detect Kamino account type from raw data
pub fn detect_account_type(data: &[u8]) -> KaminoAccountType {
    if data.len() < DISCRIMINATOR_SIZE {
        return KaminoAccountType::Unknown;
    }
    
    let disc = &data[0..8];
    
    if disc == discriminators::OBLIGATION {
        KaminoAccountType::Obligation
    } else if disc == discriminators::RESERVE {
        KaminoAccountType::Reserve
    } else if disc == discriminators::LENDING_MARKET {
        KaminoAccountType::LendingMarket
    } else {
        KaminoAccountType::Unknown
    }
}

// =============================================================================
// PARSING IMPLEMENTATIONS
// =============================================================================

impl Obligation {
    /// Parse obligation from account data
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < DISCRIMINATOR_SIZE {
            return Err(anyhow::anyhow!("Data too short for Kamino Obligation"));
        }
        
        // Verify discriminator
        if &data[0..8] != discriminators::OBLIGATION {
            return Err(anyhow::anyhow!(
                "Invalid Kamino Obligation discriminator: {:?}",
                &data[0..8]
            ));
        }
        
        // Skip discriminator and parse
        let obligation_data = &data[DISCRIMINATOR_SIZE..];
        Obligation::try_from_slice(obligation_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse Kamino Obligation: {}", e))
    }
    
    /// Calculate health factor
    /// Health Factor = allowed_borrow_value / borrow_factor_adjusted_debt_value
    pub fn health_factor(&self) -> f64 {
        let allowed = self.allowed_borrow_value_sf as f64 / SCALE_FACTOR as f64;
        let debt = self.borrow_factor_adjusted_debt_value_sf as f64 / SCALE_FACTOR as f64;
        
        if debt == 0.0 {
            return f64::MAX; // No debt = infinite health
        }
        
        allowed / debt
    }
    
    /// Get total deposited value in USD
    pub fn deposited_value_usd(&self) -> f64 {
        self.deposited_value_sf as f64 / SCALE_FACTOR as f64
    }
    
    /// Get total borrowed value in USD
    pub fn borrowed_value_usd(&self) -> f64 {
        self.borrowed_assets_market_value_sf as f64 / SCALE_FACTOR as f64
    }
    
    /// Check if obligation has any debt
    pub fn has_any_debt(&self) -> bool {
        self.has_debt != 0 || self.borrow_factor_adjusted_debt_value_sf > 0
    }
    
    /// Check if obligation has any deposits
    pub fn has_any_deposits(&self) -> bool {
        self.deposited_value_sf > 0
    }
    
    /// Get active deposit count
    pub fn active_deposit_count(&self) -> usize {
        self.deposits.iter()
            .filter(|d| d.deposited_amount > 0)
            .count()
    }
    
    /// Get active borrow count
    pub fn active_borrow_count(&self) -> usize {
        self.borrows.iter()
            .filter(|b| b.borrowed_amount_sf > 0)
            .count()
    }
    
    /// Check if obligation is stale
    pub fn is_stale(&self) -> bool {
        self.last_update.stale != 0
    }
}

// =============================================================================
// LENDING TRAIT IMPLEMENTATION
// =============================================================================

impl crate::lending_trait::LendingObligation for Obligation {
    fn borrowed_value_usd(&self) -> f64 {
        self.borrowed_value_usd()
    }
    
    fn deposited_value_usd(&self) -> f64 {
        self.deposited_value_usd()
    }
    
    fn allowed_borrow_value_usd(&self) -> f64 {
        self.allowed_borrow_value_usd()
    }
    
    fn health_factor(&self) -> f64 {
        self.health_factor()
    }
    
    fn owner(&self) -> Pubkey {
        self.owner
    }
    
    fn lending_market(&self) -> Pubkey {
        self.lending_market
    }
    
    fn has_any_debt(&self) -> bool {
        self.has_any_debt()
    }
    
    fn has_any_deposits(&self) -> bool {
        self.has_any_deposits()
    }
    
    fn is_stale(&self) -> bool {
        self.is_stale()
    }
}

impl Reserve {
    /// Parse reserve from account data
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < DISCRIMINATOR_SIZE {
            return Err(anyhow::anyhow!("Data too short for Kamino Reserve"));
        }
        
        // Verify discriminator
        if &data[0..8] != discriminators::RESERVE {
            return Err(anyhow::anyhow!(
                "Invalid Kamino Reserve discriminator: {:?}",
                &data[0..8]
            ));
        }
        
        // Skip discriminator and parse
        let reserve_data = &data[DISCRIMINATOR_SIZE..];
        Reserve::try_from_slice(reserve_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse Kamino Reserve: {}", e))
    }
    
    /// Get liquidity mint pubkey
    pub fn mint_pubkey(&self) -> Pubkey {
        self.liquidity.mint_pubkey
    }
    
    /// Get collateral mint pubkey
    pub fn collateral_mint_pubkey(&self) -> Pubkey {
        self.collateral.mint_pubkey
    }
    
    /// Get mint decimals
    pub fn mint_decimals(&self) -> u8 {
        self.liquidity.mint_decimals as u8
    }
    
    /// Get loan-to-value ratio
    pub fn ltv_pct(&self) -> u8 {
        self.config.loan_to_value_pct
    }
    
    /// Get liquidation threshold
    pub fn liquidation_threshold_pct(&self) -> u8 {
        self.config.liquidation_threshold_pct
    }
    
    /// Get liquidation bonus (min)
    pub fn liquidation_bonus_bps(&self) -> u16 {
        self.config.min_liquidation_bonus_bps
    }
    
    /// Get Pyth oracle pubkey
    pub fn pyth_oracle(&self) -> Pubkey {
        self.config.token_info.pyth_configuration.price
    }
    
    /// Get Switchboard oracle pubkey
    pub fn switchboard_oracle(&self) -> Pubkey {
        self.config.token_info.switchboard_configuration.price_aggregator
    }
    
    /// Get market price in USD
    pub fn market_price_usd(&self) -> f64 {
        self.liquidity.market_price_sf as f64 / SCALE_FACTOR as f64
    }
    
    /// Get available liquidity
    pub fn available_amount(&self) -> u64 {
        self.liquidity.available_amount
    }
    
    /// Get borrowed amount
    pub fn borrowed_amount(&self) -> f64 {
        self.liquidity.borrowed_amount_sf as f64 / SCALE_FACTOR as f64
    }
    
    /// Check if reserve is active
    pub fn is_active(&self) -> bool {
        self.config.status == 0 // Active status
    }
    
    /// Get close factor (protocol liquidation fee determines close factor behavior)
    pub fn close_factor(&self) -> f64 {
        // Kamino uses liquidation_max_debt_close_factor_pct from LendingMarket
        // Default to 50% if not available
        0.5
    }
}

impl LendingMarket {
    /// Parse lending market from account data
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.len() < DISCRIMINATOR_SIZE {
            return Err(anyhow::anyhow!("Data too short for Kamino LendingMarket"));
        }
        
        // Verify discriminator
        if &data[0..8] != discriminators::LENDING_MARKET {
            return Err(anyhow::anyhow!(
                "Invalid Kamino LendingMarket discriminator: {:?}",
                &data[0..8]
            ));
        }
        
        // Skip discriminator and parse
        let market_data = &data[DISCRIMINATOR_SIZE..];
        LendingMarket::try_from_slice(market_data)
            .map_err(|e| anyhow::anyhow!("Failed to parse Kamino LendingMarket: {}", e))
    }
    
    /// Check if market is in emergency mode
    pub fn is_emergency_mode(&self) -> bool {
        self.emergency_mode != 0
    }
    
    /// Check if borrowing is disabled
    pub fn is_borrow_disabled(&self) -> bool {
        self.borrow_disabled != 0
    }
    
    /// Get max liquidation close factor
    pub fn max_close_factor_pct(&self) -> u8 {
        self.liquidation_max_debt_close_factor_pct
    }
    
    /// Get market name as string
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.name)
            .trim_matches('\0')
            .to_string()
    }
}

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

/// Get Kamino program ID
pub fn get_program_id() -> Pubkey {
    Pubkey::from_str(KAMINO_LEND_PROGRAM_ID)
        .expect("Invalid Kamino program ID")
}

use std::str::FromStr;

/// Check if a program ID is Kamino Lend
pub fn is_kamino_program(program_id: &Pubkey) -> bool {
    program_id.to_string() == KAMINO_LEND_PROGRAM_ID
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_program_id() {
        let pubkey = get_program_id();
        assert_eq!(pubkey.to_string(), KAMINO_LEND_PROGRAM_ID);
    }
    
    #[test]
    fn test_account_type_detection() {
        // Test with obligation discriminator
        let mut data = vec![0u8; 1000];
        data[0..8].copy_from_slice(&discriminators::OBLIGATION);
        assert_eq!(detect_account_type(&data), KaminoAccountType::Obligation);
        
        // Test with reserve discriminator
        data[0..8].copy_from_slice(&discriminators::RESERVE);
        assert_eq!(detect_account_type(&data), KaminoAccountType::Reserve);
        
        // Test with unknown discriminator
        data[0..8].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(detect_account_type(&data), KaminoAccountType::Unknown);
    }
}

// =============================================================================
// INSTRUCTION DISCRIMINATORS
// =============================================================================

/// Anchor instruction discriminators for Kamino Lend
/// These are the first 8 bytes of sha256("global:<instruction_name>")
pub mod instruction_discriminators {
    // TODO: Get actual discriminators from Kamino IDL
    // Placeholder values - need to be updated with actual Kamino instruction discriminators
    // These can be obtained from: anchor build --idl or from klend-sdk
    
    /// RefreshReserve instruction discriminator
    /// sha256("global:refresh_reserve")[:8]
    pub const REFRESH_RESERVE: [u8; 8] = [0; 8]; // TODO: Replace with actual
    
    /// RefreshObligation instruction discriminator
    /// sha256("global:refresh_obligation")[:8]
    pub const REFRESH_OBLIGATION: [u8; 8] = [0; 8]; // TODO: Replace with actual
    
    /// LiquidateObligation instruction discriminator
    /// sha256("global:liquidate_obligation")[:8]
    pub const LIQUIDATE_OBLIGATION: [u8; 8] = [0; 8]; // TODO: Replace with actual
    
    /// RedeemReserveCollateral instruction discriminator
    /// sha256("global:redeem_reserve_collateral")[:8]
    pub const REDEEM_RESERVE_COLLATERAL: [u8; 8] = [0; 8]; // TODO: Replace with actual
    
    /// FlashLoan instruction discriminator
    /// sha256("global:flash_loan")[:8]
    pub const FLASH_LOAN: [u8; 8] = [0; 8]; // TODO: Replace with actual
    
    /// FlashRepay instruction discriminator
    /// sha256("global:flash_repay")[:8]
    pub const FLASH_REPAY: [u8; 8] = [0; 8]; // TODO: Replace with actual
}

/// Get instruction discriminator for RefreshReserve
pub fn get_refresh_reserve_discriminator() -> Vec<u8> {
    instruction_discriminators::REFRESH_RESERVE.to_vec()
}

/// Get instruction discriminator for RefreshObligation
pub fn get_refresh_obligation_discriminator() -> Vec<u8> {
    instruction_discriminators::REFRESH_OBLIGATION.to_vec()
}

/// Get instruction discriminator for LiquidateObligation
pub fn get_liquidate_obligation_discriminator() -> Vec<u8> {
    instruction_discriminators::LIQUIDATE_OBLIGATION.to_vec()
}

/// Get instruction discriminator for RedeemReserveCollateral
pub fn get_redeem_reserve_collateral_discriminator() -> Vec<u8> {
    instruction_discriminators::REDEEM_RESERVE_COLLATERAL.to_vec()
}

/// Get instruction discriminator for FlashLoan
pub fn get_flash_loan_discriminator() -> Vec<u8> {
    instruction_discriminators::FLASH_LOAN.to_vec()
}

/// Get instruction discriminator for FlashRepay
pub fn get_flash_repay_discriminator() -> Vec<u8> {
    instruction_discriminators::FLASH_REPAY.to_vec()
}

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

impl Reserve {
    /// Get mint pubkey from reserve
    pub fn mint_pubkey(&self) -> Pubkey {
        self.liquidity.mint_pubkey
    }
    
    /// Get mint decimals from reserve
    pub fn mint_decimals(&self) -> u8 {
        self.liquidity.mint_decimals as u8
    }
    
    /// Get collateral mint pubkey
    pub fn collateral_mint_pubkey(&self) -> Pubkey {
        self.collateral.mint_pubkey
    }
    
    /// Get Pyth oracle pubkey
    pub fn pyth_oracle(&self) -> Pubkey {
        self.config.oracle_config.pyth_config.price
    }
    
    /// Get Switchboard oracle pubkey
    pub fn switchboard_oracle(&self) -> Pubkey {
        self.config.oracle_config.switchboard_config.price_aggregator
    }
    
    /// Get market price in USD (from market_price_sf)
    pub fn market_price_usd(&self) -> f64 {
        (self.liquidity.market_price_sf as f64) / (SCALE_FACTOR as f64)
    }
    
    /// Get LTV percentage
    pub fn ltv_pct(&self) -> u16 {
        self.config.loan_to_value_pct
    }
    
    /// Get liquidation threshold percentage
    pub fn liquidation_threshold_pct(&self) -> u16 {
        self.config.liquidation_threshold_pct
    }
    
    /// Get liquidation bonus in basis points
    pub fn liquidation_bonus_bps(&self) -> u16 {
        self.config.liquidation_bonus_bps
    }
    
    /// Get close factor (0.0 to 1.0)
    pub fn close_factor(&self) -> f64 {
        // Kamino uses protocol-level close factor, default to 50%
        // TODO: Get from LendingMarket config
        0.5
    }
}

impl Obligation {
    /// Get total deposited value in USD
    pub fn deposited_value_usd(&self) -> f64 {
        (self.deposited_value_sf as f64) / (SCALE_FACTOR as f64)
    }
    
    /// Get total borrowed value in USD
    pub fn borrowed_value_usd(&self) -> f64 {
        (self.borrowed_assets_market_value_sf as f64) / (SCALE_FACTOR as f64)
    }
    
    /// Get allowed borrow value in USD
    pub fn allowed_borrow_value_usd(&self) -> f64 {
        (self.allowed_borrow_value_sf as f64) / (SCALE_FACTOR as f64)
    }
    
    /// Calculate health factor
    pub fn health_factor(&self) -> f64 {
        if self.borrowed_assets_market_value_sf == 0 {
            return f64::INFINITY;
        }
        (self.deposited_value_sf as f64) / (self.borrowed_assets_market_value_sf as f64)
    }
}

