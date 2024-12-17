use actix_web;
use bitcoin_ipc::provider::rpc;
use bitcoin_ipc::{bitcoin_utils, utils};
use std::sync::Arc;

use bitcoincore_rpc::{Client, RpcApi};

fn make_bitcoincore_rpc() -> Arc<Client> {
    let (rpc_user, rpc_pass, rpc_url, wallet_name) = match utils::load_env() {
        Ok(env) => env,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };

    let rpc = match bitcoin_utils::init_rpc_client(rpc_user, rpc_pass, rpc_url) {
        Ok(rpc) => rpc,
        Err(e) => {
            panic!("Error: {}", e);
        }
    };
    let _ = rpc.load_wallet(&wallet_name);
    let rpc = Arc::new(rpc);
    rpc
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Init the bitcoincore_rpc client

    let btc_rpc = make_bitcoincore_rpc();

    // Load the provider config

    let config = match utils::load_config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Couldn't load provider config: {}", e);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Couldn't load provider config",
            ));
        }
    };

    // Construct the JSON-RPC server

    let server_data = Arc::new(rpc::ServerData { btc_rpc, config });
    let rpc_server = rpc::make_rpc_server(server_data);

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| "3030".to_string());
    println!("Server is running on http://127.0.0.1:{}", port);

    let addr = format!("127.0.0.1:{}", port);

    actix_web::HttpServer::new(move || {
        let rpc = rpc_server.clone();
        actix_web::App::new().service(
            actix_web::web::service("/api")
                .guard(actix_web::guard::Post())
                .finish(rpc.into_web_service()),
        )
    })
    .bind(addr)?
    .run()
    .await
}
