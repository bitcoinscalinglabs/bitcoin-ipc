use crate::{
    bitcoin_utils, collateral,
    db::{self, DatabaseCore, StakingChange},
    eots, epochs,
    ipc_lib::{
        self, validate_erc_batch_data, IpcCheckpointSubnetMsg, IpcCreateSubnetMsg,
        IpcFundSubnetMsg, IpcJoinSubnetMsg, IpcJoinUeMsg, IpcKillSubnetMsg, IpcLibError,
        IpcPrefundSubnetMsg, IpcStakeCollateralMsg, IpcUnstakeCollateralMsg, IpcValidate,
        SubnetId,
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

#[cfg(feature = "emission_chain")]
use crate::db::DatabaseRewardExtensions;
#[cfg(feature = "emission_chain")]
use crate::rewards::RewardConfig;

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

    #[error("the monitor has not processed Bitcoin height {requested} yet: last processed {processed}")]
    MonitorBehind { requested: u64, processed: u64 },
}

impl ErrorLike for RpcError {
    fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized => -32001,
            RpcError::InvalidParams(_) => -32602,
            RpcError::InternalError(_) | RpcError::DbError(_) => -32603,
            RpcError::MonitorBehind { .. } => -32004,
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
    /// The EOTS key, the seed of its nonces and the votes signed, from `EOTS_SK_PATH` and
    /// `EOTS_SEED_PATH`
    pub eots_signer: Option<Arc<std::sync::Mutex<super::eots_signer::EotsSigner>>>,
}

/// The number of heights the EOTS nonce commitment of a `join-ue` covers by default.
pub const DEFAULT_EOTS_NONCE_COUNT: u32 = 100_000;

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
    pub subnet_id: SubnetId,
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

/// Request struct for the join_subnet RPC endpoint.
/// Separate from `IpcJoinSubnetMsg` because collateral is provided in the JSON request
/// but skipped in postcard serialization (it's derived from the transaction output).
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinSubnetRequest {
    pub subnet_id: SubnetId,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    pub collateral: bitcoin::Amount,
    pub ip: std::net::SocketAddr,
    pub backup_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    pub pubkey: bitcoin::XOnlyPublicKey,
    /// With it, a `join-ue` for this many epochs
    #[serde(default)]
    pub epochs: Option<u32>,
    /// The first subnet height the EOTS nonce commitment of a `join-ue` covers (default 1)
    #[serde(default)]
    pub eots_nonce_start: Option<u64>,
    /// The number of heights it covers (default `DEFAULT_EOTS_NONCE_COUNT`)
    #[serde(default)]
    pub eots_nonce_count: Option<u32>,
}

impl From<JoinSubnetRequest> for IpcJoinSubnetMsg {
    fn from(req: JoinSubnetRequest) -> Self {
        Self {
            subnet_id: req.subnet_id,
            collateral: req.collateral,
            ip: req.ip,
            backup_address: req.backup_address,
            pubkey: req.pubkey,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct JoinSubnetResponse {
    join_txid: bitcoin::Txid,
}

pub async fn join_subnet(
    data: Data<Arc<ServerData>>,
    Params(req): Params<JoinSubnetRequest>,
) -> Result<JoinSubnetResponse, JsonRpcError> {
    let epochs = req.epochs;
    let nonce_range = (
        req.eots_nonce_start.unwrap_or(1),
        req.eots_nonce_count.unwrap_or(DEFAULT_EOTS_NONCE_COUNT),
    );
    let msg: IpcJoinSubnetMsg = req.into();
    info!("joinsubnet: {:?} epochs={:?}", msg, epochs);

    let tip = match epochs {
        Some(_) => {
            let tip = data
                .btc_rpc
                .get_block_count()
                .map_err(|e| RpcError::InternalError(e.to_string()))?;
            check_ue_active(collateral::ue_activation_height(), tip).map_err(|m| {
                error!("joinsubnet (join-ue) rejected: {m}");
                JsonRpcError::from(RpcError::InvalidParams(m))
            })?;
            Some(tip)
        }
        None => None,
    };

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
    if let Some(state) = &subnet_state {
        epochs::next_multisig_members(&*data.db, &genesis_info, state)
            .and_then(|members| Ok(msg.validate_for_members(state, &members)?))
            .map_err(|e| {
                error!("Error validating join msg for subnet info: {}", e);
                RpcError::InvalidParams(e.to_string())
            })?;
    }

    if let (Some(epochs), Some(tip)) = (epochs, tip) {
        let join_txid = join_subnet_ue(&data, &genesis_info, msg, epochs, tip, nonce_range)?;
        return Ok(JoinSubnetResponse { join_txid });
    }

    // TODO this check should be done in the Db
    let multisig_address = &genesis_info.multisig_address();

    let join_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(JoinSubnetResponse { join_txid })
}

/// The join-ue path of `joinsubnet`. The caller has checked the activation switch at `tip`.
fn join_subnet_ue(
    data: &ServerData,
    genesis_info: &db::SubnetGenesisInfo,
    join: IpcJoinSubnetMsg,
    epochs: u32,
    tip: u64,
    nonce_range: (u64, u32),
) -> Result<bitcoin::Txid, JsonRpcError> {
    let invalid = |m: String| -> JsonRpcError {
        error!("joinsubnet (join-ue) rejected: {m}");
        RpcError::InvalidParams(m).into()
    };
    let Some((validator_xpk, validator_sk)) = data.validator else {
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let Some(eots_signer) = &data.eots_signer else {
        return Err(RpcError::InternalError("No EOTS key configured (EOTS_SK_PATH).".to_string()).into());
    };
    let (eots_sk, eots_seed) = {
        let signer = eots_signer.lock().expect("EOTS signer lock");
        (signer.secret_key(), signer.seed())
    };
    if join.pubkey != validator_xpk {
        return Err(invalid(format!(
            "join-ue signs with this provider's validator key {validator_xpk}, not {}",
            join.pubkey
        )));
    }
    let params = &genesis_info.create_subnet_msg;
    if epochs < params.min_timeout || epochs > params.max_timeout {
        return Err(invalid(format!(
            "epochs must be between min_timeout {} and max_timeout {}, got {epochs}",
            params.min_timeout, params.max_timeout
        )));
    }

    collateral::refuse_earlier_join(&*data.db, join.subnet_id, &join.pubkey, None)
        .map_err(|e| invalid(e.to_string()))?;
    let context =
        collateral::join_context(&*data.db, join.subnet_id).map_err(|e| invalid(e.to_string()))?;

    // Before activation the origin is the last processed Bitcoin block
    let origin = match context.h_0 {
        Some(h_0) => h_0,
        None => data
            .db
            .get_last_processed_block()
            .map_err(|e| RpcError::DbError(e))?,
    };
    let timeout =
        u32::try_from(params.cutoff_height(origin, context.epoch + u64::from(epochs) + 3))
            .ok()
            .filter(|t| *t < collateral::LOCKTIME_THRESHOLD)
            .ok_or_else(|| invalid("the timeout is not a block height".to_string()))?;
    context.check_timeout(timeout, tip + 1).map_err(invalid)?;

    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (start, count) = nonce_range;
    if start < 1 || count < 1 {
        return Err(invalid(format!(
            "the EOTS nonce range needs start >= 1 and count >= 1, got start {start} count {count}"
        )));
    }
    let (commitment, _) = eots::commit_nonces(&eots_seed, &join.subnet_id, start, count)
        .map_err(|e| invalid(e.to_string()))?;
    let msg = IpcJoinUeMsg::new_signed(&secp, join, timeout, &validator_sk, &eots_sk, commitment);
    msg.validate().map_err(|e| invalid(e.to_string()))?;
    collateral::refuse_earlier_eots_key(
        &*data.db,
        msg.join.subnet_id,
        &msg.join.pubkey,
        &msg.pk_eots,
        None,
    )
    .map_err(|e| invalid(e.to_string()))?;

    info!(
        "joinsubnet (join-ue): timeout {timeout} for {epochs} epochs from epoch {} (origin {origin}, tip {tip}), EOTS key {} nonces {start}..{}",
        context.epoch,
        msg.pk_eots,
        u128::from(start) + u128::from(count)
    );
    msg.submit_to_bitcoin(&data.btc_rpc, &context)
        .map_err(|e| JsonRpcError::internal(e.to_string()))
}

/// Refuses `join-ue` and `unstakeue` while `UE_ACTIVATION_HEIGHT` is unset or above the tip.
fn check_ue_active(activation: Option<u64>, tip: u64) -> Result<(), String> {
    match activation {
        Some(activation) if activation <= tip => Ok(()),
        Some(activation) => Err(format!(
            "unilateral exit is active from block {activation}, the tip is {tip}"
        )),
        None => Err(format!(
            "unilateral exit is off: {} is not set",
            collateral::UE_ACTIVATION_HEIGHT_VAR
        )),
    }
}

struct CheckpointPlan {
    signer: ipc_lib::CheckpointSigner,
    change_address: bitcoin::Address,
    unspent: Vec<bitcoincore_rpc::json::ListUnspentResultEntry>,
    rotation: bool,
    /// Spends every fund UTXO, as the change goes to another multisig
    sweep: bool,
}

impl CheckpointPlan {
    /// The plan from the recorded state of the subnet, without the unspent outputs.
    fn of_state(
        genesis_info: &db::SubnetGenesisInfo,
        subnet: &db::SubnetState,
    ) -> Result<Self, JsonRpcError> {
        let signer = signing_configuration(genesis_info, subnet)?;
        let rotation = subnet.last_rotation_epoch < subnet.epoch;
        let change_address = if rotation {
            subnet.latest_committee().address_checked()
        } else {
            signer.address.clone()
        };
        let sweep = change_address != signer.address;
        Ok(Self {
            signer,
            change_address,
            unspent: Vec::new(),
            rotation,
            sweep,
        })
    }
}

/// `C_wlist` until the rotation checkpoint of epoch 1, then the current configuration.
fn signing_configuration(
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
) -> Result<ipc_lib::CheckpointSigner, JsonRpcError> {
    if subnet.last_rotation_epoch == 0 {
        ipc_lib::CheckpointSigner::of_configuration(
            &subnet.id,
            &genesis_info.whitelist_configuration(),
        )
        .map_err(|e| RpcError::InternalError(e.to_string()).into())
    } else {
        Ok(ipc_lib::CheckpointSigner::of_committee(&subnet.committee))
    }
}

fn check_checkpoint_signer(
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
    validator: &bitcoin::XOnlyPublicKey,
) -> Result<(), JsonRpcError> {
    let signs = signing_configuration(genesis_info, subnet)?
        .keys
        .iter()
        .any(|(key, _)| key == validator);
    if !signs {
        error!("Configured validator isn't a validator in the specified subnet.");
        return Err(RpcError::InvalidParams(
            "Configured validator isn't a validator in the specified subnet.".to_string(),
        )
        .into());
    }
    Ok(())
}

fn plan_checkpoint(
    data: &ServerData,
    genesis_info: &db::SubnetGenesisInfo,
    subnet: &db::SubnetState,
    msg: &mut IpcCheckpointSubnetMsg,
) -> Result<CheckpointPlan, JsonRpcError> {
    let mut plan = CheckpointPlan::of_state(genesis_info, subnet)?;
    msg.is_rotation_checkpoint = plan.rotation;
    plan.unspent = wallet::get_unspent_for_address(&data.btc_watchonly_rpc, &plan.signer.address)
        .map_err(|e| JsonRpcError::from(RpcError::InternalError(e.to_string())))?;
    Ok(plan)
}

/// The members of `committee` that joined with `join-ue`.
fn ue_members(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    committee: &db::SubnetCommittee,
) -> Result<std::collections::HashSet<bitcoin::XOnlyPublicKey>, RpcError> {
    let mut members = std::collections::HashSet::new();
    for v in &committee.validators {
        if epochs::is_ue_validator(db, subnet_id, v.join_txid, &v.pubkey)? {
            members.insert(v.pubkey);
        }
    }
    Ok(members)
}

/// The collateral of every member but the UE ones.
fn kill_unstakes(
    committee: &db::SubnetCommittee,
    ue_members: &std::collections::HashSet<bitcoin::XOnlyPublicKey>,
) -> Vec<ipc_lib::IpcUnstake> {
    committee
        .validators
        .iter()
        .filter(|v| !ue_members.contains(&v.pubkey))
        .map(|v| ipc_lib::IpcUnstake {
            amount: v.collateral,
            address: v.backup_address.clone(),
            pubkey: v.pubkey,
        })
        .collect()
}

/// One unstake output per Withdraw stake change of a non-UE member of `committee`.
fn rotation_unstakes(
    stake_changes: &[db::StakeChangeRequest],
    committee: &db::SubnetCommittee,
    ue_members: &std::collections::HashSet<bitcoin::XOnlyPublicKey>,
) -> Vec<ipc_lib::IpcUnstake> {
    stake_changes
        .iter()
        .filter_map(|sc| {
            let StakingChange::Withdraw { amount } = sc.change else {
                return None;
            };
            let Some(v) = committee
                .validators
                .iter()
                .find(|v| v.pubkey == sc.validator_xpk)
            else {
                error!(
                    "Validator {} not found in committee for a stake change, skipping",
                    sc.validator_xpk
                );
                return None;
            };
            if ue_members.contains(&v.pubkey) {
                info!(
                    "Validator {} is a UE validator, no unstake output for its stake change",
                    sc.validator_xpk
                );
                return None;
            }
            Some(ipc_lib::IpcUnstake {
                amount,
                address: v.backup_address.clone(),
                pubkey: sc.validator_xpk,
            })
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
pub struct GetGenesisInfoParams {
    pub subnet_id: SubnetId,
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
    pub subnet_id: SubnetId,
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
pub struct GetSubnetBalanceResponse {
    /// Address of the subnet's current committee multisig. Useful for callers
    /// that want to top up (refund) the multisig.
    pub multisig_address: String,
    pub balance_sats: u64,
    pub utxo_count: usize,
}

/// Returns the on-chain balance of the subnet — i.e. the sats held by the
/// current committee multisig, summed across all unspent outputs the watch-only
/// wallet tracks for it.
pub async fn get_subnet_balance(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<GetSubnetBalanceResponse, JsonRpcError> {
    info!("getsubnetbalance: {}", params.subnet_id);

    let subnet_state = data
        .db
        .get_subnet_state(params.subnet_id)
        .map_err(|e| {
            error!("Error getting subnet state from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", params.subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", params.subnet_id))
        })?;

    let multisig_address = subnet_state.multisig_address();

    let unspent = wallet::get_unspent_for_address(&data.btc_watchonly_rpc, &multisig_address)
        .map_err(|e| {
            error!("Error listing unspent for multisig {}: {}", multisig_address, e);
            RpcError::InternalError(e.to_string())
        })?;

    let balance_sats: u64 = unspent.iter().map(|u| u.amount.to_sat()).sum();

    Ok(GetSubnetBalanceResponse {
        multisig_address: multisig_address.to_string(),
        balance_sats,
        utxo_count: unspent.len(),
    })
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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;
    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| RpcError::InvalidParams(e.to_string()))?;

    let multisig_address = if subnet_state.is_activated() {
        subnet_state.latest_committee().address_checked()
    } else {
        genesis_info.multisig_address()
    };

    let fund_txid = msg
        .submit_to_bitcoin(&data.btc_rpc, &multisig_address)
        .map_err(|e| JsonRpcError::internal(e.to_string()))?;

    Ok(FundSubnetResponse { fund_txid })
}

#[derive(Serialize, Deserialize)]
pub struct GetRootnetMessagesParams {
    pub subnet_id: SubnetId,
    pub block_height: u64,
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
pub struct GetTokenBalanceParams {
    pub home_subnet_id: SubnetId,
    pub home_token_address: alloy_primitives::Address,
    pub subnet_id: SubnetId,
}

#[derive(Serialize, Deserialize)]
pub struct GetTokenBalanceResponse {
    /// Balance as a decimal string (U256 may exceed u64).
    pub balance: String,
}

pub async fn get_token_balance(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetTokenBalanceParams>,
) -> Result<GetTokenBalanceResponse, JsonRpcError> {
    info!(
        "gettokenbalance: home_subnet={} token={} subnet={}",
        params.home_subnet_id, params.home_token_address, params.subnet_id
    );

    let balance = data
        .db
        .get_token_balance(
            params.home_subnet_id,
            params.home_token_address,
            params.subnet_id,
        )
        .map_err(|e| {
            error!("Error getting token balance from Db: {}", e);
            RpcError::DbError(e)
        })?;

    Ok(GetTokenBalanceResponse {
        balance: balance.to_string(),
    })
}

#[derive(Serialize, Deserialize)]
pub struct GetSubnetCheckpointParams {
    subnet_id: SubnetId,
    number: Option<u64>,
    /// With it, the latest checkpoint in a Bitcoin block at or below this height
    #[serde(default)]
    max_block_height: Option<u64>,
}

pub async fn get_subnet_checkpoint(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetCheckpointParams>,
) -> Result<Option<db::SubnetCheckpoint>, JsonRpcError> {
    info!(
        "getsubnetcheckpoint: {} number={:?} max_block_height={:?}",
        params.subnet_id, params.number, params.max_block_height
    );
    Ok(latest_checkpoint_at(
        &data.db,
        params.subnet_id,
        params.number,
        params.max_block_height,
    )?)
}

/// The checkpoint `number` of the subnet, the last one without it; with `max_block_height`,
/// the latest checkpoint in a Bitcoin block at or below that height. A height the monitor has
/// not processed is refused with `RpcError::MonitorBehind`.
pub fn latest_checkpoint_at(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    number: Option<u64>,
    max_block_height: Option<u64>,
) -> Result<Option<db::SubnetCheckpoint>, RpcError> {
    if let Some(max_block_height) = max_block_height {
        let processed = db.get_last_processed_block().map_err(|e| {
            error!("Error getting last processed block from Db: {}", e);
            RpcError::DbError(e)
        })?;
        if processed < max_block_height {
            warn!(
                "getsubnetcheckpoint: {} asks for Bitcoin height {max_block_height}, monitor at {processed}",
                subnet_id
            );
            return Err(RpcError::MonitorBehind {
                requested: max_block_height,
                processed,
            });
        }
    }

    // Check if subnet exists
    let subnet = db
        .get_subnet_state(subnet_id)
        .map_err(|e| {
            error!("Error getting subnet info from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            error!("Subnet {} not found.", subnet_id);
            RpcError::InvalidParams(format!("Subnet {} not found.", subnet_id))
        })?;

    let last_checkpoint_number = match subnet.last_checkpoint_number {
        Some(number) => number,
        None => {
            return Ok(None);
        }
    };

    // Default to the last checkpoint
    let number = number.unwrap_or(last_checkpoint_number);
    let get = |number: u64| {
        db.get_checkpoint(subnet_id, number).map_err(|e| {
            error!("Error getting checkpoint from Db: {}", e);
            RpcError::DbError(e)
        })
    };
    let Some(max_block_height) = max_block_height else {
        return get(number);
    };
    // Walk back to the first checkpoint in range
    let mut number = number;
    loop {
        match get(number)? {
            Some(checkpoint) if checkpoint.block_height <= max_block_height => {
                return Ok(Some(checkpoint))
            }
            _ if number == 0 => return Ok(None),
            _ => number -= 1,
        }
    }
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
        0,
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
    Params(mut params): Params<serde_json::Value>,
) -> Result<GenCheckpointPsbtResponse, JsonRpcError> {
    // The slash pre-signatures that the checkpoint carries, as posted on the subnet
    let carried = params
        .get("slash_pre_signature_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let posted: Vec<crate::slash::PostedSlashSignatures> = match params
        .as_object_mut()
        .and_then(|params| params.remove("slash_pre_signatures"))
    {
        Some(value) => serde_json::from_value(value)
            .map_err(|e| RpcError::InvalidParams(format!("slash_pre_signatures: {e}")))?,
        None if carried > 0 => {
            return Err(RpcError::InvalidParams(format!(
                "slash_pre_signatures missing, the checkpoint carries {carried}"
            ))
            .into())
        }
        None => vec![],
    };
    let mut msg: IpcCheckpointSubnetMsg =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
    info!(
        "gen_checkpoint_psbt: {:?} slash pre-signatures of {:?}",
        msg, posted
    );

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
    msg.slash_signatures = carried_slash_entries(&*data.db, msg.subnet_id, &posted);
    fit_slash_entries(&mut msg).map_err(|e| RpcError::InternalError(e.to_string()))?;

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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;
    check_checkpoint_signer(&genesis_info, &subnet, &validator_xonly_pubkey)?;

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

    let should_kill_subnet = subnet.should_kill();

    // Fill in the subnet addresses, erroring out if any subnet is not found
    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let plan = plan_checkpoint(&data, &genesis_info, &subnet, &mut msg)?;
    let unspent = &plan.unspent;
    let mut exhaust_unspent = plan.sweep;

    let ue_members = ue_members(&data.db, msg.subnet_id, &subnet.committee)?;

    if should_kill_subnet {
        // Mark the checkpoint as a kill checkpoint
        msg.is_kill_checkpoint = true;
        exhaust_unspent = true;

        // Calculation of total unspent and stake amounts, alongside with transfers and withdrawals to process

        let total_unspent_amount = unspent.iter().map(|u| u.amount).sum::<bitcoin::Amount>();
        let total_stake_amount = subnet
            .committee
            .validators
            .iter()
            .filter(|v| !ue_members.contains(&v.pubkey))
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
        msg.unstakes = kill_unstakes(&subnet.committee, &ue_members);

        info!(
            "Killing subnet {}, processing {} unstakes.",
            msg.subnet_id,
            msg.unstakes.len()
        );
    } else if plan.rotation && subnet.is_activated() {
        let unconfirmed_stake_changes = data
            .db
            .get_unconfirmed_stake_changes(msg.subnet_id)
            .map_err(|e| {
                error!("Error getting unconfirmed stake changes from Db: {}", e);
                RpcError::DbError(e)
            })?;
        msg.unstakes =
            rotation_unstakes(&unconfirmed_stake_changes, &subnet.committee, &ue_members);

        info!(
            "Rotation checkpoint of epoch {} to multisig address {}",
            subnet.epoch, plan.change_address
        );
        info!("Processing {} unstakes.", msg.unstakes.len());
    }

    validate_erc_batch_data(
        msg.subnet_id,
        &msg.token_registrations,
        &msg.token_supply_adjustments,
        &msg.token_transfers,
        &*data.db,
    )
    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;

    let unsigned_psbt = msg
        .build_checkpoint_psbt(
            &plan.signer,
            &plan.change_address,
            exhaust_unspent,
            fee_rate,
            unspent,
        )
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );

            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(checkpoint_txid, fee_rate, &plan.signer.address)
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

/// Identical to `gen_checkpoint_psbt` but skips the token-transfer balance
/// check.  Only available with the `dev` feature.  Used by the monitor tester
/// to inject malicious checkpoints that exceed subnet balances so we can verify
/// the monitor enforces the firewall on its side.
#[cfg(feature = "dev")]
pub async fn dev_gen_checkpoint_psbt(
    data: Data<Arc<ServerData>>,
    Params(mut msg): Params<IpcCheckpointSubnetMsg>,
) -> Result<GenCheckpointPsbtResponse, JsonRpcError> {
    info!("dev_gen_checkpoint_psbt (no balance check): {:?}", msg);

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

    let genesis_info = data
        .db
        .get_subnet_genesis_info(msg.subnet_id)
        .map_err(RpcError::DbError)?
        .ok_or(RpcError::InvalidParams(format!(
            "Subnet {} not found.",
            msg.subnet_id
        )))?;
    check_checkpoint_signer(&genesis_info, &subnet, &validator_xonly_pubkey)?;

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

    msg.update_subnets_for_transfer(&*data.db).map_err(|e| {
        error!("Error updating subnets for transfer: {}", e);
        RpcError::InvalidParams(e.to_string())
    })?;

    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_watchonly_rpc, None, None);

    let plan = plan_checkpoint(&data, &genesis_info, &subnet, &mut msg)?;

    // balance check intentionally omitted

    let unsigned_psbt = msg
        .build_checkpoint_psbt(
            &plan.signer,
            &plan.change_address,
            plan.sweep,
            fee_rate,
            &plan.unspent,
        )
        .map_err(|e| {
            error!(
                "Error generating checkpoint psbt for subnet_id={}: {}",
                &msg.subnet_id, e
            );
            RpcError::InternalError(e.to_string())
        })?;

    let checkpoint_txid = unsigned_psbt.unsigned_tx.compute_txid();

    let batch_transfer_tx = msg
        .make_reveal_batch_transfer_tx(checkpoint_txid, fee_rate, &plan.signer.address)
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
    pub unsigned_psbt_base64: String,
    pub secret_keys: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevMultisignPsbtResponse {
    // Include signatures for each input, mapped by pubkey
    pub signatures: Vec<(
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
    /// Each signer's signatures, one per input, in hex, as posted to the subnet.
    signatures: Vec<Vec<String>>,
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

    // A malformed signature counts as missing for its input
    let signatures: Vec<Vec<_>> = params
        .signatures
        .iter()
        .map(|sigs| sigs.iter().map(|sig| sig.parse().ok()).collect())
        .collect();

    // Each signer's key is the one of the configuration that signs the checkpoint, also after a
    // configuration rotation, for which its signatures verify
    let signers =
        multisig::signers_of_spend_psbt(&secp, &unsigned_psbt, &signatures).map_err(|e| {
            error!("Error resolving the signers of the PSBT: {}", e);
            RpcError::InternalError(e.to_string())
        })?;

    // Nothing is posted while the valid signatures of an input stay below its threshold
    let finalized_tx = multisig::finalize_spend_psbt_from_sigs(&secp, &unsigned_psbt, &signers)
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

    collateral::refuse_ue_member(&*data.db, msg.subnet_id, &msg.pubkey).map_err(|e| {
        error!("{e}");
        RpcError::InvalidParams(e.to_string())
    })?;
    msg.validate_for_subnet(&subnet_state).map_err(|e| {
        error!(
            "Error validating stake collateral msg for subnet info: {}",
            e
        );
        RpcError::InvalidParams(e.to_string())
    })?;
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
    epochs::next_multisig_members(&*data.db, &genesis_info, &subnet_state)
        .and_then(|members| Ok(msg.validate_for_members(&members)?))
        .map_err(|e| {
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

    collateral::refuse_ue_member(&*data.db, msg.subnet_id, &validator_xpk).map_err(|e| {
        error!("{e}");
        RpcError::InvalidParams(e.to_string())
    })?;
    msg.validate_for_subnet(&genesis_info, &subnet_state)
        .map_err(|e| {
            error!(
                "Error validating unstake collateral msg for subnet info: {}",
                e
            );
            RpcError::InvalidParams(e.to_string())
        })?;
    epochs::next_multisig_members(&*data.db, &genesis_info, &subnet_state)
        .and_then(|members| msg.validate_for_members(&genesis_info, &members))
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

#[derive(Serialize, Deserialize)]
pub struct UnstakeUeResponse {
    txid: bitcoin::Txid,
    /// The collateral spent, before the fee
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    amount: bitcoin::Amount,
    /// The address paid
    address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
}

/// Spends this validator's matured join-ue collaterals paying the backup address of the latest
/// matured record in one transaction.
pub async fn unstake_ue(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<UnstakeUeResponse, JsonRpcError> {
    info!("unstakeue: {}", params.subnet_id);

    let Some((validator_xpk, validator_sk)) = data.validator else {
        error!("No validator keypair configured.");
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let invalid = |m: String| -> JsonRpcError {
        error!("unstakeue rejected: {m}");
        RpcError::InvalidParams(m).into()
    };
    let internal = |e: &dyn std::fmt::Display| -> JsonRpcError {
        error!("unstakeue failed: {e}");
        RpcError::InternalError(e.to_string()).into()
    };

    let records = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .filter(|r| r.pubkey == validator_xpk && !r.spent)
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Err(invalid(format!(
            "Validator {validator_xpk} has no unspent join-ue collateral in subnet {}",
            params.subnet_id
        )));
    }

    let tip = data.btc_rpc.get_block_count().map_err(|e| internal(&e))?;
    check_ue_active(collateral::ue_activation_height(), tip).map_err(invalid)?;
    // The matured records paying the backup address of the latest matured one
    let Some(latest) = records
        .iter()
        .filter(|r| u64::from(r.timeout) <= tip)
        .max_by_key(|r| r.timeout)
        .cloned()
    else {
        let earliest = records.iter().map(|r| r.timeout).min().expect("non-empty");
        return Err(invalid(format!(
            "No matured join-ue collateral: the earliest matures at block {earliest}, the tip is {tip}"
        )));
    };
    let matured = records
        .iter()
        .filter(|r| u64::from(r.timeout) <= tip && r.backup_address == latest.backup_address)
        .collect::<Vec<_>>();

    let to = latest
        .backup_address
        .clone()
        .require_network(NETWORK)
        .map_err(|e| invalid(format!("backup address: {e}")))?;
    // A standard transaction weighs at most 400,000 weight units and an exit input about 305
    const MAX_EXIT_INPUTS: usize = 1000;
    if matured.len() > MAX_EXIT_INPUTS {
        info!(
            "unstakeue: {} matured outputs, spending {MAX_EXIT_INPUTS} now and the rest on the next call",
            matured.len()
        );
    }
    let inputs = matured
        .iter()
        .take(MAX_EXIT_INPUTS)
        .map(|r| {
            Ok(collateral::UeExitInput {
                outpoint: r.outpoint,
                value: r.value,
                script: collateral::collateral_script_of(&*data.db, r)?,
            })
        })
        .collect::<Result<Vec<_>, IpcLibError>>()
        .map_err(|e| internal(&e))?;
    let amount = inputs.iter().map(|i| i.value).sum::<bitcoin::Amount>();
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let keypair = validator_sk.keypair(&secp);
    let fee_rate = bitcoin_utils::get_fee_rate(&data.btc_rpc, None, None);
    let tx = collateral::build_ue_exit_tx(&secp, &inputs, &keypair, &to, fee_rate)
        .map_err(|e| internal(&e))?;
    let txid = data
        .btc_rpc
        .send_raw_transaction(&tx)
        .map_err(|e| internal(&e))?;
    info!(
        "unstakeue: spent {} collateral outputs ({amount}) of validator {validator_xpk} in subnet {} to {to}, txid={txid}",
        inputs.len(),
        params.subnet_id
    );

    Ok(UnstakeUeResponse {
        txid,
        amount,
        address: latest.backup_address,
    })
}

// Stake changes

#[derive(Serialize, Deserialize)]
pub struct GetStakeChangesParams {
    pub subnet_id: SubnetId,
    pub block_height: u64,
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

// Stake changes

#[derive(Serialize, Deserialize)]
pub struct GetKillRequestsParams {
    pub subnet_id: SubnetId,
}

pub async fn get_kill_requests(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetKillRequestsParams>,
) -> Result<Vec<db::KillRequest>, JsonRpcError> {
    info!("getkillrequests: {}", params.subnet_id);

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

    Ok(valid_kill_requests)
}

#[derive(Serialize, Deserialize)]
#[cfg(feature = "dev")]
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

#[cfg(feature = "emission_chain")]
pub async fn get_rewarded_collaterals(
    data: Data<Arc<ServerData>>,
    Params(params): Params<crate::rewards::GetRewardedCollateralsParams>,
) -> Result<crate::rewards::GetRewardedCollateralsResponse, JsonRpcError> {
    let reward_config = RewardConfig::new_from_env()
        .unwrap_or_else(|e| panic!("Failed to load reward config: {}", e));

    let (start_height, end_height) =
        reward_config
            .snapshot_boundaries(params.snapshot)
            .map_err(|e| {
                RpcError::InternalError(format!(
                "Error getting boundaries for snapshot {}. Is the server configured correctly?: {}",
                params.snapshot,
                e.to_string()
            ))
            })?;

    let last_processed_block = data.db.get_last_processed_block().map_err(|e| {
        error!("Error getting last processed block from Db: {}", e);
        RpcError::DbError(e)
    })?;

    info!(
        "get_validator_rewards snapshot={} start_height={} end_height={}. Last proccessed Bitcoin block={}",
        params.snapshot, start_height, end_height, last_processed_block
    );

    let result = data
        .db
        .get_snapshot_result(params.snapshot)
        .map_err(|e| {
            error!("Error getting reward result from Db: {}", e);
            RpcError::DbError(e)
        })?
        .ok_or_else(|| {
            info!("No cached rewards found for snapshot {}. Last processed block: {last_processed_block}", params.snapshot);
            RpcError::InvalidParams(format!(
                "No cached rewards found for snapshot {}",
                params.snapshot
            ))
        })?;

    Ok(crate::rewards::GetRewardedCollateralsResponse {
        collaterals: result
            .rewards_list
            .into_iter()
            .map(|(pk, amt)| (crate::eth_utils::eth_addr_from_x_only_pubkey(pk), amt))
            .collect(),
        total_rewarded_collateral: result.total_rewarded_collateral,
    })
}

/// A validator's EOTS key
#[derive(Serialize, Deserialize, Debug)]
pub struct EotsKeyEntry {
    pub pubkey: bitcoin::XOnlyPublicKey,
    pub pk_eots: bitcoin::XOnlyPublicKey,
}

/// `geteotskeys`: the EOTS key of every validator that joined with a collateral output.
pub async fn get_eots_keys(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<Vec<EotsKeyEntry>, JsonRpcError> {
    let mut keys = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .map(|r| EotsKeyEntry {
            pubkey: r.pubkey,
            pk_eots: r.pk_eots,
        })
        .collect::<Vec<_>>();
    keys.sort_by_key(|k| k.pubkey);
    keys.dedup_by_key(|k| k.pubkey);
    Ok(keys)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetEotsCommitmentsParams {
    pub subnet_id: SubnetId,
    pub pk_eots: bitcoin::XOnlyPublicKey,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EotsCommitmentEntry {
    pub start: u64,
    pub count: u32,
    pub root: String,
    /// The Bitcoin block that holds the transaction that carried it
    pub block_height: u64,
}

/// `geteotscommitments`: every EOTS nonce commitment the monitor recorded for an EOTS key.
pub async fn get_eots_commitments(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetEotsCommitmentsParams>,
) -> Result<Vec<EotsCommitmentEntry>, JsonRpcError> {
    Ok(data
        .db
        .get_eots_commitments(params.subnet_id, &params.pk_eots)
        .map_err(RpcError::DbError)?
        .iter()
        .map(EotsCommitmentEntry::from)
        .collect())
}

impl From<&db::EotsCommitmentRecord> for EotsCommitmentEntry {
    fn from(r: &db::EotsCommitmentRecord) -> Self {
        Self {
            start: r.commitment.start,
            count: r.commitment.count,
            root: hex::encode(r.commitment.root),
            block_height: r.block_height,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetEotsRecordsParams {
    pub subnet_id: SubnetId,
    /// The Bitcoin height up to which the caller holds the records; without it, none
    #[serde(default)]
    pub after_block_height: Option<u64>,
    /// With it, the answer at this Bitcoin height
    #[serde(default)]
    pub max_block_height: Option<u64>,
}

/// An EOTS key with its commitments recorded in the asked range of Bitcoin heights
#[derive(Serialize, Deserialize, Debug)]
pub struct EotsKeyRecords {
    pub pubkey: bitcoin::XOnlyPublicKey,
    pub pk_eots: bitcoin::XOnlyPublicKey,
    pub commitments: Vec<EotsCommitmentEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GetEotsRecordsResponse {
    /// The Bitcoin height of the answer
    pub block_height: u64,
    pub eots_keys: Vec<EotsKeyRecords>,
    pub checkpoint: Option<db::SubnetCheckpoint>,
}

/// `geteotsrecords`: from one state of the monitor, the EOTS keys and EOTS nonce commitments
/// recorded in Bitcoin blocks above `after_block_height`, and the latest checkpoint, at
/// `max_block_height` or at the last processed Bitcoin height.
pub async fn get_eots_records(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetEotsRecordsParams>,
) -> Result<GetEotsRecordsResponse, JsonRpcError> {
    debug!(
        "geteotsrecords: {} after_block_height={:?} max_block_height={:?}",
        params.subnet_id, params.after_block_height, params.max_block_height
    );
    Ok(eots_records_at(
        &data.db,
        params.subnet_id,
        params.after_block_height,
        params.max_block_height,
    )?)
}

/// The answer of `geteotsrecords`. A Bitcoin height the monitor has not processed is refused
/// with `RpcError::MonitorBehind`, also before the monitor holds a state of the subnet.
pub fn eots_records_at(
    db: &db::HeedDb,
    subnet_id: SubnetId,
    after_block_height: Option<u64>,
    max_block_height: Option<u64>,
) -> Result<GetEotsRecordsResponse, RpcError> {
    let (processed, records) = db
        .get_eots_records(subnet_id, after_block_height, max_block_height)
        .map_err(|e| {
            error!("Error getting EOTS records from Db: {}", e);
            RpcError::DbError(e)
        })?;
    if let Some(requested) = max_block_height.filter(|h| *h > processed) {
        warn!(
            "geteotsrecords: {} asks for Bitcoin height {requested}, monitor at {processed}",
            subnet_id
        );
        return Err(RpcError::MonitorBehind {
            requested,
            processed,
        });
    }
    let records = records
        .ok_or_else(|| RpcError::InvalidParams(format!("Subnet {} not found.", subnet_id)))?;
    Ok(GetEotsRecordsResponse {
        block_height: records.block_height,
        eots_keys: records
            .keys
            .iter()
            .map(|(pubkey, pk_eots, commitments)| EotsKeyRecords {
                pubkey: *pubkey,
                pk_eots: *pk_eots,
                commitments: commitments.iter().map(EotsCommitmentEntry::from).collect(),
            })
            .collect(),
        checkpoint: records.checkpoint,
    })
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignEotsVoteParams {
    pub subnet_id: SubnetId,
    pub height: u64,
    /// The subnet block hash, hex
    pub block_hash: String,
}

/// An EOTS vote with its proof, hex-encoded
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EotsVoteJson {
    pub pk_eots: String,
    pub height: u64,
    pub block_hash: String,
    pub signature: String,
    pub start: u64,
    pub proof: Vec<String>,
}

impl From<&eots::EotsVote> for EotsVoteJson {
    fn from(vote: &eots::EotsVote) -> Self {
        Self {
            pk_eots: hex::encode(vote.pk_eots),
            height: vote.height,
            block_hash: hex::encode(vote.block_hash),
            signature: hex::encode(eots::signature_to_bytes(&vote.signature)),
            start: vote.start,
            proof: vote.proof.iter().map(hex::encode).collect(),
        }
    }
}

fn hex32(what: &str, value: &str) -> Result<[u8; 32], JsonRpcError> {
    hex::decode(value)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| RpcError::InvalidParams(format!("{what} must be 32 bytes in hex")).into())
}

fn refuse_checkpointed_height(
    latest: Option<&db::SubnetCheckpoint>,
    height: u64,
) -> Result<(), String> {
    match latest {
        Some(checkpoint) if height <= checkpoint.checkpoint_height => Err(format!(
            "height {height} is at or below the latest checkpointed height {}",
            checkpoint.checkpoint_height
        )),
        _ => Ok(()),
    }
}

/// `signeotsvote`: this validator's EOTS vote for a subnet block, or the vote it already signed
/// at that height. A different subnet block at a signed height is refused.
pub async fn sign_eots_vote(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SignEotsVoteParams>,
) -> Result<EotsVoteJson, JsonRpcError> {
    let Some(signer) = &data.eots_signer else {
        return Err(RpcError::InternalError("No EOTS key configured (EOTS_SK_PATH).".to_string()).into());
    };
    let block_hash = hex32("block_hash", &params.block_hash)?;
    let mut signer = signer.lock().expect("EOTS signer lock");
    let already_signed = signer
        .signed(params.subnet_id, params.height)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;
    if already_signed.is_none() {
        let latest = latest_checkpoint_at(&data.db, params.subnet_id, None, None)?;
        refuse_checkpointed_height(latest.as_ref(), params.height).map_err(|e| {
            error!("signeotsvote refused at height {}: {e}", params.height);
            JsonRpcError::from(RpcError::InvalidParams(e))
        })?;
        if let Some(checkpoint) = latest {
            signer
                .delete_up_to(params.subnet_id, checkpoint.checkpoint_height)
                .map_err(|e| RpcError::InternalError(e.to_string()))?;
        }
    }
    let commitments = data
        .db
        .get_eots_commitments(params.subnet_id, &signer.pk_eots())
        .map_err(RpcError::DbError)?
        .into_iter()
        .map(|r| r.commitment)
        .collect::<Vec<_>>();
    let vote = signer
        .sign(params.subnet_id, &commitments, params.height, &block_hash)
        .map_err(|e| {
            error!("signeotsvote refused at height {}: {e}", params.height);
            JsonRpcError::from(RpcError::InvalidParams(e.to_string()))
        })?;
    Ok(EotsVoteJson::from(&vote))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignSlashParams {
    pub subnet_id: SubnetId,
    pub txid: bitcoin::Txid,
    pub vout: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SignSlashResponse {
    /// This validator's index in the configuration the output names
    pub index: u16,
    /// The pre-signature, hex
    pub signature: String,
}

/// `signslash`: this validator's pre-signature of the slash transaction of a collateral.
pub async fn sign_slash(
    data: Data<Arc<ServerData>>,
    Params(params): Params<SignSlashParams>,
) -> Result<SignSlashResponse, JsonRpcError> {
    let Some((_, validator_sk)) = data.validator else {
        return Err(RpcError::InternalError("No validator keypair configured.".to_string()).into());
    };
    let outpoint = bitcoin::OutPoint::new(params.txid, params.vout);
    let record = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?
        .into_iter()
        .find(|r| r.outpoint == outpoint)
        .ok_or_else(|| {
            RpcError::InvalidParams(format!(
                "{outpoint} is not a collateral of subnet {}",
                params.subnet_id
            ))
        })?;
    let script = collateral::collateral_script_of(&*data.db, &record)
        .map_err(|e| RpcError::InternalError(e.to_string()))?;
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (index, signature) = crate::slash::sign_slash(
        &secp,
        &script,
        outpoint,
        record.value,
        &validator_sk.keypair(&secp),
    )
    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
    Ok(SignSlashResponse {
        index,
        signature: hex::encode(eots::signature_to_bytes(&signature)),
    })
}

/// A collateral output that is not slashable yet, with the configuration its script names.
#[derive(Serialize, Deserialize, Debug)]
pub struct UnslashableOutput {
    pub txid: bitcoin::Txid,
    pub vout: u32,
    pub pubkey: bitcoin::XOnlyPublicKey,
    /// The join (reveal) transaction of the collateral
    pub join_txid: bitcoin::Txid,
    /// The Bitcoin block of the join
    pub block_height: u64,
    pub committee_number: u64,
    pub threshold: multisig::Power,
    /// The keys of the named configuration in ascending order, with their powers
    pub keys: Vec<(bitcoin::XOnlyPublicKey, multisig::Power)>,
    /// Whether this provider's validator key is one of them
    pub signer: bool,
}

/// `getunslashableoutputs`: the unspent collateral outputs of a subnet that the monitor has
/// not recorded as slashable, oldest first.
pub async fn get_unslashable_outputs(
    data: Data<Arc<ServerData>>,
    Params(params): Params<GetSubnetParams>,
) -> Result<Vec<UnslashableOutput>, JsonRpcError> {
    let validator = data.validator.map(|(k, _)| k);
    let mut outputs = Vec::new();
    let records = data
        .db
        .get_ue_collaterals(params.subnet_id)
        .map_err(RpcError::DbError)?;
    for record in records {
        if record.spent
            || data
                .db
                .get_slash_presigs(params.subnet_id, &record.outpoint)
                .map_err(RpcError::DbError)?
                .is_some()
        {
            continue;
        }
        let block_height =
            collateral::join_block_height(&*data.db, &record).map_err(RpcError::DbError)?;
        let configuration = collateral::configuration_by_number(
            &*data.db,
            params.subnet_id,
            record.committee_number,
        )
        .map_err(|e| RpcError::InternalError(e.to_string()))?
        .ok_or_else(|| {
            RpcError::InternalError(format!("No committee {}", record.committee_number))
        })?;
        let keys = configuration.sorted_keys();
        outputs.push(UnslashableOutput {
            txid: record.outpoint.txid,
            vout: record.outpoint.vout,
            pubkey: record.pubkey,
            join_txid: record.join_txid,
            block_height,
            committee_number: record.committee_number,
            threshold: configuration.threshold,
            signer: validator.is_some_and(|v| keys.iter().any(|(k, _)| *k == v)),
            keys,
        });
    }
    outputs.sort_by_key(|o| (o.block_height, o.txid, o.vout));
    Ok(outputs)
}

/// The posted slash pre-signature entries a checkpoint carries: invalid signatures are dropped,
/// an entry is kept only when the valid ones reach the threshold, with the valid ones in ascending
/// order of index until their powers reach it.
pub(crate) fn carried_slash_entries<D: DatabaseCore>(
    db: &D,
    subnet_id: SubnetId,
    posted: &[crate::slash::PostedSlashSignatures],
) -> Vec<crate::slash::SlashSignatureEntry> {
    let mut entries = Vec::new();
    for entry in posted {
        let (checked, unresolved) =
            match ipc_lib::check_posted_slash_signatures(db, subnet_id, entry) {
                Ok(checked) => checked,
                Err(e) => {
                    warn!("Slash pre-signatures of {} left out: {e}", entry.outpoint());
                    continue;
                }
            };
        if unresolved > 0 {
            warn!(
                "{unresolved} slash pre-signatures of {} verify for no key of its configuration without an earlier signature, left out",
                entry.outpoint()
            );
        }
        if !checked.reaches_threshold() {
            continue;
        }
        let join_height = match collateral::join_block_height(db, &checked.record) {
            Ok(height) => height,
            Err(e) => {
                warn!("Slash pre-signatures of {} left out: {e}", entry.outpoint());
                continue;
            }
        };
        entries.push((
            join_height,
            crate::slash::SlashSignatureEntry::new(
                entry.outpoint(),
                checked.signatures_to_threshold(),
            ),
        ));
    }
    entries.sort_by_key(|(height, e)| (*height, e.txid, e.vout));
    entries.into_iter().map(|(_, e)| e).collect()
}

/// Keeps the longest prefix of the slash pre-signature entries whose batch transfer reveal is
/// a standard transaction, by binary search.
fn fit_slash_entries(msg: &mut IpcCheckpointSubnetMsg) -> Result<(), IpcLibError> {
    let max = bitcoin::Weight::from_wu(u64::from(bitcoin::policy::MAX_STANDARD_TX_WEIGHT));
    let entries = std::mem::take(&mut msg.slash_signatures);
    let (mut fits, mut high) = (0, entries.len());
    while fits < high {
        let mid = (fits + high).div_ceil(2);
        msg.slash_signatures = entries[..mid].to_vec();
        if msg.batch_transfer_reveal_weight()? <= max {
            fits = mid;
        } else {
            high = mid - 1;
        }
    }
    msg.slash_signatures = entries[..fits].to_vec();
    Ok(())
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
        .with_method("getsubnetbalance", get_subnet_balance)
        .with_method("getgenesisinfo", get_genesis_info)
        .with_method("prefundsubnet", prefund_subnet)
        .with_method("fundsubnet", fund_subnet)
        .with_method("killsubnet", kill_subnet)
        // rootnet messages
        .with_method("getrootnetmessages", get_rootnet_messages)
        // token balance
        .with_method("gettokenbalance", get_token_balance)
        // multisig
        .with_method("genmultisigspendpsbt", gen_multisig_spend_psbt)
        // checkpoints
        .with_method("gencheckpointpsbt", gen_checkpoint_psbt)
        .with_method("finalizecheckpointpsbt", finalize_checkpoint_psbt)
        .with_method("getsubnetcheckpoint", get_subnet_checkpoint)
        // stake changes
        .with_method("stakecollateral", stake_collateral)
        .with_method("unstakecollateral", unstake_collateral)
        .with_method("unstakeue", unstake_ue)
        .with_method("geteotskeys", get_eots_keys)
        .with_method("geteotscommitments", get_eots_commitments)
        .with_method("geteotsrecords", get_eots_records)
        .with_method("signeotsvote", sign_eots_vote)
        .with_method("signslash", sign_slash)
        .with_method("getunslashableoutputs", get_unslashable_outputs)
        .with_method("getstakechanges", get_stake_changes)
        // kill requests
        .with_method("getkillrequests", get_kill_requests);

    #[cfg(feature = "emission_chain")]
    let server = server.with_method("getrewardedcollaterals", get_rewarded_collaterals);

    #[cfg(feature = "dev")]
    // dev methods
    let server = server
        .with_method("dev_multisignpsbt", dev_multisign_psbt)
        .with_method("dev_killsubnet", dev_kill_subnet)
        .with_method("dev_gencheckpointpsbt", dev_gen_checkpoint_psbt);

    server.finish()
}

#[cfg(test)]
mod tests;
