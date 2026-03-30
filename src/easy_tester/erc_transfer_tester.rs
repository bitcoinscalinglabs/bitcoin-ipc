use std::collections::HashMap;

use log::info;

use crate::{
    db::DatabaseCore,
    easy_tester::{
        base::BaseTester,
        error::EasyTesterError,
        model::{OutputDb, OutputExpectTarget, SetupSpec},
        tester::Tester,
    },
    ipc_lib::{IpcCrossSubnetErcTransfer, IpcErcSupplyAdjustment, IpcErcTokenRegistration},
};

pub struct ErcTransferTester {
    base: BaseTester,
    /// Registered tokens by name → (home_subnet, registration).
    /// Populated by `register_token`, used by `erc_transfer` to look up the token address.
    registered_tokens: HashMap<String, (String, IpcErcTokenRegistration)>,
    /// Pending token registrations queued by `register_token`, consumed by next checkpoint
    pending_registrations: HashMap<String, Vec<IpcErcTokenRegistration>>,
    /// Pending supply adjustments queued by `mint_token`/`burn_token`, consumed by next checkpoint
    pending_supply_adjustments: HashMap<String, Vec<IpcErcSupplyAdjustment>>,
    /// Pending ERC transfers queued by `erc_transfer`, consumed by next checkpoint
    pending_erc_transfers: HashMap<String, Vec<IpcCrossSubnetErcTransfer>>,
    /// Last read rootnet messages for expect
    last_rootnet_msgs: Option<LastRootnetMsgs>,
    /// Last read token balance for expect
    last_token_balance: Option<u64>,
}

#[derive(Debug)]
struct LastRootnetMsgs {
    _subnet_name: String,
    msgs: Vec<crate::db::RootnetMessage>,
}

impl ErcTransferTester {
    pub async fn new(setup: SetupSpec) -> Result<Self, EasyTesterError> {
        let base = BaseTester::new(setup).await?;
        Ok(Self {
            base,
            registered_tokens: HashMap::new(),
            pending_registrations: HashMap::new(),
            pending_supply_adjustments: HashMap::new(),
            pending_erc_transfers: HashMap::new(),
            last_rootnet_msgs: None,
            last_token_balance: None,
        })
    }

    fn register_token_impl(
        &mut self,
        _height: u64,
        subnet_name: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
    ) -> Result<(), EasyTesterError> {
        self.base.resolve_subnet_id(subnet_name)?;

        // Reuse existing token address if already registered (duplicate registration is allowed)
        let home_token_address = if let Some((prev_subnet, prev_reg)) = self.registered_tokens.get(name) {
            if prev_subnet != subnet_name {
                return Err(EasyTesterError::runtime(format!(
                    "token '{}' was already registered on subnet '{}', cannot re-register on '{}'",
                    name, prev_subnet, subnet_name
                )));
            }
            info!(
                "Duplicate registration for token '{}' on subnet '{}' (allowed, will be ignored by L2 contract)",
                name, subnet_name
            );
            prev_reg.home_token_address
        } else {
            alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>())
        };

        let initial_supply = alloy_primitives::U256::from(1_000_000u64);

        let etr = IpcErcTokenRegistration {
            home_token_address,
            name: name.to_string(),
            symbol: symbol.to_string(),
            decimals,
            initial_supply,
        };

        self.registered_tokens
            .insert(name.to_string(), (subnet_name.to_string(), etr.clone()));

        self.pending_registrations
            .entry(subnet_name.to_string())
            .or_default()
            .push(etr);

        info!(
            "Queued token registration on subnet '{}': {} ({}), {} decimals",
            subnet_name, name, symbol, decimals
        );
        Ok(())
    }

    fn erc_transfer_impl(
        &mut self,
        _height: u64,
        src_subnet: &str,
        dst_subnet: &str,
        token_name: &str,
        amount_str: &str,
    ) -> Result<(), EasyTesterError> {
        self.base.resolve_subnet_id(src_subnet)?; // verify src exists
        let destination_subnet_id = self.base.resolve_subnet_id(dst_subnet)?;

        let (reg_subnet, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "token '{}' not registered (use register_token first)",
                token_name
            ))
        })?;

        // home_subnet_id is always the subnet where the token was registered,
        // regardless of which subnet is sending the transfer.
        let home_subnet_id = self.base.resolve_subnet_id(reg_subnet)?;

        let amount = alloy_primitives::U256::from(
            amount_str.parse::<u64>()
                .map_err(|e| EasyTesterError::runtime(format!("invalid amount '{amount_str}': {e}")))?,
        );

        let etx = IpcCrossSubnetErcTransfer {
            home_subnet_id,
            home_token_address: reg.home_token_address,
            amount,
            destination_subnet_id,
            recipient: alloy_primitives::Address::from_slice(&rand::random::<[u8; 20]>()),
        };

        self.pending_erc_transfers
            .entry(src_subnet.to_string())
            .or_default()
            .push(etx);

        info!(
            "Queued ERC transfer from subnet '{}' to subnet '{}', token='{}', amount={}",
            src_subnet, dst_subnet, token_name, amount_str
        );
        Ok(())
    }

    fn mint_token_impl(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount_str: &str,
    ) -> Result<(), EasyTesterError> {
        self.base.resolve_subnet_id(subnet_name)?;
        let (_reg_subnet, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;

        let amount_u64: u64 = amount_str.parse().map_err(|e| {
            EasyTesterError::runtime(format!("invalid amount '{amount_str}': {e}"))
        })?;
        let delta = alloy_primitives::I256::try_from(amount_u64 as i64)
            .map_err(|e| EasyTesterError::runtime(format!("amount too large: {e}")))?;

        let ems = IpcErcSupplyAdjustment {
            home_token_address: reg.home_token_address,
            delta,
        };
        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(ems);

        info!("Queued mint for token '{}' on subnet '{}', amount={}", token_name, subnet_name, amount_str);
        Ok(())
    }

    fn burn_token_impl(
        &mut self,
        _height: u64,
        subnet_name: &str,
        token_name: &str,
        amount_str: &str,
    ) -> Result<(), EasyTesterError> {
        self.base.resolve_subnet_id(subnet_name)?;
        let (_reg_subnet, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{}' not registered", token_name))
        })?;

        let amount_u64: u64 = amount_str.parse().map_err(|e| {
            EasyTesterError::runtime(format!("invalid amount '{amount_str}': {e}"))
        })?;
        let delta = alloy_primitives::I256::try_from(-(amount_u64 as i64))
            .map_err(|e| EasyTesterError::runtime(format!("amount too large: {e}")))?;

        let ems = IpcErcSupplyAdjustment {
            home_token_address: reg.home_token_address,
            delta,
        };
        self.pending_supply_adjustments
            .entry(subnet_name.to_string())
            .or_default()
            .push(ems);

        info!("Queued burn for token '{}' on subnet '{}', amount={}", token_name, subnet_name, amount_str);
        Ok(())
    }

    /// Extract a string field value from a rootnet message for expect comparisons.
    fn msg_field_value(msg: &crate::db::RootnetMessage, field: &str) -> Result<String, String> {
        match field {
            "kind" => Ok(match msg {
                crate::db::RootnetMessage::FundSubnet { .. } => "fund".to_string(),
                crate::db::RootnetMessage::ErcTransfer { .. } => "erc_transfer".to_string(),
                crate::db::RootnetMessage::ErcRegistration { .. } => "erc_registration".to_string(),
            }),
            "tokenName" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.name.clone())
                }
                _ => Err(format!(
                    "field 'tokenName' only available on erc_registration messages"
                )),
            },
            "tokenSymbol" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.symbol.clone())
                }
                _ => Err(format!(
                    "field 'tokenSymbol' only available on erc_registration messages"
                )),
            },
            "tokenDecimals" => match msg {
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(registration.decimals.to_string())
                }
                _ => Err(format!(
                    "field 'tokenDecimals' only available on erc_registration messages"
                )),
            },
            "token" => match msg {
                crate::db::RootnetMessage::ErcTransfer { msg: etx, .. } => {
                    Ok(format!("{}", etx.home_token_address))
                }
                crate::db::RootnetMessage::ErcRegistration { registration, .. } => {
                    Ok(format!("{}", registration.home_token_address))
                }
                _ => Err(format!("field 'token' only available on ERC messages")),
            },
            "amount" => match msg {
                crate::db::RootnetMessage::ErcTransfer { msg: etx, .. } => {
                    Ok(etx.amount.to_string())
                }
                crate::db::RootnetMessage::FundSubnet { msg, .. } => {
                    Ok(msg.amount.to_sat().to_string())
                }
                _ => Err(format!(
                    "field 'amount' not available on erc_registration messages"
                )),
            },
            _ => Err(format!("unknown field '{}'", field)),
        }
    }
}

impl Tester for ErcTransferTester {
    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError> {
        self.base.mine_block(height)
    }

    fn exec_create_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        self.base.create_subnet(height, subnet_name)
    }

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .join_subnet(height, subnet_name, validator_name, collateral_sats)
    }

    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .stake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        self.base
            .unstake_subnet(height, subnet_name, validator_name, amount_sats)
    }

    fn exec_checkpoint_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let token_registrations = self
            .pending_registrations
            .remove(subnet_name)
            .unwrap_or_default();
        let supply_adjustments = self
            .pending_supply_adjustments
            .remove(subnet_name)
            .unwrap_or_default();
        let erc_transfers = self
            .pending_erc_transfers
            .remove(subnet_name)
            .unwrap_or_default();

        self.base
            .checkpoint_subnet(height, subnet_name, token_registrations, supply_adjustments, erc_transfers)?;
        Ok(())
    }

    fn exec_register_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
    ) -> Result<(), EasyTesterError> {
        self.register_token_impl(height, subnet_name, name, symbol, decimals)
    }

    fn exec_mint_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: &str,
    ) -> Result<(), EasyTesterError> {
        self.mint_token_impl(height, subnet_name, token_name, amount)
    }

    fn exec_burn_token(
        &mut self,
        height: u64,
        subnet_name: &str,
        token_name: &str,
        amount: &str,
    ) -> Result<(), EasyTesterError> {
        self.burn_token_impl(height, subnet_name, token_name, amount)
    }

    fn exec_erc_transfer(
        &mut self,
        height: u64,
        src_subnet: &str,
        dst_subnet: &str,
        token_name: &str,
        amount: &str,
    ) -> Result<(), EasyTesterError> {
        self.erc_transfer_impl(height, src_subnet, dst_subnet, token_name, amount)
    }

    fn exec_output_read(
        &mut self,
        _height: u64,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError> {
        match db {
            OutputDb::RootnetMsgs => {
                let subnet_name = &args[0];
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;
                let msgs = self
                    .base
                    .db
                    .get_all_rootnet_msgs(subnet_id)
                    .map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

                println!(
                    "OUTPUT read rootnet_msgs subnet='{}': {} messages",
                    subnet_name,
                    msgs.len()
                );
                for (i, msg) in msgs.iter().enumerate() {
                    let kind = match msg {
                        crate::db::RootnetMessage::FundSubnet { .. } => "fund",
                        crate::db::RootnetMessage::ErcTransfer { .. } => "erc_transfer",
                        crate::db::RootnetMessage::ErcRegistration { .. } => "erc_registration",
                    };
                    println!("  [{}] kind={}, nonce={}", i, kind, msg.nonce());
                }

                self.last_rootnet_msgs = Some(LastRootnetMsgs {
                    _subnet_name: subnet_name.to_string(),
                    msgs,
                });
                self.last_token_balance = None;
            }
            OutputDb::TokenBalance => {
                let subnet_name = &args[0];
                let token_name = &args[1];
                let subnet_id = self.base.resolve_subnet_id(subnet_name)?;

                let (_reg_subnet, reg) = self.registered_tokens.get(token_name.as_str()).ok_or_else(|| {
                    EasyTesterError::runtime(format!("token '{}' not registered", token_name))
                })?;
                let home_subnet_id = self.base.resolve_subnet_id(_reg_subnet)?;

                let balance = self.base.db.get_token_balance(
                    home_subnet_id,
                    reg.home_token_address,
                    subnet_id,
                ).map_err(|e| EasyTesterError::runtime(format!("db read failed: {e}")))?;

                // For test scenarios, U256 values fit in u64
                let val: u64 = balance.try_into().unwrap_or(u64::MAX);
                println!(
                    "OUTPUT read token_balance subnet='{}' token='{}': {}",
                    subnet_name, token_name, val
                );

                self.last_token_balance = Some(val);
                self.last_rootnet_msgs = None;
            }
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "ErcTransferTester does not support reading {:?}",
                    db
                )));
            }
        }
        Ok(())
    }

    fn exec_output_expect(
        &mut self,
        _height: u64,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<(), EasyTesterError> {
        // If last read was token_balance, handle simple value comparison
        if let Some(balance) = self.last_token_balance {
            let parts: Vec<&str> = target.path.split('.').collect();
            match parts.as_slice() {
                ["balance"] => {
                    let expected: u64 = expected_value.parse().map_err(|e| {
                        EasyTesterError::runtime(format!("balance must be numeric: {e}"))
                    })?;
                    if balance != expected {
                        return Err(EasyTesterError::runtime(format!(
                            "EXPECT failed: result.balance expected {}, got {}",
                            expected, balance
                        )));
                    }
                    println!("OUTPUT expect result.balance == {} (ok)", expected);
                    return Ok(());
                }
                _ => {
                    return Err(EasyTesterError::runtime(format!(
                        "after 'read token_balance', only 'result.balance' is supported, got 'result.{}'",
                        target.path
                    )));
                }
            }
        }

        // Otherwise, last read was rootnet_msgs
        let last = self.last_rootnet_msgs.as_ref().ok_or_else(|| {
            EasyTesterError::runtime("expect used but no previous 'read' command")
        })?;

        let parts: Vec<&str> = target.path.split('.').collect();
        match parts.as_slice() {
            ["count"] => {
                let expected: u64 = expected_value.parse().map_err(|e| {
                    EasyTesterError::runtime(format!("count must be numeric: {e}"))
                })?;
                let got = last.msgs.len() as u64;
                if got != expected {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed: result.count expected {}, got {}",
                        expected, got
                    )));
                }
                println!("OUTPUT expect result.count == {} (ok)", expected);
            }
            [index_str, field] => {
                let index: usize = index_str.parse().map_err(|e| {
                    EasyTesterError::runtime(format!("invalid index '{}': {}", index_str, e))
                })?;
                let msg = last.msgs.get(index).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "result[{}] out of range (have {} messages)",
                        index,
                        last.msgs.len()
                    ))
                })?;
                let got =
                    Self::msg_field_value(msg, field).map_err(|e| EasyTesterError::runtime(e))?;

                if got != expected_value {
                    return Err(EasyTesterError::runtime(format!(
                        "EXPECT failed: result.{}.{} expected '{}', got '{}'",
                        index, field, expected_value, got
                    )));
                }

                println!("OUTPUT expect result.{}.{} == {} (ok)", index, field, got);
            }
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "unsupported expect path 'result.{}' for ErcTransferTester",
                    target.path
                )));
            }
        }
        Ok(())
    }
}
