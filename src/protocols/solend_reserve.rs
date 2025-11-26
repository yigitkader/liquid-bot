//! Solend Reserve Account Structure
//! 
//! ✅ DOĞRULANMIŞ: Bu struct yapısı Solend TypeScript SDK'dan alınmıştır
//! Kaynak: https://github.com/solendprotocol/solend-sdk/blob/master/src/state/reserve.ts
//! 
//! ⚠️ ÖNEMLİ: Solend BufferLayout kullanıyor, Borsh değil!
//! Bu yüzden manuel parsing yapıyoruz.
//! 
//! Gerçek yapı (BufferLayout formatında):
//! - version: u8
//! - lastUpdate: LastUpdate (slot: u64, stale: u8)
//! - lendingMarket: Pubkey (32 bytes)
//! - liquidity: ReserveLiquidity
//! - collateral: ReserveCollateral
//! - config: ReserveConfig
//! - padding: 247 bytes (blob)

use solana_sdk::pubkey::Pubkey;
use anyhow::{Result, Context};
use std::io::Cursor;

/// Solend Reserve Account yapısı
/// 
/// ✅ DOĞRULANMIŞ: Solend TypeScript SDK'dan alınmıştır
/// Kaynak: https://github.com/solendprotocol/solend-sdk/blob/master/src/state/reserve.ts
/// 
/// NOT: BufferLayout formatında serialize edilmiş, Borsh değil!
#[derive(Debug, Clone)]
pub struct SolendReserve {
    pub version: u8,
    pub last_update: LastUpdate,
    pub lending_market: Pubkey,
    pub liquidity: ReserveLiquidity,
    pub collateral: ReserveCollateral,
    pub config: ReserveConfig,
}

/// Last Update bilgileri
#[derive(Debug, Clone)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8, // boolean olarak u8 (0 = false, 1 = true)
}

/// Reserve Liquidity bilgileri
#[derive(Debug, Clone)]
pub struct ReserveLiquidity {
    pub mint_pubkey: Pubkey,
    pub mint_decimals: u8,
    pub supply_pubkey: Pubkey,
    pub pyth_oracle: Pubkey,
    pub switchboard_oracle: Pubkey,
    pub available_amount: u64,
    pub borrowed_amount_wads: u128,
    pub cumulative_borrow_rate_wads: u128,
    pub market_price: u128,
}

/// Reserve Collateral bilgileri
#[derive(Debug, Clone)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub mint_total_supply: u64,
    pub supply_pubkey: Pubkey,
}

/// Reserve Configuration
#[derive(Debug, Clone)]
pub struct ReserveConfig {
    pub optimal_utilization_rate: u8,
    pub loan_to_value_ratio: u8, // LTV (0-100)
    pub liquidation_bonus: u8,   // Liquidation bonus (0-100)
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

/// Reserve Fees
#[derive(Debug, Clone)]
pub struct ReserveFees {
    pub borrow_fee_wad: u64,
    pub flash_loan_fee_wad: u64,
    pub host_fee_percentage: u8,
}

/// BufferLayout formatında Pubkey okuma helper
fn read_pubkey(data: &[u8], offset: &mut usize) -> Result<Pubkey> {
    if *offset + 32 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for Pubkey"));
    }
    let pubkey_bytes = &data[*offset..*offset + 32];
    *offset += 32;
    Pubkey::try_from(pubkey_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid Pubkey: {}", e))
}

/// BufferLayout formatında u64 okuma helper (little-endian)
fn read_u64(data: &[u8], offset: &mut usize) -> Result<u64> {
    if *offset + 8 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for u64"));
    }
    let bytes = &data[*offset..*offset + 8];
    *offset += 8;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

/// BufferLayout formatında u128 okuma helper (little-endian)
fn read_u128(data: &[u8], offset: &mut usize) -> Result<u128> {
    if *offset + 16 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for u128"));
    }
    let bytes = &data[*offset..*offset + 16];
    *offset += 16;
    Ok(u128::from_le_bytes(bytes.try_into().unwrap()))
}

impl SolendReserve {
    /// Account data'dan reserve parse eder (BufferLayout formatında)
    /// 
    /// ✅ DOĞRULANMIŞ: Gerçek Solend SDK yapısına göre güncellenmiştir
    /// Solend BufferLayout kullanıyor, Borsh değil!
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }
        
        // Solend Reserve account'u BufferLayout formatında
        // Padding 247 byte, toplam account size 619 byte olmalı
        let mut offset = 0;
        
        // version: u8
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for version"));
        }
        let version = data[offset];
        offset += 1;
        
        // lastUpdate: LastUpdate (slot: u64, stale: u8)
        let slot = read_u64(data, &mut offset)?;
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for stale"));
        }
        let stale = data[offset];
        offset += 1;
        let last_update = LastUpdate { slot, stale };
        
        // lendingMarket: Pubkey (32 bytes)
        let lending_market = read_pubkey(data, &mut offset)?;
        
        // liquidity: ReserveLiquidity
        let mint_pubkey = read_pubkey(data, &mut offset)?;
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for mint_decimals"));
        }
        let mint_decimals = data[offset];
        offset += 1;
        let supply_pubkey = read_pubkey(data, &mut offset)?;
        let pyth_oracle = read_pubkey(data, &mut offset)?;
        let switchboard_oracle = read_pubkey(data, &mut offset)?;
        let available_amount = read_u64(data, &mut offset)?;
        let borrowed_amount_wads = read_u128(data, &mut offset)?;
        let cumulative_borrow_rate_wads = read_u128(data, &mut offset)?;
        let market_price = read_u128(data, &mut offset)?;
        let liquidity = ReserveLiquidity {
            mint_pubkey,
            mint_decimals,
            supply_pubkey,
            pyth_oracle,
            switchboard_oracle,
            available_amount,
            borrowed_amount_wads,
            cumulative_borrow_rate_wads,
            market_price,
        };
        
        // collateral: ReserveCollateral
        let mint_pubkey = read_pubkey(data, &mut offset)?;
        let mint_total_supply = read_u64(data, &mut offset)?;
        let supply_pubkey = read_pubkey(data, &mut offset)?;
        let collateral = ReserveCollateral {
            mint_pubkey,
            mint_total_supply,
            supply_pubkey,
        };
        
        // config: ReserveConfig
        if offset + 7 > data.len() {
            return Err(anyhow::anyhow!("Insufficient data for config u8 fields"));
        }
        let optimal_utilization_rate = data[offset];
        offset += 1;
        let loan_to_value_ratio = data[offset];
        offset += 1;
        let liquidation_bonus = data[offset];
        offset += 1;
        let liquidation_threshold = data[offset];
        offset += 1;
        let min_borrow_rate = data[offset];
        offset += 1;
        let optimal_borrow_rate = data[offset];
        offset += 1;
        let max_borrow_rate = data[offset];
        offset += 1;
        
        // fees: ReserveFees
        let borrow_fee_wad = read_u64(data, &mut offset)?;
        let flash_loan_fee_wad = read_u64(data, &mut offset)?;
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for host_fee_percentage"));
        }
        let host_fee_percentage = data[offset];
        offset += 1;
        let fees = ReserveFees {
            borrow_fee_wad,
            flash_loan_fee_wad,
            host_fee_percentage,
        };
        
        // config devamı
        let deposit_limit = read_u64(data, &mut offset)?;
        let borrow_limit = read_u64(data, &mut offset)?;
        let fee_receiver = read_pubkey(data, &mut offset)?;
        if offset + 2 > data.len() {
            return Err(anyhow::anyhow!("Insufficient data for protocol fees"));
        }
        let protocol_liquidation_fee = data[offset];
        offset += 1;
        let protocol_take_rate = data[offset];
        offset += 1;
        let config = ReserveConfig {
            optimal_utilization_rate,
            loan_to_value_ratio,
            liquidation_bonus,
            liquidation_threshold,
            min_borrow_rate,
            optimal_borrow_rate,
            max_borrow_rate,
            fees,
            deposit_limit,
            borrow_limit,
            fee_receiver,
            protocol_liquidation_fee,
            protocol_take_rate,
        };
        
        // Padding: 247 bytes (skip ediyoruz)
        // offset şu an struct'ın sonunu gösteriyor, padding'i skip ediyoruz
        
        Ok(SolendReserve {
            version,
            last_update,
            lending_market,
            liquidity,
            collateral,
            config,
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
    
    /// Pyth oracle pubkey'ini döndürür
    pub fn pyth_oracle(&self) -> Pubkey {
        self.liquidity.pyth_oracle
    }
    
    /// Switchboard oracle pubkey'ini döndürür
    pub fn switchboard_oracle(&self) -> Pubkey {
        self.liquidity.switchboard_oracle
    }
    
    /// Oracle pubkey'ini döndürür (Pyth veya Switchboard)
    /// Öncelikle Pyth oracle'ı kontrol eder
    pub fn oracle_pubkey(&self) -> Option<Pubkey> {
        let pyth = self.liquidity.pyth_oracle;
        let switchboard = self.liquidity.switchboard_oracle;
        
        // Eğer Pyth oracle default (zero) değilse onu kullan
        if pyth != Pubkey::default() {
            Some(pyth)
        } else if switchboard != Pubkey::default() {
            Some(switchboard)
        } else {
            None
        }
    }
}
