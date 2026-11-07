# Bootstrap subnet A

### 1. Fresh devnet (own terminal, stays in the foreground)

```bash
cd <bitcoin-ipc-checkout>
```

Remove old validator containers:

```bash
docker ps \
  -aq \
  --filter 'name=^validator-' | xargs -r docker rm -f
```

Wipe the devnet data:

```bash
make local-delete-data
```

Start the devnet: the single `bitcoin-ipc` container. It runs bitcoind in regtest, `quickstart` once to write the configuration under `~/.ipc/`, a miner that mines one block every 20 seconds, a monitor and a provider for each of validator1–6, user1 and user2, and a second provider per validator for subnet B (ports 3130–3135). No subnet exists yet.

`<host-addr>`: `host.docker.internal` with Docker Desktop, the host's own interface address with Docker on Linux, rootless or not.

```bash
HOST_ADDR=<host-addr> make local-up
```

### 2. Wait until `result` is 101 or more (second terminal)

Query the confirmed block count:

```bash
docker exec bitcoin-ipc curl \
  -s \
  http://127.0.0.1:3135/api \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer validator6_auth_token' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getconfirmedcount","params":null}'
```

### 3. Shell in the container

```bash
docker exec -it bitcoin-ipc bash
```

Show ipc-cli's output:

```bash
export RUST_LOG=info
```

### 4. Create the subnet

Create the subnet through validator 1's provider:

```bash
ipc-cli --config-path ~/.ipc/validator1/config.toml subnet create \
  --parent /b4 \
  --min-validators 4 \
  --bottomup-check-period 30 \
  --active-validators-limit 10 \
  btc \
  --min-validator-stake 100000000 \
  --power-unit-exponent 7 \
  --min-cross-msg-fee 10 \
  --validator-whitelist 5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268,b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0,851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4,b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529 \
  --epoch-length 60 \
  --min-timeout 1 \
  --max-timeout 2000 \
  --min-deposit 330 \
  --kill-vote-period 36 \
  --kill-delay-period 5
```

Copy the subnet id from the output (`created subnet with id: /b4/t410f…`):

```bash
export SUBNET_ID=<subnet-id>
```

Record the creation time:

```bash
CREATED_AT=$(date -u +"%Y-%m-%d %H:%M:%SZ")
```

Mine one block, wait a few seconds before the next command:

```bash
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)"
```

Record its height:

```bash
CREATED_HEIGHT=$(bitcoin-cli getblockcount)
```

### 5. Join validators 1 to 4

Join validator 1:

```bash
ipc-cli --config-path ~/.ipc/validator1/config.toml subnet join-ue \
  --from 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 \
  --subnet "$SUBNET_ID" \
  --collateral 200000000 \
  --ip 66.222.44.55:8080 \
  --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n \
  --epochs 1600 \
  --eots-nonce-start 1 \
  --eots-nonce-count 500000
```

Join validator 2:

```bash
ipc-cli --config-path ~/.ipc/validator2/config.toml subnet join-ue \
  --from 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 \
  --subnet "$SUBNET_ID" \
  --collateral 110000000 \
  --ip 66.222.44.55:8081 \
  --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n \
  --epochs 1600 \
  --eots-nonce-start 1 \
  --eots-nonce-count 500000
```

Join validator 3:

```bash
ipc-cli --config-path ~/.ipc/validator3/config.toml subnet join-ue \
  --from 0x646Aed5404567ae15648E9b9B0004cbAfb126949 \
  --subnet "$SUBNET_ID" \
  --collateral 150000000 \
  --ip 66.222.44.55:8082 \
  --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n \
  --epochs 1600 \
  --eots-nonce-start 1 \
  --eots-nonce-count 500000
```

Join validator 4:

```bash
ipc-cli --config-path ~/.ipc/validator4/config.toml subnet join-ue \
  --from 0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf \
  --subnet "$SUBNET_ID" \
  --collateral 180000000 \
  --ip 66.222.44.55:8083 \
  --backup-address bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n \
  --epochs 1600 \
  --eots-nonce-start 1 \
  --eots-nonce-count 500000
```

Mine one block, wait a few seconds before the next command:

```bash
bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)"
```

### 6. Fund the subnet accounts

Fund validators 1 to 4 and users 1 and 2:

```bash
ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0x27B60D9f71D6806cCa7D5A92b391093FE100f8e8 \
  210000000

ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0xd9c4C92CA843a53bff146C79B5D32Ca4b9321414 \
  220000000

ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0x646Aed5404567ae15648E9b9B0004cbAfb126949 \
  230000000

ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf \
  240000000

ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0x005e05dd763dd125473f8889726f7c305e50fcae \
  200000000

ipc-cli --config-path ~/.ipc/validator1/config.toml cross-msg fund \
  --subnet "$SUBNET_ID" \
  btc \
  --to 0xa78bc5d61e0da3c2d96e29a495f4e358d8d2218d \
  200000000
```

Mine three blocks, wait a few seconds before the next command:

```bash
bitcoin-cli generatetoaddress 3 "$(bitcoin-cli -rpcwallet=default getnewaddress)"
```

### 7. Add the subnet to the ipc-cli config files

Append subnet A to each config:

```bash
for f in ~/.ipc/config.toml ~/.ipc/{validator1,validator2,validator3,validator4,user1,user2}/config.toml; do
  case "$f" in
    *validator2*) port=8645 ;;
    *validator3*) port=8745 ;;
    *validator4*) port=8845 ;;
    *) port=8545 ;;
  esac
  cat >> "$f" <<TOML

# Subnet A. Created on ${CREATED_AT} at height ${CREATED_HEIGHT}
[[subnets]]
id = "${SUBNET_ID}"

[subnets.config]
network_type = "fevm"
provider_http = "http://${HOST_ADDR}:${port}/"
gateway_addr = "0x77aa40b105843728088c0132e43fc44348881da8"
registry_addr = "0x74539671a1d2f1c8f200826baba665179f53a1b7"
TOML
done
```

Next: [spin_up_subnet_a.md](spin_up_subnet_a.md).
