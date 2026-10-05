//! Size of one extra multisig input in a checkpoint: what spending a deposit or an incoming
//! batch output adds, for the validator counts of the paper's evaluation.

use unilateral_validator_exit::bench::Fixture;

fn main() {
    for n in [4usize, 16, 31, 70, 100] {
        let f = Fixture::new(n, 1).expect("fixture");
        let base = f.ordinary_checkpoint().expect("ordinary").commit_vsize();
        let mut row = format!("n {n:>3}  regular {base:>5}");
        for k in [1usize, 2, 10] {
            let m = f.extra_inputs_checkpoint(k).expect("extra inputs");
            assert_eq!(m.reveal_vsize(), 0);
            let size = m.commit_vsize();
            row += &format!("  +{k} inputs {size:>6} ({:.2}/input)", (size - base) as f64 / k as f64);
        }
        println!("{row}");
    }
}
