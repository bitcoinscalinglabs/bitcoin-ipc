# Slashing

A validator that votes for two different subnet blocks at the same height *equivocates*. It loses every collateral UTXO and unbonding output it still holds to [slash transactions](transactions/slash.md). Conditions on the parameters: [instantiating the parameters](lifecycle.md#instantiating-the-parameters).

The subnet's consensus engine (CometBFT) commits a subnet block with signatures under the validator keys, which are not slashable. An *EOTS vote* is a second signature on the subnet block, under the EOTS key, and is slashable. A *consumer* is any program or person that reads subnet blocks, state or results: checkpoint creation, the relayer, wallets, explorers, indexers. Consumers must rely on [final subnet blocks](#final-subnet-blocks) only.

## EOTS key and EOTS nonce commitments

Every validator specifies once per subnet an EOTS key `pk_eots` ([EOTS](encoding.md#notation)), different from its validator key, and an [EOTS nonce commitment](encoding.md#eots-nonce-commitment) covering one public nonce for each subnet height of a range. Its [join transaction](transactions/join.md) carries both. The validator must keep its ranges covering every height at which it votes, from its first seat on and without gaps, by adding EOTS nonce commitments with [stake transactions](transactions/stake.md). A height it cannot vote at counts as a missing vote. The monitor does not check coverage. It records the EOTS nonce commitments of accepted join and stake transactions, and a provider method returns, for an EOTS key, every recorded commitment `(start, count, root)`.

The validator generates its EOTS key locally, and its provider holds it ([command](ipc-cli.md#generate-the-eots-key)). It derives the secret nonce of each height with one HMAC-SHA256, keyed by a *nonce seed*, over the subnet's `txid20` and the height, so it stores no secret nonce and no commitment tree. The nonce seed is a random secret, separate from the EOTS key. The monitor checks only the commitment, so any derivation works if every secret nonce stays secret and is used once. One secret nonce and its EOTS vote give the EOTS key, so the validator keeps the seed as secret as the key, across restarts. A lost seed makes every committed public nonce unusable.

A validator must not sign two different messages at one height. Its provider keeps a record of every EOTS vote it signed above the [latest checkpointed height](#extending-the-latest-checkpointed-block) `h_c`, across restarts, and deletes the records at or below `h_c`.

## EOTS votes

EOTS votes are consensus messages of the subnet, not subnet transactions: they cost no fee and touch no contract. Every [IPC-aware node](modules.md#ipc-aware-node), validator or not, publishes and receives them on the subnet's vote gossip. Later subnet blocks carry them ([subnet final height](#subnet-final-height)).

### The vote

An EOTS vote is a vote for one subnet block `b` at one height `h`:

- The message is `m = tagged_sha256("IPC/vote", txid20(subnet) ‖ h ‖ b)`, with `h` a `u64` little-endian and `b` the 32-byte subnet block hash (`tagged_sha256` as in [encoding](encoding.md#notation)).
- The EOTS vote is the 64-byte BIP340 signature `R_h ‖ s` on `m` under `pk_eots`, made with the secret nonce `k_h` whose public nonce `R_h` the validator committed for `h`.
- The EOTS vote record holds `pk_eots`, `h`, `b`, the signature `R_h ‖ s`, and the proof of `R_h` against the root of the commitment whose range holds `h`: that commitment's `start` and the audit path. Encoding: [EOTS vote](encoding.md#eots-vote).
- The validator publishes the record inside the subnet's signed vote envelope (`VoteRecord`: the validator's key, the subnet id, the content, a timestamp, signed with the validator key).

### Casting an EOTS vote

Once the consensus engine has committed the subnet block `b` at height `h`, every validator of the validator set at `h` must cast an EOTS vote for `b`, unless the EOTS votes it holds already show `b` [final](#final-subnet-blocks). The *validator set at `h`* is the configuration that the consensus engine uses at `h` ([applying configuration updates in the subnet](lifecycle.md#applying-configuration-updates-in-the-subnet)): as in CometBFT, a configuration that the execution of subnet block `h` applies is in use from `h + 2`.

A validator must cast at most one EOTS vote per height, only for the subnet block its consensus engine committed there, and use every secret nonce at most once. It does not vote at a height that none of its EOTS nonce commitments covers, or where [extending the latest checkpointed block](#extending-the-latest-checkpointed-block) forbids it. A validator that has missed heights resumes above those whose subnet block it knows to be final, and votes at the rest as soon as it can, in height order. The subnet does not punish a missing EOTS vote.

The validator must store its EOTS vote before it publishes it. It publishes each EOTS vote once and never again, also not after a restart. It records the greatest height it voted at and, after a restart, continues above it. It records that height after publishing the EOTS vote: after a crash in between, it may publish that EOTS vote again, which is accepted.

### Accepting an EOTS vote

A node accepts a received EOTS vote only if, in order:

1. The envelope's signature verifies, and its subnet id is this subnet.
2. `h` is above the latest checkpointed height `h_c` and at most the height of the chain's latest subnet block, and the latest checkpoint is not the kill checkpoint: finality ends with the kill checkpoint.
3. `pk_eots` is the EOTS key of the validator that signed the envelope, and that validator is in the validator set at `h`.
4. The monitor has recorded a commitment of `pk_eots` whose range holds `h`, the proof's `start` is that commitment's, and the audit path proves `R_h` at index `h − start` against its root.
5. The signature verifies under `pk_eots` on `m`.

A node drops an EOTS vote that fails a rule. The exception is rule 4: the node keeps the EOTS vote and checks it again after every read from its monitor, until its height is final. Before its first read of the EOTS keys from its monitor since its start, a node also keeps an EOTS vote received on the vote gossip that fails only the EOTS key part of rule 3, if its validator is in the validator set at `h`, `h` is above the subnet final height and at most the height of the chain's latest subnet block, and it is the first such EOTS vote of that validator at `h`; the node checks it again after that read. A node also keeps an EOTS vote received on the vote gossip whose `h` is at most 10 above the height of the chain's latest subnet block, if `pk_eots` is the EOTS key that its monitor recorded for the validator that signed the envelope (before its first read of the EOTS keys: if that validator is in the validator set at `h`), and checks it again, rule 3 included, when its chain has a subnet block at `h`. In each case the node keeps at most 256 EOTS votes per validator, those of the lowest heights. This does not apply to the EOTS votes that subnet blocks carry ([subnet final height](#subnet-final-height)). No exception keeps an EOTS vote whose audit path holds more than 32 hashes, the most a commitment allows since `count` is a `u32`.

A node accepts an EOTS vote for any subnet block hash at `h`, including one of another fork ([double votes](#double-votes)). It stores each accepted EOTS vote complete, envelope and proof included, across restarts, at most two per `(pk_eots, h)`: the first and the first for a different subnet block. It forwards an EOTS vote on the vote gossip when it first accepts it. It deletes the EOTS votes, subnet block hashes and superseded power tables at or below the subnet final height.

### Final subnet blocks

A subnet block `b` at height `h` is *final* if one of these holds:

1. `b` is the [latest checkpointed block](#extending-the-latest-checkpointed-block) or one of its ancestors.
2. `h` is above `h_c`, the subnet block at height `h − 1` is final, and there are EOTS votes for `(h, b)` that satisfy the rules of [accepting an EOTS vote](#accepting-an-eots-vote), from validators of the validator set at `h` whose powers reach the threshold.

Whether a subnet block is final depends neither on the node that asks nor on the EOTS votes that node holds. The subnet must checkpoint only final subnet blocks ([protocol consumers](#protocol-consumers)).

### Subnet final height

The chain records finality in its subnet blocks, because the execution of subnet blocks needs a finality that is the same on every node, and a node that synchronizes from subnet blocks needs to verify it. The *subnet final height* of a chain (`recorded_final_height` in the code) is the greatest height whose EOTS votes a subnet block of the chain carries. The subnet keeps it in its gateway. The execution of a subnet block that carries EOTS votes sets it to the greatest carried height, and it never decreases. Like the [parent finality](lifecycle.md#applying-configuration-updates-in-the-subnet), it is the same on every node that executed the same subnet blocks, and older on a node that is behind. The execution of subnet blocks, checkpoint signing ([protocol consumers](#protocol-consumers)), the [`finalized` tag](#the-ethereum-json-rpc-api) and the [finality query](#the-finality-query) take it from the subnet state. After a [conflict](#double-votes) at height `h`, the `finalized` tag, the finality query and checkpoint signing take at most `h − 1`.

The proposer of a subnet block includes in its proposal one consensus message (`EotsFinality`), not a transaction, with:

- The EOTS votes it holds that reach the threshold for the subnet blocks of its chain above the subnet final height, in height order, each height once, up to the subnet block's size limit, if they pass the checks below.
- For every `pk_eots` for which it holds [evidence](#double-votes) and no earlier subnet block carries any, one evidence.

Every node checks the proposal and rejects it if a check fails:

1. Every carried EOTS vote satisfies the rules of [accepting an EOTS vote](#accepting-an-eots-vote), evaluated at the parent finality of the chain's latest executed subnet block and with that subnet block as the chain's latest subnet block, so that all honest monitors [give the same result](lifecycle.md#processing-bitcoin-l1-blocks). A node whose monitor has not reached that height, or is unreachable, must wait for its monitor before it executes the subnet block. It does not skip the EOTS votes or stop.
2. For every carried height, the carried EOTS votes reach the threshold for the subnet block that the node's chain holds at that height.
3. The carried heights continue the chain's subnet final height without a gap.
4. Every carried evidence consists of two accepted EOTS votes of one `pk_eots` at one height for two different subnet blocks.
5. The proposal holds at most one `EotsFinality`.
6. The proposal carries at most one evidence per `pk_eots`.

One failing EOTS vote or evidence rejects the whole proposal. This is safe for liveness: only the proposer chooses what a proposal holds, and the checks depend only on the proposal, the chain and the Bitcoin L1 state at the height of check 1, so no other validator can cause the rejection of an honest proposer's proposal.

A committed subnet block can still hold an EOTS vote that fails check 1 on a node whose monitor has reached that check's height, for example because the monitor parsed wrong data, also when it executes the subnet blocks again or synchronizes from them. The subnet final height follows the subnet block on every node. That node does not hold or count the EOTS vote, logs an error for an administrator and continues. An invalid EOTS vote must never stop a node.

## Extending the latest checkpointed block

A provider method returns `(h_c, b_c)`, the *latest checkpointed height* and the *latest checkpointed block*: the subnet height and subnet block hash of the latest checkpoint that the monitor has recorded (`checkpoint_hash`). While there is none, `h_c` is 0 and `b_c` is the hash of the subnet's genesis block. A node reads the EOTS keys, their EOTS nonce commitments and `(h_c, b_c)` in one call to one provider method. It answers from one state of the monitor at a Bitcoin L1 height `b` that the monitor has processed: the EOTS keys and EOTS nonce commitments recorded in Bitcoin blocks above a height that the node gives and at or below `b`, each commitment with the height of its Bitcoin block, and `(h_c, b_c)` of the latest checkpoint recorded at or below `b`. A subnet block that does not extend `b_c` is invalid, and a validator must not vote for one.

Before voting, a validator reads `(h_c, b_c)`. It must vote only at heights above `h_c`, and only if its chain holds `b_c` at `h_c`. A validator whose chain has not reached `h_c` synchronizes first. One whose chain holds another subnet block at `h_c` stops voting and reports the condition.

A validator that joins or resynchronizes the subnet must accept no chain in which `h_c` holds a subnet block other than `b_c` (**unimplemented**). Snapshots of the subnet state for synchronization are taken at final heights only. A node started from a snapshot takes the subnet final height and the validator set from the gateway.

## Consumers and final subnet blocks

A subnet block that is not final can be replaced at no cost to the validators that signed it. Replacing a final subnet block needs double EOTS votes from validators whose powers exceed one third, and every such validator is slashable ([double votes](#double-votes)).

### Protocol consumers

The execution of subnet blocks uses the subnet final height.

- The subnet creates the [checkpoint](transactions/checkpoint.md) of a [checkpoint height](lifecycle.md#checkpoints) `h` in the execution of subnet block `h`, from the state at the end of `h`. The header of subnet block `h + 1` holds the app hash of `h`, so the checkpoint of `h` is final once `h + 1` is final.
- A validator signs a checkpoint with its *checkpoint signature*, on the subnet's checkpoint data, and its *Bitcoin signatures*, on the inputs of the checkpoint transaction. It must sign and relay a checkpoint only once the checkpoint is final. It broadcasts its signatures to the subnet in two cases:
  - First signing. When its node executes the subnet block that raises the subnet final height to `h + 1` or above, the validator broadcasts its checkpoint signature and its Bitcoin signatures of the checkpoint of `h`, once.
  - Re-signing. When its node executes a subnet block whose height is a multiple of `bottomup_check_period`, after that subnet block's checkpoint, the validator checks every earlier final checkpoint. It broadcasts its checkpoint signature if the checkpoint signatures have not reached the quorum, and its Bitcoin signatures if the checkpoint is above `h_c`, that is, the monitor has not recorded it. It broadcasts only signatures that the subnet does not hold.

  A signature that fails for one checkpoint must not keep the validator from signing the next.
- The relayer reads heights, events and checkpoint bundles from the subnet with the `finalized` tag of the Ethereum JSON-RPC API.
- A top-down message counts only once the subnet block that executes it is final.

### The Ethereum JSON-RPC API

The API serves every subnet block that CometBFT committed. `finalized` and `safe` resolve to the subnet final height of the node's chain, at most `h − 1` after a conflict at `h`, and `latest` and `pending` to CometBFT's latest committed subnet block. A consumer that read a subnet block, receipt or log before it was final must confirm it by comparing its subnet block hash with the one at that height under `finalized`.

### The finality query

An IPC-aware node answers a finality query with the subnet final height of its chain and, after a conflict, the conflict height. The interface of the query is not specified. CometBFT's RPC, which is not part of the Bitcoin-IPC protocol, serves every committed subnet block and marks none as final. A consumer that reads it must take the subnet final height from the finality query of the same IPC-aware node and treat every subnet block above it as not final.

### Rules for consumers

A consumer should run its own IPC-aware node, which verifies every EOTS vote against its own monitor, and read with the `finalized` tag. A consumer that reads from another party's IPC-aware node trusts that party for finality.

## Slash pre-signatures

The slash transaction of a collateral UTXO or unbonding output is built deterministically from that output ([slash transaction format](transactions/slash.md#transaction-format)). The members of the configuration named in the output's slash leaf must sign it in advance: each member's provider signs it with the validator key, and the member's node posts the slash pre-signature on the subnet. Checkpoints carry the slash pre-signatures to Bitcoin L1 ([batch-transfer transaction](transactions/batch-transfer.md)) in these steps:

1. Every monitor records the collateral UTXO of a join or stake transaction when it accepts the transaction.
2. When its node executes a subnet block whose height is a multiple of `bottomup_check_period`, a validator reads from its provider the collateral UTXOs and unbonding outputs that its monitor has accepted and that are unspent and not slashable yet. At the same subnet blocks it [re-signs](#protocol-consumers) checkpoints.
3. For every such output whose slash leaf names a configuration of which the validator is a member, its provider signs the slash transaction and its node posts the slash pre-signature to the subnet. The subnet stores each slash pre-signature of an output under the address of its poster, as it does for Bitcoin signatures of a checkpoint, with no power, no threshold and no index, and checks neither the signature nor the poster: the providers check the signatures in step 5. It rejects a second slash pre-signature of the same output and poster address. A validator does not post when the subnet holds its slash pre-signature for the output, so no validator posts the same slash pre-signature twice. A slash pre-signature that does not reach the subnet is posted again `bottomup_check_period` subnet blocks later.
4. A checkpoint carries every output that received slash pre-signatures since the last checkpoint that carried it, with the slash pre-signatures posted for it, whether or not they reach any threshold. It carries at most 380,000 bytes of slash pre-signatures, counting 40 bytes per output and 67 per slash pre-signature, and the payloads of the checkpoint's bottom-up messages and supply adjustments count against the same limit. It carries at most the first 5,671 slash pre-signatures of one output. The outputs that do not fit come first in the next checkpoint. Every provider uses the slash pre-signatures that the checkpoint carries, also when it signs the checkpoint again.
5. When it builds the checkpoint, the provider checks each slash pre-signature that the checkpoint carries against its monitor: the output is known, and the signature is valid for the slash transaction under a key of the configuration that the output's slash leaf names. No index is posted: the provider matches a signature to the key of that configuration for which it verifies, and keeps the first valid signature of each key. A signature that verifies for no key is ignored. For every output whose valid signatures of distinct keys reach the threshold of that configuration, it adds one entry to the batch-transfer data, with the valid signatures in ascending order of the key's position in the configuration, each with that position as its index, until their powers reach the threshold. An output whose valid signatures do not reach the threshold gets no entry: its members that have not posted yet post once Bitcoin L1 records the checkpoint (step 3), and a later checkpoint carries the output again with all its slash pre-signatures. This holds for every checkpoint, rotation or regular, in epoch 0 or later.
6. When it processes the checkpoint's batch-transfer transaction, every monitor checks the entries and makes their outputs [slashable](#slashable-outputs).

### Before bootstrap and in epoch 0

1. Every join transaction before bootstrap names `C_wlist` in its slash leaf.
2. In epoch 0, the members of `C_wlist` must sign the slash transaction of every accepted collateral UTXO: the join collateral UTXO of every member of `C_gen`, and the collateral UTXO of every stake transaction that the monitor accepts in epoch 0. This needs live members of `C_wlist` whose powers reach its threshold. They post these slash pre-signatures as after activation.
3. The checkpoints of epoch 0 carry them like any checkpoint ([epoch 0](lifecycle.md#epoch-0)). `H_0` is the first Bitcoin L1 height at which every unspent join collateral UTXO of a member of `C_gen` is slashable. Collateral UTXOs of stake transactions do not count for `H_0`.

### After activation

The validators must pre-sign the slash transactions of new outputs in two cases:

- The collateral UTXO of a join or stake transaction that the monitor accepts after activation, at the next subnet block of step 2 of [slash pre-signatures](#slash-pre-signatures).
- The new collateral UTXO of a [collateral refresh](lifecycle.md#collateral-refresh) and the unbonding output of a [cooperative exit](lifecycle.md#cooperative-exit). The members of the checkpoint's configuration sign its slash transaction before they sign the checkpoint.

A validator must sign a checkpoint only once the subnet holds, for every collateral UTXO and unbonding output that the checkpoint creates, valid slash pre-signatures of members whose powers reach the threshold of the checkpoint's configuration. A checkpoint cannot carry the slash pre-signatures of an output it creates, because the slash transaction commits to the txid of the transaction that creates the output. The kill checkpoint has no next checkpoint: the validators keep the slash pre-signatures of its unbonding outputs and submit those slash transactions themselves.

A collateral UTXO seats its owner in a configuration that the monitor computes at a cutoff only if it is slashable at that cutoff ([C_e](lifecycle.md#computing-configuration-c_e-for-e--2)). A collateral UTXO whose slash pre-signatures reach Bitcoin L1 after a cutoff counts from a later one.

## Slashable outputs

The monitor checks the slash pre-signatures when it processes a batch-transfer transaction that carries them:

1. Every entry names a collateral UTXO or an unbonding output of the subnet that the monitor recorded. Every signature of the entry is valid for the slash transaction of that output under the key that its index names in the configuration that the output's slash leaf names. A batch-transfer transaction with an entry that breaks this rule fails as a whole ([batch-transfer transaction](transactions/batch-transfer.md#verifying-the-transaction)).
2. A collateral UTXO or unbonding output becomes **slashable** once one entry for it holds signatures of members whose powers reach the threshold of the configuration that its slash leaf names. The monitor stores that entry's signatures. A later entry for the output changes nothing.

When a Bitcoin block spends a collateral UTXO other than in a collateral refresh, the collateral UTXO no longer counts for the configurations that the monitor computes at later cutoffs. The current configuration keeps its members until the next cutoff.

## Double votes

Two accepted EOTS votes of one `pk_eots` at one height `h` for two different subnet blocks are a *double vote*, and the pair is its *evidence*. Evidence changes no vote count: the EOTS votes of the validator count until the monitor records the spend of its collateral UTXO and a cutoff removes it from the configuration.

Two different subnet blocks at one height are both final only if validators whose powers exceed one third double-voted at that height or below. A node that holds, at a height `h`, EOTS votes that reach the threshold for a subnet block other than the one its chain holds at `h` must report this *conflict* at `h`. From then on it serves no subnet block at or above `h` as final, and its finality query carries `h`. It does not switch chains on its own.

## Slashing a validator that equivocated

Two EOTS votes of the same `pk_eots` at the same height `h` for different subnet blocks, `R_h ‖ s` on `m` and `R_h ‖ s'` on `m'`, share the public nonce `R_h`. Anyone who holds both can compute the secret key `d = (s − s') · (e − e')^{-1} mod n`, where `e` and `e'` are the BIP340 challenges of `m` and `m'`, and `n` is the order of the secp256k1 group. With `d`, it can sign the pre-signed slash transaction of every unspent collateral UTXO and unbonding output of the validator under `pk_eots`, add that signature `σ_eots` to the witness and broadcast each with a fee-paying child ([slash transaction](transactions/slash.md#creating-the-transaction)). Computing `d` and submitting the slash transaction are not part of the implementation: anyone may do it with the evidence and the slash pre-signatures (**unimplemented**).

A validator equivocates only at heights above `h_c`, because nodes accept no EOTS vote at or below it. At those heights every seated validator still holds its collateral UTXO, as long as the condition on `K` in [choosing the parameter values](lifecycle.md#choosing-the-parameter-values) holds.

The validators must sign no refresh pair and no cooperative exit of a collateral UTXO whose owner they saw equivocate.
