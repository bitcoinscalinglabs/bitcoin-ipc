# BSL validator node

Runs a validator for a BSL subnet next to the BSL client: fendermint, CometBFT, the
Ethereum JSON-RPC API and a checkpoint relayer, all configured from `bsl.env`.

Prerequisites: Docker Compose v2 on an amd64 host, the `bsl.env` from the BSL operator,
and a validator key that has joined the subnet with the BSL client (its address goes in
`NODE_VALIDATOR_ADDR`). The compose file takes over the client, so stop the one started
with `docker run` first; its volumes are reused.

```bash
docker rm -f bsl-client
docker compose --env-file bsl.env up -d
docker compose --env-file bsl.env logs -f cometbft   # state sync, then block sync; done when catching_up = false
curl -s localhost:26657/status | grep catching_up
```

Update: `docker compose --env-file bsl.env pull && docker compose --env-file bsl.env up -d`.

`bsl.env` variables used here, besides the client's own:

| Variable | Meaning |
|---|---|
| `NODE_SUBNET_ID` | subnet to validate |
| `NODE_VALIDATOR_ADDR` | `0x…` address of the key that joined (`ipc-cli wallet new --wallet-type btc`) |
| `NODE_PEERS` | CometBFT `persistent_peers` of the BSL validators |
| `NODE_RESOLVER_BOOTSTRAPS` | fendermint resolver multiaddrs of the BSL validators |
| `NODE_RPC_SERVERS` | two or more CometBFT RPCs used for state sync, comma-separated |
| `NODE_MONIKER` | optional node name |

Never run two nodes with the same validator key.

Checkpoint signatures are transactions on the subnet, so the validator address needs a
balance there: send it at least 1 subnet token (a cross-subnet transfer or a plain
transfer from a funded address) before it signs. The signature itself is free, but it is
pre-validated against the gas limit. Until then fendermint logs `error broadcasting
checkpoint signature` at every checkpoint.

Ports on the host, localhost only: 3030 (BSL provider), 26657 (CometBFT RPC), 8545
(Ethereum JSON-RPC). The node only dials out; it works behind NAT without any inbound port.

Data: volume `bsl-node` (node keys, genesis, chain data) plus the client's `bsl-client-db`
and `bsl-client-keys`, which compose never removes. `docker compose down` keeps everything;
`docker volume rm bsl-node` starts the node over.
