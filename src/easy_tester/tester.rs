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

    fn exec_checkpoint_subnet(&mut self, height: u64, subnet_name: &str) -> Result<(), EasyTesterError>;

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
        expected_sats: u64,
    ) -> Result<(), EasyTesterError>;
}

