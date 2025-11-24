use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

/// Solend Obligation Account yapısı (IDL'den)
/// Gerçek Solend obligation account yapısı
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

/// Obligation Collateral (deposit)
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ObligationCollateral {
    pub deposit_reserve: Pubkey,
    pub deposited_amount: u64,
    pub market_value: Number,
}

/// Obligation Liquidity (borrow)
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ObligationLiquidity {
    pub borrow_reserve: Pubkey,
    pub cumulative_borrow_rate_wad: u128,
    pub borrowed_amount_wad: u128,
    pub market_value: Number,
}

/// Number wrapper (u128)
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Number {
    pub value: u128,
}

impl Number {
    pub fn to_f64(&self) -> f64 {
        // Solend'de genellikle WAD (1e18) formatında tutulur
        self.value as f64 / 1_000_000_000_000_000_000.0
    }
    
    pub fn to_u64(&self) -> u64 {
        // WAD'dan normal değere çevir
        (self.value / 1_000_000_000_000_000_000) as u64
    }
}

impl SolendObligation {
    /// Account data'dan obligation parse eder (Anchor discriminator ile)
    pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }
        
        // Anchor programlarında ilk 8 byte discriminator'dır
        // Obligation account için discriminator: [0x6f, 0x62, 0x6c, 0x69, 0x67, 0x61, 0x74, 0x69] ("obligati")
        // Ancak genel olarak ilk 8 byte'ı atlayabiliriz
        let account_data = if data.len() > 8 {
            &data[8..]
        } else {
            data
        };
        
        // Borsh deserialize
        SolendObligation::try_from_slice(account_data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize Solend obligation: {}", e))
    }
    
    /// Total deposited value (USD cinsinden)
    pub fn total_deposited_value_usd(&self) -> f64 {
        self.deposited_value.to_f64()
    }
    
    /// Total borrowed value (USD cinsinden)
    pub fn total_borrowed_value_usd(&self) -> f64 {
        self.borrowed_value.to_f64()
    }
    
    /// Health factor hesaplar
    /// HF = deposited_value / borrowed_value
    pub fn calculate_health_factor(&self) -> f64 {
        let borrowed = self.total_borrowed_value_usd();
        if borrowed == 0.0 {
            return f64::INFINITY;
        }
        
        let deposited = self.total_deposited_value_usd();
        deposited / borrowed
    }
}

