# Exit

With the exit transaction, a validator spends its matured collateral outputs, through the unilateral-exit leaf, and its unbonding outputs after their delay, through the unilateral-unbonding leaf, to its backup address, without the subnet's validators. The code calls it the unilateral-exit spend, and the provider's call is `unstakeue`.

## Transaction format

| Field | Value |
|---|---|
| version | 2 |
| nLockTime | the maximum timeout `T` of the collateral inputs, as a Bitcoin block height, or 0 if there is none |
| inputs | collateral outputs and unbonding outputs of one validator, with an empty scriptSig. A collateral input has nSequence `0xfffffffd` and the witness `[sig, U(pk, T), cb]`. An unbonding input has nSequence `3K` and the witness `[sig, B(pk), cb]`. `sig` is the 64-byte BIP340 signature of `pk` over the BIP341 script-path sighash of the leaf (`SIGHASH_DEFAULT`, all prevouts), and `cb` the leaf's control block |
| output 0 | the backup address of the join transaction, with value = the sum of the inputs minus the fee |

The transaction is valid from Bitcoin block `T + 1` on (`OP_CHECKLOCKTIMEVERIFY`), and from `3K` Bitcoin blocks after the Bitcoin block that includes each unbonding input (`OP_CHECKSEQUENCEVERIFY`).

## Creating the transaction

The validator's provider builds it with `unstakeue`, with the validator key:

1. It takes the unspent collateral UTXOs of the validator key in the subnet that have matured (`T` ≤ tip) and its unbonding outputs whose delay has passed.
2. It keeps those that pay the same backup address as the latest of them, at most 1,000, so that the transaction stays within the 400,000 weight units of a standard transaction. A later call spends the rest.
3. It signs every input and submits the transaction. It refuses if their value cannot pay the fee and an output above the dust limit.

Anyone holding the key can build the same transaction by hand.

## Verifying the transaction

Bitcoin L1 verifies the leaves. The monitor records the spend of each collateral UTXO and unbonding output that the transaction spends, so no configuration seats the validator through them any more.
