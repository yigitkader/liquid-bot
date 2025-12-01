use anyhow::{Context, Result};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use crate::core::types::Opportunity;
use crate::blockchain::rpc_client::RpcClient;
use crate::protocol::solend::accounts::{
    get_associated_token_address,
    derive_lending_market_authority,
};
use crate::protocol::oracle::{get_pyth_oracle_account, get_switchboard_oracle_account};
use crate::core::config::Config;
use std::sync::Arc;
use std::str::FromStr;
use sha2::{Sha256, Digest};
use spl_token;

fn get_instruction_discriminator() -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"global:liquidateObligation");
    let hash = hasher.finalize();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}

async fn get_reserve_address_from_mint(
    mint: &Pubkey,
    rpc: &Arc<RpcClient>,
    config: &Config,
) -> Result<Pubkey> {
    let _mint_str = mint.to_string();
    
    if let Some(usdc_mint) = Pubkey::from_str(&config.usdc_mint).ok() {
        if *mint == usdc_mint {
            if let Some(usdc_reserve_str) = &config.usdc_reserve_address {
                return Pubkey::from_str(usdc_reserve_str)
                    .context("Invalid USDC reserve address in config");
            }
        }
    }
    
    if let Some(sol_mint) = Pubkey::from_str(&config.sol_mint).ok() {
        if *mint == sol_mint {
            if let Some(sol_reserve_str) = &config.sol_reserve_address {
                return Pubkey::from_str(sol_reserve_str)
                    .context("Invalid SOL reserve address in config");
            }
        }
    }
    
    let program_id = Pubkey::from_str(&config.solend_program_id)
        .context("Invalid Solend program ID")?;
    
    let _market_address = config.main_lending_market_address.as_ref()
        .and_then(|s| Pubkey::from_str(s).ok())
        .ok_or_else(|| anyhow::anyhow!("Main lending market address not configured"))?;
    
    let accounts = rpc.get_program_accounts(&program_id).await
        .context("Failed to fetch program accounts for reserve lookup")?;
    
    for (pubkey, account) in accounts {
        if account.data.len() < 8 {
            continue;
        }
        
        let reserve_discriminator = &account.data[0..8];
        let expected_discriminator = get_reserve_discriminator();
        
        if reserve_discriminator == expected_discriminator {
            if account.data.len() >= 200 {
                let liquidity_mint_offset = 8 + 32 + 32;
                if account.data.len() > liquidity_mint_offset + 32 {
                    let liquidity_mint_bytes: [u8; 32] = account.data[liquidity_mint_offset..liquidity_mint_offset + 32]
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("Invalid reserve data"))?;
                    let liquidity_mint = Pubkey::try_from(liquidity_mint_bytes)
                        .map_err(|_| anyhow::anyhow!("Invalid mint in reserve"))?;
                    
                    if liquidity_mint == *mint {
                        return Ok(pubkey);
                    }
                }
            }
        }
    }
    
    Err(anyhow::anyhow!("Reserve not found for mint: {}", mint))
}

fn get_reserve_discriminator() -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"account:Reserve");
    let hash = hasher.finalize();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}

async fn get_reserve_data(
    reserve: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<(Pubkey, Pubkey, Pubkey)> {
    let account = rpc.get_account(reserve).await
        .context("Failed to fetch reserve account")?;
    
    if account.data.len() < 200 {
        return Err(anyhow::anyhow!("Reserve account data too small"));
    }
    
    let collateral_mint_offset = 8 + 32;
    let liquidity_supply_offset = 8 + 32 + 32 + 32;
    
    if account.data.len() < liquidity_supply_offset + 32 {
        return Err(anyhow::anyhow!("Reserve account data incomplete"));
    }
    
    let collateral_mint_bytes: [u8; 32] = account.data[collateral_mint_offset..collateral_mint_offset + 32]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid reserve collateral mint data"))?;
    let collateral_mint = Pubkey::try_from(collateral_mint_bytes)
        .map_err(|_| anyhow::anyhow!("Invalid collateral mint"))?;
    
    let liquidity_supply_bytes: [u8; 32] = account.data[liquidity_supply_offset..liquidity_supply_offset + 32]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid reserve liquidity supply data"))?;
    let liquidity_supply = Pubkey::try_from(liquidity_supply_bytes)
        .map_err(|_| anyhow::anyhow!("Invalid liquidity supply"))?;
    
    Ok((collateral_mint, liquidity_supply, reserve.clone()))
}

pub async fn build_liquidate_obligation_ix(
    opportunity: &Opportunity,
    liquidator: &Pubkey,
    rpc: Option<Arc<RpcClient>>,
) -> Result<Instruction> {
    let rpc = rpc.ok_or_else(|| anyhow::anyhow!("RPC client required for liquidation instruction"))?;
    
    let config = crate::core::config::Config::from_env()
        .context("Failed to load config")?;
    
    let program_id = Pubkey::from_str(&config.solend_program_id)
        .context("Invalid Solend program ID")?;
    
    let obligation_address = opportunity.position.address;
    
    let obligation_account = rpc.get_account(&obligation_address).await
        .context("Failed to fetch obligation account")?;
    
    use crate::protocol::solend::types::SolendObligation;
    let obligation = SolendObligation::from_account_data(&obligation_account.data)
        .context("Failed to parse obligation")?;
    
    let lending_market = obligation.lending_market;
    
    let lending_market_authority = derive_lending_market_authority(&lending_market, &program_id)
        .context("Failed to derive lending market authority")?;
    
    let debt_reserve = get_reserve_address_from_mint(&opportunity.debt_mint, &rpc, &config).await
        .context("Failed to find debt reserve")?;
    
    let (debt_reserve_collateral_mint, debt_reserve_liquidity_supply, _) = get_reserve_data(&debt_reserve, &rpc).await
        .context("Failed to fetch debt reserve data")?;
    
    let collateral_reserve = get_reserve_address_from_mint(&opportunity.collateral_mint, &rpc, &config).await
        .context("Failed to find collateral reserve")?;
    
    let (_collateral_reserve_collateral_mint, collateral_reserve_liquidity_supply, _) = get_reserve_data(&collateral_reserve, &rpc).await
        .context("Failed to fetch collateral reserve data")?;
    
    let source_liquidity = get_associated_token_address(liquidator, &opportunity.debt_mint, Some(&config))
        .context("Failed to derive source liquidity ATA")?;
    
    let destination_collateral = get_associated_token_address(liquidator, &opportunity.collateral_mint, Some(&config))
        .context("Failed to derive destination collateral ATA")?;
    
    let pyth_price = get_pyth_oracle_account(&opportunity.debt_mint, Some(&config))
        .context("Failed to get Pyth oracle account")?
        .ok_or_else(|| anyhow::anyhow!("Pyth oracle not found for debt mint"))?;
    
    let switchboard_price = get_switchboard_oracle_account(&opportunity.debt_mint, Some(&config))
        .context("Failed to get Switchboard oracle account")?
        .or_else(|| {
            get_switchboard_oracle_account(&opportunity.collateral_mint, Some(&config))
                .ok()
                .flatten()
        });
    
    let switchboard_price = switchboard_price.unwrap_or(pyth_price);
    
    let token_program = spl_token::id();
    
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(destination_collateral, false),
        AccountMeta::new(obligation_address, false),
        AccountMeta::new(debt_reserve, false),
        AccountMeta::new(debt_reserve_collateral_mint, false),
        AccountMeta::new(debt_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new(collateral_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(*liquidator, true),
        AccountMeta::new_readonly(pyth_price, false),
        AccountMeta::new_readonly(switchboard_price, false),
        AccountMeta::new_readonly(token_program, false),
    ];
    
    let discriminator = get_instruction_discriminator();
    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(&opportunity.max_liquidatable.to_le_bytes());
    
    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}
