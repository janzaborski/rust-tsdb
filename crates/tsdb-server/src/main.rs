use std::sync::Arc;

use clap::Parser;
use tsdb_api::Database;
use tsdb_server::Db;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9090")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let db: Arc<dyn Database> = Arc::new(Db::new());
    let app = tsdb_api::router(db);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!("listening on {}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
