// Liquidation transaction building module
// Moved from pipeline.rs to reduce code size and improve maintainability

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    sysvar,
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address;
use spl_token::ID as TOKEN_PROGRAM_ID;
use std::str::FromStr;
use std::sync::Arc;

use crate::pipeline::{Config, LiquidationContext, LiquidationQuote};

/// Build single atomic transaction with flashloan
/// Kamino Lend protocol only
pub async fn build_flashloan_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: Hash,
    config: &Config,
) -> Result<Transaction> {
    build_kamino_flashloan_liquidation_tx(wallet, ctx, quote, rpc, blockhash, config).await
}

/// Build Kamino flashloan liquidation transaction
async fn build_kamino_flashloan_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: Hash,
    config: &Config,
) -> Result<Transaction> {
    use crate::kamino::{Reserve as KaminoReserve, Obligation as KaminoObligation, ObligationLiquidity, ObligationCollateral};
    
    let obligation = &ctx.obligation;
    let borrows = &ctx.borrows;
    let deposits = &ctx.deposits;
    let borrow_reserve = &ctx.borrow_reserve;
    let deposit_reserve = &ctx.deposit_reserve;
    let obligation_pubkey = ctx.obligation_pubkey;
    
    // Get program ID
    let program_id = Pubkey::from_str(crate::kamino::KAMINO_LEND_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Kamino program ID: {}", e))?;
    let wallet_pubkey = wallet.pubkey();
    
    // Get reserves
    let borrow_reserve = borrow_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = deposit_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    if borrows.is_empty() || deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in Kamino obligation"));
    }
    
    // Get reserve addresses
    let borrow_reserve_pubkey = borrows[0].borrow_reserve;
    let deposit_reserve_pubkey = deposits[0].deposit_reserve;
    
    // Get lending market from obligation
    let lending_market = obligation.lending_market;
    
    // Get reserve liquidity and collateral supply addresses
    // TODO: These need to be derived PDAs or fetched from reserve accounts
    // For now, using placeholder - need to implement proper PDA derivation
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity.supply_vault;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral.supply_vault;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral.mint_pubkey;
    
    // Get user's token accounts
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.mint_pubkey());
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);
    let destination_liquidity = get_associated_token_address(&wallet_pubkey, &deposit_reserve.mint_pubkey());
    
    // Validate ATAs exist
    if rpc.get_account(&source_liquidity).is_err() {
        return Err(anyhow::anyhow!(
            "Source liquidity ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            source_liquidity,
            borrow_reserve.mint_pubkey()
        ));
    }
    if rpc.get_account(&destination_collateral).is_err() {
        return Err(anyhow::anyhow!(
            "Destination collateral ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            destination_collateral,
            withdraw_reserve_collateral_mint
        ));
    }
    if rpc.get_account(&destination_liquidity).is_err() {
        return Err(anyhow::anyhow!(
            "Destination liquidity ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            destination_liquidity,
            deposit_reserve.mint_pubkey()
        ));
    }
    
    // ============================================================================
    // INSTRUCTION 1: RefreshReserve (Borrow Reserve)
    // ============================================================================
    let borrow_pyth_oracle = borrow_reserve.pyth_oracle();
    let borrow_switchboard_oracle = borrow_reserve.switchboard_oracle();
    
    let refresh_borrow_reserve_accounts = vec![
        AccountMeta::new(borrow_reserve_pubkey, false),
        AccountMeta::new_readonly(borrow_pyth_oracle, false),
        AccountMeta::new_readonly(borrow_switchboard_oracle, false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
    ];
    
    let refresh_borrow_reserve_ix = Instruction {
        program_id,
        accounts: refresh_borrow_reserve_accounts,
        data: crate::kamino::get_refresh_reserve_discriminator(),
    };
    
    // ============================================================================
    // INSTRUCTION 2: RefreshReserve (Deposit Reserve)
    // ============================================================================
    let deposit_pyth_oracle = deposit_reserve.pyth_oracle();
    let deposit_switchboard_oracle = deposit_reserve.switchboard_oracle();
    
    let refresh_deposit_reserve_accounts = vec![
        AccountMeta::new(deposit_reserve_pubkey, false),
        AccountMeta::new_readonly(deposit_pyth_oracle, false),
        AccountMeta::new_readonly(deposit_switchboard_oracle, false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
    ];
    
    let refresh_deposit_reserve_ix = Instruction {
        program_id,
        accounts: refresh_deposit_reserve_accounts,
        data: crate::kamino::get_refresh_reserve_discriminator(),
    };
    
    // ============================================================================
    // INSTRUCTION 3: RefreshObligation
    // ============================================================================
    let mut refresh_obligation_accounts = vec![
        AccountMeta::new(obligation_pubkey, false),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
    ];
    
    // Add all deposit reserves
    for deposit in deposits {
        refresh_obligation_accounts.push(AccountMeta::new_readonly(deposit.deposit_reserve, false));
    }
    
    // Add all borrow reserves
    for borrow in borrows {
        refresh_obligation_accounts.push(AccountMeta::new_readonly(borrow.borrow_reserve, false));
    }
    
    let refresh_obligation_ix = Instruction {
        program_id,
        accounts: refresh_obligation_accounts,
        data: crate::kamino::get_refresh_obligation_discriminator(),
    };
    
    // ============================================================================
    // INSTRUCTION 4: FlashLoan (if Kamino supports it)
    // ============================================================================
    // TODO: Implement Kamino flashloan if supported
    // For now, assuming user provides liquidity upfront
    let flashloan_amount = quote.debt_to_repay_raw;
    
    // ============================================================================
    // INSTRUCTION 5: LiquidateObligation
    // ============================================================================
    let liquidity_amount = quote.debt_to_repay_raw;
    
    let mut liquidation_data = crate::kamino::get_liquidate_obligation_discriminator();
    liquidation_data.extend_from_slice(&liquidity_amount.to_le_bytes());
    
    // TODO: Verify account order from Kamino IDL
    // Placeholder account order based on Anchor pattern
    let liquidation_accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // source_liquidity
        AccountMeta::new(destination_collateral, false),              // destination_collateral
        AccountMeta::new(borrow_reserve_pubkey, false),               // repay_reserve
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // repay_reserve_liquidity_supply
        AccountMeta::new_readonly(deposit_reserve_pubkey, false),     // withdraw_reserve
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // withdraw_reserve_collateral_supply
        AccountMeta::new(obligation_pubkey, false),                  // obligation
        AccountMeta::new_readonly(lending_market, false),             // lending_market
        AccountMeta::new_readonly(wallet_pubkey, true),                // transfer_authority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // clock
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),            // token_program
    ];
    
    let liquidation_ix = Instruction {
        program_id,
        accounts: liquidation_accounts,
        data: liquidation_data,
    };
    
    // ============================================================================
    // INSTRUCTION 6: RedeemReserveCollateral
    // ============================================================================
    let redeem_collateral_amount = quote.collateral_to_seize_raw;
    
    let mut redeem_data = crate::kamino::get_redeem_reserve_collateral_discriminator();
    redeem_data.extend_from_slice(&redeem_collateral_amount.to_le_bytes());
    
    // TODO: Verify account order from Kamino IDL
    let redeem_accounts = vec![
        AccountMeta::new(destination_collateral, false),              // source_collateral
        AccountMeta::new(destination_liquidity, false),               // destination_liquidity
        AccountMeta::new(deposit_reserve_pubkey, false),              // reserve
        AccountMeta::new(withdraw_reserve_collateral_mint, false),    // reserve_collateral_mint
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // reserve_collateral_supply
        AccountMeta::new_readonly(lending_market, false),             // lending_market
        AccountMeta::new_readonly(wallet_pubkey, true),                // transfer_authority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // clock
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),           // token_program
    ];
    
    let redeem_ix = Instruction {
        program_id,
        accounts: redeem_accounts,
        data: redeem_data,
    };
    
    // ============================================================================
    // INSTRUCTION 7: Jupiter Swap (SOL -> Debt Token)
    // ============================================================================
    // TODO: Add Jupiter swap instruction
    
    // ============================================================================
    // INSTRUCTION 8: FlashRepay (if flashloan was used)
    // ============================================================================
    // TODO: Implement flash repay if flashloan was used
    
    // ============================================================================
    // BUILD TRANSACTION
    // ============================================================================
    // Compute budget
    let mut compute_limit_data = vec![2u8];
    compute_limit_data.extend_from_slice(&(800_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: solana_sdk::compute_budget::id(),
        accounts: vec![],
        data: compute_limit_data,
    };
    
    // Priority fee
    let priority_fee_per_cu = std::env::var("PRIORITY_FEE_PER_CU")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1000);
    let mut priority_fee_data = vec![3u8];
    priority_fee_data.extend_from_slice(&priority_fee_per_cu.to_le_bytes());
    let priority_fee_ix = Instruction {
        program_id: solana_sdk::compute_budget::id(),
        accounts: vec![],
        data: priority_fee_data,
    };
    
    let mut instructions = vec![
        compute_budget_ix,
        priority_fee_ix,
        refresh_borrow_reserve_ix,
        refresh_deposit_reserve_ix,
        refresh_obligation_ix,
        liquidation_ix,
        redeem_ix,
    ];
    
    // TODO: Add Jupiter swap and flash repay instructions
    
    let mut tx = Transaction::new_with_payer(&instructions, Some(&wallet_pubkey));
    tx.message.recent_blockhash = blockhash;
    tx.sign(&[wallet], blockhash);
    
    log::info!(
        "✅ Built Kamino liquidation transaction for obligation {}:\n\
         - RefreshReserve (borrow): {}\n\
         - RefreshReserve (deposit): {}\n\
         - RefreshObligation: {}\n\
         - LiquidateObligation: {} debt tokens\n\
         - RedeemReserveCollateral: {} collateral tokens",
        obligation_pubkey,
        borrow_reserve_pubkey,
        deposit_reserve_pubkey,
        obligation_pubkey,
        liquidity_amount,
        redeem_collateral_amount
    );
    
    Ok(tx)
}


