//! Solend Reserve Account Structure
//! 
//! ⚠️ KRİTİK UYARI: Bu struct yapısı gerçek Solend IDL'ine göre doğrulanmamıştır!
//! 
//! Production kullanımından önce:
//! 1. Gerçek Solend IDL'ini al: `./scripts/fetch_solend_idl.sh`
//! 2. Reserve account yapısını doğrula
//! 3. Bu struct'ı gerçek IDL'e göre güncelle
//! 
//! Kaynak: https://github.com/solendprotocol/solend-program

use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;
use anyhow::Result;

/// Solend Reserve Account yapısı
/// 
/// ⚠️ UYARI: Bu yapı tahmine dayalıdır ve gerçek Solend IDL'ine göre doğrulanmamıştır.
/// Production kullanımından önce gerçek IDL'den doğrulanmalıdır.
/// 
/// Gerçek yapıyı almak için:
/// ```bash
/// ./scripts/fetch_solend_idl.sh
/// ```
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct SolendReserve {
    pub version: u8,
    pub last_update_slot: u64,
    pub lending_market: Pubkey,
    pub liquidity: ReserveLiquidity,
    pub collateral: ReserveCollateral,
    pub config: ReserveConfig,
    // NOT: Gerçek Solend reserve account yapısında daha fazla field olabilir
    // Örnek: oracle account'ları, additional config, vb.
    // Bu struct gerçek IDL'e göre güncellenmelidir
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
    // NOT: Gerçek Solend reserve yapısında oracle_pubkey field'ı olabilir
    // Örnek: pub oracle_pubkey: Pubkey, // Pyth veya Switchboard oracle
    // Ancak field sırası Borsh için kritik olduğu için gerçek IDL'e göre doğrulanmalı
}

/// Reserve Collateral bilgileri
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub supply_pubkey: Pubkey, // Reserve collateral supply token account
    pub total_deposits: u64,
    // NOT: Gerçek yapıda daha fazla field olabilir
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
    // NOT: Gerçek yapıda oracle account'ları veya başka config field'ları olabilir
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
    /// 
    /// ⚠️ UYARI: Bu fonksiyon gerçek Solend reserve account yapısına göre test edilmelidir.
    /// Parse error alırsanız, struct yapısını gerçek IDL'e göre güncelleyin.
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
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to deserialize Solend reserve: {}. \
                    This might indicate the struct structure doesn't match the real Solend IDL. \
                    Please validate against official IDL: ./scripts/fetch_solend_idl.sh",
                    e
                )
            })
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
    
    /// Oracle pubkey'ini döndürür (eğer reserve yapısında varsa)
    /// 
    /// NOT: Gerçek Solend reserve yapısında oracle_pubkey field'ı olabilir.
    /// Ancak şu an struct yapısında bu field yok (gerçek IDL'e göre doğrulanmalı).
    /// 
    /// Gelecek: Gerçek IDL'e göre struct güncellendiğinde bu fonksiyon çalışacak.
    pub fn oracle_pubkey(&self) -> Option<Pubkey> {
        // NOT: Gerçek yapıda oracle_pubkey field'ı varsa:
        // Some(self.liquidity.oracle_pubkey)
        // Şu an: None (struct'da field yok)
        None
    }
}
