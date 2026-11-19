//! Bounded failover collection on the existing computer transport. Provider implementations
//! remain in the package's fixed Python bundle during the handoff.
use aac_config::{OperatingSystem, TransportConfig};
use aac_transport::{CallOptions, Computer};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path, sync::OnceLock, time::Duration};
use tokio::time::Instant;

const CAP: usize = 2 * 1024 * 1024;
const BOOTSTRAP: &str = "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));sys.stdin=io.StringIO(json.dumps(_p['request']));exec(compile(_p['helperSource'],'managed-usage','exec'))";
macro_rules! module {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../../../scripts/account-usage/", $name, ".py")),
        )
    };
}

fn source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let modules = serde_json::Map::from_iter(
            [
                module!("failover_usage"),
                module!("failover_core"),
                module!("failover_additional"),
                module!("failover_special"),
                module!("desktop_helpers"),
                module!("desktop_usage"),
                module!("cursor_usage"),
                module!("plan_common"),
                module!("plan_usage"),
                module!("muse_console"),
                module!("muse_native"),
                module!("claude_usage"),
                (
                    "opencode_console",
                    include_str!(
                        "../../../../browser-bridge/opencode-muse/native-host/opencode_console.py"
                    ),
                ),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), Value::String(value.to_owned()))),
        );
        let encoded = Value::String(Value::Object(modules).to_string());
        [
            "import sys,json,importlib.abc,importlib.util".to_owned(),
            format!("_sources=json.loads({encoded})"),
            r#"class _Bundle(importlib.abc.MetaPathFinder,importlib.abc.Loader):
 def find_spec(self,fullname,path=None,target=None):
  return importlib.util.spec_from_loader(fullname,self) if fullname in _sources else None
 def create_module(self,spec): return None
 def exec_module(self,module):
  module.__file__='managed-usage/'+module.__name__+'.py'
  exec(compile(_sources[module.__name__],module.__file__,'exec'),module.__dict__)
sys.meta_path.insert(0,_Bundle())
import failover_usage
print(json.dumps(failover_usage.run(json.load(sys.stdin)),allow_nan=False))"#
                .into(),
        ]
        .join("\n")
    })
}

fn argv(computer: &Computer, root: &Path, args: &Value) -> Result<Vec<OsString>, &'static str> {
    let local = matches!(computer.config().transport, TransportConfig::Local);
    let ccs_dir = &args["ccsDir"];
    if !args["homeDir"].is_null()
        || (!ccs_dir.is_null()
            && (!local || ccs_dir.as_str().is_none_or(|path| Path::new(path) != root)))
    {
        return Err("unsupported");
    }
    let scoped = ccs_dir.as_str().unwrap_or("");
    if computer.config().os == OperatingSystem::Windows {
        let source = format!(
            r#"$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'; $env:PYTHONIOENCODING = 'utf-8'; $env:PYTHONUTF8 = '1'; $venv = [IO.Path]::Combine($HOME, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe'); $python = if (Test-Path -LiteralPath $venv -PathType Leaf) {{ $venv }} else {{ (Get-Command python3.exe,python.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source }}; if (-not $python) {{ exit 1 }}; & $python -c '{}'; exit $LASTEXITCODE"#,
            BOOTSTRAP.replace('\'', "''")
        );
        let text: Vec<_> = source.encode_utf16().flat_map(u16::to_le_bytes).collect();
        return Ok([
            "powershell.exe".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            STANDARD.encode(text).into(),
        ]
        .into());
    }
    let script = r#"export PYTHONIOENCODING=utf-8 PYTHONUTF8=1
ccs="$1"
if [ -z "$ccs" ]; then ccs="$HOME/.ccs"; fi
venv="$ccs/claude-session-migration/venv/bin/python3"
if [ -x "$venv" ]; then exec "$venv" -c "$2"; fi
exec /usr/bin/python3 -c "$2""#;
    Ok(["/bin/sh", "-c", script, "aac-failover", scoped, BOOTSTRAP]
        .into_iter()
        .map(Into::into)
        .collect())
}

pub(crate) async fn collect(
    computer: &Computer,
    root: &Path,
    args: &Value,
    deadline: Instant,
    cap: usize,
) -> Result<Value, &'static str> {
    let argv = argv(computer, root, args)?;
    let input = serde_json::to_vec(&json!({"helperSource": source(), "request": args}))
        .map_err(|_| "shape")?;
    if input.len() > CAP {
        return Err("shape");
    }
    let seconds = if args["action"] == "inventory" {
        12
    } else {
        35
    };
    let mut options = CallOptions::new(Duration::from_secs(seconds), cap);
    options.deadline = options.deadline.min(deadline);
    let result = computer
        .execute(&argv, &input, &options)
        .await
        .map_err(|_| "transport")?;
    if result.code != Some(0) {
        return Err("transport");
    }
    let value: Value = serde_json::from_slice(&result.stdout).map_err(|_| "reply")?;
    if value["version"] != 1 || value["action"] != args["action"] {
        return Err("reply");
    }
    Ok(value)
}
