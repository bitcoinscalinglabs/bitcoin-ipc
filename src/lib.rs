use bitcoin::{FeeRate, Network};

pub mod bitcoin_utils;
pub mod db;
pub mod ipc_lib;
pub mod provider;
pub mod wallet;

// Temporary re-exports
pub use ipc_lib::prelude::*;

/// Name of the L1 chain
pub const L1_NAME: &str = "BTC";

/// Configures the bitcoin network to use
// TODO make this configurable
pub const NETWORK: Network = Network::Regtest;

/// Number of blocks to wait for before considering a block confirmed
pub const BTC_CONFIRMATIONS: u64 = bitcoin_utils::confirmations(NETWORK);

/// Number of blocks after which the validator can spend the pre-fund UTXO
/// (withdraw the collateral for boostraping the subnet)
///
/// It must be bigger than `BTC_CONFIRMATIONS` because the validators
/// needs to wait for the transaction to be confirmed.
pub const PRE_FUND_TIMELOCK_BLOCKS: u16 = (BTC_CONFIRMATIONS as u16) + 6;

pub const DEFAULT_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(10_000);
pub const MINIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(1_000);
pub const MAXIMUM_BTC_FEE_RATE: FeeRate = FeeRate::from_sat_per_vb_unchecked(100_000);
