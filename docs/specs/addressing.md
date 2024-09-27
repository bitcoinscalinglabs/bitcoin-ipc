## Subnet ID
We follow the [IPC Specs](https://github.com/consensus-shipyard/ipc/blob/main/specs/addressing.md) and uniquely identify each IPC subnet by a `subnetId`, a structure that consists of the `Subnet Address` of each subnet in the hierarchy from the root subnet to the subnet of interest.

### String representation
The string representation of the subnet ID uses `/` as a divider.
For example, an L2 subnet could be `BTC/A`.

Example: An L2 subnetId could be `BTC/fc6631b36052648937cfbedb718f17363d467a546f1cfd84d821edd1fb9d50f3`.


## Subnet Address
### Bitcoin
The subnet Address we use for Bitcoin, in string representation, is `BTC`.

### L2 subnet over bitcoin
For L2 subnets, the `Subnet Address` is the SHA256 hash of the bitcoin transaction (the `txid`, in bitcoin terminology) of the *createChild()* command(specifically, the commit transaction, as *createChild()* is implemented with the commit-reveal technique).

Example: An L2 subnet address could be `fc6631b36052648937cfbedb718f17363d467a546f1cfd84d821edd1fb9d50f3`.

### L3+ subnets
For L3+ subnets we keep the addressing mechanism used in Fendermint. The subnet address of an L3, for example,
will be assigned by its parent L2 subnet, hence we let the existing Fendermint L2 implementation handle it.

