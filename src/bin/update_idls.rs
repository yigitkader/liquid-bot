//! IDL Güncelleme Binary
//! 
//! Bu binary, tüm IDL dosyalarını resmi kaynaklardan çeker ve günceller.
//! Registry modülündeki IdlSources fonksiyonlarını kullanır.

use anyhow::{Context, Result};
use clap::Parser;
use liquid_bot::core::registry::IdlSources;
use log;

#[derive(Parser, Debug)]
#[command(name = "update_idls")]
#[command(about = "Fetch and update IDL files from official sources")]
struct Args {
    /// Force update even if IDL files already exist
    #[arg(short, long)]
    force: bool,
    
    /// Update only Solend IDL
    #[arg(long)]
    solend_only: bool,
    
    /// Update only Pyth IDL
    #[arg(long)]
    pyth_only: bool,
    
    /// Update only Switchboard IDL
    #[arg(long)]
    switchboard_only: bool,
    
    /// Use Anchor CLI to fetch IDL (requires Anchor CLI installed)
    #[arg(long)]
    use_anchor_cli: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();
    
    let args = Args::parse();
    
    log::info!("🔄 Starting IDL update process...");
    log::info!("   Force mode: {}", args.force);
    
    if args.use_anchor_cli {
        log::info!("📦 Using Anchor CLI for IDL fetching...");
        
        use liquid_bot::core::registry::{CliTools, IdlFiles, ProgramIds};
        
        if !CliTools::is_anchor_cli_available() {
            return Err(anyhow::anyhow!(
                "Anchor CLI not found. Please install:\n  cargo install --git https://github.com/coral-xyz/anchor avm && avm install latest && avm use latest"
            ));
        }
        
        if args.solend_only || (!args.pyth_only && !args.switchboard_only) {
            log::info!("Fetching Solend IDL using Anchor CLI...");
            IdlSources::fetch_with_anchor_cli(
                ProgramIds::SOLEND,
                &IdlFiles::solend(),
            ).await
            .context("Failed to fetch Solend IDL with Anchor CLI")?;
        }
        
        if args.pyth_only || (!args.solend_only && !args.switchboard_only) {
            log::info!("Fetching Pyth IDL using Anchor CLI...");
            IdlSources::fetch_with_anchor_cli(
                ProgramIds::PYTH,
                &IdlFiles::pyth(),
            ).await
            .context("Failed to fetch Pyth IDL with Anchor CLI")?;
        }
        
        if args.switchboard_only || (!args.solend_only && !args.pyth_only) {
            log::info!("Fetching Switchboard IDL using Anchor CLI...");
            IdlSources::fetch_with_anchor_cli(
                ProgramIds::SWITCHBOARD,
                &IdlFiles::switchboard(),
            ).await
            .context("Failed to fetch Switchboard IDL with Anchor CLI")?;
        }
    } else {
        log::info!("🌐 Using GitHub sources for IDL fetching...");
        
        if args.solend_only {
            log::info!("Fetching Solend IDL...");
            IdlSources::fetch_solend().await
                .context("Failed to fetch Solend IDL")?;
        } else if args.pyth_only {
            log::info!("Fetching Pyth IDL...");
            IdlSources::fetch_pyth().await
                .context("Failed to fetch Pyth IDL")?;
        } else if args.switchboard_only {
            log::info!("Fetching Switchboard IDL...");
            IdlSources::fetch_switchboard().await
                .context("Failed to fetch Switchboard IDL")?;
        } else {
            // Fetch all
            IdlSources::fetch_all(args.force).await
                .context("Failed to fetch IDL files")?;
        }
    }
    
    log::info!("✅ IDL update completed successfully!");
    
    // Show summary
    use liquid_bot::core::registry::IdlFiles;
    let (solend_exists, pyth_exists, switchboard_exists) = IdlFiles::check_all();
    
    log::info!("📊 IDL Status:");
    log::info!("   Solend: {}", if solend_exists { "✅" } else { "❌" });
    log::info!("   Pyth: {}", if pyth_exists { "✅" } else { "❌" });
    log::info!("   Switchboard: {}", if switchboard_exists { "✅" } else { "❌" });
    
    Ok(())
}

