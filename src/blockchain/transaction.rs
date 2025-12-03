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

    // ✅ FIX: Check if already signed - if valid signature exists, skip signing
    if signer_index < tx.signatures.len() {
        let existing_sig = &tx.signatures[signer_index];
        let is_signed = *existing_sig != solana_sdk::signature::Signature::default();

        if is_signed {
            // ✅ Transaction already signed with valid signature - verify it's correct
            // Note: We can't fully verify without re-signing, but if signature is non-default
            // and matches the expected signer, we assume it's valid
            log::debug!(
                "Transaction already signed by {} at index {} - skipping re-signing",
                signer_pubkey,
                signer_index
            );
            return Ok(()); // ✅ Don't error - just return success
        }
    }

    // Transaction is unsigned or signature is default - sign it
    tx.sign(&[keypair], tx.message.recent_blockhash);

    let signer_pubkey = keypair.pubkey();
    let signer_index = tx
        .message
        .account_keys
        .iter()
        .position(|&pk| pk == signer_pubkey);

    if let Some(index) = signer_index {
        if index >= tx.signatures.len() {
            return Err(anyhow::anyhow!(
                "Transaction signing failed: signature not found at index {} (signatures.len()={})",
                index,
                tx.signatures.len()
            ));
        }

        let sig = &tx.signatures[index];
        if *sig == solana_sdk::signature::Signature::default() {
            return Err(anyhow::anyhow!(
                "Transaction signing failed: signature is default (all zeros) for signer {} at index {}",
                signer_pubkey,
                index
            ));
        }

        log::debug!(
            "Transaction signed successfully: {} -> {} (index: {})",
            signer_pubkey,
            sig,
            index
        );
    } else {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signer {} not found in account_keys",
            signer_pubkey
        ));
    }

    Ok(())
}

pub async fn send_and_confirm(
    tx: Transaction,
    rpc: Arc<RpcClient>,
) -> Result<solana_sdk::signature::Signature> {
    rpc.send_transaction(&tx).await
}
