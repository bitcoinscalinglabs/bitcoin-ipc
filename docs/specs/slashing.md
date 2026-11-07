# Slashing

A validator that votes for two different subnet blocks at the same height *equivocates*. It loses every collateral and unbonding output it still holds to slash transactions. Transaction format: [slash transaction](transactions/slash.md). Conditions on the parameters: [instantiating the parameters](lifecycle.md#instantiating-the-parameters).

The subnet's consensus engine (CometBFT) commits a block with signatures under the validator keys. These are not slashable. An *EOTS vote* is a second signature on the block, under the EOTS key, and is slashable. A *consumer* is any program or person that reads subnet blocks, state or results: checkpoint creation, the relayer, wallets, explorers, indexers. Consumers must rely on [final blocks](#final-blocks) only.

## EOTS key and EOTS nonce commitments

Every validator specifies an EOTS key `pk_eots` once per subnet (encoding: [EOTS](encoding.md#notation)), different from its validator key. It also specifies a nonce commitment covering one public nonce for each subnet height of a range (encoding: [EOTS nonce commitment](encoding.md#eots-nonce-commitment)). Its [join transaction](transactions/join.md) carries both. The validator must keep its ranges covering every height at which it votes, from its first seat on and without gaps, by adding EOTS nonce commitments with [stake transactions](transactions/stake.md). A height it cannot vote at counts as a missing vote. The monitor does not check coverage. It records the EOTS nonce commitments of accepted join and stake transactions. A provider method returns, for an EOTS key, every commitment `(start, count, root)` the monitor has recorded.

The validator generates its EOTS key locally, and its provider holds it ([command](ipc-cli.md#generate-the-eots-key)). The validator derives every secret nonce it commits to from a *nonce seed*, a random secret separate from the EOTS key, so it stores no nonce and no commitment tree. The secret nonce for height `h` is the 32-byte output of HMAC-SHA256 keyed by the nonce seed, over the message `txid20(subnet) ‖ h ‖ c`, with `h` a `u64` little-endian and `c` a one-byte counter, read as a big-endian scalar. The counter starts at 0 and increases by 1 until the output is a non-zero scalar below `n`. If the public nonce of that scalar has an odd y coordinate, the validator uses the negated scalar as `k_h`, so that `R_h` has an even y coordinate. No one checks the derivation: the monitor checks only the commitment, so any derivation works as long as every nonce is secret and none is reused. The seed is as secret as the EOTS key: one secret nonce and its EOTS vote give the key. The validator keeps the seed as it keeps the EOTS key, across restarts, since a lost seed leaves every nonce it committed to unusable. The validator keeps the complete EOTS vote of every height it has voted at, across restarts and resynchronizations. It must not sign a second message at such a height.

## EOTS votes

EOTS votes are consensus messages of the subnet, not subnet transactions. They cost no fee and touch no contract. Every [IPC-aware node](modules.md#ipc-aware-node), validator or not, publishes and receives them on the subnet's vote gossip. Later blocks carry them ([subnet final height](#subnet-final-height)).

### The vote

An EOTS vote is a vote for one subnet block `b` at one height `h`:

- The message is `m = tagged_sha256("IPC/vote", txid20(subnet) ‖ h ‖ b)`, with `h` a `u64` little-endian and `b` the 32-byte subnet block hash (`tagged_sha256` as in [encoding](encoding.md#notation)).
- The EOTS vote is the 64-byte BIP340 signature `R_h ‖ s` on `m` under `pk_eots`, made with the secret nonce `k_h` whose public nonce `R_h` the validator committed for `h`. `s = k_h + e · d mod n`, where `d` is the secret key of `pk_eots` as BIP340 uses it, `e` the BIP340 challenge of `R_h`, `pk_eots` and `m`, and `n` the order of the secp256k1 group. The vote verifies as an ordinary BIP340 signature on `m` under `pk_eots`.
- The EOTS vote record holds `pk_eots`, `h`, `b`, the signature `R_h ‖ s`, and the proof of `R_h` against the root of the commitment whose range holds `h`: the commitment's `start` and the audit path ([EOTS nonce commitment](encoding.md#eots-nonce-commitment)). Encoding: [EOTS vote](encoding.md#eots-vote).
- The validator publishes the record inside the subnet's signed vote envelope (`VoteRecord`: the validator's key, the subnet id, the content, a timestamp, signed with the validator key).

### Casting an EOTS vote

Once the subnet's consensus engine has committed the subnet block `b` at height `h`, every validator of the subnet's validator set at `h` must cast an EOTS vote for `b`, unless it already knows `b` to be [final](#final-blocks). It knows `b` to be final once the EOTS votes it holds show `b` final. The *validator set at `h`* is the subnet's power table ([power and thresholds](lifecycle.md#power-and-thresholds)) at the end of subnet block `h − 2`: as in CometBFT, a change that the execution of subnet block `h` makes to the power table applies from `h + 2`.

A validator must cast at most one EOTS vote per height, only for the subnet block its consensus engine committed there. It must use every secret nonce at most once. It does not vote at a height that none of its EOTS nonce commitments covers, or where [extending the latest checkpointed block](#extending-the-latest-checkpointed-block) forbids it. A validator that has missed heights resumes above those whose subnet block it already knows to be final, and votes at the rest as soon as it can, in height order. The subnet does not punish a validator for a missing EOTS vote.

The validator must store the complete EOTS vote before it publishes it. At start, it publishes again every own EOTS vote that no subnet block carries yet. It publishes an EOTS vote again on request. Publishing the same EOTS vote again is not a second vote and uses no new EOTS nonce.

### Accepting an EOTS vote

A node accepts a received EOTS vote only if, in order:

1. The envelope's signature verifies, and its subnet id is this subnet.
2. `h` is above the latest checkpointed height `h_c` ([extending the latest checkpointed block](#extending-the-latest-checkpointed-block)), and the latest checkpoint is not the kill checkpoint: finality ends with the kill checkpoint.
3. `pk_eots` is the EOTS key of the validator that signed the envelope, and that validator is in the validator set at `h`.
4. The monitor has recorded a commitment of `pk_eots` whose range holds `h`, the proof's `start` is that commitment's, and the audit path proves `R_h` at index `h − start` against its root.
5. The signature verifies under `pk_eots` on `m`.

A node drops an EOTS vote that fails a rule. Exception: a node whose monitor is behind the parent finality of its latest executed subnet block keeps an EOTS vote that fails rule 4 and checks it again once the monitor has caught up.

A node accepts an EOTS vote for any block hash `b` at `h`, including a block of another fork ([double votes](#double-votes)). It stores every accepted EOTS vote complete, envelope and proof included, across restarts, indexed by `(pk_eots, h)`. It keeps at most two per `(pk_eots, h)`: the first and the first one for a different block. It forwards an EOTS vote on the vote gossip when it first accepts it. It must answer a peer's request for the EOTS votes it holds at a height and for the evidence it holds. On connecting to a peer, it requests the peer's EOTS votes above the subnet final height of its chain and all the peer's evidence (peer requests **unimplemented**).

### Final blocks

A subnet block `b` at height `h` is *final* if one of these holds:

1. `b` is the [latest checkpointed block](#extending-the-latest-checkpointed-block) or one of its ancestors.
2. `h` is above the latest checkpointed height `h_c`, the block at height `h − 1` is final, and there are EOTS votes for `(h, b)` that satisfy the rules [accepting an EOTS vote](#accepting-an-eots-vote), from validators of the validator set at `h` whose powers reach the threshold.

Whether a block is final does not depend on which node asks or on which EOTS votes that node holds. The subnet must checkpoint only final blocks ([protocol consumers](#protocol-consumers)).

### Subnet final height

The execution of subnet blocks needs a subnet finality that is the same on every node, and a node that synchronizes from subnet blocks needs to verify that every earlier subnet block is final. Gossip provides neither, so the chain records finality in its subnet blocks.

The *subnet final height* of a chain (`recorded_final_height` in the code) is the greatest height whose EOTS votes a subnet block of the chain carries. The subnet keeps it in its gateway contract. The execution of a subnet block that carries EOTS votes sets it to the greatest carried height, and it never decreases. Like the [parent finality](lifecycle.md#applying-configuration-updates-in-the-subnet), it is the same on every node that executed the same subnet blocks. A node that is behind, for example while it synchronizes, holds an older value. The execution of subnet blocks, checkpoint signing ([protocol consumers](#protocol-consumers)), the [`finalized` tag](#the-ethereum-json-rpc-api) and the [finality query](#the-finality-query) take the subnet final height from the subnet state. After a [conflict](#double-votes) at a height `h`, the `finalized` tag, the finality query and checkpoint signing take at most `h − 1`. A node that synchronizes from subnet blocks stores the carried EOTS votes and evidence as if received on the gossip.

The proposer of a subnet block includes in its proposal, in one consensus message (`EotsFinality`) and not as a transaction:

- the EOTS votes it holds that reach the threshold for the subnet blocks of its chain at the heights above the subnet final height, in height order, up to the subnet block's size limit, if they pass the checks below. Each height is carried once.
- for every `pk_eots` for which the proposer holds double-vote evidence and no earlier subnet block carries any, one evidence.

Every node checks the proposal and rejects it if a check fails:

1. Every carried EOTS vote must satisfy the rules [accepting an EOTS vote](#accepting-an-eots-vote) above. Every node must evaluate them at a specific Bitcoin L1 height, the parent finality of the chain's latest executed subnet block, so that all honest monitors [give the same result](lifecycle.md#processing-bitcoin-l1-blocks). A node whose monitor has not reached that height, or is unreachable, must wait for its monitor before it executes the subnet block that carries the EOTS votes. It does not skip them or stop.
2. For every carried height, the carried EOTS votes reach the threshold for the subnet block that the node's chain holds at that height.
3. The carried heights continue the chain's subnet final height without a gap.
4. Every carried evidence consists of two accepted EOTS votes of one `pk_eots` at one height for two different subnet blocks.
5. The proposal holds at most one `EotsFinality`.

One failing EOTS vote or evidence rejects the whole proposal. This is safe for liveness. Only the proposer chooses what a proposal holds, and the checks depend only on the proposal, the chain and the Bitcoin L1 state at the height of check 1. So no other validator can cause the rejection of an honest proposer's proposal.

A committed subnet block can still hold an EOTS vote that fails check 1 on a node whose monitor has reached that check's height, for example because the monitor parsed wrong data. This also applies to a node that executes the subnet blocks again or synchronizes from them. The subnet final height follows the subnet block on every node. That node does not hold or count the EOTS vote. It logs an error for an administrator and continues. An invalid EOTS vote must never stop a node.

## Extending the latest checkpointed block

A provider method returns `(h_c, b_c)`, the *latest checkpointed height* and *block*: the subnet height and block hash of the latest checkpoint that the monitor has recorded (`checkpoint_hash`). While there is none, `h_c` is 0 and `b_c` is the hash of the subnet's genesis block. A subnet block that does not extend `b_c` is invalid, and a validator must not vote for one.

Before voting, a validator reads `(h_c, b_c)`. It must vote only at heights above `h_c`, and only if its chain holds `b_c` at `h_c`. A validator whose chain has not reached `h_c` synchronizes first. A validator whose chain holds another block at `h_c` stops voting and reports the condition.

A validator that joins or resynchronizes the subnet must accept no chain in which height `h_c` holds a block other than `b_c`. Snapshots of the subnet's state for synchronization must be taken at final heights only.

## Consumers and final blocks

A block that is not final can be replaced at no cost to the validators that signed it. Replacing a final block needs double EOTS votes from validators whose powers exceed one third, and every such validator is slashable ([double votes](#double-votes)).

An IPC-aware node verifies every EOTS vote against its own monitor.

### Protocol consumers

The execution of subnet blocks uses the subnet final height.

- The subnet creates the [checkpoint](transactions/checkpoint.md) of a [checkpoint height](lifecycle.md#checkpoints) `h` in the execution of subnet block `h`, from the state at the end of `h`. The header of subnet block `h + 1` holds the app hash of `h`, so the checkpoint of `h` is final once `h + 1` is final.
- A validator signs a checkpoint with two kinds of signatures: its *checkpoint signature*, on the subnet's checkpoint data, and its *Bitcoin signatures*, on the inputs of the checkpoint transaction. It must sign a checkpoint and relay it only once the checkpoint is final. It broadcasts its signatures to the subnet in two cases:
  - First signing. When its node executes the subnet block that raises the subnet final height to `h + 1` or above, the validator broadcasts its checkpoint signature and its Bitcoin signatures of the checkpoint of `h`, once.
  - Re-signing. When its node executes a subnet block whose height is a multiple of `bottomup_check_period`, after the subnet creates that subnet block's checkpoint, the validator checks every earlier final checkpoint. It broadcasts its checkpoint signature if the checkpoint signatures of that checkpoint have not reached the quorum. It broadcasts its Bitcoin signatures if the checkpoint is above the [latest checkpointed height](#extending-the-latest-checkpointed-block) `h_c`, that is, the monitor has not recorded it. In both cases it broadcasts only a signature that the subnet does not hold.

  A signature that fails for one checkpoint must not keep the validator from signing the next.
- The relayer reads heights, events and checkpoint bundles from the subnet with the `finalized` block tag of the Ethereum JSON-RPC API.
- A top-down message counts only once the subnet block that executes it is final.

### The Ethereum JSON-RPC API

The API serves every block that CometBFT committed. `finalized` and `safe` resolve to the subnet final height of the node's chain, except after a conflict ([double votes](#double-votes)), and `latest` and `pending` to CometBFT's latest committed block. A consumer that needs finality reads with `finalized`. A consumer that read a block, receipt or log before it was final must confirm it by comparing its block hash with the block at that height under `finalized`.

### The finality query

An IPC-aware node answers a finality query with the subnet final height of its chain, and, for a height `h` up to it, the hash of the final block at `h` and the EOTS votes it holds for it, each with its proof. The interface of the query is not specified yet.

### CometBFT's RPC

CometBFT's RPC serves every committed block and marks none as final. It is not part of the Bitcoin-IPC protocol. A consumer that reads it must take the subnet final height from the finality query of the same IPC-aware node and treat every block above it as not final.

### Rules for consumers

A consumer should run its own IPC-aware node and read with the `finalized` tag. A consumer that reads from another party's IPC-aware node trusts that party for finality.

## Slash pre-signatures

The slash transaction of a collateral or unbonding output is built deterministically from that output ([slash transaction format](transactions/slash.md#transaction-format)). The members of the configuration named in the output's slash leaf must sign it in advance. Each member's provider builds it and signs it with the validator key, and the member's node posts the slash pre-signature on the subnet. The subnet collects the slash pre-signatures, and checkpoints carry them to Bitcoin L1 ([batch-transfer transaction](transactions/batch-transfer.md)).

Slash pre-signatures reach Bitcoin L1 in these steps:

1. Every monitor records the collateral of a join or stake transaction when it accepts the transaction, as it processes the Bitcoin block that includes it.
2. When its node executes a subnet block whose height is a multiple of `bottomup_check_period`, a validator reads from its provider the collaterals and unbonding outputs that its monitor has accepted and that are unspent and not slashable yet.
3. For every such output whose slash leaf names a configuration of which the validator is a member, and for which the subnet holds no slash pre-signature of the validator, its provider signs the slash transaction and its node posts the slash pre-signature to the subnet. The subnet keeps its slash pre-signatures in one list, in the order in which they were posted. The list only grows. The subnet rejects a second slash pre-signature of the same output and validator index, and checks nothing else: the providers check them in step 5. A slash pre-signature that does not reach the subnet is posted again `bottomup_check_period` subnet blocks later.
4. The checkpoint of subnet block `h` records the length `N` of that list at the end of `h`, so that every provider uses the same slash pre-signatures for it.
5. When it builds the checkpoint, the provider checks each of the first `N` slash pre-signatures against its monitor: the output is known and not slashable yet, and the signature is valid for the configuration that the output's slash leaf names. For every output whose valid slash pre-signatures among the first `N` reach the threshold of that configuration, it adds one entry with those signatures to the batch-transfer data, oldest output first, as many entries as a standard transaction holds. The next checkpoints carry the rest. This holds for every checkpoint, rotation or regular, in epoch 0 or later.
6. When it processes the checkpoint's batch-transfer transaction, every monitor checks the entries and makes their outputs [slashable](#slashable-outputs).
7. At the subnet blocks of step 2, a validator also signs again the final checkpoints that Bitcoin L1 has not recorded and that lack its Bitcoin signature ([re-signing](#protocol-consumers)).

### Before bootstrap and in epoch 0

1. Every join transaction before bootstrap names `C_wlist` in its slash leaf.
2. In epoch 0, the members of `C_wlist` must sign the slash transaction of every accepted collateral: the join collateral of every member of `C_gen`, and the collateral of every stake transaction that the monitor accepts in epoch 0. This needs live members of `C_wlist` whose powers reach its threshold. They post these slash pre-signatures as after activation ([posting](#slash-pre-signatures)). The subnet's genesis creates an account for the subnet address of every member of `C_wlist`.
3. The checkpoints of epoch 0 carry these slash pre-signatures like any checkpoint ([checkpoints of epoch 0](transactions/checkpoint.md#checkpoints-of-epoch-0)). The monitor sets `H_0` at the first Bitcoin L1 height at which every unspent join collateral of a member of `C_gen` is slashable ([epoch 0](lifecycle.md#epoch-0)). The collaterals of stake transactions do not count for `H_0`. A member of `C_gen` whose join collateral is spent at or before `H_0` is not in `C_1` ([C_1](lifecycle.md#computing-configuration-c_1)).
4. Each entry for the join collateral of a member of `C_gen` carries signatures only until their powers reach the threshold of `C_wlist`. `boot_validators ≤ 90` ([validity of the parameters](lifecycle.md#validity-of-the-parameters)) keeps the entries of every member of `C_gen` within the batch-transfer transaction of one checkpoint of epoch 0.

### After activation

The validators must pre-sign the slash transactions of new outputs in two cases:

- The collateral of a join or stake transaction that the monitor accepts after activation. The members of the configuration that its slash leaf names sign at the first subnet block whose height is a multiple of `bottomup_check_period` after the monitor has accepted it ([posting](#slash-pre-signatures)).
- The new collateral of a [collateral refresh](lifecycle.md#collateral-refresh) and the unbonding output of a [cooperative exit](lifecycle.md#cooperative-exit). The members of the checkpoint's configuration sign its slash transaction before they sign the checkpoint.

A validator must sign a checkpoint only once the subnet holds, for every collateral and unbonding output that the checkpoint creates, slash pre-signatures of members whose powers reach the threshold of the checkpoint's configuration. A checkpoint cannot carry the slash pre-signatures of a collateral that it creates itself, because the slash transaction commits to the txid of the transaction that creates the collateral. The kill checkpoint has no next checkpoint: the validators keep the slash pre-signatures of its unbonding outputs and submit those slash transactions themselves.

A collateral seats its owner in a configuration that the monitor computes at a cutoff only if it is slashable at that cutoff ([C_e](lifecycle.md#computing-configuration-c_e-for-e--2)). A collateral whose slash pre-signatures reach Bitcoin L1 after a cutoff counts from a later one.

## Slashable outputs

The monitor checks the slash pre-signatures when it processes a batch-transfer transaction that carries them:

1. Every entry names a collateral or an unbonding output of the subnet that the monitor recorded. Every signature of the entry is valid for the slash transaction of that output under the key that its index names in the configuration that the output's slash leaf names. A batch-transfer transaction with an entry that breaks this rule fails as a whole ([batch-transfer transaction](transactions/batch-transfer.md#verifying-the-transaction)).
2. A collateral or unbonding output becomes **slashable** once one entry for it holds signatures of members whose powers reach the threshold of the configuration that its slash leaf names. The monitor stores that entry's signatures. A later entry for the output changes nothing.

When a block spends a collateral other than in a collateral refresh, the collateral no longer counts for the configurations that the monitor computes at later cutoffs. The current configuration keeps its members until the next cutoff.

## Double votes

Two accepted EOTS votes of one `pk_eots` at one height `h` for two different blocks are a *double vote*, and the pair is its *evidence*.

- A node that holds evidence must publish both EOTS votes on the vote gossip when it first holds them, on every new peer connection (publication on a new peer connection **unimplemented**) and at a fixed interval, until a block carries evidence for that `pk_eots`.
- A node keeps evidence until the slash transaction of every collateral and unbonding output of the validator is final on Bitcoin L1.
- Evidence changes no vote count. The EOTS votes of the validator count until the monitor records the spend of its collateral and a cutoff removes it from the configuration.

Two different blocks at one height are both final only if validators whose powers exceed one third double-voted at that height or below. A node that holds, at a height `h`, EOTS votes that reach the threshold for a block other than the one its chain holds at `h` must report the condition. From then on it serves no block at or above `h` as final. It does not switch chains on its own.

## Slashing a validator that equivocated

Two EOTS votes of the same `pk_eots` at the same height `h` for different blocks, `R_h ‖ s` on `m` and `R_h ‖ s'` on `m'`, share the public nonce `R_h`. Anyone who holds both can compute the secret key `d = (s − s') · (e − e')^{-1} mod n`, where `e` and `e'` are the BIP340 challenges of `m` and `m'`. With `d`, it can sign the pre-signed slash transaction of every unspent collateral and unbonding output of the validator under `pk_eots`, add that signature `σ_eots` to the witness and broadcast each with a fee-paying child ([slash transaction](transactions/slash.md#creating-the-transaction)). Computing `d` and submitting the slash transaction are not part of the implementation: anyone may do it with the evidence and the slash pre-signatures (**unimplemented**).

A validator equivocates only at heights above `h_c`, because nodes accept no EOTS vote at or below it. At those heights every seated validator still holds its collateral, as long as the condition on `K` in [choosing the parameter values](lifecycle.md#choosing-the-parameter-values) holds.

The validators must sign no refresh pair and no cooperative exit of a collateral whose owner they saw equivocate.
