use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use crate::blockchain::rpc_client::RpcClient;
use anyhow::Result;
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

    /// Build an UNSIGNED transaction from the builder.
    /// 
    /// ✅ CRITICAL: This always creates an UNSIGNED transaction.
    /// The returned transaction has empty signatures (all zeros).
    /// You MUST call sign_transaction() after build() to sign it.
    /// 
    /// This method is safe to call multiple times - each call creates a fresh transaction.
    /// However, you should use a fresh blockhash for each transaction to avoid replay attacks.
    pub fn build(&self, blockhash: solana_sdk::hash::Hash) -> Transaction {
        let mut tx = Transaction::new_with_payer(&self.instructions, Some(&self.payer));
        tx.message.recent_blockhash = blockhash;
        // ✅ Transaction::new_with_payer() creates transactions with empty signatures
        // This is safe - transaction is unsigned and ready to be signed
        tx
    }
}

/// Sign a transaction with the given keypair.
/// 
/// CRITICAL: This function should only be called on UNSIGNED transactions.
/// The TransactionBuilder::build() method creates unsigned transactions, so this is safe to call
/// immediately after build(). However, if a transaction is already signed, calling this again
/// can cause double-signing errors in Solana.
/// 
/// To prevent double-signing:
/// 1. Always build a fresh transaction using TransactionBuilder::build()
/// 2. Sign it once using this function
/// 3. Never re-sign an already-signed transaction
/// 
/// ✅ CRITICAL FIX: Returns Result to prevent silent failures
/// Caller must handle the error - silent return was dangerous for Jito bundles
pub fn sign_transaction(tx: &mut Transaction, keypair: &Keypair) -> Result<()> {
    // CRITICAL: Ensure transaction is unsigned before signing
    // Transaction::new_with_payer() creates transactions with empty signatures,
    // but we check here to be safe and prevent accidental double-signing
    let signer_pubkey = keypair.pubkey();
    let signer_index = tx.message.account_keys.iter()
        .position(|&pk| pk == signer_pubkey);
    
    if let Some(index) = signer_index {
        // Check if this signer already has a non-zero signature
        if index < tx.signatures.len() {
            let existing_sig = &tx.signatures[index];
            // A default (all zeros) signature means not signed yet
            let is_signed = *existing_sig != solana_sdk::signature::Signature::default();
            
            if is_signed {
                // ✅ CRITICAL FIX: Return error instead of silent return
                // Silent return was dangerous - caller didn't know transaction wasn't signed
                // This is especially critical for Jito bundles where unsigned transactions fail
                return Err(anyhow::anyhow!(
                    "Transaction already signed by keypair {} at index {} - double signing prevented",
                    signer_pubkey, index
                ));
            }
        }
    }
    
    // Transaction is unsigned or signer not found in account_keys - safe to sign
    tx.sign(&[keypair], tx.message.recent_blockhash);
    Ok(())
}

pub async fn send_and_confirm(
    tx: Transaction,
    rpc: Arc<RpcClient>,
) -> Result<solana_sdk::signature::Signature> {
    rpc.send_transaction(&tx).await
}

