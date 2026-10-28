# Deposit and fee top-up

With a deposit, a user sends BTC into a subnet, which credits it to an EVM address (a top-down fund message). With a fee top-up, a validator adds BTC to the subnet's fee reserve. A fee top-up credits nobody. The code calls the deposit "fund" (tag `IPCFND`).

## Transaction format

One transaction, funded by the depositor's wallet:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | 0 | OP_RETURN with 46 data bytes (deposit) or 26 data bytes (fee top-up) |
| 1 | the deposited amount | `MS(C)` of the subnet's current configuration `C` |
| 2 | wallet change | optional |

OP_RETURN data:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCFND` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 20 | the EVM address to credit (deposit only) |

## Creating the transaction

The depositor builds a deposit for a subnet in epoch 0 or active, and a fee top-up for a subnet in epoch 0, active or to be killed, paying the multisig of the current configuration (`C_0` in epoch 0). The provider's `fundsubnet` call does this for its wallet.

## Verifying the transaction

1. A monitor finds the message in output 0: an OP_RETURN with 46 or 26 data bytes starting with `IPCFND`.
2. For a deposit, the subnet must be in epoch 0 or active. For a fee top-up, it must be in epoch 0, active or to be killed.
3. Output 1 must have a value `≥ min_deposit` and pay `MS(C)` of the subnet's current configuration (`C_0` in epoch 0), or of an earlier configuration `C_e` (`e ≥ 0`) if the deposit's block height is at most `h_e + K/2`, with `h_0 = H_0` for `C_0` ([delayed arrivals](checkpoint.md#fund-utxos)). The monitor ignores any other deposit.
4. For a deposit, the monitor records a fund rootnet message for the subnet: the address, the amount (output 1's value), the next message nonce of the subnet and the Bitcoin block. For a fee top-up, it records no message, and the output is part of the fee reserve.
5. The monitor adds the outputs of both to the [fund UTXOs](checkpoint.md#fund-utxos) of the configuration, which checkpoints spend. Deposits back the BTC that the subnet mints. A fee top-up adds its value to the subnet's [fee reserve](checkpoint.md#fund-utxos), which alone pays for checkpoints.

Before bootstrap, BTC enters the subnet only through a [prefund](prefund.md). Deposits and fee top-ups submitted in epoch 0, between bootstrap and activation, pay `MS(C_0)`, and the first checkpoint spends them. The subnet is inactive in epoch 0 and mints nothing then.
