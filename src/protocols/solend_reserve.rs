//! Solend Reserve Account Structure
//! 
//! Bu modül, Solend reserve account'larını parse etmek için gerekli yapıları içerir.
//! Gerçek Solend IDL'den türetilmelidir.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;
use anyhow::Result;

/// Solend Reserve Account yapısı (IDL'den)
/// Gerçek Solend reserve account yapısı
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct SolendReserve {
    pub version: u8,
    pub last_update_slot: u64,
    pub lending_market: Pubkey,
    pub liquidity: ReserveLiquidity,
    pub collateral: ReserveCollateral,
    pub config: ReserveConfig,
}

/// Reserve Liquidity bilgileri
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveLiquidity {
    pub mint_pubkey: Pubkey,
    pub supply_pubkey: Pubkey, // Reserve liquidity supply token account
    pub fee_receiver: Pubkey,
    pub borrow_rate_wad: u128,
    pub cumulative_borrow_rate_wad: u128,
    pub available_amount: u64,
    pub borrowed_amount_wad: u128,
    pub market_price: Number,
}

/// Reserve Collateral bilgileri
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub supply_pubkey: Pubkey, // Reserve collateral supply token account
    pub total_deposits: u64,
}

/// Reserve Configuration
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveConfig {
    pub optimal_utilization_rate: u8,
    pub loan_to_value_ratio: u8, // LTV (0-100)
    pub liquidation_bonus: u8,   // Liquidation bonus (0-100)
    pub liquidation_threshold: u8,
    pub min_borrow_rate: u8,
    pub optimal_borrow_rate: u8,
    pub max_borrow_rate: u8,
    pub fees: ReserveFees,
}

/// Reserve Fees
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveFees {
    pub borrow_fee_wad: u64,
    pub flash_loan_fee_wad: u64,
    pub host_fee_percentage: u8,
}

/// Number wrapper (u128)
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Number {
    pub value: u128,
}

impl Number {
    pub fn to_f64(&self) -> f64 {
        self.value as f64 / 1_000_000_000_000_000_000.0 // WAD
    }
}

impl SolendReserve {
    /// Account data'dan reserve parse eder (Anchor discriminator ile)
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }
        
        // Anchor programlarında ilk 8 byte discriminator'dır
        let account_data = if data.len() > 8 {
            &data[8..]
        } else {
            data
        };
        
        SolendReserve::try_from_slice(account_data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Solend reserve: {}", e))
    }
    
    /// Liquidity mint address'ini döndürür
    pub fn liquidity_mint(&self) -> Pubkey {
        self.liquidity.mint_pubkey
    }
    
    /// Collateral mint address'ini döndürür
    pub fn collateral_mint(&self) -> Pubkey {
        self.collateral.mint_pubkey
    }
    
    /// Reserve liquidity supply token account'ını döndürür
    pub fn liquidity_supply(&self) -> Pubkey {
        self.liquidity.supply_pubkey
    }
    
    /// Reserve collateral supply token account'ını döndürür
    pub fn collateral_supply(&self) -> Pubkey {
        self.collateral.supply_pubkey
    }
    
    /// LTV değerini döndürür (0.0 - 1.0 arası)
    pub fn ltv(&self) -> f64 {
        self.config.loan_to_value_ratio as f64 / 100.0
    }
    
    /// Liquidation bonus'u döndürür (0.0 - 1.0 arası)
    pub fn liquidation_bonus(&self) -> f64 {
        self.config.liquidation_bonus as f64 / 100.0
    }
}

