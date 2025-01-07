use crate::{
    ipc_lib::{IpcValidate, SubnetId},
    IpcCreateSubnetMsg, BTC_CONFIRMATIONS, NETWORK,
};
use bitcoin::{address::NetworkUnchecked, TxOut};
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

    #[error("Internal server error: {0}")]
    InternalError(String),
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) => -32603,
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

#[derive(Clone)]
pub struct ServerData {
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
pub struct PreFundSubnetParams {
    multisig_address: bitcoin::Address<NetworkUnchecked>,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
}

#[derive(Serialize, Deserialize)]
pub struct PreFundSubnetResponse {
    tx_id: bitcoin::Txid,
}

pub async fn pre_fund(
    data: Data<Arc<ServerData>>,
    Params(params): Params<PreFundSubnetParams>,
) -> Result<PreFundSubnetResponse, JsonRpcError> {
    let multisig_address = &params
        .multisig_address
        .require_network(NETWORK)
        // TODO better error
        .map_err(|e| RpcError::InvalidParams(format!("Multisig address network: {}", e)))?;

    let outputs = vec![TxOut {
        value: params.amount,
        script_pubkey: multisig_address.script_pubkey(),
    }];

    let tx = crate::wallet::fund_outputs(&data.btc_rpc, outputs, None)
        .map_err(|e| RpcError::InternalError(format!("Error creating transaction: {}", e)))?;

    let tx = crate::wallet::sign_tx(&data.btc_rpc, tx)
        .map_err(|e| RpcError::InternalError(format!("Error creating transaction: {}", e)))?;

    let tx_id = tx.compute_txid();

    crate::bitcoin_utils::submit_to_mempool(&data.btc_rpc, vec![tx])
        .map_err(|e| RpcError::InternalError(format!("Error creating transaction: {}", e)))?;

    Ok(PreFundSubnetResponse { tx_id })
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedblock", get_confirmed_block)
        .with_method("getbalance", get_balance)
        .with_method("createsubnet", create_subnet)
        .finish()
}
