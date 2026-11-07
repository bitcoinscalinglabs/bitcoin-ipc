# Running the BSL client

Every participant of a subnet, the creator and each validator, runs the BSL client before the subnet exists. The client is the Docker image `ghcr.io/bitcoinscalinglabs/bsl-client`. Its container runs three programs:

- **monitor**: reads final Bitcoin blocks and derives the state of every subnet into a local database.
- **provider**: serves that state over JSON-RPC on port 3030 and builds, signs and submits the participant's Bitcoin transactions.
- **ipc-cli**: the command-line tool, run with `docker exec`. It calls the provider.

The image is built from [`docker-build-client/`](../../docker-build-client/). Its only published tag is `testnet`.

## Requirements

- Docker.
- A Bitcoin Core full node of the chosen network (mainnet, testnet or regtest) with `txindex=1`, whose RPC the container reaches.
- A funded wallet on that node, named by `FUND_WALLET`. It pays the fees, and for a validator also the collateral. The client loads the wallet but does not create it.
- For a validator: a validator key in the client ([validator key](#validator-key)).

## Configuration

The client reads its settings from a file `bsl.env`. Example for a node reached directly over RPC:

```
BITCOIN_CONN_MODE=direct
RPC_URL=http://<node-host>:<rpc-port>
RPC_USER=<rpc-user>
RPC_PASS=<rpc-password>
FUND_WALLET=<wallet-name>
PROVIDER_AUTH_TOKEN=<provider-auth-token>
UE_ACTIVATION_HEIGHT=<height>
ACTIVATION_HEIGHT=<height>
SNAPSHOT_LENGTH=<blocks>
SUBNET_ID=<reward-subnet-id>
```

| Variable | Meaning |
|---|---|
| `BITCOIN_CONN_MODE` | `direct`: RPC at `RPC_URL`. `ssh-tunnel` (default): RPC through an SSH tunnel to `SSH_HOST` |
| `RPC_URL` | the node's RPC address, in `direct` mode |
| `SSH_HOST`, `SSH_USER` | the tunnel's host and user, in `ssh-tunnel` mode. The tunnel reaches RPC port 18443 on `SSH_HOST`. The tunnel key is mounted at `/run/tunnel_key` |
| `RPC_USER`, `RPC_PASS` | the node's RPC credentials |
| `FUND_WALLET` | the node's wallet that pays fees, collaterals and deposits |
| `PROVIDER_AUTH_TOKEN` | the bearer token of the provider's JSON-RPC, a random secret such as the output of `openssl rand -hex 32` |
| `UE_ACTIVATION_HEIGHT` | the Bitcoin height from which monitors accept joins. Unset: no join. Every client of a deployment uses the same value |
| `ACTIVATION_HEIGHT`, `SNAPSHOT_LENGTH` | the reward parameters of the deployment. Every client of a deployment uses the same values |
| `SUBNET_ID` | the deployment's reward subnet, `<prefix>/<address>`. `ipc-cli` takes its network prefix as the parent |
| `SUBNET_ID2`, `URL`, `URL2`, `TOKEN`, `GATEWAY_ADDR`, `REGISTRY_ADDR` | optional. For `SUBNET_ID` with `URL`, and for `SUBNET_ID2` with `URL2`, the client adds an `ipc-cli` entry for that subnet with EVM endpoint `URL` + `TOKEN` (or `URL2` + `TOKEN`). Commands on the subnet and the relayer need it |
| `MONITOR_SYNC_BATCH_SIZE`, `RUST_LOG` | optional. Defaults: `1000`, info logs |

`PROVIDER_PORT` and `DATABASE_URL` keep their defaults, `3030` and `/var/lib/bsl/db`, which the port mapping and the database volume below use.

## Start

```bash
docker run -d --name bsl-client --restart unless-stopped \
  --env-file bsl.env \
  -v bsl-client-db:/var/lib/bsl/db \
  -v bsl-client-keys:/root/.ipc \
  -p 127.0.0.1:3030:3030 \
  ghcr.io/bitcoinscalinglabs/bsl-client:testnet
```

In `ssh-tunnel` mode, add `-v <tunnel-key-path>:/run/tunnel_key:ro`.

The volume `bsl-client-db` holds the monitor's database, `bsl-client-keys` the `ipc-cli` keystore. `docker rm -f bsl-client` stops the client and keeps both.

## Check

```bash
docker ps --filter name=bsl-client
docker logs --tail 20 bsl-client
```

The logs show `==> starting monitor`, `==> starting provider`, then the monitor's `Sync completed` and one `Processed block <n>` per new final block.

```bash
curl -s -X POST http://127.0.0.1:3030/api \
  -H 'Authorization: Bearer <provider-auth-token>' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getconfirmedcount"}'
curl -s -X POST http://127.0.0.1:3030/api \
  -H 'Authorization: Bearer <provider-auth-token>' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getblockcount"}'
```

`getconfirmedcount` returns the last height the monitor has processed, `getblockcount` the node's tip. The monitor has caught up when the difference is `CONFIRMATIONS` ([network parameters](../specs/lifecycle.md#network-parameters)).

## Validator key

A validator creates its validator key in the client and gives it to the provider, which signs the validator's join and checkpoints with it.

```bash
docker exec bsl-client ipc-cli wallet new --wallet-type btc
```

Output: `Created IPC wallet with address: <validator-address>`.

```bash
docker exec bsl-client ipc-cli wallet list --wallet-type btc | grep -v 'Secret key'
```

`XonlyPubKey` is the validator public key, the key that goes into a subnet's whitelist.

```bash
docker exec bsl-client ipc-cli wallet export --wallet-type btc --address <validator-address> --hex --output /root/.ipc/validator.sk
docker restart bsl-client
docker logs bsl-client 2>&1 | grep 'provider signs with'
```

The last command prints `==> provider signs with /root/.ipc/validator.sk`. The key stays in the volume `bsl-client-keys`.
