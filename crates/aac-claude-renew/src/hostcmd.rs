//! The fixed commands run on each computer: read every profile's sign-in with
//! `claude auth status --json`, and run `claude auth login --claudeai` for one profile. On the Mac
//! both run inside the desktop session ([`desktop`]), the only place the Keychain opens.
//!
//! `claude auth login` reads the authorization code from a plain pipe on every system, so no
//! terminal is needed; the link it prints and the code it reads stay in memory.

use crate::config::{Host, Os};
use crate::plan::{Reading, SignIn};
use crate::remote::{Script, sh_quote};

/// A config folder path that is safe inside a `cmd` line or an `sh` word.
pub fn safe_dir(dir: &str) -> bool {
    !dir.is_empty()
        && dir.len() <= 300
        && !dir
            .chars()
            .any(|c| c.is_control() || "\"'`$%&|^<>!;*?()[]{}\n\r".contains(c))
}

/// A `claude` path that is safe inside a `cmd` line or an `sh` word.
pub fn safe_claude(path: &str) -> bool {
    safe_dir(path)
}

const POSIX_FIND_CLAUDE: &str = r#"C=$(command -v claude 2>/dev/null)
for p in "$HOME/.local/bin/claude" /opt/homebrew/bin/claude /usr/local/bin/claude "$HOME/.claude/local/claude"; do
  if [ -z "$C" ] && [ -x "$p" ]; then C=$p; fi
done
"#;

/// The script that prints one line per profile: `AACROW<TAB>name<TAB>dir<TAB>default<TAB>json`,
/// plus `AACCLAUDE<TAB>path` (or `AACNOCLAUDE`).
pub fn scan(host: &Host, ignore: &[String]) -> Script {
    let ignore_words: Vec<&str> = ignore
        .iter()
        .map(String::as_str)
        .filter(|n| crate::config::safe_name(n))
        .collect();
    match host.os {
        Os::Windows => {
            let configured = host
                .claude
                .as_deref()
                .filter(|c| safe_claude(c))
                .map(|c| format!("$c = '{c}'\n"))
                .unwrap_or_default();
            let list = ignore_words
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(",");
            Script::Pwsh(format!(
                r#"$ErrorActionPreference = 'SilentlyContinue'
$c = $null
{configured}if (-not $c) {{ $c = (Get-Command claude -ErrorAction SilentlyContinue).Source }}
if (-not $c) {{ $p = Join-Path $env:USERPROFILE '.local\bin\claude.exe'; if (Test-Path $p) {{ $c = $p }} }}
if (-not $c) {{ 'AACNOCLAUDE'; exit 0 }}
"AACCLAUDE`t$c"
Remove-Item Env:CLAUDE_CODE_OAUTH_TOKEN, Env:ANTHROPIC_API_KEY -ErrorAction SilentlyContinue
$ignore = @({list})
function Row($n, $d, $def) {{
  if ($def) {{ Remove-Item Env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue }} else {{ $env:CLAUDE_CONFIG_DIR = $d }}
  $s = ((& $c auth status --json 2>$null) -join '') -replace "[`r`n`t]", ''
  "AACROW`t$n`t$d`t$([int]$def)`t$s"
}}
$base = Join-Path $env:USERPROFILE '.claude-t3'
if (Test-Path $base) {{
  Get-ChildItem $base -Directory | Sort-Object Name | ForEach-Object {{
    if ((Test-Path (Join-Path $_.FullName '.claude.json')) -and ($ignore -notcontains $_.Name) -and -not $_.Name.StartsWith('.')) {{ Row $_.Name $_.FullName $false }}
  }}
}}
$def = Join-Path $env:USERPROFILE '.claude'
if (Test-Path $def) {{ Row 'default' $def $true }}
"#
            ))
        }
        Os::Linux | Os::Mac => {
            let configured = host
                .claude
                .as_deref()
                .filter(|c| safe_claude(c))
                .map(|c| format!("C={}\n", sh_quote(c)))
                .unwrap_or_else(|| POSIX_FIND_CLAUDE.to_owned());
            let inner = format!(
                r#"{configured}if [ -z "$C" ]; then echo AACNOCLAUDE; exit 0; fi
printf 'AACCLAUDE\t%s\n' "$C"
IGNORE=' {ignore} '
row() {{
  if [ "$3" = 1 ]; then
    s=$(env -u CLAUDE_CONFIG_DIR -u CLAUDE_CODE_OAUTH_TOKEN -u ANTHROPIC_API_KEY "$C" auth status --json 2>/dev/null | tr -d '\r\n\t')
  else
    s=$(env -u CLAUDE_CODE_OAUTH_TOKEN -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR="$2" "$C" auth status --json 2>/dev/null | tr -d '\r\n\t')
  fi
  printf 'AACROW\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$s"
}}
if [ -d "$HOME/.claude-t3" ]; then
  for d in "$HOME"/.claude-t3/*; do
    n=${{d##*/}}
    [ -d "$d" ] && [ -f "$d/.claude.json" ] || continue
    case "$IGNORE" in *" $n "*) continue ;; esac
    row "$n" "$d" 0
  done
fi
if [ -d "$HOME/.claude" ]; then row default "$HOME/.claude" 1; fi
"#,
                ignore = ignore_words.join(" ")
            );
            if host.os == Os::Mac {
                desktop(&inner, 600)
            } else {
                Script::Sh(inner)
            }
        }
    }
}

/// The script that runs `claude auth login --claudeai` for one profile. Its output carries the
/// link; its input takes the code. `None` when the folder or the `claude` path is unsafe.
pub fn login(host: &Host, claude: &str, dir: &str, default: bool) -> Option<Script> {
    if !safe_claude(claude) || !safe_dir(dir) {
        return None;
    }
    match host.os {
        Os::Windows => {
            // No space before &&: cmd keeps it in the value and Claude saves to a look-alike folder.
            let env = if default {
                "set CLAUDE_CONFIG_DIR=".to_owned()
            } else {
                format!("set \"CLAUDE_CONFIG_DIR={dir}\"")
            };
            Some(Script::Cmd(format!(
                "{env}&& set BROWSER=none&& set CLAUDE_CODE_OAUTH_TOKEN=&& set ANTHROPIC_API_KEY=&& \"{claude}\" auth login --claudeai"
            )))
        }
        Os::Linux | Os::Mac => {
            let env = if default {
                "-u CLAUDE_CONFIG_DIR".to_owned()
            } else {
                format!("CLAUDE_CONFIG_DIR={}", sh_quote(dir))
            };
            let inner = format!(
                "exec env -u CLAUDE_CODE_OAUTH_TOKEN -u ANTHROPIC_API_KEY {env} BROWSER=true {} auth login --claudeai 2>&1\n",
                sh_quote(claude)
            );
            Some(if host.os == Os::Mac {
                desktop(&inner, 900)
            } else {
                Script::Sh(inner)
            })
        }
    }
}

/// Wraps a script so it runs inside the Mac's desktop session: a one-shot launchd job whose
/// input and output are connected to this ssh session through two private FIFOs. Over plain ssh
/// the Keychain is closed, so `claude` there sees no sign-in and could not save a new one.
pub fn desktop(inner: &str, timeout_secs: u32) -> Script {
    desktop_as(inner, timeout_secs, Parent::Sh)
}

/// The program a desktop-session job starts under. macOS grants Automation (driving Mail) to the
/// job's own program, and the grant the earlier runner left is `/usr/bin/python3`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    /// `/bin/sh` runs the script.
    Sh,
    /// `/usr/bin/python3` starts `/bin/sh` and waits for it, so it stays the job's program.
    Python3,
}

/// [`desktop`] with a chosen job program.
pub fn desktop_as(inner: &str, timeout_secs: u32, parent: Parent) -> Script {
    let program = match parent {
        Parent::Sh => {
            r#"<string>/bin/sh</string><string>-c</string><string>exec /bin/sh "\$0/job.sh" &lt;"\$0/in" &gt;"\$0/out" 2&gt;&amp;1</string>"#
        }
        Parent::Python3 => {
            r#"<string>/usr/bin/python3</string><string>-c</string><string>import subprocess, sys; d = sys.argv[1]; sys.exit(subprocess.call(["/bin/sh", d + "/job.sh"], stdin=open(d + "/in"), stdout=open(d + "/out", "w"), stderr=subprocess.STDOUT))</string>"#
        }
    };
    Script::Sh(format!(
        r#"set -u
umask 077
base="$HOME/.ccs/claude-renew/run"
mkdir -p "$base" && chmod 700 "$base" || exit 90
find "$base" -maxdepth 1 -name 'job.*' -mmin +120 -exec rm -rf {{}} + 2>/dev/null
d=$(mktemp -d "$base/job.XXXXXX") || exit 90
label="party.sittingmongoose.aac.claude-renew.${{d##*.}}"
uid=$(id -u)
cleanup() {{ launchctl bootout "gui/$uid/$label" >/dev/null 2>&1; rm -rf "$d"; }}
trap cleanup EXIT
trap 'exit 92' INT TERM HUP
mkfifo "$d/in" "$d/out" || exit 90
cat > "$d/job.sh" <<'AAC_JOB_EOF'
{inner}
AAC_JOB_EOF
cat > "$d/job.plist" <<AAC_PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array>{program}<string>$d</string></array>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
AAC_PLIST_EOF
exec 3<&0
cat <&3 > "$d/in" &
writer=$!
launchctl bootstrap "gui/$uid" "$d/job.plist" >/dev/null 2>&1 || {{ kill $writer 2>/dev/null; exit 91; }}
cat "$d/out" &
reader=$!
( sleep {timeout_secs}; kill $reader 2>/dev/null ) </dev/null >/dev/null 2>&1 &
dog=$!
wait $reader
pkill -P $dog 2>/dev/null
kill $dog $writer 2>/dev/null
exit 0
"#
    ))
}

/// What a scan found on one computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The `claude` path there, if found.
    pub claude: Option<String>,
    /// One reading per profile.
    pub readings: Vec<Reading>,
}

/// Parses an ISO-8601 UTC time (`2026-12-01T18:00:59.507Z`) to milliseconds.
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, mo, d) = (num(&text[0..4])?, num(&text[5..7])?, num(&text[8..10])?);
    let (h, mi, s) = (
        num(&text[11..13])?,
        num(&text[14..16])?,
        num(&text[17..19])?,
    );
    let rest = &text[19..];
    let (frac, zone) = match rest.strip_prefix('.') {
        Some(r) => {
            let digits: String = r.chars().take_while(char::is_ascii_digit).collect();
            let ms = format!("{digits:0<3}");
            (num(&ms[..3])?, &r[digits.len()..])
        }
        None => (0, rest),
    };
    if zone != "Z"
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || h > 23
        || mi > 59
        || s > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000 + frac)
}

/// Formats milliseconds as ISO-8601 UTC with milliseconds.
pub fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

/// Turns one `claude auth status --json` answer into a state, email and end.
pub fn status_reading(json: &str) -> (SignIn, Option<String>, Option<i64>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return (SignIn::NotSignedIn, None, None);
    };
    let email = value["email"]
        .as_str()
        .map(|e| e.trim().to_lowercase())
        .filter(|e| crate::config::safe_email(e));
    let ends = value["loginExpiresAt"].as_str().and_then(parse_iso_ms);
    let logged_in = value["loggedIn"].as_bool() == Some(true);
    let claude_ai = value["authMethod"]
        .as_str()
        .is_none_or(|m| m == "claude.ai");
    let state = match (logged_in, claude_ai, ends) {
        (true, true, Some(_)) => SignIn::Ok,
        (true, _, _) => SignIn::NoEndDate,
        // Signed out but the account is still recorded: an ended or cleared sign-in.
        (false, _, _) if email.is_some() => SignIn::LoggedOut,
        (false, _, _) => SignIn::NotSignedIn,
    };
    (state, email, ends)
}

/// Parses a scan's output for one computer. A default folder that was never signed in is left
/// out: there is nothing to keep alive.
pub fn parse_scan(host: &str, stdout: &str) -> Scan {
    let mut scan = Scan::default();
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(path) = line.strip_prefix("AACCLAUDE\t") {
            if safe_claude(path.trim()) {
                scan.claude = Some(path.trim().to_owned());
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("AACROW\t") else {
            continue;
        };
        let mut parts = rest.splitn(4, '\t');
        let (Some(name), Some(dir), Some(default), json) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !crate::config::safe_name(name) || !safe_dir(dir) {
            continue;
        }
        let default = default == "1";
        let (state, email, ends_at) = status_reading(json.unwrap_or(""));
        if default && state == SignIn::NotSignedIn {
            continue;
        }
        scan.readings.push(Reading {
            host: host.to_owned(),
            profile: name.to_owned(),
            dir: dir.to_owned(),
            default,
            email,
            state,
            ends_at,
        });
    }
    scan
}

/// The authorize link in the login prompt's output.
pub fn login_url(text: &str) -> Option<String> {
    static URL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"https://claude\.(?:com|ai)/\S*oauth/authorize\?\S+")
            .expect("static regex")
    });
    URL.find(text).map(|m| m.as_str().to_owned())
}

/// Whether the login prompt reported success.
pub fn login_succeeded(text: &str, code: Option<i32>) -> bool {
    let lower = text.to_lowercase();
    code == Some(0)
        && (lower.contains("login successful")
            || lower.contains("logged in")
            || lower.contains("success"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(os: Os) -> Host {
        Host {
            name: "h".into(),
            os,
            ssh: Some("h".into()),
            claude: None,
        }
    }

    #[test]
    fn iso_round_trip() {
        let ms = parse_iso_ms("2026-12-01T18:00:59.507Z").expect("parse");
        assert_eq!(iso(ms), "2026-12-01T18:00:59.507Z");
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2024-02-29T12:00:00.5Z").map(iso).as_deref(),
            Some("2024-02-29T12:00:00.500Z")
        );
        assert_eq!(parse_iso_ms("2026-12-01 18:00:59Z"), None);
        assert_eq!(parse_iso_ms("2026-12-01T18:00:59+01:00"), None);
    }

    #[test]
    fn status_answers_map_to_states() {
        let ok = r#"{"loggedIn":true,"authMethod":"claude.ai","loginExpiresAt":"2026-12-01T18:00:59.507Z","email":"A@B.com"}"#;
        let (state, email, ends) = status_reading(ok);
        assert_eq!(state, SignIn::Ok);
        assert_eq!(email.as_deref(), Some("a@b.com"));
        assert_eq!(ends, parse_iso_ms("2026-12-01T18:00:59.507Z"));
        assert_eq!(
            status_reading(r#"{"loggedIn":false,"email":"a@b.com"}"#).0,
            SignIn::LoggedOut
        );
        assert_eq!(
            status_reading(r#"{"loggedIn":false}"#).0,
            SignIn::NotSignedIn
        );
        assert_eq!(
            status_reading(r#"{"loggedIn":true,"authMethod":"oauth_token"}"#).0,
            SignIn::NoEndDate
        );
        assert_eq!(status_reading("").0, SignIn::NotSignedIn);
    }

    #[test]
    fn scan_output_parses_and_skips_unsafe_or_unsigned_defaults() {
        let out = concat!(
            "AACCLAUDE\t/home/u/.local/bin/claude\n",
            "AACROW\tparty\t/home/u/.claude-t3/party\t0\t{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"loginExpiresAt\":\"2026-12-24T19:22:26.000Z\",\"email\":\"p@x.party\"}\r\n",
            "AACROW\tdefault\t/home/u/.claude\t1\t{\"loggedIn\":false}\n",
            "AACROW\tbad;name\t/home/u/.claude-t3/x\t0\t{}\n",
            "AACROW\tevil\t/home/u/$(rm)\t0\t{}\n",
            "noise\n"
        );
        let scan = parse_scan("ubuntu", out);
        assert_eq!(scan.claude.as_deref(), Some("/home/u/.local/bin/claude"));
        assert_eq!(scan.readings.len(), 1);
        let r = &scan.readings[0];
        assert_eq!(
            (r.key().as_str(), r.state, r.email.as_deref()),
            ("ubuntu/party", SignIn::Ok, Some("p@x.party"))
        );
    }

    #[test]
    fn login_link_and_success() {
        let text = "Opening browser to sign in…\nIf the browser didn't open, visit:\nhttps://claude.com/cai/oauth/authorize?code=true&client_id=x&state=y\nPaste code here if prompted > ";
        assert_eq!(
            login_url(text).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=y")
        );
        assert_eq!(login_url("https://evil.example/oauth/authorize?x"), None);
        assert!(login_succeeded("Login successful.", Some(0)));
        assert!(!login_succeeded("Login successful.", Some(1)));
        assert!(!login_succeeded(
            "That doesn't look like the full code.",
            Some(0)
        ));
    }

    #[test]
    fn login_commands_per_system() {
        let Some(Script::Cmd(win)) = login(
            &host(Os::Windows),
            "C:\\Users\\s\\.local\\bin\\claude.exe",
            "C:\\Users\\s\\.claude-t3\\party",
            false,
        ) else {
            panic!("windows login is a cmd line");
        };
        assert!(win.starts_with(
            "set \"CLAUDE_CONFIG_DIR=C:\\Users\\s\\.claude-t3\\party\"&& set BROWSER=none&&"
        ));
        assert!(win.ends_with("auth login --claudeai"));
        assert!(!win.contains(" &&"));
        let Some(Script::Sh(linux)) =
            login(&host(Os::Linux), "/u/claude", "/u/.claude-t3/me", false)
        else {
            panic!("linux login is sh");
        };
        assert!(linux.contains(
            "CLAUDE_CONFIG_DIR='/u/.claude-t3/me' BROWSER=true '/u/claude' auth login --claudeai"
        ));
        let Some(Script::Sh(mac)) = login(&host(Os::Mac), "/u/claude", "/u/.claude", true) else {
            panic!("mac login is sh");
        };
        assert!(mac.contains("launchctl bootstrap"));
        assert!(mac.contains("-u CLAUDE_CONFIG_DIR BROWSER=true"));
        assert!(login(&host(Os::Linux), "/u/claude", "/u/$(x)", false).is_none());
        assert!(login(&host(Os::Windows), "C:\\c.exe", "C:\\a&b", false).is_none());
    }

    #[test]
    fn the_desktop_wrapper_keeps_the_job_private_and_bounded() {
        let Script::Sh(s) = desktop("echo hi\n", 60) else {
            panic!("sh");
        };
        assert!(s.contains("umask 077"));
        assert!(s.contains("mkfifo \"$d/in\" \"$d/out\""));
        assert!(s.contains("echo hi\n\nAAC_JOB_EOF"));
        assert!(s.contains("( sleep 60; kill $reader 2>/dev/null ) </dev/null >/dev/null 2>&1 &"));
        assert!(s.contains("launchctl bootout"));
        assert!(s.contains("cat <&3 > \"$d/in\" &"));
        assert!(s.contains(
            "<string>/bin/sh</string><string>-c</string><string>exec /bin/sh \"\\$0/job.sh\""
        ));
        let Script::Sh(py) = desktop_as("echo hi\n", 60, Parent::Python3) else {
            panic!("sh");
        };
        assert!(py.contains("<string>/usr/bin/python3</string>"));
        assert!(py.contains("stdin=open(d + \"/in\")"));
    }

    #[test]
    fn scan_scripts_skip_ignored_profiles_and_unsafe_names() {
        let Script::Sh(s) = scan(&host(Os::Linux), &["retired".into(), "x;y".into()]) else {
            panic!("sh");
        };
        assert!(s.contains("IGNORE=' retired '"));
        assert!(!s.contains("x;y"));
        assert!(matches!(scan(&host(Os::Windows), &[]), Script::Pwsh(_)));
        assert!(
            matches!(scan(&host(Os::Mac), &[]), Script::Sh(ref m) if m.contains("launchctl bootstrap"))
        );
    }
}
