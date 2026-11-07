use crate::bitcoin_utils::RevealShape;
use crate::easy_tester::{
    model::{JoinUeSpec, OutputExpectTarget},
    EasyTesterError, OutputDb,
};

pub trait Tester {
    /// The number of blocks already mined before the scenario starts.
    /// `run_scenario` initialises `mined_height` to this value so it won't
    /// try to re-mine blocks that the tester has already set up.
    fn starting_block(&self) -> u64 {
        0
    }

    fn exec_mine_block(&mut self, height: u64) -> Result<(), EasyTesterError>;

    /// `shape` rebuilds the create reveal; a rebuilt reveal still names the subnet
    fn exec_create_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        shape: RevealShape,
    ) -> Result<(), EasyTesterError>;

    fn exec_join_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        collateral_sats: u64,
    ) -> Result<(), EasyTesterError>;

    /// The joins of one `join-ue` line go in one reveal transaction, in order, rebuilt in `shape`
    fn exec_join_ue(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _joins: &[JoinUeSpec],
        _shape: RevealShape,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "join-ue is not supported by this tester",
        ))
    }

    /// `raw`: hand-built and submitted without the provider
    fn exec_stake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
        raw: bool,
    ) -> Result<(), EasyTesterError>;

    fn exec_unstake_subnet(
        &mut self,
        height: u64,
        subnet_name: &str,
        validator_name: &str,
        amount_sats: u64,
        raw: bool,
    ) -> Result<(), EasyTesterError>;

    fn exec_unstake_ue(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _validator_name: &str,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "unstake-ue is not supported by this tester",
        ))
    }

    /// One checkpoint per subnet; their batch transfers go in one reveal, rebuilt in `shape`
    fn exec_presign(
        &mut self,
        _height: u64,
        _subnet_name: &str,
        _spec: super::model::PresignSpec,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "presign is not supported by this tester",
        ))
    }

    fn exec_checkpoint(
        &mut self,
        height: u64,
        subnets: &[String],
        shape: RevealShape,
    ) -> Result<(), EasyTesterError>;

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
