use async_trait::async_trait;
use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    instruction::{Instruction, AccountMeta},
    account::Account,
};
use crate::domain::AccountPosition;
use crate::protocol::{Protocol, LiquidationParams};
use crate::solana_client::SolanaClient;
use sha2::{Sha256, Digest};
use std::sync::Arc;
use crate::protocols::solend_accounts::{get_associated_token_address, derive_lending_market_authority};

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

use solend_idl::SolendObligation;

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
    
    async fn resolve_liquidation_accounts(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: &SolanaClient,
    ) -> Result<(
        Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey, Pubkey,
    )> {
        let obligation_pubkey = Pubkey::try_from(opportunity.account_position.account_address.as_str())
            .context("Invalid obligation pubkey")?;
        let obligation = solend_idl::SolendObligation::from_account_data(
            &rpc_client.get_account(&obligation_pubkey).await?.data
        ).context("Failed to parse obligation")?;
        
        let borrow_reserve_pubkey = Pubkey::try_from(opportunity.target_debt_mint.as_str())
            .ok()
            .or_else(|| obligation.borrows.first().map(|b| b.borrow_reserve))
            .ok_or_else(|| anyhow::anyhow!("No borrow reserve found"))?;
        
        use crate::protocols::reserve_helper::parse_reserve_account;
        let reserve_info = parse_reserve_account(&borrow_reserve_pubkey, 
            &rpc_client.get_account(&borrow_reserve_pubkey).await?).await
            .context("Failed to parse reserve")?;
        
        let debt_mint = reserve_info.liquidity_mint.ok_or_else(|| anyhow::anyhow!("No liquidity mint"))?;
        let reserve_liquidity_supply = reserve_info.liquidity_supply.ok_or_else(|| anyhow::anyhow!("No liquidity supply"))?;
        
        let collateral_reserve_pubkey = Pubkey::try_from(opportunity.target_collateral_mint.as_str())
            .ok()
            .or_else(|| obligation.deposits.first().map(|d| d.deposit_reserve))
            .ok_or_else(|| anyhow::anyhow!("No collateral reserve found"))?;
        
        let collateral_reserve_info = parse_reserve_account(&collateral_reserve_pubkey,
            &rpc_client.get_account(&collateral_reserve_pubkey).await?).await
            .context("Failed to parse collateral reserve")?;
        
        let collateral_mint = collateral_reserve_info.collateral_mint.ok_or_else(|| anyhow::anyhow!("No collateral mint"))?;
        
        let lending_market_authority = derive_lending_market_authority(&obligation.lending_market, &self.program_id)
            .context("Failed to derive lending market authority")?;
        
        let source_liquidity = get_associated_token_address(liquidator, &debt_mint)?;
        let destination_collateral = get_associated_token_address(liquidator, &collateral_mint)?;
        
        use crate::protocols::oracle_helper::{get_oracle_accounts_from_reserve, get_oracle_accounts_from_mint};
        let (pyth, switchboard) = get_oracle_accounts_from_reserve(&reserve_info)
            .or_else(|_| get_oracle_accounts_from_mint(&debt_mint))
            .unwrap_or((None, None));
        
        Ok((
            source_liquidity,
            destination_collateral,
            borrow_reserve_pubkey,
            collateral_mint,
            reserve_liquidity_supply,
            obligation.lending_market,
            lending_market_authority,
            reserve_liquidity_supply,
            pyth.unwrap_or_default(),
            switchboard.unwrap_or_default(),
        ))
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
        rpc_client: Option<Arc<SolanaClient>>,
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
        
        use crate::protocols::reserve_helper::parse_reserve_account;
        
        async fn get_reserve_info(
            reserve_pubkey: &Pubkey,
            rpc_client: Option<&Arc<SolanaClient>>,
        ) -> Option<crate::protocols::reserve_helper::ReserveInfo> {
            let rpc = rpc_client?;
            let account = rpc.get_account(reserve_pubkey).await.ok()?;
            parse_reserve_account(reserve_pubkey, &account).await.ok()
        }
        
        let mut collateral_assets = Vec::new();
        for deposit in &obligation.deposits {
            let (mint, ltv) = match get_reserve_info(&deposit.deposit_reserve, rpc_client.as_ref()).await {
                Some(reserve_info) => {
                    let mint = reserve_info.collateral_mint
                        .unwrap_or_else(|| reserve_info.liquidity_mint.unwrap_or(deposit.deposit_reserve));
                    (mint.to_string(), reserve_info.ltv)
                }
                None => {
                    log::warn!("Failed to get reserve info for {}, using fallback", deposit.deposit_reserve);
                    (deposit.deposit_reserve.to_string(), 0.75)
                }
            };
            
            collateral_assets.push(crate::domain::CollateralAsset {
                mint,
                amount: deposit.deposited_amount,
                amount_usd: deposit.market_value.to_f64(),
                ltv,
            });
        }
        
        let mut debt_assets = Vec::new();
        for borrow in &obligation.borrows {
            let borrowed_amount = (borrow.borrowed_amount_wad / 1_000_000_000_000_000_000) as u64;
            
            let (mint, borrow_rate) = match get_reserve_info(&borrow.borrow_reserve, rpc_client.as_ref()).await {
                Some(reserve_info) => {
                    let mint = reserve_info.liquidity_mint.unwrap_or(borrow.borrow_reserve);
                    (mint.to_string(), reserve_info.borrow_rate)
                }
                None => {
                    log::warn!("Failed to get reserve info for {}, using fallback", borrow.borrow_reserve);
                    (borrow.borrow_reserve.to_string(), 0.0)
                }
            };
            
            debt_assets.push(crate::domain::DebtAsset {
                mint,
                amount: borrowed_amount,
                amount_usd: borrow.market_value.to_f64(),
                borrow_rate,
            });
        }
        
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
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction> {
        let obligation_pubkey = Pubkey::try_from(opportunity.account_position.account_address.as_str())
            .context("Invalid obligation pubkey")?;
        
        let mut hasher = Sha256::new();
        hasher.update(b"global:liquidateObligation");
        let instruction_discriminator: [u8; 8] = hasher.finalize()[0..8].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to create discriminator"))?;
        
        let (source_liquidity, destination_collateral, reserve, reserve_collateral_mint, 
             reserve_liquidity_supply, lending_market, lending_market_authority, destination_liquidity,
             pyth_price, switchboard_price) = if let Some(rpc) = rpc_client {
            self.resolve_liquidation_accounts(opportunity, liquidator, &*rpc).await?
        } else {
            log::warn!("⚠️  RPC client not provided, using placeholder accounts");
            (Pubkey::default(), Pubkey::default(), Pubkey::default(), Pubkey::default(),
             Pubkey::default(), self.program_id, Pubkey::default(), Pubkey::default(),
             Pubkey::default(), Pubkey::default())
        };
        
        let accounts = vec![
            AccountMeta::new(source_liquidity, false),
            AccountMeta::new(destination_collateral, false),
            AccountMeta::new(obligation_pubkey, false),
            AccountMeta::new(reserve, false),
            AccountMeta::new(reserve_collateral_mint, false),
            AccountMeta::new(reserve_liquidity_supply, false),
            AccountMeta::new_readonly(lending_market, false),
            AccountMeta::new_readonly(lending_market_authority, false),
            AccountMeta::new(destination_liquidity, false),
            AccountMeta::new(*liquidator, true),
            AccountMeta::new_readonly(pyth_price, false),
            AccountMeta::new_readonly(switchboard_price, false),
            AccountMeta::new_readonly(spl_token::id(), false),
        ];
        
        // Instruction data: [discriminator (8 bytes), liquidityAmount (8 bytes)]
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&instruction_discriminator);
        data.extend_from_slice(&opportunity.max_liquidatable_amount.to_le_bytes());
        
        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}

