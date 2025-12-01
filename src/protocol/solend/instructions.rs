use anyhow::Result;
use solana_sdk::instruction::Instruction;
use crate::core::types::Opportunity;
use solana_sdk::pubkey::Pubkey;
use crate::blockchain::rpc_client::RpcClient;
use std::sync::Arc;

pub async fn build_liquidate_obligation_ix(
    _opportunity: &Opportunity,
    _liquidator: &Pubkey,
    _rpc: Option<Arc<RpcClient>>,
) -> Result<Instruction> {
    Err(anyhow::anyhow!("Solend liquidation instruction building requires full implementation - not yet implemented"))
}
