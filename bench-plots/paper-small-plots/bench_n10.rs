//! Sizes for N = 10 subnets: 9 incoming batch outputs spent, and transfers split over 9 targets.
use unilateral_validator_exit::bench::Fixture;
fn main() {
    for n in [16usize, 31, 70] {
        let f = Fixture::new(n, 1).expect("fixture");
        let base = f.ordinary_checkpoint().unwrap().commit_vsize();
        let inc = f.extra_inputs_checkpoint(9).unwrap().commit_vsize();
        let mut row = format!("n {n:>3} regular {base} +9inputs {inc} (delta {})", inc - base);
        for (t, g) in [(1000usize, 1usize), (1000, 9), (16500, 1), (16497, 9)] {
            let m = f.transfer_checkpoint(t, g).unwrap();
            row += &format!(" | t={t} g={g}: {}", m.commit_vsize() + m.reveal_vsize() - base);
        }
        println!("{row}");
    }
}
