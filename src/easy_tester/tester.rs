use crate::easy_tester::{model::OutputExpectTarget, EasyTesterError, OutputDb};

pub trait Tester {
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
        _decimals: u8,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "register_token is not supported by this tester",
        ))
    }

    fn exec_erc_transfer(
        &mut self,
        _height: u64,
        _src_subnet: &str,
        _dst_subnet: &str,
        _token_name: &str,
        _amount: &str,
    ) -> Result<(), EasyTesterError> {
        Err(EasyTesterError::runtime(
            "erc_transfer is not supported by this tester",
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
    ) -> Result<(), EasyTesterError>;
}
