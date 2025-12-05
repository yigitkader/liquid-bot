pub mod token_program;

pub use token_program::get_token_program_for_mint;

// ATA verification module

/// Check if an ATA exists and verify its owner
pub async fn verify_ata_exists(
    ata: &solana_sdk::pubkey::Pubkey,
    expected_owner: &solana_sdk::pubkey::Pubkey,
    rpc: &std::sync::Arc<crate::blockchain::rpc_client::RpcClient>,
) -> anyhow::Result<bool> {
    match rpc.get_account(ata).await {
        Ok(account) => {
            if account.data.len() >= 64 {
                let owner_bytes = &account.data[32..64];
                if let Ok(owner) = solana_sdk::pubkey::Pubkey::try_from(owner_bytes) {
                    return if owner == *expected_owner {
                        Ok(true)
                    } else {
                        Err(anyhow::anyhow!(
                            "ATA {} exists but has different owner: {} (expected: {})",
                            ata,
                            owner,
                            expected_owner
                        ))
                    }
                }
            }
            Ok(true) // Account exists but couldn't parse owner
        }
        Err(_) => Ok(false), // Account doesn't exist
    }
}

/// Verify ATA after creation with retries
pub async fn verify_ata_after_creation(
    ata: &solana_sdk::pubkey::Pubkey,
    name: &str,
    rpc: &std::sync::Arc<crate::blockchain::rpc_client::RpcClient>,
    max_attempts: u32,
) -> bool {
    for attempt in 1..=max_attempts {
        match rpc.get_account(ata).await {
            Ok(_) => {
                log::info!("✅ {} ATA verified: {}", name, ata);
                return true;
            }
            Err(e) => {
                if attempt < max_attempts {
                    log::debug!(
                        "⚠️  {} ATA verification attempt {} failed, retrying...: {}",
                        name,
                        attempt,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                } else {
                    log::warn!(
                        "⚠️  {} ATA verification failed after {} attempts: {}",
                        name,
                        max_attempts,
                        e
                    );
                }
            }
        }
    }
    false
}

use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::protocol::solend::accounts::get_associated_token_address;
use anyhow::{Context, Result};
use futures_util::future::join_all;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;
use std::sync::Arc;

// Max ATAs per transaction - optimized for compute unit usage
// CRITICAL: Each ATA creation uses ~15k CU, so we need to be conservative
// Calculation: 5 ATAs × 15k CU = 75k CU + transaction overhead (~10-20k CU) = ~85-95k CU
// This leaves a safe margin within the 200k CU limit
// Note: 7 ATAs would be 105k CU + overhead, which could exceed limits with other instructions
// This value should be tested in production/mainnet to ensure it doesn't exceed compute limits
const MAX_ATAS_PER_TX: usize = 5;

pub async fn ensure_required_atas(
    rpc: Arc<RpcClient>,
    wallet: Arc<Keypair>,
    config: &Config,
) -> Result<()> {
    let wallet_pubkey = wallet.pubkey();
    log::info!("🔍 Checking required ATAs for wallet: {}", wallet_pubkey);

    // Get all required mints from config
    let mints = vec![
        ("USDC", config.usdc_mint.as_str()),
        ("SOL", config.sol_mint.as_str()),
        (
            "USDT",
            config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
        (
            "ETH",
            config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
        (
            "BTC",
            config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or(""),
        ),
    ];

    let mut to_check = Vec::new();

    for (name, mint_str) in &mints {
        if mint_str.is_empty() {
            continue;
        }

        let mint = Pubkey::from_str(mint_str).with_context(|| format!("Invalid {} mint", name))?;

        let ata = get_associated_token_address(&wallet_pubkey, &mint, Some(config))
            .with_context(|| format!("Failed to derive ATA for {}", name))?;

        to_check.push((*name, mint, ata));
    }

    if to_check.is_empty() {
        log::info!("🎉 No ATAs to check!");
        return Ok(());
    }

    log::info!("🔍 Checking {} ATA(s) on-chain...", to_check.len());

    let check_tasks: Vec<_> = to_check
        .into_iter()
        .map(|(name, mint, ata)| {
            let rpc = Arc::clone(&rpc);
            let name = name.to_string();

            async move {
                let timeout = tokio::time::Duration::from_secs(10);
                let check_result = tokio::time::timeout(timeout, rpc.get_account(&ata)).await;

                match check_result {
                    Ok(Ok(_)) => {
                        log::info!("✅ {} ATA already exists: {}", name, ata);
                        Ok((name, mint, ata, true))
                    }
                    Ok(Err(e)) => {
                        let error_msg = e.to_string();
                        if error_msg.contains("AccountNotFound")
                            || error_msg.contains("account not found")
                        {
                            log::warn!("❌ {} ATA not found: {}, will create", name, ata);
                            Ok((name, mint, ata, false))
                        } else {
                            log::warn!("⚠️  Failed to check {} ATA ({}): {}", name, ata, e);
                            Err(format!("RPC error for {}: {}", name, e))
                        }
                    }
                    Err(_) => {
                        log::warn!(
                            "⚠️  Timeout checking {} ATA ({}), will verify again before creating",
                            name,
                            ata
                        );
                        // Don't assume missing on timeout - send_ata_batch will verify again
                        Ok((name, mint, ata, false))
                    }
                }
            }
        })
        .collect();

    let results = join_all(check_tasks).await;

    let mut missing_atas = Vec::new();

    for result in results {
        match result {
            Ok((name, mint, ata, exists)) => {
                if !exists {
                    log::info!("📋 Adding {} ATA to missing list: {}", name, ata);
                    missing_atas.push((name, mint, ata));
                }
            }
            Err(e) => {
                log::warn!("⚠️  ATA check returned error (will skip): {}", e);
            }
        }
    }

    log::info!("📊 Found {} missing ATA(s) to create", missing_atas.len());

    if missing_atas.is_empty() {
        log::info!("🎉 All required ATAs already exist!");
        return Ok(());
    }

    log::info!("📝 Creating {} missing ATA(s)...", missing_atas.len());

    for chunk in missing_atas.chunks(MAX_ATAS_PER_TX) {
        log::info!("📤 Sending batch of {} ATA(s)...", chunk.len());

        let mut instructions = Vec::new();
        let mut batch_atas = Vec::new();

        for (name, mint, ata) in chunk {
            log::info!("🔨 Creating instruction for {} ATA: {}", name, ata);
            match create_ata_instruction(&wallet_pubkey, &wallet_pubkey, mint, Some(config), Some(&rpc)).await {
                Ok(ix) => {
                    log::info!("✅ Created instruction for {} ATA", name);
                    instructions.push(ix);
                    batch_atas.push((name.clone(), *mint, *ata));
                }
                Err(e) => {
                    log::error!("❌ Failed to create instruction for {} ATA: {}", name, e);
                    log::warn!("   Skipping {} ATA creation", name);
                }
            }
        }

        if instructions.is_empty() {
            log::warn!("⚠️  No instructions created for this batch, skipping...");
            continue;
        }

        log::info!(
            "📦 Prepared {} instruction(s) for batch",
            instructions.len()
        );

        log::info!(
            "🚀 Sending batch transaction with {} instruction(s)...",
            instructions.len()
        );

        const CRITICAL_ATAS: &[&str] = &["USDC", "SOL"];
        let has_critical_ata = batch_atas
            .iter()
            .any(|(name, _, _)| CRITICAL_ATAS.contains(&name.as_str()));

        match send_ata_batch(
            Arc::clone(&rpc),
            Arc::clone(&wallet),
            instructions,
            &batch_atas,
        )
        .await
        {
            Ok(sig) => {
                log::info!("✅ Batch transaction sent: {}", sig);
            }
            Err(e) => {
                log::error!("❌ Batch transaction failed: {}", e);

                if has_critical_ata {
                    log::error!("🚨 CRITICAL: Critical ATA creation failed (USDC or SOL) - bot cannot operate!");
                    return Err(anyhow::anyhow!(
                        "Critical ATA creation failed: {}. Bot requires USDC and SOL ATAs to operate.",
                        e
                    ));
                } else {
                    log::warn!(
                        "   Bot will continue, ATAs can be created on-demand during liquidation"
                    );
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    log::info!("🎉 ATA setup completed!");
    Ok(())
}

async fn send_ata_batch(
    rpc: Arc<RpcClient>,
    wallet: Arc<Keypair>,
    instructions: Vec<Instruction>,
    atas: &[(String, Pubkey, Pubkey)],
) -> Result<solana_sdk::signature::Signature> {
    let wallet_pubkey = wallet.pubkey();
    
    // First, check if any ATA already exists - if so, we should NOT try to create it
    for (name, _mint, ata) in atas {
        log::info!("🔍 Pre-check: Verifying {} ATA {} does not already exist...", name, ata);
        match rpc.get_account(ata).await {
            Ok(account) => {
                // Try to parse as token account to get owner
                if account.data.len() >= 64 {
                    let owner_bytes = &account.data[32..64];
                    if let Ok(owner) = solana_sdk::pubkey::Pubkey::try_from(owner_bytes) {
                        if owner == wallet_pubkey {
                            log::info!(
                                "✅ {} ATA {} already exists with correct owner - skipping creation",
                                name,
                                ata
                            );
                            // Return a special signature to indicate success without transaction
                            return Ok(solana_sdk::signature::Signature::default());
                        } else {
                            log::error!(
                                "❌ {} ATA {} exists but has different owner: {} (expected: {})",
                                name,
                                ata,
                                owner,
                                wallet_pubkey
                            );
                            return Err(anyhow::anyhow!(
                                "ATA {} exists but has different owner: {} (expected: {})",
                                ata,
                                owner,
                                wallet_pubkey
                            ));
                        }
                    }
                }
                log::warn!(
                    "⚠️  {} ATA {} exists but could not parse owner - proceeding with caution",
                    name,
                    ata
                );
            }
            Err(_) => {
                log::debug!("   {} ATA {} does not exist - will create", name, ata);
            }
        }
    }

    log::debug!("Getting recent blockhash...");
    let recent_blockhash = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        rpc.get_recent_blockhash(),
    )
    .await
    .context("Timeout getting blockhash")??;

    log::info!(
        "🔨 Building transaction with {} instruction(s)...",
        instructions.len()
    );
    
    // Log transaction details before building
    log::info!("📋 Transaction Details:");
    log::info!("   Payer: {} (will be signer)", wallet_pubkey);
    log::info!("   Instructions: {}", instructions.len());
    for (idx, instruction) in instructions.iter().enumerate() {
        log::info!("   Instruction [{}]: program_id={}, accounts={}, data_len={}",
            idx,
            instruction.program_id,
            instruction.accounts.len(),
            instruction.data.len()
        );
        for (acc_idx, account_meta) in instruction.accounts.iter().enumerate() {
            log::info!(
                "      Account [{}]: pubkey={}, is_signer={}, is_writable={}",
                acc_idx,
                account_meta.pubkey,
                account_meta.is_signer,
                account_meta.is_writable
            );
        }
    }
    
    let mut tx = Transaction::new_with_payer(&instructions, Some(&wallet_pubkey));
    tx.message.recent_blockhash = recent_blockhash;
    
    // Log transaction message account keys
    log::info!("📋 Transaction Message Account Keys ({} total):", tx.message.account_keys.len());
    for (idx, key) in tx.message.account_keys.iter().enumerate() {
        let is_signer = idx < tx.message.header.num_required_signatures as usize;
        log::info!("   [{}] pubkey={}, is_signer={}", idx, key, is_signer);
    }
    log::info!("📋 Transaction header: num_required_signatures={}, num_readonly_signed_accounts={}, num_readonly_unsigned_accounts={}",
        tx.message.header.num_required_signatures,
        tx.message.header.num_readonly_signed_accounts,
        tx.message.header.num_readonly_unsigned_accounts
    );
    
    // ✅ FIX: Use sign_transaction instead of direct tx.sign() to avoid KeypairPubkeyMismatch
    use crate::blockchain::transaction::sign_transaction;
    sign_transaction(&mut tx, wallet.as_ref())
        .context("Failed to sign ATA creation transaction")?;
    
    log::info!("✅ Transaction signed with wallet: {}", wallet_pubkey);
    
    // Try to simulate transaction first to get detailed error information
    log::info!("🔍 Simulating transaction before sending...");
    match rpc.simulate_transaction(&tx).await {
        Ok(sim_result) => {
            if let Some(err) = sim_result.err {
                log::error!("❌ Transaction simulation failed: {:?}", err);
                if let Some(logs) = sim_result.logs {
                    log::error!("📋 Simulation logs:");
                    for log_line in logs {
                        log::error!("   {}", log_line);
                    }
                }
                if let Some(accounts) = sim_result.accounts {
                    log::info!("📋 Simulation returned {} account(s)", accounts.len());
                }
            } else {
                log::info!("✅ Transaction simulation succeeded");
                if let Some(logs) = sim_result.logs {
                    log::info!("📋 Simulation logs:");
                    for log_line in logs {
                        log::info!("   {}", log_line);
                    }
                }
            }
        }
        Err(e) => {
            log::warn!("⚠️  Failed to simulate transaction: {}, proceeding anyway", e);
        }
    }
    
    // Note: ATA existence check is now done BEFORE building transaction (above)
    // This check is redundant but kept for additional logging
    for (name, _mint, ata) in atas {
        log::debug!("🔍 Post-check: Verifying {} ATA {} status...", name, ata);
        match rpc.get_account(ata).await {
            Ok(account) => {
                log::warn!(
                    "⚠️  {} ATA {} already exists! owner={}, lamports={}, data_len={}",
                    name,
                    ata,
                    account.owner,
                    account.lamports,
                    account.data.len()
                );
                // This should not happen if pre-check worked correctly
                log::warn!("   This should have been caught in pre-check - transaction may fail");
            }
            Err(_) => {
                log::debug!("   {} ATA {} does not exist - proceeding with creation", name, ata);
            }
        }
    }
    
    log::info!("📤 Sending transaction...");
    let sig = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        rpc.send_transaction(&tx),
    )
    .await
    .context("Timeout sending transaction")??;

    log::debug!("Waiting for transaction confirmation...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    for (name, _mint, ata) in atas {
        if !verify_ata_after_creation(ata, name, &rpc, 3).await {
            log::warn!(
                "⚠️  {} ATA not found after transaction. It may take longer to confirm. Transaction: {}",
                name,
                sig
            );
        }
    }

    Ok(sig)
}

/// Create an ATA (Associated Token Account) instruction
/// 
/// This function creates the instruction needed to create an ATA for a given mint.
/// It handles both standard SPL Token and Token-2022 programs automatically.
/// 
/// Parameters:
/// - payer: The account that will pay for the ATA creation (signer)
/// - wallet: The wallet that will own the ATA
/// - mint: The token mint address
/// - config: Optional configuration (for associated token program ID)
/// - rpc: Optional RPC client (for token program detection)
/// 
/// Returns:
/// - Ok(Instruction): The ATA creation instruction
/// - Err: If token program detection fails or ATA derivation fails
pub async fn create_ata_instruction(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    config: Option<&Config>,
    rpc: Option<&Arc<RpcClient>>,
) -> Result<Instruction> {
    use crate::protocol::solend::accounts::get_associated_token_program_id;

    let associated_token_program = get_associated_token_program_id(config)
        .context("Failed to get associated token program ID")?;
    
    // Determine correct token program based on mint
    // Some mints use Token-2022 (Token Extensions), others use standard SPL Token
    // Check mint account owner on-chain for accurate determination
    let token_program = token_program::get_token_program_for_mint(mint, rpc)
        .await
        .context("Failed to determine token program for mint")?;

    // Always use manual construction to ensure correct account order and metadata
    // SPL library may have compatibility issues with certain Solana SDK versions
    log::info!("🔍 Deriving ATA address...");
    log::info!("   Wallet: {}", wallet);
    log::info!("   Token Program: {}", token_program);
    log::info!("   Mint: {}", mint);
    log::info!("   Associated Token Program: {}", associated_token_program);
    
    let seeds = &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()];
    log::info!("   Seeds: wallet ({} bytes) + token_program ({} bytes) + mint ({} bytes) = {} total bytes",
        wallet.as_ref().len(),
        token_program.as_ref().len(),
        mint.as_ref().len(),
        seeds.iter().map(|s| s.len()).sum::<usize>()
    );
    
    let (ata, bump) = Pubkey::try_find_program_address(
        seeds,
        &associated_token_program,
    )
    .ok_or_else(|| anyhow::anyhow!("Failed to find program address for ATA"))?;
    
    log::info!("✅ ATA derived: {} (bump: {})", ata, bump);
    
    // Verify ATA derivation using spl_associated_token_account if available
    // Note: We use manual derivation to ensure compatibility, but we can verify with SPL library
    let expected_ata = spl_associated_token_account::get_associated_token_address_with_program_id(
        wallet,
        mint,
        &token_program,
    );
    if expected_ata != ata {
        log::error!(
            "❌ ATA derivation mismatch! Manual: {}, SPL library: {}",
            ata,
            expected_ata
        );
        log::error!("   This could cause 'Provided owner is not allowed' error!");
        log::error!("   Using SPL library derived ATA instead: {}", expected_ata);
        // Use SPL library derived ATA to ensure compatibility
        return Ok(Instruction {
            program_id: associated_token_program,
            accounts: vec![
                solana_sdk::instruction::AccountMeta::new(*payer, true),
                solana_sdk::instruction::AccountMeta::new(expected_ata, false),
                solana_sdk::instruction::AccountMeta::new_readonly(*wallet, false),
                solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
                solana_sdk::instruction::AccountMeta::new_readonly(
                    solana_sdk::system_program::id(),
                    false,
                ),
                solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
            ],
            data: vec![],
        });
    } else {
        log::info!("✅ ATA derivation matches SPL library: {}", ata);
    }

    log::info!(
        "🔨 Creating ATA instruction: payer={}, wallet={}, mint={}, ata={}, token_program={}, associated_program={}",
        payer,
        wallet,
        mint,
        ata,
        token_program,
        associated_token_program
    );

    // Associated Token Program Create instruction format:
    // Accounts (in order):
    // 0. Payer (signer, writable) - pays for account creation
    // 1. ATA account (writable) - the account being created
    // 2. Owner (readonly) - the wallet that will own the ATA
    // 3. Mint (readonly) - the token mint
    // 4. System Program (readonly) - for account creation
    // 5. Token Program (readonly) - SPL Token or Token-2022 program
    // Data: empty (discriminator is handled by program)
    let accounts = vec![
        solana_sdk::instruction::AccountMeta::new(*payer, true),
        solana_sdk::instruction::AccountMeta::new(ata, false),
        solana_sdk::instruction::AccountMeta::new_readonly(*wallet, false),
        solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
        solana_sdk::instruction::AccountMeta::new_readonly(
            solana_sdk::system_program::id(),
            false,
        ),
        solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
    ];
    
    // Log detailed account information
    log::info!("📋 ATA Instruction Account Details:");
    for (idx, account_meta) in accounts.iter().enumerate() {
        log::info!(
            "   [{}] pubkey={}, is_signer={}, is_writable={}",
            idx,
            account_meta.pubkey,
            account_meta.is_signer,
            account_meta.is_writable
        );
    }
    log::info!("📋 Instruction data length: {} bytes", 0);
    log::info!("📋 Program ID: {}", associated_token_program);
    
    Ok(Instruction {
        program_id: associated_token_program,
        accounts,
        data: vec![],
    })
}
