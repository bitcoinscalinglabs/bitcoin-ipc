use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use bitcoin_ipc::db::{DatabaseCore, HeedDb};
use bitcoin_ipc::{bitcoin_utils, eth_utils, provider};
use clap::Parser;
use log::{error, info, warn};

const DEFAULT_PROVIDER_PORT: &str = "3030";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to env file
    #[arg(long, default_value = ".env")]
    env: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Parse command line arguments

    let args = Args::parse();

    // Load .env file

    let env_path = if args.env.starts_with('/') {
        PathBuf::from(&args.env)
    } else {
        env::current_dir().map(|a| a.join(&args.env)).unwrap()
    };

    dotenv::from_path(env_path.as_path())
        .unwrap_or_else(|_| panic!("Failed to load env file: {}", args.env));

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Initialize the database

    let db = Arc::new(
        HeedDb::new(
            &std::env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            true, // using db in read-only mode
        )
        .await
        .expect("Failed to initialize database"),
    );

    // Load auth token from env

    let token = std::env::var("PROVIDER_AUTH_TOKEN").map_err(|e| {
        error!("Couldn't load PROVIDER_AUTH_TOKEN: {}", e);
        std::io::Error::other("Couldn't load PROVIDER_AUTH_TOKEN")
    })?;

    // Init the bitcoincore_rpc client

    let btc_rpc = Arc::new(bitcoin_utils::make_rpc_client_from_env());
    let btc_watchonly_rpc = Arc::new(bitcoin_utils::make_watchonly_rpc_client_from_env());

    // Set correct fvm network, before the vote database parses subnet ids

    eth_utils::set_fvm_network();

    // Load validator secret key
    let validator = load_validator()?;
    let eots_keys = eots_keys()?.map(Arc::new);
    if let Some(keys) = &eots_keys {
        for signer in keys.signers() {
            build_nonce_trees(db.clone(), signer);
        }
    }

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| DEFAULT_PROVIDER_PORT.to_string());

    let server_data = Arc::new(provider::ServerData {
        db,
        btc_rpc,
        btc_watchonly_rpc,
        validator,
        eots_keys,
    });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}

/// Loads in the background the subtree roots of every recorded commitment of the signer's key,
/// and builds the subtree of the next height to sign.
fn build_nonce_trees(
    db: Arc<HeedDb>,
    signer: Arc<std::sync::Mutex<provider::eots_signer::EotsSigner>>,
) {
    std::thread::spawn(move || {
        let pk_eots = signer.lock().expect("EOTS signer lock").pk_eots();
        let records = match db.get_eots_commitments_of_key(&pk_eots) {
            Ok(records) => records,
            Err(e) => {
                warn!("EOTS nonce trees not built: {e}");
                return;
            }
        };
        for record in records {
            if signer
                .lock()
                .expect("EOTS signer lock")
                .has_tree(record.subnet_id, &record.commitment)
            {
                continue;
            }
            let started = std::time::Instant::now();
            let built = provider::eots_signer::EotsSigner::hold_tree(
                &signer,
                record.subnet_id,
                &record.commitment,
            );
            if let Err(e) = built {
                warn!(
                    "EOTS nonce tree of subnet {} not built: {e}",
                    record.subnet_id
                );
                continue;
            }
            match record.commitment.end() {
                Ok(end) => info!(
                    "EOTS nonce tree of subnet {} heights {}..{} built in {:.1} s",
                    record.subnet_id,
                    record.commitment.start,
                    end,
                    started.elapsed().as_secs_f64()
                ),
                Err(e) => warn!("EOTS nonce tree of subnet {} built: {e}", record.subnet_id),
            }
        }
    });
}

fn load_validator(
) -> Result<Option<(bitcoin::XOnlyPublicKey, bitcoin::secp256k1::SecretKey)>, std::io::Error> {
    let Some(validator_sk) = load_secret_key("VALIDATOR_SK_PATH", "validator")? else {
        warn!("VALIDATOR_SK_PATH is empty, you won't be able to sign transactions!");
        return Ok(None);
    };
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (validator_pk, _) = validator_sk.x_only_public_key(&secp);
    info!("Validator PK: {}", validator_pk);
    Ok(Some((validator_pk, validator_sk)))
}

/// The secret key in the hex file named by `var`, `None` when `var` is unset or empty.
fn load_secret_key(
    var: &str,
    what: &str,
) -> Result<Option<bitcoin::secp256k1::SecretKey>, std::io::Error> {
    let Some(sk_bytes) = load_hex32(var, &format!("{what} secret key"))? else {
        return Ok(None);
    };
    let sk = bitcoin::secp256k1::SecretKey::from_slice(&sk_bytes).map_err(|e| {
        error!("Invalid {what} secret key: {}", e);
        std::io::Error::other(format!("Invalid {what} secret key: {}", e))
    })?;
    Ok(Some(sk))
}

/// The 32 bytes in the hex file named by `var`, `None` when `var` is unset or empty.
fn load_hex32(var: &str, what: &str) -> Result<Option<[u8; 32]>, std::io::Error> {
    let Some(path) = env_path(var) else {
        warn!("{var} is empty: no {what} loaded");
        return Ok(None);
    };
    let bytes = provider::eots_signer::read_hex32(&path).map_err(|e| {
        error!("Couldn't read {what}: {e}");
        std::io::Error::other(format!("Couldn't read {what}: {e}"))
    })?;
    info!("Loaded {what} from {}.", path.display());
    Ok(Some(bytes))
}

/// The EOTS keys: the key of `EOTS_SK_PATH` and `EOTS_SEED_PATH`, if set, and one per subnet in
/// the directory of the validator key file, where a join-ue generates them. `None` without a
/// validator key file and a configured key.
fn eots_keys() -> Result<Option<provider::eots_keys::EotsKeys>, std::io::Error> {
    let configured = match (env_path("EOTS_SK_PATH"), env_path("EOTS_SEED_PATH")) {
        (Some(key), Some(seed)) => Some(provider::eots_signer::EotsKeyFiles { key, seed }),
        (Some(_), None) => {
            error!("EOTS_SEED_PATH is empty: an EOTS key needs the seed of its nonces");
            return Err(std::io::Error::other("EOTS_SEED_PATH is empty"));
        }
        (None, _) => None,
    };
    let key_file =
        env_path("VALIDATOR_SK_PATH").or_else(|| configured.as_ref().map(|f| f.key.clone()));
    let Some(key_file) = key_file else {
        warn!("VALIDATOR_SK_PATH and EOTS_SK_PATH are empty: no EOTS key");
        return Ok(None);
    };
    let dir = provider::eots_keys::key_dir(&key_file);
    let keys =
        provider::eots_keys::EotsKeys::load(dir.clone(), configured.as_ref()).map_err(|e| {
            error!("EOTS keys: {e}");
            std::io::Error::other(format!("EOTS keys: {e}"))
        })?;
    info!(
        "EOTS keys: {} held, a join-ue generates the key of a new subnet in {}",
        keys.signers().len(),
        dir.display()
    );
    Ok(Some(keys))
}

/// The path in the environment variable `var`, `None` when unset or empty.
fn env_path(var: &str) -> Option<PathBuf> {
    env::var(var)
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}
