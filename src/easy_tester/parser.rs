use std::{collections::HashSet, fs, path::PathBuf};

use crate::easy_tester::{
    error::EasyTesterError,
    model::{
        generate_validator, parse_u16_allow_underscores, parse_u64_allow_underscores, OutputDb,
        OutputExpectTarget, ParsedTest, ScenarioCommand, SetupSpec, SubnetSpec, TestConfig,
        TesterConfig,
    },
};

enum Section {
    None,
    Config,
    Setup,
    Scenario,
}

enum SetupBuilder {
    Validator {
        name: String,
    },
    Subnet {
        name: String,
        whitelist_names: Option<Vec<String>>,
        min_validators: Option<u16>,
    },
}

impl SetupBuilder {
    fn name(&self) -> &str {
        match self {
            SetupBuilder::Validator { name } => name,
            SetupBuilder::Subnet { name, .. } => name,
        }
    }

    fn finalize(
        self,
        path: &PathBuf,
        line_no: usize,
        original_line: &str,
        setup: &mut SetupSpec,
        next_validator_ordinal: &mut usize,
    ) -> Result<(), EasyTesterError> {
        match self {
            SetupBuilder::Validator { name } => {
                if setup.validators.contains_key(&name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("duplicate validator name '{name}'"),
                        original_line,
                    ));
                }
                let spec = generate_validator(&name, *next_validator_ordinal);
                *next_validator_ordinal += 1;
                setup.validators.insert(name, spec);
                Ok(())
            }
            SetupBuilder::Subnet {
                name,
                whitelist_names,
                min_validators,
            } => {
                if setup.subnets.contains_key(&name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("duplicate subnet name '{name}'"),
                        original_line,
                    ));
                }

                let Some(whitelist_names) = whitelist_names else {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("subnet '{name}' missing required field 'whitelist'"),
                        original_line,
                    ));
                };
                let Some(min_validators) = min_validators else {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("subnet '{name}' missing required field 'min'"),
                        original_line,
                    ));
                };

                if min_validators < 4 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("subnet '{name}' min must be at least 4 (got {min_validators})"),
                        original_line,
                    ));
                }

                if whitelist_names.len() < 4 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!(
                            "subnet '{name}' whitelist must have at least 4 entries (got {})",
                            whitelist_names.len()
                        ),
                        original_line,
                    ));
                }

                let mut unique = HashSet::new();
                for vname in &whitelist_names {
                    if !unique.insert(vname.clone()) {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!("subnet '{name}' whitelist contains duplicate '{vname}'"),
                            original_line,
                        ));
                    }
                    if !setup.validators.contains_key(vname) {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!(
                                "subnet '{name}' whitelist references unknown validator '{vname}' (validators must be declared earlier in setup)"
                            ),
                            original_line,
                        ));
                    }
                }

                if whitelist_names.len() < min_validators as usize {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!(
                            "subnet '{name}' whitelist size ({}) is less than min ({min_validators})",
                            whitelist_names.len()
                        ),
                        original_line,
                    ));
                }

                let whitelist_pubkeys = whitelist_names
                    .iter()
                    .map(|n| setup.validators[n].pubkey)
                    .collect::<Vec<_>>();

                setup.subnets.insert(
                    name.clone(),
                    SubnetSpec {
                        name,
                        min_validators,
                        whitelist_names,
                        whitelist_pubkeys,
                    },
                );
                Ok(())
            }
        }
    }
}

pub fn parse_test_file(path: impl Into<PathBuf>) -> Result<ParsedTest, EasyTesterError> {
    let path = path.into();
    let raw = fs::read_to_string(&path).map_err(|e| EasyTesterError::Io {
        path: path.clone(),
        source: e,
    })?;

    let mut section = Section::None;
    let mut config: Option<TestConfig> = None;
    let mut setup = SetupSpec::new();
    let mut scenario_entries: Vec<ScenarioEntry> = Vec::new();

    let mut current_builder: Option<SetupBuilder> = None;
    let mut next_validator_ordinal: usize = 1;

    let mut seen_setup = false;
    let mut seen_scenario = false;

    for (idx0, original_line) in raw.lines().enumerate() {
        let line_no = idx0 + 1;
        // Support `#` line comments anywhere on the line.
        let without_comment = original_line
            .split_once('#')
            .map(|(before, _)| before)
            .unwrap_or(original_line);
        let line_trimmed = without_comment.trim();

        if line_trimmed.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = line_trimmed.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }

        match section {
            Section::None => {
                if tokens.len() == 1 && tokens[0] == "config" {
                    section = Section::Config;
                    continue;
                }
                return Err(EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    "expected 'config' as the first section",
                    original_line,
                ));
            }
            Section::Config => {
                if tokens.len() == 1 && tokens[0] == "setup" {
                    if config.is_none() {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "missing required 'tester ...' line in config section",
                            original_line,
                        ));
                    }
                    seen_setup = true;
                    section = Section::Setup;
                    continue;
                }

                if tokens[0] != "tester" {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        "unknown config directive (expected 'tester ...')",
                        original_line,
                    ));
                }

                if config.is_some() {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        "tester already specified",
                        original_line,
                    ));
                }

                if tokens.len() < 2 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        "tester line requires a tester name",
                        original_line,
                    ));
                }

                let tester_name = tokens[1];
                match tester_name {
                    "RewardTester" => {
                        let kv = parse_kv_pairs(&tokens[2..]).map_err(|e| {
                            EasyTesterError::parse(path.clone(), line_no, e, original_line)
                        })?;
                        let activation_height =
                            require_kv_u64(&kv, "activation_height").map_err(|e| {
                                EasyTesterError::parse(path.clone(), line_no, e, original_line)
                            })?;
                        let snapshot_length =
                            require_kv_u64(&kv, "snapshot_length").map_err(|e| {
                                EasyTesterError::parse(path.clone(), line_no, e, original_line)
                            })?;

                        config = Some(TestConfig {
                            tester: TesterConfig::RewardTester {
                                activation_height,
                                snapshot_length,
                            },
                        });
                    }
                    "ErcTransferTester" => {
                        config = Some(TestConfig {
                            tester: TesterConfig::ErcTransferTester,
                        });
                    }
                    _ => {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!("unknown tester '{tester_name}'"),
                            original_line,
                        ));
                    }
                }
            }
            Section::Setup => {
                if tokens.len() == 1 && tokens[0] == "scenario" {
                    seen_scenario = true;
                    if let Some(b) = current_builder.take() {
                        b.finalize(
                            &path,
                            line_no,
                            original_line,
                            &mut setup,
                            &mut next_validator_ordinal,
                        )?;
                    }
                    section = Section::Scenario;
                    continue;
                }

                // Entity header: validatorX or subnetY
                if tokens.len() == 1
                    && (tokens[0].starts_with("validator") || tokens[0].starts_with("subnet"))
                {
                    if let Some(b) = current_builder.take() {
                        b.finalize(
                            &path,
                            line_no,
                            original_line,
                            &mut setup,
                            &mut next_validator_ordinal,
                        )?;
                    }

                    let name = tokens[0].to_string();
                    if name.starts_with("validator") {
                        current_builder = Some(SetupBuilder::Validator { name });
                    } else {
                        current_builder = Some(SetupBuilder::Subnet {
                            name,
                            whitelist_names: None,
                            min_validators: None,
                        });
                    }
                    continue;
                }

                // Detail lines must belong to an active subnet builder.
                let Some(builder) = current_builder.as_mut() else {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        "expected 'validatorX' or 'subnetY' entity declaration",
                        original_line,
                    ));
                };

                match builder {
                    SetupBuilder::Validator { .. } => {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!(
                                "unexpected setup details under validator '{}' (validators have no fields yet)",
                                builder.name()
                            ),
                            original_line,
                        ));
                    }
                    SetupBuilder::Subnet {
                        whitelist_names,
                        min_validators,
                        ..
                    } => match tokens[0] {
                        "whitelist" => {
                            if tokens.len() < 5 {
                                return Err(EasyTesterError::parse(
                                    path.clone(),
                                    line_no,
                                    "whitelist requires at least 4 validator names",
                                    original_line,
                                ));
                            }
                            let names: Vec<String> =
                                tokens[1..].iter().map(|s| (*s).to_string()).collect();
                            *whitelist_names = Some(names);
                        }
                        "min" => {
                            if tokens.len() != 2 {
                                return Err(EasyTesterError::parse(
                                    path.clone(),
                                    line_no,
                                    "min requires exactly one numeric argument",
                                    original_line,
                                ));
                            }
                            let v = parse_u16_allow_underscores(tokens[1]).map_err(|e| {
                                EasyTesterError::parse(path.clone(), line_no, e, original_line)
                            })?;
                            *min_validators = Some(v);
                        }
                        _ => {
                            return Err(EasyTesterError::parse(
                                path.clone(),
                                line_no,
                                format!("unknown subnet setup field '{}'", tokens[0]),
                                original_line,
                            ));
                        }
                    },
                }
            }
            Section::Scenario => match tokens[0] {
                "block" => {
                    if tokens.len() != 2 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "block requires exactly one numeric argument",
                            original_line,
                        ));
                    }
                    let height = parse_u64_allow_underscores(tokens[1]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Block { height },
                    });
                }
                "create" => {
                    if tokens.len() != 2 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "create requires exactly one subnet name",
                            original_line,
                        ));
                    }
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Create {
                            subnet_name: tokens[1].to_string(),
                        },
                    });
                }
                "join" => {
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "join syntax: join <subnetName> <validatorName> <collateralSats>",
                            original_line,
                        ));
                    }
                    let collateral_sats = parse_u64_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Join {
                            subnet_name: tokens[1].to_string(),
                            validator_name: tokens[2].to_string(),
                            collateral_sats,
                        },
                    });
                }
                "stake" => {
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "stake syntax: stake <subnetName> <validatorName> <amountToAddSats>",
                            original_line,
                        ));
                    }
                    let amount_sats = parse_u64_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Stake {
                            subnet_name: tokens[1].to_string(),
                            validator_name: tokens[2].to_string(),
                            amount_sats,
                        },
                    });
                }
                "unstake" => {
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "unstake syntax: unstake <subnetName> <validatorName> <amountToRemoveSats>",
                            original_line,
                        ));
                    }
                    let amount_sats = parse_u64_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Unstake {
                            subnet_name: tokens[1].to_string(),
                            validator_name: tokens[2].to_string(),
                            amount_sats,
                        },
                    });
                }
                "checkpoint" => {
                    if tokens.len() != 2 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "checkpoint requires exactly one subnet name",
                            original_line,
                        ));
                    }
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Checkpoint {
                            subnet_name: tokens[1].to_string(),
                        },
                    });
                }
                "register_token" => {
                    // register_token <subnet> <name> <symbol> <decimals>
                    if tokens.len() != 5 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "register_token syntax: register_token <subnet> <name> <symbol> <decimals>",
                            original_line,
                        ));
                    }
                    let decimals: u8 = tokens[4].parse().map_err(|e| {
                        EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!("invalid decimals: {e}"),
                            original_line,
                        )
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::RegisterToken {
                            subnet_name: tokens[1].to_string(),
                            name: tokens[2].to_string(),
                            symbol: tokens[3].to_string(),
                            decimals,
                        },
                    });
                }
                "mint_token" => {
                    // mint_token <subnet> <token_name> <amount>
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "mint_token syntax: mint_token <subnet> <token_name> <amount>",
                            original_line,
                        ));
                    }
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::MintToken {
                            subnet_name: tokens[1].to_string(),
                            token_name: tokens[2].to_string(),
                            amount: tokens[3].to_string(),
                        },
                    });
                }
                "burn_token" => {
                    // burn_token <subnet> <token_name> <amount>
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "burn_token syntax: burn_token <subnet> <token_name> <amount>",
                            original_line,
                        ));
                    }
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::BurnToken {
                            subnet_name: tokens[1].to_string(),
                            token_name: tokens[2].to_string(),
                            amount: tokens[3].to_string(),
                        },
                    });
                }
                "erc_transfer" => {
                    // erc_transfer <src_subnet> <dst_subnet> <token_name> <amount>
                    if tokens.len() != 5 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "erc_transfer syntax: erc_transfer <src_subnet> <dst_subnet> <token_name> <amount>",
                            original_line,
                        ));
                    }
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::ErcTransfer {
                            src_subnet: tokens[1].to_string(),
                            dst_subnet: tokens[2].to_string(),
                            token_name: tokens[3].to_string(),
                            amount: tokens[4].to_string(),
                        },
                    });
                }
                "read" => {
                    if tokens.len() < 3 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "read syntax: read <db> <args...>",
                            original_line,
                        ));
                    }
                    let db = parse_output_db(tokens[1]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    let args = tokens[2..]
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>();
                    validate_output_args(&path, line_no, original_line, db, &args)?;

                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::OutputRead { db, args },
                    });
                }
                "expect" => {
                    let (target, expected_value) =
                        parse_output_expect(&path, line_no, original_line, &tokens[1..])?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::OutputExpect {
                            target,
                            expected_value,
                        },
                    });
                }
                "setup" | "scenario" => {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("unexpected section header '{}' inside scenario", tokens[0]),
                        original_line,
                    ));
                }
                other => {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        line_no,
                        format!("unknown scenario command '{}'", other),
                        original_line,
                    ));
                }
            },
        }
    }

    if !seen_setup {
        return Err(EasyTesterError::runtime(
            "test file did not contain a 'setup' section (after 'config')",
        ));
    }
    if !seen_scenario {
        return Err(EasyTesterError::runtime(
            "test file did not contain a 'scenario' section",
        ));
    }
    if let Some(b) = current_builder.take() {
        return Err(EasyTesterError::runtime(format!(
            "internal error: leftover setup builder '{}'",
            b.name()
        )));
    }

    let Some(config) = config else {
        return Err(EasyTesterError::runtime(
            "test file did not contain a valid 'config' section",
        ));
    };

    validate_scenario(&path, &setup, &scenario_entries)?;
    let scenario = scenario_entries
        .into_iter()
        .map(|e| e.cmd)
        .collect::<Vec<_>>();

    Ok(ParsedTest {
        config,
        setup,
        scenario,
    })
}

struct ScenarioEntry {
    line_no: usize,
    text: String,
    cmd: ScenarioCommand,
}

fn validate_scenario(
    path: &PathBuf,
    setup: &SetupSpec,
    scenario: &[ScenarioEntry],
) -> Result<(), EasyTesterError> {
    let mut block_set = false;
    let mut last_block_height: Option<u64> = None;
    let mut created_subnets: HashSet<String> = HashSet::new();
    let mut last_output_read_db: Option<OutputDb> = None;

    for entry in scenario {
        match &entry.cmd {
            ScenarioCommand::Block { height } => {
                if let Some(prev) = last_block_height {
                    if *height <= prev {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            entry.line_no,
                            format!(
                                "block heights must be strictly increasing (previous {}, got {})",
                                prev, height
                            ),
                            &entry.text,
                        ));
                    }
                }
                last_block_height = Some(*height);
                block_set = true;
            }
            ScenarioCommand::Create { subnet_name } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !setup.subnets.contains_key(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' is created more than once"),
                        &entry.text,
                    ));
                }
                created_subnets.insert(subnet_name.clone());
            }
            ScenarioCommand::Join {
                subnet_name,
                validator_name,
                collateral_sats,
            } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !setup.subnets.contains_key(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !setup.validators.contains_key(validator_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("validator '{validator_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before join"),
                        &entry.text,
                    ));
                }
                if *collateral_sats == 0 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "collateral must be greater than 0",
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::Stake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !setup.subnets.contains_key(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !setup.validators.contains_key(validator_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("validator '{validator_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before stake"),
                        &entry.text,
                    ));
                }
                if *amount_sats == 0 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "amount must be greater than 0",
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::Unstake {
                subnet_name,
                validator_name,
                amount_sats,
            } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !setup.subnets.contains_key(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !setup.validators.contains_key(validator_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("validator '{validator_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before unstake"),
                        &entry.text,
                    ));
                }
                if *amount_sats == 0 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "amount must be greater than 0",
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::Checkpoint { subnet_name } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !setup.subnets.contains_key(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' was not declared in setup"),
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before checkpoint"),
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::RegisterToken { subnet_name, .. } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before register_token"),
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::MintToken { subnet_name, .. }
            | ScenarioCommand::BurnToken { subnet_name, .. } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(subnet_name) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{subnet_name}' must be created before mint/burn"),
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::ErcTransfer {
                src_subnet,
                dst_subnet,
                ..
            } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(src_subnet) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{src_subnet}' must be created before erc_transfer"),
                        &entry.text,
                    ));
                }
                if !created_subnets.contains(dst_subnet) {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        format!("subnet '{dst_subnet}' must be created before erc_transfer"),
                        &entry.text,
                    ));
                }
            }
            ScenarioCommand::OutputRead { db, args } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }

                match db {
                    OutputDb::Subnet
                    | OutputDb::SubnetGenesis
                    | OutputDb::StakeChanges
                    | OutputDb::KillRequests
                    | OutputDb::Committee
                    | OutputDb::RewardCandidates => {
                        let subnet_arg_index = if *db == OutputDb::RewardCandidates {
                            1
                        } else {
                            0
                        };
                        let subnet_name = args.get(subnet_arg_index).ok_or_else(|| {
                            EasyTesterError::parse(
                                path.clone(),
                                entry.line_no,
                                "missing subnet name argument",
                                &entry.text,
                            )
                        })?;

                        if !setup.subnets.contains_key(subnet_name) {
                            return Err(EasyTesterError::parse(
                                path.clone(),
                                entry.line_no,
                                format!("subnet '{subnet_name}' was not declared in setup"),
                                &entry.text,
                            ));
                        }

                        if !created_subnets.contains(subnet_name) {
                            return Err(EasyTesterError::parse(
                                path.clone(),
                                entry.line_no,
                                format!("subnet '{subnet_name}' must be created before read"),
                                &entry.text,
                            ));
                        }
                    }
                    OutputDb::RewardResults => {}
                    OutputDb::RootnetMsgs | OutputDb::TokenBalance => {
                        let subnet_name = args.get(0).ok_or_else(|| {
                            EasyTesterError::parse(
                                path.clone(),
                                entry.line_no,
                                "missing subnet name argument",
                                &entry.text,
                            )
                        })?;
                        if !created_subnets.contains(subnet_name) {
                            return Err(EasyTesterError::parse(
                                path.clone(),
                                entry.line_no,
                                format!("subnet '{subnet_name}' must be created before read"),
                                &entry.text,
                            ));
                        }
                    }
                }

                last_output_read_db = Some(*db);
            }
            ScenarioCommand::OutputExpect {
                target: _target, ..
            } => {
                if !block_set {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "must set 'block <height>' before actions",
                        &entry.text,
                    ));
                }

                let valid_precursor = matches!(
                    last_output_read_db,
                    Some(OutputDb::RewardResults)
                        | Some(OutputDb::RootnetMsgs)
                        | Some(OutputDb::TokenBalance)
                );
                if !valid_precursor {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "expect is only supported after a 'read' command",
                        &entry.text,
                    ));
                }
            }
        }
    }

    Ok(())
}

fn parse_output_expect(
    path: &PathBuf,
    line_no: usize,
    original_line: &str,
    tokens: &[&str],
) -> Result<(OutputExpectTarget, String), EasyTesterError> {
    // Examples:
    // expect result.rewards_list.validator1 = 100_000 sats
    // expect result.total_rewarded_collateral = 1_000 sats
    let err = |msg: String| EasyTesterError::parse(path.clone(), line_no, msg, original_line);

    if tokens.len() < 3 {
        return Err(err("expected: expect <lhs> = <sats> [sats]".to_string()));
    }

    // Support both `lhs = rhs` and `lhs=rhs` forms.
    let mut flat: Vec<String> = Vec::new();
    for t in tokens {
        if *t == "=" {
            flat.push("=".to_string());
            continue;
        }
        if let Some((l, r)) = t.split_once('=') {
            if !l.trim().is_empty() {
                flat.push(l.trim().to_string());
            }
            flat.push("=".to_string());
            if !r.trim().is_empty() {
                flat.push(r.trim().to_string());
            }
        } else {
            flat.push((*t).to_string());
        }
    }

    let eq_pos = flat
        .iter()
        .position(|t| t == "=")
        .ok_or_else(|| err("expected '=' in expect (e.g. ... = 100_000 sats)".to_string()))?;

    if eq_pos == 0 || eq_pos + 1 >= flat.len() {
        return Err(err("expected: expect <lhs> = <sats> [sats]".to_string()));
    }

    let lhs = flat[0..eq_pos].join(" ");
    if lhs.contains(' ') {
        return Err(err(
            "lhs must not contain spaces (e.g. result.rewards_list.validator1)".to_string(),
        ));
    }

    let rhs_tokens = &flat[(eq_pos + 1)..];
    let rhs_str = rhs_tokens
        .get(0)
        .ok_or_else(|| err("missing rhs value".to_string()))?
        .to_string();

    // Allow optional "sats" unit suffix for numeric values
    if rhs_tokens.len() > 1 {
        let unit = rhs_tokens[1].to_ascii_lowercase();
        if unit != "sat" && unit != "sats" && unit != "satoshi" && unit != "satoshis" {
            return Err(err(format!(
                "unknown unit '{}' (expected 'sats')",
                rhs_tokens[1]
            )));
        }
    }
    if rhs_tokens.len() > 2 {
        return Err(err(
            "too many tokens after rhs; expected: <value> [sats]".to_string()
        ));
    }

    let path = lhs.strip_prefix("result.").ok_or_else(|| {
        err("lhs must start with 'result.' (e.g. result.count, result.0.kind)".to_string())
    })?;

    if path.is_empty() {
        return Err(err("path after 'result.' must not be empty".to_string()));
    }

    let target = OutputExpectTarget {
        path: path.to_string(),
    };

    Ok((target, rhs_str))
}

fn parse_output_db(s: &str) -> Result<OutputDb, String> {
    match s {
        "subnet" => Ok(OutputDb::Subnet),
        "subnet_genesis" => Ok(OutputDb::SubnetGenesis),
        "stake_changes" => Ok(OutputDb::StakeChanges),
        "kill_requests" => Ok(OutputDb::KillRequests),
        "committee" => Ok(OutputDb::Committee),
        "reward_candidates" => Ok(OutputDb::RewardCandidates),
        "reward_results" => Ok(OutputDb::RewardResults),
        "rootnet_msgs" => Ok(OutputDb::RootnetMsgs),
        "token_balance" => Ok(OutputDb::TokenBalance),
        _ => Err(format!("unknown output db '{s}'")),
    }
}

fn validate_output_args(
    path: &PathBuf,
    line_no: usize,
    original_line: &str,
    db: OutputDb,
    args: &[String],
) -> Result<(), EasyTesterError> {
    let err = |msg: &str| EasyTesterError::parse(path.clone(), line_no, msg, original_line);

    match db {
        OutputDb::Subnet | OutputDb::SubnetGenesis => {
            if args.len() != 1 {
                return Err(err("expected: read <db> <subnetName>"));
            }
        }
        OutputDb::Committee => {
            if args.len() != 2 {
                return Err(err(
                    "expected: read committee <subnetName> <committee_number>",
                ));
            }
            parse_u64_allow_underscores(&args[1])
                .map_err(|e| EasyTesterError::parse(path.clone(), line_no, e, original_line))?;
        }
        OutputDb::StakeChanges => {
            if args.len() != 2 {
                return Err(err(
                    "expected: read stake_changes <subnetName> <configuration_number>",
                ));
            }
            parse_u64_allow_underscores(&args[1])
                .map_err(|e| EasyTesterError::parse(path.clone(), line_no, e, original_line))?;
        }
        OutputDb::KillRequests => {
            if args.len() != 2 {
                return Err(err(
                    "expected: read kill_requests <subnetName> <current_block_height>",
                ));
            }
            parse_u64_allow_underscores(&args[1])
                .map_err(|e| EasyTesterError::parse(path.clone(), line_no, e, original_line))?;
        }
        OutputDb::RewardCandidates => {
            if args.len() != 2 {
                return Err(err(
                    "expected: read reward_candidates <snapshot> <subnetName>",
                ));
            }
            parse_u64_allow_underscores(&args[0])
                .map_err(|e| EasyTesterError::parse(path.clone(), line_no, e, original_line))?;
        }
        OutputDb::RewardResults => {
            if args.len() != 1 {
                return Err(err("expected: read reward_results <snapshot>"));
            }
            parse_u64_allow_underscores(&args[0])
                .map_err(|e| EasyTesterError::parse(path.clone(), line_no, e, original_line))?;
        }
        OutputDb::RootnetMsgs => {
            if args.len() != 1 {
                return Err(err("expected: read rootnet_msgs <subnetName>"));
            }
        }
        OutputDb::TokenBalance => {
            if args.len() != 2 {
                return Err(err("expected: read token_balance <subnetName> <tokenName>"));
            }
        }
    }

    Ok(())
}

fn parse_kv_pairs(tokens: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut flat: Vec<String> = Vec::new();
    for t in tokens {
        if *t == "=" {
            flat.push("=".to_string());
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().is_empty() {
                return Err(format!("invalid token '{t}'"));
            }
            flat.push(k.trim().to_string());
            flat.push("=".to_string());
            flat.push(v.trim().to_string());
        } else {
            flat.push((*t).to_string());
        }
    }

    let mut i = 0usize;
    let mut out = std::collections::HashMap::new();
    while i < flat.len() {
        let key = flat
            .get(i)
            .ok_or_else(|| "expected key".to_string())?
            .to_string();
        let eq = flat.get(i + 1).ok_or_else(|| "expected '='".to_string())?;
        if eq != "=" {
            return Err(format!("expected '=' after '{key}'"));
        }
        let value = flat
            .get(i + 2)
            .ok_or_else(|| format!("expected value after '{key}='"))?
            .to_string();
        if out.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate config key '{key}'"));
        }
        i += 3;
    }
    Ok(out)
}

fn require_kv_u64(
    map: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<u64, String> {
    let v = map
        .get(key)
        .ok_or_else(|| format!("missing required config key '{key}'"))?;
    parse_u64_allow_underscores(v)
}
