//! Claude profile operations run through the authenticated native agent transport.
use crate::agent_transport::{Agents, MutationReply, Selector};
use serde_json::Value;
use std::sync::Arc;

pub(crate) async fn call(
    agents: &Arc<Agents>,
    host: &str,
    alias: &str,
    args: Value,
    deadline_ms: u64,
) -> Option<Value> {
    receipt(
        agents
            .call_mutation(
                "claude-host",
                &Selector::usage(host, Some(alias)),
                args,
                deadline_ms,
            )
            .await,
    )
}

fn receipt(outcome: MutationReply) -> Option<Value> {
    match outcome {
        MutationReply::Result(value) => Some(value),
        // An unavailable or declined native host stays unavailable. An uncertain
        // mutation must never replay through another program.
        MutationReply::NotDispatched | MutationReply::Uncertain => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_declined_native_operation_never_runs_the_packaged_program() {
        assert_eq!(receipt(MutationReply::NotDispatched), None);
    }

    #[test]
    fn native_receipts_survive_and_uncertain_mutations_never_replay() {
        let value = json!({"ok":true});
        assert_eq!(receipt(MutationReply::Result(value.clone())), Some(value));
        assert_eq!(receipt(MutationReply::Uncertain), None);
    }
}
