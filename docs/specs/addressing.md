## Subnet ID

### Bitcoin
The Subnet ID we use for Bitcoin is `BTC`.

### L2+ Subnets
Following the [IPC Specs](https://github.com/consensus-shipyard/ipc/blob/main/specs/addressing.md), each IPC subnet is uniquely identified by a `subnet-id`, which contains the ID of the root subnet and the IDs of all intermediate subnets.

### String representation
The string representation of the subnet ID uses `/` as a divider.
For example, an L2 subnet could be `BTC/A`.