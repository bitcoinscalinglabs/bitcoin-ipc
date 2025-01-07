use bitcoin_ipc::bitcoin_utils::make_rpc_client_from_env;
use bitcoin_ipc::db::Db;
use bitcoin_ipc::provider;
use dotenv::dotenv;
use log::error;
use std::sync::Arc;

const DEFAULT_PROVIDER_PORT: &str = "3030";

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Initialize the database

    let db = Arc::new(
        Db::new(
            &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            true, // using db in read-only mode
        )
        .await
        .expect("Failed to initialize database"),
    );

    // Load auth token from env

    let token = std::env::var("PROVIDER_AUTH_TOKEN").map_err(|e| {
        error!("Couldn't load PROVIDER_AUTH_TOKEN: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::Other,
            "Couldn't load PROVIDER_AUTH_TOKEN",
        )
    })?;

    // Init the bitcoincore_rpc client

    let btc_rpc = Arc::new(make_rpc_client_from_env());

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| DEFAULT_PROVIDER_PORT.to_string());

    let server_data = Arc::new(provider::ServerData { db, btc_rpc });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}
