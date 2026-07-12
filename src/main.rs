use std::sync::Arc;

use clap::Parser;
use tsdb::{Db, router};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9090")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let app = router(Arc::new(Db::new()));

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!("listening on {}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
