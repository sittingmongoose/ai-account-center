//! Fixture profiles in temporary folders, with fake credential files only. Each native reading
//! is compared with what `claude auth status --json` prints for the same files.

use super::*;
use crate::browser::Waits;
use crate::config::Config;
use crate::hostcmd::{iso, parse_scan, status_reading};
use crate::remote::{Output, Remote, RemoteError, Script, Session};
use crate::tick::{Deps, scan_host};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;
const END: i64 = NOW + 20 * DAY + 123;
const FAKE_ACCESS: &str = "fake-access-value-for-tests";

struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        let home = Home(tempfile::tempdir().expect("temp home"));
        home.write("bin/claude", "#!/bin/sh\nexit 0\n");
        set_mode(&home.path().join("bin/claude"), 0o755);
        home
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn text(&self) -> String {
        self.path().to_str().expect("utf-8 temp path").to_owned()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn read(&self) -> LocalRead {
        LocalRead {
            home: self.path().to_owned(),
            cwd: None,
            managed_dir: self.path().join("etc-claude-code"),
            config_home: None,
            env_names: vec!["HOME".into(), "PATH".into(), "LANG".into()],
            path: Some(self.path().join("bin").into_os_string()),
        }
    }

    /// A profile under `~/.claude-t3` with this `.claude.json`.
    fn profile(&self, name: &str, config: &str) -> PathBuf {
        self.write(&format!(".claude-t3/{name}/.claude.json"), config);
        self.path().join(".claude-t3").join(name)
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// `.claude.json` with an account, among the unrelated fields the real file carries.
fn account(email: &str) -> String {
    format!(
        r#"{{"numStartups":7,"projects":{{"/w":{{"allowedTools":[],"history":[1,2]}}}},"oauthAccount":{{"accountUuid":"acct-fake","emailAddress":"{email}","organizationUuid":"org-fake","organizationName":"Fake Org"}}}}"#
    )
}

/// `.credentials.json` with fake values.
fn creds(expires_at: i64, ends: Option<i64>, refresh: bool) -> String {
    let ends = ends.map_or("null".to_owned(), |e| e.to_string());
    let refresh = if refresh {
        r#""fake-refresh-value""#
    } else {
        "null"
    };
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{FAKE_ACCESS}","refreshToken":{refresh},"expiresAt":{expires_at},"refreshTokenExpiresAt":{ends},"scopes":["user:inference","user:profile","user:sessions:claude_code"],"subscriptionType":"max","rateLimitTier":"default_claude_max_20x"}},"mcpOAuth":{{}}}}"#
    )
}

/// What `claude auth status --json` (2.1.x) prints for a claude.ai sign-in.
fn cli_claude_ai(email: &str, ends: Option<i64>) -> String {
    let ends = ends.map_or("null".to_owned(), |e| format!("\"{}\"", iso(e)));
    format!(
        r#"{{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","analyticsDisabled":false,"loginExpiresAt":{ends},"email":"{email}","orgId":"org-fake","orgName":"Fake Org","subscriptionType":"max","storedClaudeAiLogin":true}}"#
    )
}

const CLI_LOGGED_OUT: &str = r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty","analyticsDisabled":false,"loginExpiresAt":null}"#;
const CLI_TOKEN_ONLY: &str = r#"{"loggedIn":true,"authMethod":"oauth_token","apiProvider":"firstParty","analyticsDisabled":false,"loginExpiresAt":null}"#;

fn same_as_cli(native: Profile, cli: &str) {
    let (state, email, ends) = status_reading(cli);
    assert_eq!(
        native,
        Profile::Read(state, email, ends),
        "cli answer {cli}"
    );
}

#[test]
fn every_fixture_state_reads_as_the_cli_reports_it() {
    let home = Home::new();
    let read = home.read();

    // Logged in.
    let dir = home.profile("party", &account("P@X.party"));
    home.write(
        ".claude-t3/party/.credentials.json",
        &creds(NOW + 8 * 3_600_000, Some(END), true),
    );
    let got = read.read_profile(&dir, false, NOW);
    assert_eq!(
        got,
        Profile::Read(SignIn::Ok, Some("p@x.party".into()), Some(END))
    );
    same_as_cli(got, &cli_claude_ai("P@X.party", Some(END)));

    // Expired access token, sign-in still running: the CLI still reports it signed in.
    let dir = home.profile("stale", &account("s@x.party"));
    home.write(
        ".claude-t3/stale/.credentials.json",
        &creds(NOW - DAY, Some(END), true),
    );
    same_as_cli(
        read.read_profile(&dir, false, NOW),
        &cli_claude_ai("s@x.party", Some(END)),
    );

    // Logged out: the credentials hold no access token.
    let dir = home.profile("out", &account("o@x.party"));
    home.write(".claude-t3/out/.credentials.json", r#"{"mcpOAuth":{}}"#);
    same_as_cli(read.read_profile(&dir, false, NOW), CLI_LOGGED_OUT);
    home.write(
        ".claude-t3/out/.credentials.json",
        r#"{"claudeAiOauth":{"accessToken":"","subscriptionType":"max"}}"#,
    );
    same_as_cli(read.read_profile(&dir, false, NOW), CLI_LOGGED_OUT);

    // Token-only profile: the settings' token wins, with no end and no email.
    let dir = home.profile("tokentest", "{}");
    home.write(
        ".claude-t3/tokentest/settings.json",
        r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"fake-token-value"}}"#,
    );
    home.write(".claude-t3/tokentest/remote-settings.json", "{}");
    same_as_cli(read.read_profile(&dir, false, NOW), CLI_TOKEN_ONLY);

    // Signed in without a known end: no refresh token, or an access token far past the end.
    let dir = home.profile("noend", &account("n@x.party"));
    home.write(
        ".claude-t3/noend/.credentials.json",
        &creds(NOW, Some(END), false),
    );
    same_as_cli(
        read.read_profile(&dir, false, NOW),
        &cli_claude_ai("n@x.party", None),
    );
    home.write(
        ".claude-t3/noend/.credentials.json",
        &creds(END + 3 * DAY + 1, Some(END), true),
    );
    same_as_cli(
        read.read_profile(&dir, false, NOW),
        &cli_claude_ai("n@x.party", None),
    );
    home.write(
        ".claude-t3/noend/.credentials.json",
        &creds(NOW, Some(1_000), true),
    );
    same_as_cli(
        read.read_profile(&dir, false, NOW),
        &cli_claude_ai("n@x.party", None),
    );

    // The default folder takes its account from ~/.claude.json, as the CLI does.
    home.write(".claude.json", &account("d@x.party"));
    home.write(".claude/.claude.json", "{}");
    home.write(".claude/.credentials.json", &creds(NOW, Some(END), true));
    same_as_cli(
        read.read_profile(&home.path().join(".claude"), true, NOW),
        &cli_claude_ai("d@x.party", Some(END)),
    );

    // Unrelated settings keep the native read.
    let dir = home.profile("plain", &account("p2@x.party"));
    home.write(
        ".claude-t3/plain/.credentials.json",
        &creds(NOW, Some(END), true),
    );
    home.write(
        ".claude-t3/plain/settings.json",
        r#"{"cleanupPeriodDays":99,"env":{"CLAUDE_CODE_EXPERIMENTAL_CODE_MODE":"1"},"hooks":{},"model":"opus","enabledPlugins":{}}"#,
    );
    same_as_cli(
        read.read_profile(&dir, false, NOW),
        &cli_claude_ai("p2@x.party", Some(END)),
    );
}

#[test]
fn files_that_cannot_settle_a_profile_go_to_the_cli() {
    let home = Home::new();
    let read = home.read();
    let case = |name: &str, creds_text: Option<&str>, extra: &[(&str, &str)]| {
        let dir = home.profile(name, &account("c@x.party"));
        if let Some(text) = creds_text {
            home.write(&format!(".claude-t3/{name}/.credentials.json"), text);
        }
        for (file, text) in extra {
            home.write(&format!(".claude-t3/{name}/{file}"), text);
        }
        read.read_profile(&dir, false, NOW)
    };
    let good = creds(NOW, Some(END), true);
    assert_eq!(
        case("missing", None, &[]),
        Profile::Cli("credentials_missing")
    );
    assert_eq!(
        case("damaged", Some("{\"claudeAiOauth\":{\"accessToken\":"), &[]),
        Profile::Cli("credentials_damaged")
    );
    assert_eq!(
        case("array", Some("[1,2]"), &[]),
        Profile::Cli("credentials_damaged")
    );
    assert_eq!(
        case(
            "numeric",
            Some(r#"{"claudeAiOauth":{"accessToken":5,"scopes":["user:inference"]}}"#),
            &[]
        ),
        Profile::Cli("credentials_shape")
    );
    assert_eq!(
        case(
            "noscope",
            Some(r#"{"claudeAiOauth":{"accessToken":"fake","scopes":["user:profile"]}}"#),
            &[]
        ),
        Profile::Cli("scopes")
    );
    assert_eq!(
        case("ended", Some(&creds(NOW - DAY, Some(NOW - 1), true)), &[]),
        Profile::Cli("sign_in_ended")
    );
    assert_eq!(
        case("legacy", Some(&good), &[(".config.json", "{}")]),
        Profile::Cli("legacy_config")
    );
    assert_eq!(
        case(
            "helper",
            Some(&good),
            &[("settings.json", r#"{"apiKeyHelper":"/bin/echo x"}"#)]
        ),
        Profile::Cli("settings_auth")
    );
    assert_eq!(
        case(
            "gateway",
            Some(&good),
            &[(
                "settings.json",
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://gw.example"}}"#
            )]
        ),
        Profile::Cli("settings_auth")
    );
    assert_eq!(
        case("badsettings", Some(&good), &[("settings.json", "{")]),
        Profile::Cli("settings_damaged")
    );
    assert_eq!(
        case(
            "orgtoken",
            Some(&good),
            &[(
                "remote-settings.json",
                r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"fake"}}"#
            )]
        ),
        Profile::Cli("settings_auth")
    );
    let dir = home.profile("apikey", r#"{"primaryApiKey":"fake-key"}"#);
    home.write(".claude-t3/apikey/.credentials.json", &good);
    assert_eq!(read.read_profile(&dir, false, NOW), Profile::Cli("api_key"));
    let dir = home.profile("linked", &account("l@x.party"));
    home.write("elsewhere.json", &good);
    std::os::unix::fs::symlink(
        home.path().join("elsewhere.json"),
        dir.join(".credentials.json"),
    )
    .expect("symlink");
    assert_eq!(
        read.read_profile(&dir, false, NOW),
        Profile::Cli("credentials_link")
    );
    // The default folder without ~/.claude.json.
    home.write(".claude/.credentials.json", &good);
    assert_eq!(
        read.read_profile(&home.path().join(".claude"), true, NOW),
        Profile::Cli("config_missing")
    );
}

#[test]
fn the_server_environment_and_machine_settings_can_send_everything_to_the_cli() {
    let home = Home::new();
    let mut read = home.read();
    assert_eq!(read.host_blocker(), None);
    read.env_names = vec![
        "CLAUDE_CONFIG_DIR".into(),
        "CLAUDE_CODE_OAUTH_TOKEN".into(),
        "ANTHROPIC_API_KEY".into(),
        "CLAUDECODE".into(),
    ];
    assert_eq!(
        read.host_blocker(),
        None,
        "the scan sets or clears these itself"
    );
    read.env_names.push("ANTHROPIC_BASE_URL".into());
    assert_eq!(read.host_blocker(), Some("auth_environment"));
    read.env_names = vec!["CLAUDE_CODE_USE_BEDROCK".into()];
    assert_eq!(read.host_blocker(), Some("auth_environment"));
    read.env_names.clear();

    home.write("etc-claude-code/managed-settings.json", "{}");
    assert_eq!(read.host_blocker(), Some("managed_settings"));
    std::fs::remove_dir_all(home.path().join("etc-claude-code")).expect("rm");

    home.write("xdg/anthropic/configs/a.json", "{}");
    read.config_home = Some(home.path().join("xdg"));
    assert_eq!(read.host_blocker(), Some("anthropic_profiles"));
    read.config_home = None;

    home.write(
        "server/.claude/settings.local.json",
        r#"{"apiKeyHelper":"x"}"#,
    );
    read.cwd = Some(home.path().join("server"));
    assert_eq!(read.host_blocker(), Some("settings_auth"));
    home.write(
        "server/.claude/settings.local.json",
        r#"{"permissions":{}}"#,
    );
    assert_eq!(read.host_blocker(), None);
}

#[derive(Default)]
struct Counting {
    runs: AtomicU32,
    stdout: String,
}

impl Remote for Counting {
    fn run(&self, _: &Host, script: &Script, _: &[u8], _: Duration) -> Result<Output, RemoteError> {
        let (Script::Sh(text) | Script::Cmd(text) | Script::Pwsh(text)) = script;
        assert!(text.contains("auth status --json"));
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(Output {
            code: Some(0),
            stdout: self.stdout.clone(),
        })
    }

    fn start(&self, _: &Host, _: &Script, _: bool) -> Result<Box<dyn Session>, RemoteError> {
        Err(RemoteError::Spawn)
    }
}

fn cfg(hosts: &str) -> Config {
    let list: serde_json::Value = serde_json::from_str(hosts).expect("hosts");
    let browser = list[0]["name"].as_str().expect("name");
    Config::parse(&format!(
        r#"{{"hosts":{hosts},"browserHost":"{browser}","ignoreProfiles":["retired"]}}"#
    ))
    .expect("cfg")
}

fn scan_with(
    cfg: &Config,
    remote: &Counting,
    local: Option<&LocalRead>,
) -> (Result<Scan, String>, Vec<String>) {
    let lines = Mutex::new(vec![]);
    let log = |l: &str| lines.lock().expect("lock").push(l.to_owned());
    let now = || NOW;
    let deps = Deps {
        remote,
        now: &now,
        log: &log,
        waits: Waits::default(),
        local,
    };
    let out = scan_host(cfg, &deps, &cfg.hosts[0]);
    let said = lines.lock().expect("lock").clone();
    (out, said)
}

/// A computer like the dashboard's: claude.ai profiles, a token-only one, a retired one, a
/// folder that is not a profile, and a signed-in default folder.
fn dashboard_home() -> Home {
    let home = Home::new();
    home.profile("party", &account("p@x.party"));
    home.write(
        ".claude-t3/party/.credentials.json",
        &creds(NOW, Some(END), true),
    );
    home.profile("me", &account("Me@X.party"));
    home.write(
        ".claude-t3/me/.credentials.json",
        &creds(NOW, Some(END + DAY), true),
    );
    home.profile("tokentest", "{}");
    home.write(
        ".claude-t3/tokentest/settings.json",
        r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"fake-token-value"}}"#,
    );
    home.profile("retired", &account("r@x.party"));
    home.profile(".hidden", &account("h@x.party"));
    home.write(".claude-t3/notes/readme.txt", "not a profile");
    home.write(".claude.json", &account("d@x.party"));
    home.write(
        ".claude/.credentials.json",
        &creds(NOW, Some(END + 2 * DAY), true),
    );
    home
}

/// What the CLI scan printed for [`dashboard_home`].
fn dashboard_cli_output(home: &Home) -> String {
    let h = home.text();
    format!(
        concat!(
            "AACCLAUDE\t{h}/bin/claude\n",
            "AACROW\tme\t{h}/.claude-t3/me\t0\t{me}\n",
            "AACROW\tparty\t{h}/.claude-t3/party\t0\t{party}\n",
            "AACROW\ttokentest\t{h}/.claude-t3/tokentest\t0\t{token}\n",
            "AACROW\tdefault\t{h}/.claude\t1\t{default}\n",
        ),
        h = h,
        me = cli_claude_ai("Me@X.party", Some(END + DAY)),
        party = cli_claude_ai("p@x.party", Some(END)),
        token = CLI_TOKEN_ONLY,
        default = cli_claude_ai("d@x.party", Some(END + 2 * DAY)),
    )
}

#[test]
fn a_readable_local_computer_is_scanned_without_starting_a_process() {
    let home = dashboard_home();
    let cfg = cfg(r#"[{"name":"ubuntu","os":"linux"}]"#);
    let expected = parse_scan("ubuntu", &dashboard_cli_output(&home));
    assert_eq!(expected.readings.len(), 4);

    // Before: every scan started the CLI script.
    let before = Counting {
        stdout: dashboard_cli_output(&home),
        ..Counting::default()
    };
    let (out, _) = scan_with(&cfg, &before, None);
    assert_eq!(before.runs.load(Ordering::SeqCst), 1);
    assert_eq!(out, Ok(expected.clone()));

    // Now: the same scan, read from the files, with no process at all.
    let read = home.read();
    let now = Counting::default();
    let (out, lines) = scan_with(&cfg, &now, Some(&read));
    assert_eq!(
        now.runs.load(Ordering::SeqCst),
        0,
        "no process for a readable profile"
    );
    assert_eq!(out, Ok(expected));
    assert!(lines.is_empty(), "{lines:?}");
}

#[test]
fn one_unsettled_profile_reads_the_whole_computer_with_the_cli() {
    let home = dashboard_home();
    home.profile("fresh", &account("f@x.party"));
    let cfg = cfg(r#"[{"name":"ubuntu","os":"linux"}]"#);
    let read = home.read();
    let remote = Counting {
        stdout: dashboard_cli_output(&home),
        ..Counting::default()
    };
    let (out, lines) = scan_with(&cfg, &remote, Some(&read));
    assert_eq!(remote.runs.load(Ordering::SeqCst), 1);
    assert_eq!(out, Ok(parse_scan("ubuntu", &dashboard_cli_output(&home))));
    assert_eq!(
        lines,
        vec!["ubuntu: reading its sign-ins with the CLI (credentials_missing)".to_owned()]
    );
    assert!(lines.iter().all(|l| !l.contains("fake")));
}

#[test]
fn other_computers_the_mac_and_windows_keep_the_cli() {
    let home = dashboard_home();
    let read = home.read();
    for hosts in [
        r#"[{"name":"nas1","os":"linux","ssh":"nas1"}]"#,
        r#"[{"name":"mac","os":"mac"}]"#,
        r#"[{"name":"windows","os":"windows"}]"#,
    ] {
        let remote = Counting {
            stdout: "AACCLAUDE\t/u/claude\n".into(),
            ..Counting::default()
        };
        let (out, _) = scan_with(&cfg(hosts), &remote, Some(&read));
        assert_eq!(remote.runs.load(Ordering::SeqCst), 1, "{hosts}");
        assert!(out.is_ok());
    }
}

#[test]
fn claude_is_found_as_the_scan_script_finds_it() {
    let home = dashboard_home();
    let mut read = home.read();
    let host = |claude: Option<&str>| Host {
        name: "ubuntu".into(),
        os: Os::Linux,
        ssh: None,
        claude: claude.map(str::to_owned),
    };
    assert_eq!(
        read.find_claude(&host(None)),
        Some(format!("{}/bin/claude", home.text()))
    );
    assert_eq!(
        read.find_claude(&host(Some("/opt/claude/bin/claude")))
            .as_deref(),
        Some("/opt/claude/bin/claude")
    );
    // Not executable: not found on PATH; ~/.local/bin is next.
    set_mode(&home.path().join("bin/claude"), 0o644);
    home.write(".local/bin/claude", "#!/bin/sh\n");
    set_mode(&home.path().join(".local/bin/claude"), 0o700);
    assert_eq!(
        read.find_claude(&host(None)),
        Some(format!("{}/.local/bin/claude", home.text()))
    );
    // Nowhere (when this machine has none on the shared install paths): the CLI scan's
    // AACNOCLAUDE, and still no process.
    std::fs::remove_file(home.path().join(".local/bin/claude")).expect("rm");
    read.path = None;
    let system = ["/opt/homebrew/bin/claude", "/usr/local/bin/claude"];
    if system.iter().all(|p| !executable(Path::new(p))) {
        assert_eq!(read.find_claude(&host(None)), None);
        let remote = Counting::default();
        let (out, _) = scan_with(
            &cfg(r#"[{"name":"ubuntu","os":"linux"}]"#),
            &remote,
            Some(&read),
        );
        assert_eq!(out, Err("claude_not_found".into()));
        assert_eq!(remote.runs.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn a_default_folder_that_was_never_signed_in_is_left_out() {
    let home = Home::new();
    home.write(".claude.json", "{}");
    home.write(".claude/.credentials.json", "{}");
    let Native::Scan(scan) = home.read().scan(
        &Host {
            name: "ubuntu".into(),
            os: Os::Linux,
            ssh: None,
            claude: None,
        },
        &[],
        NOW,
    ) else {
        panic!("native scan");
    };
    assert!(scan.readings.is_empty());
    assert!(scan.claude.is_some());
}
