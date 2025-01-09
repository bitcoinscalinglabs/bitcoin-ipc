use crate::{
    db::{self, Database, HeedDb},
    ipc_lib::{IpcJoinSubnetMsg, IpcValidate, SubnetId},
    IpcCreateSubnetMsg, BTC_CONFIRMATIONS, NETWORK,
};
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, ErrorLike, MapRouter, Params};
use log::error;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;

pub type RpcServer = Arc<jsonrpc_v2::Server<MapRouter>>;

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Unauthorized: Invalid token")]
    Unauthorized,

    #[error("Invalid params: {0}")]
    InvalidParams(String),

    #[error("Database error occurred: {0}")]
    DbError(#[from] db::DbError),

    #[error("Internal server error: {0}")]
    InternalError(String),
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) | RpcError::DbError(_) => -32603,
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }
}

impl actix_web::error::ResponseError for RpcError {
    fn error_response(&self) -> actix_web::HttpResponse {
        let json_rpc_error = json!({
            "jsonrpc": "2.0",
            "error": {
                "code": &self.code(),
                "message": &self.message()
            },
            "id": null
        });

        actix_web::HttpResponse::Ok()
            .content_type("application/json")
            .body(json_rpc_error.to_string())
    }
}

// TODO use generics
#[derive(Clone)]
pub struct ServerData {
    pub db: Arc<HeedDb>,
    pub btc_rpc: Arc<Client>,
}

//
// Bitcoin RPC
//

#[derive(Serialize, Deserialize)]
pub struct GetBlockHashParams {
    height: u64,
}

pub async fn get_block_hash(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetBlockHashParams>,
) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_hash(params.height) {
        Ok(block_hash) => Ok(block_hash.to_string()),
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

pub async fn get_block_count(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(block_count) => Ok(block_count),
        Err(e) => Err(RpcError::InternalError(e.to_string()).into()),
    }
}

pub async fn get_confirmed_block(data: Data<Arc<ServerData>>) -> Result<String, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_block_count() {
        Ok(current_height) => {
            // Since BTC_CONFIRMATIONS is 0 in regtest and sigtest
            // Clippy will complain about absurd comparisons
            #[allow(clippy::absurd_extreme_comparisons)]
            if current_height < BTC_CONFIRMATIONS {
                return Err(JsonRpcError::internal(
                    "Not enough blocks to have a confirmed block",
                ));
            }

            let confirmed_block_height = current_height - BTC_CONFIRMATIONS;
            match client.get_block_hash(confirmed_block_height) {
                Ok(block_hash) => Ok(block_hash.to_string()),
                Err(e) => Err(JsonRpcError::internal(e)),
            }
        }
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

/// Get the balance of the wallet in Satoshis
/// Note: Bitcoin Core RPC returns the balance in BTC (using f64)
pub async fn get_balance(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let client = data.btc_rpc.as_ref();

    match client.get_balance(None, None) {
        Ok(balance) => Ok(balance.to_sat()),
        Err(e) => Err(JsonRpcError::internal(e)),
    }
}

//
// IPC
//

#[derive(Serialize, Deserialize)]
pub struct CreateSubnetResponse {
    subnet_id: SubnetId,
}

pub async fn create_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcCreateSubnetMsg>,
) -> Result<CreateSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_id = msg
        .submit_to_bitcoin(&data.btc_rpc)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    // Return the response
    Ok(CreateSubnetResponse { subnet_id })
}

#[derive(Serialize, Deserialize)]
pub struct JoinSubnetResponse {
    join_txid: bitcoin::Txid,
}

pub async fn join_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcJoinSubnetMsg>,
) -> Result<JoinSubnetResponse, JsonRpcError> {
    if let Err(err) = msg.validate() {
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .await
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    // Check if the subnet is already bootstrapped
    if subnet_info.bootstrapped {
        return Err(RpcError::InvalidParams(format!(
            "Subnet {} is already bootstrapped.",
            msg.subnet_id
        ))
        .into());
    }

    if subnet_info.create_subnet_msg.min_validator_stake < msg.collateral {
        return Err(RpcError::InvalidParams(format!(
            "Collateral must be at least {}",
            subnet_info.create_subnet_msg.min_validator_stake
        ))
        .into());
    }

    // check already prefunded

    // TODO this check should be done in the Db
    let multisig_address = &subnet_info
        .multisig_address()
        .require_network(NETWORK)
        .map_err(|_| {
            RpcError::InvalidParams(format!("Multisig address must be for {} network", NETWORK))
        })?;

    let join_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(JoinSubnetResponse { join_txid })
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedblock", get_confirmed_block)
        .with_method("getbalance", get_balance)
        .with_method("createsubnet", create_subnet)
        .with_method("joinsubnet", join_subnet)
        .finish()
}
