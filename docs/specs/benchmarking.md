# Benchmarking the size of transfer()

We want to compare the virtual size of data written on bitcoin (in virtual bytes, or vB) for transferring some amount between
1. two bitcoin users
2. two IPC users with accounts on IPC L2 subnets.

The first case always consumes 141 vB, which is the bitcoin size of a transaction with one input (a UTXO owned by the sender) and two outputs (a UTXO locked with the recipient's address and a change UTXO). Hence, *N* transfers require *141N* vB.

In the second case, we can batch multiple transfers, which leads to a lower amortized size per transfer. The logic is the following (see `transactions.md` for details): Periodically we read the postbox of an L2 subnet *A* and batch together all transfers that are found there. Obviously, all batched transfers have the same source subnet, but they may have different target subnets, e.g., we may batch two transfers from *A* to *B* and two from *A* to *C*.
The code creates two bitcoin transactions, a *commit* and a *reveal* transaction. The commit transaction contains one input UTXO for the source subnet and one output UTXO per target subnet, as well as one output UTXO locked with the hash of a script. The reveal transaction uses this script-UTXO as input and reveals the full script in the witness. The full script contains the details of each transfer, that is, the recipient account and the amount.

### Running the benchmark
We measure the virtual size of the batched transfers in `benches/measure_transfer_weight.rs`, for multiple numbers of target subnet (variable `number_of_subnets`). The benchmark batches a number of transfers (variable `total_transfers`), equally split among all target subnets, and then creates the required bitcoin transactions using the functionality in `src/ipc-lib.rs`.

To run the benchmark, start a local `bitcoin core` node and `btc_monitor` (following the Setup steps and steps 1 and 2 on `README.md`).
Then you can use
```
cargo run --bin measure_transfer_weight
```

The code first creates the required number of subnets, if they are not already created, and then runs the benchmark.

### Results
The code writes the result in `outputs/transfer.csv`. We then manually paste the content in a [Spreadsheet file](https://docs.google.com/spreadsheets/d/1VZtpPHY2IwF11sb3uXlqa6nXgET-CbNxcq4vbO-lqiU/edit?usp=sharing) and do the following analysis.

- The benchmark outputs the size (in vB) of the commit and the reveal transactions.
- We add the size of both transactions.
- We divide with the total number of transfers in the batch, which gives us the *amortized size* of each transfer.
- We plot the amortized size per transfer vs total number of transfers.

In the following diagram we see the result.
![Transfer virtual size](../bench-plots/transfer-size-detail.png)

In the plot we see the following:
- Using native bitcoin transfers, the size per transfers remains, of course, constant, at 141 vB.
- The plot shows one line per number of target IPC subnets (for 1, 2, 5, 10 subnets).
- Using the IPC infrastructure, independently of the number of target subnets, the amortized cost per transfer drops.
- The more the target subnets, the more expensive the batched transfer is. This is because
    1. The commit transaction contains one output UTXO per target subnet.
    2. The reveal transaction contains (only once) the address of each target subnet.
- The usage of IPC subnets starts paying off if we batch at least 3 to 5 transfers, depending on the number of target subnets. For example:
    - If all transfers have the same target subnet, then IPC can batch 3 transfers using  ~130 vB on average for each, which is cheaper than the 141 vB of native bitcoin.
    - If the transfers have two different target subnets, then IPC needs a bit more space to encode them, ~150 vB per transfer for 3 transfers, and ~117 vB per transfer for 4 transfers, hence IPC saves space if we batch at least 4 transfers.
    - Similarly, if the transfers have five or ten different target subnets, IPC pays off we batch at least 5 of them.

### Taking batching to the limits
The limit for batching transfers is the limit on the *witness* field in a bitcoin transaction, which equals the block size limit.
To reach this limit, we increase the batched number of transactions, as long as *both* the commit and the reveal transactions fit in less that 1M vB.

We get the following result.
![Transfer virtual size](../bench-plots/transfer-size-all-log.png)

From this plot we can see that
- For any number of target subnets, the amortized virtual size per transfer using IPC converges to 20.26 vB. This is a 7-times compression, compared to standard bitcoin without IPC L2 networks.


### General observations

Observations from the data on the [Spreadsheet file](https://docs.google.com/spreadsheets/d/1VZtpPHY2IwF11sb3uXlqa6nXgET-CbNxcq4vbO-lqiU/edit?usp=sharing) and these plots:
- The size of the commit transaction only depends on the number of target subnets, as it includes one output UTXO per target subnet. It adds +43 vB for each target subnet.
- The reveal transaction grows with every transfer we add and with every subnet.
- The reveal transaction grows faster than the commit transaction, and it is the first to reach the 1M vB limit.

### Conclusion
Essentially, IPC offers a throughput-latency tradeoff: An IPC subnet, either periodically or when a certain number of outgoing transfers become finalized in it, creates a batch and submits it to bitcoin. The bigger the batch is the cheaper it will be, in terms of bytes writen to bitcoin, but also the more time it takes to fill the batch.

For large enough batches, our experiments show that we can reach a *compression factor* of 7.