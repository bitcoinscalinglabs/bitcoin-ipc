# Bitcoin-IPC specification

This directory specifies the Bitcoin-IPC protocol, implemented by bitcoin-ipc and ipc: the lifecycle of a subnet and the state every monitor derives from Bitcoin L1, every transaction the protocol uses (its format, how its sender builds it and how monitors verify it), and the `ipc-cli` commands with a Bitcoin L1 parent. It says what an implementation must do in every case, so that an engineer can implement the protocol from it and an audit can check the code against it.

Each commit holds the specification of that commit, and a tag holds the specification of that version.

## Scope

Validators join with their collateral in their own output, exit unilaterally after the collateral's timeout or earlier with the subnet's consent (cooperative exit), lose their collateral to a slash transaction if they equivocate, and refresh their collateral once per epoch. The configuration changes only at an epoch cutoff, and the rotation checkpoint of the new epoch moves the subnet's funds to it. A checkpoint spends only outputs of final transactions, so no checkpoint spends the output of a checkpoint that is not final.

## Conventions

- "Bitcoin L1" is the Bitcoin network a deployment runs on. Network-specific values are parameters ([lifecycle.md](lifecycle.md#parameters)).
- A rule written with "must" is a requirement on every implementation. The monitor ignores a message that breaks a rule, and that message changes no state. The monitor still records every spend of an output it holds.
- Names in code font are the code's names where they exist.

## Documents

- [lifecycle.md](lifecycle.md): actors, parameters, the lifecycle of a subnet from creation to its end with its configurations (`C_0`, `C_gen`, `C_1`, `C_e`), subnet status, the state the monitor keeps, how the monitor processes blocks, and the conditions on the parameter values with the assumptions behind them.
- [slashing.md](slashing.md): slashing through the life of a subnet: EOTS keys and nonce commitments, votes on the subnet, the pre-signatures of every collateral and unbonding output and the checkpoints that carry them, the monitor's checks, and the slashing of a validator that equivocated.
- [encoding.md](encoding.md): fee rate and dust, subnet ids, postcard encoding, OP_RETURN outputs, commit-reveal pairs, configuration multisigs, the collateral and unbonding scripts, and EOTS nonce commitments.
- Transactions, in lifecycle order:
  - [create-subnet.md](transactions/create-subnet.md): create a subnet (`IPCCRT`).
  - [prefund.md](transactions/prefund.md): deposit before bootstrap into the genesis balance (`IPCPFD`).
  - [join.md](transactions/join.md): join with the collateral in the validator's own output (`IPCJUE`).
  - [stake.md](transactions/stake.md): add or renew a collateral of a validator that has joined (`IPCSUE`).
  - [deposit.md](transactions/deposit.md): deposit and fee top-up (`IPCFND`).
  - [checkpoint.md](transactions/checkpoint.md): the checkpoint, including the first checkpoint and the kill checkpoint (`IPCCPT`).
  - [batch-transfer.md](transactions/batch-transfer.md): the checkpoint's reveal with transfers, ERC20 records and slash pre-signatures (`IPCTFR`).
  - [exit.md](transactions/exit.md): spend matured collaterals and unbonding outputs (no tag).
  - [slash.md](transactions/slash.md): burn the collateral of an equivocating validator (no tag).
- [ipc-cli.md](ipc-cli.md): `ipc-cli` commands with a Bitcoin L1 parent and the provider methods they call.
