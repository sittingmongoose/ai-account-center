//! The established fixed host programs, used only after a definite agent decline.
use crate::{
    agent_transport::{Agents, MutationReply, Selector},
    reset_source_route::{SourceContext, claude_host_ssh},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{future::Future, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAC_COMMAND: &str = r#"/usr/bin/python3 -c "import sys,json;_d=json.loads(sys.stdin.read());exec(compile(_d['program'],'aac-claude-host','exec'),{'REQUEST':_d['request'],'__name__':'__main__'})""#;
const WINDOWS_BOOTSTRAP: &str = "$data = [Console]::In.ReadLine(); $request = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($data)) | ConvertFrom-Json; $s = [Console]::In.ReadToEnd(); & ([scriptblock]::Create($s)); exit $LASTEXITCODE";
const REPLY_CAP: u64 = 64 * 1024;

pub(crate) async fn call(
    agents: &Arc<Agents>,
    host: &str,
    alias: &str,
    args: Value,
    deadline_ms: u64,
) -> Option<Value> {
    let outcome = agents
        .call_mutation(
            "claude-host",
            &Selector::usage(host, Some(alias)),
            args.clone(),
            deadline_ms,
        )
        .await;
    after_dispatch(outcome, || {
        reference(agents, host, alias, args, deadline_ms)
    })
    .await
}

async fn after_dispatch<F: Future<Output = Option<Value>>>(
    outcome: MutationReply,
    reference: impl FnOnce() -> F,
) -> Option<Value> {
    match outcome {
        MutationReply::Result(receipt) => Some(receipt),
        MutationReply::NotDispatched => reference().await,
        MutationReply::Uncertain => None,
    }
}

fn request(host: &str, args: &Value) -> Option<Value> {
    let op = args["op"].as_str()?;
    if !matches!(
        op,
        "create" | "undo" | "state" | "session" | "trash" | "restore" | "purge"
    ) || args["host"] != host
    {
        return None;
    }
    if op == "purge" {
        aac_core::claude_purge::verify(
            args["token"].as_str(),
            host,
            args["trashName"].as_str()?,
            aac_core::UtcMs::now().ok()?.get(),
        )
        .ok()?;
    }
    let mut fields = serde_json::Map::new();
    fields.insert("op".into(), json!(op));
    for name in ["profileId", "trashName"] {
        if let Some(value) = args[name].as_str() {
            fields.insert(name.into(), json!(value));
        }
    }
    for name in [
        "profilePath",
        "launcherName",
        "launcherPath",
        "startMenuPath",
    ] {
        if let Some(value) = args["launcher"][name].as_str() {
            fields.insert(name.into(), json!(value));
        }
    }
    Some(Value::Object(fields))
}

fn invocation(host: &str, args: &Value) -> Option<(String, Vec<u8>)> {
    let request = request(host, args)?;
    match host {
        "mac" => Some((
            MAC_COMMAND.to_owned(),
            serde_json::to_vec(
                &json!({"program":include_str!("claude_host/mac.py"),"request":request}),
            )
            .ok()?,
        )),
        "windows" => {
            let encoded = STANDARD.encode(
                WINDOWS_BOOTSTRAP
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            let input = format!(
                "{}\n{}",
                STANDARD.encode(serde_json::to_vec(&request).ok()?),
                include_str!("claude_host/windows.ps1")
            );
            (input.len() <= 16 * 1024).then(|| {
                (
                    format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}"),
                    input.into_bytes(),
                )
            })
        }
        _ => None,
    }
}

async fn reference(
    agents: &Agents,
    host: &str,
    alias: &str,
    args: Value,
    deadline_ms: u64,
) -> Option<Value> {
    let _permit = agents.permit()?;
    let (program, input) = invocation(host, &args)?;
    let ctx = SourceContext::from_env(agents.root(), None);
    let alias = alias.to_owned();
    let command = tokio::task::spawn_blocking(move || claude_host_ssh(&ctx, &alias, &program))
        .await
        .ok()?
        .ok()?;
    let mut child = tokio::process::Command::from(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?.take(REPLY_CAP + 1);
    let write = async move {
        stdin.write_all(&input).await?;
        stdin.shutdown().await
    };
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_millis(deadline_ms), async {
        let (_, count) = tokio::try_join!(write, stdout.read_to_end(&mut reply)).ok()?;
        if count as u64 > REPLY_CAP || !child.wait().await.ok()?.success() {
            return None;
        }
        serde_json::from_slice::<Value>(&reply)
            .ok()
            .filter(Value::is_object)
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_indeterminate_dispatch_never_replays_and_definite_declines_do() {
        let should_not_run = || async { panic!("mutating host operation replayed") };
        assert_eq!(
            after_dispatch(MutationReply::Uncertain, should_not_run).await,
            None
        );
        let receipt = json!({"ok":true});
        assert_eq!(
            after_dispatch(MutationReply::Result(receipt.clone()), should_not_run).await,
            Some(receipt.clone()),
        );
        assert_eq!(
            after_dispatch(MutationReply::NotDispatched, || async {
                Some(receipt.clone())
            })
            .await,
            Some(receipt),
        );
    }
}
