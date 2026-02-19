use std::path::PathBuf;

use bitcoin_ipc::easy_tester::{parse_test_file, DbTester};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: easy_tester <scenario_file>");
        std::process::exit(2);
    };

    if args.next().is_some() {
        eprintln!("usage: easy_tester <scenario_file>");
        std::process::exit(2);
    }

    let path = PathBuf::from(path);
    let parsed = parse_test_file(&path)?;
    let mut tester = DbTester::new(parsed).await?;
    tester.run()?;
    Ok(())
}

