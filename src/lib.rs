pub mod bitcoin_utils;
pub mod ipc_lib;
pub mod ipc_state;
pub mod l1_manager;
pub mod provider;
pub mod subnet_simulator;
pub mod utils;

use bitcoin::Network;

pub const NETWORK: Network = Network::Regtest;
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";
pub const IPC_DEPOSIT_TAG: &str = "IPC:DEPOSIT";
pub const IPC_CHECKPOINT_TAG: &str = "IPC:CHECKPOINT";
pub const IPC_TRANSFER_TAG: &str = "IPC:TRANSFER";
pub const IPC_WITHDRAW_TAG: &str = "IPC:WITHDRAW";
pub const IPC_DELETE_SUBNET_TAG: &str = "IPC:DELETE";

pub const DELIMITER: &str = "#";

pub const DEMO_UBUNTU: &str = "scripts/demo_ubuntu.sh";
pub const DEMO_MACOS: &str = "scripts/demo_macos.sh";

pub const L1_NAME: &str = "BTC";
