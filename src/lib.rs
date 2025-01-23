use bitcoin::{FeeRate, Network};

pub mod bitcoin_utils;
pub mod db;
pub mod eth_utils;
pub mod ipc_lib;
pub mod multisig;
pub mod provider;
pub mod wallet;

// Temporary re-exports
pub use ipc_lib::prelude::*;

/// Configures the bitcoin network to use
// TODO make this configurable
pub const NETWORK: Network = Network::Regtest;
/// Name of the L1 chain - Regtest
/// See https://github.com/bitcoin/bips/blob/master/bip-0122.mediawiki
pub const L1_NAME: &str =
    "/bip122:0f9188f13cb7b2c71f2a335e3a4fc328bf5beb436012afca590b1a11466e2206";

/// Number of blocks to wait for before considering a block confirmed
pub const BTC_CONFIRMATIONS: u64 = bitcoin_utils::confirmations(NETWORK);

pub const DEFAULT_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(10);
pub const MINIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(1);
pub const MAXIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(100);
