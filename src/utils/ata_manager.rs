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
use std::sync::Arc;
use std::str::FromStr;

// Max ATAs per transaction - optimized for compute unit usage
// Each ATA creation uses ~10k CU, so 7 ATAs = ~70k CU (well within 200k CU limit)
// Note: This should be tested in production to ensure it doesn't exceed compute limits
const MAX_ATAS_PER_TX: usize = 7;

/// Ensure all required ATAs exist for the bot wallet
/// Simple approach: Check on-chain, create if missing
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
        ("USDT", config.usdt_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
        ("ETH", config.eth_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
        ("BTC", config.btc_mint.as_ref().map(|s| s.as_str()).unwrap_or("")),
    ];

    // Collect ATAs that need checking
    let mut to_check = Vec::new();
    
    for (name, mint_str) in &mints {
        if mint_str.is_empty() {
            continue;
        }

        let mint = Pubkey::from_str(mint_str)
            .with_context(|| format!("Invalid {} mint", name))?;

        let ata = get_associated_token_address(&wallet_pubkey, &mint, Some(config))
            .with_context(|| format!("Failed to derive ATA for {}", name))?;

        to_check.push((*name, mint, ata));
    }

    if to_check.is_empty() {
        log::info!("🎉 No ATAs to check!");
        return Ok(());
    }

    log::info!("🔍 Checking {} ATA(s) on-chain...", to_check.len());

    // Check on-chain in parallel with timeout
    let check_tasks: Vec<_> = to_check.into_iter().map(|(name, mint, ata)| {
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
                    if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                        log::warn!("❌ {} ATA not found: {}, will create", name, ata);
                        Ok((name, mint, ata, false))
                    } else {
                        log::warn!("⚠️  Failed to check {} ATA ({}): {}", name, ata, e);
                        Err(format!("RPC error for {}: {}", name, e))
                    }
                }
                Err(_) => {
                    log::warn!("⚠️  Timeout checking {} ATA ({}), will try to create", name, ata);
                    Ok((name, mint, ata, false)) // Assume missing and try to create
                }
            }
        }
    }).collect();

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

    // ⚠️ KEY FIX: Send ATAs in batches (max 7 per transaction to optimize compute unit usage)
    for chunk in missing_atas.chunks(MAX_ATAS_PER_TX) {
        log::info!("📤 Sending batch of {} ATA(s)...", chunk.len());
        
        let mut instructions = Vec::new();
        let mut batch_atas = Vec::new();
        
        for (name, mint, ata) in chunk {
            log::info!("🔨 Creating instruction for {} ATA: {}", name, ata);
            match create_ata_instruction(&wallet_pubkey, &wallet_pubkey, mint, Some(config)) {
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

        log::info!("📦 Prepared {} instruction(s) for batch", instructions.len());

        // Send transaction for this batch
        log::info!("🚀 Sending batch transaction with {} instruction(s)...", instructions.len());
        match send_ata_batch(
            Arc::clone(&rpc),
            Arc::clone(&wallet),
            instructions,
            &batch_atas,
        ).await {
            Ok(sig) => {
                log::info!("✅ Batch transaction sent: {}", sig);
            }
            Err(e) => {
                log::error!("❌ Batch transaction failed: {}", e);
                log::warn!("   Bot will continue, ATAs can be created on-demand during liquidation");
            }
        }

        // Small delay between batches to avoid RPC rate limits
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

    log::debug!("Getting recent blockhash...");
    let recent_blockhash = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        rpc.get_recent_blockhash()
    ).await
    .context("Timeout getting blockhash")??;
    
    log::debug!("Building transaction with {} instructions...", instructions.len());
    let mut tx = Transaction::new_with_payer(&instructions, Some(&wallet_pubkey));
    tx.sign(&[wallet.as_ref()], recent_blockhash);
    
    log::debug!("Sending transaction...");
    let sig = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        rpc.send_transaction(&tx)
    ).await
    .context("Timeout sending transaction")??;
    
    // Wait for confirmation (longer wait for ATA creation)
    log::debug!("Waiting for transaction confirmation...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    
    // Verify ATAs were created (with retries)
    for (name, _mint, ata) in atas {
        let mut verified = false;
        for attempt in 1..=3 {
            match rpc.get_account(ata).await {
                Ok(_) => {
                    log::info!("✅ {} ATA verified: {}", name, ata);
                    verified = true;
                    break;
                }
                Err(e) => {
                    if attempt < 3 {
                        log::debug!("⚠️  {} ATA verification attempt {} failed, retrying...: {}", name, attempt, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    } else {
                        log::warn!("⚠️  {} ATA verification failed after 3 attempts: {}", name, e);
                        log::warn!("   Transaction was sent: {}, ATA may still be creating...", sig);
                    }
                }
            }
        }
        if !verified {
            log::warn!("⚠️  {} ATA not found after transaction. It may take longer to confirm.", name);
        }
    }
    
    Ok(sig)
}

fn create_ata_instruction(
    payer: &Pubkey,
    wallet: &Pubkey,
    mint: &Pubkey,
    config: Option<&Config>,
) -> Result<Instruction> {
    use crate::protocol::solend::accounts::get_associated_token_program_id;
    
    let associated_token_program = get_associated_token_program_id(config)
        .context("Failed to get associated token program ID")?;
    let token_program = spl_token::id();

    let (ata, _bump) = Pubkey::try_find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &associated_token_program,
    ).ok_or_else(|| anyhow::anyhow!("Failed to find program address for ATA"))?;
    
    log::debug!("Creating ATA instruction: payer={}, wallet={}, mint={}, ata={}", 
        payer, wallet, mint, ata);
    
    Ok(Instruction {
        program_id: associated_token_program,
        accounts: vec![
            solana_sdk::instruction::AccountMeta::new(*payer, true),
            solana_sdk::instruction::AccountMeta::new(ata, false),
            solana_sdk::instruction::AccountMeta::new_readonly(*wallet, false),
            solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
            solana_sdk::instruction::AccountMeta::new_readonly(
                solana_sdk::system_program::id(),
                false,
            ),
            solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
        ],
        data: vec![], // Create instruction has no data
    })
}
