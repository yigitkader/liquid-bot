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
    pub const WAD: f64 = 1_000_000_000_000_000_000.0;
    
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

        let account_data_with_skip = if data.len() > 8 { &data[8..] } else { data };
        let has_discriminator = data.len() > 8 && data[0..8].iter().any(|&b| b != 0);
        let account_data = if has_discriminator {
            account_data_with_skip
        } else {
            data
        };

        // Hesaplanmış expected size: Mevcut struct'ı Borsh ile serialize ederek uzunluğunu alıyoruz.
        // Bu, account başındaki Anchor discriminator dahil DEĞİL; sadece struct'ın kendi boyutu.
        let dummy = SolendObligation {
            last_update_slot: 0,
            lending_market: Pubkey::default(),
            owner: Pubkey::default(),
            deposited_value: Number { value: 0 },
            borrowed_value: Number { value: 0 },
            allowed_borrow_value: Number { value: 0 },
            unhealthy_borrow_value: Number { value: 0 },
            deposits: Vec::new(),
            borrows: Vec::new(),
        };

        let expected_size = borsh::to_vec(&dummy)
            .map_err(|e| anyhow::anyhow!("Failed to compute SolendObligation expected size: {}", e))?
            .len();

        if account_data.len() < expected_size {
            return Err(anyhow::anyhow!(
                "Account data too small for obligation: {} bytes (expected at least {} bytes). \
                 This might be a reserve, market, or config account (not an obligation).",
                account_data.len(),
                expected_size
            ));
        }

        // Eğer account_data, struct'tan daha büyükse (örneğin, Solend yeni alanlar eklediyse),
        // Borsh `Not all bytes read` hatası vermesin diye sadece ilk `expected_size` kadarını deserialize ediyoruz.
        let slice_to_decode = &account_data[..expected_size];

        SolendObligation::try_from_slice(slice_to_decode).map_err(|e| {
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
                "Failed to deserialize Solend obligation: {}\n   Account data size: {} bytes (after discriminator: {} bytes, used {} bytes)\n   First 64 bytes (hex): {}\n   This might indicate:\n   1. Struct layout mismatch\n   2. Account is not an obligation (reserve/market/config)\n   3. Protocol version mismatch",
                e,
                data.len(),
                account_data.len(),
                expected_size,
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
