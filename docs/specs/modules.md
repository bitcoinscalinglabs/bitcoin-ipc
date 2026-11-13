# Modules

The programs and the contract that implement the protocol: what each one is, what it holds and what it trusts. The roles they serve are in [actors](lifecycle.md#actors), the paths and the interfaces between the two repositories in [code-map.md](code-map.md), and how the modules work together for checkpoints and EOTS finality, call by call, in [finality and checkpoints](../architecture/finality-and-checkpoints.md).

## IPC-aware node

An *IPC-aware node* is the set of processes that follows a subnet and verifies it against Bitcoin L1: a Bitcoin L1 node, a monitor, a provider, CometBFT, Fendermint and the Ethereum JSON-RPC API process. CometBFT and Fendermint together are the *subnet node*. A validator runs an IPC-aware node with its validator key and EOTS key. Anyone else can run one without keys.

## The modules

| Module | Program | Holds |
|---|---|---|
| monitor | `monitor` binary (bitcoin-ipc) | the state of every subnet ([state kept by the monitor](lifecycle.md#state-kept-by-the-monitor)), in its database. No key |
| provider | `provider` binary (bitcoin-ipc), a JSON-RPC server | the validator key, the EOTS key and the nonce seed of one validator, or no key for a user, and the records of the EOTS votes it has signed above the latest checkpointed height. Several providers on one machine can share the database of one monitor |
| CometBFT | `cometbft` binary (upstream CometBFT), the subnet's consensus engine | the subnet blocks |
| Fendermint | `fendermint run` (ipc), the application that executes subnet blocks | the subnet state, with the gateway |
| gateway | `GatewayDiamond` contract (ipc), which the subnet's genesis creates in the subnet state | the parent finality, the configuration updates and the subnet's validator set, the checkpoints and their signatures, the slash pre-signatures, the subnet final height, the cross-subnet messages |
| Ethereum JSON-RPC API | `fendermint eth run` (ipc) | no key |
| relayer | `ipc-cli checkpoint relayer` ([command](ipc-cli.md#run-the-checkpoint-relayer)) | no key in use: it loads the `ipc-cli` keystore, and with a Bitcoin L1 parent it signs nothing |
| `ipc-cli` | `ipc-cli` binary (ipc) ([commands](ipc-cli.md)) | the keystore of its user or validator |

CometBFT, Fendermint and the provider of one validator must hold the same validator key ([join](transactions/join.md#transaction-format)).

## Trust

Within one validator, the modules trust each other. The provider and Fendermint take the state of their monitor as true and do not check it again. For example, the collateral UTXOs that need slash pre-signatures (`getunslashableoutputs`) come from the monitor's state, without a new check that the monitor accepted each one.

Nodes do not trust each other's modules. They agree because all honest monitors hold the same state ([processing Bitcoin L1 blocks](lifecycle.md#processing-bitcoin-l1-blocks)), and a subnet block that depends on Bitcoin L1 state reads it at the same Bitcoin L1 height on every node: its parent finality ([applying configuration updates in the subnet](lifecycle.md#applying-configuration-updates-in-the-subnet)), and for EOTS votes the parent finality of the chain's latest executed subnet block ([subnet final height](slashing.md#subnet-final-height)).

A relayer needs no trust. It signs nothing, and Bitcoin L1 accepts a checkpoint only once, with the signatures of the configurations that lock its inputs. Any number of relayers can run for one subnet. `ipc-cli` trusts the provider and the Ethereum JSON-RPC API that its configuration names ([rules for consumers](slashing.md#rules-for-consumers)).
