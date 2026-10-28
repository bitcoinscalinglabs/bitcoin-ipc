# Slashing

A validator that votes for two different subnet blocks at the same height equivocates. It loses every collateral and unbonding output it holds to [slash](transactions/slash.md) transactions. This document follows slashing through the life of a subnet: EOTS keys and nonces, votes, the pre-signatures that make each collateral slashable and the checkpoints that carry them to Bitcoin L1, the monitor's checks, and the slashing itself. [Slash](transactions/slash.md) gives the transaction format, and [instantiating the parameters](lifecycle.md#instantiating-the-parameters) the conditions on the parameters under which slashing works.

## EOTS key and EOTS nonce commitments

Every validator joins with an [EOTS](encoding.md#notation) key `pk_eots`, different from its validator key, and an [EOTS nonce commitment](encoding.md#eots-nonce-commitment) of `pk_eots`: one public nonce for each subnet height of a range. Its [join](transactions/join.md) carries both. Before its committed ranges run out, the validator adds an EOTS nonce commitment for later heights with a [stake](transactions/stake.md). The monitor records the EOTS nonce commitments of accepted joins and stakes, and the subnet reads them through its provider.

## Votes on the subnet

Once the subnet's consensus has decided the block `b` at height `h`, every validator of the subnet's validator set votes for it:

- The message is `m = tagged_sha256("IPC/vote", txid20(subnet) ‖ h ‖ b)`, with `h` a `u64` little-endian and `b` the 32-byte subnet block hash (`tagged_sha256` as in [encoding](encoding.md#notation)).
- The vote is the 64-byte BIP340 signature `R_h ‖ s` on `m` under `pk_eots`, made with the secret nonce `k_h` whose public nonce `R_h` the validator committed for `h` ([EOTS nonce commitment](encoding.md#eots-nonce-commitment)). `s = k_h + e · d mod n`, where `d` is the secret key of `pk_eots` as BIP340 uses it, `e` the BIP340 challenge of `R_h`, `pk_eots` and `m`, and `n` the order of the secp256k1 group.
- The validator posts the vote on the subnet, without fees, with the proof of `R_h` against the root of the commitment whose range holds `h`. Every validator reads the votes there.

A validator accepts a vote only if the signature verifies under `pk_eots` and the proof shows that `R_h` is the public nonce committed for `h`. A subnet block is final once it has votes from validators whose powers reach the threshold of their configuration ([power and thresholds](lifecycle.md#power-and-thresholds)). A checkpoint commits only a final block. A subnet block that does not extend the subnet block of the latest checkpoint that the monitor has recorded is invalid, and a validator never votes for one. A validator that joins or resynchronizes the subnet starts from that block.

A validator votes at most once per height, and only for the decided block. It uses every secret nonce for one message only. At a height that none of its EOTS nonce commitments covers, it casts no vote.

## Pre-signatures

The slash transaction of a collateral or an unbonding output depends only on that output ([transaction format](transactions/slash.md#transaction-format)). The members of the configuration that the output's slash leaf names therefore build and sign it in advance, each with its validator key. The subnet collects these pre-signatures, and checkpoints carry them to Bitcoin L1 in their [batch transfer](transactions/batch-transfer.md) data, where every monitor checks them ([slashable outputs](#slashable-outputs)).

### Before bootstrap and in epoch 0

1. Every join before bootstrap names `C_0` in its slash leaf ([C_0](lifecycle.md#computing-configuration-c_0)).
2. In epoch 0, the members of `C_0` sign the slash transaction of the join collateral of every member of `C_gen`, and the subnet collects these pre-signatures. The subnet's genesis creates an account for the subnet address of every member of `C_0`, so that each can post them. This needs live members of `C_0` whose powers reach its threshold.
3. The subnet creates the [first checkpoint](transactions/checkpoint.md#the-first-checkpoint) only once it holds the pre-signatures for every member of `C_gen`. The reveal of the first checkpoint carries them all. A member of `C_gen` whose collateral they do not make slashable is not in `C_1` ([C_1](lifecycle.md#computing-configuration-c_1)).
4. Each entry of the first checkpoint holds at least `⌊2 · min_validators / 3⌋ + 1` signatures, so the size of its reveal grows with the square of `min_validators`. `min_validators ≤ 90` ([validity of the parameters](lifecycle.md#validity-of-the-parameters)) keeps it within one standard transaction.

### After activation

The validators pre-sign the slash transactions of new outputs in two cases:

- The collateral of a join or a stake that the monitor accepts, other than the collaterals of `C_gen`. The members of the configuration that its slash leaf names sign once the monitor has accepted it.
- The new collateral of a [refresh](lifecycle.md#collateral-refresh) and the unbonding output of a [cooperative exit](lifecycle.md#cooperative-exit). The members of the checkpoint's configuration sign its slash transaction before they sign the checkpoint.

A validator signs a checkpoint only once the subnet holds, for every collateral and unbonding output that the checkpoint creates, pre-signatures of members whose powers reach the threshold of the checkpoint's configuration. Once the subnet holds pre-signatures that reach the threshold for an output, the next checkpoints, rotation or regular, carry them in one entry of their batch transfer data, oldest first, as many entries as a standard transaction holds. A checkpoint cannot carry the pre-signatures of a collateral that it creates itself, because the slash transaction commits to the txid of the transaction that creates the collateral. The kill checkpoint has no next checkpoint: the validators keep the pre-signatures of its unbonding outputs and submit those slash transactions themselves.

A collateral seats its owner in a configuration that the monitor computes at a cutoff only if it is slashable at that cutoff ([C_e](lifecycle.md#computing-configuration-c_e-for-e--2)). A collateral whose pre-signatures reach Bitcoin L1 after a cutoff counts from a later one.

## Slashable outputs

The monitor checks the pre-signatures when it processes a batch transfer that carries them:

1. Every entry names a collateral or an unbonding output of the subnet that the monitor recorded. Every signature of the entry is valid for the slash transaction of that output under the key that its index names in the configuration that the output's slash leaf names. A batch transfer with an entry that breaks this rule fails as a whole ([batch transfer](transactions/batch-transfer.md#verifying-the-transaction)).
2. A collateral or unbonding output becomes **slashable** once one entry for it holds signatures of members whose powers reach the threshold of the configuration that its slash leaf names. The monitor stores that entry's signatures, and a later entry for the output changes nothing.

When a block spends a collateral other than in a refresh, the collateral no longer counts for the configurations that the monitor computes at later cutoffs. The current configuration keeps its members until the next cutoff.

## Slashing a validator that equivocated

Two votes of the same `pk_eots` at the same height `h` for different blocks, `R_h ‖ s` on `m` and `R_h ‖ s'` on `m'`, share the nonce `R_h`. Anyone who holds both computes the secret key `d = (s − s') · (e − e')^{-1} mod n`, where `e` and `e'` are the BIP340 challenges of `m` and `m'`. With `d`, it signs the pre-signed slash transaction of every unspent collateral and unbonding output of the validator under `pk_eots`, adds that signature `σ_eots` to the witness ([transaction format](transactions/slash.md#transaction-format)) and broadcasts each with a fee-paying child.

The validators sign no refresh and no exit of a collateral whose owner they saw equivocate.
