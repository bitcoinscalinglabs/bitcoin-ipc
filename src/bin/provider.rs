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
    let eots_signer = match load_secret_key("EOTS_SK_PATH", "EOTS")? {
        Some(sk) => {
            let Some(seed) = load_hex32("EOTS_SEED_PATH", "EOTS nonce seed")? else {
                error!("EOTS_SEED_PATH is empty: an EOTS key needs the seed of its nonces");
                return Err(std::io::Error::other("EOTS_SEED_PATH is empty"));
            };
            let signer = provider::eots_signer::EotsSigner::load(
                sk,
                bitcoin_ipc::eots::EotsSeed::from_bytes(seed),
                &PathBuf::from(std::env::var("EOTS_SK_PATH").expect("set above")),
            )
            .map_err(|e| std::io::Error::other(format!("EOTS sign record: {e}")))?;
            let signer = Arc::new(std::sync::Mutex::new(signer));
            build_nonce_trees(db.clone(), signer.clone());
            Some(signer)
        }
        None => None,
    };

    // Start up the actix-web server

    let port = std::env::var("PROVIDER_PORT").unwrap_or_else(|_| DEFAULT_PROVIDER_PORT.to_string());

    let server_data = Arc::new(provider::ServerData {
        db,
        btc_rpc,
        btc_watchonly_rpc,
        validator,
        eots_signer,
    });

    provider::Server::new(token, port, server_data)
        .serve()
        .await
}

/// Builds in the background the nonce tree of every recorded commitment of the signer's key.
fn build_nonce_trees(
    db: Arc<HeedDb>,
    signer: Arc<std::sync::Mutex<provider::eots_signer::EotsSigner>>,
) {
    std::thread::spawn(move || {
        let (pk_eots, seed) = {
            let signer = signer.lock().expect("EOTS signer lock");
            (signer.pk_eots(), signer.seed())
        };
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
            let tree =
                match bitcoin_ipc::eots::nonce_tree(&seed, &record.subnet_id, &record.commitment) {
                    Ok(tree) => tree,
                    Err(e) => {
                        warn!(
                            "EOTS nonce tree of subnet {} not built: {e}",
                            record.subnet_id
                        );
                        continue;
                    }
                };
            signer.lock().expect("EOTS signer lock").insert_tree(
                record.subnet_id,
                record.commitment.start,
                tree,
            );
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
    let path = match std::env::var(var) {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            warn!("{var} is empty: no {what} loaded");
            return Ok(None);
        }
    };
    let text = std::fs::read_to_string(&path).map_err(|e| {
        error!("Couldn't read {what} from {}: {}", path.display(), e);
        std::io::Error::other(format!("Couldn't read {what}: {}", e))
    })?;
    let text = text.trim();
    let text = text.strip_prefix("0x").unwrap_or(text);
    let bytes = hex::decode(text).map_err(|e| {
        error!("Invalid hex encoding in {what} file: {}", e);
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid hex encoding in {what} file: {}", e),
        )
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        error!("Invalid {what}: not 32 bytes");
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid {what}: not 32 bytes"),
        )
    })?;
    info!("Loaded {what} from {}.", path.display());
    Ok(Some(bytes))
}
