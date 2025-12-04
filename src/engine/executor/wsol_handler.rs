use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::transaction::TransactionBuilder;
use crate::core::config::Config;
use crate::protocol::solend::instructions::{build_unwrap_sol_instruction, build_wrap_sol_instruction, is_wsol_mint};
use crate::utils::helpers::read_ata_balance;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::time::Duration;

/// WSOL handling utilities for Executor
pub struct WsolHandler;

impl WsolHandler {
    /// Add WSOL wrap instructions to transaction builder if needed
    pub async fn add_wrap_instructions_if_needed(
        wallet_pubkey: &Pubkey,
        wsol_ata: &Pubkey,
        needed_amount: u64,
        rpc: &Arc<RpcClient>,
        config: &Config,
        tx_builder: &mut TransactionBuilder,
    ) -> Result<()> {
        let wsol_balance = read_ata_balance(wsol_ata, rpc).await.unwrap_or(0);

        if wsol_balance < needed_amount {
            let wrap_amount = needed_amount.saturating_sub(wsol_balance);

            let wallet_account = rpc
                .get_account(wallet_pubkey)
                .await
                .context("Failed to fetch wallet account for native SOL balance check")?;
            let native_sol_balance = wallet_account.lamports;
            let min_reserve = config.min_reserve_lamports;
            let required_sol = wrap_amount
                .checked_add(min_reserve)
                .ok_or_else(|| anyhow::anyhow!("Wrap amount + reserve overflow"))?;

            if native_sol_balance < required_sol {
                return Err(anyhow::anyhow!(
                    "Insufficient native SOL for WSOL wrap: need {} lamports (wrap: {} + reserve: {}), have {} lamports",
                    required_sol,
                    wrap_amount,
                    min_reserve,
                    native_sol_balance
                ))
                .context("Cannot wrap SOL to WSOL - insufficient native SOL balance");
            }

            log::info!(
                "Executor: Wrapping {} lamports of native SOL to WSOL (current WSOL balance: {}, needed: {}, native SOL: {})",
                wrap_amount,
                wsol_balance,
                needed_amount,
                native_sol_balance
            );

            let wrap_instructions = build_wrap_sol_instruction(
                wallet_pubkey,
                wsol_ata,
                wrap_amount,
                Some(rpc),
                Some(config),
            )
            .await
            .context("Failed to build WSOL wrap instructions")?;
            for wrap_ix in wrap_instructions {
                tx_builder.add_instruction(wrap_ix);
            }
        } else {
            log::debug!(
                "Executor: Sufficient WSOL balance ({} >= {}), no wrap needed",
                wsol_balance,
                needed_amount
            );
        }

        Ok(())
    }

}

