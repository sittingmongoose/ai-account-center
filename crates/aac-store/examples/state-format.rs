//! Offline rollback of this branch's account-state format. Close the dashboard first and
//! preserve the entire store directory. Newer schema versions are refused by Engine::open.
use aac_store::{ops, usage};

fn main() -> aac_store::error::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 || args[1] != "--rollback-v1" {
        return Err(aac_store::error::Error::new(
            "invalid",
            "usage: state-format --rollback-v1 <offline-store-directory>",
        ));
    }
    let engine = ops::open_engine(&args[2], None, None)?;
    println!("{}", usage::migrate_account_states(&engine, true)?);
    engine.close()?;
    Ok(())
}
