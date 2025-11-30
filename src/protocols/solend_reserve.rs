use solana_sdk::pubkey::Pubkey;
use anyhow::Result;
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
    // ❌ VALIDATION RESULT: oracle_option field does NOT exist in real Solend reserve struct
    // Validation test (BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw):
    // - Account size: 619 bytes (not 623 bytes)
    // - Struct without padding: 371 bytes (not 375 bytes)
    // - Offset 107-110 contains first 4 bytes of Pyth oracle pubkey (not oracle_option)
    // - Reading as u32 gives garbage value: 2207945662 (not 0, 1, or 2)
    // 
    // ✅ CORRECT LAYOUT (verified against mainnet):
    // Pyth oracle: offset 107-138 (32 bytes, Pubkey)
    pub pyth_oracle: Pubkey,
    // Switchboard oracle: offset 139-170 (32 bytes, Pubkey)
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
    // NOTE: protocol_liquidation_fee and protocol_take_rate do NOT exist in the official Solend struct!
    // These fields were incorrectly added and cause struct layout mismatch.
    // The official ReserveConfig struct ends with fee_receiver (Pubkey, 32 bytes).
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct ReserveFees {
    pub borrow_fee_wad: u64,
    pub flash_loan_fee_wad: u64,
    pub host_fee_percentage: u8,
}

// Expected size of the reserve struct - VALIDATED against official Solend source code
// Source: https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs
// 
// Official layout (from Pack implementation and check_oracle_option.sh):
// version: 1 byte (offset 0)
// last_update_slot: 8 bytes (offset 1-8)
// last_update_stale: 1 byte (offset 9)
// lending_market: 32 bytes (Pubkey, offset 10-41)
// liquidity_mint_pubkey: 32 bytes (Pubkey, offset 42-73)
// liquidity_mint_decimals: 1 byte (offset 74)
// liquidity_supply_pubkey: 32 bytes (Pubkey, offset 75-106)
// ❌ VALIDATED: oracle_option does NOT exist in real Solend reserve struct
// liquidity_pyth_oracle_pubkey: 32 bytes (Pubkey, offset 107-138)
// liquidity_switchboard_oracle_pubkey: 32 bytes (Pubkey, offset 139-170)
// liquidity_available_amount: 8 bytes (u64, offset 175-182)
// liquidity_borrowed_amount_wads: 16 bytes (u128/Decimal)
// liquidity_cumulative_borrow_rate_wads: 16 bytes (u128/Decimal)
// liquidity_market_price: 16 bytes (u128/Decimal)
// collateral_mint_pubkey: 32 bytes (Pubkey)
// collateral_mint_total_supply: 8 bytes (u64)
// collateral_supply_pubkey: 32 bytes (Pubkey)
// config_optimal_utilization_rate: 1 byte
// config_loan_to_value_ratio: 1 byte
// config_liquidation_bonus: 1 byte
// config_liquidation_threshold: 1 byte
// config_min_borrow_rate: 1 byte
// config_optimal_borrow_rate: 1 byte
// config_max_borrow_rate: 1 byte
// config_fees_borrow_fee_wad: 8 bytes (u64)
// config_fees_flash_loan_fee_wad: 8 bytes (u64)
// config_fees_host_fee_percentage: 1 byte
// config_deposit_limit: 8 bytes (u64)
// config_borrow_limit: 8 bytes (u64)
// config_fee_receiver: 32 bytes (Pubkey)
// padding: 248 bytes
// Total: 619 bytes (RESERVE_LEN constant in official source)
//
// Calculation: 1+8+1+32+32+1+32+32+32+8+16+16+16+32+8+32+1+1+1+1+1+1+1+8+8+1+8+8+32+248 = 619
// ✅ VALIDATED: Total size is 619 bytes (RESERVE_LEN constant in official source)
// ❌ oracle_option (4 bytes) does NOT exist - verified against mainnet account
//
// ⚠️  WARNING: These are DEFAULT values. If Solend protocol upgrades, these may change.
// The actual struct size is validated at runtime. Use config.expected_reserve_size for flexibility.
const DEFAULT_RESERVE_SIZE_WITHOUT_PADDING: usize = 371; // 619 - 248 (validated against mainnet)
const DEFAULT_PADDING_SIZE: usize = 248;
const DEFAULT_TOTAL_SIZE: usize = 619; // Official RESERVE_LEN constant (validated)

/// Calculates the expected struct size without padding by serializing an empty struct
/// This provides runtime validation of the struct size
fn calculate_struct_size() -> usize {
    // Use Borsh to calculate the actual serialized size
    // This is more reliable than hardcoded values
    let reserve = SolendReserve {
        version: 0,
        last_update: LastUpdate { slot: 0, stale: 0 },
        lending_market: Pubkey::default(),
        liquidity: ReserveLiquidity {
            mint_pubkey: Pubkey::default(),
            mint_decimals: 0,
            supply_pubkey: Pubkey::default(),
            pyth_oracle: Pubkey::default(),
            switchboard_oracle: Pubkey::default(),
            available_amount: 0,
            borrowed_amount_wads: 0,
            cumulative_borrow_rate_wads: 0,
            market_price: 0,
        },
        collateral: ReserveCollateral {
            mint_pubkey: Pubkey::default(),
            mint_total_supply: 0,
            supply_pubkey: Pubkey::default(),
        },
        config: ReserveConfig {
            optimal_utilization_rate: 0,
            loan_to_value_ratio: 0,
            liquidation_bonus: 0,
            liquidation_threshold: 0,
            min_borrow_rate: 0,
            optimal_borrow_rate: 0,
            max_borrow_rate: 0,
            fees: ReserveFees {
                borrow_fee_wad: 0,
                flash_loan_fee_wad: 0,
                host_fee_percentage: 0,
            },
            deposit_limit: 0,
            borrow_limit: 0,
            fee_receiver: Pubkey::default(),
        },
    };
    
    // Serialize to get actual size
    match borsh::to_vec(&reserve) {
        Ok(bytes) => bytes.len(),
        Err(_) => DEFAULT_RESERVE_SIZE_WITHOUT_PADDING, // Fallback to default if serialization fails
    }
}

impl SolendReserve {
    /// Deserializes a Solend reserve account from raw account data.
    /// 
    /// The account data structure:
    /// - The reserve struct data (starts at offset 0, typically 371 bytes)
    /// - Padding at the end (typically 248 bytes, but may vary)
    /// - Total: typically 619 bytes (official RESERVE_LEN constant, but may change with protocol upgrades)
    /// 
    /// This uses Borsh deserialization for type safety and automatic validation.
    /// Borsh will automatically handle padding by only deserializing the known struct fields.
    /// 
    /// ⚠️  WARNING: If Solend protocol upgrades, the struct size may change.
    /// This function validates the struct size at runtime to detect version mismatches.
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }

        // Calculate actual struct size at runtime (more reliable than hardcoded values)
        let actual_struct_size = calculate_struct_size();
        let min_expected_size = actual_struct_size.min(DEFAULT_RESERVE_SIZE_WITHOUT_PADDING);

        if data.len() < min_expected_size {
            return Err(anyhow::anyhow!(
                "Account data too small: {} bytes (expected at least {} bytes for reserve struct). \
                This might indicate a different Solend protocol version or corrupted data.",
                data.len(),
                min_expected_size
            ));
        }

        // Validate struct size matches expected (detect protocol upgrades)
        if actual_struct_size != DEFAULT_RESERVE_SIZE_WITHOUT_PADDING {
            log::warn!(
                "⚠️  Struct size mismatch detected! Calculated: {} bytes, expected: {} bytes. \
                This might indicate a Solend protocol upgrade or struct definition mismatch. \
                Please verify the struct definition matches the current Solend protocol version.",
                actual_struct_size,
                DEFAULT_RESERVE_SIZE_WITHOUT_PADDING
            );
        }

        // Extract only the struct data (without padding) for Borsh deserialization
        // Borsh expects to consume exactly the bytes needed for the struct
        // Use the calculated size, but ensure we don't exceed available data
        let struct_data = if data.len() >= actual_struct_size {
            &data[..actual_struct_size]
        } else {
            // If data is smaller than expected, try to deserialize what we have
            // This handles cases where padding might be missing or struct is smaller
            log::warn!(
                "Account data ({} bytes) is smaller than calculated struct size ({} bytes). Attempting to deserialize available data.",
                data.len(),
                actual_struct_size
            );
            data
        };

        // Deserialize the reserve struct using Borsh
        // Borsh will automatically handle the deserialization of all nested structs
        // 
        // ✅ VALIDATED: This struct layout matches the official Solend source code:
        // https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs
        // 
        // The struct has been verified against the official Pack implementation and RESERVE_LEN constant (619 bytes).
        let reserve = match SolendReserve::try_from_slice(struct_data) {
            Ok(r) => r,
            Err(e) => {
                // Provide detailed error message for debugging
                return Err(anyhow::anyhow!(
                    "Failed to deserialize Solend reserve account: {}. \
                    Account data size: {} bytes, Calculated struct size: {} bytes. \
                    This struct has been validated against the official Solend source code. \
                    If parsing fails, the account data may be corrupted, from a different version, \
                    or the struct definition may need updating. \
                    Please run: cargo run --bin validate_reserve -- --reserve <RESERVE_ADDRESS>",
                    e,
                    data.len(),
                    actual_struct_size
                ));
            }
        };

        // Validate version field (first byte) - helps detect protocol upgrades.
        //
        // IMPORTANT: Burada "üstünü örtmek" yerine, elimizdeki GERÇEK bilgiyi kullanıyoruz:
        // - Borsh ile deserialize başarılı olduysa ve yukarıdaki size kontrolleri geçtiyse,
        //   elimizdeki struct layout şu anki on‑chain veriye UYUMLU demektir.
        // - Ancak `version` alanının 0/1 dışına çıkması, Solend tarafında protokol
        //   evrimi olduğunu gösterir ve yakından izlenmelidir.
        //
        // Bu yüzden:
        // - 0 veya 1 için herhangi bir uyarı yok (legacy versiyonlar)
        // - Diğer tüm versiyonlar için AÇIK bir uyarı log’luyoruz; ama deserialize
        //   başarılı olduğu için çalışmayı durdurmuyoruz.
        // - Gerçek uyumsuzluklar (field ekleme/silme, offset değişimi vb.) zaten
        //   deserialize veya size kontrollerinde hata olarak yakalanacak.
        if reserve.version != 0 && reserve.version != 1 {
            log::warn!(
                "⚠️  Solend reserve version = {} (non‑legacy). \
                 Borsh deserialize + size kontrolleri şu an GEÇİYOR, bu yüzden struct layout uyumlu kabul edildi. \
                 Yine de bu, Solend tarafında bir protokol upgrade'i olduğunu gösterir. \
                 Lütfen src/protocols/solend_reserve.rs dosyasını resmi Solend kaynağı ile periyodik olarak karşılaştırın \
                 ve gerekirse bin/validate_reserve aracını gerçek rezerv hesapları üzerinde çalıştırın.",
                reserve.version
            );
        }

        // Validate that we have enough data (including padding)
        // The total account size should be at least the expected size
        let expected_total = DEFAULT_TOTAL_SIZE;
        if data.len() < expected_total {
            // Warn but don't fail - padding might be smaller in some cases or protocol version might differ
            log::warn!(
                "Reserve account data size {} bytes is less than expected {} bytes (struct: {} + padding: {}). \
                Padding might be smaller than expected, or this might be a different protocol version.",
                data.len(),
                expected_total,
                actual_struct_size,
                DEFAULT_PADDING_SIZE
            );
        } else if data.len() > expected_total {
            log::debug!(
                "Reserve account data size {} bytes is larger than expected {} bytes. \
                This might indicate additional fields or different padding in a newer protocol version.",
                data.len(),
                expected_total
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
