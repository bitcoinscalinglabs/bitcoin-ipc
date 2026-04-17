use std::path::PathBuf;

use bitcoin_ipc::easy_tester::{
    error::EasyTesterError, parse_config_file, parse_fendermint_test_file, parse_test_file,
    validate_scenario_for_tester, DbTester, FendermintTester, MonitorTester, ScenarioCommand,
    Tester, TesterConfig,
};

#[tokio::main]
async fn main() {
    if let Err(e) = try_main().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn try_main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let mut scenario_path: Option<String> = None;
    let mut config_path: Option<String> = None;

    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => {
                scenario_path = args.next();
                if scenario_path.is_none() {
                    eprintln!("error: --scenario requires a value");
                    std::process::exit(2);
                }
            }
            "--tester" => {
                config_path = args.next();
                if config_path.is_none() {
                    eprintln!("error: --tester requires a value");
                    std::process::exit(2);
                }
            }
            other => {
                eprintln!("error: unexpected argument '{other}'");
                eprintln!("usage: easy_tester --scenario <scenario_file> --tester <config_file>");
                std::process::exit(2);
            }
        }
    }

    let (Some(scenario_path), Some(config_path)) = (scenario_path, config_path) else {
        eprintln!("usage: easy_tester --scenario <scenario_file> --tester <config_file>");
        std::process::exit(2);
    };

    let config = parse_config_file(PathBuf::from(&config_path))?;

    // Quick check: does the scenario file contain "tester fendermint"?
    let scenario_text = std::fs::read_to_string(&scenario_path)?;
    let scenario_is_fendermint = scenario_text
        .lines()
        .any(|l| l.split('#').next().unwrap_or("").trim() == "tester fendermint");

    match config {
        TesterConfig::Fendermint { setup } => {
            if !scenario_is_fendermint {
                eprintln!(
                    "error: tester config is 'fendermint' but the scenario file does not contain \
                     'tester fendermint' in its setup section.\n\
                     Fendermint scenarios require a different setup format (issuers + subnets).\n\
                     Use a fendermint-specific scenario file, or switch to a db/monitor tester config."
                );
                std::process::exit(1);
            }
            let parsed = parse_fendermint_test_file(PathBuf::from(&scenario_path), setup)?;
            let scenario = parsed.scenario;
            let mut tester = FendermintTester::new(parsed.setup)?;
            tester.run(scenario)?;
        }
        _ => {
            if scenario_is_fendermint {
                eprintln!(
                    "error: scenario file contains 'tester fendermint' but the tester config is '{}'.\n\
                     Fendermint scenarios can only run with a fendermint tester config.",
                    match &config {
                        TesterConfig::Db { .. } => "db",
                        TesterConfig::Monitor { .. } => "monitor",
                        TesterConfig::Fendermint { .. } => unreachable!(),
                    }
                );
                std::process::exit(1);
            }
            let parsed = parse_test_file(PathBuf::from(&scenario_path))?;
            validate_scenario_for_tester(&parsed, &config)?;
            let scenario = parsed.scenario.clone();

            match config {
                TesterConfig::Db {
                    activation_height,
                    snapshot_length,
                } => {
                    let mut tester =
                        DbTester::new(parsed.setup, activation_height, snapshot_length).await?;
                    run_scenario(&mut tester, scenario, 1)?;
                }
                TesterConfig::Monitor {
                    activation_height,
                    snapshot_length,
                    monitor_log_level,
                    provider_log_level,
                } => {
                    let mut tester = MonitorTester::new(
                        parsed.setup,
                        activation_height,
                        snapshot_length,
                        monitor_log_level,
                        provider_log_level,
                    )
                    .await?;
                    run_scenario(&mut tester, scenario, 101)?;
                }
                TesterConfig::Fendermint { .. } => unreachable!(),
            }
        }
    }

    Ok(())
}

fn run_scenario<T: Tester>(
    tester: &mut T,
    scenario: Vec<(usize, ScenarioCommand)>,
    starting_block: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    if starting_block < 1 {
        return Err("starting_block must be >= 1".into());
    }
    let mut working_height: u64 = starting_block;

    for (line_no, cmd) in scenario {
        let annotate = |e: EasyTesterError| -> Box<dyn std::error::Error> {
            format!("line {line_no}: {e}").into()
        };
        match cmd {
            ScenarioCommand::Block { height } => {
                if height < working_height {
                    return Err(annotate(EasyTesterError::runtime("Scenario blocks cannot be decreasing")));
                }
                for h in working_height..height {
                    tester.exec_mine_block(h).map_err(&annotate)?;
                }
                working_height = height;
            }
            ScenarioCommand::Create { subnet_name } => {
                tester.exec_create_subnet(working_height, &subnet_name).map_err(annotate)?;
            }
            ScenarioCommand::Join {
                subnet_name,
                validator_name,
                collateral_sats,
            } => {
                tester.exec_join_subnet(
                    working_height,
                    &subnet_name,
                    &validator_name,
                    collateral_sats,
                ).map_err(annotate)?;
            }
            ScenarioCommand::Stake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                tester.exec_stake_subnet(
                    working_height,
                    &subnet_name,
                    &validator_name,
                    amount_sats,
                ).map_err(annotate)?;
            }
            ScenarioCommand::Unstake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                tester.exec_unstake_subnet(
                    working_height,
                    &subnet_name,
                    &validator_name,
                    amount_sats,
                ).map_err(annotate)?;
            }
            ScenarioCommand::Checkpoint { subnet_name } => {
                tester.exec_checkpoint_subnet(working_height, &subnet_name).map_err(annotate)?;
            }
            ScenarioCommand::RegisterToken {
                subnet_name,
                issuer: _,
                name,
                symbol,
                initial_supply,
            } => {
                tester.exec_register_token(
                    working_height,
                    &subnet_name,
                    &name,
                    &symbol,
                    initial_supply,
                ).map_err(annotate)?;
            }
            ScenarioCommand::MintToken {
                subnet_name,
                token_name,
                amount,
            } => {
                tester.exec_mint_token(working_height, &subnet_name, &token_name, amount).map_err(annotate)?;
            }
            ScenarioCommand::BurnToken {
                subnet_name,
                token_name,
                amount,
            } => {
                tester.exec_burn_token(working_height, &subnet_name, &token_name, amount).map_err(annotate)?;
            }
            ScenarioCommand::Wait { seconds } => {
                println!("line {line_no}: Waiting {seconds} seconds...");
                std::thread::sleep(std::time::Duration::from_secs(seconds));
            }
            ScenarioCommand::Deposit { .. } => {
                unimplemented!("deposit is only supported by the FendermintTester");
            }
            ScenarioCommand::ErcTransfer {
                src_subnet,
                src_actor: _,
                dst_subnet,
                dst_actor: _,
                token_name,
                amount,
            } => {
                tester.exec_erc_transfer(
                    working_height,
                    &src_subnet,
                    &dst_subnet,
                    &token_name,
                    amount,
                ).map_err(annotate)?;
            }
            ScenarioCommand::OutputRead { db, args } => {
                tester.exec_output_read(working_height, db, &args).map_err(annotate)?;
            }
            ScenarioCommand::OutputExpect {
                target,
                expected_value,
            } => match tester.exec_output_expect(working_height, target, &expected_value) {
                Ok(msg) => println!("OUTPUT expect {} \x1b[32m(ok)\x1b[0m", msg),
                Err(e) => {
                    return Err(format!("line {line_no}: {} \x1b[31m(fail)\x1b[0m", e).into());
                }
            },
        }
    }

    Ok(())
}
