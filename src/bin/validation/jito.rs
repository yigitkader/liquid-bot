// Jito Bundle Double-Signing Validation Tests
// Tests that verify Jito bundle transactions are signed correctly and prevent double-signing

use anyhow::{Context, Result};
use solana_sdk::{
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use std::sync::Arc;

use liquid_bot::blockchain::jito::{JitoBundle, JitoClient};
use liquid_bot::blockchain::transaction::sign_transaction;
use super::result::TestResult;
use super::builder::ValidationBuilder;

/// Validates Jito bundle double-signing prevention
/// Tests:
/// 1. Tip transaction signing
/// 2. Main transaction signing
/// 3. Double-signing prevention (sign_transaction should not re-sign already signed transactions)
/// 4. Signature verification correctness
pub async fn validate_jito_bundle_signing() -> Result<Vec<TestResult>> {
    let mut builder = ValidationBuilder::new();

    // Generate a test keypair
    let wallet = Arc::new(Keypair::new());

    // Create a dummy blockhash for testing
    let blockhash = Hash::new_unique();

    // Test 1: Tip transaction signing
    builder = test_tip_transaction_signing(builder, &wallet, blockhash)?;

    // Test 2: Main transaction signing
    builder = test_main_transaction_signing(builder, &wallet, blockhash)?;

    // Test 3: Double-signing prevention
    builder = test_double_signing_prevention(builder, &wallet, blockhash)?;

    // Test 4: Signature verification
    builder = test_signature_verification(builder, &wallet, blockhash)?;

    // Test 5: Jito bundle integration
    builder = test_jito_bundle_integration(builder, &wallet, blockhash)?;

    Ok(builder.build())
}

/// Test 1: Tip transaction should be signed correctly
fn test_tip_transaction_signing(
    builder: ValidationBuilder,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<ValidationBuilder> {
    let wallet_pubkey = wallet.pubkey();
    let tip_account = Pubkey::new_unique();
    let tip_amount = 10_000_000u64;

    let tip_ix = system_instruction::transfer(&wallet_pubkey, &tip_account, tip_amount);
    let mut tip_tx = Transaction::new_with_payer(&[tip_ix], Some(&wallet_pubkey));
    tip_tx.message.recent_blockhash = blockhash;

    // Transaction should be unsigned initially
    let initial_sig = tip_tx.signatures[0];
    let is_unsigned = initial_sig == solana_sdk::signature::Signature::default();

    let builder = builder.check(
        "Jito Tip Transaction - Initial State",
        || is_unsigned,
    );

    // Sign the transaction
    match sign_transaction(&mut tip_tx, wallet.as_ref()) {
        Ok(_) => {
            let signed_sig = tip_tx.signatures[0];
            let is_signed = signed_sig != solana_sdk::signature::Signature::default();
            let sig_changed = signed_sig != initial_sig;

            let builder = builder.check(
                "Jito Tip Transaction - Signing Success",
                || is_signed && sig_changed,
            );

            // Verify signature is valid
            use bincode::serialize;
            let message_bytes = serialize(&tip_tx.message)
                .map_err(|e| anyhow::anyhow!("Failed to serialize message: {}", e))?;
            let is_valid = signed_sig.verify(wallet_pubkey.as_ref(), &message_bytes);

            Ok(builder.check(
                "Jito Tip Transaction - Signature Validity",
                || is_valid,
            ))
        }
        Err(e) => Ok(builder.check_result(
            "Jito Tip Transaction - Signing",
            Err::<(), _>(e),
            "Signing succeeded",
        )),
    }
}

/// Test 2: Main transaction should be signed correctly
fn test_main_transaction_signing(
    builder: ValidationBuilder,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<ValidationBuilder> {
    let wallet_pubkey = wallet.pubkey();

    // Create a simple main transaction
    let main_ix = system_instruction::transfer(
        &wallet_pubkey,
        &Pubkey::new_unique(),
        1_000_000u64,
    );
    let mut main_tx = Transaction::new_with_payer(&[main_ix], Some(&wallet_pubkey));
    main_tx.message.recent_blockhash = blockhash;

    // Transaction should be unsigned initially
    let initial_sig = main_tx.signatures[0];
    let is_unsigned = initial_sig == solana_sdk::signature::Signature::default();

    let builder = builder.check(
        "Jito Main Transaction - Initial State",
        || is_unsigned,
    );

    // Sign the transaction
    match sign_transaction(&mut main_tx, wallet.as_ref()) {
        Ok(_) => {
            let signed_sig = main_tx.signatures[0];
            let is_signed = signed_sig != solana_sdk::signature::Signature::default();
            let sig_changed = signed_sig != initial_sig;

            let builder = builder.check(
                "Jito Main Transaction - Signing Success",
                || is_signed && sig_changed,
            );

            // Verify signature is valid
            use bincode::serialize;
            let message_bytes = serialize(&main_tx.message)
                .map_err(|e| anyhow::anyhow!("Failed to serialize message: {}", e))?;
            let is_valid = signed_sig.verify(wallet_pubkey.as_ref(), &message_bytes);

            Ok(builder.check(
                "Jito Main Transaction - Signature Validity",
                || is_valid,
            ))
        }
        Err(e) => Ok(builder.check_result(
            "Jito Main Transaction - Signing",
            Err::<(), _>(e),
            "Signing succeeded",
        )),
    }
}

/// Test 3: Double-signing prevention - sign_transaction should not re-sign already signed transactions
fn test_double_signing_prevention(
    builder: ValidationBuilder,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<ValidationBuilder> {
    let wallet_pubkey = wallet.pubkey();

    // Create a transaction
    let test_ix = system_instruction::transfer(
        &wallet_pubkey,
        &Pubkey::new_unique(),
        1_000_000u64,
    );
    let mut test_tx = Transaction::new_with_payer(&[test_ix], Some(&wallet_pubkey));
    test_tx.message.recent_blockhash = blockhash;

    // First signing
    sign_transaction(&mut test_tx, wallet.as_ref())
        .context("First signing should succeed")?;
    let first_sig = test_tx.signatures[0].clone();

    // Second signing attempt - should be skipped if signature is valid
    sign_transaction(&mut test_tx, wallet.as_ref())
        .context("Second signing should succeed (but should skip if already signed)")?;
    let second_sig = test_tx.signatures[0].clone();

    // Signature should remain the same (not re-signed)
    let sig_unchanged = first_sig == second_sig;

    let builder = builder.check(
        "Jito Double-Signing Prevention - Signature Unchanged",
        || sig_unchanged,
    );

    // Verify the signature is still valid
    use bincode::serialize;
    let message_bytes = serialize(&test_tx.message)
        .map_err(|e| anyhow::anyhow!("Failed to serialize message: {}", e))?;
    let is_valid = second_sig.verify(wallet_pubkey.as_ref(), &message_bytes);

    Ok(builder.check(
        "Jito Double-Signing Prevention - Signature Still Valid",
        || is_valid,
    ))
}

/// Test 4: Signature verification - invalid signatures should be re-signed
fn test_signature_verification(
    builder: ValidationBuilder,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<ValidationBuilder> {
    let wallet_pubkey = wallet.pubkey();

    // Create a transaction
    let test_ix = system_instruction::transfer(
        &wallet_pubkey,
        &Pubkey::new_unique(),
        1_000_000u64,
    );
    let mut test_tx = Transaction::new_with_payer(&[test_ix], Some(&wallet_pubkey));
    test_tx.message.recent_blockhash = blockhash;

    // Manually set an invalid signature (not from this keypair)
    // ✅ FIX: Use sign_transaction instead of direct tx.sign() to avoid KeypairPubkeyMismatch
    // We need to sign with wrong_keypair first, then test that sign_transaction detects and fixes it
    let wrong_keypair = Keypair::new();
    // Manually sign with wrong keypair to create invalid signature
    let message_bytes = test_tx.message.serialize();
    let invalid_signature = wrong_keypair.sign_message(&message_bytes);
    test_tx.signatures[0] = invalid_signature;
    let invalid_sig = test_tx.signatures[0].clone();

    // Now try to sign with the correct keypair
    // sign_transaction should detect invalid signature and re-sign
    sign_transaction(&mut test_tx, wallet.as_ref())
        .context("Re-signing with correct keypair should succeed")?;
    let corrected_sig = test_tx.signatures[0].clone();

    // Signature should have changed (re-signed with correct keypair)
    let sig_changed = invalid_sig != corrected_sig;

    let builder = builder.check(
        "Jito Signature Verification - Invalid Signature Re-signed",
        || sig_changed,
    );

    // Verify the new signature is valid
    use bincode::serialize;
    let message_bytes = serialize(&test_tx.message)
        .map_err(|e| anyhow::anyhow!("Failed to serialize message: {}", e))?;
    let is_valid = corrected_sig.verify(wallet_pubkey.as_ref(), &message_bytes);

    Ok(builder.check(
        "Jito Signature Verification - Corrected Signature Valid",
        || is_valid,
    ))
}

/// Test 5: Jito bundle integration - tip and main transactions should both be signed correctly
fn test_jito_bundle_integration(
    builder: ValidationBuilder,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<ValidationBuilder> {
    let tip_account = Pubkey::new_unique();
    let tip_amount = 10_000_000u64;

    // Create Jito client and bundle
    let jito_client = JitoClient::new(
        "https://mainnet.block-engine.jito.wtf".to_string(),
        tip_account,
        tip_amount,
    );

    let mut bundle = JitoBundle::new(tip_account, tip_amount);

    // Add tip transaction
    jito_client
        .add_tip_transaction(&mut bundle, wallet, blockhash)
        .context("Failed to add tip transaction")?;

    // Verify tip transaction is signed
    // ✅ FIX: Clone the message before adding main transaction to avoid borrow checker error
    let tip_tx = bundle.transactions().get(0)
        .ok_or_else(|| anyhow::anyhow!("Tip transaction not found in bundle"))?;
    let tip_sig = tip_tx.signatures[0];
    let tip_is_signed = tip_sig != solana_sdk::signature::Signature::default();
    // Clone message for later use after mutable borrow
    let tip_message = tip_tx.message.clone();

    let builder = builder.check(
        "Jito Bundle Integration - Tip Transaction Signed",
        || tip_is_signed,
    );

    // Create and add main transaction
    let wallet_pubkey = wallet.pubkey();
    let main_ix = system_instruction::transfer(
        &wallet_pubkey,
        &Pubkey::new_unique(),
        1_000_000u64,
    );
    let mut main_tx = Transaction::new_with_payer(&[main_ix], Some(&wallet_pubkey));
    main_tx.message.recent_blockhash = blockhash;

    sign_transaction(&mut main_tx, wallet.as_ref())
        .context("Failed to sign main transaction")?;

    bundle.add_transaction(main_tx);

    // Verify bundle has 2 transactions
    let tx_count = bundle.transactions().len();
    let builder = builder.check(
        "Jito Bundle Integration - Transaction Count",
        || tx_count == 2,
    );

    // Verify main transaction is signed
    let main_tx = bundle.transactions().get(1)
        .ok_or_else(|| anyhow::anyhow!("Main transaction not found in bundle"))?;
    let main_sig = main_tx.signatures[0];
    let main_is_signed = main_sig != solana_sdk::signature::Signature::default();
    // Clone message for later use
    let main_message = main_tx.message.clone();

    let builder = builder.check(
        "Jito Bundle Integration - Main Transaction Signed",
        || main_is_signed,
    );

    // Verify both signatures are different (different transactions)
    let sigs_different = tip_sig != main_sig;
    let builder = builder.check(
        "Jito Bundle Integration - Signatures Different",
        || sigs_different,
    );

    // Verify both signatures are valid
    use bincode::serialize;
    
    let tip_message_bytes = serialize(&tip_message)
        .map_err(|e| anyhow::anyhow!("Failed to serialize tip message: {}", e))?;
    let tip_valid = tip_sig.verify(wallet_pubkey.as_ref(), &tip_message_bytes);

    let main_message_bytes = serialize(&main_message)
        .map_err(|e| anyhow::anyhow!("Failed to serialize main message: {}", e))?;
    let main_valid = main_sig.verify(wallet_pubkey.as_ref(), &main_message_bytes);

    let builder = builder.check(
        "Jito Bundle Integration - Tip Signature Valid",
        || tip_valid,
    );

    Ok(builder.check(
        "Jito Bundle Integration - Main Signature Valid",
        || main_valid,
    ))
}

