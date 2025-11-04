import csv
import matplotlib.pyplot as plt
import numpy as np


csv_file_transfer = 'bench-plots/bench-transfer-sizes.csv'
csv_file_withdraw = 'bench-plots/bench-withdraw-sizes.csv'
n_transfers_list_default = [1, 2, 3, 4, 5, 10, 20, 30, 50]

fee_per_vb = 3.0  # Fee per vbyte for cost analysis

markers_subnets = ['o', 's', 'D', 'v']
markers_validators = ['*', 'H', 'X', '2', '+', 'p']
markers_transfers = ['*', 'H', 'X', '2', '+', 'p']

# Third plot: amortized size per transfer vs total number of transfers, for n_validators=4, one line per n_destination_subnets
def plot3(filename, title, title_fee, n_transfers_list, x_ticks, log_scale=False):
    n_target_subnets_list = [1, 2, 5, 10]
    transfers_dict = {n: [] for n in n_target_subnets_list}
    avg_sizes_dict = {n: [] for n in n_target_subnets_list}

    with open(csv_file_transfer, newline='') as f:
        reader = csv.DictReader(f)
        for row in reader:
            if int(row['n_validators']) == 4:
                n_subnets = int(row['n_destination_subnets'])
                n_transfers = int(row['n_transfers'])
                if n_subnets in n_target_subnets_list and n_transfers in n_transfers_list and n_transfers >= n_subnets:
                    commit_vsize = int(row['checkpoint_tx_size'])
                    reveal_vsize = int(row['transfer_tx_size'])
                    total_size = commit_vsize + reveal_vsize
                    transfers_dict[n_subnets].append(n_transfers)
                    avg_sizes_dict[n_subnets].append(total_size / float(n_transfers))
                    if n_transfers == 15000:
                        print(f'n_validators=4, n_transfers={n_transfers}, n_subnets={n_subnets}, amortized_size={total_size / float(n_transfers)}')

    # First plot: size in vbytes
    plt.figure(figsize=(8, 5))
    for i, n_subnets in enumerate(n_target_subnets_list):
        # Sort by n_transfers for each line for better plot
        pairs = sorted(zip(transfers_dict[n_subnets], avg_sizes_dict[n_subnets]))
        if pairs:
            x, y = zip(*pairs)
            plt.plot(x, y, marker=markers_subnets[i % len(markers_subnets)], label=f'{n_subnets} target subnets')

    # Add Native bitcoin line
    defined_x_points = set()
    for n_subnets in n_target_subnets_list:
        defined_x_points.update(transfers_dict[n_subnets])
    native_bitcoin_x = sorted(defined_x_points)
    native_bitcoin_y = [141 for _ in native_bitcoin_x]
    plt.plot(native_bitcoin_x, native_bitcoin_y, linestyle='--', color='black', label='Native bitcoin')

    plt.xlabel('Total number of batched transfers (to all destination subnets)')
    if log_scale:
        plt.xlabel('Total number of batched transfers (to all destination subnets), log scale')
    plt.ylabel('Amortized size per transfer (vbytes)')
    plt.title(title)
    plt.grid(True)
    plt.legend()
    plt.xticks(x_ticks)
    if log_scale:
        plt.xscale('log')
    plt.tight_layout()
    plt.savefig(filename)

    # Second plot: with fee multiplication
    plt.figure(figsize=(8, 5))
    for i, n_subnets in enumerate(n_target_subnets_list):
        # Sort by n_transfers for each line for better plot
        pairs = sorted(zip(transfers_dict[n_subnets], avg_sizes_dict[n_subnets]))
        if pairs:
            x, y = zip(*pairs)
            y_fee = [size * fee_per_vb for size in y]
            plt.plot(x, y_fee, marker=markers_subnets[i % len(markers_subnets)], label=f'{n_subnets} target subnets')

    # Add Native bitcoin line (also multiplied by fee)
    native_bitcoin_y_fee = [141 * fee_per_vb for _ in native_bitcoin_x]
    plt.plot(native_bitcoin_x, native_bitcoin_y_fee, linestyle='--', color='black', label='Native bitcoin')
    plt.xlabel('Total number of batched transfers (to all destination subnets)')
    if log_scale:
        plt.xlabel('Total number of batched transfers (to all destination subnets), log scale')
    plt.ylabel(f'Amortized fee per transfer (in sats)')
    # plt.title(f'{title} , using fee rate {fee_per_vb} sat / vB)- Cost Analysis')
    plt.title(title_fee)
    plt.grid(True)
    if log_scale:
        plt.xscale('log')
    plt.legend()
    plt.xticks(x_ticks)
    plt.tight_layout()
    plt.savefig(filename.replace('.png', '_fee.png'))

# Seventh plot: throughput vs total number of batched transfers
def plot7(n_transfers_list, x_ticks):
    n_target_subnets_list = [1, 2, 5, 10]
    transfers_dict = {n: [] for n in n_target_subnets_list}
    throughput_dict = {n: [] for n in n_target_subnets_list}

    with open(csv_file_transfer, newline='') as f:
        reader = csv.DictReader(f)
        for row in reader:
            if int(row['n_validators']) == 4:
                n_subnets = int(row['n_destination_subnets'])
                n_transfers = int(row['n_transfers'])
                if n_subnets in n_target_subnets_list and n_transfers in n_transfers_list and n_transfers >= n_subnets:
                    commit_vsize = int(row['checkpoint_tx_size'])
                    reveal_vsize = int(row['transfer_tx_size'])
                    total_size = commit_vsize + reveal_vsize
                    amortized_vsize = total_size / float(n_transfers)
                    throughput = 7 * (141 / amortized_vsize)
                    transfers_dict[n_subnets].append(n_transfers)
                    throughput_dict[n_subnets].append(throughput)

    plt.figure(figsize=(8, 5))
    for i, n_subnets in enumerate(n_target_subnets_list):
        # Sort by n_transfers for each line for better plot
        pairs = sorted(zip(transfers_dict[n_subnets], throughput_dict[n_subnets]))
        if pairs:
            x, y = zip(*pairs)
            plt.plot(x, y, marker=markers_subnets[i % len(markers_subnets)], label=f'{n_subnets} target subnets')

    # Add Native bitcoin line (throughput = 7 * (141 / 141) = 7)
    defined_x_points = set()
    for n_subnets in n_target_subnets_list:
        defined_x_points.update(transfers_dict[n_subnets])
    native_bitcoin_x = sorted(defined_x_points)
    native_bitcoin_y = [7 for _ in native_bitcoin_x]
    plt.plot(native_bitcoin_x, native_bitcoin_y, linestyle='--', color='black', label='Native bitcoin')

    plt.xlabel('Total number of batched transfers (to all destination subnets)')
    plt.ylabel('Throughput (tps)')
    plt.title('Attainable throughput (tps) vs. Total number of batched transfers (4 validators)')
    plt.grid(True)
    plt.legend()
    plt.xscale('log')
    plt.xticks(x_ticks)
    current_yticks = list(plt.yticks()[0])
    if 7 not in current_yticks:
        current_yticks.append(7)
        current_yticks = sorted(current_yticks)
        plt.yticks(current_yticks)
    plt.ylim(bottom=-3)
    plt.tight_layout()
    plt.savefig('bench-plots/7-throughput_vs_n_transfers.png')


# First plot: total size of transfers vs number of validators, for varying number of transfers
validators_dict = {n: [] for n in n_transfers_list_default}
total_sizes_dict = {n: [] for n in n_transfers_list_default}

with open(csv_file_transfer, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        if row['n_destination_subnets'] == '1':
            n_transfers = int(row['n_transfers'])
            if n_transfers in n_transfers_list_default:
                num_validators = int(row['n_validators'])
                commit_vsize = int(row['checkpoint_tx_size'])
                reveal_vsize = int(row['transfer_tx_size'])
                total_size = commit_vsize + reveal_vsize
                validators_dict[n_transfers].append(num_validators)
                total_sizes_dict[n_transfers].append(total_size)

plt.figure(figsize=(8, 5))
for i, n_transfers in enumerate(n_transfers_list_default):
    plt.plot(validators_dict[n_transfers], total_sizes_dict[n_transfers], marker=markers_transfers[i % len(markers_transfers)], label=f'{n_transfers} total transfers')

plt.xlabel('Number of validators in each subnet')
plt.ylabel('Total size of transfers (vbytes)')
plt.title('Total size of transfers vs Number of validators (1 target subnet)')
plt.grid(True)
plt.legend()
plt.tight_layout()
plt.savefig('bench-plots/1-total_vsize_vs_n_validators_vary_n_transfers.png')
# plt.show()


# Second plot: average size per transfer vs number of validators, for varying number of transfers
n_transfers_list_for_plot = [1, 2, 5, 10, 50, 100]
n_validators_list_for_plot = [1, 7, 16, 25, 37, 52, 76, 100]
n_subnets_for_plot = 1
validators_dict = {n: [] for n in n_transfers_list_for_plot}
total_sizes_dict = {n: [] for n in n_transfers_list_for_plot}

with open(csv_file_transfer, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        if row['n_destination_subnets'] == str(n_subnets_for_plot):
            n_transfers = int(row['n_transfers'])
            if n_transfers >= n_subnets_for_plot:
                if n_transfers in n_transfers_list_for_plot:
                    num_validators = int(row['n_validators'])
                    if num_validators in n_validators_list_for_plot:
                        commit_vsize = int(row['checkpoint_tx_size'])
                        reveal_vsize = int(row['transfer_tx_size'])
                        total_size = commit_vsize + reveal_vsize
                        validators_dict[n_transfers].append(num_validators)
                        total_sizes_dict[n_transfers].append(total_size)

# First plot: size in vbytes
plt.figure(figsize=(8, 5))
for i, n_transfers in enumerate(n_transfers_list_for_plot):
    avg_sizes = [total / float(n_transfers) for total in total_sizes_dict[n_transfers]]
    plt.plot(validators_dict[n_transfers], avg_sizes, marker=markers_transfers[i % len(markers_transfers)], label=f'{n_transfers} total transfers')

plt.xlabel('Number of validators in source subnet')
plt.ylabel('Amortized size per transfer (vbytes)')
plt.title(f'Amortized size per transfer vs Number of validators (1 target subnet)')
plt.grid(True)
plt.legend()
plt.xticks(n_validators_list_for_plot)
plt.tight_layout()
plt.savefig(f'bench-plots/2-amortized_vsize_vs_n_validators_vary_n_transfers_{n_subnets_for_plot}_subnets.png')

# Second plot: with fee multiplication
plt.figure(figsize=(8, 5))
for i, n_transfers in enumerate(n_transfers_list_for_plot):
    avg_sizes = [total / float(n_transfers) for total in total_sizes_dict[n_transfers]]
    avg_sizes_fee = [size * fee_per_vb for size in avg_sizes]
    plt.plot(validators_dict[n_transfers], avg_sizes_fee, marker=markers_transfers[i % len(markers_transfers)], label=f'{n_transfers} total transfers')

plt.xlabel('Number of validators in source subnet')
plt.ylabel(f'Amortized fee per transfer (in sats)')
plt.title(f'Fee per transfer vs Number of validators (1 target subnet, fee rate = {fee_per_vb} sat / vB)')
plt.grid(True)
plt.legend()
plt.xticks(n_validators_list_for_plot)
plt.tight_layout()
plt.savefig(f'bench-plots/2-amortized_vsize_vs_n_validators_vary_n_transfers_{n_subnets_for_plot}_subnets_fee.png')
# plt.show()


# Third plot
plot3('bench-plots/3-amortized_vsize_vs_n_transfers_vary_n_subnets.png', 'Amortized size per transfer vs. total number of transfers (4 validators)', f'Fee per transfer vs. total number of transfers (4 validators, fee rate = {fee_per_vb} sat / vB)', n_transfers_list_default, [5,10,15,20,25,30,35,40,45,50])

# Third plot, take it to limits
plot3('bench-plots/3B-amortized_vsize_vs_n_transfers_vary_n_subnets.png', 'Amortized size per transfer vs. total number of transfers (4 validators)', f'Fee per transfer vs. total number of transfers (4 validators, fee rate = {fee_per_vb} sat / vB)',[1,2,3,5, 10,20,50, 100, 200, 500, 1000, 10000, 15000], [0,1000, 2000, 5000, 10000, 15000])

# Third plot, zoom in
plot3('bench-plots/3C-amortized_vsize_vs_n_transfers_vary_n_subnets.png', 'Amortized size per transfer vs. total number of transfers (4 validators, zoom-in)', f'Fee per transfer vs. total number of transfers (4 validators, fee rate = {fee_per_vb} sat / vB)', [1,2,3,5, 10], [1,2,3,5, 10])

# Third plot, log scale
plot3('bench-plots/3D-amortized_vsize_vs_n_transfers_vary_n_subnets_log.png', 'Amortized size per transfer vs. total number of transfers (4 validators)', f'Fee per transfer vs. total number of transfers (4 validators, fee rate = {fee_per_vb} sat / vB)', [1,2,3,5, 10,20,50, 100, 200, 500, 1000, 10000, 16500], [1, 10, 100, 1000, 10000, 16500], log_scale=True)


# Fourth plot: amortized size per transfer vs total number of transfers, for varying n_validators and n_destination_subnets=1
n_validators_list = [4, 10, 25, 52, 76, 100]
n_transfers_list_for_plot = n_transfers_list_default
n_subnets_for_plot = 1
transfers_dict = {n: [] for n in n_validators_list}
avg_sizes_dict = {n: [] for n in n_validators_list}

with open(csv_file_transfer, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        n_validators = int(row['n_validators'])
        n_subnets = int(row['n_destination_subnets'])
        n_transfers = int(row['n_transfers'])
        if n_validators in n_validators_list and n_subnets == n_subnets_for_plot and n_transfers in n_transfers_list_for_plot and n_transfers >= n_subnets:
            commit_vsize = int(row['checkpoint_tx_size'])
            reveal_vsize = int(row['transfer_tx_size'])
            total_size = commit_vsize + reveal_vsize
            transfers_dict[n_validators].append(n_transfers)
            avg_sizes_dict[n_validators].append(total_size / float(n_transfers))

plt.figure(figsize=(8, 5))

for i, n_validators in enumerate(n_validators_list):
    # Sort by n_transfers for each line for better plot
    pairs = sorted(zip(transfers_dict[n_validators], avg_sizes_dict[n_validators]))
    if pairs:
        x, y = zip(*pairs)
        plt.plot(x, y, marker=markers_validators[i % len(markers_validators)], label=f'{n_validators} validators')

# Add Native bitcoin line to plot 4
defined_x_points_4 = set()
for n_validators in n_validators_list:
    defined_x_points_4.update(transfers_dict[n_validators])
native_bitcoin_x_4 = sorted(defined_x_points_4)
native_bitcoin_y_4 = [141 for _ in native_bitcoin_x_4]
plt.plot(native_bitcoin_x_4, native_bitcoin_y_4, linestyle='--', color='black', label='Native bitcoin')

plt.xlabel('Total number of batched transfers (to all destination subnets)')
plt.ylabel('Amortized size per transfer (vbytes)')
plt.title(f'Amortized size per transfer vs Total number of transfers (1 target subnet)')
plt.grid(True)
plt.legend()
plt.xticks([5,10,15,20,25,30,35,40,45,50])
plt.tight_layout()
plt.savefig(f'bench-plots/4-amortized_vsize_vs_n_transfers_vary_n_validators_{n_subnets_for_plot}_subnets.png')
# plt.show()


# Fifth plot: amortized checkpoint tx size per withdrawal vs number of withdrawals
n_withdrawals = []
amortized_sizes = []

with open(csv_file_withdraw, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        n = int(row['n_withdrawals'])
        size = int(row['checkpoint_tx_size'])
        n_withdrawals.append(n)
        amortized_sizes.append(size / n if n != 0 else 0)

plt.figure(figsize=(8, 5))
plt.plot(n_withdrawals, amortized_sizes, marker='o')
plt.xlabel('Total number of batched withdrawals')
plt.ylabel('Amortized withdrawal size (vbytes)')
plt.title('Amortized withdrawal size vs. total number of withdrawals')
plt.grid(True)
# plt.xticks(n_withdrawals)
plt.tight_layout()
plt.savefig('bench-plots/5-withdraw_size_vs_n_withdrawals.png')


# Sixth plot: total daily overhead vs checkpoint period

overhead = 90  # vBytes
checkpoint_periods = [0.5, 1, 2, 4, 8, 12, 16, 20, 24]  # hours

daily_overheads = [overhead * (24 / period) for period in checkpoint_periods]
print(daily_overheads)

# First plot: size in vbytes
plt.figure(figsize=(8, 5))
plt.plot(checkpoint_periods, daily_overheads, marker='o')
plt.xlabel('Checkpoint period (hours)')
plt.ylabel('Total data submitted per 24 hours (vbytes)')
plt.title('Total overhead per 24 hours vs. checkpoint period')
plt.grid(True)
plt.xticks([1, 2, 4, 8, 12, 16, 20, 24])
plt.tight_layout()
plt.savefig('bench-plots/6-daily_overhead_vs_checkpoint_period.png')

# Second plot: with fee multiplication
plt.figure(figsize=(8, 5))
daily_overheads_fee = [overhead * fee_per_vb * (24 / period) for period in checkpoint_periods]
plt.plot(checkpoint_periods, daily_overheads_fee, marker='o')
plt.xlabel('Checkpoint period (hours)')
plt.ylabel(f'Total daily fee cost (in sats)')
plt.title(f'Total daily fee cost vs. checkpoint period (fee rate = {fee_per_vb} sat / vB)')
plt.grid(True)
plt.xticks([1, 2, 4, 8, 12, 16, 20, 24])
plt.tight_layout()
plt.savefig('bench-plots/6-daily_overhead_vs_checkpoint_period_fee.png')

# Seventh plot: throughput vs total number of batched transfers
plot7([1,2,3,5, 10,20,50, 100, 200, 500, 1000, 10000, 16500], [1, 10, 100, 1000, 10000, 16500])