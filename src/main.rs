use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tsdb::storage::wal::{Durability, WalConfig};
use tsdb::{Db, router};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9090")]
    listen: String,

    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,

    #[arg(long, default_value_t = 10_000)]
    max_buffer_samples: usize,

    #[arg(long)]
    wal_dir: Option<PathBuf>,

    #[arg(long, default_value = "none", value_parser = parse_durability)]
    durability: Durability,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let wal_dir = args.wal_dir.unwrap_or_else(|| args.data_dir.join("wal"));

    let db = Arc::new(Db::open_with(
        &args.data_dir,
        args.max_buffer_samples,
        WalConfig::new(wal_dir),
        args.durability,
    )?);
    let app = router(db.clone());

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!("listening on {}", args.listen);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    db.flush()?;
    Ok(())
}

fn parse_durability(s: &str) -> Result<Durability, String> {
    match s {
        "none" => Ok(Durability::None),
        "written" => Ok(Durability::Written),
        _ => Err(format!(
            "unknown durability {s:?} (expected none or written)"
        )),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install Ctrl-C handler");
}
