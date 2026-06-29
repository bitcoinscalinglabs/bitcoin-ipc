# bsl-client image

Docker image for interacting with the Bitcoin-IPC framework. It reads the framework's
state from Bitcoin — subnets, their validators, stakes, and checkpoints — and can act
on it, for example joining a subnet as a validator. It reaches a Bitcoin node over an
SSH tunnel (currently Bitcoin Core on the Regtest testnet only).

The container runs three processes:

- **monitor** — continuously reads the Bitcoin chain and reconstructs the Bitcoin-IPC
  state (subnets, validators, collateral, checkpoints, IPC-BTC rewards) into a local
  database.
- **provider** — serves that state over a local JSON-RPC API (port 3030) and builds the
  Bitcoin transactions Bitcoin-IPC needs, such as joining a subnet.
- **ipc-cli** — command-line tool (run via `docker exec`) to query a subnet and act on
  it; it talks to the provider.

`monitor` and `provider` are built from this repo, `ipc-cli` from the ipc repo. The
image is built multi-arch from both source trees and pushed to
`ghcr.io/bitcoinscalinglabs/bsl-client` by `../.github/workflows/bsl-client-image.yml`.
