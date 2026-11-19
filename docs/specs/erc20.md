# ERC20 tokens

A token issued on one subnet can move to other subnets. A token is identified by its *home subnet* and its *home token address*, the address of its ERC20 contract on the home subnet. This document gives the life of a token: registration, how the registration reaches the other subnets, how a transfer is carried and how the supply is accounted. The format and the monitor's checks of the data are in the [batch-transfer transaction](transactions/batch-transfer.md).

## Registration

The owner of an ERC20 contract registers it on its subnet through the subnet's gateway contract. The gateway reads the token's name, symbol, decimals and total supply. The total supply at registration is the token's *initial supply*. A token is registered once on its home subnet. Only the owner of the token contract can register it.

The registration enters the next checkpoint of the home subnet and its batch-transfer transaction (`token_registrations`). The monitor then:

- sets the home subnet's balance of the token to the initial supply,
- records an ERC20 registration rootnet message for every other subnet.

## Recipient subnets

A subnet that receives an ERC20 registration rootnet message records the token's name, symbol and decimals under (home subnet, home token address). It deploys nothing. The first record stays.

The first ERC20 transfer of the token to a non-home subnet deploys the *wrapped token* of that subnet: an ERC20 contract with the recorded name, symbol and decimals, whose only minter and burner is the subnet's gateway. A transfer of a token whose registration the subnet has not recorded fails on that subnet.

## Transfers

A user transfers a token with `ipc-cli cross-msg transfer-erc` ([ipc-cli](ipc-cli.md)). The transfer enters the next checkpoint of the source subnet and its batch-transfer transaction (`token_transfers`), and the monitor records an ERC20 transfer rootnet message for the destination subnet.

| Source | Source subnet | Destination subnet |
|---|---|---|
| the home subnet | locks the amount in its gateway | the home subnet: releases the amount from the lock. Another subnet: mints the amount of the wrapped token |
| another subnet | burns the amount of the wrapped token | the home subnet: releases the amount from the lock. Another subnet: mints the amount of the wrapped token |

The tokens locked on the home subnet always equal the tokens that exist as wrapped tokens on other subnets.

## Supply

The home subnet's total supply of a token can change after registration by a mint or a burn. At each checkpoint, the home subnet reports the net change of the total supply since the previous checkpoint (`token_supply_adjustments`). Mints and burns of wrapped tokens on other subnets are not reported, since each follows an ERC20 transfer that the monitor has recorded.

The monitor keeps, per token and subnet, the *token balance*: the amount the subnet holds ([state kept by the monitor](lifecycle.md#state-kept-by-the-monitor)). The balances of a token sum to its total supply on the home subnet.

- A registration sets the home subnet's balance to the initial supply.
- A reported mint raises, and a reported burn lowers, the home subnet's balance.
- An ERC20 transfer moves its amount from the source subnet's balance to the destination's.

A subnet must never send more of a token than its balance. The monitor rejects a batch-transfer transaction that breaks this ([ERC20 rules](transactions/batch-transfer.md#verifying-the-transaction)). The monitor does not hold token metadata. It only appears in the ERC20 registration rootnet messages.
