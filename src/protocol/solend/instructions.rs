use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::core::types::Opportunity;
use crate::protocol::solend::accounts::{
    derive_lending_market_authority, get_associated_token_address,
};
use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use spl_token;
use std::str::FromStr;
use std::sync::Arc;

// Reserve cache artık reserve_cache modülünde
use crate::protocol::solend::reserve_cache;
pub use reserve_cache::{
    start_reserve_cache_refresh,
    wait_for_reserve_cache_initialization,
    get_reserve_address_from_mint,
    get_reserve_data,
};

/// Get instruction discriminator for liquidateObligation
/// 
/// Calculates SHA256("global:liquidateObligation")[..8]
/// This matches Anchor's instruction discriminator calculation.
pub fn get_instruction_discriminator() -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(b"global:liquidateObligation");
    let hash = hasher.finalize();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}

// Reserve cache functions artık reserve_cache modülünde (yukarıda pub use ile export edildi)

pub async fn build_liquidate_obligation_ix(
    opportunity: &Opportunity,
    liquidator: &Pubkey,
    rpc: Option<Arc<RpcClient>>,
    config: &crate::core::config::Config,
) -> Result<Instruction> {
    // ✅ CRITICAL FIX: Accept Config as parameter instead of calling from_env()
    // Problem: Previous code called Config::from_env() here, which could lead to:
    //   - Inconsistent config values if env vars change during runtime
    //   - Different behavior in test vs prod if different .env files are used
    //   - Non-deterministic behavior if config is loaded multiple times
    // Solution: Accept Config as parameter (dependency injection)
    //   - Config is loaded once in main.rs and passed to all components
    //   - Ensures consistent config values throughout the application lifecycle
    //   - Makes testing easier (can inject test config)
    let rpc =
        rpc.ok_or_else(|| anyhow::anyhow!("RPC client required for liquidation instruction"))?;

    let program_id =
        Pubkey::from_str(&config.solend_program_id).context("Invalid Solend program ID")?;

    let obligation_address = opportunity.position.address;

    let obligation_account = retry_with_backoff(
        || rpc.get_account(&obligation_address),
        RetryConfig::for_rpc(),
    )
    .await
    .context("Failed to fetch obligation account")?;

    use crate::protocol::solend::types::SolendObligation;
    let obligation = SolendObligation::from_account_data(&obligation_account.data)
        .context("Failed to parse obligation")?;

    let lending_market = obligation.lending_market;

    let lending_market_authority = derive_lending_market_authority(&lending_market, &program_id)
        .context("Failed to derive lending market authority")?;

    let debt_reserve = get_reserve_address_from_mint(&opportunity.debt_mint, &rpc, &config)
        .await
        .context("Failed to find debt reserve")?;

    let (_debt_reserve_collateral_mint, debt_reserve_liquidity_supply, _, _debt_reserve_ltv) =
        get_reserve_data(&debt_reserve, &rpc)
            .await
            .context("Failed to fetch debt reserve data")?;

    let collateral_reserve =
        get_reserve_address_from_mint(&opportunity.collateral_mint, &rpc, &config)
            .await
            .context("Failed to find collateral reserve")?;

    let (collateral_reserve_collateral_mint, collateral_reserve_liquidity_supply, _, _collateral_reserve_ltv) =
        get_reserve_data(&collateral_reserve, &rpc)
            .await
            .context("Failed to fetch collateral reserve data")?;

    // ✅ FIX: Solend protocol uses Wrapped SOL (WSOL) for SOL, not native SOL
    // Problem: Previous code used wallet address directly for SOL, but Solend is an SPL token protocol
    //   Solend requires SPL token accounts (ATA) for all tokens, including SOL
    //   SOL must be wrapped as WSOL and stored in an ATA to be used in Solend
    // Solution: Use WSOL ATA for SOL, same as other SPL tokens
    //   Config.sol_mint should be WSOL mint: So11111111111111111111111111111111111111112
    //   All tokens (including SOL/WSOL) require ATA accounts in Solend protocol
    
    // ✅ FIX: Always use ATA for all tokens, including SOL (WSOL)
    // Solend protocol requires SPL token accounts for all operations
    // SOL must be wrapped as WSOL and stored in ATA
    let source_liquidity = get_associated_token_address(liquidator, &opportunity.debt_mint, Some(&config))
        .context("Failed to derive source liquidity ATA")?;

    let destination_collateral = get_associated_token_address(liquidator, &opportunity.collateral_mint, Some(&config))
        .context("Failed to derive destination collateral ATA")?;

    let token_program = spl_token::id();
    
    // ✅ FIX: Add clock sysvar (required by Solend liquidateObligation instruction)
    let clock_sysvar = solana_sdk::sysvar::clock::id();

    // ✅ FIX: Account order matches IDL exactly (idl/solend.json)
    // IDL order for liquidateObligation instruction:
    // 1. sourceLiquidity (isMut: true, isSigner: false)
    // 2. destinationCollateral (isMut: true, isSigner: false)
    // 3. repayReserve (isMut: true, isSigner: false)
    // 4. repayReserveLiquiditySupply (isMut: true, isSigner: false)
    // 5. withdrawReserve (isMut: true, isSigner: false)
    // 6. withdrawReserveCollateralMint (isMut: true, isSigner: false)
    // 7. withdrawReserveLiquiditySupply (isMut: true, isSigner: false)
    // 8. obligation (isMut: true, isSigner: false)
    // 9. lendingMarket (isMut: false, isSigner: false)
    // 10. lendingMarketAuthority (isMut: false, isSigner: false)
    // 11. transferAuthority (isMut: false, isSigner: true)
    // 12. clockSysvar (isMut: false, isSigner: false)
    // 13. tokenProgram (isMut: false, isSigner: false)
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),                          // 1. sourceLiquidity
        AccountMeta::new(destination_collateral, false),                    // 2. destinationCollateral
        AccountMeta::new(debt_reserve, false),                              // 3. repayReserve
        AccountMeta::new(debt_reserve_liquidity_supply, false),             // 4. repayReserveLiquiditySupply
        AccountMeta::new(collateral_reserve, false),                        // 5. withdrawReserve
        AccountMeta::new(collateral_reserve_collateral_mint, false),        // 6. withdrawReserveCollateralMint
        AccountMeta::new(collateral_reserve_liquidity_supply, false),       // 7. withdrawReserveLiquiditySupply
        AccountMeta::new(obligation_address, false),                        // 8. obligation
        AccountMeta::new_readonly(lending_market, false),                   // 9. lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),         // 10. lendingMarketAuthority
        AccountMeta::new_readonly(*liquidator, true),                        // 11. transferAuthority (signer)
        AccountMeta::new_readonly(clock_sysvar, false),                     // 12. clockSysvar
        AccountMeta::new_readonly(token_program, false),                     // 13. tokenProgram
    ];

    // ✅ CRITICAL FIX: Convert max_liquidatable from USD×1e6 to token amount
    // Problem: opportunity.max_liquidatable is stored as USD×1e6 (micro-USD)
    //   But Solend liquidateObligation instruction expects token amount in smallest unit (lamports/decimals)
    //   Example: SOL = 100 USD, max_liquidatable_usd = 100 → max_liquidatable = 100 * 1e6 = 100_000_000
    //   But 1 SOL (9 decimals) = 1_000_000_000 lamports
    //   So we need: (100 / 100) * 10^9 = 1_000_000_000 lamports (1 SOL), not 100_000_000
    // Solution: Convert USD amount to token amount using oracle price and token decimals
    use crate::utils::helpers::{read_mint_decimals, usd_to_token_amount};
    use crate::protocol::oracle::{get_oracle_accounts_from_mint, read_oracle_price};
    use std::sync::Arc;
    
    // Get oracle price for debt mint
    let (pyth_oracle, switchboard_oracle) = get_oracle_accounts_from_mint(&opportunity.debt_mint, Some(&config))
        .context("Failed to get oracle accounts for debt mint")?;
    
    let price_data = read_oracle_price(
        pyth_oracle.as_ref(),
        switchboard_oracle.as_ref(),
        Arc::clone(&rpc),
        Some(&config),
    )
    .await
    .context("Failed to read oracle price for debt mint")?
    .ok_or_else(|| anyhow::anyhow!("No oracle price data available for debt mint {}", opportunity.debt_mint))?;
    
    // Get decimals for debt mint
    let decimals = read_mint_decimals(&opportunity.debt_mint, &rpc)
        .await
        .context("Failed to read decimals for debt mint")?;
    
    // Convert USD×1e6 to token amount
    let max_liquidatable_token_amount = usd_to_token_amount(
        opportunity.max_liquidatable,
        price_data.price,
        decimals,
    )
    .context("Failed to convert max_liquidatable from USD to token amount")?;
    
    log::info!(
        "build_liquidate_obligation_ix: Converted max_liquidatable: USD×1e6={}, price={:.6}, decimals={}, token_amount={}",
        opportunity.max_liquidatable,
        price_data.price,
        decimals,
        max_liquidatable_token_amount
    );

    let discriminator = get_instruction_discriminator();
    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(&max_liquidatable_token_amount.to_le_bytes());

    Ok(Instruction {
        program_id,
        accounts,
        data,
    })
}

/// WSOL mint address (So11111111111111111111111111111111111111112)
pub fn get_wsol_mint() -> Pubkey {
    Pubkey::from_str("So11111111111111111111111111111111111111112")
        .expect("Invalid WSOL mint address")
}

/// Check if a mint is WSOL
pub fn is_wsol_mint(mint: &Pubkey) -> bool {
    *mint == get_wsol_mint()
}

/// Build instruction to wrap native SOL to WSOL
/// This transfers native SOL to WSOL ATA and syncs it to WSOL tokens
/// 
/// Steps:
/// 1. Check if WSOL ATA exists (via RPC if provided)
/// 2. If ATA doesn't exist, create it (create_associated_token_account instruction)
/// 3. Transfer native SOL directly to WSOL ATA address using system_instruction::transfer
/// 4. Sync native SOL in WSOL ATA to WSOL tokens using token_instruction::sync_native
/// 
/// ✅ FIX: WSOL ATA existence check and creation
/// Problem: Previous code assumed WSOL ATA exists, but transfer fails if ATA doesn't exist
/// Solution: Check ATA existence via RPC, create if needed, then transfer + sync_native
/// 
/// ✅ CORRECT APPROACH (per Solana documentation):
/// - Transfer native SOL directly to WSOL ATA address (not to owner)
/// - WSOL token accounts can receive native SOL directly via system_instruction::transfer
/// - Then call sync_native to convert native SOL (lamports) to WSOL tokens
/// 
/// References:
/// - Solana docs: https://solana.com/docs/tokens/basics/sync-native
/// - QuickNode guide: Native SOL should be sent to the associated token account for NATIVE_MINT
pub async fn build_wrap_sol_instruction(
    wallet: &Pubkey,
    wsol_ata: &Pubkey,
    amount: u64,
    rpc: Option<&Arc<RpcClient>>,
    config: Option<&Config>,
) -> Result<Vec<Instruction>> {
    use solana_sdk::system_instruction;
    use spl_token::instruction as token_instruction;
    
    if amount == 0 {
        return Err(anyhow::anyhow!("Wrap amount cannot be zero"));
    }
    
    if let (Some(rpc_client), Some(config)) = (rpc, config) {
        let wallet_account = retry_with_backoff(
            || rpc_client.get_account(wallet),
            RetryConfig::for_rpc(),
        )
        .await
        .context("Failed to fetch wallet account for native SOL balance check")?;
        let native_sol = wallet_account.lamports;
        let min_reserve = config.min_reserve_lamports;
        let required = amount.checked_add(min_reserve)
            .ok_or_else(|| anyhow::anyhow!("Wrap amount + reserve overflow"))?;
        
        if native_sol < required {
            return Err(anyhow::anyhow!(
                "Insufficient native SOL: need {} lamports (wrap: {} + reserve: {}), have {} lamports",
                required, amount, min_reserve, native_sol
            )).context("Cannot wrap SOL to WSOL - insufficient native SOL balance");
        }
    }
    
    let mut instructions = Vec::new();
    
    // ✅ FIX: Check if WSOL ATA exists, create if needed
    // Problem: Transfer to non-existent ATA fails
    // Solution: Check ATA existence via RPC, add create_ata instruction if needed
    let wsol_mint = get_wsol_mint();
    let needs_ata_creation = if let Some(rpc_client) = rpc {
        // Check if ATA exists via RPC
        match retry_with_backoff(
            || rpc_client.get_account(wsol_ata),
            RetryConfig::for_rpc(),
        )
        .await
        {
            Ok(_) => {
                log::debug!("WSOL ATA {} exists, skipping creation", wsol_ata);
                false
            }
            Err(e) => {
                let error_str = e.to_string().to_lowercase();
                if error_str.contains("accountnotfound") || error_str.contains("account not found") {
                    log::info!("WSOL ATA {} doesn't exist, will create it", wsol_ata);
                    true
                } else {
                    // For other errors (timeout, etc.), log warning but proceed
                    // create_ata is idempotent, so it's safe to add even if ATA exists
                    log::warn!("Failed to check WSOL ATA {} existence: {}. Will add create_ata instruction as fallback (idempotent)", wsol_ata, e);
                    true
                }
            }
        }
    } else {
        // No RPC provided - assume ATA needs to be created (safe fallback)
        // create_ata is idempotent, so it won't fail if ATA already exists
        log::warn!("No RPC provided to build_wrap_sol_instruction, adding create_ata instruction as fallback (idempotent)");
        true
    };
    
    // Step 1: Create WSOL ATA if needed
    if needs_ata_creation {
        use crate::utils::ata_manager::create_ata_instruction;
        
        // Use centralized ATA creation function
        let create_ata_ix = create_ata_instruction(
            wallet,
            wallet,
            &wsol_mint,
            config,
            rpc,
        )
        .await
        .context("Failed to create WSOL ATA instruction")?;
        
        instructions.push(create_ata_ix);
    }
    
    // Step 2: Transfer native SOL directly to WSOL ATA address
    // WSOL token accounts are special SPL token accounts that can hold native SOL (lamports).
    // When you send native SOL to a WSOL token account via system_instruction::transfer,
    // the lamports are deposited into that account, but the token amount field is not
    // automatically updated. That's why sync_native is needed.
    // 
    // This is the standard and correct way to wrap SOL in Solana:
    // 1. system_instruction::transfer(wallet, wsol_ata, amount) - sends native SOL to WSOL ATA
    // 2. token_instruction::sync_native() - syncs lamports to WSOL token amount
    // 
    // ⚠️ CRITICAL REQUIREMENTS:
    // - The wallet must already have sufficient native SOL balance
    // - The WSOL ATA account must be writable in the transaction
    // - The wallet must be a signer in the transaction
    let transfer_ix = system_instruction::transfer(wallet, wsol_ata, amount);
    instructions.push(transfer_ix);
    
    // Step 3: Sync native SOL (lamports) to WSOL tokens
    // SPL Token SyncNative instruction reads the native SOL balance (lamports) in the
    // WSOL ATA and updates the token account's amount field accordingly.
    // This converts the native SOL (lamports) that was transferred in step 2 to WSOL tokens.
    // Instruction discriminator: 17 (SyncNative)
    // 
    // ⚠️ IMPORTANT: sync_native does NOT transfer SOL - it only syncs the token amount
    // field with the lamports already in the account. If the WSOL ATA has no lamports
    // (no native SOL was transferred), sync_native will result in 0 WSOL tokens.
    // Both the transfer AND sync_native are required for wrapping to work.
    let sync_native_ix = token_instruction::sync_native(
        &spl_token::id(),
        wsol_ata,
    )?;
    instructions.push(sync_native_ix);
    
    Ok(instructions)
}

/// Build instruction to unwrap WSOL to native SOL
/// This closes the WSOL ATA and returns native SOL to the wallet
/// 
/// Note: This will fail if the ATA has a balance > 0 (must transfer out first)
pub fn build_unwrap_sol_instruction(
    wallet: &Pubkey,
    wsol_ata: &Pubkey,
) -> Result<Instruction> {
    use spl_token::instruction as token_instruction;
    
    // CloseAccount instruction closes the WSOL ATA and returns native SOL to the wallet
    // Instruction discriminator: 9 (CloseAccount)
    let close_ix = token_instruction::close_account(
        &spl_token::id(),
        wsol_ata,      // Account to close
        wallet,        // Destination for remaining lamports
        wallet,        // Owner/authority
        &[],           // Signers (wallet is already a signer)
    )?;
    
    Ok(close_ix)
}
