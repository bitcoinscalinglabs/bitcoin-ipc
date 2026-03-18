use std::path::PathBuf;

use bitcoin_ipc::easy_tester::model::TesterConfig;
use bitcoin_ipc::easy_tester::{parse_test_file, ScenarioCommand, Tester};

#[tokio::main]
async fn main() {
    if let Err(e) = try_main().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn try_main() -> Result<(), Box<dyn std::error::Error>> {
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

    let scenario = parsed.scenario.clone();
    match parsed.config.tester {
        TesterConfig::RewardTester {
            activation_height,
            snapshot_length,
        } => {
            let mut tester = bitcoin_ipc::easy_tester::RewardTester::new(
                parsed.setup,
                activation_height,
                snapshot_length,
            )
            .await?;
            run_scenario(&mut tester, scenario)?;
        }
    }
    Ok(())
}

fn run_scenario<T: Tester>(
    tester: &mut T,
    scenario: Vec<ScenarioCommand>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut mined_height: u64 = 0;
    let mut current_block: Option<u64> = None;

    for cmd in scenario {
        match cmd {
            ScenarioCommand::Block { height } => {
                // Close previous block (mine it), then mine empty blocks up to height-1.
                if let Some(prev) = current_block.take() {
                    if prev > mined_height {
                        tester.exec_mine_block(prev)?;
                        mined_height = prev;
                    }
                }

                if height > 0 && mined_height < height.saturating_sub(1) {
                    for h in (mined_height + 1)..=height.saturating_sub(1) {
                        tester.exec_mine_block(h)?;
                        mined_height = h;
                    }
                }

                current_block = Some(height);
            }
            ScenarioCommand::Create { subnet_name } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_create_subnet(height, &subnet_name)?;
            }
            ScenarioCommand::Join {
                subnet_name,
                validator_name,
                collateral_sats,
            } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_join_subnet(height, &subnet_name, &validator_name, collateral_sats)?;
            }
            ScenarioCommand::Stake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_stake_subnet(height, &subnet_name, &validator_name, amount_sats)?;
            }
            ScenarioCommand::Unstake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_unstake_subnet(height, &subnet_name, &validator_name, amount_sats)?;
            }
            ScenarioCommand::Checkpoint { subnet_name } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_checkpoint_subnet(height, &subnet_name)?;
            }
            ScenarioCommand::OutputRead { db, args } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_output_read(height, db, &args)?;
            }
            ScenarioCommand::OutputExpect {
                target,
                expected_sats,
            } => {
                let height = current_block
                    .ok_or("scenario error: must set 'block <height>' before actions")?;
                tester.exec_output_expect(height, target, expected_sats)?;
            }
        }
    }

    // Mine/close the final block, if any.
    if let Some(final_height) = current_block {
        if final_height > mined_height {
            tester.exec_mine_block(final_height)?;
        }
    }

    Ok(())
}
