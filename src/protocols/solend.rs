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

fn build_liquidation_accounts(
    source_liquidity: Pubkey,
    destination_collateral: Pubkey,
    repay_reserve: Pubkey,
    repay_reserve_liquidity_supply: Pubkey,
    withdraw_reserve: Pubkey,
    withdraw_reserve_collateral_supply: Pubkey,
    obligation: Pubkey,
    lending_market: Pubkey,
    lending_market_authority: Pubkey,
    liquidator: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(destination_collateral, false),
        AccountMeta::new(repay_reserve, false),
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new(withdraw_reserve, false),
        AccountMeta::new(withdraw_reserve_collateral_supply, false),
        AccountMeta::new(obligation, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new_readonly(liquidator, true),
        AccountMeta::new_readonly(solana_sdk::sysvar::clock::id(), false),
        AccountMeta::new_readonly(spl_token::id(), false),
    ]
}

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
        /// WAD (Wei-scAlar Decimal) format constant: 1e18
        /// 
        /// Solend uses WAD format for all decimal values, consistent with:
        /// - Solend SDK (src/state/obligation.ts)
        /// - Solana's decimal representation standard
        /// - Compound/MakerDAO WAD format (1e18)
        /// 
        /// Reference: https://docs.solend.fi/developers/protocol-overview
        pub const WAD: f64 = 1_000_000_000_000_000_000.0; // 1e18
        
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
    config: Option<crate::config::Config>,
}

impl SolendProtocol {
    /// Deprecated: Use `new_with_config` instead. This method uses a hardcoded program ID.
    pub const SOLEND_PROGRAM_ID: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";

    /// Create SolendProtocol with config (recommended)
    pub fn new_with_config(config: &crate::config::Config) -> Result<Self> {
        let program_id = Pubkey::try_from(config.solend_program_id.as_str())
            .context("Invalid Solend program ID from config")?;

        Ok(SolendProtocol {
            program_id,
            config: Some(config.clone()),
        })
    }

    /// Create SolendProtocol with default hardcoded program ID (for backward compatibility)
    pub fn new() -> Result<Self> {
        let program_id =
            Pubkey::try_from(Self::SOLEND_PROGRAM_ID).context("Invalid Solend program ID")?;

        Ok(SolendProtocol {
            program_id,
            config: None,
        })
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
        
        let source_liquidity = get_associated_token_address(liquidator, &debt_mint, self.config.as_ref())?;
        let destination_collateral =
            get_associated_token_address(liquidator, &withdraw_reserve_collateral_mint, self.config.as_ref())?;

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
        
        // ⚠️ CRITICAL: Filter by account size to avoid trying to parse reserves as obligations
        // Reserve accounts are typically 619-1300 bytes (with padding)
        // Obligation accounts are typically smaller (200-800 bytes depending on deposits/borrows)
        // If account is exactly 1300 bytes, it's almost certainly a reserve, not an obligation
        let data_size = account_data.data.len();
        if data_size >= 1200 {
            // Very large accounts (>=1200 bytes) are almost certainly reserves, not obligations
            // Skip parsing to avoid false errors
            log::debug!(
                "Skipping large account {} ({} bytes) - likely a reserve, not an obligation",
                account_address,
                data_size
            );
            return Ok(None);
        }
        
        // Check first byte - if it's a high version number (>=200), it's likely a reserve
        if data_size > 0 {
            let first_byte = account_data.data[0];
            if first_byte >= 200 {
                // Reserve version numbers are typically 0, 1, or high numbers (252-255)
                // Obligation version is typically 0 or 1
                log::debug!(
                    "Skipping account {} with high version byte 0x{:02x} ({} - likely a reserve)",
                    account_address,
                    first_byte,
                    first_byte
                );
                return Ok(None);
            }
        }
        
        let obligation = match SolendObligation::from_account_data(&account_data.data) {
            Ok(obligation) => obligation,
            Err(e) => {
                // Most Solend program accounts are NOT obligations (reserves, markets, config, etc.).
                // During get_program_accounts scans we optimistically try to parse everything as an
                // obligation and fall back to `Ok(None)` on failure. This is expected and not an
                // error condition, so we log it only at debug level to avoid noisy WARN spam.
                // However, if we're in initial discovery and finding 0 obligations, we need more detail.
                log::debug!(
                    "Skipping non-obligation Solend account {}: {}",
                    account_address,
                    e
                );
                
                // Log detailed info for first few parse errors to help diagnose struct issues
                static PARSE_ERROR_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let error_count = PARSE_ERROR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                
                if error_count < 3 {
                    let data_size = account_data.data.len();
                    let hex_dump: String = account_data.data
                        .iter()
                        .take(128) // First 128 bytes
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .chunks(32)
                        .map(|chunk| chunk.join(" "))
                        .collect::<Vec<_>>()
                        .join("\n      ");
                    
                    log::warn!(
                        "⚠️  Obligation parse error #{} (account: {}):\n   Error: {}\n   Data size: {} bytes\n   First 128 bytes (hex):\n      {}",
                        error_count + 1,
                        account_address,
                        e,
                        data_size,
                        hex_dump
                    );
                    
                    // Try to extract version byte if possible
                    if data_size > 0 {
                        let version_byte = account_data.data[0];
                        log::warn!(
                            "   First byte (version?): 0x{:02x} ({})",
                            version_byte,
                            version_byte
                        );
                    }
                    
                    // Check if it might be a reserve instead
                    if data_size > 100 {
                        log::warn!(
                            "   Note: This might be a reserve account (not an obligation). \
                             Reserves are expected and not an error."
                        );
                    }
                }
                
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
            let (mint, ltv, liquidation_threshold) =
                match get_reserve_info(&deposit.deposit_reserve, rpc_client.as_ref()).await {
                    Some(reserve_info) => {
                        let mint = reserve_info.collateral_mint.unwrap_or_else(|| {
                            reserve_info
                                .liquidity_mint
                                .unwrap_or(deposit.deposit_reserve)
                        });
                        (mint.to_string(), reserve_info.ltv, reserve_info.liquidation_threshold)
                    }
                    None => {
                        log::error!(
                            "❌ CRITICAL: Failed to get reserve info for {}. Cannot determine LTV/liquidation_threshold. Skipping this collateral.",
                            deposit.deposit_reserve
                        );
                        log::error!(
                            "   This should not happen in production. Reserve account should be parseable."
                        );
                        // Skip this collateral - we cannot safely calculate health factor without reserve info
                        continue;
                    }
                };

            collateral_assets.push(crate::domain::CollateralAsset {
                mint,
                amount: deposit.deposited_amount,
                amount_usd: deposit.market_value.to_f64(),
                ltv,
                liquidation_threshold,
            });
        }

        let mut debt_assets = Vec::new();
        for borrow in &obligation.borrows {
            let borrowed_amount = (borrow.borrowed_amount_wad / solend_idl::Number::WAD as u128) as u64;

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

        if position.total_debt_usd == 0.0 {
            return Ok(f64::INFINITY);
        }

        let weighted_collateral: f64 = position
            .collateral_assets
            .iter()
            .map(|asset| asset.amount_usd * asset.liquidation_threshold)
            .sum();
        
        Ok(weighted_collateral / position.total_debt_usd)
    }

    fn get_liquidation_params(&self) -> LiquidationParams {
        let config = self.config.as_ref();
        LiquidationParams {
            liquidation_bonus: config.map(|c| c.liquidation_bonus).unwrap_or(0.05),
            close_factor: config.map(|c| c.close_factor).unwrap_or(0.5),
            max_liquidation_slippage: config.map(|c| c.max_liquidation_slippage).unwrap_or(0.01),
        }
    }

    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction> {
        let discriminator: [u8; 8] = {
            let mut hasher = Sha256::new();
            hasher.update(b"global:liquidateObligation");
            hasher.finalize()[0..8]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to create discriminator"))?
        };
        
        let accounts = if let Some(rpc) = rpc_client.as_ref() {
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
                _,
            ) = self.resolve_liquidation_accounts(opportunity, liquidator, rpc).await?;
            
            build_liquidation_accounts(
                source_liquidity,
                destination_collateral,
                repay_reserve,
                repay_reserve_liquidity_supply,
                withdraw_reserve,
                withdraw_reserve_collateral_supply,
                obligation,
                lending_market,
                lending_market_authority,
                *liquidator,
            )
        } else {
            log::warn!("RPC client not provided, using placeholder accounts");
            return Err(anyhow::anyhow!("RPC client required for liquidation"));
        };
        
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&discriminator);
        data.extend_from_slice(&opportunity.max_liquidatable_amount.to_le_bytes());

        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}
