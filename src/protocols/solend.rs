use crate::domain::AccountPosition;
use crate::protocol::{LiquidationParams, Protocol};
use crate::protocols::solend_accounts::{
    derive_lending_market_authority, get_associated_token_address,
};
use crate::solana_client::SolanaClient;
use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use std::sync::Arc;

// Solend IDL account structures
pub mod solend_idl {
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
        pub fn to_f64(&self) -> f64 {
            // Solend uses WAD (Wei-scAlar Decimal) format: 1e18
            // Reference: Solend SDK uses WAD for all decimal values
            // This is consistent with Solana's decimal representation standard
            self.value as f64 / 1_000_000_000_000_000_000.0
        }

        pub fn to_u64(&self) -> u64 {
            (self.value / 1_000_000_000_000_000_000) as u64
        }
    }

    impl SolendObligation {
        pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
            if data.is_empty() {
                return Err(anyhow::anyhow!("Empty account data"));
            }

            let account_data = if data.len() > 8 { &data[8..] } else { data };

            SolendObligation::try_from_slice(account_data)
                .map_err(|e| anyhow::anyhow!("Failed to deserialize Solend obligation: {}", e))
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
}

use solend_idl::SolendObligation;

pub struct SolendProtocol {
    program_id: Pubkey,
}

impl SolendProtocol {
    /// Deprecated: Use `new_with_config` instead. This method uses a hardcoded program ID.
    pub const SOLEND_PROGRAM_ID: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

    /// Create SolendProtocol with config (recommended)
    pub fn new_with_config(config: &crate::config::Config) -> Result<Self> {
        let program_id = Pubkey::try_from(config.solend_program_id.as_str())
            .context("Invalid Solend program ID from config")?;

        Ok(SolendProtocol { program_id })
    }

    /// Create SolendProtocol with default hardcoded program ID (for backward compatibility)
    pub fn new() -> Result<Self> {
        let program_id =
            Pubkey::try_from(Self::SOLEND_PROGRAM_ID).context("Invalid Solend program ID")?;

        Ok(SolendProtocol { program_id })
    }
    async fn resolve_liquidation_accounts(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: &SolanaClient,
    ) -> Result<(
        Pubkey, // source_liquidity
        Pubkey, // destination_collateral
        Pubkey, // obligation
        Pubkey, // lending_market
        Pubkey, // lending_market_authority
        Pubkey, // repay_reserve
        Pubkey, // repay_reserve_liquidity_supply
        Pubkey, // withdraw_reserve
        Pubkey, // withdraw_reserve_collateral_supply
        Pubkey, // withdraw_reserve_collateral_mint (for account #5)
    )> {
        let obligation_pubkey =
            Pubkey::try_from(opportunity.account_position.account_address.as_str())
                .context("Invalid obligation pubkey")?;
        let obligation = solend_idl::SolendObligation::from_account_data(
            &rpc_client.get_account(&obligation_pubkey).await?.data,
        )
        .context("Failed to parse obligation")?;

        let repay_reserve_pubkey = Pubkey::try_from(opportunity.target_debt_mint.as_str())
            .ok()
            .or_else(|| obligation.borrows.first().map(|b| b.borrow_reserve))
            .ok_or_else(|| anyhow::anyhow!("No borrow reserve found"))?;

        use crate::protocols::reserve_helper::parse_reserve_account;
        let repay_reserve_info = parse_reserve_account(
            &repay_reserve_pubkey,
            &rpc_client.get_account(&repay_reserve_pubkey).await?,
        )
        .await
        .context("Failed to parse repay reserve")?;

        let debt_mint = repay_reserve_info
            .liquidity_mint
            .ok_or_else(|| anyhow::anyhow!("No liquidity mint"))?;
        let repay_reserve_liquidity_supply = repay_reserve_info
            .liquidity_supply
            .ok_or_else(|| anyhow::anyhow!("No liquidity supply"))?;

        let withdraw_reserve_pubkey = Pubkey::try_from(opportunity.target_collateral_mint.as_str())
            .ok()
            .or_else(|| obligation.deposits.first().map(|d| d.deposit_reserve))
            .ok_or_else(|| anyhow::anyhow!("No collateral reserve found"))?;

        let withdraw_reserve_info = parse_reserve_account(
            &withdraw_reserve_pubkey,
            &rpc_client.get_account(&withdraw_reserve_pubkey).await?,
        )
        .await
        .context("Failed to parse withdraw reserve")?;

        let withdraw_reserve_collateral_supply = withdraw_reserve_info
            .collateral_supply
            .ok_or_else(|| anyhow::anyhow!("No collateral supply"))?;

        let withdraw_reserve_collateral_mint = withdraw_reserve_info
            .collateral_mint
            .ok_or_else(|| anyhow::anyhow!("No collateral mint in withdraw reserve"))?;

        let lending_market_authority =
            derive_lending_market_authority(&obligation.lending_market, &self.program_id)
                .context("Failed to derive lending market authority")?;
        
        let source_liquidity = get_associated_token_address(liquidator, &debt_mint)?;
        let destination_collateral =
            get_associated_token_address(liquidator, &withdraw_reserve_collateral_mint)?;

        Ok((
            source_liquidity,
            destination_collateral,
            obligation_pubkey,
            obligation.lending_market,
            lending_market_authority,
            repay_reserve_pubkey,
            repay_reserve_liquidity_supply,
            withdraw_reserve_pubkey,
            withdraw_reserve_collateral_supply,
            withdraw_reserve_collateral_mint,
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

        if account_data.owner != self.program_id {
            return Ok(None);
        }
        
        let obligation = match SolendObligation::from_account_data(&account_data.data) {
            Ok(obligation) => obligation,
            Err(e) => {
                log::warn!(
                    "Failed to parse Solend obligation {}: {}",
                    account_address,
                    e
                );
                return Ok(None);
            }
        };
        
        let health_factor = obligation.calculate_health_factor();
        
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
            let (mint, ltv) =
                match get_reserve_info(&deposit.deposit_reserve, rpc_client.as_ref()).await {
                    Some(reserve_info) => {
                        let mint = reserve_info.collateral_mint.unwrap_or_else(|| {
                            reserve_info
                                .liquidity_mint
                                .unwrap_or(deposit.deposit_reserve)
                        });
                        (mint.to_string(), reserve_info.ltv)
                    }
                    None => {
                        log::warn!(
                            "Failed to get reserve info for {}, using fallback",
                            deposit.deposit_reserve
                        );
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

            let (mint, borrow_rate) =
                match get_reserve_info(&borrow.borrow_reserve, rpc_client.as_ref()).await {
                    Some(reserve_info) => {
                        let mint = reserve_info.liquidity_mint.unwrap_or(borrow.borrow_reserve);
                        (mint.to_string(), reserve_info.borrow_rate)
                    }
                    None => {
                        log::warn!(
                            "Failed to get reserve info for {}, using fallback",
                            borrow.borrow_reserve
                        );
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
        if position.health_factor > 0.0 {
            return Ok(position.health_factor);
        }

        // Calculate health factor if not already set
        // Note: This is a simplified calculation. Solend's actual formula considers:
        // - Individual LTV for each collateral asset
        // - Individual liquidation threshold for each asset
        // - Weighted average based on asset values
        // For accurate health factor, use the value from parse_account_position which uses
        // SolendObligation::calculate_health_factor() that reads from on-chain data
        if position.total_debt_usd == 0.0 {
            return Ok(f64::INFINITY);
        }

        // Simplified health factor calculation
        // Real implementation should use asset-specific LTVs from reserve configs
        let collateral_value = position.total_collateral_usd;
        let debt_value = position.total_debt_usd;

        // Health Factor = (Collateral * LTV) / Debt
        // Using average LTV (0.75 = 75%) as fallback
        // In production, calculate weighted average LTV from all collateral assets
        Ok((collateral_value * 0.75) / debt_value)
    }

    fn get_liquidation_params(&self) -> LiquidationParams {
        LiquidationParams {
            liquidation_bonus: 0.05,
            close_factor: 0.5,      
            max_liquidation_slippage: 0.01,
        }
    }

    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction> {
        let mut hasher = Sha256::new();
        hasher.update(b"global:liquidateObligation");
        let discriminator: [u8; 8] = hasher.finalize()[0..8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to create discriminator"))?;
        
        log::info!(
            "Using Anchor discriminator format: discriminator={:02x?}, amount={}",
            discriminator,
            opportunity.max_liquidatable_amount
        );
        
        let (
            source_liquidity,
            destination_collateral,
            obligation,
            lending_market,
            lending_market_authority,
            repay_reserve,
            repay_reserve_liquidity_supply,
            withdraw_reserve,
            withdraw_reserve_collateral_supply,
            withdraw_reserve_collateral_mint,
        ) = if let Some(rpc) = rpc_client.as_ref() {
            self.resolve_liquidation_accounts(opportunity, liquidator, rpc)
                .await?
        } else {
            log::warn!("⚠️  RPC client not provided, using placeholder accounts");
            (
                Pubkey::default(),
                Pubkey::default(),
                Pubkey::default(),
                self.program_id,
                Pubkey::default(),
                Pubkey::default(),
                Pubkey::default(),
                Pubkey::default(),
                Pubkey::default(),
                Pubkey::default(),
            )
        };

        let (pyth_oracle, switchboard_oracle) = if let Some(rpc) = rpc_client.as_ref() {
            use crate::protocols::reserve_helper::parse_reserve_account;
            
            let (pyth, switchboard) = match rpc.get_account(&repay_reserve).await {
                Ok(reserve_account) => {
                    match parse_reserve_account(&repay_reserve, &reserve_account).await {
                        Ok(reserve_info) => (
                            reserve_info.pyth_oracle.unwrap_or(Pubkey::default()),
                            reserve_info.switchboard_oracle.unwrap_or(Pubkey::default()),
                        ),
                        Err(_) => (Pubkey::default(), Pubkey::default()),
                    }
                }
                Err(_) => (Pubkey::default(), Pubkey::default()),
            };

            (pyth, switchboard)
        } else {
            (Pubkey::default(), Pubkey::default())
        };
        
        let accounts = vec![
            // 1. sourceLiquidity - liquidator's token account for debt (isMut: true, isSigner: false)
            AccountMeta::new(source_liquidity, false),
            // 2. destinationCollateral - liquidator's token account for collateral (isMut: true, isSigner: false)
            AccountMeta::new(destination_collateral, false),
            // 3. obligation - obligation account being liquidated (isMut: true, isSigner: false)
            AccountMeta::new(obligation, false),
            // 4. reserve - the reserve we're repaying debt to (isMut: true, isSigner: false)
            AccountMeta::new(repay_reserve, false),
            // 5. reserveCollateralMint - collateral mint of withdraw reserve (isMut: true, isSigner: false)
            // ✅ DOĞRU: Reserve struct'ından alınan collateral_mint kullanılıyor
            AccountMeta::new(withdraw_reserve_collateral_mint, false),
            // 6. reserveLiquiditySupply - liquidity supply token account of repay reserve (isMut: true, isSigner: false)
            AccountMeta::new(repay_reserve_liquidity_supply, false),
            // 7. lendingMarket - lending market account (isMut: false, isSigner: false)
            AccountMeta::new_readonly(lending_market, false),
            // 8. lendingMarketAuthority - lending market authority PDA (isMut: false, isSigner: false)
            AccountMeta::new_readonly(lending_market_authority, false),
            // 9. destinationLiquidity - collateral supply token account of withdraw reserve (isMut: true, isSigner: false)
            AccountMeta::new(withdraw_reserve_collateral_supply, false),
            // 10. liquidator - liquidator pubkey (isMut: false, isSigner: true)
            AccountMeta::new_readonly(*liquidator, true),
            // 11. pythPrice - Pyth oracle account (isMut: false, isSigner: false)
            AccountMeta::new_readonly(pyth_oracle, false),
            // 12. switchboardPrice - Switchboard oracle account (isMut: false, isSigner: false)
            AccountMeta::new_readonly(switchboard_oracle, false),
            // 13. tokenProgram - SPL Token program (isMut: false, isSigner: false)
            AccountMeta::new_readonly(spl_token::id(), false),
        ];

        
        if accounts.len() != 13 {
            return Err(anyhow::anyhow!(
                "Account count mismatch: expected 13 accounts (per IDL), got {}",
                accounts.len()
            ));
        }
        
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&discriminator);
        data.extend_from_slice(&opportunity.max_liquidatable_amount.to_le_bytes());

        log::debug!(
            "Instruction data: discriminator={:02x?}, amount={}, data_len={}, data_hex={}",
            discriminator,
            opportunity.max_liquidatable_amount,
            data.len(),
            data.iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}
