use dotenv::dotenv;
use std::env;

pub fn load_env() -> Result<(String, String, String, String), env::VarError> {
    dotenv().ok();
    let rpc_user = env::var("RPC_USER")?;
    let rpc_pass = env::var("RPC_PASS")?;
    let rpc_url = env::var("RPC_URL")?;
    let wallet_name = env::var("WALLET_NAME")?;

    Ok((rpc_user, rpc_pass, rpc_url, wallet_name))
}
