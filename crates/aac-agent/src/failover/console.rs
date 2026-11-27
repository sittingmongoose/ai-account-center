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
        if crate::failover_additional::handles(&c.provider) {
            self.additional
                .inventory_for(&c.provider, c.platform, &c.home, &c.ccs)
        } else if crate::failover_special::handles(&c.provider) {
            self.special
                .inventory_for(&c.provider, c.platform, &c.home, &c.ccs)
        } else {
            Err(())
        }
    }
    async fn collect(&mut self, candidate: &Value, c: &Context) -> Value {
        if crate::failover_additional::handles(&c.provider) {
            self.additional
                .collect_for(candidate, c.platform, &c.home, &c.ccs)
                .await
        } else if crate::failover_special::handles(&c.provider) {
            self.special
                .collect_for(candidate, c.platform, &c.home, &c.ccs)
                .await
        } else {
            super::dispatch::failure("unavailable", &Value::Null, &Value::Null)
        }
    }
}
