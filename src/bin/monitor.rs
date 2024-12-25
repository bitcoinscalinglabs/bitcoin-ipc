use bitcoin_ipc::bitcoin_utils::make_rpc_client_from_env;
use bitcoin_ipc::BTC_CONFIRMATIONS;
use bitcoincore_rpc::RpcApi;
use dotenv::dotenv;
use log::{debug, error, info};
use tokio::signal;
use tokio::sync::oneshot;
use tokio::time::Duration;

// TODO make configurable
const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    // Load .env file

    dotenv().ok();

    // Initialize the logger, configurable by the RUST_LOG env

    env_logger::init();

    // Init the bitcoincore_rpc client

    let btc_rpc = make_rpc_client_from_env();

    // TODO make configurable
    let mut monitor = Monitor::new(btc_rpc, POLL_INTERVAL);

    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        // Sync
        if let Err(e) = monitor.sync().await {
            error!("Error syncing: {:?}", e);
            // Signal termination
            tx.send(Err(e)).expect("Could not signal termination.");
        }
        // Listen for new block
        monitor.listen().await;
    });

    // Wait for a termination signal (e.g., Ctrl+C) or the spawned task to complete
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received Ctrl+C");
        }
        result = rx => {
            match result {
                Ok(Ok(())) => info!("Monitor task completed"),
                Ok(Err(e)) => error!("Monitor task failed: {:?}", e),
                Err(_) => error!("Monitor task channel closed unexpectedly"),
            }
        }
    }

    info!("Shutting down");
}

// TODO use a generic for the RPC client + add a trait for the monitor
struct Monitor {
    rpc: bitcoincore_rpc::Client,
    check_interval: Duration,
    current_height: u64,
}

impl Monitor {
    fn new(rpc: bitcoincore_rpc::Client, check_interval: Duration) -> Self {
        Self {
            rpc,
            check_interval,
            current_height: 0,
        }
    }

    async fn sync(&mut self) -> Result<(), bitcoincore_rpc::Error> {
        info!("Syncing...");

        loop {
            // Get the latest block height
            let latest_block_height = self.get_latest_confirmed_height()?;

            // Process blocks from current_height to latest_block_height
            while self.current_height < latest_block_height {
                let next_height = self.current_height + 1;
                match self.process_block(next_height) {
                    Ok(_) => {
                        info!("Processed block {}", next_height);
                        self.current_height = next_height;
                    }
                    Err(e) => {
                        error!(
                            "Error processing block {}: {:?}. Retrying...",
                            next_height, e
                        );
                        // Retry logic can be added here if needed
                    }
                }
            }

            // Check if we are up-to-date
            if self.current_height == latest_block_height {
                info!("Sync completed");
                break;
            }
        }

        Ok(())
    }

    async fn listen(&mut self) {
        info!("Listening for new blocks");
        loop {
            match self.get_latest_confirmed_height() {
                Ok(block_count) => {
                    if block_count > self.current_height {
                        match self.process_block(block_count) {
                            Ok(_) => {
                                info!("Processed block {}", block_count);
                                self.current_height = block_count;
                            }
                            Err(e) => {
                                error!(
                                    "Error processing block {}: {:?}. Retrying...",
                                    block_count, e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error fetching block count: {:?}", e);
                }
            }
            tokio::time::sleep(self.check_interval).await;
        }
    }

    fn get_latest_confirmed_height(&self) -> Result<u64, bitcoincore_rpc::Error> {
        let latest = self.rpc.get_block_count()?;

        // Since BTC_CONFIRMATIONS is 0 in regtest and sigtest
        // Clippy will complain about absurd comparisons
        #[allow(clippy::absurd_extreme_comparisons)]
        if latest < BTC_CONFIRMATIONS {
            return Ok(0);
        }

        Ok(latest - BTC_CONFIRMATIONS)
    }

    fn process_block(&self, block_height: u64) -> Result<(), bitcoincore_rpc::Error> {
        info!("Processing block {}", block_height);
        let block_hash = self.rpc.get_block_hash(block_height)?;
        let block = self.rpc.get_block(&block_hash)?;

        for tx in block.txdata {
            self.process_transaction(&tx, block_height)?;
        }

        Ok(())
    }

    fn process_transaction(
        &self,
        tx: &bitcoin::Transaction,
        _block_height: u64,
    ) -> Result<(), bitcoincore_rpc::Error> {
        debug!("Processing transaction {}", tx.compute_txid());
        Ok(())
    }
}
