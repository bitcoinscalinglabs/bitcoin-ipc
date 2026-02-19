use std::{collections::HashSet, fs, path::PathBuf};

use crate::easy_tester::{
    error::EasyTesterError,
    model::{
        parse_u16_allow_underscores, parse_u64_allow_underscores, generate_validator, ParsedTestFile,
        ScenarioCommand, SetupSpec, SubnetSpec,
    },
};

enum Section {
    None,
    Setup,
    Scenario,
}

enum SetupBuilder {
    Validator { name: String },
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

pub fn parse_test_file(path: impl Into<PathBuf>) -> Result<ParsedTestFile, EasyTesterError> {
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
        let line_trimmed = original_line.trim();

        if line_trimmed.is_empty() || line_trimmed.starts_with('#') {
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
            Section::Scenario => {
                match tokens[0] {
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
                }
            }
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
        .map(|e| e.cmd)
        .collect::<Vec<_>>();

    Ok(ParsedTestFile { setup, scenario })
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
    let mut created_subnets: HashSet<String> = HashSet::new();

    for entry in scenario {
        match &entry.cmd {
            ScenarioCommand::Block { .. } => {
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
        }
    }

    Ok(())
}
