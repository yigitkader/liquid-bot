use anyhow::{Context, Result};
use clap::Parser;
use liquid_bot::core::registry::{CliTools, IdlFiles, IdlSources, ProgramIds};
use log;

#[derive(Parser, Debug)]
#[command(name = "update_idls")]
#[command(about = "Fetch and update IDL files from official sources. Uses Anchor CLI by default, falls back to GitHub if Anchor CLI is not available.")]
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
    
    /// Use GitHub sources instead of Anchor CLI (fallback method)
    #[arg(long)]
    use_github: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();
    
    let args = Args::parse();
    
    log::info!("🔄 Starting IDL update process...");
    log::info!("   Force mode: {}", args.force);
    
    let update_all = !args.solend_only && !args.pyth_only && !args.switchboard_only;
    let do_solend = args.solend_only || update_all;
    let do_pyth = args.pyth_only || update_all;
    let do_switchboard = args.switchboard_only || update_all;
    
    let use_anchor_cli = !args.use_github && CliTools::is_anchor_cli_available();
    
    if use_anchor_cli {
        log::info!("📦 Using Anchor CLI for IDL fetching (recommended method)...");
        
        if do_solend {
            log::info!("Fetching Solend IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(
                ProgramIds::SOLEND,
                &IdlFiles::solend(),
                args.force,
            ).await {
                Ok(_) => log::info!("✅ Solend IDL fetched successfully"),
                Err(e) => {
                    log::error!("❌ Failed to fetch Solend IDL with Anchor CLI: {}", e);
                    log::info!("   Falling back to GitHub...");
                    IdlSources::fetch_solend(args.force).await
                        .context("Failed to fetch Solend IDL from GitHub (fallback)")?;
                }
            }
        }
        
        if do_pyth {
            log::info!("Fetching Pyth IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(
                ProgramIds::PYTH,
                &IdlFiles::pyth(),
                args.force,
            ).await {
                Ok(_) => log::info!("✅ Pyth IDL fetched successfully"),
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Pyth IDL with Anchor CLI: {} (optional)", e);
                    log::info!("   Falling back to GitHub...");
                    if let Err(gh_err) = IdlSources::fetch_pyth(args.force).await {
                        log::warn!("⚠️  Failed to fetch Pyth IDL from GitHub: {} (optional, skipping)", gh_err);
                    }
                }
            }
        }
        
        if do_switchboard {
            log::info!("Fetching Switchboard IDL using Anchor CLI...");
            match IdlSources::fetch_with_anchor_cli(
                ProgramIds::SWITCHBOARD,
                &IdlFiles::switchboard(),
                args.force,
            ).await {
                Ok(_) => log::info!("✅ Switchboard IDL fetched successfully"),
                Err(e) => {
                    log::warn!("⚠️  Failed to fetch Switchboard IDL with Anchor CLI: {} (optional)", e);
                    log::info!("   Falling back to GitHub...");
                    if let Err(gh_err) = IdlSources::fetch_switchboard(args.force).await {
                        log::warn!("⚠️  Failed to fetch Switchboard IDL from GitHub: {} (optional, skipping)", gh_err);
                    }
                }
            }
        }
    } else {
        if args.use_github {
            log::info!("🌐 Using GitHub sources for IDL fetching (fallback method)...");
        } else {
            log::warn!("⚠️  Anchor CLI not found, falling back to GitHub sources...");
            log::info!("   Install Anchor CLI for better reliability: cargo install --git https://github.com/coral-xyz/anchor avm && avm install latest && avm use latest");
            log::info!("🌐 Using GitHub sources for IDL fetching...");
        }
        
        if do_solend {
            log::info!("Fetching Solend IDL...");
            IdlSources::fetch_solend(args.force).await
                .context("Failed to fetch Solend IDL")?;
        }
        
        if do_pyth {
            log::info!("Fetching Pyth IDL...");
            IdlSources::fetch_pyth(args.force).await
                .context("Failed to fetch Pyth IDL")?;
        }
        
        if do_switchboard {
            log::info!("Fetching Switchboard IDL...");
            IdlSources::fetch_switchboard(args.force).await
                .context("Failed to fetch Switchboard IDL")?;
        }
    }
    
    log::info!("✅ IDL update completed successfully!");
    
    let (solend_exists, pyth_exists, switchboard_exists) = IdlFiles::check_all();
    
    log::info!("📊 IDL Status:");
    log::info!("   Solend: {}", if solend_exists { "✅" } else { "❌" });
    log::info!("   Pyth: {}", if pyth_exists { "✅" } else { "❌" });
    log::info!("   Switchboard: {}", if switchboard_exists { "✅" } else { "❌" });
    
    Ok(())
}
