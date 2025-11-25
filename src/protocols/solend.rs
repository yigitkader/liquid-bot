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
        // Not: Reserve account'larından LTV bilgisini almak için ek implementasyon gerekir.
        // Şu an Solend'in tipik LTV değerleri kullanılıyor (USDC: 0.85, SOL: 0.75, vb.)
        let collateral_assets: Vec<crate::domain::CollateralAsset> = obligation.deposits.iter()
            .map(|deposit| {
                // Reserve pubkey'den mint'i çıkarmak için reserve account'unu parse etmek gerekir.
                // Şu an reserve pubkey kullanılıyor, gerçek implementasyonda:
                // 1. Reserve account'unu oku
                // 2. Reserve'den mint address'ini al
                // 3. Reserve'den LTV değerini al
                // 
                // Gelecek iyileştirme: Reserve account parsing modülü eklenebilir
                let default_ltv = 0.75; // Solend için tipik LTV (reserve'den alınmalı)
                
                crate::domain::CollateralAsset {
                    mint: deposit.deposit_reserve.to_string(), // TODO: Reserve'den gerçek mint alınmalı
                    amount: deposit.deposited_amount,
                    amount_usd: deposit.market_value.to_f64(),
                    ltv: default_ltv, // TODO: Reserve account'undan gerçek LTV alınmalı
                }
            })
            .collect();
        
        // Debt assets'i domain modeline dönüştür
        // Not: Reserve account'larından borrow rate bilgisini almak için ek implementasyon gerekir.
        let debt_assets: Vec<crate::domain::DebtAsset> = obligation.borrows.iter()
            .map(|borrow| {
                // Borrowed amount'u WAD'dan normal değere çevir
                // WAD = 1e18 (Wei Adjusted Decimal)
                let borrowed_amount = (borrow.borrowed_amount_wad / 1_000_000_000_000_000_000) as u64;
                
                // Reserve pubkey'den mint'i çıkarmak için reserve account'unu parse etmek gerekir.
                // Şu an reserve pubkey kullanılıyor, gerçek implementasyonda:
                // 1. Reserve account'unu oku
                // 2. Reserve'den mint address'ini al
                // 3. Reserve'den borrow rate'i al (current_borrow_rate)
                //
                // Gelecek iyileştirme: Reserve account parsing modülü eklenebilir
                let default_borrow_rate = 0.0; // Reserve'den gerçek borrow rate alınmalı
                
                crate::domain::DebtAsset {
                    mint: borrow.borrow_reserve.to_string(), // TODO: Reserve'den gerçek mint alınmalı
                    amount: borrowed_amount,
                    amount_usd: borrow.market_value.to_f64(),
                    borrow_rate: default_borrow_rate, // TODO: Reserve account'undan gerçek borrow rate alınmalı
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
        // NOT: Gerçek implementasyonda reserve account'larını obligation'dan almak gerekir:
        // 1. Obligation'daki borrow reserve'ü bul (opportunity.target_debt_mint'ten)
        // 2. Reserve account'unu oku
        // 3. Reserve'den gerekli account'ları al:
        //    - Reserve liquidity supply
        //    - Reserve collateral mint
        //    - Reserve liquidity mint
        //    - Oracle accounts (Pyth/Switchboard)
        // 4. Lending market authority'yi hesapla (PDA)
        //
        // Şu an placeholder account'lar kullanılıyor çünkü:
        // - Reserve account parsing implementasyonu gerekiyor
        // - Oracle account'larını bulmak için ek logic gerekiyor
        // - PDA hesaplamaları gerekiyor
        //
        // Gelecek iyileştirme: Reserve account parsing modülü eklenebilir
        let accounts = vec![
            AccountMeta::new(*liquidator, true),                    // liquidator (signer)
            AccountMeta::new(obligation_pubkey, false),              // obligation
            AccountMeta::new(self.program_id, false),                // lendingMarket
            // TODO: Gerçek reserve account'larını obligation'dan al
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
        
        // Gelecek iyileştirme: Reserve account parsing
        // 
        // Gerçek implementasyonda reserve account'larını almak için:
        // 1. Obligation account'u parse et (zaten yapılıyor)
        // 2. Borrow reserve'ü bul (opportunity.target_debt_mint veya obligation.borrows'den)
        // 3. Reserve account'unu RPC'den oku
        // 4. Reserve account'unu parse et (Reserve struct)
        // 5. Reserve'den gerekli account'ları al:
        //    - liquidity_supply (token account)
        //    - collateral_mint
        //    - liquidity_mint
        //    - oracle_account (Pyth veya Switchboard)
        // 6. Lending market authority'yi hesapla (PDA derivation)
        // 7. Tüm account'ları IDL sırasına göre ekle
        //
        // Bu iyileştirme için:
        // - Reserve account struct'ı eklenmeli
        // - Reserve account parsing fonksiyonu eklenmeli
        // - Oracle account bulma logic'i eklenmeli
        // - PDA derivation helper'ı eklenmeli
        
        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}

