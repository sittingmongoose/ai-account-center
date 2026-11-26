//! Connect the U3 Muse/OpenCode parsers to U2's public boundary and race checks.
//! Private candidates stay inside these adapters; `dispatch` reconstructs every public reply.
use super::{Adapter, Context};
use crate::{failover_additional::AdditionalAdapter, failover_special::SpecialAdapter};
use serde_json::Value;

pub(super) struct Console {
    additional: AdditionalAdapter,
    special: SpecialAdapter,
}
impl Console {
    pub(super) fn new() -> Self {
        Self {
            additional: AdditionalAdapter::new(),
            special: SpecialAdapter::new(),
        }
    }
}
impl Adapter for Console {
    fn inventory(&mut self, c: &Context) -> Result<Value, ()> {
        if c.claude_profile.is_some() {
            return Err(());
        }
        match c.provider.as_str() {
            "muse" => self
                .additional
                .inventory_for(&c.provider, c.platform, &c.home, &c.ccs),
            "opencode-wallet" => {
                self.special
                    .inventory_for(&c.provider, c.platform, &c.home, &c.ccs)
            }
            _ => Err(()),
        }
    }
    async fn collect(&mut self, candidate: &Value, c: &Context) -> Value {
        match c.provider.as_str() {
            "muse" => {
                self.additional
                    .collect_for(candidate, c.platform, &c.home, &c.ccs)
                    .await
            }
            "opencode-wallet" => {
                self.special
                    .collect_for(candidate, c.platform, &c.home, &c.ccs)
                    .await
            }
            _ => super::dispatch::failure("unavailable", &Value::Null, &Value::Null),
        }
    }
}

#[cfg(test)]
mod tests;
