use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tsdb::storage::wal::WalConfig;
use tsdb::{Db, router};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9090")]
    listen: String,

    #[arg(long, default_value = "./data/wal")]
    wal_dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let db = Db::open(WalConfig {
        dir: args.wal_dir,
        segment_max_bytes: 64 * 1024 * 1024,
    })?;

    let app = router(Arc::new(db));

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!("listening on {}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
