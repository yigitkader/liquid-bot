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
use crate::solend::solend_program_id;

/// Build single atomic transaction with flashloan
/// Flow:
/// 1. FlashLoan: Borrow debt_amount USDC from Solend (flash)
/// 2. LiquidateObligation: Repay debt, receive cSOL
/// 3. RedeemReserveCollateral: cSOL -> SOL
/// 4. Jupiter Swap: SOL -> USDC
/// 5. FlashRepayReserveLiquidity: Repay borrowed USDC + fee (EXPLICIT - REQUIRED!)
/// All in ONE transaction - atomicity guaranteed!
pub async fn build_flashloan_liquidation_tx(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: Hash,
    config: &Config,
) -> Result<Transaction> {
    let program_id = solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();
    
    // Get reserves
    let borrow_reserve = ctx.borrow_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx.deposit_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;
    
    // Get reserve addresses
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral().supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral().mintPubkey;
    
    // Validate reserve addresses
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_supply == Pubkey::default()
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }
    
    // Get user's token accounts
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity().mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);
    let destination_liquidity = get_associated_token_address(&wallet_pubkey, &deposit_reserve.liquidity().mintPubkey);
    
    // Validate ATAs exist
    if rpc.get_account(&source_liquidity).is_err() {
        return Err(anyhow::anyhow!(
            "Source liquidity ATA does not exist: {}. Please create ATA for token {} before liquidation.",
            source_liquidity,
            borrow_reserve.liquidity().mintPubkey
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
            deposit_reserve.liquidity().mintPubkey
        ));
    }
    
    // ============================================================================
    // INSTRUCTION 1: RefreshReserve (Borrow Reserve)
    // ============================================================================
    // CRITICAL: Must refresh reserves BEFORE RefreshObligation and LiquidateObligation
    // This updates the reserve's accumulated interest and oracle prices
    let borrow_pyth_oracle = borrow_reserve.oracle_pubkey();
    let borrow_switchboard_oracle = borrow_reserve.liquidity().liquiditySwitchboardOracle;
    
    let refresh_borrow_reserve_accounts = vec![
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),    // reserve
        AccountMeta::new_readonly(borrow_pyth_oracle, false),      // pythOracle
        AccountMeta::new_readonly(borrow_switchboard_oracle, false), // switchboardOracle
        AccountMeta::new_readonly(sysvar::clock::id(), false),     // clock
    ];
    
    let refresh_borrow_reserve_ix = Instruction {
        program_id,
        accounts: refresh_borrow_reserve_accounts,
        data: vec![crate::solend::get_refresh_reserve_discriminator()],
    };
    
    // ============================================================================
    // INSTRUCTION 2: RefreshReserve (Deposit Reserve)
    // ============================================================================
    let deposit_pyth_oracle = deposit_reserve.oracle_pubkey();
    let deposit_switchboard_oracle = deposit_reserve.liquidity().liquiditySwitchboardOracle;
    
    let refresh_deposit_reserve_accounts = vec![
        AccountMeta::new(ctx.deposits[0].depositReserve, false),   // reserve
        AccountMeta::new_readonly(deposit_pyth_oracle, false),      // pythOracle
        AccountMeta::new_readonly(deposit_switchboard_oracle, false), // switchboardOracle
        AccountMeta::new_readonly(sysvar::clock::id(), false),      // clock
    ];
    
    let refresh_deposit_reserve_ix = Instruction {
        program_id,
        accounts: refresh_deposit_reserve_accounts,
        data: vec![crate::solend::get_refresh_reserve_discriminator()],
    };
    
    // ============================================================================
    // INSTRUCTION 3: RefreshObligation
    // ============================================================================
    // CRITICAL: Must be called after RefreshReserve and before LiquidateObligation
    // This updates the obligation's borrowedValue, allowedBorrowValue with current prices/interest
    let mut refresh_obligation_accounts = vec![
        AccountMeta::new(ctx.obligation_pubkey, false),             // obligation
        AccountMeta::new_readonly(sysvar::clock::id(), false),      // clock
    ];
    // Add all deposit reserves used by the obligation
    for deposit in &ctx.deposits {
        refresh_obligation_accounts.push(AccountMeta::new_readonly(deposit.depositReserve, false));
    }
    // Add all borrow reserves used by the obligation
    for borrow in &ctx.borrows {
        refresh_obligation_accounts.push(AccountMeta::new_readonly(borrow.borrowReserve, false));
    }
    
    let refresh_obligation_ix = Instruction {
        program_id,
        accounts: refresh_obligation_accounts,
        data: vec![crate::solend::get_refresh_obligation_discriminator()],
    };
    
    // ============================================================================
    // INSTRUCTION 4: FlashLoan (Solend native)
    // ============================================================================
    let flashloan_amount = quote.debt_to_repay_raw;
    
    let mut flashloan_data = vec![crate::solend::get_flashloan_discriminator()];
    flashloan_data.extend_from_slice(&flashloan_amount.to_le_bytes());
    
    let flashloan_accounts = vec![
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new_readonly(wallet_pubkey, true),
        AccountMeta::new_readonly(sysvar::instructions::id(), false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ];
    
    let flashloan_ix = Instruction {
        program_id,
        accounts: flashloan_accounts,
        data: flashloan_data,
    };
    
    // ============================================================================
    // INSTRUCTION 5: LiquidateObligation
    // ============================================================================
    let liquidity_amount = quote.debt_to_repay_raw;
    
    let mut liquidation_data = vec![crate::solend::get_liquidate_obligation_discriminator()];
    liquidation_data.extend_from_slice(&liquidity_amount.to_le_bytes());
    
    let liquidation_accounts = vec![
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(destination_collateral, false),
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(ctx.deposits[0].depositReserve, false),
        AccountMeta::new(withdraw_reserve_collateral_supply, false),
        AccountMeta::new(ctx.obligation_pubkey, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new_readonly(wallet_pubkey, true),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ];
    
    let liquidation_ix = Instruction {
        program_id,
        accounts: liquidation_accounts,
        data: liquidation_data,
    };
    
    // ============================================================================
    // INSTRUCTION 3: RedeemReserveCollateral
    // ============================================================================
    let redeem_collateral_amount = quote.collateral_to_seize_raw;
    
    let mut redeem_data = vec![crate::solend::get_redeem_reserve_collateral_discriminator()];
    redeem_data.extend_from_slice(&redeem_collateral_amount.to_le_bytes());
    
    let redeem_accounts = vec![
        AccountMeta::new(destination_collateral, false),
        AccountMeta::new(destination_liquidity, false),
        AccountMeta::new(ctx.deposits[0].depositReserve, false),
        AccountMeta::new(withdraw_reserve_collateral_mint, false),
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(lending_market_authority, false),
        AccountMeta::new_readonly(wallet_pubkey, true),
        AccountMeta::new_readonly(sysvar::clock::id(), false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ];
    
    let redeem_ix = Instruction {
        program_id,
        accounts: redeem_accounts,
        data: redeem_data,
    };
    
    // ============================================================================
    // INSTRUCTION 4: Jupiter Swap (SOL -> USDC)
    // ============================================================================
    let jupiter_swap_ixs = crate::jup::build_jupiter_swap_instruction(
        &quote.quote,
        &wallet_pubkey,
        &config.jupiter_url,
    )
        .await
    .context("Failed to build Jupiter swap instruction")?;
    
    // ============================================================================
    // INSTRUCTION 5: FlashRepayReserveLiquidity
    // ============================================================================
    let flashloan_fee_amount = quote.flashloan_fee_raw;
    let repay_amount = flashloan_amount + flashloan_fee_amount;
    
    let mut repay_data = vec![crate::solend::get_flashrepay_discriminator()];
    repay_data.extend_from_slice(&repay_amount.to_le_bytes());
    repay_data.extend_from_slice(&flashloan_amount.to_le_bytes());
    
    let reserve_liquidity_fee_receiver = repay_reserve_liquidity_supply;
    
    let repay_accounts = vec![
        AccountMeta::new(source_liquidity, false),
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new(reserve_liquidity_fee_receiver, false),
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),
        AccountMeta::new_readonly(lending_market, false),
        AccountMeta::new_readonly(wallet_pubkey, true),
        AccountMeta::new_readonly(sysvar::instructions::id(), false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ];
    
    let repay_ix = Instruction {
        program_id,
        accounts: repay_accounts,
        data: repay_data,
    };
    
    // ============================================================================
    // Compute Budget Instructions
    // ============================================================================
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;
    
    // Increased compute budget: RefreshReserve x2 + RefreshObligation + FlashLoan + Liquidate + Redeem + Jupiter + Repay
    // Each instruction needs ~50-100k CU, Jupiter swap can need 200-400k CU
    let mut compute_limit_data = vec![2u8];
    compute_limit_data.extend_from_slice(&(800_000u32).to_le_bytes()); // Increased from 500k to 800k
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };
    
    let mut compute_price_data = vec![3u8];
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes());
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };
    
    // ============================================================================
    // BUILD TRANSACTION
    // ============================================================================
    // Order is CRITICAL:
    // 1. Compute budget (first)
    // 2. RefreshReserve (borrow) - update reserve with current oracle prices
    // 3. RefreshReserve (deposit) - update reserve with current oracle prices  
    // 4. RefreshObligation - update obligation with current values
    // 5. FlashBorrow - borrow USDC
    // 6. LiquidateObligation - repay debt, receive cTokens
    // 7. RedeemReserveCollateral - convert cTokens to underlying
    // 8. Jupiter Swap - swap collateral to USDC
    // 9. FlashRepay - repay borrowed USDC + fee
    let mut instructions = vec![
        compute_budget_ix,
        priority_fee_ix,
        refresh_borrow_reserve_ix,   // NEW: Refresh borrow reserve first
        refresh_deposit_reserve_ix,  // NEW: Refresh deposit reserve
        refresh_obligation_ix,       // NEW: Then refresh obligation
        flashloan_ix,
        liquidation_ix,
        redeem_ix,
    ];
    
    instructions.extend(jupiter_swap_ixs);
    instructions.push(repay_ix);
    
    let mut tx = Transaction::new_with_payer(
        &instructions,
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;
    
    log::info!(
        "Built atomic flashloan liquidation transaction for obligation {}:\n\
         - RefreshReserve (borrow): {}\n\
         - RefreshReserve (deposit): {}\n\
         - RefreshObligation: {}\n\
         - FlashBorrow: {} USDC\n\
         - Liquidate: {} debt tokens\n\
         - Redeem: {} cTokens\n\
         - Jupiter Swap: {} instructions\n\
         - FlashRepay: {} USDC",
        ctx.obligation_pubkey,
        ctx.borrows[0].borrowReserve,
        ctx.deposits[0].depositReserve,
        ctx.obligation_pubkey,
        flashloan_amount,
        quote.debt_to_repay_raw,
        quote.collateral_to_seize_raw,
        instructions.len() - 8, // Subtract fixed instructions
        repay_amount
    );
    
    Ok(tx)
}

