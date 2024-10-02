## Benchmarking the size of transfer()

We want to compare the virtual size of data written on bitcoin (in virtual bytes, or vB) for transferring some amount between
1. two bitcoin users
2. two IPC users with accounts on IPC L2 subnets.

The first case always consumes 141 vB, which is the bitcoin size of a transaction with one input (a UTXO owned by the sender) and two outputs (a UTXO locked with the recipient's address and a change UTXO). Hence, *N* transfers require *141N* vB.

In the second case, we can batch multiple transfers, which leads to a lower amortized size per transfer. The logic is the following (see `transactions.md` for details): Periodically we read the postbox of an L2 subnet *A* and batch together all transfers that are found there. Obviously, all batched transfers have the same source subnet, but they may have different target subnets, e.g., we may batch two transfers from *A* to *B* and two from *A* to *C*. 

### The benchmark
We measure the virtual size of the batched transfers in `measure_transfer_weight.rs`, for multiple numbers of target subnet (variable `number_of_subnets`). The benchmark batches a number of transfers (variable `total_transfers`), equally split among all target subnets, and then creates the bitcoin transactions needed to submit them to bitcoin (two transactions, a *commit* and a *reveall* transaction) using the functionality in `ipc-lib.rs`. The code writes the result in `outputs/transfer.csv`. We then manually paste the content in a [Spreadsheet file](https://docs.google.com/spreadsheets/d/1VZtpPHY2IwF11sb3uXlqa6nXgET-CbNxcq4vbO-lqiU/edit?usp=sharing) and do the following analysis.

- The benchmark outputs the size (in vB) of the commit and the reveal transactions.
- We only consider those numbers of transfers, for which both the commit and the reveal transactions fit in less that 1M vB (the bitcoin limit for a transaction).
- We add the size of both transactions.
- We divide with the total number of transfers in the batch, which gives us the *amortized size* of each transfer.


 In the following diagram we see the result.

 Observations:
 - The size of the commit transaction only depends on the number of target subnets, as it includes one output UTXO per target subnet. It adds +43 vB for each target subnet.
 - The reveal transaction grows with every transfer we add and with every subnet.
 - The reveal transaction grows faster than the commit transaction, and it is the first to reach the 1M vB limit.