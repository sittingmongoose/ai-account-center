use aac_status::reset::{Outcome, Result, Snapshot, Store};
use serde_json::Value;

#[allow(
    dead_code,
    reason = "Each integration suite uses its own subset of these read boundaries"
)]
pub trait ReadFixtures {
    fn read_finish(
        &mut self,
        account: &str,
        attempt: &str,
        outcome: Outcome,
        after: Option<&Snapshot>,
        now: i64,
        retry: Option<i64>,
    ) -> Result<Value>;
    fn read_check(&mut self, account: &str, attempt: &str, after: &Snapshot, now: i64) -> Result<Value>;
    fn read_inventory(&mut self, account: &str, snapshot: &Snapshot, now: i64, entropy: &str) -> Result<Value>;
}
impl ReadFixtures for Store {
    fn read_finish(
        &mut self,
        account: &str,
        attempt: &str,
        outcome: Outcome,
        after: Option<&Snapshot>,
        now: i64,
        retry: Option<i64>,
    ) -> Result<Value> {
        let mut fresh = after.cloned();
        if let Some(fresh) = &mut fresh {
            fresh.read_ticket = Some(self.read_ticket(account)?);
        }
        self.finish(account, attempt, outcome, fresh.as_ref(), now, retry)
    }
    fn read_check(&mut self, account: &str, attempt: &str, after: &Snapshot, now: i64) -> Result<Value> {
        let mut fresh = after.clone();
        fresh.read_ticket = Some(self.read_ticket(account)?);
        self.check(account, attempt, &fresh, now)
    }
    fn read_inventory(&mut self, account: &str, snapshot: &Snapshot, now: i64, entropy: &str) -> Result<Value> {
        let mut fresh = snapshot.clone();
        fresh.read_ticket = Some(self.read_ticket(account)?);
        self.inventory(account, &fresh, now, entropy)
    }
}
