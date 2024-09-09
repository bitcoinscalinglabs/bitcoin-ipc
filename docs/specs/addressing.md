## Subnet Address

### Bitcoin
The subnet Address we use for Bitcoin, in string representation, is `BTC`.

### L2 subnet over bitcoin
For an L2 subnet with bitcoin as L1, we use as subnet address the bitcoin address derived from `subnetPK` (see `subnet-pk.md`).

Implementation: The code currently uses a P2TR bitcoin address derived from `SubnetPK`. This gives a 64-byte output.

Example: An L2 subnet address could be `bcrt1p203537p7w99xlmrxqlmxywnjh6y8dlq6cf9appg5hph06gplfzqsedm67f`.

### L3+ subnets
For L3+ subnets we keep the addressing mechanism used in Fendermint. The subnet address of an L3, for example,
will be assigned by its parent L2 subnet, hence we let the existing Fendermint L2 implementation handle it.


## Subnet ID
We follow the [IPC Specs](https://github.com/consensus-shipyard/ipc/blob/main/specs/addressing.md) and uniquely identify each IPC subnet by a `subnetId`, a structure that consists of the `Subnet Address` of each subnet in the hierarchy from the root subnet to the subnet of interest.

### String representation
The string representation of the subnet ID uses `/` as a divider.
For example, an L2 subnet could be `BTC/A`.

Example: An L2 subnetId could be `BTC/bcrt1p203537p7w99xlmrxqlmxywnjh6y8dlq6cf9appg5hph06gplfzqsedm67f`.