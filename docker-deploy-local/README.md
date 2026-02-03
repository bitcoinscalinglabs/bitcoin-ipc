# Docker Deployment Guide (Local)

Guide for running Bitcoin IPC in Docker for **local deployment**: all validator containers on a single host. The Compose project name is `bitcoin-ipc-local`; the main container name is `bitcoin-ipc`, specified in `Dockerfile1`; this uses container `ipc-builder`, specified in `Dockerfile.ipc`.

## What does the `bitcoin-ipc` container run?

The `bitcoin-ipc` container contains/runs:

- **Bitcoin Core** in `regtest` mode for private blockchain testing.
- Bitcoin wallets and IPC configuration (under `~/.ipc/`) for all 5 validators and 2 users, using the `quickstart` script.
- The `monitor` and `provider` binaries for each validator (`validator1`–`validator5`) and user (`user1`, `user2`).
  - Captures logs for each monitor/provider instance, outputting them to `/root/logs/` inside the container.
- The `ipc-cli` tool and `Fendermint` image.
- Sets the environment variables
- A script to quickly create, join, and fund subnet can be found on `/workspace/bitcoin-ipc/internal/bootstrap_subnet_in_container.sh` (not run automatically).
- The script `/workspace/bitcoin-ipc/scripts/miner.sh` is started automatically by the entrypoint script.

## Layout Inside the Container

- `/workspace/bitcoin-ipc/` – bitcoin-ipc repo
- `/workspace/ipc/` – IPC repo
- `/root/.bitcoin/` – Bitcoin Core regtest data and config
- `/root/.ipc/` – IPC configs (validators/users)
- `/root/logs/` – monitor and provider log files (e.g. `monitor-validator1.log`, `provider-validator1.log`)

Binaries (in `PATH`): `monitor`, `provider`, `quickstart`, `ipc-cli`, `fendermint`, `bitcoind`, `bitcoin-cli`, Foundry tools.


## Exposed Ports
<!-- - **18443** – Bitcoin RPC -->
- **3030–3034** – Provider (validator1–5)
- **3040–3041** – Provider (user1–2)


## IPC build caching (separate image)

The IPC repo is built into a separate image to avoid rebuilding it when you make local changes in this repo.

Build it once (from repo root):

```bash
docker build -f docker-deploy-local/Dockerfile.ipc -t ipc-builder:latest .
```

Then build/run the main container normally:

```bash
docker-compose up --build
```

To rebuild the image from scratch (ignore cache):

```bash
docker-compose build --no-cache
docker-compose up
```
Persistent data (Bitcoin chain, IPC config in volumes) is unchanged; only the image is rebuilt.

## Monitor and provider logs

Log files live inside the container at `/root/logs/` (same volume as `/root`, so they persist across restarts):

- `monitor-validator1.log`, `provider-validator1.log` … `monitor-validator5.log`, `provider-validator5.log`
- `monitor-user1.log`, `provider-user1.log`, `monitor-user2.log`, `provider-user2.log`

**View logs from the host:**

```bash
# Stream one log
docker exec bitcoin-ipc tail -f /root/logs/monitor-validator1.log
```

## Running monitor and provider manually

The entrypoint script starts them automatically. To run in the foreground (e.g. in a separate terminal) use, while inside the container:

```bash
# Start monitor for validator1
monitor --env ~/.ipc/validator1/.env

# Start provider for validator1
provider --env ~/.ipc/validator1/.env
```

### List running monitor/provider processes

```bash
pgrep -a monitor || true
pgrep -a provider || true
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
