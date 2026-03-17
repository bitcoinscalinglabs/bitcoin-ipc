# Docker Deployment Guide (Local)

Guide for running Bitcoin IPC in Docker for **local deployment**.

## Building the repo and starting the `bitcoin-ipc` container

The IPC repo is built into a separate image to avoid rebuilding when we make local changes to this repo.

Build it once (from repo root):

```bash
docker build -f docker-deploy-local/Dockerfile.ipc -t ipc-builder:latest .
```

Then build/run the main container normally:

```bash
docker-compose up --build
```

Persistent data (Bitcoin chain, IPC config in volumes) is unchanged; only the image is rebuilt.


## What does the `bitcoin-ipc` container run?

The `bitcoin-ipc` container contains/runs:

- Bitcoin Core in `regtest` mode.
- Bitcoin wallets and IPC configuration (under `~/.ipc/`) for all 6 validators and 2 users, generated using the `src/bin/quickstart.rs` script upon container creation.
- The `monitor` and `provider` binaries for each validator (`validator1`–`validator6`) and user (`user1`, `user2`), automatically started upon container creation.
- The captured logs for each monitor and provider instance in the `/root/logs/` directory inside the container.
- The `ipc-cli` tool.
- Sets the required environment variables
- The script `/workspace/bitcoin-ipc/scripts/miner.sh` that mines regtest blocks every 10 seconds is started automatically upon container creation.
- Scripts to quickly create, join, and fund subnets: `bootstrap_subnet_a_from_container.sh` and `bootstrap_subnet_b_from_container.sh` (mounted from `scripts/`, not run automatically).


## Layout Inside the Container

- `/workspace/bitcoin-ipc/` – bitcoin-ipc repo
- `/workspace/ipc/` – IPC repo
- `/root/.bitcoin/` – Bitcoin Core regtest data and config
- `/root/.ipc/` – IPC configs (validators/users)
- `/root/logs/` – monitor, provider, and relayer log files (e.g. `monitor-validator1.log`, `provider-validator1.log`, `relayer-subnet-a-validator1.log`)

Binaries (in `PATH`): `monitor`, `provider`, `quickstart`, `ipc-cli`, `fendermint`, `bitcoind`, `bitcoin-cli`, Foundry tools.


## Exposed Ports
<!-- - **18443** – Bitcoin RPC -->
- **3030–3035** – Provider (validator1–6)
- **3040–3041** – Provider (user1–2)




## Monitor, provider, and relayer logs

Log files live inside the container at `/root/logs/` (same volume as `/root`, so they persist across restarts):

- `monitor-validator1.log`, `provider-validator1.log` … `monitor-validator6.log`, `provider-validator6.log`
- `monitor-user1.log`, `provider-user1.log`, `monitor-user2.log`, `provider-user2.log`
- `relayer-subnet-a-validator1.log` … `relayer-subnet-a-validator4.log` (started by `spin_up_subnet_a_from_container.sh`)
- `relayer-subnet-b-validator1.log` … `relayer-subnet-b-validator4.log` (started by `spin_up_subnet_b_from_container.sh`)

**View logs from the host:**

```bash
# Stream monitor log
docker exec bitcoin-ipc tail -f /root/logs/monitor-validator1.log

# Stream provider log
docker exec bitcoin-ipc tail -f /root/logs/provider-validator1.log

# Stream relayer log (Subnet A)
docker exec bitcoin-ipc tail -f /root/logs/relayer-subnet-a-validator1.log
```

## Running monitor and provider manually

The entrypoint script starts them automatically. To run in the foreground (e.g. in a separate terminal) use, while inside the container:

```bash
# Start monitor for validator1
monitor --env ~/.ipc/validator1/.env

# Start provider for validator1
provider --env ~/.ipc/validator1/.env
```

### List running monitor/provider/relayer processes

```bash
pgrep -a monitor || true
pgrep -a provider || true
pgrep -af "checkpoint relayer" || true
```

## Available commands (IPC-CLI, cast)
All ipc-cli commands can be run from within the subnet. For example:

- Send native (wBTC) tokens between accounts in the subnet:
```
export RPC_URL="http://host.docker.internal:8545"

# Read hex private key from validator1 sk file.
# Supports either raw hex with 0x prefix.
# base64-encoded bytes (some key files): will be converted to hex
SENDER_PRIVATE_KEY="$(tr -d '\r\n[:space:]' < /root/.ipc/validator1/validator.sk)"

RECIPIENT_ADDRESS=0xb8c4486622484150084a8E1Ee6687e17fEBE6229

# In the subnet 1 ether = 1 wBTC. To send 50K sats:
VALUE="0.0005ether"

# send 0.01 native units to an address
cast send \
  --rpc-url "$RPC_URL" \
  --private-key "$SENDER_PRIVATE_KEY" \
  --value $VALUE \
  $RECIPIENT_ADDRESS
```

- List balances in a subnet:
```
ipc-cli wallet balances --wallet-type btc --subnet $SubnetID
```
and
```
# With --ether to show the balance in wBTC unit (in the subnet 1 ether = 1 wBTC)
cast balance --ether 0x27b60d9f71d6806cca7d5a92b391093fe100f8e8 --rpc-url $RPC_URL
```

Subnet creation (set `WHITELIST` first, e.g. from quickstart output):

```bash
docker exec -e WHITELIST=$WHITELIST bitcoin-ipc ipc-cli \
  --config-path ~/.ipc/validator1/config.toml subnet create \
  --parent /b4 --min-validators 4 --bottomup-check-period 60 \
  btc --min-validator-stake 100000000 --min-cross-msg-fee 10 \
  --validator-whitelist $WHITELIST
```

Other commands (balance, etc.): use the same `ipc-cli --config-path ~/.ipc/validatorN/config.toml` (or `userN`) inside the container.


## Troubleshooting
### Recreating `.ipc` or Bitcoin data

**Reset IPC config (new wallets/keys):**

```bash
docker exec bitcoin-ipc rm -rf /root/.ipc
docker-compose restart
# or: docker-compose down && docker-compose up -d
```

Quickstart will recreate `~/.ipc/` on next start.

**Reset Bitcoin (fresh chain):**

```bash
docker exec bitcoin-ipc rm -rf /root/.bitcoin
docker-compose restart
```

The entrypoint recreates `/root/.bitcoin` and `bitcoin.conf` only when `/root/.bitcoin` is missing, so existing chain data is preserved until you delete it.

### Fendermint image missing on host

The fendermint image is built in the container when the Docker socket is mounted. To build manually:

```bash
docker exec bitcoin-ipc bash -c "cd /workspace/ipc/fendermint && make docker-build"
docker images fendermint
```
