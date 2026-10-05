# Column-width plots for the paper (2026-09-29)

The `_small` plots used by the two-column version of the technical paper
(`BSL-technical-paper/images/*_small.png`).

- `bench-transfer-sizes.csv`, `bench-withdraw-sizes.csv`: output of `test_transfer_size` and
  `test_withdraw_size` with `quorum-two-thirds.patch` applied to `src/test_sizes.rs`
  (a checkpoint is signed by floor(2n/3)+1 validators, as in the paper). The CSVs one level
  up are the committed ones and use the old quorum.
- `plot_small.py`: draws the plots from that CSV, 4.2 in wide with 8-9 pt fonts. Run from the
  repository root: `python bench-plots/paper-small-plots/plot_small.py`.

To reproduce the CSVs: apply the patch, delete the CSVs in `bench-plots/`, then run
`cargo test --release test_transfer_size -- --ignored --nocapture` (same for `test_withdraw_size`).
