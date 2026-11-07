# Deposit and fee top-up

With a deposit transaction, a user sends BTC into a subnet, which credits it to an EVM address (a top-down fund message). With a fee top-up transaction, a validator adds BTC to the subnet's fee reserve (fee top-up **unimplemented**). A fee top-up credits nobody. The code calls the deposit "fund" (tag `IPCFND`).

## Transaction format

One transaction, funded by the depositor's wallet:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | 0 | OP_RETURN with 46 data bytes (deposit) or 26 data bytes (fee top-up **unimplemented**) |
| 1 | the deposited amount | `MS(C)` of the subnet's current configuration `C` |
| 2 | wallet change | optional |

OP_RETURN data:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCFND` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 20 | the EVM address to credit (deposit only) |

## Creating the transaction

The depositor builds a deposit transaction for a subnet in epoch 0 or active, and a fee top-up transaction (fee top-up **unimplemented**) for a subnet in epoch 0, active or to be killed, paying the multisig of the current configuration (`C_wlist` in epoch 0). The provider's `fundsubnet` call does this for its wallet.

## Verifying the transaction

1. A monitor finds the message in output 0: an OP_RETURN with 46 or 26 data bytes starting with `IPCFND`.
2. For a deposit transaction, the subnet must be in epoch 0 or active. For a fee top-up transaction (fee top-up **unimplemented**), it must be in epoch 0, active or to be killed.
3. Output 1 must have a value `≥ min_deposit` and pay `MS(C)` (the `MS(C)` check **unimplemented**) of the subnet's current configuration (`C_wlist` in epoch 0), or of an earlier configuration `C_e` (`e ≥ 1`) or `C_wlist` if the block height of the transaction is at most `H_e + K/2`, where `H_e` is the cutoff that ends epoch `e` (`H_0` for `C_wlist`) ([delayed arrivals](checkpoint.md#fund-utxos)). The monitor ignores any other deposit or fee top-up transaction.
4. For a deposit transaction, the monitor records a fund rootnet message for the subnet: the address, the amount (output 1's value), the next message nonce of the subnet and the Bitcoin block that includes the transaction. The subnet credits the amount once its parent finality reaches that Bitcoin block. For a fee top-up transaction, it records no message, and the output is part of the fee reserve.
5. The monitor adds output 1 of both to the fund UTXOs of the configuration, which checkpoints spend. Deposits back the BTC that the subnet mints. A fee top-up adds its value to the subnet's fee reserve, which alone pays for checkpoints.

Before bootstrap, BTC enters the subnet only through a [prefund transaction](prefund.md). Deposit and fee top-up transactions submitted in epoch 0, between bootstrap and activation, pay `MS(C_wlist)`: each is a fund UTXO of `C_wlist`, which the checkpoints of epoch 0 and the rotation checkpoint of epoch 1 spend. The subnet credits a deposit transaction of epoch 0 as after activation, once its parent finality reaches the Bitcoin block that includes the transaction ([epoch 0](../lifecycle.md#epoch-0)).
