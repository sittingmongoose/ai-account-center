#![cfg(test)]
//! W2 desktop updater tests. Each ports a case of the Python oracle
//! (`tests/unit/app-updates/app_updates_test.py`) or of the bridge's TypeScript tests
//! (`tests/unit/app-updates/app-update-codex.test.ts`). Every path is under a
//! temporary root; nothing touches the real home, network or processes.
use super::bridge::{self, BridgeMode, Deps, MarkerChange, Request, StopError};
use super::*;
use crate::apps::updater::{
    AppRow, CommandOutput, CommandSpec, Diagnostics, Downloaded, Fingerprint, Platform, Probe,
    Process, ProcessTable, Shared, System, attempt, shared, tests as contract, updater,
};
use crate::codex_update::{Identity, RestartPlan, RestartState};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::time::Instant;

type Answer = Box<dyn Fn(&[String]) -> Result<String, UpdateFailure> + Send + Sync>;

/// A scripted host: command answers, downloads by URL, process scans in order.
struct Fake {
    runs: Mutex<Vec<Vec<String>>>,
    answer: Answer,
    downloads: Mutex<Vec<(String, u64, u64)>>,
    files: Mutex<HashMap<String, Result<Vec<u8>, &'static str>>>,
    tables: Mutex<VecDeque<Result<ProcessTable, &'static str>>>,
    scans: Mutex<usize>,
    fingerprint: Mutex<Option<Fingerprint>>,
    remote: Mutex<Vec<u8>>,
}

impl Fake {
    fn new(
        answer: impl Fn(&[String]) -> Result<String, UpdateFailure> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(Vec::new()),
            answer: Box::new(answer),
            downloads: Mutex::new(Vec::new()),
            files: Mutex::new(HashMap::new()),
            tables: Mutex::new(VecDeque::new()),
            scans: Mutex::new(0),
            fingerprint: Mutex::new(None),
            remote: Mutex::new(Vec::new()),
        })
    }

    fn silent() -> Arc<Self> {
        Self::new(|_| Ok(String::new()))
    }

    fn serve(&self, url: &str, body: Result<Vec<u8>, &'static str>) {
        self.files
            .lock()
            .expect("fixture")
            .insert(url.to_owned(), body);
    }

    fn scans(&self, tables: Vec<Result<ProcessTable, &'static str>>) {
        *self.tables.lock().expect("fixture") = tables.into();
    }

    fn ran(&self, word: &str) -> bool {
        self.runs
            .lock()
            .expect("fixture")
            .iter()
            .any(|argv| argv.iter().any(|arg| arg.contains(word)))
    }

    fn fetched(&self) -> Vec<String> {
        self.downloads
            .lock()
            .expect("fixture")
            .iter()
            .map(|(url, ..)| url.clone())
            .collect()
    }
}

impl System for Fake {
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
        Box::pin(async move {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.runs.lock().expect("fixture").push(argv.clone());
            let answer = (self.answer)(&argv);
            if !spec.quiet {
                let exit = match &answer {
                    Ok(_) => Some(0),
                    Err(failure) => failure.exit_code,
                };
                diagnostics.lock().expect("fixture").ran(
                    &spec.argv,
                    spec.label,
                    exit,
                    Some(answer.is_err()),
                );
            }
            answer.map(|stdout| CommandOutput { stdout })
        })
    }

    fn processes(&self, _: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
        Box::pin(async move {
            *self.scans.lock().expect("fixture") += 1;
            let mut tables = self.tables.lock().expect("fixture");
            let next = if tables.len() > 1 {
                tables.pop_front()
            } else {
                tables.front().cloned()
            };
            match next {
                None => Ok(ProcessTable::default()),
                Some(Ok(table)) => Ok(table),
                Some(Err(code)) => Err(UpdateFailure::new(code)),
            }
        })
    }

    fn download<'a>(
        &'a self,
        url: &'a str,
        destination: &'a Path,
        maximum: u64,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
        Box::pin(async move {
            self.downloads.lock().expect("fixture").push((
                url.to_owned(),
                maximum,
                timeout.as_secs(),
            ));
            match self.files.lock().expect("fixture").get(url) {
                Some(Ok(body)) => {
                    std::fs::write(destination, body).expect("fixture");
                    Ok(Downloaded {
                        bytes: body.len() as u64,
                        url: url.to_owned(),
                    })
                }
                Some(Err(code)) => Err(UpdateFailure::new(code)),
                None => Err(UpdateFailure::failed()),
            }
        })
    }

    fn fetch<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: u64,
        _: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
        Box::pin(async { Err(UpdateFailure::failed()) })
    }

    fn fingerprint<'a>(&'a self, _: &'a str, _: Duration) -> BoxFuture<'a, Option<Fingerprint>> {
        Box::pin(async move { self.fingerprint.lock().expect("fixture").clone() })
    }

    fn range<'a>(
        &'a self,
        url: &'a str,
        range: &'a str,
        _: Duration,
    ) -> BoxFuture<'a, Result<shared::RangeReply, UpdateFailure>> {
        Box::pin(async move {
            let body = self.remote.lock().expect("fixture").clone();
            if body.is_empty() {
                return Err(UpdateFailure::failed());
            }
            let total = body.len() as u64;
            let spec = range.strip_prefix("bytes=").expect("fixture");
            let (first, last) = match spec.strip_prefix('-') {
                Some(suffix) => (
                    total.saturating_sub(suffix.parse().expect("fixture")),
                    total - 1,
                ),
                None => {
                    let (first, last) = spec.split_once('-').expect("fixture");
                    (
                        first.parse().expect("fixture"),
                        last.parse::<u64>().expect("fixture").min(total - 1),
                    )
                }
            };
            Ok(shared::RangeReply {
                start: first,
                data: body[first as usize..=last as usize].to_vec(),
                total,
                url: url.to_owned(),
            })
        })
    }
}

/// A context with half an hour left (a Windows replace needs 390 s).
fn context(root: &Path, platform: Platform, system: Arc<Fake>) -> Context {
    context_with(root, platform, system, Duration::from_secs(1800))
}

fn context_with(root: &Path, platform: Platform, system: Arc<Fake>, left: Duration) -> Context {
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    let cx = Context::new(
        platform,
        contract::host(root, platform),
        system,
        Arc::new(Shared::default()),
        Instant::now() + left,
        root.join("stage"),
        root.join("state"),
        None,
        || false,
        move |name| seen.lock().expect("fixture").push(name),
    );
    PHASES.with(|cell| *cell.borrow_mut() = Some(phases));
    cx
}

thread_local! {
    static PHASES: std::cell::RefCell<Option<Arc<Mutex<Vec<&'static str>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The long phases a flow reported (only `downloading` and `updating` matter here).
fn phases() -> Vec<&'static str> {
    PHASES.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|phases| {
                phases
                    .lock()
                    .expect("fixture")
                    .iter()
                    .copied()
                    .filter(|name| matches!(*name, "downloading" | "updating"))
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("fixture")
}

fn row(value: &AppRow) -> (String, String, Option<String>, bool) {
    (
        value.status.as_str().to_owned(),
        value.message_code().to_owned(),
        value.version.clone(),
        value.update_attempted,
    )
}

fn expect(value: &AppRow, status: &str, code: &str, version: Option<&str>, attempted: bool) {
    assert_eq!(
        row(value),
        (
            status.to_owned(),
            code.to_owned(),
            version.map(str::to_owned),
            attempted
        ),
        "{:?}",
        value.to_value()
    );
}

// ------------------------------------------------------------------ fixtures

/// A ZIP with stored entries (what `zipfile.writestr` makes for these tests).
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let offset = out.len() as u32;
        let size = data.len() as u32;
        let header = |signature: u32, central: bool| {
            let mut bytes = signature.to_le_bytes().to_vec();
            if central {
                bytes.extend(20u16.to_le_bytes());
            }
            bytes.extend(20u16.to_le_bytes());
            bytes.extend([0, 0, 0, 0, 0, 0, 0, 0]);
            bytes.extend(crc.sum().to_le_bytes());
            bytes.extend(size.to_le_bytes());
            bytes.extend(size.to_le_bytes());
            bytes.extend((name.len() as u16).to_le_bytes());
            bytes.extend(0u16.to_le_bytes());
            if central {
                bytes.extend([0u8; 10]);
                bytes.extend(offset.to_le_bytes());
            }
            bytes.extend(name.as_bytes());
            bytes
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        directory.extend(header(0x0201_4b50, true));
    }
    let start = out.len() as u32;
    out.extend(&directory);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0, 0, 0, 0]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((directory.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn manifest(name: &str, publisher: &str, version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"><Identity Name="{name}" Publisher="{publisher}" Version="{version}" ProcessorArchitecture="x64"/></Package>"#
    )
}

fn msix(name: &str, publisher: &str, version: &str) -> Vec<u8> {
    zip(&[
        ("[Content_Types].xml", b"<Types/>"),
        (
            "AppxManifest.xml",
            manifest(name, publisher, version).as_bytes(),
        ),
    ])
}

/// A Mac bundle whose `Info.plist` names `identity` and `version`.
fn mac_bundle(folder: &Path, name: &str, identity: &str, version: &str) -> PathBuf {
    let bundle = folder.join(name);
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).expect("fixture");
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleIdentifier</key>\n\t<string>{identity}</string>\n\t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\t<key>CFBundleURLTypes</key>\n\t<array><dict><key>CFBundleIdentifier</key><string>nested.ignored</string></dict></array>\n</dict>\n</plist>\n"
        ),
    )
    .expect("fixture");
    std::fs::write(bundle.join("Contents/MacOS/app"), b"binary").expect("fixture");
    bundle
}

fn short_version(bundle: &Path) -> String {
    let text = std::fs::read_to_string(bundle.join("Contents/Info.plist")).expect("fixture");
    xml::plist(&text).expect("fixture")["CFBundleShortVersionString"]
        .as_str()
        .expect("fixture")
        .to_owned()
}

fn mac_install(app_id: &'static str, bundle: &Path, version: &str) -> Installed {
    let mut install = Installed::new(
        app_id,
        Platform::Mac,
        bundle.to_path_buf(),
        Some(version.into()),
        "official-download",
    );
    install.identity = Some(mac::bundle(app_id).1.to_owned());
    install.package_root = Some(bundle.to_path_buf());
    install
}

/// codesign answers for a bundle signed by `team`; every other command succeeds silently.
fn signed(team: &'static str) -> impl Fn(&[String]) -> Result<String, UpdateFailure> {
    move |argv| {
        if argv[0] == "/usr/bin/codesign" && argv[1] == "-dv" {
            return Ok(format!("Executable=x\nTeamIdentifier={team}\n"));
        }
        Ok(String::new())
    }
}

fn running_in(bundle: &Path) -> ProcessTable {
    ProcessTable::new(vec![Process {
        pid: 31,
        ppid: 1,
        exe: bundle
            .join("Contents/MacOS/app")
            .to_string_lossy()
            .into_owned(),
        identity: "31".into(),
        args: vec!["app".into()],
        ..Process::default()
    }])
}

fn windows_install(app_id: &'static str, version: &str) -> Installed {
    let (identity, executable, _) = windows::package_names(app_id);
    let mut install = Installed::new(
        app_id,
        Platform::Windows,
        PathBuf::from("/fixture").join(executable),
        Some(version.into()),
        "msix",
    );
    install.identity = Some(identity.into());
    install.publisher = Some("CN=fixture".into());
    install.package_root = Some(PathBuf::from("/fixture"));
    install
}

fn feed(value: &Value) -> Result<Vec<u8>, &'static str> {
    Ok(serde_json::to_vec(value).expect("fixture"))
}

// ------------------------------------------------------------------ XML readers

#[test]
fn plist_reads_top_level_keys_entities_and_hdiutil_output() {
    let info = xml::plist(
        "<?xml version=\"1.0\"?><!DOCTYPE plist PUBLIC \"x\" \"y\"><plist version=\"1.0\"><dict><key>A&amp;B</key><string>x &lt; y</string><key>n</key><integer>7</integer><key>t</key><true/><key>e</key><string/></dict></plist>",
    )
    .expect("fixture");
    assert_eq!(info, json!({"A&B": "x < y", "n": 7, "t": true, "e": ""}));
    let mounted = xml::plist("<plist><dict><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk4</string></dict><dict><key>dev-entry</key><string>/dev/disk4s1</string><key>mount-point</key><string>/Volumes/ChatGPT</string></dict></array></dict></plist>").expect("fixture");
    assert_eq!(
        mounted["system-entities"][1]["mount-point"],
        "/Volumes/ChatGPT"
    );
    for garbage in [
        "hdiutil: this is not a plist",
        "<plist><dict><key>a</key></dict></plist>",
        "<plist><dict><key>a</key><string>b</dict></plist>",
        "<plist><string>a</string></plist><plist/>",
    ] {
        assert_eq!(xml::plist(garbage), None, "{garbage}");
    }
}

#[test]
fn appx_identity_is_the_roots_direct_child_in_any_namespace() {
    let text = manifest("OpenAI.Codex", "CN=OpenAI &quot;Inc&quot;, O=x", "1.2.3.4");
    let identity = xml::appx_identity(&text).expect("fixture");
    assert_eq!(identity["Name"], "OpenAI.Codex");
    assert_eq!(identity["Publisher"], "CN=OpenAI \"Inc\", O=x");
    let prefixed = "<p:Package xmlns:p=\"u\"><!-- c --><p:Identity Name='N' Version='1.0.0.0'></p:Identity><Applications/></p:Package>";
    assert_eq!(xml::appx_identity(prefixed).expect("fixture")["Name"], "N");
    // Deeper elements and a missing root child are not the package identity.
    assert_eq!(
        xml::appx_identity("<Package><Properties><Identity Name='x'/></Properties></Package>"),
        None
    );
    assert_eq!(xml::appx_identity("<Package><Identity Name='x'>"), None);
}

#[test]
fn launch_agent_plist_round_trips() {
    let text = gui::agent_plist(
        "com.ccs.app-updates.cursor-update.abc",
        &["/h/.local/bin/cursor-agent".into(), "update".into()],
        Path::new("/tmp/stage-1"),
    );
    let value = xml::plist(&text).expect("fixture");
    assert_eq!(
        value["ProgramArguments"],
        json!(["/h/.local/bin/cursor-agent", "update"])
    );
    assert_eq!(
        value["StandardErrorPath"],
        Path::new("/tmp/stage-1")
            .join("stderr")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(value["ExitTimeOut"], 1);
    assert_eq!(value["KeepAlive"], false);
    assert_eq!(value["EnvironmentVariables"]["CODEX_NON_INTERACTIVE"], "1");
}

// ------------------------------------------------------------------ Windows (MSIX)

#[test]
fn windows_package_keeps_only_an_exact_package_family() {
    assert_eq!(
        windows::exact_family("OpenAI.Codex", Some("OpenAI.Codex_2p2nqsd0c76g0")),
        Some("OpenAI.Codex_2p2nqsd0c76g0".into())
    );
    for family in [
        "OpenAI.Codex_2P2NQSD0C76G0",
        "OpenAI.Codex_2p2nqsd0c76g",
        "Other_2p2nqsd0c76g0",
        "OpenAI.Codex-2p2nqsd0c76g0",
        "OpenAI.Codex_2p2nqsd0c76g0!App",
    ] {
        assert_eq!(
            windows::exact_family("OpenAI.Codex", Some(family)),
            None,
            "{family}"
        );
    }
}

#[tokio::test]
async fn windows_package_query_timeout_is_not_not_installed() {
    let root = temp();
    for (code, probe) in [
        ("timeout", Probe::Timeout),
        ("update_failed", Probe::Failed),
    ] {
        let fake = Fake::new(move |_| Err(UpdateFailure::new(code)));
        let cx = context(root.path(), Platform::Windows, fake);
        let (install, _) = windows::package(&cx, "codex-desktop", false)
            .await
            .expect("fixture");
        assert_eq!(install.probe, probe);
    }
    // An empty answer is the only "not installed".
    let cx = context(root.path(), Platform::Windows, Fake::silent());
    assert!(
        windows::package(&cx, "codex-desktop", false)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn windows_package_reads_the_registered_package() {
    let root = temp();
    let location = root.path().join("WindowsApps/OpenAI.Codex_1");
    std::fs::create_dir_all(location.join("app")).expect("fixture");
    std::fs::write(location.join("app/ChatGPT.exe"), b"exe").expect("fixture");
    let answer =
        json!({"name": "OpenAI.Codex", "version": "26.930.3748.0", "publisher": "CN=fixture",
        "root": location, "family": "OpenAI.Codex_2p2nqsd0c76g0"})
        .to_string();
    let fake = Fake::new(move |_| Ok(format!("\u{feff}{answer}")));
    let cx = context(root.path(), Platform::Windows, fake);
    let (install, family) = windows::package(&cx, "codex-desktop", false)
        .await
        .expect("fixture");
    assert_eq!(install.version.as_deref(), Some("26.930.3748.0"));
    assert_eq!(install.publisher.as_deref(), Some("CN=fixture"));
    assert_eq!(install.probe, Probe::Ok);
    assert_eq!(install.path, location.join("app/ChatGPT.exe"));
    assert_eq!(family.as_deref(), Some("OpenAI.Codex_2p2nqsd0c76g0"));
    // A foreign name or a missing executable is an unreadable package, not an absent one.
    let foreign = Fake::new(|_| Ok(r#"{"name":"Impostor","root":"/x"}"#.into()));
    let cx = context(root.path(), Platform::Windows, foreign);
    let (install, _) = windows::package(&cx, "codex-desktop", false)
        .await
        .expect("fixture");
    assert_eq!(install.probe, Probe::Failed);
}

#[tokio::test]
async fn msix_identity_mismatch_stops_before_process_shutdown() {
    let root = temp();
    let fake = Fake::silent();
    let install = windows_install("codex-desktop", "1.0.0.0");
    let release = windows::CODEX_RELEASE_PREFIX.to_owned() + "2.0.0.0/ChatGPT-x64.msix";
    fake.serve(
        windows::CODEX_STORE_FEED,
        feed(&json!({"buildVersion": "1.0.0.0",
        "storeProductId": windows::CODEX_STORE_ID, "packageIdentity": "OpenAI.Codex"})),
    );
    fake.serve(
        windows::CODEX_RELEASE_FEED,
        feed(&json!({"schemaVersion": 1, "packageVersion": "2.0.0.0",
        "packages": [{"architecture": "x64", "downloadUrl": release}]})),
    );
    fake.serve(&release, Ok(msix("Impostor", "Other", "9.0.0.0")));
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &install).await;
    assert_eq!(value.message_code(), "signature_failed");
    assert!(!fake.ran("Add-AppxPackage") && !fake.ran("Get-AppxPackage"));
}

#[test]
fn codex_release_feed_names_only_that_versions_own_package() {
    let folder = windows::CODEX_RELEASE_PREFIX.to_owned() + "26.1016.6573.0/";
    let good = json!({"schemaVersion": 1, "releaseDate": "2026-10-21T08:00:20Z", "packageVersion": "26.1016.6573.0",
        "packages": [{"architecture": "x64", "downloadUrl": folder.clone() + "ChatGPT-x64.msix"},
                     {"architecture": "arm64", "downloadUrl": folder.clone() + "ChatGPT-arm64.msix"}]});
    let expected = |arch: &str| {
        Some((
            "26.1016.6573.0".to_owned(),
            format!("{folder}ChatGPT-{arch}.msix"),
        ))
    };
    assert_eq!(windows::release_package(&good, "x64"), expected("x64"));
    assert_eq!(windows::release_package(&good, "arm64"), expected("arm64"));
    let mut renamed = good.clone();
    renamed["packages"] =
        json!([{"architecture": "x86_64", "downloadUrl": folder.clone() + "ChatGPT-x64.msix"}]);
    assert_eq!(windows::release_package(&renamed, "x64"), expected("x64"));
    let with_packages = |packages: Value| {
        let mut value = good.clone();
        value["packages"] = packages;
        value
    };
    let one = |url: String| with_packages(json!([{"architecture": "x64", "downloadUrl": url}]));
    let mut broken = vec![
        one(
            "https://elsewhere.invalid/codex-app-prod/releases/26.1016.6573.0/ChatGPT-x64.msix"
                .into(),
        ),
        one(windows::CODEX_RELEASE_PREFIX.to_owned() + "99.0.0.0/ChatGPT-x64.msix"),
        one(folder.clone() + "../ChatGPT-x64.msix"),
        one(folder.clone() + "ChatGPT-x64.msix?x=1"),
        one(folder.clone() + "ChatGPT-x64.exe"),
        with_packages(
            json!([{"architecture": "arm64", "downloadUrl": folder.clone() + "ChatGPT-arm64.msix"}]),
        ),
        with_packages(json!({})),
        json!([good.clone()]),
    ];
    let mut three = good.clone();
    three["packageVersion"] = json!("26.1016.6573");
    broken.push(three);
    let mut schema = good.clone();
    schema["schemaVersion"] = json!(2);
    broken.push(schema);
    for value in broken {
        assert_eq!(windows::release_package(&value, "x64"), None, "{value}");
    }
}

#[tokio::test]
async fn codex_store_feed_is_read_only_for_its_own_package() {
    let root = temp();
    let install = windows_install("codex-desktop", "26.930.3748.0");
    for (body, wanted) in [
        (
            feed(
                &json!({"schemaVersion": 1, "buildVersion": "26.1002.7124.0", "storeProductId": "9PLM9XGG6VKS", "packageIdentity": "OpenAI.Codex"}),
            ),
            Some("26.1002.7124.0"),
        ),
        (
            feed(
                &json!({"buildVersion": "99.0.0.0", "storeProductId": "9PLM9XGG6VKS", "packageIdentity": "Impostor"}),
            ),
            None,
        ),
        (Err("update_failed"), None),
    ] {
        let fake = Fake::silent();
        fake.serve(windows::CODEX_STORE_FEED, body);
        let cx = context(root.path(), Platform::Windows, fake);
        assert_eq!(
            windows::codex_store_version(&cx, &install).await.as_deref(),
            wanted
        );
    }
}

/// A Windows Codex whose two feeds answer as given; package reads and downloads fail loudly.
fn codex_feeds(store: Option<&str>, release: Option<&str>) -> Arc<Fake> {
    let fake = Fake::new(|argv| {
        assert!(
            !argv.iter().any(|arg| arg.contains("winget")),
            "winget was asked"
        );
        Ok(String::new())
    });
    if let Some(store) = store {
        fake.serve(
            windows::CODEX_STORE_FEED,
            feed(&json!({"buildVersion": store,
            "storeProductId": windows::CODEX_STORE_ID, "packageIdentity": "OpenAI.Codex"})),
        );
    }
    if let Some(release) = release {
        let url = format!(
            "{}{release}/ChatGPT-x64.msix",
            windows::CODEX_RELEASE_PREFIX
        );
        fake.serve(
            windows::CODEX_RELEASE_FEED,
            feed(&json!({"schemaVersion": 1, "packageVersion": release,
            "packages": [{"architecture": "x64", "downloadUrl": url}]})),
        );
    }
    fake
}

#[tokio::test]
async fn windows_codex_store_build_without_a_release_answer_is_unknown() {
    let root = temp();
    let fake = codex_feeds(Some("26.1002.7124.0"), None);
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("codex-desktop", "26.930.3748.0")).await;
    expect(
        &value,
        "unknown",
        "readiness_unknown",
        Some("26.930.3748.0"),
        false,
    );
    assert_eq!(fake.fetched().len(), 2, "only the two feeds");
}

#[tokio::test]
async fn windows_codex_store_build_the_release_feed_answers_is_current() {
    let root = temp();
    let fake = codex_feeds(Some("26.1002.7124.0"), Some("26.930.3748.0"));
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("codex-desktop", "26.930.3748.0")).await;
    expect(&value, "current", "current", Some("26.930.3748.0"), false);
    assert_eq!(fake.fetched().len(), 2);
}

#[tokio::test]
async fn windows_codex_with_no_newer_build_in_either_feed_is_current_without_the_direct_package() {
    let root = temp();
    for release in [None, Some("26.930.3748.0")] {
        let fake = codex_feeds(Some("26.930.3748.0"), release);
        let cx = context(root.path(), Platform::Windows, fake.clone());
        let value = update_desktop(&cx, &windows_install("codex-desktop", "26.930.3748.0")).await;
        expect(&value, "current", "current", Some("26.930.3748.0"), false);
        assert_eq!(fake.fetched().len(), 2);
    }
    // Neither feed answers: the check could not run.
    let fake = codex_feeds(None, None);
    let cx = context(root.path(), Platform::Windows, fake);
    let value = update_desktop(&cx, &windows_install("codex-desktop", "26.930.3748.0")).await;
    expect(
        &value,
        "unknown",
        "readiness_unknown",
        Some("26.930.3748.0"),
        false,
    );
}

#[tokio::test]
async fn windows_running_app_with_current_manifest_is_left_running_without_download() {
    let root = temp();
    let fake = Fake::silent();
    *fake.remote.lock().expect("fixture") = msix("Claude", "CN=fixture", "2.19675.1.0");
    fake.scans(vec![Ok(ProcessTable::new(vec![Process {
        pid: 31,
        ppid: 1,
        exe: "/fixture/app/Claude.exe".into(),
        identity: "31".into(),
        args: vec!["/fixture/app/Claude.exe".into()],
        session: Some(1),
        ..Process::default()
    }]))]);
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(&value, "current", "current", Some("2.19675.1.0"), false);
    assert!(fake.fetched().is_empty());
}

#[tokio::test]
async fn windows_unreopenable_running_app_reports_quit_first_before_any_download() {
    let root = temp();
    let fake = Fake::silent();
    *fake.remote.lock().expect("fixture") = msix("Claude", "CN=fixture", "3.0.0.0");
    // This host's session cannot be read here, so no instance can be reopened exactly.
    fake.scans(vec![Ok(ProcessTable::new(vec![Process {
        pid: 31,
        ppid: 1,
        exe: "/fixture/app/Claude.exe".into(),
        identity: "31".into(),
        args: vec!["/fixture/app/Claude.exe".into()],
        session: Some(1),
        ..Process::default()
    }]))]);
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.0.0.0")).await;
    expect(
        &value,
        "action_required",
        "quit_first",
        Some("2.0.0.0"),
        false,
    );
    assert!(fake.fetched().is_empty());
}

/// A Claude MSIX update with nothing running: the package downloads, then Add-AppxPackage
/// answers `deploy` and the re-query names `refreshed`.
fn claude_msix(root: &Path, deploy: &'static str, refreshed: &'static str) -> Arc<Fake> {
    let location = root.join("WindowsApps/Claude_2");
    std::fs::create_dir_all(location.join("app")).expect("fixture");
    std::fs::write(location.join("app/Claude.exe"), b"exe").expect("fixture");
    let query = json!({"name": "Claude", "version": refreshed, "publisher": "CN=fixture",
        "root": location, "family": "Claude_pzs8sxrjxfjjc"})
    .to_string();
    let fake = Fake::new(move |argv| {
        let decoded = decode_powershell(argv);
        if decoded.contains("Add-AppxPackage") {
            return Ok(deploy.to_owned());
        }
        if decoded.contains("Get-AppxPackage") {
            return Ok(query.clone());
        }
        Ok(String::new())
    });
    fake.serve(
        "https://claude.ai/api/desktop/win32/x64/msix/latest/redirect",
        Ok(msix("Claude", "CN=fixture", "2.19700.0.0")),
    );
    fake
}

/// The script of a `powershell -EncodedCommand <base64 UTF-16LE>` run.
fn decode_powershell(argv: &[String]) -> String {
    use base64::Engine as _;
    let Some(at) = argv
        .iter()
        .position(|arg| arg.eq_ignore_ascii_case("-EncodedCommand"))
    else {
        return argv.join(" ");
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&argv[at + 1])
        .unwrap_or_default();
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

#[tokio::test]
async fn windows_not_running_installs_without_relaunching_anything() {
    let root = temp();
    let fake = claude_msix(root.path(), "CCS_ADDAPPX_OK", "2.19700.0.0");
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(&value, "updated", "updated", Some("2.19700.0.0"), true);
    assert_eq!(value.restarted_processes, 0);
    assert_eq!(value.forced_stops, Some(0));
    assert_eq!(value.restart_targets, None);
    assert_eq!(phases(), ["downloading", "updating"]);
    // Only the package itself was downloaded: no manifest decided, so the whole package did.
    assert_eq!(
        fake.fetched(),
        ["https://claude.ai/api/desktop/win32/x64/msix/latest/redirect"]
    );
}

#[tokio::test]
async fn windows_closed_app_still_in_use_reports_an_attempted_package_hold() {
    let root = temp();
    let fake = claude_msix(
        root.path(),
        "CCS_ADDAPPX_FAILED: System.Exception: Deployment failed with HRESULT: 0x80073D02",
        "2.19675.1.0",
    );
    let cx = context(root.path(), Platform::Windows, fake);
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(
        &value,
        "action_required",
        "package_in_use",
        Some("2.19675.1.0"),
        true,
    );
    assert_eq!(value.forced_stops, Some(0));
}

#[tokio::test]
async fn windows_other_deployment_failure_and_unchanged_version_are_failed() {
    let root = temp();
    let fake = claude_msix(root.path(), "CCS_ADDAPPX_FAILED: other", "2.19675.1.0");
    let cx = context(root.path(), Platform::Windows, fake);
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(&value, "failed", "update_failed", Some("2.19675.1.0"), true);
    let fake = claude_msix(root.path(), "CCS_ADDAPPX_OK", "2.19675.1.0");
    let cx = context(root.path(), Platform::Windows, fake);
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(
        &value,
        "failed",
        "version_unknown",
        Some("2.19675.1.0"),
        true,
    );
}

#[tokio::test]
async fn windows_identity_mismatch_never_installs() {
    let root = temp();
    let fake = Fake::silent();
    fake.serve(
        "https://claude.ai/api/desktop/win32/x64/msix/latest/redirect",
        Ok(msix("Impostor", "Other", "99.0.0.0")),
    );
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.0.0.0")).await;
    assert_eq!(value.message_code(), "signature_failed");
    assert!(!fake.ran("-EncodedCommand"));
}

#[tokio::test]
async fn windows_impostor_manifest_does_not_decide_current() {
    let root = temp();
    let fake = Fake::silent();
    *fake.remote.lock().expect("fixture") = msix("Impostor", "Other", "1.0.0.0");
    fake.serve(
        "https://claude.ai/api/desktop/win32/x64/msix/latest/redirect",
        Err("timeout"),
    );
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.0.0.0")).await;
    assert_eq!(fake.fetched().len(), 1);
    expect(&value, "failed", "timeout", Some("2.0.0.0"), false);
}

#[tokio::test]
async fn windows_process_scan_timeout_reports_its_code_before_any_download() {
    let root = temp();
    let fake = Fake::silent();
    *fake.remote.lock().expect("fixture") = msix("Claude", "CN=fixture", "3.0.0.0");
    fake.scans(vec![Err("timeout")]);
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.0.0.0")).await;
    expect(&value, "failed", "timeout", Some("2.0.0.0"), false);
    assert!(fake.fetched().is_empty());
}

#[tokio::test]
async fn windows_blocked_download_says_check_in_app_and_a_short_deadline_closes_nothing() {
    let root = temp();
    let fake = Fake::silent();
    fake.serve(
        "https://claude.ai/api/desktop/win32/x64/msix/latest/redirect",
        Err("download_blocked"),
    );
    let cx = context(root.path(), Platform::Windows, fake.clone());
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.0.0.0")).await;
    expect(
        &value,
        "action_required",
        "check_in_app",
        Some("2.0.0.0"),
        false,
    );
    assert_eq!(fake.fetched().len(), 3, "three tries");
    // With less than the replace time left nothing downloads (the reserve) or closes.
    let fake = claude_msix(root.path(), "CCS_ADDAPPX_OK", "2.19700.0.0");
    let cx = context_with(
        root.path(),
        Platform::Windows,
        fake.clone(),
        Duration::from_secs(200),
    );
    let value = update_desktop(&cx, &windows_install("claude-desktop", "2.19675.1.0")).await;
    expect(&value, "failed", "timeout", Some("2.19675.1.0"), false);
    assert!(!fake.ran("-EncodedCommand"));
    assert!(fake.fetched().is_empty());
}

#[test]
fn a_local_zip_member_is_read_or_absent() {
    let root = temp();
    let package = root.path().join("p.msix");
    std::fs::write(&package, msix("Claude", "CN=x", "1.0.0.0")).expect("fixture");
    let found = windows::local_zip_member(&package, "AppxManifest.xml", 1024).expect("fixture");
    assert!(
        String::from_utf8(found.expect("fixture"))
            .expect("fixture")
            .contains("Claude")
    );
    assert_eq!(
        windows::local_zip_member(&package, "missing", 1024).expect("fixture"),
        None
    );
    assert!(windows::local_zip_member(&package, "AppxManifest.xml", 8).is_err());
    std::fs::write(&package, b"not a zip").expect("fixture");
    assert!(windows::local_zip_member(&package, "AppxManifest.xml", 1024).is_err());
}

// ------------------------------------------------------------------ Mac

fn claude_feed(current: &str, url_version: &str) -> Result<Vec<u8>, &'static str> {
    feed(
        &json!({"currentRelease": current, "releases": [{"version": current,
        "updateTo": {"version": current, "url": format!("{}{url_version}/Claude.zip", mac::CLAUDE_DARWIN_PREFIX)}}]}),
    )
}

#[tokio::test]
async fn mac_running_claude_with_newer_feed_reports_quit_first_without_package_download() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.19675.0",
    );
    let fake = Fake::silent();
    fake.serve(
        mac::CLAUDE_DARWIN_FEED,
        claude_feed("2.19700.0", "2.19700.0"),
    );
    fake.scans(vec![Ok(running_in(&bundle))]);
    let cx = context(root.path(), Platform::Mac, fake.clone());
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.19675.0")).await;
    expect(
        &value,
        "action_required",
        "quit_first",
        Some("2.19675.0"),
        false,
    );
    assert_eq!(fake.fetched(), [mac::CLAUDE_DARWIN_FEED]);
    assert!(fake.runs.lock().expect("fixture").is_empty());
    assert!(phases().is_empty());
}

#[tokio::test]
async fn mac_running_codex_with_remembered_newer_dmg_reports_quit_first_without_download() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "ChatGPT.app",
        "com.openai.codex",
        "26.930.41038",
    );
    let fake = Fake::silent();
    let fingerprint = Fingerprint {
        url: mac::CODEX_DMG.into(),
        size: 750_982_223,
        validator: "\"0xFIXTURE\"".into(),
    };
    *fake.fingerprint.lock().expect("fixture") = Some(fingerprint.clone());
    fake.scans(vec![Ok(running_in(&bundle))]);
    let cx = context(root.path(), Platform::Mac, fake.clone());
    crate::apps::updater::common::write_private_json(
        &mac::package_memory(&cx, "codex-desktop"),
        &json!({"url": fingerprint.url, "size": fingerprint.size, "validator": fingerprint.validator, "version": "26.930.61225"}),
    )
    .expect("fixture");
    let value = update_desktop(&cx, &mac_install("codex-desktop", &bundle, "26.930.41038")).await;
    expect(
        &value,
        "action_required",
        "quit_first",
        Some("26.930.41038"),
        false,
    );
    assert!(fake.fetched().is_empty());
    // A different published DMG is not remembered.
    let other = Fingerprint {
        size: 1,
        ..fingerprint
    };
    assert_eq!(
        mac::remembered_dmg_version(
            &cx,
            &mac_install("codex-desktop", &bundle, "1"),
            Some(&other)
        ),
        None
    );
}

#[tokio::test]
async fn mac_codex_remembered_current_dmg_skips_download() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "ChatGPT.app",
        "com.openai.codex",
        "26.930.61225",
    );
    let fake = Fake::silent();
    let fingerprint = Fingerprint {
        url: mac::CODEX_DMG.into(),
        size: 750_982_223,
        validator: "\"0xFIXTURE\"".into(),
    };
    *fake.fingerprint.lock().expect("fixture") = Some(fingerprint.clone());
    let cx = context(root.path(), Platform::Mac, fake.clone());
    crate::apps::updater::common::write_private_json(
        &mac::package_memory(&cx, "codex-desktop"),
        &json!({"url": fingerprint.url, "size": fingerprint.size, "validator": fingerprint.validator, "version": "26.930.61225"}),
    )
    .expect("fixture");
    let value = update_desktop(&cx, &mac_install("codex-desktop", &bundle, "26.930.61225")).await;
    expect(&value, "current", "current", Some("26.930.61225"), false);
    assert!(fake.fetched().is_empty());
}

/// hdiutil's answer for a DMG mounted at `volume`.
fn mounted(volume: &Path) -> String {
    format!(
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk9</string><key>mount-point</key><string>{}</string></dict></array></dict></plist>",
        volume.display()
    )
}

#[tokio::test]
async fn mac_running_codex_with_unknown_dmg_downloads_live_then_reports_quit_first_without_swapping()
 {
    let root = temp();
    let applications = root.path().join("Applications");
    let bundle = mac_bundle(
        &applications,
        "ChatGPT.app",
        "com.openai.codex",
        "26.928.31416",
    );
    let volume = root.path().join("mnt");
    let candidate = mac_bundle(&volume, "ChatGPT.app", "com.openai.codex", "26.930.41038");
    std::fs::write(candidate.join("Contents/staged-marker.txt"), b"staged").expect("fixture");
    let answer = mounted(&volume);
    let sign = signed("2DC432GLL2");
    let fake = Fake::new(move |argv| {
        if argv[0] == "/usr/bin/hdiutil" && argv[1] == "attach" {
            return Ok(answer.clone());
        }
        sign(argv)
    });
    let fingerprint = Fingerprint {
        url: mac::CODEX_DMG.into(),
        size: 1,
        validator: "\"0xNEW\"".into(),
    };
    *fake.fingerprint.lock().expect("fixture") = Some(fingerprint.clone());
    fake.serve(mac::CODEX_DMG, Ok(b"dmg".to_vec()));
    fake.scans(vec![Ok(running_in(&bundle))]);
    let cx = context(root.path(), Platform::Mac, fake.clone());
    let install = mac_install("codex-desktop", &bundle, "26.928.31416");
    let value = update_desktop(&cx, &install).await;
    expect(
        &value,
        "action_required",
        "quit_first",
        Some("26.928.31416"),
        false,
    );
    assert_eq!(phases(), ["downloading", "updating"]);
    assert!(!bundle.join("Contents/staged-marker.txt").exists());
    let names: Vec<_> = std::fs::read_dir(&applications)
        .expect("fixture")
        .map(|entry| entry.expect("fixture").file_name())
        .collect();
    assert_eq!(names, ["ChatGPT.app"]);
    // The next click answers in seconds: the verified version is remembered.
    assert_eq!(
        mac::remembered_dmg_version(&cx, &install, Some(&fingerprint)).as_deref(),
        Some("26.930.41038")
    );
    // The mounted image is always detached.
    assert!(
        fake.runs
            .lock()
            .expect("fixture")
            .iter()
            .any(|argv| argv[..2] == ["/usr/bin/hdiutil", "detach"])
    );
}

#[tokio::test]
async fn mac_app_opened_while_staging_is_never_swapped() {
    let root = temp();
    let applications = root.path().join("Applications");
    let bundle = mac_bundle(
        &applications,
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.0.0",
    );
    let fake = Fake::new(move |argv| {
        if argv[0] == "/usr/bin/ditto" {
            mac_bundle(
                Path::new(&argv[4]),
                "Claude.app",
                "com.anthropic.claudefordesktop",
                "3.0.0",
            );
        }
        signed("Q6L2SF6YDW")(argv)
    });
    fake.serve(mac::CLAUDE_DARWIN_FEED, claude_feed("3.0.0", "3.0.0"));
    fake.serve(
        &format!("{}3.0.0/Claude.zip", mac::CLAUDE_DARWIN_PREFIX),
        Ok(b"zip".to_vec()),
    );
    fake.scans(vec![
        Ok(ProcessTable::default()),
        Ok(ProcessTable::default()),
        Ok(running_in(&bundle)),
    ]);
    let cx = context(root.path(), Platform::Mac, fake);
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.0.0")).await;
    expect(
        &value,
        "action_required",
        "quit_first",
        Some("2.0.0"),
        false,
    );
    assert_eq!(short_version(&bundle), "2.0.0");
    let names: Vec<_> = std::fs::read_dir(&applications)
        .expect("fixture")
        .map(|entry| entry.expect("fixture").file_name())
        .collect();
    assert_eq!(names, ["Claude.app"]);
}

#[tokio::test]
async fn claude_feed_zip_installs_through_ditto_when_not_running() {
    let root = temp();
    let applications = root.path().join("Applications");
    let bundle = mac_bundle(
        &applications,
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.0.0",
    );
    let fake = Fake::new(move |argv| {
        if argv[0] == "/usr/bin/ditto" {
            mac_bundle(
                Path::new(&argv[4]),
                "Claude.app",
                "com.anthropic.claudefordesktop",
                "3.0.0",
            );
        }
        signed("Q6L2SF6YDW")(argv)
    });
    let package = format!("{}3.0.0/Claude-y.zip", mac::CLAUDE_DARWIN_PREFIX);
    fake.serve(
        mac::CLAUDE_DARWIN_FEED,
        feed(
            &json!({"currentRelease": "3.0.0", "releases": [{"version": "3.0.0",
        "updateTo": {"version": "3.0.0", "url": package.clone()}}]}),
        ),
    );
    fake.serve(&package, Ok(b"zip".to_vec()));
    let cx = context(root.path(), Platform::Mac, fake.clone());
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.0.0")).await;
    expect(&value, "updated", "updated", Some("3.0.0"), true);
    assert_eq!(value.restarted_processes, 0);
    assert_eq!(value.forced_stops, Some(0));
    let downloads = fake.downloads.lock().expect("fixture").clone();
    assert_eq!(
        downloads[0],
        (mac::CLAUDE_DARWIN_FEED.to_owned(), 256 * 1024, 60)
    );
    assert_eq!(
        downloads[1],
        (package, DESKTOP_DOWNLOAD_MAXIMUM, DESKTOP_DOWNLOAD_TIMEOUT)
    );
    assert_eq!(short_version(&bundle), "3.0.0");
    let names: Vec<_> = std::fs::read_dir(&applications)
        .expect("fixture")
        .map(|entry| entry.expect("fixture").file_name())
        .collect();
    assert_eq!(names, ["Claude.app"]);
    // Verified three times: the candidate, the staged copy and the installed bundle.
    let verifies = fake
        .runs
        .lock()
        .expect("fixture")
        .iter()
        .filter(|argv| argv.get(1).is_some_and(|arg| arg == "--verify"))
        .count();
    assert_eq!(verifies, 3);
}

#[tokio::test]
async fn mac_wrong_team_is_never_installed() {
    let root = temp();
    let applications = root.path().join("Applications");
    let bundle = mac_bundle(
        &applications,
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.0.0",
    );
    let fake = Fake::new(move |argv| {
        if argv[0] == "/usr/bin/ditto" {
            mac_bundle(
                Path::new(&argv[4]),
                "Claude.app",
                "com.anthropic.claudefordesktop",
                "3.0.0",
            );
        }
        signed("OTHERTEAM1")(argv)
    });
    fake.serve(mac::CLAUDE_DARWIN_FEED, claude_feed("3.0.0", "3.0.0"));
    fake.serve(
        &format!("{}3.0.0/Claude.zip", mac::CLAUDE_DARWIN_PREFIX),
        Ok(b"zip".to_vec()),
    );
    let cx = context(root.path(), Platform::Mac, fake);
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.0.0")).await;
    expect(&value, "failed", "signature_failed", Some("2.0.0"), false);
    assert_eq!(short_version(&bundle), "2.0.0");
}

#[tokio::test]
async fn claude_feed_current_skips_package_download() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.19675.0",
    );
    let fake = Fake::silent();
    fake.serve(
        mac::CLAUDE_DARWIN_FEED,
        claude_feed("2.19675.0", "2.19675.0"),
    );
    let cx = context(root.path(), Platform::Mac, fake.clone());
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.19675.0")).await;
    expect(&value, "current", "current", Some("2.19675.0"), false);
    assert_eq!(
        fake.downloads.lock().expect("fixture").clone(),
        [(mac::CLAUDE_DARWIN_FEED.to_owned(), 256 * 1024, 60)]
    );
    assert!(fake.runs.lock().expect("fixture").is_empty());
}

#[tokio::test]
async fn mac_download_timeout_keeps_its_code_and_a_block_says_check_in_app() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.19675.0",
    );
    for (code, status, shown) in [
        ("timeout", "failed", "timeout"),
        ("download_blocked", "action_required", "check_in_app"),
    ] {
        let fake = Fake::silent();
        fake.serve(mac::CLAUDE_DARWIN_FEED, Err(code));
        let cx = context(root.path(), Platform::Mac, fake);
        let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.19675.0")).await;
        expect(&value, status, shown, Some("2.19675.0"), false);
    }
}

#[tokio::test]
async fn dmg_plist_garbage_reports_update_failed() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "ChatGPT.app",
        "com.openai.codex",
        "26.928.31416",
    );
    let fake = Fake::new(|argv| {
        if argv[0] == "/usr/bin/hdiutil" {
            return Ok("hdiutil: this is not a plist".into());
        }
        Ok(String::new())
    });
    fake.serve(mac::CODEX_DMG, Ok(b"dmg".to_vec()));
    let cx = context(root.path(), Platform::Mac, fake);
    let value = update_desktop(&cx, &mac_install("codex-desktop", &bundle, "26.928.31416")).await;
    expect(
        &value,
        "failed",
        "update_failed",
        Some("26.928.31416"),
        false,
    );
}

#[tokio::test]
async fn legacy_restart_marker_is_dropped() {
    let root = temp();
    let bundle = mac_bundle(
        &root.path().join("Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.0.0",
    );
    let fake = Fake::silent();
    fake.serve(mac::CLAUDE_DARWIN_FEED, claude_feed("2.0.0", "2.0.0"));
    let cx = context(root.path(), Platform::Mac, fake);
    let marker = restart_marker(&cx, "claude-desktop");
    crate::apps::updater::common::write_private_json(&marker, &json!({"version": "2.0.0"}))
        .expect("fixture");
    let value = update_desktop(&cx, &mac_install("claude-desktop", &bundle, "2.0.0")).await;
    assert_eq!(value.status, Status::Current);
    assert!(!marker.exists());
}

#[tokio::test]
async fn mac_detection_reads_the_bundle_and_refuses_a_foreign_one() {
    let root = temp();
    let system = root.path().join("system/Applications");
    let cx = context(root.path(), Platform::Mac, Fake::silent());
    assert!(detect_desktop(&cx, "claude-desktop").await.is_none());
    mac_bundle(&system, "Claude.app", "com.impostor", "9.0.0");
    let found = detect_desktop(&cx, "claude-desktop")
        .await
        .expect("fixture");
    assert_eq!((found.probe, found.version), (Probe::Failed, None));
    std::fs::remove_dir_all(system.join("Claude.app")).expect("fixture");
    let bundle = mac_bundle(
        &cx.host.home.join("Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.19675.0",
    );
    let found = detect_desktop(&cx, "claude-desktop")
        .await
        .expect("fixture");
    assert_eq!(found.version.as_deref(), Some("2.19675.0"));
    assert_eq!(found.package_root.as_deref(), Some(bundle.as_path()));
    assert_eq!(found.manager, "official-download");
}

// ------------------------------------------------------------------ Ubuntu (apt)

const APT_POLICY_VM: &str = "chatgpt:\n  Installed: 26.930.51102\n  Candidate: 26.930.51102\n  Version table:\n *** 26.930.51102 100\n        100 /var/lib/dpkg/status\n";
const APT_POLICY_NAS1: &str = "chatgpt:\n  Installed: 26.1007.21434\n  Candidate: 26.1007.21434\n  Version table:\n *** 26.1007.21434 500\n        500 https://persistent.oaistatic.com/codex-app-prod/linux/deb stable/main amd64 Packages\n        100 /var/lib/dpkg/status\n";

fn apt_install(app_id: &'static str, version: &str) -> Installed {
    let (name, path) = if app_id == "codex-desktop" {
        ("chatgpt", "/usr/lib/chatgpt/ChatGPT")
    } else {
        ("claude-desktop", "/usr/lib/claude-desktop/claude-desktop")
    };
    let mut install = Installed::new(
        app_id,
        Platform::Ubuntu,
        PathBuf::from(path),
        Some(version.into()),
        "apt",
    );
    install.identity = Some(name.into());
    install
}

#[test]
fn apt_sources_come_only_from_enabled_repositories() {
    assert!(linux::apt_package_sources(APT_POLICY_VM).is_empty());
    assert_eq!(
        linux::apt_package_sources(APT_POLICY_NAS1),
        ["https://persistent.oaistatic.com/codex-app-prod/linux/deb"]
    );
    // A version row whose version is all digits must not read as a "500" source.
    assert!(linux::apt_package_sources("     2026 500\n").is_empty());
}

#[tokio::test]
async fn untrusted_apt_origin_never_stops_or_installs() {
    let root = temp();
    let fake = Fake::new(|argv| {
        Ok(if argv.iter().any(|arg| arg == "policy") {
            "Candidate: 2.0.0\n Version table:\n     2.0.0 500\n        500 https://impostor.test/deb stable/main amd64 Packages\n".into()
        } else if argv.iter().any(|arg| arg == "madison") {
            "chatgpt | 2.0.0 | https://impostor.test/deb stable/main amd64 Packages".into()
        } else {
            String::new()
        })
    });
    let cx = context(root.path(), Platform::Ubuntu, fake.clone());
    let value = update_desktop(&cx, &apt_install("codex-desktop", "1.0.0")).await;
    assert_eq!(value.message_code(), "signature_failed");
    assert_eq!(*fake.scans.lock().expect("fixture"), 0);
    assert!(!fake.ran("--only-upgrade"));
    assert!(linux::trusted_codex_candidate(
        "chatgpt | 2.0.0 | https://persistent.oaistatic.com/codex-app-prod/linux/deb stable/main amd64 Packages",
        "2.0.0"
    ));
}

#[tokio::test]
async fn disabled_apt_source_reports_source_disabled_and_never_reports_current() {
    let root = temp();
    let fake = Fake::new(|argv| {
        Ok(if argv.iter().any(|arg| arg == "policy") {
            APT_POLICY_VM.into()
        } else {
            String::new()
        })
    });
    let cx = context(root.path(), Platform::Ubuntu, fake.clone());
    let value = update_desktop(&cx, &apt_install("codex-desktop", "26.930.51102")).await;
    expect(
        &value,
        "action_required",
        "source_disabled",
        Some("26.930.51102"),
        false,
    );
    assert!(!fake.ran("madison") && !fake.ran("install"));
}

#[tokio::test]
async fn enabled_apt_source_with_the_installed_copy_as_candidate_is_current() {
    let root = temp();
    let fake = Fake::new(|argv| {
        Ok(if argv.iter().any(|arg| arg == "policy") {
            APT_POLICY_NAS1.into()
        } else {
            String::new()
        })
    });
    let cx = context(root.path(), Platform::Ubuntu, fake);
    let value = update_desktop(&cx, &apt_install("claude-desktop", "26.1007.21434")).await;
    expect(&value, "current", "current", Some("26.1007.21434"), false);
}

#[tokio::test]
async fn claude_desktop_apt_upgrade_reports_the_new_version() {
    let root = temp();
    let executable = root
        .path()
        .join("system/usr/lib/claude-desktop/claude-desktop");
    std::fs::create_dir_all(executable.parent().expect("fixture")).expect("fixture");
    std::fs::write(&executable, b"elf").expect("fixture");
    let installed = Arc::new(Mutex::new("1.0.0".to_owned()));
    let seen = installed.clone();
    let fake = Fake::new(move |argv| {
        if argv.iter().any(|arg| arg == "policy") {
            return Ok("claude-desktop:\n  Installed: 1.0.0\n  Candidate: 2.0.0\n  Version table:\n     2.0.0 500\n        500 https://pkg.claude.test/deb stable/main amd64 Packages\n".into());
        }
        if argv.iter().any(|arg| arg == "--only-upgrade") {
            *seen.lock().expect("fixture") = "2.0.0".into();
        }
        if argv[0] == "/usr/bin/dpkg-query" {
            return Ok(seen.lock().expect("fixture").clone());
        }
        Ok(String::new())
    });
    let cx = context(root.path(), Platform::Ubuntu, fake.clone());
    let install = detect_desktop(&cx, "claude-desktop")
        .await
        .expect("fixture");
    assert_eq!(install.version.as_deref(), Some("1.0.0"));
    let value = update_desktop(&cx, &install).await;
    expect(&value, "updated", "updated", Some("2.0.0"), true);
    assert_eq!(value.forced_stops, Some(0));
    assert_eq!(*installed.lock().expect("fixture"), "2.0.0");
    assert!(
        fake.runs
            .lock()
            .expect("fixture")
            .iter()
            .any(
                |argv| argv[..3] == ["/usr/bin/sudo", "-n", "/usr/bin/apt-get"]
                    && argv.contains(&"update".to_owned())
            )
    );
}

#[tokio::test]
async fn apt_failure_reports_its_code_and_dpkg_timeout_is_check_timeout() {
    let root = temp();
    let fake = Fake::new(|argv| {
        if argv.iter().any(|arg| arg == "update") {
            return Err(UpdateFailure::new("timeout"));
        }
        Ok(String::new())
    });
    let cx = context(root.path(), Platform::Ubuntu, fake);
    let value = update_desktop(&cx, &apt_install("claude-desktop", "1.0.0")).await;
    expect(&value, "failed", "timeout", Some("1.0.0"), false);
    let executable = root.path().join("system/usr/lib/chatgpt/ChatGPT");
    std::fs::create_dir_all(executable.parent().expect("fixture")).expect("fixture");
    std::fs::write(&executable, b"elf").expect("fixture");
    let cx = context(
        root.path(),
        Platform::Ubuntu,
        Fake::new(|_| Err(UpdateFailure::new("timeout"))),
    );
    let found = detect_desktop(&cx, "codex-desktop").await.expect("fixture");
    assert_eq!(found.probe, Probe::Timeout);
}

// ------------------------------------------------------------------ the whole attempt (red on Unported)

#[tokio::test]
async fn attempt_reports_a_current_mac_claude_desktop() {
    let root = temp();
    mac_bundle(
        &root.path().join("system/Applications"),
        "Claude.app",
        "com.anthropic.claudefordesktop",
        "2.19675.0",
    );
    let fake = Fake::silent();
    fake.serve(
        mac::CLAUDE_DARWIN_FEED,
        claude_feed("2.19675.0", "2.19675.0"),
    );
    let cx = context(root.path(), Platform::Mac, fake);
    let value = attempt(updater("claude-desktop").expect("fixture"), &cx).await;
    expect(&value, "current", "current", Some("2.19675.0"), false);
    assert_eq!(value.manager.as_deref(), Some("official-download"));
}

#[tokio::test]
async fn attempt_reports_an_absent_desktop_as_not_installed() {
    let root = temp();
    for platform in [Platform::Mac, Platform::Ubuntu] {
        for app in ["codex-desktop", "claude-desktop"] {
            let cx = context(root.path(), platform, Fake::silent());
            let value = attempt(updater(app).expect("fixture"), &cx).await;
            assert_eq!(value.status, Status::NotInstalled, "{app} {platform:?}");
        }
    }
    let cx = context(root.path(), Platform::Windows, Fake::silent());
    let value = attempt(updater("codex-desktop").expect("fixture"), &cx).await;
    assert_eq!(value.status, Status::NotInstalled);
}

#[tokio::test]
async fn attempt_reports_a_current_ubuntu_claude_desktop() {
    let root = temp();
    let executable = root
        .path()
        .join("system/usr/lib/claude-desktop/claude-desktop");
    std::fs::create_dir_all(executable.parent().expect("fixture")).expect("fixture");
    std::fs::write(&executable, b"elf").expect("fixture");
    let fake = Fake::new(|argv| {
        Ok(if argv.iter().any(|arg| arg == "policy") {
            APT_POLICY_NAS1
                .replace("chatgpt", "claude-desktop")
                .replace("26.1007.21434", "1.0.0")
        } else if argv[0] == "/usr/bin/dpkg-query" {
            "1.0.0".into()
        } else {
            String::new()
        })
    });
    let cx = context(root.path(), Platform::Ubuntu, fake);
    let value = attempt(updater("claude-desktop").expect("fixture"), &cx).await;
    expect(&value, "current", "current", Some("1.0.0"), false);
}

// ------------------------------------------------------------------ the Mac GUI runner

#[test]
fn launchctl_print_state_and_exit_code() {
    let printed = "gui/501/com.ccs.x = {\n\tactive count = 0\n\tstate = not running\n\tlast exit code = 1\n}\n";
    assert_eq!(
        gui::service_state(printed),
        (Some("not running".into()), Some(1))
    );
    assert_eq!(
        gui::service_state("\tstate = running\n\tlast exit code = (never exited)\n"),
        (Some("running".into()), None)
    );
}

#[test]
fn output_tail_keeps_whole_lines_from_the_end() {
    let root = temp();
    let path = root.path().join("stderr");
    std::fs::write(&path, "first line\nsecond line\nError: keychain locked\n").expect("fixture");
    assert_eq!(
        gui::tail(&path, 1024),
        "first line\nsecond line\nError: keychain locked\n"
    );
    assert_eq!(gui::tail(&path, 30), "Error: keychain locked\n");
    assert_eq!(gui::tail(&root.path().join("missing"), 30), "");
}

/// launchctl answers: the domain exists, bootstrap works, the service prints `printed`.
fn launchd(printed: &'static str, domain: bool, bootout: bool) -> Arc<Fake> {
    Fake::new(move |argv| match argv.get(1).map(String::as_str) {
        Some("print") if argv[2].matches('/').count() == 1 => {
            if domain {
                Ok(String::new())
            } else {
                let mut failure = UpdateFailure::failed();
                failure.output = "Could not find domain for user gui: 501".into();
                Err(failure)
            }
        }
        Some("print") if printed.is_empty() => Err(UpdateFailure::failed()),
        Some("print") => Ok(printed.into()),
        Some("bootout") if !bootout => Err(UpdateFailure::failed()),
        _ => Ok(String::new()),
    })
}

#[tokio::test]
async fn gui_command_waits_for_the_agent_and_records_the_apps_run() {
    let root = temp();
    let fake = launchd("\tstate = not running\n\tlast exit code = 0\n", true, true);
    let cx = context(root.path(), Platform::Mac, fake.clone());
    gui::mac_gui_command(&cx, gui::GuiAction::CursorUpdate, Duration::from_secs(5))
        .await
        .expect("fixture");
    let runs = fake.runs.lock().expect("fixture").clone();
    let words: Vec<&str> = runs.iter().map(|argv| argv[1].as_str()).collect();
    assert_eq!(words, ["print", "bootstrap", "print", "bootout"]);
    assert!(runs[1][2].starts_with("gui/") && runs[1][3].ends_with("agent.plist"));
    let row = AppRow::new("cursor-cli", Platform::Mac, Status::Updated).to_value();
    let diagnostics = cx.record(|record| record.export(&row));
    assert_eq!(diagnostics["tool"], "cursor-agent", "{diagnostics}");
    // The private folder is gone.
    assert_eq!(
        std::fs::read_dir(root.path().join("stage"))
            .expect("fixture")
            .count(),
        0
    );
}

#[tokio::test]
async fn gui_command_names_a_failed_exit_a_lost_session_and_a_failed_cleanup() {
    let root = temp();
    let cx = context(
        root.path(),
        Platform::Mac,
        launchd("\tstate = exited\n\tlast exit code = 1\n", true, true),
    );
    let failure = gui::mac_gui_command(&cx, gui::GuiAction::MuseWarm, Duration::from_secs(5))
        .await
        .expect_err("fixture");
    assert_eq!(
        (failure.code, failure.exit_code),
        ("update_failed", Some(1))
    );
    let cx = context(root.path(), Platform::Mac, launchd("", false, true));
    let failure = gui::mac_gui_command(&cx, gui::GuiAction::MuseWarm, Duration::from_secs(5))
        .await
        .expect_err("fixture");
    assert_eq!(failure.code, "mac_gui_unavailable");
    // The service vanished and the GUI domain with it: the session is gone.
    let fake = Fake::new({
        let calls = Mutex::new(0);
        move |argv| {
            if argv[1] == "print" && argv[2].matches('/').count() == 2 {
                return Err(UpdateFailure::failed());
            }
            if argv[1] == "print" {
                let mut calls = calls.lock().expect("fixture");
                *calls += 1;
                if *calls > 1 {
                    let mut failure = UpdateFailure::failed();
                    failure.output = "Could not find domain".into();
                    return Err(failure);
                }
            }
            Ok(String::new())
        }
    });
    let cx = context(root.path(), Platform::Mac, fake);
    let failure = gui::mac_gui_command(&cx, gui::GuiAction::MuseWarm, Duration::from_secs(5))
        .await
        .expect_err("fixture");
    assert_eq!(failure.code, "mac_gui_unavailable");
    let cx = context(
        root.path(),
        Platform::Mac,
        launchd("\tstate = not running\n\tlast exit code = 0\n", true, false),
    );
    let failure = gui::mac_gui_command(&cx, gui::GuiAction::CursorUpdate, Duration::from_secs(5))
        .await
        .expect_err("fixture");
    assert_eq!(failure.code, "mac_gui_cleanup_failed");
    let cx = context(
        root.path(),
        Platform::Mac,
        launchd("\tstate = running\n", true, true),
    );
    let failure = gui::mac_gui_command(
        &cx,
        gui::GuiAction::CursorUpdate,
        Duration::from_millis(600),
    )
    .await
    .expect_err("fixture");
    assert_eq!(failure.code, "timeout");
}

// ------------------------------------------------------------------ the Codex bridge (execute)

/// The bridge's TypeScript `fixture()`: a CLI at 1.0.0 whose update installs 2.0.0, and a
/// runtime that is busy twice before it stops.
struct Bridge {
    clock: u64,
    version: String,
    updates_to: Option<String>,
    update_fails: bool,
    events: Vec<&'static str>,
    marker: Option<Map<String, Value>>,
    busy: u32,
    stop_fails: bool,
    start_fails: bool,
    old_proxies: bool,
    /// What stop() closes and how many of the desktops come back after start().
    planned: RestartPlan,
    back: usize,
    daemon_up: bool,
    launched: u32,
}

impl Bridge {
    fn new() -> Self {
        Self {
            clock: 0,
            version: "1.0.0".into(),
            updates_to: Some("2.0.0".into()),
            update_fails: false,
            events: Vec::new(),
            marker: None,
            busy: 2,
            stop_fails: false,
            start_fails: false,
            old_proxies: false,
            planned: RestartPlan::default(),
            back: 0,
            daemon_up: false,
            launched: 0,
        }
    }

    fn idle(mut self, planned: RestartPlan, back: usize, daemon_up: bool) -> Self {
        self.busy = 0;
        self.planned = planned;
        self.back = back;
        self.daemon_up = daemon_up;
        self
    }
}

const DAEMON: Identity = Identity {
    pid: 1,
    start_time: String::new(),
};

impl Deps for Bridge {
    fn now(&self) -> u64 {
        self.clock
    }
    async fn sleep(&mut self, millis: u64) {
        self.clock += millis;
        self.events.push("wait");
    }
    async fn read_version(&mut self) -> Result<String, ()> {
        Ok(self.version.clone())
    }
    async fn update(&mut self) -> Result<(), ()> {
        self.events.push("update");
        if let Some(version) = self.updates_to.clone() {
            self.version = version;
        }
        if self.update_fails {
            return Err(());
        }
        Ok(())
    }
    fn marked(&self) -> Option<Map<String, Value>> {
        self.marker.clone()
    }
    fn change(&mut self, change: &MarkerChange) -> bool {
        match change {
            MarkerChange::Keep => {}
            MarkerChange::Clear => {
                self.marker = None;
                self.events.push("clear");
            }
            MarkerChange::Write(value) => {
                self.marker = value.as_object().cloned();
                self.events.push("mark");
            }
        }
        true
    }
    async fn restart_proxies(&mut self) -> Result<u32, ()> {
        self.events.push("proxy-reconnect");
        Ok(0)
    }
    async fn has_old_proxies(&mut self) -> (bool, u32) {
        (self.old_proxies, 0)
    }
    async fn stop(
        &mut self,
        record: &(dyn Fn(RestartPlan) -> MarkerChange + Sync),
    ) -> Result<(), StopError> {
        self.events.push("stop");
        if self.busy > 0 {
            self.busy -= 1;
            return Err(StopError::Busy);
        }
        let change = record(self.planned);
        self.change(&change);
        if self.stop_fails {
            return Err(StopError::Failed);
        }
        Ok(())
    }
    fn stop_plan(&self) -> RestartPlan {
        self.planned
    }
    async fn retired_plan(&mut self) -> Result<RestartPlan, ()> {
        Ok(RestartPlan {
            daemon: self.planned.daemon,
            desktops: self.planned.desktops.saturating_sub(self.back),
        })
    }
    async fn restart_state(&mut self) -> Result<RestartState, ()> {
        Ok(RestartState {
            daemon: self.daemon_up.then_some(DAEMON),
            proxy: false,
        })
    }
    async fn verify_running_daemon(&mut self) -> Result<Option<Identity>, ()> {
        Ok(Some(DAEMON))
    }
    async fn start(&mut self) -> Result<(), ()> {
        self.events.push("start");
        if self.start_fails {
            return Err(());
        }
        self.launched = u32::from(self.planned.daemon) + u32::try_from(self.back).expect("fixture");
        Ok(())
    }
    async fn dispose(&mut self) {
        self.events.push("dispose");
    }
    fn launches(&self) -> u32 {
        self.launched
    }
}

fn request(mode: BridgeMode, expected: Option<&str>, restart_only: bool) -> Request {
    Request {
        mode,
        expected: expected.map(str::to_owned),
        restart_only,
        seconds: 900,
        idle_seconds: 60,
        settle_ms: 10_000,
    }
}

fn cli() -> Request {
    request(BridgeMode::Cli, None, false)
}

#[tokio::test]
async fn bridge_queues_busy_work_then_reconnects_proxies_under_stop_start_protection() {
    let mut deps = Bridge::new();
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!(output.status, Status::Updated);
    assert_eq!(
        deps.events,
        [
            "update",
            "stop",
            "wait",
            "stop",
            "wait",
            "stop",
            "proxy-reconnect",
            "start",
            "dispose"
        ]
    );
    assert_eq!(deps.marker, None);
    let row = output.row(BridgeMode::Cli).to_value();
    assert_eq!(row["appId"], "codex-cli");
    assert_eq!(row["manager"], "native");
    assert_eq!(row["forcedStops"], 0);
}

#[tokio::test]
async fn bridge_says_the_restart_failed_when_start_fails() {
    let mut deps = Bridge::new();
    deps.start_fails = true;
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!(
        (output.status, output.code),
        (Status::RestartFailed, "restart_failed")
    );
    assert_eq!(deps.marker, None);
}

#[tokio::test]
async fn bridge_does_not_stop_a_current_package() {
    let mut deps = Bridge::new();
    deps.updates_to = None;
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!((output.status, output.code), (Status::Current, "current"));
    assert_eq!(deps.events, ["update"]);
}

#[tokio::test]
async fn bridge_gives_up_on_a_codex_that_stays_busy_after_about_a_minute() {
    let mut deps = Bridge::new();
    deps.busy = u32::MAX;
    let output = bridge::execute(&mut deps, &cli()).await;
    assert!((55_000..=60_000).contains(&deps.clock), "{}", deps.clock);
    assert!(deps.events.iter().filter(|event| **event == "stop").count() <= 14);
    assert_eq!(
        (output.status, output.code),
        (Status::ActionRequired, "codex_busy")
    );
    assert_eq!(output.version.as_deref(), Some("2.0.0"));
    assert!(!deps.events.contains(&"start"));
    let mut deps = Bridge::new();
    deps.busy = u32::MAX;
    let mut short = cli();
    short.idle_seconds = 10;
    let output = bridge::execute(&mut deps, &short).await;
    assert!(deps.clock <= 10_000);
    assert_eq!(output.code, "codex_busy");
}

#[tokio::test]
async fn bridge_reports_an_unreplaced_proxy_as_a_restart_failure() {
    let mut deps = Bridge::new();
    deps.old_proxies = true;
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!(output.status, Status::RestartFailed);
}

#[tokio::test]
async fn bridge_cli_restarts_the_daemon_or_says_it_did_not_come_up() {
    let planned = RestartPlan {
        daemon: true,
        desktops: 0,
    };
    let mut deps = Bridge::new().idle(planned, 0, true);
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!(
        (
            output.status,
            output.previous.as_deref(),
            output.version.as_deref(),
            output.restarted
        ),
        (Status::Updated, Some("1.0.0"), Some("2.0.0"), 1)
    );
    let mut deps = Bridge::new().idle(planned, 0, false);
    let output = bridge::execute(&mut deps, &cli()).await;
    assert_eq!(
        (output.status, output.code),
        (Status::RestartFailed, "restart_failed")
    );
    assert_eq!(deps.marker, None);
    // A failed updater stops nothing and names the version installed now.
    for (left, shown) in [(None, "1.0.0"), (Some("codex-cli 1.5.0"), "1.5.0")] {
        let mut deps = Bridge::new().idle(planned, 0, true);
        deps.version = "codex-cli 1.0.0".into();
        deps.update_fails = true;
        deps.updates_to = left.map(str::to_owned);
        let output = bridge::execute(&mut deps, &cli()).await;
        assert_eq!(
            (output.status, output.code, output.version.as_deref()),
            (Status::Failed, "update_failed", Some(shown))
        );
        assert!(!deps.events.contains(&"stop"));
    }
}

fn desktop(expected: &str) -> Request {
    request(BridgeMode::Desktop, Some(expected), false)
}

#[tokio::test]
async fn bridge_desktop_with_unchanged_bits_is_not_updated_and_owes_nothing() {
    let mut deps = Bridge::new().idle(RestartPlan::default(), 0, false);
    deps.updates_to = None;
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_eq!(output.status, Status::Failed);
    assert_eq!(deps.marker, None);
    // A changed version that is not the requested candidate is never "updated".
    let mut deps = Bridge::new().idle(RestartPlan::default(), 0, false);
    let output = bridge::execute(&mut deps, &desktop("3.0.0")).await;
    assert_ne!(output.status, Status::Updated);
}

#[tokio::test]
async fn bridge_desktop_recovers_a_pending_restart_without_another_version_change() {
    let mut deps = Bridge::new().idle(RestartPlan::default(), 0, true);
    deps.marker = json!({"version": "1.0.0", "daemon": true})
        .as_object()
        .cloned();
    let output = bridge::execute(
        &mut deps,
        &request(BridgeMode::Desktop, Some("1.0.0"), true),
    )
    .await;
    assert_eq!(output.status, Status::Updated);
    assert!(!deps.events.contains(&"update"));
    assert_eq!(deps.marker, None);
}

#[tokio::test]
async fn bridge_desktop_records_desktops_before_stop_and_relaunches_them() {
    let planned = RestartPlan {
        daemon: false,
        desktops: 1,
    };
    let mut deps = Bridge::new().idle(planned, 1, false);
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_eq!(output.status, Status::Updated);
    assert_eq!(deps.marker, None);
    assert_eq!(output.restarted, 1);
    // The first write happens inside stop(), with the install target.
    assert_eq!(deps.events[..2], ["stop", "mark"]);
    let plan = RestartPlan {
        daemon: true,
        desktops: 1,
    };
    let change = bridge::marker_change(&desktop("2.0.0"), Some("1.0.0"), plan.desktops, true);
    assert_eq!(
        change,
        MarkerChange::Write(
            json!({"version": "1.0.0", "desktop": true, "desktops": 1, "target": "2.0.0"})
        )
    );
    assert_eq!(
        bridge::marker_change(&cli(), Some("1.0.0"), 1, true),
        MarkerChange::Keep
    );
}

#[tokio::test]
async fn bridge_desktop_owes_the_desktops_start_did_not_see_come_up() {
    let mut deps = Bridge::new().idle(
        RestartPlan {
            daemon: false,
            desktops: 2,
        },
        1,
        false,
    );
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_eq!(output.status, Status::RestartFailed);
    assert_eq!(
        deps.marker,
        json!({"version": "2.0.0", "desktop": true, "desktops": 1})
            .as_object()
            .cloned()
    );
}

#[tokio::test]
async fn bridge_desktop_never_clears_what_an_earlier_run_owes_and_says_so_once() {
    for marker in [
        json!({"desktop": true, "desktops": 2}),
        json!({"desktop": true}),
        json!({}),
    ] {
        let mut deps = Bridge::new().idle(
            RestartPlan {
                daemon: false,
                desktops: 1,
            },
            1,
            false,
        );
        let mut value = json!({"version": "1.0.0"});
        value
            .as_object_mut()
            .expect("fixture")
            .extend(marker.as_object().expect("fixture").clone());
        deps.marker = value.as_object().cloned();
        let output = bridge::execute(
            &mut deps,
            &request(BridgeMode::Desktop, Some("1.0.0"), true),
        )
        .await;
        assert_eq!(
            (output.status, output.code),
            (Status::RestartFailed, "restart_not_running"),
            "{marker}"
        );
        assert_eq!(deps.marker, None);
    }
}

#[tokio::test]
async fn bridge_desktop_failed_install_owes_only_what_stayed_closed() {
    let planned = RestartPlan {
        daemon: false,
        desktops: 1,
    };
    let mut deps = Bridge::new().idle(planned, 1, false);
    deps.update_fails = true;
    deps.updates_to = None;
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_eq!(output.status, Status::Failed);
    assert_eq!(deps.marker, None);
    let mut deps = Bridge::new().idle(planned, 0, false);
    deps.update_fails = true;
    deps.updates_to = None;
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_eq!(output.status, Status::RestartFailed);
    assert_eq!(
        deps.marker,
        json!({"version": "1.0.0", "desktop": true, "desktops": 1, "target": "2.0.0"})
            .as_object()
            .cloned()
    );
}

#[tokio::test]
async fn bridge_desktop_records_what_a_failed_stop_closed_and_did_not_bring_back() {
    let mut deps = Bridge::new().idle(
        RestartPlan {
            daemon: false,
            desktops: 2,
        },
        1,
        false,
    );
    deps.stop_fails = true;
    let output = bridge::execute(&mut deps, &desktop("2.0.0")).await;
    assert_ne!(output.status, Status::Updated);
    assert_eq!(
        deps.marker,
        json!({"version": "1.0.0", "desktop": true, "desktops": 1, "target": "2.0.0"})
            .as_object()
            .cloned()
    );
}

#[test]
fn bridge_versions_markers_and_owed_desktops() {
    assert_eq!(
        bridge::version(Some("codex-cli 0.48.0-alpha.2")).as_deref(),
        Some("0.48.0")
    );
    assert_eq!(
        bridge::full_version(Some("codex-cli 0.48.0-alpha.2")).as_deref(),
        Some("0.48.0-alpha.2")
    );
    assert_eq!(bridge::version(None), None);
    let marker = |value: Value| value.as_object().cloned();
    assert_eq!(
        bridge::desktops_owed(marker(json!({"desktops": ["a", "b", 3]})).as_ref(), false),
        2
    );
    assert_eq!(
        bridge::desktops_owed(marker(json!({"desktops": 3})).as_ref(), false),
        3
    );
    assert_eq!(
        bridge::desktops_owed(marker(json!({"desktop": true})).as_ref(), false),
        1
    );
    assert_eq!(
        bridge::desktops_owed(marker(json!({"desktops": 0})).as_ref(), true),
        1
    );
    assert_eq!(bridge::desktops_owed(None, false), 0);
    assert!(bridge::marker_matches(
        marker(json!({"target": "26.1.2"})).as_ref(),
        Some("26.1.2-x")
    ));
    assert!(!bridge::marker_matches(
        marker(json!({"version": "26.1.2"})).as_ref(),
        None
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn bridge_lock_is_exclusive_and_a_stale_one_is_taken_over() {
    let root = temp();
    let codex = root.path().join(".codex");
    std::fs::create_dir_all(&codex).expect("fixture");
    let held = bridge::ActivationLock::acquire(&codex, 30)
        .await
        .expect("fixture");
    assert!(codex.join(".ccs-activation.lock").is_dir());
    let started = std::time::Instant::now();
    tokio::time::pause();
    let second = bridge::ActivationLock::acquire(&codex, 5).await;
    tokio::time::resume();
    assert!(second.is_none());
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(held);
    assert!(!codex.join(".ccs-activation.lock").exists());
    // A lock left by a bridge that died long ago is stale.
    std::fs::create_dir(codex.join(".ccs-activation.lock")).expect("fixture");
    let old = std::time::SystemTime::now() - Duration::from_secs(600);
    std::fs::File::open(codex.join(".ccs-activation.lock"))
        .expect("fixture")
        .set_modified(old)
        .expect("fixture");
    assert!(bridge::ActivationLock::acquire(&codex, 5).await.is_some());
}
