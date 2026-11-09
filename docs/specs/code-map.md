# Code map

Where the code lives and the interfaces between its parts.
Paths are relative to the directory that holds the `ipc` and `bitcoin-ipc` repositories. Function names, not line numbers: line numbers drift.

## Interfaces

1. **Bitcoin L1 → monitor:** transaction encoding (OP_RETURN tags, postcard witness data) in
   `bitcoin-ipc/src/ipc_lib.rs`. The spec `bitcoin-ipc/docs/specs/encoding.md` and
   `transactions/*.md` define it.
2. **Monitor → provider:** the LMDB database (`bitcoin-ipc/src/db.rs`). The provider opens it
   read-only, so provider-owned state (the EOTS sign record) lives in files next to its keys.
3. **Provider ↔ ipc:** JSON-RPC. Server: `make_rpc_server` in `bitcoin-ipc/src/provider/rpc.rs`
   registers each method with `.with_method`. Client: `BtcSubnetManager` in
   `ipc/ipc/provider/src/manager/btc/manager.rs`, with hand-built `json!` bodies per method. Method
   names and parameter/result JSON are the contract between the repos.
4. **Solidity → Rust:** contracts in `ipc/contracts/contracts/`, bindings by `make gen`, called
   through `GatewayCaller` in `ipc/fendermint/vm/interpreter/src/fvm/state/ipc.rs`.
5. **Node ↔ node:** vote gossip (`VoteRecord` envelope in `ipc/ipld/resolver/src/vote_record.rs`,
   topic in `behaviour/membership.rs`) and consensus messages in proposals (`ChainMessage`,
   `IpcMessage` in `ipc/fendermint/vm/message/src/`).
6. **Fendermint → Ethereum JSON-RPC API:** a separate process (`fendermint/app/src/cmd/eth.rs`)
   that reads through CometBFT RPC and ABCI queries only.

New functionality moves information between Bitcoin L1 and the subnet through the existing
top-down path (the parent finality commit: top-down messages and configuration updates) and the
existing bottom-up path (checkpoints). It adds no provider method for that. Fendermint calls the
provider for the reads that these paths, EOTS finality and slash pre-signatures need, and to sign
with a key the provider holds (validator key, EOTS key). The calls are in
[finality and checkpoints](../architecture/finality-and-checkpoints.md#4-interfaces).

## Areas

**A. bitcoin-ipc library and monitor** (`bitcoin-ipc/src/`)
- `ipc_lib.rs`: message types, wire formats, `validate`/`process`/`save_to_db` per message.
  Batch transfer: `BatchTransferWireData`, `IpcBatchTransferMsg`; checkpoint: `IpcCheckpointSubnetMsg`
  (`slash_pre_signature_count`). Bootstrap: `IpcJoinSubnetMsg::save_to_db`.
- `collateral.rs`: unilateral-exit collateral script, `check_join_ue` (with the `JoinContext`
  of rule 2.6), `process_join_ue`, `record_collateral_spends`.
- `epochs.rs`: the cutoff pass (`process_cutoffs`: sets `H_0` at the first Bitcoin height at which
  `genesis_collaterals_slashable`, computes `C_1`, releases the configuration updates, activates), `compute_configuration` (the configuration of an epoch from
  the slashable collaterals and the multisig members), `configuration_changes`.
- `multisig.rs`: configuration multisig scripts, PSBT signing and finalizing.
- `eots.rs`, `slash.rs`: EOTS nonces and Merkle tree, slash transaction and its pre-signatures.
- `db.rs`: heed tables (`MAX_DBS`), records `SubnetState`, `SubnetCommittee`, `SubnetCheckpoint`, `UeCollateral`.
- `bin/monitor.rs`: block loop, `process_transaction`, `process_ipc_msg`.
- Checks: `cargo build` and `cargo test` for every feature combination, debug and `--release`, db-tester scenarios.

**B. bitcoin-ipc provider** (`bitcoin-ipc/src/provider/`, `src/bin/provider.rs`)
- `rpc.rs`: every JSON-RPC method, `ServerData` (db, Bitcoin RPC, validator key). Checkpoint
  planning: `signing_configuration`, `plan_checkpoint` (fund UTXOs, fee), `carried_slash_entries`
  (the reveal takes slash pre-signatures from the first `N` entries); `get_unslashable_outputs`,
  `latest_checkpoint_at`.
- `eots_signer.rs`: EOTS signing and the sign record; the nonce trees are rebuilt from the seed
  at start (`build_nonce_trees` in `bin/provider.rs`), nothing of them is stored.
- Keys from env: `VALIDATOR_SK_PATH`, `EOTS_SK_PATH`, `EOTS_SEED_PATH` (`load_validator`,
  `load_hex32` in `bin/provider.rs`).
- Checks: as A, plus monitor-tester scenarios (they start one provider per validator).

**C. ipc client side** (`ipc/ipc/`)
- `provider/src/manager/btc/manager.rs`: provider client, checkpoint transaction building
  (`get_checkpoint_transaction` maps checkpoint messages to `gencheckpointpsbt`).
- `provider/src/checkpoint.rs`: relayer (`BottomUpCheckpointManager`, `submit_next_epoch`).
- `provider/src/lib.rs`: `IpcProvider` (join, keys). `cli/src/commands/`: ipc-cli commands.
- `api/src/`: shared types (`checkpoint.rs`, `subnet.rs`, `eots.rs`). `wallet/src/evm/btc_keys.rs`: BTC keys.
- `eots/`: EOTS verification crate shared by the client side and Fendermint.
- Checks: `cargo test -p` for each touched crate.

**D. Contracts** (`ipc/contracts/contracts/`)
- `gateway/router/CheckpointingFacet.sol`: checkpoints, Bitcoin checkpoint signatures, slash
  pre-signatures, `setSubnetFinalHeight` (system actor, keeps the maximum). `GatewayGetterFacet.sol`: subnet final
  height getter. `structs/Quorum.sol`, `lib/LibQuorum.sol`: `quorumSignerWeights`
  (`getBitcoinCheckpointSignatures` returns the PSBT whose signers carry the most weight).
  `lib/LibGateway.sol`: `commitBottomUpMsg`.
- `lib/LibGatewayActorStorage.sol`: gateway storage. `structs/Bitcoin.sol`, `structs/CrossNet.sol`: structs (`BottomUpCheckpoint.slashPreSignatureCount`).
- Checks: `make gen`, then `cargo check -p fendermint_app`.

**E. Fendermint** (`ipc/fendermint/`)
- `app/src/app.rs`: ABCI handlers (`prepare_proposal`, `process_proposal`, `begin_block`, `end_block`, `commit`).
- `app/src/cmd/run.rs`: wiring: keys, rocksdb namespaces (`namespaces!`), vote loops, resolver
  event dispatch (`dispatch_vote`), `make_ipc_provider`.
- `app/src/ipc.rs`: `AppVote`. `app/src/eots.rs`: EOTS vote loop (`vote_loop`), store, finality query.
- `app/src/validators.rs`: proposer lookup, built from the current and the previous configuration of the gateway.
- `vm/topdown/src/voting.rs`: `VoteTally` (top-down finality). `vm/topdown/src/eots.rs`: EOTS tally
  (`votes_to_cast`, `mark_cast`: a height counts as voted once its EOTS vote is signed and published; `update_power_table`).
- `vm/topdown/src/finality/fetch.rs`: parent finality reads (`wait_for_confirmed_height`, retried reads with capped back-off).
- `vm/interpreter/src/chain.rs`: `prepare`/`process`/`deliver` of consensus messages
  (`at_most_one_eots_finality`; deliver of `EotsFinality` writes the subnet final height; `end` installs a validator update at `h + 2` in the EOTS tally).
- `vm/interpreter/src/fvm/exec.rs` (`end`): calls the checkpoint creation, signing and slash pre-signature functions at the end of a subnet block.
- `vm/interpreter/src/fvm/checkpoint.rs`: checkpoint creation (`maybe_create_checkpoint`,
  `should_create_checkpoint`: the checkpoint of `H` at the end of subnet block `H`); first signing
  (`newly_final_checkpoints`, `newly_final_heights`); re-signing at multiples of the period
  (`checkpoints_to_resign`, `resign_plan`, `final_heights_above`, `unsigned_checkpoints`);
  signature broadcast to the gateway (`broadcast_signatures`, `broadcast_checkpoint_signatures`,
  `broadcast_signature`, `broadcast_bitcoin_signature`); slash pre-signatures
  (`post_slash_pre_signatures`, `outputs_to_presign` via `getunslashableoutputs`, then `signslash` and the gateway's `addSlashPreSignature`).
- `vm/interpreter/src/fvm/state/ipc.rs`: `GatewayCaller` (`subnet_final_height`,
  `set_subnet_final_height`, `has_bitcoin_checkpoint_signature`, `slash_pre_signature_count`,
  `slash_pre_signatures`, `add_slash_pre_signature_calldata`, `last_applied_change_number`, `last_power_table`).
- `vm/message/src/ipc.rs`: `IpcMessage`.
- Checks: `cargo test -p` for each touched crate, release build of fendermint and ipc-cli.

**F. Ethereum JSON-RPC API** (`ipc/fendermint/eth/api/src/`)
- `state.rs`: block tags (`block_by_height`, `header_by_height`, `query_height`). `apis/eth.rs`: methods.

## easy_tester

- `cargo run --bin easy_tester -- --scenario <file> --tester <config>`. Scenarios in
  `bitcoin-ipc/src/easy_tester/scenaria/`, tester configs in `testers/`, format in its `README.md`.
- The db tester drives the monitor logic on the database. The monitor tester runs bitcoind,
  monitors and providers. Neither runs Fendermint.
