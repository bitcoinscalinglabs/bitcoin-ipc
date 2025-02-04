# Bitcoin Ipc — Local Subnet Deployment

We'll now create a local subnet with 4 validators. This guide will loosly follow the official [quickstart guide](https://docs.ipc.space/quickstarts/deploy-a-subnet), with some bitcoin-specific changes we will note.

## Step 1: Prepare and install `ipc-cli`

See [the guide](./ipc-cli.md) for instructions on how to install `ipc-cli` that supports bitcoin.

## Step 2: Set up a Bitcoin wallet for each validator

In order for each validator to stake, they need to have a wallet set up. We'll create and fund a wallet for each validator.

```sh
# create 4 wallets
bitcoin-cli createwallet "validator1"
bitcoin-cli createwallet "validator2"
bitcoin-cli createwallet "validator3"
bitcoin-cli createwallet "validator4"

# fund 4 wallets
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator1 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator2 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator3 getnewaddress)"
bitcoin-cli generatetoaddress 102 "$(bitcoin-cli --rpcwallet=validator4 getnewaddress)"

# check balances
bitcoin-cli --rpcwallet=validator1 getbalance
bitcoin-cli --rpcwallet=validator2 getbalance
bitcoin-cli --rpcwallet=validator3 getbalance
bitcoin-cli --rpcwallet=validator4 getbalance
```

All wallets should have at least 100 BTC.

## Step 3: Run Monitor and Provider for each validator

Navigate to bitcoin-ipc directory and run the following commands:

```sh
mkdir -p ~/.ipc/validator1/
mkdir -p ~/.ipc/validator2/
mkdir -p ~/.ipc/validator3/
mkdir -p ~/.ipc/validator4/

cp .env.example ~/.ipc/validator1/.env
cp .env.example ~/.ipc/validator2/.env
cp .env.example ~/.ipc/validator3/.env
cp .env.example ~/.ipc/validator4/.env
```

Modify the env files to match `WALLET_NAME`, `DATABASE_URL`, `PROVIDER_PORT` and `PROVIDER_AUTH_TOKEN` for each validator. We'll use ports 3030, 3031, 3032, and 3033 for each validator respectively. Let's see the `validator1.env` file as an example:

```sh
# Bitcoin Core RPC

RPC_USER=user
RPC_PASS=pass
RPC_URL=http://localhost:18443
WALLET_NAME=validator1

# Provider + Monitor

DATABASE_URL=validator1_regtest_db

# Provider

PROVIDER_PORT=3030
PROVIDER_AUTH_TOKEN=validator1_auth_token

# General

RUST_LOG=bitcoin_ipc=debug,monitor=debug,bitcoincore_rpc=error,actix_web=info
```

Let's now run the monitor and provider for each validator, make sure every process is run in a separate terminal window/pane.

```sh
./target/release/monitor --env ~/.ipc/validator1/.env
./target/release/provider --env ~/.ipc/validator1/.env

./target/release/monitor --env ~/.ipc/validator2/.env
./target/release/provider --env ~/.ipc/validator2/.env

./target/release/monitor --env ~/.ipc/validator3/.env
./target/release/provider --env ~/.ipc/validator3/.env

./target/release/monitor --env ~/.ipc/validator4/.env
./target/release/provider --env ~/.ipc/validator4/.env
```

## Step 4: Initialise your config

```sh
ipc-cli --config-path=~/.ipc/validator1/config.toml config init
ipc-cli --config-path=~/.ipc/validator2/config.toml config init
ipc-cli --config-path=~/.ipc/validator3/config.toml config init
ipc-cli --config-path=~/.ipc/validator4/config.toml config init

cat ~/.ipc/validator1/config.toml
```

It should show the following.

```toml
keystore_path = "~/.ipc"

# Bitcoin Regtest
[[subnets]]
id = "/b4"

[subnets.config]
network_type = "btc"
provider_http = "http://127.0.0.1:3030/api"
auth_token = ""

# Filecoin Calibration
[[subnets]]
id = "/r314159"

[subnets.config]
network_type = "fevm"
provider_http = "https://api.calibration.node.glif.io/rpc/v1"
gateway_addr = "0x1AEe8A878a22280fc2753b3C63571C8F895D2FE3"
registry_addr = "0x0b4e239FF21b40120cDa817fba77bD1B366c1bcD"

# Subnet template - uncomment and adjust before using
# [[subnets]]
# id = "/r314159/<SUBNET_ID>"

# [subnets.config]
# network_type = "fevm"
# provider_http = "https://<RPC_ADDR>/"
# gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
# registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
```

For each validator's config file, modify the `provider_http` port and `auth_token` field to match the port and `PARENT_AUTH_TOKEN` you've set in the previous step.

> `/b4` is the identifier for the Bitcoin regtest network.


## Step 5: Set up IPC wallets for each validators

Now that each validator has personal funds on Bitcoin, and the monitor and provider are running, we can set up IPC wallets for each validator. These wallets will be used to interact with the subnet and sign multisig transactions.

```sh
ipc-cli wallet new --wallet-type btc
ipc-cli wallet new --wallet-type btc
ipc-cli wallet new --wallet-type btc
ipc-cli wallet new --wallet-type btc
```

List and record the newly created wallets addresses and corresponding x-only public keys:

```sh
ipc-cli wallet list --wallet-type btc
```

## Step 6: Create a child subnet

The next step is to create a subnet under `/b4` — Bitcoin Regtest. Validator 1 will pay the Bitcoin fee, but it could be any of the validators or someone else entirely.

```sh
ipc-cli --config-path=~/.ipc/validator1/config.toml subnet create \
	# general subnet options
	--parent /b4 --min-validators 4 --bottomup-check-period 300 \
	# btc specific options
	btc --min-validator-stake 100000000 --min-cross-msg-fee 10 \
	# comma-separated list of our validators x-only public keys
	--validator-whitelist 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268,851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4,b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0,b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529
```

You should see the subnet ID printed to the console. Let's save the subnet ID for later use.

The create transaction was sent to the mempool, so let's mine a block to include it in the blockchain, afterwhich it'll be picked up by our monitors.

```sh
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli getnewaddress)"
```

If everything went well, all monitors should have printed the subnet create message to the console.

## Step 7: Join the subnet

Before we deploy the infrastructure for the subnet, we will have to bootstrap the subnet and join from our validators, putting some initial collateral into the subnet.

Replace the `--from` field with validator IPC wallet addresses we created in Step 5. Replace the `--subnet` with the subnet ID we got in the previous step.

```sh
ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=200000000 --ip 66.222.44.55:8080 --backup-address "$(bitcoin-cli --rpcwallet=validator1 getnewaddress)"

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=110000000 --ip 66.222.44.55:8081 --backup-address "$(bitcoin-cli --rpcwallet=validator2 getnewaddress)"

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=150000000 --ip 66.222.44.55:8082 --backup-address "$(bitcoin-cli --rpcwallet=validator3 getnewaddress)"

ipc-cli --config-path=~/.ipc/validator1/config.toml subnet join --from 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --subnet=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e btc --collateral=180000000 --ip 66.222.44.55:8083 --backup-address "$(bitcoin-cli --rpcwallet=validator4 getnewaddress)"
```

Let's include the join transactions in the blockchain by mining a block.

```sh
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli getnewaddress)"
```

We should see the monitors print the join messages to the console.

## Step 8: Deploy the infrastructure

Let's export the secret keys of validators:

```sh
ipc-cli wallet export --wallet-type btc --address 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 --hex > ~/.ipc/validator1/validator.sk
ipc-cli wallet export --wallet-type btc --address 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 --hex > ~/.ipc/validator2/validator.sk
ipc-cli wallet export --wallet-type btc --address 0x646Aed5404567ae15648E9b9B0004cbAfb126949 --hex > ~/.ipc/validator3/validator.sk
ipc-cli wallet export --wallet-type btc --address 0xBcE2f194e9628E6ae06fa0D85DD57Cd5579213bf --hex > ~/.ipc/validator4/validator.sk
```

Let's start our first validator which the rest of the validators will bootstrap from. Make sure you have docker running before running this command.

```sh
cargo make --makefile infra/fendermint/Makefile.toml \
    -e NODE_NAME=validator-1 \
    -e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
    -e PRIVATE_KEY_PATH=$HOME/.ipc/validator1/validator.sk \
    -e CMT_P2P_HOST_PORT=26656 \
    -e CMT_RPC_HOST_PORT=26657 \
    -e ETHAPI_HOST_PORT=8545 \
    -e RESOLVER_HOST_PORT=26655 \
    -e PARENT_ENDPOINT="http://host.docker.internal:3030/api" \
    -e PARENT_AUTH_TOKEN="validator1_auth_token" \
    -e FM_PULL_SKIP=1 \
    child-validator
```

Once the first validator is up and running, it will print out the relative information for this validator.

```
#################################
#                               #
# Subnet node ready! 🚀         #
#                               #
#################################

Subnet ID:
	/r314159/t410f6b2qto756ox3qfoonq4ii6pdrylxwyretgpixuy

Eth API:
	http://0.0.0.0:8545

Chain ID:
	3684170297508395

Fendermint API:
	http://localhost:26658

CometBFT API:
	http://0.0.0.0:26657

CometBFT node ID:
	ca644ac3194d39a2834f5d98e141d682772c149b

CometBFT P2P:
	http://0.0.0.0:26656

IPLD Resolver Multiaddress:
	/ip4/0.0.0.0/tcp/26655/p2p/16Uiu2HAkwhrWn9hYFQMR2QmW5Ky7HJKSGVkT8xKnQr1oUGCkqWms

```

You'll need the final component of the `IPLD Resolver Multiaddress` (the `peer ID`) and the `CometBFT node ID` for the next nodes to start.

*   _**BOOTSTRAPS**_: \<CometBFT node ID for validator1>@validator-1-cometbft:26656

    ```
    // An example
    ca644ac3194d39a2834f5d98e141d682772c149b@validator-1-cometbft:26656
    ```
*   _**RESOLVER\_BOOTSTRAPS**_: /dns/validator-1-fendermint/tcp/26655/p2p/\<Peer ID in IPLD Resolver Multiaddress>

    <pre><code>// An example
    <strong>/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAkwhrWn9hYFQMR2QmW5Ky7HJKSGVkT8xKnQr1oUGCkqWms
    </strong></code></pre>

Now, let's start the rest of the validators:

```sh
# Run second validator
cargo make --makefile infra/fendermint/Makefile.toml \
	-e NODE_NAME=validator-2 \
	-e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
	-e PRIVATE_KEY_PATH=$HOME/.ipc/validator2/validator.sk \
	-e CMT_P2P_HOST_PORT=26756 \
	-e CMT_RPC_HOST_PORT=26757 \
	-e ETHAPI_HOST_PORT=8645 \
	-e RESOLVER_HOST_PORT=26755 \
	-e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
	-e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
	-e PARENT_ENDPOINT="http://host.docker.internal:3031/api" \
	-e PARENT_AUTH_TOKEN="validator2_auth_token" \
	-e FM_PULL_SKIP=1 \
	child-validator

# Run third validator
cargo make --makefile infra/fendermint/Makefile.toml \
	-e NODE_NAME=validator-3 \
	-e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
	-e PRIVATE_KEY_PATH=$HOME/.ipc/validator3/validator.sk \
	-e CMT_P2P_HOST_PORT=26856 \
	-e CMT_RPC_HOST_PORT=26857 \
	-e ETHAPI_HOST_PORT=8745 \
	-e RESOLVER_HOST_PORT=26855 \
	-e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
	-e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
	-e PARENT_ENDPOINT="http://host.docker.internal:3032/api" \
	-e PARENT_AUTH_TOKEN="validator3_auth_token" \
	-e FM_PULL_SKIP=1 \
	child-validator

# Run fourth validator
cargo make --makefile infra/fendermint/Makefile.toml \
	-e NODE_NAME=validator-4 \
	-e SUBNET_ID=/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e \
	-e PRIVATE_KEY_PATH=$HOME/.ipc/validator4/validator.sk \
	-e CMT_P2P_HOST_PORT=26956 \
	-e CMT_RPC_HOST_PORT=26957 \
	-e ETHAPI_HOST_PORT=8845 \
	-e RESOLVER_HOST_PORT=26955 \
	-e BOOTSTRAPS=b082595e23d0814b202984313759a5e5bc6c6fbd@validator-1-cometbft:26656 \
	-e RESOLVER_BOOTSTRAPS=/dns/validator-1-fendermint/tcp/26655/p2p/16Uiu2HAmDa2iAkkotWE2X65RfAGYFjU2QwyASesHcpM6nEacj336 \
	-e PARENT_ENDPOINT="http://host.docker.internal:3033/api" \
	-e PARENT_AUTH_TOKEN="validator4_auth_token" \
	-e FM_PULL_SKIP=1 \
	child-validator
```

## Step 9: Check status of the subnet

Add the new subnet configuration to your default IPC CLI configuration available at `~/.ipc/config.toml`:

```toml
[[subnets]]
id = "/b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e"

[subnets.config]
network_type = "fevm"
provider_http = "http://localhost:8545/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
```

To check the status of the subnet, run:

```sh
ipc-cli subnet rpc --network /b4/t420fdvyrihvwxp5m4ppz2jlwhzq35jaxi4fyints7dwni22fqjz2ftevhzr24e
```

You should see the chain id and rpc url printed to the console.

> Run `docker ps -a` to see the running docker containers.


## Cleanup

To clean up the subnet, stop and remove all docker containers:

```sh
docker stop $(docker ps -aq)
docker rm $(docker ps -aq)

ipc-cli wallet list --wallet-type btc
ipc-cli wallet remove --wallet-type btc --address $addr
```
