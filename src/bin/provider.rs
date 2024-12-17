use actix_web;
use bitcoin_ipc::{bitcoin_utils, utils};
use jsonrpc_v2::{Data, Error as JsonRpcError, Params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use bitcoincore_rpc::{Client, RpcApi};

#[derive(Clone)]
struct ServerData {
    btc_rpc: Arc<Client>,
    config: utils::Config,
}

#[derive(Serialize, Deserialize)]
struct GetBlockHashParams {
    height: u64,
}

async fn getblockhash(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetBlockHashParams>,
) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_hash(params.height) {
        Ok(block_hash) => Ok(block_hash.to_string()),
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

async fn getblockcount(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(block_count) => Ok(block_count),
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

async fn getconfirmedblock(data: Data<Arc<ServerData>>) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    let confirmations = data.config.ipc_finalization_parameter;

    match client.get_block_count() {
        Ok(current_height) => {
            if current_height < confirmations {
                return Err(JsonRpcError::Full {
                    code: -1,
                    message: "Not enough blocks to have a final block".to_string(),
                    data: None,
                });
            }

            let final_block_height = current_height - confirmations;
            match client.get_block_hash(final_block_height) {
                Ok(block_hash) => Ok(block_hash.to_string()),
                Err(e) => Err(JsonRpcError::Full {
                    code: -1,
                    message: e.to_string(),
                    data: None,
                }),
            }
        }
        Err(e) => Err(JsonRpcError::Full {
            code: -1,
            message: e.to_string(),
            data: None,
        }),
    }
}

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

    let server_data = Arc::new(ServerData { btc_rpc, config });

    let rpc_server = jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        .with_method("getblockhash", getblockhash)
        .with_method("getblockcount", getblockcount)
        .with_method("getconfirmedblock", getconfirmedblock)
        .finish();

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
