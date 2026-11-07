# ipc_tester

Scenarios on the full subnet (real ipc-cli, fendermint, provider, monitor, process kills and outages) are not run by a tester binary. They are one script, `ue_scenario.sh`, run inside the `bitcoin-ipc` container with the local devnet up.

## Requirements

The scenario starts none of its infrastructure. It needs Docker and the `bitcoin-ipc` devnet container running (bitcoind, the miner, the monitor and the providers, subnet B providers included), started fresh with `UE_ACTIVATION_HEIGHT=1 make local-up` after `make local-delete-data && make local-build`, with the Bitcoin regtest chain past 101 blocks (mined by the devnet miner) and the subnets up. The tests are written for 1 s blocks: `BLOCK_INTERVAL=1` (default 20 s).

The script runs inside the container, which needs Docker socket access, so that it can build `fendermint:latest` (skip with `UE_BUILD_IMAGE=0` if the image exists). `HOST_ADDR` must resolve from the containers to the host (default `host.docker.internal`). The script only checks `UE_ACTIVATION_HEIGHT` and stops if it is unset. The steps `create`, `bootstrap` and similar are one-shot: repeating them needs a fresh devnet. The caller (the test-runner) provides this state.

## Run

`docker-compose.local.yml` mounts `src/easy_tester/ipc_tester/ue_scenario.sh` as `/usr/local/bin/ue_scenario.sh`. Fresh devnet, from `bitcoin-ipc`:

```
make local-delete-data && make local-build && BLOCK_INTERVAL=1 UE_ACTIVATION_HEIGHT=1 make local-up
docker exec -e HOST_ADDR=<host address> bitcoin-ipc ue_scenario.sh [--group GROUP | --step STEP] [--state-dir DIR]
```

`ue_scenario.sh --help` lists groups and steps. Groups: `all` (default), `setup`, `deposits`, `lifecycle`, `steady`, `provider-outage`, `monitor-outage`, `replay`, `bootstrap-pair`, `pair-verify`.

`all` runs every step once, in order, on one devnet: create, bootstrap, spin_up, the read-only checks, deposits, the provider and monitor outages, the lifecycle from `join_v5` to `steady`, then the validator1 crash (`l2_replay`). It is the cheapest full run. Each step has a timeout of about three times a passing run at 1 s blocks; `UE_STEP_TIMEOUT=<seconds>` sets one value for all steps.

Missing prerequisites run automatically; completed ones reuse the saved state under `--state-dir` (default `UE_STATE_DIR`, else `/root/.ipc/test-round/ue`) on the same Bitcoin chain. A fresh devnet is required to repeat `create`, `bootstrap`, `spin_up`, `join_v5`, `exit` or a bootstrap helper. `UE_BUILD_IMAGE=0` reuses an existing `fendermint:latest` image.

`bootstrap-pair` needs its own fresh devnet and runs before either L2 starts. The step `subnet_b_cli_routing` is in no group: its join check needs subnet B active, as every join is refused in epoch 0. Once subnet B's L2 runs and has its first checkpoint, it runs on the same devnet and state (it waits up to 90 s for activation):

```
docker exec -e HOST_ADDR=<host address> bitcoin-ipc ue_scenario.sh --step subnet_b_cli_routing
```

## Sign-record reader

`spec_eots_sign_record.rs` is a cargo integration test (`[[test]] spec_eots_sign_record`) used as a read-only helper: it dumps the signed EOTS votes of a heed database (`EOTS_SIGN_RECORD_DB`, `EOTS_SIGN_RECORD_OUTPUT`). The step `provider_outage` runs it as `/tmp/spec-eots-sign-record` in the container, or as the executable that `UE_SIGN_RECORD_READER` names. The scenario does not build it, as the container has no source tree. A run that selects `provider_outage` stops before its first step when the reader is missing or does not start.

The caller builds it in the container after `make local-up`, from `bitcoin-ipc` on the host:

```
docker exec bitcoin-ipc mkdir -p /tmp/bitcoin-ipc-src
```

```
tar --exclude=./target --exclude=./venv -cf - . | docker exec -i bitcoin-ipc tar -xf - -C /tmp/bitcoin-ipc-src
```

```
docker exec -w /tmp/bitcoin-ipc-src bitcoin-ipc sh -c 'cargo test --no-run --test spec_eots_sign_record --message-format=json | jq -r "select(.target.name == \"spec_eots_sign_record\") | .executable // empty" | xargs -I{} install -m 755 {} /tmp/spec-eots-sign-record'
```

Built in the container, the reader needs only the container's libc, libm and libgcc_s. A build on the host links the host's glibc and does not start in the container when that glibc is newer: an Ubuntu 24.04 build (glibc 2.39) fails in the container (Debian bookworm, glibc 2.36) with `GLIBC_2.39 not found`. The container keeps `/tmp` until it is recreated. `docker cp` copies the built reader out and into a later container of the same image.
