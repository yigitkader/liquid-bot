use solana_sdk::pubkey::Pubkey;
use anyhow::{Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct SolendReserve {
    pub version: u8,
    pub last_update: LastUpdate,
    pub lending_market: Pubkey,
    pub liquidity: ReserveLiquidity,
    pub collateral: ReserveCollateral,
    pub config: ReserveConfig,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveLiquidity {
    pub mint_pubkey: Pubkey,
    pub mint_decimals: u8,
    pub supply_pubkey: Pubkey,
    // Solend'in gerçek kodunda oracle_option YOK!
    // Sadece iki oracle pubkey var - hangisinin aktif olduğu program tarafından belirlenir
    pub pyth_oracle: Pubkey,
    pub switchboard_oracle: Pubkey,
    pub available_amount: u64,
    pub borrowed_amount_wads: u128,
    pub cumulative_borrow_rate_wads: u128,
    pub market_price: u128,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub mint_total_supply: u64,
    pub supply_pubkey: Pubkey,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveConfig {
    pub optimal_utilization_rate: u8,
    pub loan_to_value_ratio: u8,
    pub liquidation_bonus: u8,
    pub liquidation_threshold: u8,
    pub min_borrow_rate: u8,
    pub optimal_borrow_rate: u8,
    pub max_borrow_rate: u8,
    pub fees: ReserveFees,
    pub deposit_limit: u64,
    pub borrow_limit: u64,
    pub fee_receiver: Pubkey,
    pub protocol_liquidation_fee: u8,
    pub protocol_take_rate: u8,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveFees {
    pub borrow_fee_wad: u64,
    pub flash_loan_fee_wad: u64,
    pub host_fee_percentage: u8,
}

// Expected size of the reserve struct without padding
// This is calculated from the struct layout:
// version: 1 byte
// LastUpdate: 9 bytes (slot: 8, stale: 1)
// lending_market: 32 bytes
// ReserveLiquidity: 32 + 1 + 32 + 32 + 32 + 8 + 16 + 16 + 16 = 185 bytes
// ReserveCollateral: 32 + 8 + 32 = 72 bytes
// ReserveConfig: 7 + (8 + 8 + 1) + 8 + 8 + 32 + 1 + 1 = 74 bytes
// Total: ~372 bytes
const EXPECTED_RESERVE_SIZE_WITHOUT_PADDING: usize = 372;
const EXPECTED_PADDING_SIZE: usize = 247;
const EXPECTED_TOTAL_SIZE: usize = EXPECTED_RESERVE_SIZE_WITHOUT_PADDING + EXPECTED_PADDING_SIZE;

impl SolendReserve {
    /// Deserializes a Solend reserve account from raw account data.
    /// 
    /// The account data structure:
    /// - The reserve struct data (starts at offset 0)
    /// - 247 bytes of padding at the end (automatically skipped by Borsh)
    /// 
    /// This uses Borsh deserialization for type safety and automatic validation.
    /// Borsh will automatically handle padding by only deserializing the known struct fields.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }

        if data.len() < EXPECTED_RESERVE_SIZE_WITHOUT_PADDING {
            return Err(anyhow::anyhow!(
                "Account data too small: {} bytes (expected at least {} bytes for reserve struct)",
                data.len(),
                EXPECTED_RESERVE_SIZE_WITHOUT_PADDING
            ));
        }

        // Extract only the struct data (without padding) for Borsh deserialization
        // Borsh expects to consume exactly the bytes needed for the struct
        let struct_data = if data.len() >= EXPECTED_RESERVE_SIZE_WITHOUT_PADDING {
            &data[..EXPECTED_RESERVE_SIZE_WITHOUT_PADDING]
        } else {
            data
        };

        // Deserialize the reserve struct using Borsh
        // Borsh will automatically handle the deserialization of all nested structs
        let reserve = SolendReserve::try_from_slice(struct_data)
            .context("Failed to deserialize Solend reserve account. This might indicate the struct structure doesn't match the real Solend IDL. Please validate against official IDL: ./scripts/fetch_solend_idl.sh")?;

        // Validate that we have enough data (including padding)
        // The total account size should be at least the expected size
        if data.len() < EXPECTED_TOTAL_SIZE {
            // Warn but don't fail - padding might be smaller in some cases
            log::warn!(
                "Reserve account data size {} bytes is less than expected {} bytes (struct: {} + padding: {}). Padding might be smaller than expected.",
                data.len(),
                EXPECTED_TOTAL_SIZE,
                EXPECTED_RESERVE_SIZE_WITHOUT_PADDING,
                EXPECTED_PADDING_SIZE
            );
        }

        Ok(reserve)
    }
    
    pub fn liquidity_mint(&self) -> Pubkey {
        self.liquidity.mint_pubkey
    }
    
    pub fn collateral_mint(&self) -> Pubkey {
        self.collateral.mint_pubkey
    }
    
    pub fn liquidity_supply(&self) -> Pubkey {
        self.liquidity.supply_pubkey
    }
    
    pub fn collateral_supply(&self) -> Pubkey {
        self.collateral.supply_pubkey
    }
    
    pub fn ltv(&self) -> f64 {
        self.config.loan_to_value_ratio as f64 / 100.0
    }
    
    pub fn liquidation_bonus(&self) -> f64 {
        self.config.liquidation_bonus as f64 / 100.0
    }
    
    /// Pyth oracle pubkey'ini döndürür
    pub fn pyth_oracle(&self) -> Pubkey {
        self.liquidity.pyth_oracle
    }
    
    /// Switchboard oracle pubkey'ini döndürür
    pub fn switchboard_oracle(&self) -> Pubkey {
        self.liquidity.switchboard_oracle
    }
}
