pub mod bitcoin_utils;
pub mod ipc_lib;
pub mod ipc_state;
pub mod subnet_simulator;
pub mod utils;

use bitcoin::Network;

pub const NETWORK: Network = Network::Regtest;
pub const IPC_CREATE_SUBNET_TAG: &str = "IPC:CREATE";
pub const IPC_JOIN_SUBNET_TAG: &str = "IPC:JOIN";

pub const L1_NAME: &str = "BTC";
