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
    ipc_lib::{IpcCrossSubnetErcTransfer, IpcErcTokenRegistration},
};

pub struct ErcTransferTester {
    base: BaseTester,
    /// Registered tokens by name → (home_subnet, registration).
    /// Populated by `register_token`, used by `erc_transfer` to look up the token address.
    registered_tokens: HashMap<String, (String, IpcErcTokenRegistration)>,
    /// Pending token registrations queued by `register_token`, consumed by next checkpoint
    pending_registrations: HashMap<String, Vec<IpcErcTokenRegistration>>,
    /// Pending ERC transfers queued by `erc_transfer`, consumed by next checkpoint
    pending_erc_transfers: HashMap<String, Vec<IpcCrossSubnetErcTransfer>>,
    /// Last read rootnet messages for expect
    last_rootnet_msgs: Option<LastRootnetMsgs>,
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
            pending_erc_transfers: HashMap::new(),
            last_rootnet_msgs: None,
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

        let etr = IpcErcTokenRegistration {
            home_token_address,
            name: name.to_string(),
            symbol: symbol.to_string(),
            decimals,
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
        let home_subnet_id = self.base.resolve_subnet_id(src_subnet)?;
        let destination_subnet_id = self.base.resolve_subnet_id(dst_subnet)?;

        let (reg_subnet, reg) = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "token '{}' not registered (use register_token first)",
                token_name
            ))
        })?;

        if reg_subnet != src_subnet {
            return Err(EasyTesterError::runtime(format!(
                "token '{}' was registered on subnet '{}', not '{}'",
                token_name, reg_subnet, src_subnet
            )));
        }

        // Parse amount as decimal → big-endian U256
        let amount_u64: u64 = amount_str
            .parse()
            .map_err(|e| EasyTesterError::runtime(format!("invalid amount '{amount_str}': {e}")))?;
        let mut amount = [0u8; 32];
        amount[24..32].copy_from_slice(&amount_u64.to_be_bytes());

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
                    // Parse big-endian U256 → decimal
                    let val = u64::from_be_bytes(etx.amount[24..32].try_into().unwrap());
                    Ok(val.to_string())
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
        let erc_transfers = self
            .pending_erc_transfers
            .remove(subnet_name)
            .unwrap_or_default();

        self.base
            .checkpoint_subnet(height, subnet_name, token_registrations, erc_transfers)?;
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
            }
            _ => {
                return Err(EasyTesterError::runtime(format!(
                    "ErcTransferTester only supports reading rootnet_msgs, got {:?}",
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
        let last = self.last_rootnet_msgs.as_ref().ok_or_else(|| {
            EasyTesterError::runtime("expect used but no previous 'read rootnet_msgs'")
        })?;

        let parts: Vec<&str> = target.path.split('.').collect();
        match parts.as_slice() {
            ["count"] => {
                let expected: u64 = expected_value
                    .parse()
                    .map_err(|e| EasyTesterError::runtime(format!("count must be numeric: {e}")))?;
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

                // Compare as string — works for both numeric and string fields
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
