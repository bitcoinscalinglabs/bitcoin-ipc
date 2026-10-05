//! Total checkpoint sizes (tx_cpt^1 and tx_cpt^2) in the CSV format of bench-plots/plot*.py,
//! built with the same checkpoint code as `bench` (Table 5 of the paper).
use std::io::Write;
use unilateral_validator_exit::bench::Fixture;
fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let mut f = std::fs::File::create(out).unwrap();
    writeln!(f, "n_validators,n_destination_subnets,n_transfers,checkpoint_tx_size,transfer_tx_size").unwrap();
    let ts = [1usize, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 30, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 15000, 16500];
    for n in [1usize, 4, 7, 10, 25, 37, 52, 76, 100] {
        let fx = Fixture::new(n, 1).expect("fixture");
        for g in [1usize, 2, 5, 10] {
            for &t in ts.iter().filter(|t| **t >= g) {
                let m = fx.transfer_checkpoint(t, g).expect("checkpoint");
                writeln!(f, "{},{},{},{},{}", n, g, t, m.commit_vsize(), m.reveal_vsize()).unwrap();
            }
        }
        eprintln!("n={n} done");
    }
}
