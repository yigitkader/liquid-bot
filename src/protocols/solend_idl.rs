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
    pub fn to_f64(&self) -> f64 {
        // Solend'de genellikle WAD (1e18) formatında tutulur // todo: validate this information
        self.value as f64 / 1_000_000_000_000_000_000.0
    }

    pub fn to_u64(&self) -> u64 {
        (self.value / 1_000_000_000_000_000_000) as u64
    }
}

impl SolendObligation {
    pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }

        let account_data = if data.len() > 8 { &data[8..] } else { data };
        
        SolendObligation::try_from_slice(account_data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Solend obligation: {}", e))
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
