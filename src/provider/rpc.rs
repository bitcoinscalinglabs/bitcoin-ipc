use crate::{
    bitcoin_utils,
    db::{self, Database, StakingChange},
    ipc_lib::{
        self, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg, IpcFundSubnetMsg, IpcJoinSubnetMsg,
        IpcKillSubnetMsg, IpcPrefundSubnetMsg, IpcStakeCollateralMsg, IpcUnstakeCollateralMsg,
        IpcValidate, SubnetId,
    },
    multisig, wallet, NETWORK,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use bitcoin::hashes::Hash;
use bitcoincore_rpc::{Client, RpcApi};
use jsonrpc_v2::{Data, Error as JsonRpcError, ErrorLike, MapRouter, Params};
use log::{debug, error, info, trace, warn};
use num_traits::Zero;
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
    pub db: Arc<db::HeedDb>,
    pub btc_rpc: Arc<Client>,
    pub btc_watchonly_rpc: Arc<Client>,
    pub validator: Option<(bitcoin::XOnlyPublicKey, bitcoin::secp256k1::SecretKey)>,
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

pub async fn get_confirmed_block_height(data: Data<Arc<ServerData>>) -> Result<u64, JsonRpcError> {
    let last_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last processed block from Db: {}", e);
        RpcError::DbError(e)
    })?;

    Ok(last_block_height)
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
    info!("createsubnet: {:?}", msg);

    if let Err(err) = msg.validate() {
        error!("Invalid create message={msg:?}: {err}");
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
    info!("joinsubnet: {:?}", msg);

    if let Err(err) = msg.validate() {
        error!("Invalid join message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let subnet_state = data.db.get_subnet_state(msg.subnet_id).map_err(|e| {
        error!("Error getting subnet info from Db: {}", e);
        RpcError::DbError(e)
    })?;

    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!("Error validating join msg for subnet info: {}", e);
            RpcError::InvalidParams(e.to_string())
        })?;

    // TODO this check should be done in the Db
    let multisig_address = &genesis_info.multisig_address();

    let join_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(JoinSubnetResponse { join_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetGenesisInfoParams {
    subnet_id: SubnetId,
}

pub async fn get_genesis_info(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetGenesisInfoParams>,
) -> Result<db::SubnetGenesisInfo, JsonRpcError> {
    info!("getgenesisinfo: {}", params.subnet_id);

    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    Ok(genesis_info)
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetParams {
    subnet_id: SubnetId,
}

pub async fn get_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<db::SubnetState, JsonRpcError> {
    info!("getsubnet: {}", params.subnet_id);

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", params.subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", params.subnet_id))
        })?;

    Ok(subnet)
}

#[derive(Serialize, Deserialize)]
pub struct PrefundSubnetResponse {
    prefund_txid: bitcoin::Txid,
}

pub async fn prefund_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcPrefundSubnetMsg>,
) -> Result<PrefundSubnetResponse, JsonRpcError> {
    info!("prefundsubnet: {}", msg.subnet_id);

    if let Err(err) = msg.validate() {
        error!("Invalid prefund message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_genesis_info(&genesis_info).map_err(|e| {
        error!("Error validating prefund msg for subnet info: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    // TODO this check should be done in the Db
    let multisig_address = genesis_info.multisig_address();

    let prefund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(PrefundSubnetResponse { prefund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct FundSubnetResponse {
    fund_txid: bitcoin::Txid,
}

pub async fn fund_subnet(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcFundSubnetMsg>,
) -> Result<FundSubnetResponse, JsonRpcError> {
    info!("fundsubnet: {}", msg.subnet_id);

    if let Err(err) = msg.validate() {
        error!("Invalid prefund message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_state = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let multisig_address = subnet_state.multisig_address();

    let fund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(FundSubnetResponse { fund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetRootnetMessagesParams {
    subnet_id: SubnetId,
    block_height: u64,
}

pub async fn get_rootnet_messages(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetRootnetMessagesParams>,
) -> Result<Vec<db::RootnetMessage>, JsonRpcError> {
    info!(
        "getrootnetmessages: {} at {}",
        params.subnet_id, params.block_height
    );

    // Check subnet exists
    data.db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    data.db
        .get_rootnet_msgs_by_height(params.subnet_id, params.block_height)
        .map_err(|e| {
            error!("Error getting rootnet messages from Db: {}", e);
            RpcError::DbError(e).into()
        })
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetCheckpointParams {
    subnet_id: SubnetId,
    number: Option<u64>,
}

pub async fn get_subnet_checkpoint(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetCheckpointParams>,
) -> Result<Option<db::SubnetCheckpoint>, JsonRpcError> {
    info!(
        "getsubnetcheckpoint: {} number={:?}",
        params.subnet_id, params.number
    );

    // Check if subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", params.subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", params.subnet_id))
        })?;

    let last_checkpoint_number = match subnet.last_checkpoint_number {
        Some(number) => number,
        None => {
            return Ok(None);
        }
    };

    // Default to the last checkpoint
    let number = params.number.unwrap_or(last_checkpoint_number);

    data.db
        .get_checkpoint(params.subnet_id, number)
        .map_err(|e| {
            error!("Error getting checkpoint from Db: {}", e);
            RpcError::DbError(e).into()
        })
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GenMultisigSpendPsbtParams {
    subnet_id: SubnetId,
    recipient: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
}

#[derive(Serialize, Deserialize)]
pub struct GenMultisigSpendPsbtResponse {
    unsigned_psbt: bitcoin::Psbt,
    unsigned_psbt_base64: String,
    unsigned_psbt_hash: bitcoin::hashes::sha256::Hash,
    psbt_inputs_signatures: Vec<bitcoin::secp256k1::schnorr::Signature>,
}

pub async fn gen_multisig_spend_psbt(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GenMultisigSpendPsbtParams>,
) -> Result<GenMultisigSpendPsbtResponse, JsonRpcError> {
    info!("gen_multisig_spend_psbt: {:?}", params);

    let (_, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    let recipient = params.recipient.require_network(NETWORK).map_err(|_| {
        RpcError::InvalidParams(format!("Invalid address network, required {NETWORK}"))
    })?;

    let committee_address = subnet
        .committee
        .multisig_address
        .clone()
        .require_network(NETWORK)
        .expect("Multisig should be valid for saved subnet genesis info");
    let committee_keys = subnet.committee.validator_weighted_keys();
    let commitee_threshold = subnet.committee.threshold;

    let unspent = subnet
        .committee
        .get_unspent(&data.btc_watchonly_rpc)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let secp = bitcoin::secp256k1::Secp256k1::new();

    let unsigned_psbt = multisig::construct_spend_psbt(
        &secp,
        &params.subnet_id,
        &committee_keys,
        commitee_threshold,
        &committee_address,
        &unspent,
        false,
        &[bitcoin::TxOut {
            value: params.amount,
            script_pubkey: recipient.script_pubkey(),
        }],
        &fee_rate,
    )
    .map_err(|e| {
        error!(
            "Error generating multisig spend psbt for subnet_id={}: {}",
            &params.subnet_id, e
        );
        RpcError::InternalError(e.to_string())
    })?;

    let validator_keypair = validator_sk.keypair(&secp);

    let (_, psbt_inputs_signatures) =
        multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), validator_keypair).map_err(
            |e| {
                error!(
                    "Error signing multisig spend psbt for subnet_id={}: {}",
                    &params.subnet_id, e
                );
                RpcError::InternalError(e.to_string())
            },
        )?;

    let unsigned_psbt_bytes = unsigned_psbt.serialize();
    let unsigned_psbt_hash = bitcoin::hashes::sha256::Hash::hash(&unsigned_psbt_bytes);
    let unsigned_psbt_base64 = BASE64_STANDARD.encode(unsigned_psbt_bytes);

    Ok(GenMultisigSpendPsbtResponse {
        unsigned_psbt_base64,
        unsigned_psbt_hash,
        psbt_inputs_signatures,
        unsigned_psbt,
    })
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PostBoostrapHandoverParams {
    subnet_id: SubnetId,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PostBoostrapHandoverResponse {
    unsigned_psbt: bitcoin::Psbt,
    unsigned_psbt_base64: String,
    psbt_inputs_signatures: Vec<bitcoin::secp256k1::schnorr::Signature>,
}

pub async fn gen_bootstrap_handover(
    data: Data<Arc<ServerData>>,
    Params(params): Params<PostBoostrapHandoverParams>,
) -> Result<PostBoostrapHandoverResponse, JsonRpcError> {
    info!("post_bootstrap_handover: {:?}", params);

    let (validator_xonly_pubkey, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    // Check genesis info
    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    // Check if self is a validator in the subnet
    if !subnet.is_validator(&validator_xonly_pubkey) {
        error!("Configured validator isn't a validator in the specified subnet.");
        return Err(RpcError::InvalidParams(
            "Configured validator isn't a validator in the specified subnet.".to_string(),
        )
        .into());
    }

    if subnet.last_checkpoint_number.is_some() {
        // NOTE: we should accept to bootstrap handover even if the subnet already has checkpoints
        // normally it should be done before the first checkpoint is posted, but in an
        // exceptional case better to handover to the 2nd committee then not at all?
        // if subnet.last_checkpoint_number.is_some() {
        //     error!(
        //         "Subnet {} already has checkpoints, cannot post bootstrap handover.",
        //         params.subnet_id
        //     );
        //     return Err(RpcError::InvalidParams(format!(
        //         "Subnet {} already has checkpoints, cannot post bootstrap handover.",
        //         params.subnet_id
        //     ))
        //     .into());
        // }

        warn!(
            "Subnet {} already has checkpoints, but proceeding with bootstrap handover.",
            params.subnet_id
        );
    }

    // Collect committee information

    let committee_address = subnet
        .committee
        .multisig_address
        .clone()
        .require_network(NETWORK)
        .expect("Multisig should be valid for saved subnet genesis info");

    // Collect whitelist collateral

    let whitelist_keys: Vec<multisig::WeightedKey> = genesis_info
        .create_subnet_msg
        .whitelist
        .iter()
        // Each key has the same weight
        .map(|xpk| (*xpk, 1))
        .collect::<Vec<_>>();
    let whitelist_threshold: u32 = genesis_info.create_subnet_msg.min_validators.into();
    let whitelist_multisig_addr = genesis_info
        .create_subnet_msg
        .multisig_address_from_whitelist(&params.subnet_id)
        .map_err(|e| {
            error!("Error creating multisig address from whitelist: {}", e);
            RpcError::InternalError(e.to_string())
        })?;

    let unspent =
        wallet::get_unspent_for_address(&data.btc_watchonly_rpc, &whitelist_multisig_addr)
            .map_err(|e| {
                error!("Error getting unspent for whitelist multisig: {}", e);
                RpcError::InternalError(e.to_string())
            })?;

    if unspent.is_empty() {
        return Err(RpcError::InvalidParams(format!(
            "No unspent outputs found for whitelist multisig address {}. The handover was possibly already done.",
            whitelist_multisig_addr
        ))
        .into());
    }

    debug!("Unspent for whitelist multisig: {:?}", unspent);

    // Create the handover PSBT

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);
    let secp = bitcoin::secp256k1::Secp256k1::new();

    let unsigned_psbt = multisig::construct_spend_psbt(
        &secp,
        &params.subnet_id,
        &whitelist_keys,
        whitelist_threshold,
        &committee_address,
        &unspent,
        true,
        &[],
        &fee_rate,
    )
    .map_err(|e| {
        error!(
            "Error generating multisig spend psbt for subnet_id={}: {}",
            &params.subnet_id, e
        );
        RpcError::InternalError(e.to_string())
    })?;

    let validator_keypair = validator_sk.keypair(&secp);

    let (_, psbt_inputs_signatures) =
        multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), validator_keypair).map_err(
            |e| {
                error!(
                    "Error signing multisig spend psbt for subnet_id={}: {}",
                    &params.subnet_id, e
                );
                RpcError::InternalError(e.to_string())
            },
        )?;

    Ok(PostBoostrapHandoverResponse {
        unsigned_psbt: unsigned_psbt.clone(),
        unsigned_psbt_base64: BASE64_STANDARD.encode(unsigned_psbt.serialize()),
        psbt_inputs_signatures,
    })
}

#[derive(Serialize, Deserialize)]
pub struct GenCheckpointPsbtResponse {
    // Checkpoint
    unsigned_psbt: bitcoin::Psbt,
    unsigned_psbt_base64: String,
    unsigned_psbt_hash: bitcoin::hashes::sha256::Hash,
    psbt_inputs_signatures: Vec<bitcoin::secp256k1::schnorr::Signature>,
    // Batch transfer reveal
    batch_transfer_tx_hex: Option<String>,
}

pub async fn gen_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcCheckpointSubnetMsg>,
) -> Result<GenCheckpointPsbtResponse, JsonRpcError> {
    info!("gen_checkpoint_psbt: {:?}", msg);

    let (validator_xonly_pubkey, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    if let Err(err) = msg.validate() {
        error!("Invalid checkpoint message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    if msg.change_address.is_some() {
        return Err(
            RpcError::InvalidParams("Specifying change address not supported".to_string()).into(),
        );
    }

    if !msg.unstakes.len().is_zero() {
        return Err(
            RpcError::InvalidParams("Specifying unstakes not supported".to_string()).into(),
        );
    }

    // Check subnet exists
    let subnet = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Check if self is a validator in the subnet
    if !subnet.is_validator(&validator_xonly_pubkey) {
        error!("Configured validator isn't a validator in the specified subnet.");
        return Err(RpcError::InvalidParams(
            "Configured validator isn't a validator in the specified subnet.".to_string(),
        )
        .into());
    }

    if subnet.is_killed() {
        error!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        );
        return Err(RpcError::InvalidParams(format!(
            "Subnet {} is killed, cannot generate checkpoint.",
            msg.subnet_id
        ))
        .into());
    }

    let should_kill_subnet = subnet.killed == db::SubnetKillState::ToBeKilled;

    // Fill in the subnet addresses, erroring out if any subnet is not found
    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let unspent = subnet
        .committee
        .get_unspent(&data.btc_watchonly_rpc)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    // assume no change in the committee
    let mut next_committee = subnet.committee.clone();
    let current_committee_configuration = subnet.committee.configuration_number;

    if should_kill_subnet {
        // Calculation of total unspent and stake amounts, alongside with transfers and withdrawals to process

        let total_unspent_amount = unspent.iter().map(|u| u.amount).sum::<bitcoin::Amount>();
        let total_stake_amount = subnet
            .committee
            .validators
            .iter()
            .map(|v| v.collateral)
            .sum::<bitcoin::Amount>();

        let total_transfer_amount_to_process = msg
            .transfers
            .iter()
            .map(|t| t.amount)
            .sum::<bitcoin::Amount>();

        let total_withdrawal_amount_to_process = msg
            .withdrawals
            .iter()
            .map(|w| w.amount)
            .sum::<bitcoin::Amount>();

        let total_extra_amount = total_unspent_amount
            - total_stake_amount
            - total_withdrawal_amount_to_process
            - total_transfer_amount_to_process;

        info!(
            "Killing subnet {}, total unspent amount: {}, total stake amount: {}, extra value after withdrawals and transfers: {}",
            msg.subnet_id, total_unspent_amount, total_stake_amount, total_extra_amount
        );

        // Process all validators in the committee for unstaking
        msg.unstakes = subnet
            .committee
            .validators
            .iter()
            .map(|v| ipc_lib::IpcUnstake {
                amount: v.collateral,
                address: v.backup_address.clone(),
                pubkey: v.pubkey.clone(),
            })
            .collect::<Vec<_>>();

        info!(
            "Killing subnet {}, processing {} unstakes.",
            msg.subnet_id,
            msg.unstakes.len()
        );
    } else
    // update next_committee and process unstakes if the configuration number changed
    if msg.next_committee_configuration_number > current_committee_configuration {
        next_committee = data
            .db
            .get_stake_change(msg.subnet_id, msg.next_committee_configuration_number)
            .map_err(|e| {
                error!("Error getting stake change from Db: {}", e);
                RpcError::DbError(e)
            })?
            .ok_or(RpcError::InvalidParams(format!(
                "Stake change with configuration number {} not found.",
                msg.next_committee_configuration_number
            )))?
            .committee_after_change;

        // Get unconfirmed/unprocessed unstakes
        let unconfirmed_stake_changes = data
            .db
            .get_unconfirmed_stake_changes(msg.subnet_id)
            .map_err(|e| {
                error!("Error getting unconfirmed stake changes from Db: {}", e);
                RpcError::DbError(e)
            })?;
        let unstakes = unconfirmed_stake_changes
            .iter()
            .filter_map(|sc| {
                let validator = subnet
                    .committee
                    .validators
                    .iter()
                    .find(|v| v.pubkey == sc.validator_xpk);

                let address = match validator {
                    Some(v) => v.backup_address.clone(),
                    None => {
                        error!(
                            "Validator {} not found in committee for a stake change, skipping",
                            sc.validator_xpk
                        );
                        return None;
                    }
                };

                match sc.change {
                    StakingChange::Withdraw { amount } => Some(ipc_lib::IpcUnstake {
                        amount,
                        address,
                        pubkey: sc.validator_xpk,
                    }),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();

        msg.unstakes = unstakes;

        info!(
            "Rotating committee to configuration number {} multisig address {}",
            msg.next_committee_configuration_number,
            next_committee.address_checked()
        );
        info!("Processing {} unstakes.", msg.unstakes.len());
    }
    // no change if the configuration number is zero or the same
    else if msg.next_committee_configuration_number.is_zero()
        || msg.next_committee_configuration_number == current_committee_configuration
    {
        trace!("gen_checkpoint_psbt: no change to the committee configuration")
    }
    // error if the configuration number is less than the current one, unexpected
    else {
        return Err(RpcError::InvalidParams(format!(
            "Invalid next committee configuration number: {}",
            msg.next_committee_configuration_number
        ))
        .into());
    }

    let unsigned_psbt = msg
        .to_checkpoint_psbt(&subnet.committee, &next_committee, fee_rate, &unspent)
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(
            checkpoint_txid,
            fee_rate,
            &subnet.committee.address_checked(),
        )
        .map_err(|e| {
            error!(
                "Error generating batch transfer tx for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    trace!(
        "checkpoint_txid={} batch_transfer_txid={:?}",
        checkpoint_txid,
        batch_transfer_tx.clone().map(|tx| tx.compute_txid()),
    );

    let batch_transfer_tx_hex =
        batch_transfer_tx.map(|tx| bitcoin::consensus::encode::serialize_hex(&tx));

    let validator_keypair = validator_sk.keypair(&secp);

    let (_, psbt_inputs_signatures) =
        multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), validator_keypair).map_err(
            |e| {
                error!(
                    "Error signing multisig spend psbt for subnet_id={}: {}",
                    &msg.subnet_id, e
                );
                RpcError::InternalError(e.to_string())
            },
        )?;

    let unsigned_psbt_bytes = unsigned_psbt.serialize();
    let unsigned_psbt_hash = bitcoin::hashes::sha256::Hash::hash(&unsigned_psbt_bytes);
    let unsigned_psbt_base64 = BASE64_STANDARD.encode(unsigned_psbt_bytes);

    Ok(GenCheckpointPsbtResponse {
        unsigned_psbt_base64,
        unsigned_psbt_hash,
        psbt_inputs_signatures,
        unsigned_psbt,
        batch_transfer_tx_hex,
    })
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtParams {
    unsigned_psbt_base64: String,
    secret_keys: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtResponse {
    // Include signatures for each input, mapped by pubkey
    signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
}

#[cfg(feature = "dev")]
pub async fn dev_multisign_psbt(
    _data: Data<Arc<ServerData>>,
    Params(params): Params<DevMultisignPsbtParams>,
) -> Result<DevMultisignPsbtResponse, JsonRpcError> {
    info!(
        "dev_multisign_psbt: unsigned_psbt with {} secret_keys",
        params.secret_keys.len()
    );

    // Decode base64 PSBT
    let psbt_bytes = BASE64_STANDARD
        .decode(params.unsigned_psbt_base64.as_bytes())
        .map_err(|e| {
            error!("Invalid base64 format for PSBT: {}", e);
            RpcError::InvalidParams(format!("Invalid base64 format for PSBT: {}", e))
        })?;

    // Deserialize PSBT
    let unsigned_psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).map_err(|e| {
        error!("Invalid PSBT format: {}", e);
        RpcError::InvalidParams(format!("Invalid PSBT format: {}", e))
    })?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Store signatures keyed by public key
    let mut all_signatures = Vec::new();

    // Convert each hex string to a SecretKey and sign the PSBT
    for hex_key in params.secret_keys {
        // Remove "0x" prefix if present
        let hex_key = hex_key.strip_prefix("0x").unwrap_or(&hex_key);

        // Convert hex string to bytes
        let key_bytes = hex::decode(hex_key).map_err(|e| {
            error!("Invalid hex format for secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid hex format for secret key: {}", e))
        })?;

        // Create secret key from bytes
        let secret_key = bitcoin::secp256k1::SecretKey::from_slice(&key_bytes).map_err(|e| {
            error!("Invalid secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid secret key: {}", e))
        })?;

        // Create keypair
        let keypair = secret_key.keypair(&secp);
        let (xonly_pubkey, _) = keypair.x_only_public_key();

        // Sign and collect signatures
        let (_, signatures) = multisig::sign_spend_psbt(&secp, unsigned_psbt.clone(), keypair)
            .map_err(|e| {
                error!("Error signing psbt with key: {}", e);
                RpcError::InternalError(e.to_string())
            })?;

        all_signatures.push((xonly_pubkey, signatures));
    }

    Ok(DevMultisignPsbtResponse {
        signatures: all_signatures,
    })
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeCheckpointPsbtParams {
    subnet_id: SubnetId,
    unsigned_psbt_base64: String,
    signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
    batch_transfer_tx_hex: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeCheckpointPsbtResponse {
    tx: bitcoin::Transaction,
    txid: String,
    tx_hex: String,
    batch_transfer_txid: Option<String>,
}

pub async fn finalize_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(params): Params<FinalizeCheckpointPsbtParams>,
) -> Result<FinalizeCheckpointPsbtResponse, JsonRpcError> {
    info!("finalize_checkpoint_psbt for subnet {}", params.subnet_id);

    // Check subnet exists and get committee info
    let subnet = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    // Get committee info
    let committee_keys = subnet.committee.validator_weighted_keys();
    let committee_threshold = subnet.committee.threshold;

    let batch_transfer_tx: Option<bitcoin::Transaction> = match params.batch_transfer_tx_hex {
        Some(hex) => {
            if hex.is_empty() {
                None
            } else {
                let tx_bytes = hex::decode(hex).map_err(|e| {
                    error!("Invalid hex format for batch transfer tx: {}", e);
                    RpcError::InvalidParams(format!(
                        "Invalid hex format for batch transfer tx: {}",
                        e
                    ))
                })?;

                Some(bitcoin::consensus::deserialize(&tx_bytes).map_err(|e| {
                    error!("Invalid transaction format for batch transfer tx: {}", e);
                    RpcError::InvalidParams(format!(
                        "Invalid transaction format for batch transfer tx: {}",
                        e
                    ))
                })?)
            }
        }
        None => None,
    };

    // Decode base64 PSBT
    let psbt_bytes = BASE64_STANDARD
        .decode(params.unsigned_psbt_base64.as_bytes())
        .map_err(|e| {
            error!("Invalid base64 format for PSBT: {}", e);
            RpcError::InvalidParams(format!("Invalid base64 format for PSBT: {}", e))
        })?;

    // Deserialize PSBT
    let unsigned_psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).map_err(|e| {
        error!("Invalid PSBT format: {}", e);
        RpcError::InvalidParams(format!("Invalid PSBT format: {}", e))
    })?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Create a map of provided signatures indexed by public key
    let signatures_map: std::collections::HashMap<
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    > = params.signatures.into_iter().collect();

    // Prepare signature sets in the same order as committee keys
    let mut signature_sets: Vec<&[bitcoin::secp256k1::schnorr::Signature]> =
        Vec::with_capacity(committee_keys.len());

    // Check if any unrecognized public keys were provided
    for pubkey in signatures_map.keys() {
        if !committee_keys
            .iter()
            .any(|(committee_key, _)| committee_key == pubkey)
        {
            error!("Unrecognized public key in signatures: {}", pubkey);
            return Err(RpcError::InvalidParams(format!(
                "Unrecognized public key in signatures: {}",
                pubkey
            ))
            .into());
        }
    }

    // For each committee key, get the corresponding signatures or use empty set
    for (pubkey, _) in &committee_keys {
        let sigs = match signatures_map.get(pubkey) {
            Some(sigs) => sigs.as_slice(),
            None => &[],
        };
        signature_sets.push(sigs);
    }

    // Finalize the PSBT using multisig::finalize_spend_psbt_from_sigs
    let finalized_tx = multisig::finalize_spend_psbt_from_sigs(
        &secp,
        &params.subnet_id,
        &committee_keys,
        committee_threshold,
        &unsigned_psbt,
        &signature_sets,
    )
    .map_err(|e| {
        error!("Error finalizing PSBT: {}", e);
        RpcError::InternalError(e.to_string())
    })?;

    // Convert transaction to hex
    let tx_hex = hex::encode(bitcoin::consensus::serialize(&finalized_tx));

    // Get transaction ID
    let txid = finalized_tx.compute_txid().to_string();
    let batch_transfer_txid = batch_transfer_tx
        .clone()
        .map(|tx| tx.compute_txid().to_string());

    trace!("checkpoint_txid = {}", txid);
    if let Some(ref batch_transfer_txid) = batch_transfer_txid {
        trace!("batch_transfer_txid = {}", *batch_transfer_txid);
    }

    let mut tx_to_submit = vec![finalized_tx.clone()];

    if let Some(batch_transfer_tx) = batch_transfer_tx {
        tx_to_submit.push(batch_transfer_tx);
    }

    // Send the transaction to the Bitcoin network
    bitcoin_utils::submit_to_mempool(&data.btc_rpc, tx_to_submit).map_err(|e| {
        error!("Error sending transaction to Bitcoin network: {}", e);
        RpcError::InternalError(format!(
            "Error sending transaction to Bitcoin network: {}",
            e
        ))
    })?;

    Ok(FinalizeCheckpointPsbtResponse {
        tx: finalized_tx,
        txid,
        tx_hex,
        batch_transfer_txid,
    })
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeBootstrapHandoverParams {
    subnet_id: SubnetId,
    unsigned_psbt_base64: String,
    signatures: Vec<(
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    )>,
}

#[derive(Serialize, Deserialize)]
pub struct FinalizeBootstrapHandoverResponse {
    tx: bitcoin::Transaction,
    txid: String,
    tx_hex: String,
}

pub async fn finalize_bootstrap_handover(
    data: Data<Arc<ServerData>>,
    Params(params): Params<FinalizeBootstrapHandoverParams>,
) -> Result<FinalizeBootstrapHandoverResponse, JsonRpcError> {
    info!(
        "finalize_bootstrap_handover for subnet {}",
        params.subnet_id
    );

    // Check genesis info
    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet genesis info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    // Get whitelist multisig info for signature verification
    let whitelist_keys: Vec<multisig::WeightedKey> = genesis_info
        .create_subnet_msg
        .whitelist
        .iter()
        // Each key has the same weight
        .map(|xpk| (*xpk, 1))
        .collect::<Vec<_>>();
    let whitelist_threshold: u32 = genesis_info.create_subnet_msg.min_validators.into();

    // Decode base64 PSBT
    let psbt_bytes = BASE64_STANDARD
        .decode(params.unsigned_psbt_base64.as_bytes())
        .map_err(|e| {
            error!("Invalid base64 format for PSBT: {}", e);
            RpcError::InvalidParams(format!("Invalid base64 format for PSBT: {}", e))
        })?;

    // Deserialize PSBT
    let unsigned_psbt = bitcoin::psbt::Psbt::deserialize(&psbt_bytes).map_err(|e| {
        error!("Invalid PSBT format: {}", e);
        RpcError::InvalidParams(format!("Invalid PSBT format: {}", e))
    })?;

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Create a map of provided signatures indexed by public key
    let signatures_map: std::collections::HashMap<
        bitcoin::XOnlyPublicKey,
        Vec<bitcoin::secp256k1::schnorr::Signature>,
    > = params.signatures.into_iter().collect();

    // Prepare signature sets in the same order as whitelist keys
    let mut signature_sets: Vec<&[bitcoin::secp256k1::schnorr::Signature]> =
        Vec::with_capacity(whitelist_keys.len());

    // Check if any unrecognized public keys were provided
    for pubkey in signatures_map.keys() {
        if !whitelist_keys
            .iter()
            .any(|(whitelist_key, _)| whitelist_key == pubkey)
        {
            error!("Unrecognized public key in signatures: {}", pubkey);
            return Err(RpcError::InvalidParams(format!(
                "Unrecognized public key in signatures: {}",
                pubkey
            ))
            .into());
        }
    }

    // For each whitelist key, get the corresponding signatures or use empty set
    for (pubkey, _) in &whitelist_keys {
        let sigs = match signatures_map.get(pubkey) {
            Some(sigs) => sigs.as_slice(),
            None => &[],
        };
        signature_sets.push(sigs);
    }

    // Finalize the PSBT using multisig::finalize_spend_psbt_from_sigs
    let finalized_tx = multisig::finalize_spend_psbt_from_sigs(
        &secp,
        &params.subnet_id,
        &whitelist_keys,
        whitelist_threshold,
        &unsigned_psbt,
        &signature_sets,
    )
    .map_err(|e| {
        error!("Error finalizing bootstrap handover PSBT: {}", e);
        RpcError::InternalError(e.to_string())
    })?;

    // Convert transaction to hex
    let tx_hex = hex::encode(bitcoin::consensus::serialize(&finalized_tx));

    // Get transaction ID
    let txid = finalized_tx.compute_txid().to_string();

    trace!("bootstrap_handover_txid = {}", txid);

    // Send the transaction to the Bitcoin network
    bitcoin_utils::submit_to_mempool(&data.btc_rpc, vec![finalized_tx.clone()]).map_err(|e| {
        error!(
            "Error sending bootstrap handover transaction to Bitcoin network: {}",
            e
        );
        RpcError::InternalError(format!(
            "Error sending bootstrap handover transaction to Bitcoin network: {}",
            e
        ))
    })?;

    Ok(FinalizeBootstrapHandoverResponse {
        tx: finalized_tx,
        txid,
        tx_hex,
    })
}

// Stake collateral

#[derive(Serialize, Deserialize)]
pub struct StakeCollateralResponse {
    txid: bitcoin::Txid,
}

pub async fn stake_collateral(
    data: Data<Arc<ServerData>>,
    Params(msg): Params<IpcStakeCollateralMsg>,
) -> Result<StakeCollateralResponse, JsonRpcError> {
    info!(
        "stakecollateral: {} {} {}",
        msg.subnet_id, msg.pubkey, msg.amount
    );

    if let Err(err) = msg.validate() {
        error!("Invalid stake collateral message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let subnet_state = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_subnet(&subnet_state).map_err(|e| {
        error!(
            "Error validating stake collateral msg for subnet info: {}",
            e
        );
        RpcError::InvalidParams(e.to_string())
    })?;

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(StakeCollateralResponse { txid })
}

// Unstake collateral

#[derive(Serialize, Deserialize)]
pub struct UnstakeCollateralResponse {
    txid: bitcoin::Txid,
}

pub async fn unstake_collateral(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcUnstakeCollateralMsg>,
) -> Result<UnstakeCollateralResponse, JsonRpcError> {
    info!("unstakecollateral: {} {}", msg.subnet_id, msg.amount);

    let (validator_xpk, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    msg.pubkey = Some(validator_xpk);

    if let Err(err) = msg.validate() {
        error!("Invalid unstake collateral message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let subnet_state = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!(
                "Error validating unstake collateral msg for subnet info: {}",
                e
            );
            RpcError::InvalidParams(e.to_string())
        })?;

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address, validator_sk)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(UnstakeCollateralResponse { txid })
}

// Stake changes

#[derive(Serialize, Deserialize)]
pub struct GetStakeChangesParams {
    subnet_id: SubnetId,
    block_height: u64,
}

pub async fn get_stake_changes(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetStakeChangesParams>,
) -> Result<Vec<db::StakeChangeRequest>, JsonRpcError> {
    info!(
        "getstakechanges: {} at {}",
        params.subnet_id, params.block_height
    );

    // Check subnet exists
    data.db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    data.db
        .get_stake_changes_by_height(params.subnet_id, params.block_height)
        .map_err(|e| {
            error!("Error getting stake changes from Db: {}", e);
            RpcError::DbError(e).into()
        })
}

// Kill subnet

#[derive(Serialize, Deserialize)]
pub struct KillSubnetResponse {
    txid: bitcoin::Txid,
}

pub async fn kill_subnet(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcKillSubnetMsg>,
) -> Result<KillSubnetResponse, JsonRpcError> {
    info!("killsubnet: {}", msg.subnet_id);

    let (validator_xpk, validator_sk) = match data.validator {
        Some(validator) => validator,
        None => {
            error!("No validator keypair configured.");
            return Err(
                RpcError::InternalError("No validator keypair configured.".to_string()).into(),
            );
        }
    };

    msg.pubkey = Some(validator_xpk);

    if let Err(err) = msg.validate() {
        error!("Invalid kill subnet message={msg:?}: {err}");
        return Err(RpcError::InvalidParams(err.to_string()).into());
    }

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    let subnet_state = data
        .db
        .get_subnet_state(msg.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;

    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!("Error validating kill subnet msg for subnet info: {}", e);
            RpcError::InvalidParams(e.to_string())
        })?;

    let current_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last block height from Db: {}", e);
        RpcError::DbError(e)
    })?;

    let current_valid_requests = data
        .db
        .get_valid_kill_requests(msg.subnet_id, current_block_height)
        .map_err(|e| {
            error!("Error getting valid kill requests from Db: {}", e);
            RpcError::DbError(e)
        })?;

    // Check if the validator has already submitted a kill request
    if current_valid_requests
        .iter()
        .any(|kr| kr.validator_xpk == validator_xpk)
    {
        return Err(RpcError::InvalidParams(format!(
            "Validator {} has already submitted a kill request for subnet {}",
            validator_xpk, msg.subnet_id
        ))
        .into());
    }

    let multisig_address = subnet_state.multisig_address();

    let txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address, validator_sk)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(KillSubnetResponse { txid })
}

#[derive(Serialize, Deserialize)]
pub struct DevKillSubnetParams {
    subnet_id: SubnetId,
    secret_keys: Vec<String>,
}

#[cfg(feature = "dev")]
pub async fn dev_kill_subnet(
    data: Data<Arc<ServerData>>,
    Params(params): Params<DevKillSubnetParams>,
) -> Result<Vec<KillSubnetResponse>, JsonRpcError> {
    use std::str::FromStr;

    info!(
        "dev_killsubnet: {} with {} secret keys",
        params.subnet_id,
        params.secret_keys.len()
    );

    let genesis_info = data
        .db
        .get_subnet_genesis_info(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    let subnet_state = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            params.subnet_id
        )))?;

    let current_block_height = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last block height from Db: {}", e);
        RpcError::DbError(e)
    })?;

    let valid_kill_requests = data
        .db
        .get_valid_kill_requests(params.subnet_id, current_block_height)
        .map_err(|e| {
            error!("Error getting valid kill requests from Db: {}", e);
            RpcError::DbError(e)
        })?;

    let multisig_address = subnet_state.multisig_address();
    let mut responses = Vec::new();

    for secret_key_str in params.secret_keys {
        // Parse the secret key
        let secret_key = bitcoin::secp256k1::SecretKey::from_str(&secret_key_str).map_err(|e| {
            error!("Invalid secret key: {}", e);
            RpcError::InvalidParams(format!("Invalid secret key: {}", e))
        })?;

        // Get the public key from the secret key
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let (validator_xpk, _) = secret_key.x_only_public_key(&secp);

        if valid_kill_requests
            .iter()
            .any(|kr| kr.validator_xpk == validator_xpk)
        {
            continue; // Skip if this validator has already submitted a kill request
        }

        // Create the kill subnet message
        let msg = IpcKillSubnetMsg {
            subnet_id: params.subnet_id,
            pubkey: Some(validator_xpk),
        };

        // Validate the message
        if let Err(err) = msg.validate() {
            error!("Invalid kill subnet message={msg:?}: {err}");
            return Err(RpcError::InvalidParams(err.to_string()).into());
        }

        // Validate for the specific subnet
        if let Err(err) = msg.validate_for_subnet(&genesis_info, &subnet_state) {
            error!("Error validating kill subnet msg for subnet info: {}", err);
            return Err(RpcError::InvalidParams(err.to_string()).into());
        }

        // Submit to Bitcoin
        let txid = msg
            .submit_to_bitcoin(&data.btc_rpc, &multisig_address, secret_key)
            .map_err(|e| {
                error!("Error submitting kill subnet transaction: {}", e);
                JsonRpcError::internal(e.to_string())
            })?;

        info!("Kill subnet transaction submitted with txid: {}", txid);
        responses.push(KillSubnetResponse { txid });
    }

    Ok(responses)
}

pub fn make_rpc_server(server_data: Arc<ServerData>) -> RpcServer {
    let server = jsonrpc_v2::Server::new()
        .with_data(Data::new(server_data))
        // btc info
        .with_method("getblockhash", get_block_hash)
        .with_method("getblockcount", get_block_count)
        .with_method("getconfirmedcount", get_confirmed_block_height)
        // subnet
        .with_method("createsubnet", create_subnet)
        .with_method("joinsubnet", join_subnet)
        .with_method("getsubnet", get_subnet)
        .with_method("getgenesisinfo", get_genesis_info)
        .with_method("prefundsubnet", prefund_subnet)
        .with_method("fundsubnet", fund_subnet)
        .with_method("killsubnet", kill_subnet)
        // rootnet messages
        .with_method("getrootnetmessages", get_rootnet_messages)
        // multisig
        .with_method("genmultisigspendpsbt", gen_multisig_spend_psbt)
        .with_method("genbootstraphandover", gen_bootstrap_handover)
        .with_method("finalizebootstraphandover", finalize_bootstrap_handover)
        // checkpoints
        .with_method("gencheckpointpsbt", gen_checkpoint_psbt)
        .with_method("finalizecheckpointpsbt", finalize_checkpoint_psbt)
        .with_method("getsubnetcheckpoint", get_subnet_checkpoint)
        // stake changes
        .with_method("stakecollateral", stake_collateral)
        .with_method("unstakecollateral", unstake_collateral)
        .with_method("getstakechanges", get_stake_changes);

    #[cfg(feature = "dev")]
    // dev methods
    let server = server
        .with_method("dev_multisignpsbt", dev_multisign_psbt)
        .with_method("dev_killsubnet", dev_kill_subnet);

    server.finish()
}
