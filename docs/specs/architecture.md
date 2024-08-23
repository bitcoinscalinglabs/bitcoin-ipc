Definitions:
- An *IPC-enabled node* is a bitcoin full node with IPC integration, i.e., that supports viewing, creating, and joining existing IPC subnets on Bitcoin.
- `IpcState`: See state-types.md
- `SubnetState` and `checkpoint()`: See state-types.md
- IPC commands and terminology, such as `createChild`, `joinChild`, `transfer`, `propagate`, `deposit`, `withdraw`, `postBox`: see `scope-of-work.md`.

## Software stack on an IPC-enabled node
An IPC-enabled node runs all the following modules.

### Bitcoin full node
A bitcoin node must be available to connect to over RPC. This can be either a local node running Bitcoin Core (all examples and demos in this repo assume this approach) or a public bitcoin full node.

### IPC L1 Manager
It is responsible for keeping track of the `IpcState`.
It also exposes an interface that allows the user to modify it (e.g., create new subnet or join an existing).
It consists of two submodules, the Bitcoin Monitor and the BTC-IPC library.

### Bitcoin Monitor
It monitors the bitcoin chain (using the Bitcoin full node over RPC) for IPC-related transactions.
This could be, for example, a create-subnet command, submitted by another bitcoin user.
Whenever such a transaction is detected and becomes final, the Bitcoin Monitor parses it and communicates the result back to the IPC L1 Manager.

### BTC-IPC library
It contains all the necessary logic for translating IPC commands (such as `createChild`, `joinChild`, submit a checkpoint from a subnet, propagate subnet transactions for one subnet to another) to bitcoin transactions.
It is used by the IPC L1 Manager and the Relayer modules.

### Subnet Simulator
An IPC-enabled node that decides to join a child runs a validator for that subnet.

As explained in `scope-of-work.md`, in the first stage a `Subnet Simulator` will be used to instantiate the subnet. This is a mock implementation of a validator that runs locally on the IPC-enabled node and connects to no other validators (equivalent to a single-validator subnet). It maintains the `SubnetState` and exposes a simple interface, simulating a token-transfer application.

In later Stages Fendermint nodes will be used as the validator.

### Relayer
It is responsible for monitoring all IPC subnets and relaying necessary information to bitcoin.
Specifically:
- It periodically checks the `postBox` of each subnet. If there are cross-chain `transfer` or `withdraw` commands, it uses the BTC-IPC library to submit the appropriate transaction to bitcoin.
- It periodically calls `checkpoint()` on the subnet and submits the checkpoint to bitcoin, using the BTC-IPC library.

To achieve these, it connects to all the validators of the subnet. In the initial stages, where the subnet is instantiated by a Subnet Simulator, the Relayer only uses the interface of the Subnet Simulator to read the postbox and perform the checkpointing.
In later stages, when the Fendermint node will be used to instantiate a subnet, the Relayer will connect to it using the means provided by Fendermint.

### Subnet Interactor
The Subnet Interactor is not mandatory on an IPC-enabled node.
It allows the users of a subnet to interact with it.

The Subnet Interactor connects to all the validators of the subnet in order to submit subnet-related commands.
Similar to what we describe with the Relayer, in the initial stages, where the subnet is instantiated by a `Subnet Simulator` running locally (see `scope-of-work.md`), the Subnet Interactor only uses the interface of the `Subnet Simulator`.
In later stages, when the Fendermint node will be used to instantiate a subnet, the Subnet Interactor will actually connect to all subnet validators.

