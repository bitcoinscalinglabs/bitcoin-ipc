use std::{collections::{HashMap, HashSet}, fs, path::PathBuf};

use crate::easy_tester::{
    error::EasyTesterError,
    model::{
        generate_validator, normalize_numeric_literal, parse_u16_allow_underscores,
        parse_u64_allow_underscores, parse_u256_allow_underscores, FendermintIssuer,
        FendermintSetup, FendermintSubnet, OutputDb, OutputExpectTarget, ParsedFendermintTest,
        ParsedTest, ScenarioCommand, SetupSpec, SubnetSpec, TesterConfig,
    },
};

enum Section {
    None,
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
                if tokens.len() == 1 && tokens[0] == "setup" {
                    seen_setup = true;
                    section = Section::Setup;
                    continue;
                }
                return Err(EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    "expected 'setup' as the first section",
                    original_line,
                ));
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
                "wait" => {
                    if tokens.len() != 2 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "wait syntax: wait <seconds>",
                            original_line,
                        ));
                    }
                    let seconds = parse_u64_allow_underscores(tokens[1]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Wait { seconds },
                    });
                }
                "deposit" => {
                    if tokens.len() != 4 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "deposit syntax: deposit <subnet> <address_name> <amount_sats>",
                            original_line,
                        ));
                    }
                    let amount_sats = parse_u64_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, e, original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::Deposit {
                            subnet_name: tokens[1].to_string(),
                            address_name: tokens[2].to_string(),
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
                    // register_token <subnet> <name> <symbol> <initial_supply>
                    if tokens.len() != 5 {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            "register_token syntax: register_token <subnet> <name> <symbol> <initial_supply>",
                            original_line,
                        ));
                    }
                    let initial_supply = parse_u256_allow_underscores(tokens[4]).map_err(|e| {
                        EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!("invalid initial_supply: {e}"),
                            original_line,
                        )
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::RegisterToken {
                            subnet_name: tokens[1].to_string(),
                            issuer: None,
                            name: tokens[2].to_string(),
                            symbol: tokens[3].to_string(),
                            initial_supply,
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
                    let amount = parse_u256_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, format!("invalid amount: {e}"), original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::MintToken {
                            subnet_name: tokens[1].to_string(),
                            token_name: tokens[2].to_string(),
                            amount,
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
                    let amount = parse_u256_allow_underscores(tokens[3]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, format!("invalid amount: {e}"), original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::BurnToken {
                            subnet_name: tokens[1].to_string(),
                            token_name: tokens[2].to_string(),
                            amount,
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
                    let amount = parse_u256_allow_underscores(tokens[4]).map_err(|e| {
                        EasyTesterError::parse(path.clone(), line_no, format!("invalid amount: {e}"), original_line)
                    })?;
                    scenario_entries.push(ScenarioEntry {
                        line_no,
                        text: original_line.to_string(),
                        cmd: ScenarioCommand::ErcTransfer {
                            src_subnet: tokens[1].to_string(),
                            src_actor: None,
                            dst_subnet: tokens[2].to_string(),
                            dst_actor: None,
                            token_name: tokens[3].to_string(),
                            amount,
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
                        .map(|s| normalize_numeric_literal(s))
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
                            expected_value: normalize_numeric_literal(&expected_value),
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
            "test file did not contain a 'setup' section",
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

    validate_scenario(&path, &setup, &scenario_entries)?;
    let scenario = scenario_entries
        .into_iter()
        .map(|e| (e.line_no, e.cmd))
        .collect::<Vec<_>>();

    Ok(ParsedTest { setup, scenario })
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
            ScenarioCommand::Wait { .. } => {}
            ScenarioCommand::Deposit { amount_sats, .. } => {
                if *amount_sats == 0 {
                    return Err(EasyTesterError::parse(
                        path.clone(),
                        entry.line_no,
                        "deposit amount must be greater than 0",
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
                    OutputDb::TokenMetadata => {
                        // Only valid for FendermintTester — skip validation here
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
                        | Some(OutputDb::TokenMetadata)
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
        line_no,
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
        "token_metadata" => Ok(OutputDb::TokenMetadata),
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
            if args.len() != 2 && args.len() != 3 {
                return Err(err("expected: read token_balance <subnetName> <tokenName> or read token_balance <subnetName> <actorName> <tokenName>"));
            }
        }
        OutputDb::TokenMetadata => {
            if args.len() != 2 {
                return Err(err("expected: read token_metadata <subnetName> <tokenName>"));
            }
        }
    }

    Ok(())
}

/// Parse a separate tester config file (space-separated `key value` lines, no `=`).
///
/// Supported keys: `tester`, `activation_height`, `snapshot_length`.
/// `tester` must be `db`, `monitor`, or `fendermint`.
///
/// Fendermint config additionally supports:
///   `subnet1 <subnet_id> <eth_rpc_url> <provider_url>`
///   `docker_container <name>`
pub fn parse_config_file(path: impl Into<PathBuf>) -> Result<TesterConfig, EasyTesterError> {
    let path = path.into();
    let raw = fs::read_to_string(&path).map_err(|e| EasyTesterError::Io {
        path: path.clone(),
        source: e,
    })?;

    let mut tester_type: Option<String> = None;
    let mut activation_height: Option<u64> = None;
    let mut snapshot_length: Option<u64> = None;
    let mut monitor_log_level: Option<String> = None;
    let mut provider_log_level: Option<String> = None;

    // Fendermint-specific
    let mut fm_docker_container = "bitcoin-ipc".to_string();
    let mut fm_subnets: HashMap<String, FendermintSubnet> = HashMap::new();
    let mut fm_subnet_order: Vec<String> = Vec::new();
    let mut fm_print_queries = false;

    for (idx0, original_line) in raw.lines().enumerate() {
        let line_no = idx0 + 1;
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

        match tokens[0] {
            "tester" => {
                if tokens.len() != 2 {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no, "tester requires exactly one argument", original_line,
                    ));
                }
                tester_type = Some(tokens[1].to_string());
            }
            "activation_height" | "snapshot_length" | "monitor_log_level" | "provider_log_level" => {
                if tokens.len() != 2 {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no,
                        format!("{} requires exactly one argument", tokens[0]),
                        original_line,
                    ));
                }
                match tokens[0] {
                    "activation_height" => {
                        activation_height = Some(
                            parse_u64_allow_underscores(tokens[1]).map_err(|e| {
                                EasyTesterError::parse(path.clone(), line_no, e, original_line)
                            })?,
                        );
                    }
                    "snapshot_length" => {
                        snapshot_length = Some(
                            parse_u64_allow_underscores(tokens[1]).map_err(|e| {
                                EasyTesterError::parse(path.clone(), line_no, e, original_line)
                            })?,
                        );
                    }
                    "monitor_log_level" => monitor_log_level = Some(tokens[1].to_string()),
                    "provider_log_level" => provider_log_level = Some(tokens[1].to_string()),
                    _ => unreachable!(),
                }
            }
            "docker_container" => {
                if tokens.len() != 2 {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no,
                        "docker_container requires exactly one argument", original_line,
                    ));
                }
                fm_docker_container = tokens[1].to_string();
            }
            "print_ipc_queries" => {
                if tokens.len() != 2 {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no,
                        "print_ipc_queries requires exactly one argument (on|off)", original_line,
                    ));
                }
                fm_print_queries = match tokens[1] {
                    "on" => true,
                    "off" => false,
                    other => {
                        return Err(EasyTesterError::parse(
                            path.clone(), line_no,
                            format!("print_ipc_queries value must be 'on' or 'off', got '{other}'"),
                            original_line,
                        ));
                    }
                };
            }
            name if name.starts_with("subnet") => {
                // subnet1 <subnet_id> <eth_rpc_url> <provider_url>
                if tokens.len() != 4 {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no,
                        "subnet syntax: <name> <subnet_id> <eth_rpc_url> <provider_url>",
                        original_line,
                    ));
                }
                let sname = tokens[0].to_string();
                if fm_subnets.contains_key(&sname) {
                    return Err(EasyTesterError::parse(
                        path.clone(), line_no,
                        format!("duplicate subnet '{sname}'"), original_line,
                    ));
                }
                fm_subnet_order.push(sname.clone());
                fm_subnets.insert(sname.clone(), FendermintSubnet {
                    name: sname,
                    subnet_id: tokens[1].to_string(),
                    eth_rpc_url: tokens[2].to_string(),
                    provider_url: tokens[3].to_string(),
                });
            }
            other => {
                return Err(EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    format!("unknown config key '{other}'"),
                    original_line,
                ));
            }
        }
    }

    let Some(tester_type) = tester_type else {
        return Err(EasyTesterError::runtime(
            "config file missing required 'tester' line",
        ));
    };

    match tester_type.as_str() {
        "db" => Ok(TesterConfig::Db {
            activation_height,
            snapshot_length,
        }),
        "monitor" => Ok(TesterConfig::Monitor {
            activation_height,
            snapshot_length,
            monitor_log_level,
            provider_log_level,
        }),
        "fendermint" => {
            if fm_subnets.is_empty() {
                return Err(EasyTesterError::runtime(
                    "fendermint config must declare at least one subnet",
                ));
            }
            Ok(TesterConfig::Fendermint {
                setup: FendermintSetup {
                    docker_container: fm_docker_container,
                    issuers: HashMap::new(), // parsed from scenario file
                    subnets: fm_subnets,
                    subnet_order: fm_subnet_order,
                    print_queries: fm_print_queries,
                },
            })
        }
        other => Err(EasyTesterError::runtime(format!(
            "unknown tester '{other}' (expected 'db', 'monitor', or 'fendermint')"
        ))),
    }
}

/// Validate block heights in `parsed` against `config`.
///
/// For `monitor`, the first `block N` command must have N ≥ 102 (bitcoind
/// pre-mines 101 blocks on startup, so height 101 is already confirmed and
/// `mine_to_height(101)` would be a no-op).
pub fn validate_scenario_for_tester(
    parsed: &ParsedTest,
    config: &TesterConfig,
) -> Result<(), EasyTesterError> {
    if !matches!(config, TesterConfig::Monitor { .. }) {
        return Ok(());
    }
    for (_line_no, cmd) in &parsed.scenario {
        if let ScenarioCommand::Block { height } = cmd {
            if *height < 102 {
                return Err(EasyTesterError::runtime(format!(
                    "monitor tester: block heights must be ≥ 102 \
                     (bitcoind pre-mines 101 blocks; got block {height})"
                )));
            }
        }
    }
    Ok(())
}

// ── Fendermint test file parser ────────────────────────────────────────

/// Parse a scenario file with `tester fendermint` in the setup section.
/// Subnets are provided via the config file (already in `config_setup`);
/// the scenario's setup section declares only issuers.
///
/// Setup syntax:
/// ```text
/// setup
/// tester fendermint
///
/// issuer1 0x27b60d9f71d6806cca7d5a92b391093fe100f8e8
/// user1 0x005e05dd763dd125473f8889726f7c305e50fcae
///
/// scenario
/// register_token subnet1 issuer1 TOKEN1 TK1 1_000_000
/// ...
/// ```
pub fn parse_fendermint_test_file(
    path: impl Into<PathBuf>,
    config_setup: FendermintSetup,
) -> Result<ParsedFendermintTest, EasyTesterError> {
    let path = path.into();
    let raw = fs::read_to_string(&path).map_err(|e| EasyTesterError::Io {
        path: path.clone(),
        source: e,
    })?;

    let mut section = Section::None;
    let mut setup = config_setup;
    let mut scenario: Vec<(usize, ScenarioCommand)> = Vec::new();
    let mut seen_tester_line = false;

    for (idx0, original_line) in raw.lines().enumerate() {
        let line_no = idx0 + 1;
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
                if tokens.len() == 1 && tokens[0] == "setup" {
                    section = Section::Setup;
                    continue;
                }
                return Err(EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    "expected 'setup' as the first section",
                    original_line,
                ));
            }
            Section::Setup => {
                if tokens.len() == 1 && tokens[0] == "scenario" {
                    section = Section::Scenario;
                    continue;
                }

                // "tester fendermint" — required, already detected by caller
                if tokens.len() == 2 && tokens[0] == "tester" && tokens[1] == "fendermint" {
                    seen_tester_line = true;
                    continue;
                }

                // Issuer line: <issuer_name> <0xAddress>
                // Exactly 2 tokens, second starts with 0x, name doesn't start with "subnet"
                if tokens.len() == 2
                    && tokens[1].starts_with("0x")
                    && !tokens[0].starts_with("subnet")
                {
                    let name = tokens[0].to_string();
                    if setup.issuers.contains_key(&name) {
                        return Err(EasyTesterError::parse(
                            path.clone(),
                            line_no,
                            format!("duplicate issuer '{name}'"),
                            original_line,
                        ));
                    }
                    setup.issuers.insert(
                        name.clone(),
                        FendermintIssuer {
                            name: name.clone(),
                            ipc_address: tokens[1].to_string(),
                        },
                    );
                    continue;
                }

                return Err(EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    "unrecognised setup line (expected: issuer or tester)",
                    original_line,
                ));
            }
            Section::Scenario => {
                let cmd = parse_fendermint_scenario_command(
                    &path,
                    line_no,
                    original_line,
                    &tokens,
                    &setup,
                )?;
                scenario.push((line_no, cmd));
            }
        }
    }

    if !seen_tester_line {
        return Err(EasyTesterError::runtime(
            "fendermint test file must contain 'tester fendermint' in setup",
        ));
    }
    if setup.issuers.is_empty() {
        return Err(EasyTesterError::runtime(
            "fendermint test file must declare at least one issuer",
        ));
    }
    if setup.subnets.is_empty() {
        return Err(EasyTesterError::runtime(
            "fendermint test file must declare at least one subnet",
        ));
    }

    Ok(ParsedFendermintTest { setup, scenario })
}

fn parse_fendermint_scenario_command(
    path: &PathBuf,
    line_no: usize,
    original_line: &str,
    tokens: &[&str],
    setup: &FendermintSetup,
) -> Result<ScenarioCommand, EasyTesterError> {
    let err =
        |msg: &str| EasyTesterError::parse(path.clone(), line_no, msg, original_line);

    match tokens[0] {
        // NOPs: block, checkpoint, create, join, stake, unstake
        "block" | "checkpoint" => Ok(ScenarioCommand::Block { height: 0 }),
        "create" | "join" | "stake" | "unstake" => Err(err(
            &format!("'{}' is not supported by FendermintTester", tokens[0]),
        )),

        // register_token <subnet> <issuer> <name> <symbol> <initial_supply>
        "register_token" => {
            if tokens.len() != 6 {
                return Err(err(
                    "register_token syntax: register_token <subnet> <issuer> <name> <symbol> <initial_supply>",
                ));
            }
            let subnet_name = tokens[1].to_string();
            if !setup.subnets.contains_key(&subnet_name) {
                return Err(err(&format!(
                    "subnet '{subnet_name}' not declared in setup"
                )));
            }
            let issuer_name = tokens[2].to_string();
            if !setup.issuers.contains_key(&issuer_name) {
                return Err(err(&format!(
                    "issuer '{issuer_name}' not declared in setup"
                )));
            }
            let initial_supply = parse_u256_allow_underscores(tokens[5]).map_err(|e| {
                EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    format!("invalid initial_supply: {e}"),
                    original_line,
                )
            })?;
            Ok(ScenarioCommand::RegisterToken {
                subnet_name,
                issuer: Some(issuer_name),
                name: tokens[3].to_string(),
                symbol: tokens[4].to_string(),
                initial_supply,
            })
        }

        // mint_token <subnet> <token_name> <amount>
        "mint_token" => {
            if tokens.len() != 4 {
                return Err(err("mint_token syntax: mint_token <subnet> <token_name> <amount>"));
            }
            let amount = parse_u256_allow_underscores(tokens[3]).map_err(|e| {
                EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    format!("invalid amount: {e}"),
                    original_line,
                )
            })?;
            Ok(ScenarioCommand::MintToken {
                subnet_name: tokens[1].to_string(),
                token_name: tokens[2].to_string(),
                amount,
            })
        }

        // burn_token <subnet> <token_name> <amount>
        "burn_token" => {
            if tokens.len() != 4 {
                return Err(err("burn_token syntax: burn_token <subnet> <token_name> <amount>"));
            }
            let amount = parse_u256_allow_underscores(tokens[3]).map_err(|e| {
                EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    format!("invalid amount: {e}"),
                    original_line,
                )
            })?;
            Ok(ScenarioCommand::BurnToken {
                subnet_name: tokens[1].to_string(),
                token_name: tokens[2].to_string(),
                amount,
            })
        }

        // wait <seconds>
        "wait" => {
            if tokens.len() != 2 {
                return Err(err("wait syntax: wait <seconds>"));
            }
            let seconds = parse_u64_allow_underscores(tokens[1]).map_err(|e| {
                EasyTesterError::parse(path.clone(), line_no, format!("invalid seconds: {e}"), original_line)
            })?;
            Ok(ScenarioCommand::Wait { seconds })
        }

        // deposit <subnet> <address_name> <amount_sats>
        "deposit" => {
            if tokens.len() != 4 {
                return Err(err("deposit syntax: deposit <subnet> <address_name> <amount_sats>"));
            }
            let subnet_name = tokens[1].to_string();
            if !setup.subnets.contains_key(&subnet_name) {
                return Err(err(&format!("subnet '{subnet_name}' not declared in setup")));
            }
            let address_name = tokens[2].to_string();
            if !setup.issuers.contains_key(&address_name) {
                return Err(err(&format!("address '{address_name}' not declared as issuer in setup")));
            }
            let amount_sats = parse_u64_allow_underscores(tokens[3]).map_err(|e| {
                EasyTesterError::parse(path.clone(), line_no, format!("invalid amount: {e}"), original_line)
            })?;
            Ok(ScenarioCommand::Deposit {
                subnet_name,
                address_name,
                amount_sats,
            })
        }

        // erc_transfer <src_subnet> <src_actor> <dst_subnet> <dst_actor> <token> <amount>
        "erc_transfer" => {
            if tokens.len() != 7 {
                return Err(err(
                    "erc_transfer syntax: erc_transfer <src_subnet> <src_actor> <dst_subnet> <dst_actor> <token_name> <amount>",
                ));
            }
            let src_subnet = tokens[1].to_string();
            let dst_subnet = tokens[3].to_string();
            if !setup.subnets.contains_key(&src_subnet) {
                return Err(err(&format!("subnet '{src_subnet}' not declared in setup")));
            }
            if !setup.subnets.contains_key(&dst_subnet) {
                return Err(err(&format!("subnet '{dst_subnet}' not declared in setup")));
            }
            let src_actor = tokens[2].to_string();
            let dst_actor = tokens[4].to_string();
            if !setup.issuers.contains_key(&src_actor) {
                return Err(err(&format!("actor '{src_actor}' not declared as issuer in setup")));
            }
            if !setup.issuers.contains_key(&dst_actor) {
                return Err(err(&format!("actor '{dst_actor}' not declared as issuer in setup")));
            }
            let amount = parse_u256_allow_underscores(tokens[6]).map_err(|e| {
                EasyTesterError::parse(
                    path.clone(),
                    line_no,
                    format!("invalid amount: {e}"),
                    original_line,
                )
            })?;
            Ok(ScenarioCommand::ErcTransfer {
                src_subnet,
                src_actor: Some(src_actor),
                dst_subnet,
                dst_actor: Some(dst_actor),
                token_name: tokens[5].to_string(),
                amount,
            })
        }

        // read token_balance <subnet> <actor> <token>
        // read token_metadata <subnet> <token>
        // other reads → NOP
        "read" => {
            if tokens.len() < 2 {
                return Err(err("read syntax: read <type> <args...>"));
            }
            match tokens[1] {
                "token_balance" => {
                    if tokens.len() != 5 {
                        return Err(err(
                            "read token_balance syntax: read token_balance <subnet> <actor> <token>",
                        ));
                    }
                    let subnet = tokens[2].to_string();
                    if !setup.subnets.contains_key(&subnet) {
                        return Err(err(&format!("subnet '{subnet}' not declared in setup")));
                    }
                    let actor = tokens[3].to_string();
                    if !setup.issuers.contains_key(&actor) {
                        return Err(err(&format!("actor '{actor}' not declared as issuer in setup")));
                    }
                    Ok(ScenarioCommand::OutputRead {
                        db: OutputDb::TokenBalance,
                        args: vec![subnet, actor, tokens[4].to_string()],
                    })
                }
                "token_metadata" => {
                    if tokens.len() != 4 {
                        return Err(err(
                            "read token_metadata syntax: read token_metadata <subnet> <token>",
                        ));
                    }
                    Ok(ScenarioCommand::OutputRead {
                        db: OutputDb::TokenMetadata,
                        args: vec![tokens[2].to_string(), tokens[3].to_string()],
                    })
                }
                other => {
                    // Unsupported read — will NOP at runtime
                    let args = tokens[2..].iter().map(|s| s.to_string()).collect();
                    let db = parse_output_db(other).unwrap_or(OutputDb::Subnet);
                    Ok(ScenarioCommand::OutputRead { db, args })
                }
            }
        }

        // expect result.X = Y  or  expect result.X.not_empty
        "expect" => {
            // Check for .not_empty form: "expect result.wrapped_token.not_empty"
            if tokens.len() == 2 && tokens[1].ends_with(".not_empty") {
                let lhs = tokens[1]
                    .strip_suffix(".not_empty")
                    .unwrap()
                    .strip_prefix("result.")
                    .ok_or_else(|| err("lhs must start with 'result.'"))?;
                return Ok(ScenarioCommand::OutputExpect {
                    target: OutputExpectTarget {
                        path: lhs.to_string(),
                        line_no,
                    },
                    expected_value: "__not_empty__".to_string(),
                });
            }

            // Standard expect: reuse existing parser
            let (target, expected_value) =
                parse_output_expect(&path, line_no, original_line, &tokens[1..])?;
            Ok(ScenarioCommand::OutputExpect {
                target,
                expected_value: normalize_numeric_literal(&expected_value),
            })
        }

        other => Err(err(&format!("unknown command '{other}'"))),
    }
}
