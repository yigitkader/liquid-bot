use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct SolendObligation {
    pub last_update_slot: u64,
    pub lending_market: Pubkey,
    pub owner: Pubkey,
    pub deposited_value: Number,
    pub borrowed_value: Number,
    pub allowed_borrow_value: Number,
    pub unhealthy_borrow_value: Number,
    pub deposits: Vec<ObligationCollateral>,
    pub borrows: Vec<ObligationLiquidity>,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ObligationCollateral {
    pub deposit_reserve: Pubkey,
    pub deposited_amount: u64,
    pub market_value: Number,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ObligationLiquidity {
    pub borrow_reserve: Pubkey,
    pub cumulative_borrow_rate_wad: u128,
    pub borrowed_amount_wad: u128,
    pub market_value: Number,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Number {
    pub value: u128,
}

impl Number {
    /// WAD (Wei-scAlar Decimal) format constant: 1e18
    /// 
    /// Solend uses WAD format for all decimal values, consistent with:
    /// - Solend SDK (src/state/obligation.ts)
    /// - Solana's decimal representation standard
    /// - Compound/MakerDAO WAD format (1e18)
    /// 
    /// Reference: https://docs.solend.fi/developers/protocol-overview
    pub const WAD: f64 = 1_000_000_000_000_000_000.0; // 1e18
    
    pub fn to_f64(&self) -> f64 {
        self.value as f64 / Self::WAD
    }

    pub fn to_u64(&self) -> u64 {
        (self.value / Self::WAD as u128) as u64
    }
}

impl SolendObligation {
    pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }

        // ⚠️ IMPORTANT: Solend obligation accounts may or may not have an 8-byte discriminator
        // According to Solend SDK, obligation accounts use Anchor's discriminator (8 bytes)
        // But we need to check if the first 8 bytes are actually a discriminator or part of the struct
        // 
        // Strategy: Try parsing with discriminator skip first, if that fails, try without skip
        // This handles both cases: accounts with and without discriminator
        
        // First, try with 8-byte discriminator skip (Anchor standard)
        let account_data_with_skip = if data.len() > 8 { &data[8..] } else { data };
        
        // Check if this looks like it has a discriminator
        // Discriminators are typically non-zero bytes in the first 8 bytes
        let has_discriminator = data.len() > 8 && data[0..8].iter().any(|&b| b != 0);
        
        let account_data = if has_discriminator {
            account_data_with_skip
        } else {
            // No discriminator, use full data
            data
        };
        
        // Calculate expected minimum size for obligation struct
        // Basic fields: last_update_slot (8) + lending_market (32) + owner (32) + 
        // deposited_value (16) + borrowed_value (16) + allowed_borrow_value (16) + 
        // unhealthy_borrow_value (16) + deposits vec (4 + 0*72) + borrows vec (4 + 0*72)
        // Minimum: ~140 bytes for empty vectors
        let min_expected_size = 140;
        
        if account_data.len() < min_expected_size {
            return Err(anyhow::anyhow!(
                "Account data too small for obligation: {} bytes (expected at least {} bytes). \
                 This might be a reserve, market, or config account (not an obligation).",
                account_data.len(),
                min_expected_size
            ));
        }
        
        SolendObligation::try_from_slice(account_data)
            .map_err(|e| {
                // Provide more detailed error message
                let hex_preview: String = data
                    .iter()
                    .take(64)
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .chunks(32)
                    .map(|chunk| chunk.join(" "))
                    .collect::<Vec<_>>()
                    .join(" ");
                
                anyhow::anyhow!(
                    "Failed to deserialize Solend obligation: {}\n   Account data size: {} bytes (after discriminator: {} bytes)\n   First 64 bytes (hex): {}\n   This might indicate:\n   1. Struct layout mismatch (check src/protocols/solend_idl.rs)\n   2. Account is not an obligation (reserve/market/config)\n   3. Protocol version mismatch",
                    e,
                    data.len(),
                    account_data.len(),
                    hex_preview
                )
            })
    }
    
    pub fn total_deposited_value_usd(&self) -> f64 {
        self.deposited_value.to_f64()
    }
    
    pub fn total_borrowed_value_usd(&self) -> f64 {
        self.borrowed_value.to_f64()
    }
    
    pub fn calculate_health_factor(&self) -> f64 {
        let borrowed = self.total_borrowed_value_usd();
        if borrowed == 0.0 {
            return f64::INFINITY;
        }

        let deposited = self.total_deposited_value_usd();
        deposited / borrowed
    }
}
