use crate::blockchain::rpc_client::RpcClient;
use anyhow::Result;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::sync::Arc;

pub struct TransactionBuilder {
    instructions: Vec<solana_sdk::instruction::Instruction>,
    payer: Pubkey,
}

impl TransactionBuilder {
    pub fn new(payer: Pubkey) -> Self {
        TransactionBuilder {
            instructions: Vec::new(),
            payer,
        }
    }

    pub fn add_compute_budget(&mut self, units: u32, price: u64) -> &mut Self {
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(units);
        let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(price);
        self.instructions.push(compute_budget_ix);
        self.instructions.push(priority_fee_ix);
        self
    }

    pub fn add_instruction(&mut self, ix: solana_sdk::instruction::Instruction) -> &mut Self {
        self.instructions.push(ix);
        self
    }

    pub fn build(&self, blockhash: solana_sdk::hash::Hash) -> Transaction {
        let mut tx = Transaction::new_with_payer(&self.instructions, Some(&self.payer));
        tx.message.recent_blockhash = blockhash;
        tx
    }
}

pub fn sign_transaction(tx: &mut Transaction, keypair: &Keypair) -> Result<()> {
    let signer_pubkey = keypair.pubkey();
    
    // Find signer index in account_keys
    let signer_index = tx
        .message
        .account_keys
        .iter()
        .position(|&pk| pk == signer_pubkey)
        .ok_or_else(|| anyhow::anyhow!(
            "Transaction signing failed: signer {} not found in account_keys",
            signer_pubkey
        ))?;

    // ✅ FIX: Ensure signature array is properly initialized
    // Solana transactions: signature array size = num_required_signatures (not account_keys.len())
    // Transaction::new_with_payer initializes signatures to num_required_signatures size
    // If signatures array is empty or wrong size, we need to resize it
    let num_required_signatures = tx.message.header.num_required_signatures as usize;
    if tx.signatures.len() != num_required_signatures {
        // Resize signature array to match num_required_signatures
        tx.signatures.resize(num_required_signatures, solana_sdk::signature::Signature::default());
    }
    
    // ✅ FIX: Verify signer_index is within required signatures range
    // Signer must be in the first num_required_signatures accounts
    if signer_index >= num_required_signatures {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signer {} at index {} is not in required signatures range (0..{})",
            signer_pubkey,
            signer_index,
            num_required_signatures
        ));
    }

    // ✅ CRITICAL FIX: Verify if transaction is already signed with correct keypair
    // Problem: Previous check only verified signature is non-default, but didn't verify
    //   if signature is valid for the expected keypair. This could cause issues if:
    //   - Transaction was signed with wrong keypair (signature mismatch)
    //   - Transaction was partially signed (invalid signature)
    // Solution: Verify signature validity before skipping re-signing
    //   This prevents double-signing AND ensures signature is correct for the keypair
    if signer_index < tx.signatures.len() {
        let existing_sig = &tx.signatures[signer_index];
        let is_signed = *existing_sig != solana_sdk::signature::Signature::default();

        if is_signed {
            // ✅ FIX: Verify signature is valid for this keypair before skipping
            // This ensures we don't skip signing if signature is invalid or from wrong keypair
            // Transaction message needs to be serialized for verification
            use bincode::serialize;
            let message_bytes = serialize(&tx.message)
                .map_err(|e| anyhow::anyhow!("Failed to serialize transaction message for signature verification: {}", e))?;
            
            // Verify signature with the expected keypair
            let is_valid = existing_sig.verify(signer_pubkey.as_ref(), &message_bytes);
            
            if is_valid {
                // ✅ Transaction already signed with VALID signature for this keypair - skip re-signing
                // This is SAFE because:
                //   1. Signature is verified to be valid for the expected keypair
                //   2. Re-signing would create a different signature (even if valid), causing mismatch
                //   3. Jito bundles: Each transaction is signed once (tip tx, main tx) - this is correct
                log::debug!(
                    "Transaction already signed with valid signature by {} at index {} - skipping re-signing to prevent double-signing",
                    signer_pubkey,
                    signer_index
                );
                return Ok(());
            } else {
                // ⚠️ Signature exists but is INVALID for this keypair
                // This could happen if:
                //   - Transaction was signed with wrong keypair
                //   - Transaction message changed after signing
                //   - Signature is corrupted
                // Solution: Re-sign with correct keypair (will overwrite invalid signature)
                log::warn!(
                    "Transaction signature at index {} is invalid for keypair {} - re-signing with correct keypair",
                    signer_index,
                    signer_pubkey
                );
                // Continue to signing below - will overwrite invalid signature
            }
        }
    }

    // ✅ Transaction is unsigned, signature is default, or signature is invalid - sign it
    // This code path is reached if:
    //   1. Transaction is not signed (signature is default)
    //   2. Signature exists but is invalid for this keypair
    //   3. Signer index is valid and within bounds
    //   4. Signature array size matches num_required_signatures
    // 
    // ✅ FIX: Manual signing to avoid KeypairPubkeyMismatch error
    // Problem: tx.sign() expects keypair pubkeys to match account_keys[0..num_required_signatures] in order
    // Solution: Manually sign the transaction message using Solana's signing algorithm
    // 
    // Solana transaction signing process:
    // 1. Serialize the message (without signatures - they're already default)
    // 2. Sign the serialized message with the keypair
    // 3. Assign the signature to the correct index
    // 
    // Serialize the transaction message for signing
    // Note: Solana uses a specific serialization format for transaction messages
    // The message is serialized without the signatures array
    let message_bytes = tx.message.serialize();
    
    // Sign the message with the keypair
    // Use the Signer trait's sign_message method which handles the signing correctly
    let signature = keypair.sign_message(&message_bytes);
    
    // Assign signature to the correct index
    if signer_index >= tx.signatures.len() {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signer index {} out of bounds (signatures.len()={})",
            signer_index,
            tx.signatures.len()
        ));
    }
    tx.signatures[signer_index] = signature;

    // Verify signing succeeded
    // Note: signer_index is already known from above, no need to recalculate
    if signer_index >= tx.signatures.len() {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signature not found at index {} (signatures.len()={})",
            signer_index,
            tx.signatures.len()
        ));
    }

    let sig = &tx.signatures[signer_index];
    if *sig == solana_sdk::signature::Signature::default() {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signature is default (all zeros) for signer {} at index {}",
            signer_pubkey,
            signer_index
        ));
    }

    log::debug!(
        "Transaction signed successfully: {} -> {} (index: {})",
        signer_pubkey,
        sig,
        signer_index
    );

    Ok(())
}

pub async fn send_and_confirm(
    tx: Transaction,
    rpc: Arc<RpcClient>,
) -> Result<solana_sdk::signature::Signature> {
    rpc.send_transaction(&tx).await
}
