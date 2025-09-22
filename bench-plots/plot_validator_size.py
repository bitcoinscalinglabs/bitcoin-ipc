import csv
import matplotlib.pyplot as plt

# File path
csv_file = 'bench-transfer-sizes.csv'

n_transfers_list = [1, 2, 3, 4, 5, 10, 20, 50]

# First plot: total size of transfers vs number of validators, for varying number of transfers
validators_dict = {n: [] for n in n_transfers_list}
total_sizes_dict = {n: [] for n in n_transfers_list}

with open(csv_file, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        if row['n_destination_subnets'] == '1':
            n_transfers = int(row['n_transfers'])
            if n_transfers in n_transfers_list:
                num_validators = int(row['n_validators'])
                commit_vsize = int(row['checkpoint_tx_size'])
                reveal_vsize = int(row['transfer_tx_size'])
                total_size = commit_vsize + reveal_vsize
                validators_dict[n_transfers].append(num_validators)
                total_sizes_dict[n_transfers].append(total_size)

plt.figure(figsize=(8, 5))
for n_transfers in n_transfers_list:
    plt.plot(validators_dict[n_transfers], total_sizes_dict[n_transfers], marker='o', label=f'n_transfers={n_transfers}')

plt.xlabel('Number of validators in each subnet')
plt.ylabel('Total size of transfers (vbytes)')
plt.title('Total size of transfers vs Number of validators (n_destination_subnets=1)')
plt.grid(True)
plt.legend()
plt.tight_layout()
plt.savefig('bench-plots/total_vsize_vs_n_validators_vary_n_transfers.png')
# plt.show()


# Second plot: average size per transfer vs number of validators, for varying number of transfers
n_transfers_list_for_plot = [1, 2, 5, 10, 50]
n_subnets_for_plot = 1
validators_dict = {n: [] for n in n_transfers_list}
total_sizes_dict = {n: [] for n in n_transfers_list}

with open(csv_file, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        if row['n_destination_subnets'] == str(n_subnets_for_plot):
            n_transfers = int(row['n_transfers'])
            if n_transfers >= n_subnets_for_plot:
                if n_transfers in n_transfers_list_for_plot:
                    num_validators = int(row['n_validators'])
                    commit_vsize = int(row['checkpoint_tx_size'])
                    reveal_vsize = int(row['transfer_tx_size'])
                    total_size = commit_vsize + reveal_vsize
                    validators_dict[n_transfers].append(num_validators)
                    total_sizes_dict[n_transfers].append(total_size)

plt.figure(figsize=(8, 5))
for n_transfers in n_transfers_list_for_plot:
    avg_sizes = [total / float(n_transfers) for total in total_sizes_dict[n_transfers]]
    plt.plot(validators_dict[n_transfers], avg_sizes, marker='o', label=f'n_transfers={n_transfers}')

plt.xlabel('Number of validators in source subnet')
plt.ylabel('Amortized size per transfer (vbytes)')
plt.title(f'Amortized size per transfer vs Number of validators (n_destination_subnets={n_subnets_for_plot})')
plt.grid(True)
plt.legend()
plt.tight_layout()
plt.savefig(f'bench-plots/amortized_vsize_vs_n_validators_vary_n_transfers_{n_subnets_for_plot}_subnets.png')
# plt.show()


# Third plot: amortized size per transfer vs total number of transfers, for n_validators=4, one line per n_destination_subnets
n_target_subnets_list = [1, 2, 5, 10]
n_transfers_list_for_plot = n_transfers_list  # [1, 10, 50, 100]
transfers_dict = {n: [] for n in n_target_subnets_list}
avg_sizes_dict = {n: [] for n in n_target_subnets_list}

with open(csv_file, newline='') as f:
    reader = csv.DictReader(f)
    for row in reader:
        if int(row['n_validators']) == 4:
            n_subnets = int(row['n_destination_subnets'])
            n_transfers = int(row['n_transfers'])
            if n_subnets in n_target_subnets_list and n_transfers in n_transfers_list_for_plot and n_transfers >= n_subnets:
                commit_vsize = int(row['checkpoint_tx_size'])
                reveal_vsize = int(row['transfer_tx_size'])
                total_size = commit_vsize + reveal_vsize
                transfers_dict[n_subnets].append(n_transfers)
                avg_sizes_dict[n_subnets].append(total_size / float(n_transfers))

plt.figure(figsize=(8, 5))
for n_subnets in n_target_subnets_list:
    # Sort by n_transfers for each line for better plot
    pairs = sorted(zip(transfers_dict[n_subnets], avg_sizes_dict[n_subnets]))
    if pairs:
        x, y = zip(*pairs)
        plt.plot(x, y, marker='o', label=f'n_destination_subnets={n_subnets}')

# Add Native bitcoin line
defined_x_points = set()
for n_subnets in n_target_subnets_list:
    defined_x_points.update(transfers_dict[n_subnets])
native_bitcoin_x = sorted(defined_x_points)
native_bitcoin_y = [141 for _ in native_bitcoin_x]
plt.plot(native_bitcoin_x, native_bitcoin_y, linestyle='--', color='black', label='Native bitcoin')

plt.xlabel('Total number of transfers (to all destination subnets)')
plt.ylabel('Amortized size per transfer (vbytes)')
plt.title('Amortized size per transfer vs Total number of transfers (n_validators=4)')
plt.grid(True)
plt.legend()
plt.tight_layout()
plt.savefig('bench-plots/amortized_vsize_vs_n_transfers_vary_n_subnets.png')
# plt.show()


# Fourth plot: amortized size per transfer vs total number of transfers, for varying n_validators and n_destination_subnets=1
n_validators_list = [1, 4, 10, 25, 36]
n_transfers_list_for_plot = n_transfers_list
n_subnets_for_plot = 1
transfers_dict = {n: [] for n in n_validators_list}
avg_sizes_dict = {n: [] for n in n_validators_list}

with open(csv_file, newline='') as f:
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
for n_validators in n_validators_list:
    # Sort by n_transfers for each line for better plot
    pairs = sorted(zip(transfers_dict[n_validators], avg_sizes_dict[n_validators]))
    if pairs:
        x, y = zip(*pairs)
        plt.plot(x, y, marker='o', label=f'n_validators={n_validators}')

# Add Native bitcoin line to plot 4
defined_x_points_4 = set()
for n_validators in n_validators_list:
    defined_x_points_4.update(transfers_dict[n_validators])
native_bitcoin_x_4 = sorted(defined_x_points_4)
native_bitcoin_y_4 = [141 for _ in native_bitcoin_x_4]
plt.plot(native_bitcoin_x_4, native_bitcoin_y_4, linestyle='--', color='black', label='Native bitcoin')

plt.xlabel('Total number of transfers (to all destination subnets)')
plt.ylabel('Amortized size per transfer (vbytes)')
plt.title(f'Amortized size per transfer vs Total number of transfers (n_destination_subnets={n_subnets_for_plot})')
plt.grid(True)
plt.legend()
plt.tight_layout()
plt.savefig(f'bench-plots/amortized_vsize_vs_n_transfers_vary_n_validators_{n_subnets_for_plot}_subnets.png')
# plt.show()
