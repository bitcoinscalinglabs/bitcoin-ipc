//! Read-only vote snapshots for ue_scenario.sh, which runs this test executable as
//! /tmp/spec-eots-sign-record (the caller builds it, README.md); no key file or provider
//! write API is used.
use bitcoin_ipc::eots::EotsVote;
use heed::types::{SerdeBincode, Str};
use heed::{Database, EnvFlags, EnvOpenOptions};

// slashing.md, EOTS key and EOTS nonce commitments: snapshot every complete signed vote for restart comparisons.
#[test]
#[ignore = "invoked by ue_scenario.sh with explicit database and output paths"]
fn dump_sign_record() {
    let path = std::env::var_os("EOTS_SIGN_RECORD_DB").expect("EOTS_SIGN_RECORD_DB");
    let output = std::env::var_os("EOTS_SIGN_RECORD_OUTPUT").expect("EOTS_SIGN_RECORD_OUTPUT");
    let env = unsafe {
        EnvOpenOptions::new()
            .max_dbs(1)
            .flags(EnvFlags::READ_ONLY)
            .open(path)
            .unwrap()
    };
    let txn = env.read_txn().unwrap();
    let votes: Database<Str, SerdeBincode<EotsVote>> = env
        .open_database(&txn, Some("votes"))
        .unwrap()
        .expect("votes database");
    let mut records = Vec::new();
    for item in votes.iter(&txn).unwrap() {
        let (key, vote) = item.unwrap();
        let (subnet, height) = key.rsplit_once(':').expect("subnet:height database key");
        assert_eq!(
            height.parse::<u64>().unwrap(),
            vote.height,
            "database key and vote disagree"
        );
        records.push(serde_json::json!({"subnet_id": subnet, "vote": vote}));
    }
    std::fs::write(output, serde_json::to_vec(&records).unwrap()).unwrap();
}
