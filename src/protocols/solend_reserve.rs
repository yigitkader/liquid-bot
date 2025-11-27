use solana_sdk::pubkey::Pubkey;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct SolendReserve {
    pub version: u8,
    pub last_update: LastUpdate,
    pub lending_market: Pubkey,
    pub liquidity: ReserveLiquidity,
    pub collateral: ReserveCollateral,
    pub config: ReserveConfig,
}

#[derive(Debug, Clone)]
pub struct LastUpdate {
    pub slot: u64,
    pub stale: u8,
}

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

#[derive(Debug, Clone)]
pub struct ReserveCollateral {
    pub mint_pubkey: Pubkey,
    pub mint_total_supply: u64,
    pub supply_pubkey: Pubkey,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct ReserveFees {
    pub borrow_fee_wad: u64,
    pub flash_loan_fee_wad: u64,
    pub host_fee_percentage: u8,
}

fn read_pubkey(data: &[u8], offset: &mut usize) -> Result<Pubkey> {
    if *offset + 32 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for Pubkey"));
    }
    let pubkey_bytes = &data[*offset..*offset + 32];
    *offset += 32;
    Pubkey::try_from(pubkey_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid Pubkey: {}", e))
}

fn read_u32(data: &[u8], offset: &mut usize) -> Result<u32> {
    if *offset + 4 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for u32"));
    }
    let bytes = &data[*offset..*offset + 4];
    *offset += 4;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(data: &[u8], offset: &mut usize) -> Result<u64> {
    if *offset + 8 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for u64"));
    }
    let bytes = &data[*offset..*offset + 8];
    *offset += 8;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u128(data: &[u8], offset: &mut usize) -> Result<u128> {
    if *offset + 16 > data.len() {
        return Err(anyhow::anyhow!("Insufficient data for u128"));
    }
    let bytes = &data[*offset..*offset + 16];
    *offset += 16;
    Ok(u128::from_le_bytes(bytes.try_into().unwrap()))
}

impl SolendReserve {
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }
        
        if data.len() < 42 {
            return Err(anyhow::anyhow!(
                "Account data too small: {} bytes (expected at least 42 bytes for basic fields)",
                data.len()
            ));
        }
        
        let mut offset = 0;
        
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for version"));
        }
        let version = data[offset];
        offset += 1;
        
        let slot = read_u64(data, &mut offset)?;
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for stale"));
        }
        let stale = data[offset];
        offset += 1;
        let last_update = LastUpdate { slot, stale };
        
        let lending_market = read_pubkey(data, &mut offset)?;
        
        let mint_pubkey = read_pubkey(data, &mut offset)?;
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Insufficient data for mint_decimals"));
        }
        let mint_decimals = data[offset];
        offset += 1;
        let supply_pubkey = read_pubkey(data, &mut offset)?;
        let _oracle_option = read_u32(data, &mut offset)?;
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
        
        let mint_pubkey = read_pubkey(data, &mut offset)?;
        let mint_total_supply = read_u64(data, &mut offset)?;
        let supply_pubkey = read_pubkey(data, &mut offset)?;
        let collateral = ReserveCollateral {
            mint_pubkey,
            mint_total_supply,
            supply_pubkey,
        };
        
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
        // Official SDK: BufferLayout.blob(247, "padding")
        // Note: Padding is used to align the account size to 619 bytes total
        // We don't need to validate padding as it's just empty space
        
        // Validate that we've read the expected amount of data (excluding padding)
        // Expected size without padding: ~372 bytes
        // With padding: ~619 bytes (official SDK RESERVE_SIZE)
        // We don't validate exact size here because padding might vary, but we ensure we have enough data
        
        Ok(SolendReserve {
            version,
            last_update,
            lending_market,
            liquidity,
            collateral,
            config,
        })
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
    
    pub fn pyth_oracle(&self) -> Pubkey {
        self.liquidity.pyth_oracle
    }
    
    pub fn switchboard_oracle(&self) -> Pubkey {
        self.liquidity.switchboard_oracle
    }
}
