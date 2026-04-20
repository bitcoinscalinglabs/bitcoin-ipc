use crate::easy_tester::{model::OutputExpectTarget, EasyTesterError, OutputDb};

pub trait Tester {
    /// The number of blocks already mined before the scenario starts.
    /// `run_scenario` initialises `mined_height` to this value so it won't
    /// try to re-mine blocks that the tester has already set up.
    fn starting_block(&self) -> u64 {
        0
    }

    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError>;

    fn exec_create_subnet(&mut self, height: u64, subnet_name: &str) -> Result<(), EasyTesterError>;

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError>;

    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError>;

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError>;

    fn exec_checkpoint_subnet(&mut self, height: u64, subnet_name: &str) -> Result<(), EasyTesterError>;

    fn exec_register_token(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _name: &str,
        _symbol: &str,
        _initial_supply: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "register_token is not supported by this tester",
        ))
    }

    fn exec_mint_token(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _token_name: &str,
        _amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "mint_token is not supported by this tester",
        ))
    }

    fn exec_burn_token(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _token_name: &str,
        _amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "burn_token is not supported by this tester",
        ))
    }

    fn exec_erc_transfer(
        &mut self,
        _height: u64,
        _src_subnet: &str,
        _dst_subnet: &str,
        _token_name: &str,
        _amount: alloy_primitives::U256,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "erc_transfer is not supported by this tester",
        ))
    }

    fn exec_deposit(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _address_name: &str,
        _amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "deposit is not supported by this tester",
        ))
    }

    fn exec_output_read(
        &mut self,
        height: u64,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError>;

    fn exec_output_expect(
        &mut self,
        height: u64,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<String, EasyTesterError>;
}
