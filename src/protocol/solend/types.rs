use solana_sdk::pubkey::Pubkey;

#[derive(Debug, Clone)]
pub struct Decimal {
    /// Scaled decimal value, stored as 128-bit integer (same size as on-chain Decimal)
    pub value: i128,
}

impl Decimal {
    pub const WAD: f64 = 1_000_000_000_000_000_000.0; // 10^18

    pub fn from_le_bytes(bytes: [u8; 16]) -> Self {
        let value = i128::from_le_bytes(bytes);
        Decimal { value }
    }

    pub fn to_f64(&self) -> f64 {
        self.value as f64 / Self::WAD
    }
}

fn unpack_decimal(src: &[u8]) -> anyhow::Result<Decimal> {
    let arr: [u8; 16] = src
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to read Decimal bytes"))?;
    Ok(Decimal::from_le_bytes(arr))
}
#[derive(Debug, Clone)]
pub struct SolendObligation {
    pub version: u8,
    pub lending_market: Pubkey,
    pub owner: Pubkey,
    pub deposits: Vec<ObligationCollateral>,
    pub borrows: Vec<ObligationLiquidity>,
    pub deposited_value: Decimal,
    pub borrowed_value: Decimal,
    pub unweighted_borrowed_value: Decimal,
    pub borrowed_value_upper_bound: Decimal,
    pub allowed_borrow_value: Decimal,
    pub unhealthy_borrow_value: Decimal,
    pub super_unhealthy_borrow_value: Decimal,
    pub borrowing_isolated_asset: bool,
    pub closeable: bool,
}

#[derive(Debug, Clone)]
pub struct ObligationCollateral {
    pub deposit_reserve: Pubkey,
    pub deposited_amount: u64,
    pub market_value: Decimal,
    pub attributed_borrow_value: Decimal,
}

#[derive(Debug, Clone)]
pub struct ObligationLiquidity {
    pub borrow_reserve: Pubkey,
    pub cumulative_borrow_rate_wads: Decimal,
    pub borrowed_amount_wads: Decimal,
    pub market_value: Decimal,
}

impl SolendObligation {
    pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
        if data.is_empty() {
            return Err(anyhow::anyhow!("Empty account data"));
        }
        // On-chain SPL Obligation layout (see solana-program-library lending obligation.rs)
        const OBLIGATION_COLLATERAL_LEN: usize = 88;
        const OBLIGATION_LIQUIDITY_LEN: usize = 112;
        const MAX_OBLIGATION_RESERVES: usize = 10;
        const OBLIGATION_LEN: usize = 1300;

        if data.len() < OBLIGATION_LEN {
            return Err(anyhow::anyhow!(
                "Account data too small for obligation: {} bytes (expected at least {} bytes)",
                data.len(),
                OBLIGATION_LEN
            ));
        }

        let src = &data[..OBLIGATION_LEN];
        let mut offset = 0usize;

        // version: u8
        let version = src[offset];
        offset += 1;

        // last_update_slot: u64 (8 bytes) + stale: u8 (1 byte)
        let _last_update_slot_bytes: [u8; 8] = src[offset..offset + 8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read last_update_slot"))?;
        let _last_update_slot = u64::from_le_bytes(_last_update_slot_bytes);
        offset += 8;

        let _last_update_stale = src[offset];
        offset += 1;

        // lending_market: Pubkey
        let lending_market = Pubkey::new_from_array(
            src[offset..offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read lending_market pubkey"))?,
        );
        offset += 32;

        // owner: Pubkey
        let owner = Pubkey::new_from_array(
            src[offset..offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read owner pubkey"))?,
        );
        offset += 32;

        // deposited_value: Decimal
        let deposited_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // borrowed_value: Decimal
        let borrowed_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // allowed_borrow_value: Decimal
        let allowed_borrow_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // unhealthy_borrow_value: Decimal
        let unhealthy_borrow_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // borrowed_value_upper_bound: Decimal
        let borrowed_value_upper_bound = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // borrowing_isolated_asset: bool (u8)
        let borrowing_isolated_asset = src[offset] != 0;
        offset += 1;

        // super_unhealthy_borrow_value: Decimal
        let super_unhealthy_borrow_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // unweighted_borrowed_value: Decimal
        let unweighted_borrowed_value = unpack_decimal(&src[offset..offset + 16])?;
        offset += 16;

        // closeable: bool (u8)
        let closeable = src[offset] != 0;
        offset += 1;

        // padding: 14 bytes
        offset += 14;

        // deposits_len: u8
        let deposits_len = src[offset] as usize;
        offset += 1;

        // borrows_len: u8
        let borrows_len = src[offset] as usize;
        offset += 1;

        if deposits_len + borrows_len > MAX_OBLIGATION_RESERVES {
            return Err(anyhow::anyhow!(
                "Invalid obligation: deposits_len + borrows_len = {} exceeds MAX_OBLIGATION_RESERVES {}",
                deposits_len + borrows_len,
                MAX_OBLIGATION_RESERVES
            ));
        }

        // data_flat
        let data_flat = &src[offset..];

        // Parse deposits
        let mut deposits = Vec::with_capacity(deposits_len);
        let mut d_offset = 0usize;
        for _ in 0..deposits_len {
            if d_offset + OBLIGATION_COLLATERAL_LEN > data_flat.len() {
                return Err(anyhow::anyhow!(
                    "Obligation collateral data too small: needed {}, have {}",
                    d_offset + OBLIGATION_COLLATERAL_LEN,
                    data_flat.len()
                ));
            }
            let slice = &data_flat[d_offset..d_offset + OBLIGATION_COLLATERAL_LEN];

            let deposit_reserve = Pubkey::new_from_array(
                slice[0..32]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Failed to read deposit_reserve pubkey"))?,
            );
            let deposited_amount = u64::from_le_bytes(
                slice[32..40]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Failed to read deposited_amount"))?,
            );
            let market_value = unpack_decimal(&slice[40..56])?;
            let attributed_borrow_value = unpack_decimal(&slice[56..72])?;
            // remaining 16 bytes are padding

            deposits.push(ObligationCollateral {
                deposit_reserve,
                deposited_amount,
                market_value,
                attributed_borrow_value,
            });

            d_offset += OBLIGATION_COLLATERAL_LEN;
        }

        // Parse borrows
        let mut borrows = Vec::with_capacity(borrows_len);
        for _ in 0..borrows_len {
            if d_offset + OBLIGATION_LIQUIDITY_LEN > data_flat.len() {
                return Err(anyhow::anyhow!(
                    "Obligation liquidity data too small: needed {}, have {}",
                    d_offset + OBLIGATION_LIQUIDITY_LEN,
                    data_flat.len()
                ));
            }
            let slice = &data_flat[d_offset..d_offset + OBLIGATION_LIQUIDITY_LEN];

            let borrow_reserve = Pubkey::new_from_array(
                slice[0..32]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Failed to read borrow_reserve pubkey"))?,
            );
            let cumulative_borrow_rate_wads = unpack_decimal(&slice[32..48])?;
            let borrowed_amount_wads = unpack_decimal(&slice[48..64])?;
            let market_value = unpack_decimal(&slice[64..80])?;
            // remaining 32 bytes are padding

            borrows.push(ObligationLiquidity {
                borrow_reserve,
                cumulative_borrow_rate_wads,
                borrowed_amount_wads,
                market_value,
            });

            d_offset += OBLIGATION_LIQUIDITY_LEN;
        }

        Ok(SolendObligation {
            version,
            lending_market,
            owner,
            deposits,
            borrows,
            deposited_value,
            borrowed_value,
            unweighted_borrowed_value,
            borrowed_value_upper_bound,
            allowed_borrow_value,
            unhealthy_borrow_value,
            super_unhealthy_borrow_value,
            borrowing_isolated_asset,
            closeable,
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
