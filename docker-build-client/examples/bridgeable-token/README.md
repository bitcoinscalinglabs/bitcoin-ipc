# BridgeableToken example

A minimal ERC-20 that can be moved between BSL subnets. It extends OpenZeppelin `ERC20`,
`ERC20Burnable`, and `Ownable` — cross-subnet registration requires an `Ownable` token, since
`GatewayErcFacet.registerBridgeableToken` only accepts a token whose `owner()` is the caller.

A Foundry project with the contract (`src/BridgeableToken.sol`) and its OpenZeppelin
dependency (`lib/`) vendored. `deploy.sh` compiles it, deploys it, and registers it on the
gateway in one step. See `cross-subnet.md` for the full walkthrough and the transfer that
follows.

```bash
./deploy.sh \
  --rpc-url <subnet-rpc-url> --private-key <key> --gateway <gateway-addr> \
  --name MyToken --symbol MTK --decimals 18 \
  --initial-supply 1000000000000000000000000 --broadcast
```
