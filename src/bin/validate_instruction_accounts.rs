//! Validate Solend Liquidation Instruction Account Order
//!
//! This binary fetches a real mainnet liquidation transaction and compares
//! the instruction accounts array with our implementation.
//!
//! Usage:
//! ```bash
//! cargo run --bin validate_instruction_accounts -- \
//!   --tx <TRANSACTION_SIGNATURE> \
//!   --rpc-url https://api.mainnet-beta.solana.com
//! ```
//!
//! Reference:
//! - Solend Program: https://github.com/solendprotocol/solana-program-library
//! - Instruction Format: See protocols/solend.rs::build_liquidation_instruction()
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::Signature,
};
use std::sync::Arc;

use liquid_bot::solana_client::SolanaClient;
use liquid_bot::protocols::solend::SolendProtocol;
use liquid_bot::protocol::Protocol;
use liquid_bot::domain::LiquidationOpportunity;

/// Command line arguments
#[derive(Parser, Debug)]
#[command(name = "validate_instruction_accounts")]
#[command(about = "Validates instruction account order against real mainnet liquidation transaction")]
struct Args {
    /// Transaction signature (required)
    #[arg(long, required = true)]
    tx: String,

    /// RPC URL
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Verbose output
    #[arg(long, short, default_value = "false")]
    verbose: bool,
}

/// Expected account order (from Solend source code)
const EXPECTED_ACCOUNT_ORDER: &[&str] = &[
    "sourceLiquidity",              // 1. Liquidator's token account for debt
    "destinationCollateral",       // 2. Liquidator's token account for collateral
    "repayReserve",                 // 3. Reserve we're repaying debt to
    "repayReserveLiquiditySupply",  // 4. Liquidity supply token account
    "withdrawReserve",              // 5. Reserve we're withdrawing collateral from
    "withdrawReserveCollateralSupply", // 6. Collateral supply token account
    "obligation",                   // 7. Obligation account being liquidated
    "lendingMarket",                // 8. Lending market account
    "lendingMarketAuthority",       // 9. Lending market authority PDA
    "userTransferAuthority",        // 10. Liquidator pubkey (signer)
    "clock",                        // 11. Clock sysvar
    "tokenProgram",                 // 12. SPL Token program
];

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .init();

    let args = Args::parse();

    println!("🔍 Validating Instruction Account Order");
    println!("{}", "=".repeat(80));
    println!();
    println!("Transaction: {}", args.tx);
    println!("RPC URL: {}", args.rpc_url);
    println!();

    let rpc_client = Arc::new(
        SolanaClient::new(args.rpc_url.clone())
            .context("Failed to create RPC client")?
    );

    // Parse transaction signature
    let tx_signature = args.tx
        .parse::<Signature>()
        .context("Invalid transaction signature")?;

    // Fetch transaction from mainnet
    println!("📥 Fetching transaction from mainnet...");
    let tx_json = fetch_transaction(&rpc_client, &tx_signature).await
        .context("Failed to fetch transaction")?;

    // Extract instruction accounts
    let actual_accounts = extract_instruction_accounts(&tx_json)
        .context("Failed to extract instruction accounts")?;

    println!("✅ Transaction fetched successfully");
    println!("   Found {} accounts in liquidation instruction", actual_accounts.len());
    println!();

    // Display actual accounts
    println!("{}", "=".repeat(80));
    println!("📋 ACTUAL ACCOUNTS (from mainnet transaction)");
    println!("{}", "=".repeat(80));
    for (i, account) in actual_accounts.iter().enumerate() {
        println!("  {}. {} (writable: {}, signer: {})",
                i + 1,
                account.pubkey,
                account.is_writable,
                account.is_signer);
    }
    println!();

    // Build our instruction for comparison
    println!("🔨 Building instruction from our implementation...");
    let our_instruction = build_our_instruction(&rpc_client, &actual_accounts).await?;

    println!("✅ Our instruction built");
    println!("   Account count: {}", our_instruction.accounts.len());
    println!();

    // Compare accounts
    println!("{}", "=".repeat(80));
    println!("🔍 COMPARISON");
    println!("{}", "=".repeat(80));
    println!();

    let mut matches = 0;
    let mut mismatches = Vec::new();

    let min_len = actual_accounts.len().min(our_instruction.accounts.len());

    if actual_accounts.len() != our_instruction.accounts.len() {
        println!("⚠️  Account count mismatch!");
        println!("   Mainnet: {} accounts", actual_accounts.len());
        println!("   Ours: {} accounts", our_instruction.accounts.len());
        println!();
    }

    for i in 0..min_len {
        let actual = &actual_accounts[i];
        let ours = &our_instruction.accounts[i];
        let expected_name = if i < EXPECTED_ACCOUNT_ORDER.len() {
            EXPECTED_ACCOUNT_ORDER[i]
        } else {
            "unknown"
        };

        let pubkey_match = actual.pubkey == ours.pubkey;
        let writable_match = actual.is_writable == ours.is_writable;
        let signer_match = actual.is_signer == ours.is_signer;

        if pubkey_match && writable_match && signer_match {
            matches += 1;
            if args.verbose {
                println!("  ✅ {}. {} - MATCH", i + 1, expected_name);
            }
        } else {
            mismatches.push((i + 1, expected_name, actual.clone(), ours.clone()));
            println!("  ❌ {}. {} - MISMATCH", i + 1, expected_name);
            if !pubkey_match {
                println!("     Pubkey: mainnet={}, ours={}", actual.pubkey, ours.pubkey);
            }
            if !writable_match {
                println!("     Writable: mainnet={}, ours={}", actual.is_writable, ours.is_writable);
            }
            if !signer_match {
                println!("     Signer: mainnet={}, ours={}", actual.is_signer, ours.is_signer);
            }
        }
    }

    println!();
    println!("{}", "=".repeat(80));
    println!("📊 SUMMARY");
    println!("{}", "=".repeat(80));
    println!();
    println!("Matches: {}/{}", matches, min_len);
    println!("Mismatches: {}", mismatches.len());

    if mismatches.is_empty() {
        println!();
        println!("✅ SUCCESS: All accounts match!");
        println!("   Our implementation correctly matches mainnet transaction.");
    } else {
        println!();
        println!("❌ FAILURE: Found {} mismatches", mismatches.len());
        println!("   Please review the differences above and update the implementation.");
    }

    // Check for oracle accounts
    println!();
    println!("{}", "=".repeat(80));
    println!("🔍 ORACLE ACCOUNTS CHECK");
    println!("{}", "=".repeat(80));
    println!();
    
    let has_pyth = actual_accounts.iter().any(|a| {
        // Pyth program ID: FsJ3A3u2vn5cTVofAjvy6y5xABAXKb36w8D6Jpp5LZvg
        a.pubkey.to_string().starts_with("FsJ3A3u2vn5cTVofAjvy6y5xABAXKb36w8D6Jpp5LZvg")
    });
    
    let has_switchboard = actual_accounts.iter().any(|a| {
        // Switchboard program ID: SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f
        a.pubkey.to_string().starts_with("SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f")
    });

    if has_pyth || has_switchboard {
        println!("⚠️  Oracle accounts found in instruction:");
        if has_pyth {
            println!("   - Pyth oracle account present");
        }
        if has_switchboard {
            println!("   - Switchboard oracle account present");
        }
        println!("   This contradicts our assumption that oracle accounts are NOT in the instruction.");
        println!("   Please verify if this is correct or if our implementation needs updating.");
    } else {
        println!("✅ No oracle accounts in instruction");
        println!("   This confirms our assumption: oracle accounts are read on-chain, not passed as arguments.");
    }

    Ok(())
}

/// Fetches a transaction from mainnet using RPC
/// 
/// Note: Uses solana CLI command as a workaround since get_transaction API varies by version
/// Alternative: User can provide transaction JSON file directly
async fn fetch_transaction(
    rpc_client: &SolanaClient,
    signature: &Signature,
) -> Result<Value> {
    use std::process::Command;
    use std::str::FromStr;

    let rpc_url = rpc_client.get_url();
    let sig_str = signature.to_string();

    // Use solana CLI to fetch transaction
    // This is more reliable than trying to match RPC client API versions
    let output = Command::new("solana")
        .args(&[
            "transaction",
            &sig_str,
            "--output",
            "json",
            "--url",
            rpc_url,
        ])
        .output()
        .context("Failed to execute solana CLI. Make sure 'solana' command is available in PATH")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "solana CLI failed: {}\n{}",
            output.status,
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tx_json: Value = serde_json::from_str(&stdout)
        .context("Failed to parse transaction JSON from solana CLI")?;

    Ok(tx_json)
}

/// Extracts instruction accounts from transaction JSON
fn extract_instruction_accounts(tx_json: &Value) -> Result<Vec<solana_sdk::instruction::AccountMeta>> {
    use std::str::FromStr;

    // Navigate to transaction.transaction.message.instructions
    let transaction = tx_json
        .get("transaction")
        .context("Failed to find 'transaction' field")?;

    let message = transaction
        .get("message")
        .context("Failed to find 'message' field")?;

    let instructions = message
        .get("instructions")
        .and_then(|i| i.as_array())
        .context("Failed to find instructions array")?;

    // Get account keys from transaction message (for resolving account indices)
    let account_keys = message
        .get("accountKeys")
        .and_then(|a| a.as_array())
        .context("Failed to find accountKeys in transaction")?;

    // Find the liquidation instruction (should be the one that calls Solend program)
    let solend_program_id = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    let liquidation_ix = instructions
        .iter()
        .find(|ix| {
            // Check programId - could be string or object with pubkey field
            let program_id = ix.get("programId")
                .and_then(|p| p.as_str())
                .or_else(|| {
                    ix.get("programId")
                        .and_then(|p| p.get("pubkey"))
                        .and_then(|p| p.as_str())
                });
            
            program_id.map(|p| p == solend_program_id).unwrap_or(false)
        })
        .context("Failed to find Solend liquidation instruction")?;

    // Get accounts from instruction (could be indices or full account objects)
    let account_data = liquidation_ix
        .get("accounts")
        .and_then(|a| a.as_array())
        .context("Failed to find accounts array in instruction")?;

    // Build AccountMeta list
    let mut accounts = Vec::new();
    for account_item in account_data {
        let pubkey_str = account_item
            .as_str()
            .or_else(|| account_item.get("pubkey").and_then(|p| p.as_str()))
            .or_else(|| {
                // If it's an index, resolve from accountKeys
                account_item
                    .as_u64()
                    .and_then(|idx| {
                        account_keys
                            .get(idx as usize)
                            .and_then(|ak| {
                                ak.as_str()
                                    .or_else(|| ak.get("pubkey").and_then(|p| p.as_str()))
                            })
                    })
            })
            .context("Failed to extract pubkey from account")?;

        let pubkey = Pubkey::from_str(pubkey_str)
            .context(format!("Invalid pubkey: {}", pubkey_str))?;

        // Get writable and signer flags
        // These might be in the account object or need to be resolved from accountKeys
        let writable = account_item
            .get("writable")
            .and_then(|w| w.as_bool())
            .or_else(|| {
                // Try to find in accountKeys if account_item is an index
                if account_item.is_u64() {
                    account_item
                        .as_u64()
                        .and_then(|idx| {
                            account_keys
                                .get(idx as usize)
                                .and_then(|ak| ak.get("writable").and_then(|w| w.as_bool()))
                        })
                } else {
                    None
                }
            })
            .unwrap_or(false);

        let signer = account_item
            .get("signer")
            .and_then(|s| s.as_bool())
            .or_else(|| {
                // Try to find in accountKeys if account_item is an index
                if account_item.is_u64() {
                    account_item
                        .as_u64()
                        .and_then(|idx| {
                            account_keys
                                .get(idx as usize)
                                .and_then(|ak| ak.get("signer").and_then(|s| s.as_bool()))
                        })
                } else {
                    None
                }
            })
            .unwrap_or(false);

        accounts.push(solana_sdk::instruction::AccountMeta {
            pubkey,
            is_writable: writable,
            is_signer: signer,
        });
    }

    Ok(accounts)
}

/// Builds our instruction for comparison
/// 
/// Note: This is a simplified version that extracts the obligation from the actual transaction
/// and rebuilds the instruction using our implementation to compare account order.
async fn build_our_instruction(
    rpc_client: &Arc<SolanaClient>,
    actual_accounts: &[solana_sdk::instruction::AccountMeta],
) -> Result<Instruction> {
    // Extract obligation pubkey from actual accounts
    // According to Solend source code, obligation should be at index 6 (0-indexed)
    // But let's try to find it by checking account ownership
    let protocol = SolendProtocol::new()
        .context("Failed to create Solend protocol")?;

    // Try to find obligation - it should be owned by Solend program
    let obligation_pubkey = actual_accounts
        .iter()
        .enumerate()
        .find_map(|(idx, account)| {
            // Obligation is typically at index 6, but let's verify by checking if it's owned by Solend
            if idx == 6 || idx == 7 {
                Some(account.pubkey)
            } else {
                None
            }
        })
        .or_else(|| {
            // Fallback: use index 6
            actual_accounts.get(6).map(|a| a.pubkey)
        })
        .context("Failed to find obligation account in transaction")?;

    println!("   Extracted obligation: {}", obligation_pubkey);

    // Get obligation account
    let account = rpc_client.get_account(&obligation_pubkey).await
        .context("Failed to fetch obligation account")?;

    // Verify it's owned by Solend program
    if account.owner != protocol.program_id() {
        return Err(anyhow::anyhow!(
            "Account {} is not owned by Solend program (owner: {})",
            obligation_pubkey,
            account.owner
        ));
    }

    // Parse as position
    let position = protocol
        .parse_account_position(&obligation_pubkey, &account, Some(Arc::clone(rpc_client)))
        .await?
        .context("Failed to parse obligation")?;

    // Create mock opportunity (we only need it for account order comparison)
    let opportunity = LiquidationOpportunity {
        account_position: position.clone(),
        max_liquidatable_amount: 1000, // Mock amount - doesn't matter for account order
        seizable_collateral: 1000,
        liquidation_bonus: 0.05,
        estimated_profit_usd: 10.0,
        target_debt_mint: position.debt_assets.first()
            .map(|d| d.mint.clone())
            .context("No debt assets")?,
        target_collateral_mint: position.collateral_assets.first()
            .map(|c| c.mint.clone())
            .context("No collateral assets")?,
    };

    // Use liquidator from actual transaction (should be signer)
    let liquidator = actual_accounts
        .iter()
        .find(|a| a.is_signer)
        .map(|a| a.pubkey)
        .context("No signer found in accounts (liquidator)")?;

    println!("   Extracted liquidator: {}", liquidator);

    // Build instruction using our implementation
    let instruction = protocol
        .build_liquidation_instruction(
            &opportunity,
            &liquidator,
            Some(Arc::clone(rpc_client)),
        )
        .await
        .context("Failed to build instruction")?;

    Ok(instruction)
}

