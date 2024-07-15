use dotenv::dotenv;
use std::env;

use crate::bitcoin_utils::LocalNodeError;

/// Load environment variables from a .env file
///
/// # Returns
///
/// A tuple containing the RPC user, RPC password, RPC URL, and wallet name
pub fn load_env() -> Result<(String, String, String, String), LocalNodeError> {
    dotenv().ok();
    let rpc_user = env::var("RPC_USER").map_err(|e| LocalNodeError::EnvVarError {
        var_name: String::from("RPC_USER"),
        internal_error: e,
    })?;
    let rpc_pass = env::var("RPC_PASS").map_err(|e| LocalNodeError::EnvVarError {
        var_name: String::from("RPC_PASS"),
        internal_error: e,
    })?;
    let rpc_url = env::var("RPC_URL").map_err(|e| LocalNodeError::EnvVarError {
        var_name: String::from("RPC_URL"),
        internal_error: e,
    })?;
    let wallet_name = env::var("WALLET_NAME").map_err(|e| LocalNodeError::EnvVarError {
        var_name: String::from("WALLET_NAME"),
        internal_error: e,
    })?;

    Ok((rpc_user, rpc_pass, rpc_url, wallet_name))
}
