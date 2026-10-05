"""Column-width ("_small") versions of the benchmark plots used in the paper.

Same data and same curves as plot.py; only the appearance differs. Every figure is
4.2 in wide with 8-9 pt fonts, like 7-throughput_vs_n_transfers_small.png, so that all
plots have the same font size when they are shown one column wide.

Run from the repository root, after the benchmarks have written the CSV:
    python bench-plots/paper-small-plots/plot_small.py
"""
import csv

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

CSV_TRANSFER = 'bench-plots/paper-small-plots/bench-transfer-sizes.csv'
OUT_DIR = 'bench-plots/paper-small-plots'

FEE_PER_VB = 3      # sat/vB, the fee rate of the paper's setup
NATIVE_VSIZE = 141  # vB, a native Bitcoin transfer
BLOCK_SPACE_PER_SECOND = 1_000_000 / 600  # vB per second: blocks of 1M vB every 10 minutes

FIGSIZE = (4.2, 2.8)
FONT_LABEL = 9
FONT_TICKS = 8
FONT_LEGEND = 8
DPI = 200

MARKERS_SUBNETS = ['o', 's', 'D', 'v']
MARKERS_VALIDATORS = ['*', 'H', 'X', '2', '+', 'p']
MARKERS_TRANSFERS = ['*', 'H', 'X', '2', '+', 'p']


def read_total_sizes():
    """(n_validators, n_target_subnets, n_transfers) -> total size in vB of both checkpoint transactions."""
    sizes = {}
    with open(CSV_TRANSFER, newline='') as f:
        for row in csv.DictReader(f):
            key = (int(row['n_validators']), int(row['n_destination_subnets']), int(row['n_transfers']))
            sizes[key] = int(row['checkpoint_tx_size']) + int(row['transfer_tx_size'])
    return sizes


def finish(filename, title, xlabel, ylabel, xticks=None, log_x=False, legend_columns=1, headroom=0.0):
    """headroom: fraction of the y-range added above the curves, to make room for the legend."""
    plt.title(title, fontsize=FONT_LABEL)
    plt.xlabel(xlabel, fontsize=FONT_LABEL)
    plt.ylabel(ylabel, fontsize=FONT_LABEL)
    plt.grid(True)
    if headroom > 0:
        bottom, top = plt.ylim()
        plt.ylim(bottom, top + headroom * (top - bottom))
    plt.legend(fontsize=FONT_LEGEND, ncol=legend_columns, loc='upper left' if headroom > 0 else 'best')
    plt.tick_params(labelsize=FONT_TICKS)
    if log_x:
        plt.xscale('log')
    if xticks is not None:
        plt.xticks(xticks)
    plt.tight_layout()
    plt.savefig(f'{OUT_DIR}/{filename}', dpi=DPI, bbox_inches='tight', pad_inches=0.02)
    plt.close()


def plot_vs_transfers_vary_subnets(sizes, filename, title, ylabel, scale):
    """Plot 3D: amortized size (or fee) per transfer vs batched transfers, 4 validators, one line per number of target subnets."""
    n_transfers_list = [1, 2, 3, 5, 10, 20, 50, 100, 200, 500, 1000, 10000, 16500]
    plt.figure(figsize=FIGSIZE)
    all_x = set()
    for i, n_subnets in enumerate([1, 2, 5, 10]):
        x = [t for t in n_transfers_list if t >= n_subnets and (4, n_subnets, t) in sizes]
        y = [scale * sizes[(4, n_subnets, t)] / t for t in x]
        all_x.update(x)
        plt.plot(x, y, marker=MARKERS_SUBNETS[i], label=f'{n_subnets} target subnets')
    x = sorted(all_x)
    plt.plot(x, [scale * NATIVE_VSIZE] * len(x), linestyle='--', color='black', label='Native bitcoin')
    finish(filename, title, 'Total number of batched transfers', ylabel,
           xticks=[1, 10, 100, 1000, 10000, 16500], log_x=True)


def plot_vs_transfers_vary_validators(sizes, filename):
    """Plot 4: amortized size per transfer vs batched transfers, 1 target subnet, one line per number of validators."""
    n_transfers_list = [1, 2, 3, 4, 5, 10, 20, 30, 50]
    plt.figure(figsize=FIGSIZE)
    all_x = set()
    for i, n_validators in enumerate([4, 10, 25, 52, 76, 100]):
        x = [t for t in n_transfers_list if (n_validators, 1, t) in sizes]
        y = [sizes[(n_validators, 1, t)] / t for t in x]
        all_x.update(x)
        plt.plot(x, y, marker=MARKERS_VALIDATORS[i], label=f'{n_validators} validators')
    x = sorted(all_x)
    plt.plot(x, [NATIVE_VSIZE] * len(x), linestyle='--', color='black', label='Native bitcoin')
    finish(filename, 'Size per transfer vs. batched transfers (1 target subnet)',
           'Total number of batched transfers', 'Amortized size (vbytes)',
           xticks=[5, 10, 15, 20, 25, 30, 35, 40, 45, 50])


def plot_vs_validators_vary_transfers(sizes, filename):
    """Plot 2: amortized size per transfer vs number of validators, 1 target subnet, one line per batch size."""
    n_validators_list = [1, 7, 16, 25, 37, 52, 76, 100]
    plt.figure(figsize=FIGSIZE)
    for i, n_transfers in enumerate([1, 2, 5, 10, 50, 100]):
        x = [v for v in n_validators_list if (v, 1, n_transfers) in sizes]
        y = [sizes[(v, 1, n_transfers)] / n_transfers for v in x]
        plt.plot(x, y, marker=MARKERS_TRANSFERS[i], label=f'{n_transfers} total transfers')
    finish(filename, 'Size per transfer vs. validators (1 target subnet)',
           'Number of validators in source subnet', 'Amortized size (vbytes)',
           xticks=n_validators_list, legend_columns=2, headroom=0.45)


def plot_transfers_per_second(sizes, filename):
    """Plot 7: monetary transfers per second if blocks are filled with batches (or with native payments),
    vs batched transfers, 4 validators, one line per number of target subnets."""
    n_transfers_list = [1, 2, 3, 5, 10, 20, 50, 100, 200, 500, 1000, 10000, 16500]
    plt.figure(figsize=(FIGSIZE[0], 2.5))
    all_x = set()
    for i, n_subnets in enumerate([1, 2, 5, 10]):
        x = [t for t in n_transfers_list if t >= n_subnets and (4, n_subnets, t) in sizes]
        y = [BLOCK_SPACE_PER_SECOND / (sizes[(4, n_subnets, t)] / t) for t in x]
        all_x.update(x)
        plt.plot(x, y, marker=MARKERS_SUBNETS[i], label=f'{n_subnets} target subnets')
    x = sorted(all_x)
    native = BLOCK_SPACE_PER_SECOND / NATIVE_VSIZE
    plt.plot(x, [native] * len(x), linestyle='--', color='black', label=f'Native bitcoin ({native:.1f})')
    plt.ylim(bottom=-5)
    finish(filename, 'Transfers per second vs. batched transfers (4 validators)',
           'Total number of batched transfers', 'Monetary transfers per second',
           xticks=[1, 10, 100, 1000, 10000, 16500], log_x=True)

if __name__ == '__main__':
    sizes = read_total_sizes()
    plot_vs_transfers_vary_subnets(
        sizes, '3D-amortized_vsize_vs_n_transfers_vary_n_subnets_log_small.png',
        'Size per transfer vs. batched transfers (4 validators)', 'Amortized size (vbytes)', scale=1)
    plot_vs_transfers_vary_subnets(
        sizes, '3D-amortized_vsize_vs_n_transfers_vary_n_subnets_log_fee_small.png',
        f'Fee per transfer vs. batched transfers ({FEE_PER_VB} sat/vB)',
        'Amortized fee (sats)', scale=FEE_PER_VB)
    plot_vs_transfers_vary_validators(
        sizes, '4-amortized_vsize_vs_n_transfers_vary_n_validators_1_subnets_small.png')
    plot_vs_validators_vary_transfers(
        sizes, '2-amortized_vsize_vs_n_validators_vary_n_transfers_1_subnets_small.png')
    plot_transfers_per_second(sizes, '7-transfers_per_second_vs_n_transfers_small.png')
