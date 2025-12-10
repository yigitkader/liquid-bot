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
        AccountMeta::new(*obligation_pubkey, false),
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
        AccountMeta::new(*obligation_pubkey, false),                  // obligation
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

/// Build Solend flashloan liquidation transaction (REMOVED - legacy protocol)
#[allow(dead_code)]
async fn _build_solend_flashloan_liquidation_tx_removed(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: Hash,
    config: &Config,
) -> Result<Transaction> {
    use crate::solend::{Obligation as SolendObligation, Reserve as SolendReserve};
    
    let LiquidationContext::Solend {
        obligation,
        borrows,
        deposits,
        borrow_reserve,
        deposit_reserve,
        obligation_pubkey,
        ..
    } = ctx else {
        return Err(anyhow::anyhow!("Expected Solend context"));
    };
    
    let program_id = solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();
    
    // Get reserves
    let borrow_reserve = borrow_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = deposit_reserve.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let lending_market = obligation.lendingMarket;
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
        AccountMeta::new(borrows[0].borrowReserve, false),    // reserve
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
        AccountMeta::new(deposits[0].depositReserve, false),   // reserve
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
        AccountMeta::new(*obligation_pubkey, false),             // obligation
        AccountMeta::new_readonly(sysvar::clock::id(), false),      // clock
    ];
    // Add all deposit reserves used by the obligation
    for deposit in deposits {
        refresh_obligation_accounts.push(AccountMeta::new_readonly(deposit.depositReserve, false));
    }
    // Add all borrow reserves used by the obligation
    for borrow in borrows {
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
        AccountMeta::new(borrows[0].borrowReserve, false),
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
        AccountMeta::new(borrows[0].borrowReserve, false),
        AccountMeta::new(repay_reserve_liquidity_supply, false),
        AccountMeta::new_readonly(deposits[0].depositReserve, false),
        AccountMeta::new(withdraw_reserve_collateral_supply, false),
        AccountMeta::new(*obligation_pubkey, false),
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
        AccountMeta::new(deposits[0].depositReserve, false),
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
        AccountMeta::new(borrows[0].borrowReserve, false),
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
        obligation_pubkey,
        borrows[0].borrowReserve,
        deposits[0].depositReserve,
        obligation_pubkey,
        flashloan_amount,
        quote.debt_to_repay_raw,
        quote.collateral_to_seize_raw,
        instructions.len() - 8, // Subtract fixed instructions
        repay_amount
    );
    
    Ok(tx)
}

