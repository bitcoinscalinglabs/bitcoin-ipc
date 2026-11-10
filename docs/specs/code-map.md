# Code map

Where the code lives and the interfaces between its parts. Paths are relative to the directory that holds the `ipc` and `bitcoin-ipc` repositories. Function names, not line numbers: line numbers drift.

## Interfaces

1. **Bitcoin L1 → monitor:** transaction encoding (OP_RETURN tags, postcard witness data) in `bitcoin-ipc/src/ipc_lib.rs`, defined by [encoding.md](encoding.md) and `transactions/*.md`.
2. **Monitor → provider:** the LMDB database (`bitcoin-ipc/src/db.rs`). The provider opens it read-only, so the provider's own state (the EOTS sign record) lives in files next to its keys.
3. **Provider ↔ ipc:** JSON-RPC. Server: `make_rpc_server` in `bitcoin-ipc/src/provider/rpc.rs` registers each method with `.with_method`. Client: `BtcSubnetManager` in `ipc/ipc/provider/src/manager/btc/manager.rs`, with hand-built `json!` bodies. Method names and parameter and result JSON are the contract between the repositories.
4. **Solidity → Rust:** contracts in `ipc/contracts/contracts/`, bindings by `make gen`, called through `GatewayCaller` in `ipc/fendermint/vm/interpreter/src/fvm/state/ipc.rs`.
5. **Node ↔ node:** vote gossip (`VoteRecord` envelope in `ipc/ipld/resolver/src/vote_record.rs`, topic in `behaviour/membership.rs`) and consensus messages in proposals (`ChainMessage`, `IpcMessage` in `ipc/fendermint/vm/message/src/`).
6. **Fendermint → Ethereum JSON-RPC API:** a separate process (`fendermint/app/src/cmd/eth.rs`) that reads through CometBFT RPC and ABCI queries only.

Information moves between Bitcoin L1 and the subnet only through the existing top-down path (the parent finality commit: top-down messages and configuration updates) and bottom-up path (checkpoints), with no new provider method for that. Fendermint calls the provider for the reads that these paths, EOTS finality and slash pre-signatures need, and to sign with a key the provider holds (validator key, EOTS key). The calls are in [finality and checkpoints](../architecture/finality-and-checkpoints.md).

## Areas

**A. bitcoin-ipc library and monitor** (`bitcoin-ipc/src/`)
- `ipc_lib.rs`: message types, wire formats, `validate`/`process`/`save_to_db` per message. Batch transfer: `BatchTransferWireData`, `IpcBatchTransferMsg`. Checkpoint: `IpcCheckpointSubnetMsg` (`slash_pre_signature_count`). Bootstrap: `IpcJoinSubnetMsg::save_to_db`.
- `collateral.rs`: collateral script, `check_join_ue` (with the `JoinContext` of rule 2.6), `process_join_ue`, `record_collateral_spends`.
- `epochs.rs`: `process_cutoffs` (sets `H_0` once `genesis_collaterals_slashable`, computes `C_1`, releases the configuration updates, activates), `compute_configuration` (the configuration of an epoch), `configuration_changes`.
- `multisig.rs`: configuration multisig scripts, PSBT signing and finalizing.
- `eots.rs`, `slash.rs`: EOTS nonces and Merkle tree, slash transaction and its pre-signatures.
- `db.rs`: heed tables (`MAX_DBS`), records `SubnetState`, `SubnetCommittee`, `SubnetCheckpoint`, `UeCollateral`.
- `bin/monitor.rs`: Bitcoin block loop, `process_transaction`, `process_ipc_msg`.
- Checks: `cargo build` and `cargo test` for every feature combination, debug and `--release`, db-tester scenarios.

**B. bitcoin-ipc provider** (`bitcoin-ipc/src/provider/`, `src/bin/provider.rs`)
- `rpc.rs`: every JSON-RPC method, `ServerData` (database, Bitcoin RPC, validator key). Checkpoint building: `signing_configuration`, `plan_checkpoint` (fund UTXOs, fee), `carried_slash_entries` (from the slash pre-signatures that the checkpoint carries), `get_unslashable_outputs`, `latest_checkpoint_at`.
- `eots_signer.rs`: EOTS signing and the sign record. The commitment trees are rebuilt from the nonce seed at start (`build_nonce_trees` in `bin/provider.rs`) and never stored.
- Keys from the environment: `VALIDATOR_SK_PATH`, `EOTS_SK_PATH`, `EOTS_SEED_PATH` (`load_validator`, `load_hex32` in `bin/provider.rs`).
- Checks: as A, plus monitor-tester scenarios (one provider per validator).

**C. ipc client side** (`ipc/ipc/`)
- `provider/src/manager/btc/manager.rs`: provider client and checkpoint transaction building (`get_checkpoint_transaction` maps checkpoint messages to `gencheckpointpsbt`).
- `provider/src/checkpoint.rs`: relayer (`BottomUpCheckpointManager`, `submit_next_epoch`).
- `provider/src/lib.rs`: `IpcProvider` (join, keys). `cli/src/commands/`: ipc-cli commands.
- `api/src/`: shared types (`checkpoint.rs`, `subnet.rs`, `eots.rs`). `wallet/src/evm/btc_keys.rs`: Bitcoin keys.
- `eots/`: EOTS verification crate shared by the client side and Fendermint.
- Checks: `cargo test -p` for each touched crate.

**D. Contracts** (`ipc/contracts/contracts/`)
- `gateway/router/CheckpointingFacet.sol`: checkpoints, Bitcoin signatures, slash pre-signatures, `setSubnetFinalHeight` (system actor, keeps the maximum). `GatewayGetterFacet.sol`: subnet final height getter. `structs/Quorum.sol`, `lib/LibQuorum.sol`: `quorumSignerWeights` (`getBitcoinCheckpointSignatures` returns the PSBT whose signers carry the most weight). `lib/LibGateway.sol`: `commitBottomUpMsg`.
- `lib/LibGatewayActorStorage.sol`: gateway storage. `structs/Bitcoin.sol`, `structs/CrossNet.sol`: structs (`BottomUpCheckpoint.slashPreSignatureCount`).
- Checks: `make gen`, then `cargo check -p fendermint_app`.

**E. Fendermint** (`ipc/fendermint/`)
- `app/src/app.rs`: ABCI handlers (`prepare_proposal`, `process_proposal`, `begin_block`, `end_block`, `commit`).
- `app/src/cmd/run.rs`: wiring of keys, rocksdb namespaces (`namespaces!`), vote loops, resolver event dispatch (`dispatch_vote`), `make_ipc_provider`.
- `app/src/ipc.rs`: `AppVote`. `app/src/eots.rs`: EOTS vote loop (`vote_loop`), store, finality query.
- `app/src/validators.rs`: proposer lookup.
- `vm/topdown/src/voting.rs`: `VoteTally` (top-down finality). `vm/topdown/src/eots.rs`: EOTS tally (`votes_to_cast`, `update_power_table`, and `mark_cast`, after which a height counts as voted once its EOTS vote is signed and published).
- `vm/topdown/src/finality/fetch.rs`: parent finality reads (`wait_for_confirmed_height`, retried reads with capped back-off).
- `vm/interpreter/src/chain.rs`: `prepare`/`process`/`deliver` of consensus messages (`at_most_one_eots_finality`, the subnet final height written when `EotsFinality` is delivered, `end` installing a validator update at `h + 2` in the EOTS tally).
- `vm/interpreter/src/fvm/exec.rs` (`end`): calls checkpoint creation, signing and slash pre-signature posting at the end of a subnet block.
- `vm/interpreter/src/fvm/checkpoint.rs`: checkpoint creation (`maybe_create_checkpoint`, `should_create_checkpoint`), first signing (`newly_final_checkpoints`, `newly_final_heights`), re-signing (`checkpoints_to_resign`, `resign_plan`, `final_heights_above`, `unsigned_checkpoints`), signature broadcast (`broadcast_signatures`, `broadcast_checkpoint_signatures`, `broadcast_signature`, `broadcast_bitcoin_signature`), slash pre-signatures (`post_slash_pre_signatures`, `outputs_to_presign` through `getunslashableoutputs`, then `signslash` and the gateway's `addSlashPreSignature`).
- `vm/interpreter/src/fvm/state/ipc.rs`: `GatewayCaller` (`subnet_final_height`, `set_subnet_final_height`, `has_bitcoin_checkpoint_signature`, `slash_pre_signature_count`, `slash_pre_signatures`, `add_slash_pre_signature_calldata`, `last_applied_change_number`, `last_power_table`).
- `vm/message/src/ipc.rs`: `IpcMessage`.
- Checks: `cargo test -p` for each touched crate, release build of fendermint and ipc-cli.

**F. Ethereum JSON-RPC API** (`ipc/fendermint/eth/api/src/`)
- `state.rs`: block tags (`block_by_height`, `header_by_height`, `query_height`). `apis/eth.rs`: methods.

## easy_tester

- `cargo run --bin easy_tester -- --scenario <file> --tester <config>`. Scenarios in `bitcoin-ipc/src/easy_tester/scenaria/`, tester configurations in `testers/`, format in its `README.md`.
- The db tester drives the monitor logic on the database. The monitor tester runs bitcoind, monitors and providers. Neither runs Fendermint.
