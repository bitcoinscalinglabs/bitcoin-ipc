# Slash

The slash transaction burns the collateral or unbonding output of a validator that equivocated on the subnet with its [EOTS](../encoding.md#notation) key, that is, voted for two different blocks at the same height ([slashing](../slashing.md)). The validators of the configuration that the output's slash leaf names pre-sign its slash transaction. Anyone who learns the validator's EOTS secret key completes it and burns the output.

## Transaction format

The slash transaction of an output `o` of value `S` that is a collateral, locked under the [collateral script](../encoding.md#collateral-script) of `pk`, `pk_eots`, `T` and configuration `C`, or an unbonding output, locked under the [unbonding script](../encoding.md#unbonding-script) of `pk`, `pk_eots` and `C`:

| Field | Value |
|---|---|
| version | 3 (TRUC) |
| nLockTime | 0 |
| input 0 | `o`, spent through the slash leaf `L(pk_eots, C)`, with nSequence `0xffffffff` and the witness `[s_n, …, s_1, σ_eots, L(pk_eots, C), cb]` |
| output 0 | value `S`, scriptPubKey `6a`: an OP_RETURN without data, which burns the value of `o` |
| output 1 | value 0, scriptPubKey `51 02 4e 73`: a pay-to-anchor output |

Every signature in the witness is a 64-byte BIP340 signature over the BIP341 script-path sighash of input 0 with the leaf `L(pk_eots, C)` and `SIGHASH_DEFAULT`: `s_i` under the `i`-th key of `C` in ascending order, or an empty element for a key that does not sign, and `σ_eots` under `pk_eots`. `cb` is the control block of the slash leaf in the script of `o`.

The transaction pays no fee. Whoever broadcasts it submits it as a package with a child transaction that pays the fee of both (CPFP). The child has version 3, spends output 1 with an empty witness and at least one output of the submitter's wallet, and is at most 1,000 vB. The two go to a Bitcoin L1 node together (`submitpackage`). TRUC limits the slash transaction to 10,000 vB and the child to 1,000 vB. A configuration of at most 350 members, which the [validity of the parameters](../lifecycle.md#validity-of-the-parameters) guarantees, keeps the slash transaction below that limit even when every member signs. Bitcoin Core relays the package from version 29.0 on, which accepts an anchor of value 0 in a transaction without fee, and only if the submitter passes `maxburnamount` (in BTC) of at least `S`.

## Creating the transaction

The members of `C` pre-sign the transaction, and checkpoints carry their pre-signatures to Bitcoin L1 ([pre-signatures](../slashing.md#pre-signatures)). Anyone who computes the secret key of `pk_eots` from two votes of the validator at the same height adds `σ_eots` to the witness and broadcasts the transaction with a fee-paying child ([slashing a validator that equivocated](../slashing.md#slashing-a-validator-that-equivocated)).

## Verifying the transaction

Bitcoin L1 verifies the slash leaf. The monitor checks the pre-signatures when it processes the batch transfers that carry them ([slashable outputs](../slashing.md#slashable-outputs)), and records the spend of the output, as for every output it holds.
