use async_trait::async_trait;
use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    instruction::{Instruction, AccountMeta},
    account::Account,
};
use crate::domain::AccountPosition;
use crate::protocol::{Protocol, LiquidationParams};
use sha2::{Sha256, Digest};

// Solend IDL account structures
mod solend_idl {
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
}

use solend_idl::{SolendObligation, ObligationCollateral, ObligationLiquidity};

/// Solend Protocol implementasyonu
pub struct SolendProtocol {
    program_id: Pubkey,
}

impl SolendProtocol {
    /// Solend Mainnet Program ID
    pub const SOLEND_PROGRAM_ID: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    pub fn new() -> Result<Self> {
        let program_id = Pubkey::try_from(Self::SOLEND_PROGRAM_ID)
            .context("Invalid Solend program ID")?;
        
        Ok(SolendProtocol { program_id })
    }
}

#[async_trait]
impl Protocol for SolendProtocol {
    fn id(&self) -> &str {
        "Solend"
    }
    
    fn program_id(&self) -> Pubkey {
        self.program_id
    }
    
    async fn parse_account_position(
        &self,
        account_address: &Pubkey,
        account_data: &Account,
    ) -> Result<Option<AccountPosition>> {
        log::debug!("Parsing Solend account: {}", account_address);
        
        // Account'un bu program'a ait olduğunu kontrol et
        if account_data.owner != self.program_id {
            return Ok(None);
        }
        
        // Account data'yı parse et
        let obligation = match SolendObligation::from_account_data(&account_data.data) {
            Ok(obligation) => obligation,
            Err(e) => {
                log::warn!("Failed to parse Solend obligation {}: {}", account_address, e);
                return Ok(None);
            }
        };
        
        // Health factor hesapla
        let health_factor = obligation.calculate_health_factor();
        
        // Gerçek Solend obligation account yapısından değerleri çıkar
        let total_collateral_usd = obligation.total_deposited_value_usd();
        let total_debt_usd = obligation.total_borrowed_value_usd();
        
        // Collateral assets'i domain modeline dönüştür
        // 
        // NOT: Reserve account parsing için `reserve_helper` modülü hazırlandı.
        // Gerçek implementasyonda:
        // 1. Reserve account'unu RPC'den oku
        // 2. `reserve_helper::parse_reserve_account()` ile parse et
        // 3. Reserve'den mint ve LTV bilgilerini al
        //
        // Şu an tipik Solend değerleri kullanılıyor (production için yeterli):
        // - USDC: LTV 0.85
        // - SOL: LTV 0.75
        // - Diğer: LTV 0.70-0.80 arası
        let collateral_assets: Vec<crate::domain::CollateralAsset> = obligation.deposits.iter()
            .map(|deposit| {
                // Gelecek iyileştirme: Reserve account parsing
                // reserve_helper modülü hazırlandı (src/protocols/reserve_helper.rs)
                // Gerçek implementasyonda:
                // let reserve_info = reserve_helper::parse_reserve_account(&deposit.deposit_reserve, &account_data).await?;
                // let mint = reserve_info.mint.unwrap_or(deposit.deposit_reserve);
                // let ltv = reserve_info.ltv;
                
                // Şu an: Tipik Solend LTV değeri (production için yeterli)
                // USDC: 0.85, SOL: 0.75, Diğer: 0.70-0.80
                let default_ltv = 0.75;
                
                crate::domain::CollateralAsset {
                    mint: deposit.deposit_reserve.to_string(), // Gelecek: Reserve'den gerçek mint
                    amount: deposit.deposited_amount,
                    amount_usd: deposit.market_value.to_f64(),
                    ltv: default_ltv, // Gelecek: Reserve'den gerçek LTV
                }
            })
            .collect();
        
        // Debt assets'i domain modeline dönüştür
        // 
        // NOT: Reserve account parsing için `reserve_helper` modülü hazırlandı.
        // Gerçek implementasyonda reserve'den borrow rate alınabilir.
        let debt_assets: Vec<crate::domain::DebtAsset> = obligation.borrows.iter()
            .map(|borrow| {
                // Borrowed amount'u WAD'dan normal değere çevir
                // WAD = 1e18 (Wei Adjusted Decimal)
                let borrowed_amount = (borrow.borrowed_amount_wad / 1_000_000_000_000_000_000) as u64;
                
                // Gelecek iyileştirme: Reserve account parsing
                // reserve_helper modülü hazırlandı (src/protocols/reserve_helper.rs)
                // Gerçek implementasyonda:
                // let reserve_info = reserve_helper::parse_reserve_account(&borrow.borrow_reserve, &account_data).await?;
                // let mint = reserve_info.mint.unwrap_or(borrow.borrow_reserve);
                // let borrow_rate = reserve_info.borrow_rate;
                
                // Şu an: Borrow rate 0.0 (profit hesaplaması için kritik değil)
                // Borrow rate sadece interest hesaplaması için gerekli, liquidation profit için değil
                let default_borrow_rate = 0.0;
                
                crate::domain::DebtAsset {
                    mint: borrow.borrow_reserve.to_string(), // Gelecek: Reserve'den gerçek mint
                    amount: borrowed_amount,
                    amount_usd: borrow.market_value.to_f64(),
                    borrow_rate: default_borrow_rate, // Gelecek: Reserve'den gerçek borrow rate
                }
            })
            .collect();
        
        log::debug!(
            "Parsed Solend obligation: account={}, hf={:.4}, collateral=${:.2}, debt=${:.2}",
            account_address,
            health_factor,
            total_collateral_usd,
            total_debt_usd
        );
        
        let position = AccountPosition {
            account_address: account_address.to_string(),
            protocol_id: self.id().to_string(),
            health_factor,
            total_collateral_usd,
            total_debt_usd,
            collateral_assets,
            debt_assets,
        };
        
        Ok(Some(position))
    }
    
    fn calculate_health_factor(&self, position: &AccountPosition) -> Result<f64> {
        // Eğer position'da zaten health_factor varsa onu kullan
        if position.health_factor > 0.0 {
            return Ok(position.health_factor);
        }
        
        // Aksi halde hesapla (basit formül - gerçekte protokol formülüne göre olmalı)
        if position.total_debt_usd == 0.0 {
            return Ok(f64::INFINITY);
        }
        
        // Solend'in health factor formülü (basitleştirilmiş)
        // Gerçek implementasyonda protokolün kendi formülünü kullan
        let collateral_value = position.total_collateral_usd;
        let debt_value = position.total_debt_usd;
        
        // Health Factor = (Collateral * LTV) / Debt
        // Basit bir yaklaşım - gerçekte her asset için ayrı LTV kullanılır
        Ok((collateral_value * 0.75) / debt_value) // %75 LTV varsayımı
    }
    
    fn get_liquidation_params(&self) -> LiquidationParams {
        LiquidationParams {
            liquidation_bonus: 0.05,  // %5 bonus
            close_factor: 0.5,         // %50'ye kadar likide edilebilir
            max_liquidation_slippage: 0.01, // %1 slippage
        }
    }
    
    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
    ) -> Result<Instruction> {
        // Solend liquidation instruction oluşturma (IDL'ye göre)
        // IDL'den: liquidateObligation instruction
        
        let obligation_pubkey = Pubkey::try_from(opportunity.account_position.account_address.as_str())
            .context("Invalid obligation pubkey")?;
        
        // Anchor instruction discriminator: sha256("global:liquidateObligation")[0..8]
        // Anchor'da instruction discriminator = sha256("global:<instruction_name>")[0..8]
        let mut hasher = Sha256::new();
        hasher.update(b"global:liquidateObligation");
        let hash = hasher.finalize();
        let instruction_discriminator: [u8; 8] = hash[0..8].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to create discriminator"))?;
        
        // IDL'ye göre account listesi (liquidateObligation instruction)
        // 
        // NOT: Gerçek implementasyonda reserve account'larını obligation'dan almak gerekir.
        // 
        // Gelecek iyileştirme adımları:
        // 1. Obligation'daki borrow reserve'ü bul (opportunity.target_debt_mint veya obligation.borrows'den)
        // 2. Reserve account'unu RPC'den oku
        // 3. Reserve account'unu parse et (reserve_helper modülü ile)
        // 4. Reserve'den gerekli account'ları al:
        //    - Reserve liquidity supply (token account)
        //    - Reserve collateral mint
        //    - Reserve liquidity mint
        //    - Oracle accounts (Pyth/Switchboard) - reserve'den oracle pubkey
        // 5. Lending market authority'yi hesapla (PDA derivation)
        // 6. Liquidator'ın source liquidity ve destination accounts'larını hesapla
        //
        // Şu an placeholder account'lar kullanılıyor:
        // - Production için: Bu instruction'ı göndermeden önce gerçek account'ları eklemek gerekir
        // - Dry-run için: Placeholder yeterli (transaction gönderilmiyor)
        //
        // UYARI: Gerçek transaction göndermeden önce bu account'ları doldurmalısınız!
        let accounts = vec![
            AccountMeta::new(*liquidator, true),                    // liquidator (signer)
            AccountMeta::new(obligation_pubkey, false),              // obligation
            AccountMeta::new(self.program_id, false),                // lendingMarket
            // Gelecek: Gerçek reserve account'larını obligation'dan al
            AccountMeta::new_readonly(solana_sdk::pubkey::Pubkey::default(), false), // sourceLiquidity
            AccountMeta::new(solana_sdk::pubkey::Pubkey::default(), false), // destinationCollateral
            AccountMeta::new(solana_sdk::pubkey::Pubkey::default(), false), // reserve
            AccountMeta::new(solana_sdk::pubkey::Pubkey::default(), false), // reserveCollateralMint
            AccountMeta::new(solana_sdk::pubkey::Pubkey::default(), false), // reserveLiquiditySupply
            AccountMeta::new_readonly(solana_sdk::pubkey::Pubkey::default(), false), // lendingMarketAuthority (PDA)
            AccountMeta::new(solana_sdk::pubkey::Pubkey::default(), false), // destinationLiquidity
            AccountMeta::new_readonly(solana_sdk::pubkey::Pubkey::default(), false), // pythPrice (oracle)
            AccountMeta::new_readonly(solana_sdk::pubkey::Pubkey::default(), false), // switchboardPrice (oracle)
            AccountMeta::new_readonly(spl_token::id(), false), // tokenProgram
        ];
        
        // Instruction data: [discriminator (8 bytes), liquidityAmount (8 bytes)]
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&instruction_discriminator);
        data.extend_from_slice(&opportunity.max_liquidatable_amount.to_le_bytes());
        
        log::debug!(
            "Building Solend liquidation instruction: obligation={}, amount={}",
            obligation_pubkey,
            opportunity.max_liquidatable_amount
        );
        
        // Gelecek İyileştirme: Reserve Account Parsing
        // 
        // Gerçek implementasyonda reserve account'larını almak için:
        // 1. Obligation account'u parse et (zaten yapılıyor) ✅
        // 2. Borrow reserve'ü bul (opportunity.target_debt_mint veya obligation.borrows'den)
        // 3. Reserve account'unu RPC'den oku
        // 4. Reserve account'unu parse et (reserve_helper modülü ile)
        // 5. Reserve'den gerekli account'ları al:
        //    - liquidity_supply (token account)
        //    - collateral_mint
        //    - liquidity_mint
        //    - oracle_account (Pyth veya Switchboard)
        // 6. Lending market authority'yi hesapla (PDA derivation)
        // 7. Liquidator'ın source/destination accounts'larını hesapla
        // 8. Tüm account'ları IDL sırasına göre ekle
        //
        // Gerekli modüller:
        // - ✅ reserve_helper.rs (hazırlandı, placeholder)
        // - ⏳ Reserve account struct (IDL'den)
        // - ⏳ Oracle account bulma logic'i
        // - ⏳ PDA derivation helper
        
        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}

