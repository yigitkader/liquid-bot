use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::Keypair,
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

    pub fn build(&self, blockhash: solana_sdk::hash::Hash) -> Transaction {
        let mut tx = Transaction::new_with_payer(&self.instructions, Some(&self.payer));
        tx.message.recent_blockhash = blockhash;
        tx
    }
}

pub fn sign_transaction(tx: &mut Transaction, keypair: &Keypair) {
    tx.sign(&[keypair], tx.message.recent_blockhash);
}

pub async fn send_and_confirm(
    tx: Transaction,
    rpc: Arc<RpcClient>,
) -> Result<solana_sdk::signature::Signature> {
    rpc.send_transaction(&tx).await
}

