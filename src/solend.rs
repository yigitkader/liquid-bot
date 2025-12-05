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
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            &data[8..]
        } else {
            data
        };
        BorshDeserialize::try_from_slice(data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Obligation: {}", e))
    }

    /// Calculate health factor: allowedBorrowValue / borrowedValue
    /// Per Structure.md section 4.2
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

// Helper implementation for Reserve
impl Reserve {
    /// Parse reserve from account data using Borsh
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        // Skip discriminator (first 8 bytes) if present
        let data = if data.len() > 8 && data[0..8] == [0u8; 8] {
            &data[8..]
        } else {
            data
        };
        BorshDeserialize::try_from_slice(data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Reserve: {}", e))
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

