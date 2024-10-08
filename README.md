# bitcoin-ipc

# Bitcoin Regtest Setup Guide

This guide will walk you through the necessary steps needed to setup the environment to be able to run the bitcoin-ipc demo locally.


## Setup

### 1. Install Bitcoin Core

Follow the installation instructions for your operating system provided on the [Bitcoin Core download page](https://bitcoin.org/en/download).

On Linux you can install Bitcoin Core over a shell following the instructions [here](https://bitcoin.org/en/full-node#linux-instructions).
For example, you can install version 27.0 like this:
```sh
wget https://bitcoin.org/bin/bitcoin-core-27.0/bitcoin-27.0-x86_64-linux-gnu.tar.gz
tar xfz bitcoin-27.0-x86_64-linux-gnu.tar.gz
sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-27.0/bin/*
sudo install -m 0755 -o root -g root -t /usr/local/bin bitcoin-27.0/bin/*
```

To verify the installation: Open a terminal or command prompt and verify that `bitcoind` and `bitcoin-cli` are installed correctly:
```sh
bitcoind --version
bitcoin-cli --version
```


### 2. Configure and start Bitcoin Core
   
2.1.  Ensure that the Bitcoin data directory exists. The default locations are:
```
- Linux: ~/.bitcoin/
- macOS: ~/Library/Application Support/Bitcoin/
- Windows: %APPDATA%\Bitcoin\
```

2.2. Create or edit the `bitcoin.conf` file in the Bitcoin data directory and add the following configuration to enable regtest:

```sh
cd ~/.bitcoin/
```

bitcoin.conf:
```ini
regtest=1
server=1
rpcuser=yourusername
rpcpassword=yourpassword
```
  
2.3. Start Bitcoin Core daemon in regtest mode:

```sh
bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001
```

To verify bitcoind is Running:
```sh
bitcoin-cli getblockchaininfo
```


### 3. Configure wallet
  
3.1.  Update .env in the project root

.env file:
```ini
RPC_USER=yourusername
RPC_PASS=yourpassword
RPC_URL=http://localhost:18443
WALLET_NAME=default
```

3.2. Source the .env, in case you have updated it.

```sh
. .env
```

3.3. Set up a wallet (Optional - this is done in the code)
<details>
<summary> Manual Wallet Setup and Block Generation.</summary>

3.3.1 Create a Wallet named default
  
```sh
bitcoin-cli createwallet "default"
```

3.3.2 (Optional - if wallet is not already loaded) Load the Wallet

```sh
bitcoin-cli loadwallet "default"
```

3.3.3 Generate 101 blocks

```sh
bitcoin-cli -regtest generatetoaddress 101 "$(bitcoin-cli -regtest getnewaddress)"
```

3.3.4 Verify block generation

```sh
bitcoin-cli -regtest getblockcount
```
The output should be 101.
</details>


## Set up the demo parameters:
The demo uses the paramateres defined in `config.json`.
- `listener_interval`: Determines how often (in seconds) `btc_listener` contacts the local bitcoin core node to fetch new blocks.
- `ipc_finalization_parameter`: Determines when a bitcoin block is considered final by `btc_listener`. The value `1` means all blocks are considered final, value `v` means that the `v`-th block from the tip of the chain is considered valid (hence, `v-1` blocks must be created after it.)
- `checkpoint_interval`: Determines how often a `relayer` submits checkpoints on behalf of an IPC L2 subnet.
- `postbox_interval`:  Determines how often a `relayer` reads the `postbox` of an IPC L2 subnet.

Remark: You can refer to `architecture.md` for a description of componenets, such as the `btc_listener`, `relayer`, and `postbox`, and to `transactions.md` for a description of IPC-related functionality, such as `checkpoint`.

## Running the demo using the automated scripts

- On MacOS run the following command:
```sh
./scripts/demo_ubuntu.sh 
```

- On Ubuntu run the following command:
```sh
./scripts/demo_ubuntu.sh 
```

This will run `bitcoind`, `btc_monitor`, the `l1_manager`, and it will open a `subnet_interactor` terminal for every existing subnet.


## Running the demo manually

1. Make sure you have started the `bitcoin core` client 

```sh
bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001
```

2. Start `btc_monitor`
```sh
cargo run --bin btc_monitor
```

3. Run the `l1_manager` binary to interact with L1 IPC
```sh
cargo run --bin l1_manager_cli
```

4. Run the `subnet_interactor` binary to interact with a child subnet.
```sh
cargo run --bin subnet_interactor -- --subnet-id <subnet_id> 
```

5. Run a relayer that submits checkpoint transactions for a subnet periodically 
```sh
cargo run --bin relayer -- --subnet-id <subnet_id>
```

## Interacting with the L1 Manager
After the L1 Manager has been started, either using a script or manually, you can interact with following its interactive interface.

1. Press 1 to *Read the IPC state*. This lists all IPC L2 subnets that have been created on top of the bitcoin network that the Bitcoin Monitor is listening to.

2. Press 2 to *create a child subnet*. The L1 Manager prompts for all required subnet data (number of validators and collateral). It then creates the required bitcoin transactions (see `transactions.md`) and submits them to the bitcoin network. Observe the output of the Bitcoin Monitor.

3. Press 3 to *join a subnet*. The L1 Manager prompts to pick a subnet to join and then asks for the required validator data (IP address, a bitcoin public key, where the collateral will be returned, and a name, to be used as identifier for the validator). It then uses the local wallet information to create and submit the required bitcoin transactions (see `transactions.md`). Observe the output of the Bitcoin Monitor. You can also check the balance of the local bitcoin wallet using `bitcoin-cli getbalance` before and after joining the subnet. Observe that in this demo the same bitcoin wallet submits all transactions and mines all blocks, and the block reward is 50 BTC. Hence, after submitting the transactions, the balance should 50 BTC higher, minus the subnet collateral and the fees used for the two transactions.

## Interacting with a subnet using the Subnet Interactor
The Subnet Interactor also exposes an interactive user interface. It allows a subnet user to create an account, transfer funds from that account, and withdraw funds to a bitcoin wallet. It also allows a subnet user to submit *checkpoint* and *delete* transactions to the subnet - when these transactions get finalized on the subnet, they are written on the postbox of that subnet and handled by the Relayer.


## Shutting down the demo
You can stop the bitcoin core client by running
```sh
bitcoin-cli stop
```
