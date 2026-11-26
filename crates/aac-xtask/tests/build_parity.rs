//! Independent Node-oracle checks; only fixture data is written.
use aac_xtask::{
    build::{self, Kind},
    ui,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
fn root() -> PathBuf {
    std::env::var_os("AAC_X1_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn node(code: &str, args: &[&str]) -> Value {
    let out = Command::new("node")
        .args(["-e", code])
        .args(args)
        .current_dir(root())
        .output()
        .expect("node oracle");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("oracle JSON")
}
#[test]
fn native_source_hashes_match_node() {
    for (kind, file) in [
        (Kind::Agent, "agent"),
        (Kind::Usage, "usage-scan"),
        (Kind::Cursor, "cursor-account"),
    ] {
        let expected = node(
            "console.log(JSON.stringify(require('./scripts/build-'+process.argv[1]+'.js').sourceHash()))",
            &[file],
        );
        assert_eq!(
            json!(build::source_hash(&root(), kind).expect("source hash")),
            expected
        );
    }
}
#[test]
fn ui_source_shell_and_precache_match_node() {
    let r = root();
    let id = "0123456789ab";
    let expected = node(
        "const b=require('./scripts/build-ui');const p=require('./scripts/ui-precache');const a=b.stagedAppFiles('web-dashboard/leptos','0123456789ab');console.log(JSON.stringify({source:b.leptosFingerprint('web-dashboard/leptos'),shell:b.shellFingerprint('web-dashboard/public',a),app:[...a].map(([p,b])=>[p,b.toString('base64')]),precache:p.precacheList(['sw.js','sw-route.js','app/index.html','app/app.css','a.mjs','b.d.ts','z.txt'],a.get('app/app.css'))}))",
        &[],
    );
    let app = ui::staged_app_files(&r.join("web-dashboard/leptos"), id).expect("app");
    assert_eq!(
        ui::leptos_fingerprint(&r).expect("fingerprint"),
        expected["source"]
    );
    assert_eq!(
        ui::shell_fingerprint(&r.join("web-dashboard/public"), &app).expect("shell"),
        expected["shell"]
    );
    let paths = [
        "sw.js",
        "sw-route.js",
        "app/index.html",
        "app/app.css",
        "a.mjs",
        "b.d.ts",
        "z.txt",
    ]
    .map(str::to_string);
    assert_eq!(
        json!(ui::precache_list(
            &paths,
            app.get("app/app.css").expect("css")
        )),
        expected["precache"]
    );
}
#[test]
fn stamping_and_wasm_strip_match_node() {
    let source = "var BUILD = '__AAC_BUILD_ID__'; var PRECACHE = [/*__AAC_PRECACHE__*/];";
    let list = vec!["/".into(), "/a$&".into()];
    let expected = node(
        "console.log(JSON.stringify(require('./scripts/build-ui').stampedSwSource(process.argv[1],'abcd',['/','/a$&'])))",
        &[source],
    );
    assert_eq!(
        json!(ui::stamped_sw(source, "abcd", Some(&list)).expect("stamp")),
        expected
    );
    assert!(ui::stamped_sw("__AAC_BUILD_ID____AAC_BUILD_ID__", "x", None).is_err());
    let wasm = vec![
        0, 97, 115, 109, 1, 0, 0, 0, 0, 5, 4, b'n', b'a', b'm', b'e', 0, 2, 1, b'x',
    ];
    let oracle = node(
        "console.log(JSON.stringify([...require('./web-dashboard/leptos/build/strip-names.cjs').stripNameSection(Buffer.from(JSON.parse(process.argv[1])))]))",
        &[&serde_json::to_string(&wasm).expect("json")],
    );
    assert_eq!(json!(ui::strip_names(&wasm).expect("wasm")), oracle);
    assert!(ui::strip_names(&[0, 97, 115, 109, 1, 0, 0, 0, 1, 127]).is_err());
}
#[test]
fn compression_matches_node_bytes() {
    let dir = tempfile::tempdir().expect("temp");
    let bytes = "const repeated = 'abcdefghijklmnopqrstuvwxyz';\n".repeat(100);
    fs::write(dir.path().join("test.js"), &bytes).expect("fixture");
    let entries = ui::precompress_ui(dir.path()).expect("compress");
    let expected = node(
        "const z=require('node:zlib'),c=require('node:crypto');const b=Buffer.from(process.argv[1]);console.log(JSON.stringify([['br',z.brotliCompressSync(b,{params:{[z.constants.BROTLI_PARAM_QUALITY]:11,[z.constants.BROTLI_PARAM_SIZE_HINT]:b.length,[z.constants.BROTLI_PARAM_MODE]:z.constants.BROTLI_MODE_TEXT}})],['gzip',z.gzipSync(b,{level:9})]].map(([encoding,b])=>({encoding,sha256:c.createHash('sha256').update(b).digest('hex'),bytes:b.length}))))",
        &[&bytes],
    );
    for (entry, wanted) in entries.iter().zip(expected.as_array().expect("array")) {
        assert_eq!(entry["sha256"], wanted["sha256"], "{}", entry["encoding"]);
        assert_eq!(entry["bytes"], wanted["bytes"]);
    }
}
#[test]
fn corrupted_catalog_pin_is_rejected() {
    let r = tempfile::tempdir().expect("temp");
    let dir = r.path().join("dist/agent");
    let target = build::TARGETS[0];
    let rel = format!("{target}/aac-agent");
    fs::create_dir_all(dir.join(target)).expect("dirs");
    fs::write(dir.join(&rel), b"binary").expect("binary");
    let hash = build::sha(b"binary");
    build::write_json(&dir.join("manifest.json"),&json!({"name":"aac-agent","version":"0.1.0","targets":{target:{"file":rel,"sha256":hash,"size":6}}})).expect("manifest");
    build::write_json(&r.path().join("config/agent-artifacts.json"),&json!({"artifacts":[{"target":target,"file":format!("dist/agent/{rel}"),"sha256":hash,"version":"0.1.0"}]})).expect("valid catalog");
    assert!(build::verify_dist(r.path(), Kind::Agent, &dir).is_ok());
    build::write_json(&r.path().join("config/agent-artifacts.json"),&json!({"artifacts":[{"target":target,"file":format!("dist/agent/{rel}"),"sha256":"wrong","version":"0.1.0"}]})).expect("catalog");
    assert!(build::verify_dist(r.path(), Kind::Agent, &dir).is_err());
}
#[test]
fn build_lock_refuses_concurrent_holder() {
    use fs2::FileExt;
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("lock");
    let held = fs::File::create(&path).expect("lock");
    held.lock_exclusive().expect("acquire");
    assert!(ui::with_build_lock(&path, std::time::Duration::ZERO, || Ok(())).is_err());
    held.unlock().expect("release");
    assert!(ui::with_build_lock(&path, std::time::Duration::ZERO, || Ok(())).is_ok());
}
#[test]
fn source_hash_normalizes_crlf_but_ui_hash_does_not() {
    let dir = tempfile::tempdir().expect("temp");
    let crate_dir = dir.path().join("native/aac-usage");
    fs::create_dir_all(crate_dir.join(".cargo")).expect("config");
    fs::create_dir_all(crate_dir.join("src")).expect("src");
    for p in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "src/main.rs",
    ] {
        fs::write(crate_dir.join(p), b"a\r\nb\r\n").expect("write");
    }
    let before = build::source_hash(dir.path(), Kind::Usage).expect("hash");
    let raw = ui::fingerprint_inputs(&crate_dir, &["src".into()]).expect("raw");
    for p in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "src/main.rs",
    ] {
        fs::write(crate_dir.join(p), b"a\nb\n").expect("write");
    }
    assert_eq!(
        before,
        build::source_hash(dir.path(), Kind::Usage).expect("hash")
    );
    assert_ne!(
        raw,
        ui::fingerprint_inputs(&crate_dir, &["src".into()]).expect("raw")
    );
}
#[cfg(unix)]
#[test]
fn ui_fingerprint_rejects_parent_symlink() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("temp");
    fs::create_dir(dir.path().join("real")).expect("real");
    fs::write(dir.path().join("real/config.toml"), b"config").expect("file");
    symlink("real", dir.path().join(".cargo")).expect("link");
    assert!(ui::fingerprint_inputs(dir.path(), &[".cargo/config.toml".into()]).is_err());
}
#[test]
fn whole_ui_package_matches_node() {
    let fixture = tempfile::tempdir().expect("fixture");
    let r = fixture.path();
    let c = r.join("web-dashboard/leptos");
    for p in ["style", "fonts", "pkg"] {
        fs::create_dir_all(c.join(p)).expect("dirs");
    }
    fs::create_dir_all(r.join("web-dashboard/public")).expect("public");
    fs::write(c.join("index.html"),"<link href='{{CSS}}'><script src='{{PKG}}/aac_web.js'></script><p>{{PKG}}/aac_web_bg.wasm</p>").expect("html");
    fs::write(c.join("style/app.css"), "body{color:red;}\n".repeat(100)).expect("css");
    for font in ["MartianMono.ttf", "OFL-MartianMono.txt"] {
        fs::write(c.join("fonts").join(font), font).expect("font");
    }
    fs::write(
        c.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .expect("manifest");
    let mut lock = String::from("version = 4\n");
    for (name, version) in [
        ("leptos", "0.9.0-rc1"),
        ("wasm-bindgen", "0.2.130"),
        ("wasm-bindgen-futures", "0.4.80"),
        ("js-sys", "0.3.107"),
        ("web-sys", "0.3.107"),
    ] {
        lock += &format!("[[package]]\nname='{name}'\nversion='{version}'\n");
    }
    fs::write(c.join("Cargo.lock"), lock).expect("lock");
    fs::write(
        c.join("pkg/aac_web_bg.wasm"),
        [0, 97, 115, 109, 1, 0, 0, 0, 0, 5, 4, b'n', b'a', b'm', b'e'],
    )
    .expect("wasm");
    fs::write(
        c.join("pkg/aac_web.js"),
        "export default function(){};\n".repeat(100),
    )
    .expect("js");
    fs::write(c.join("pkg/package.json"), "{}").expect("skip");
    fs::write(
        r.join("web-dashboard/public/sw.js"),
        "var BUILD='__AAC_BUILD_ID__'; var PRECACHE = [/*__AAC_PRECACHE__*/];",
    )
    .expect("sw");
    let strip = fs::read(root().join("web-dashboard/leptos/build/strip-names.cjs")).expect("strip");
    fs::create_dir_all(c.join("build")).expect("build");
    fs::write(c.join("build/strip-names.cjs"), strip).expect("strip");
    fs::create_dir_all(r.join("web-dashboard/public/a")).expect("nested");
    fs::write(
        r.join("web-dashboard/public/a/asset.js"),
        "nested asset\n".repeat(200),
    )
    .expect("nested asset");
    fs::write(
        r.join("web-dashboard/public/a.js"),
        "sibling asset\n".repeat(200),
    )
    .expect("sibling asset");
    let expected = node(
        "const b=require('./scripts/build-ui'),fs=require('fs'),p=require('path'),crypto=require('crypto');let m=b.buildUi({repoRoot:process.argv[1],run:(cmd)=>cmd.endsWith('rustc')?'rustc 1.99.1':cmd.endsWith('rustup')?'wasm32-unknown-unknown':cmd.endsWith('wasm-pack')?'wasm-pack 0.15.1':'',readCommit:()=>null,lock:{run:()=>'',useFlock:true}});const dir=p.join(process.argv[1],'dist/ui');function walk(base){return fs.readdirSync(p.join(dir,base)).sort().flatMap(f=>{const rel=base?base+'/'+f:f;return fs.statSync(p.join(dir,rel)).isDirectory()?walk(rel):[[rel,crypto.createHash('sha256').update(fs.readFileSync(p.join(dir,rel))).digest('hex')]]})}console.log(JSON.stringify({manifest:m,files:walk('').filter(([p])=>p!=='ui-build-manifest.json')}))",
        &[r.to_str().expect("path")],
    );
    let actual = ui::package_ui(r).expect("package");
    assert_eq!(actual, expected["manifest"]);
    let files = ui::list_files(&r.join("dist/ui"))
        .expect("files")
        .into_iter()
        .filter(|p| p != "ui-build-manifest.json")
        .map(|p| {
            let h = build::sha(&fs::read(r.join("dist/ui").join(&p)).expect("read"));
            json!([p, h])
        })
        .collect::<Vec<_>>();
    assert_eq!(json!(files), expected["files"]);
}
#[test]
fn copy_only_filters_stale_sources_and_mixed_protocols() {
    let fixture = tempfile::tempdir().expect("fixture");
    let r = fixture.path();
    let c = r.join("native/aac-usage");
    fs::create_dir_all(c.join(".cargo")).expect("config");
    fs::create_dir_all(c.join("src")).expect("src");
    for p in ["Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"] {
        fs::write(c.join(p), "").expect("input");
    }
    fs::write(c.join("Cargo.toml"), "[package]\nversion=\"0.1.0\"\n").expect("manifest");
    fs::write(c.join("src/protocol.rs"), "pub const PROTOCOL: u64 = 7;\n").expect("protocol");
    let hash = build::source_hash(r, Kind::Usage).expect("hash");
    for (n, target) in build::TARGETS.iter().enumerate() {
        let bin = Kind::Usage.binary(target);
        let file = c.join("prebuilt").join(target).join(&bin);
        fs::create_dir_all(file.parent().expect("parent")).expect("folder");
        fs::write(&file, b"fixture").expect("binary");
        build::write_json(&file.with_file_name(format!("{bin}.json")),&json!({"sourceSha256":if n==1{"stale"}else{&hash},"sha256":if n==2{"wrong".into()}else{build::sha(b"fixture")},"protocol":if n==3{8}else{7},"size":7})).expect("note");
    }
    fs::create_dir_all(r.join("scripts")).expect("scripts");
    fs::copy(
        root().join("scripts/build-usage-scan.js"),
        r.join("scripts/build-usage-scan.js"),
    )
    .expect("oracle script");
    let expected = node(
        "const p=require('path'),fs=require('fs'); const r=process.argv[1];const o=require('child_process').spawnSync(process.execPath,[p.join(r,'scripts/build-usage-scan.js'),'--copy-only']);if(o.status!==0)throw Error(o.stderr.toString());console.log(JSON.stringify(fs.readFileSync(p.join(r,'dist/native/aac-usage/manifest.json'),'utf8')))",
        &[r.to_str().expect("path")],
    );
    let m = build::copy_only(r, Kind::Usage).expect("stage");
    assert_eq!(
        json!(fs::read_to_string(r.join("dist/native/aac-usage/manifest.json")).expect("manifest")),
        expected
    );

    assert_eq!(m["targets"].as_object().expect("targets").len(), 1);
    assert!(m["targets"][build::TARGETS[0]].is_object());
}
#[test]
fn owner_lock_reclaims_stale_and_preserves_live_owner() {
    let dir = tempfile::tempdir().expect("temp");
    let file = dir.path().join("owner");
    fs::write(&file, b"invalid").expect("stale");
    let owner = ui::try_owner_file(&file, 1)
        .expect("owner")
        .expect("acquired");
    assert!(ui::try_owner_file(&file, 2).expect("check").is_none());
    drop(owner);
    assert!(!file.exists());
    assert!(ui::owner_is_stale(br#"{"pid":1,"acquiredAt":0}"#, 10800001));
}
#[test]
fn pinned_lock_accepts_current_versions() {
    ui::assert_pins(&root().join("web-dashboard/leptos")).expect("current pins");
    for kind in [Kind::Agent, Kind::Usage, Kind::Cursor] {
        assert!(build::version(&root(), kind).expect("version").is_string());
    }
}
#[test]
fn dashboard_aliases_and_package_metadata_match_node() {
    let fixture = tempfile::tempdir().expect("fixture");
    let artifacts = fixture.path().join("artifacts");
    let package = fixture.path().join("package");
    fs::create_dir_all(&artifacts).expect("artifacts");
    let ext = if cfg!(windows) { ".exe" } else { "" };
    for stem in ["aac-http", "ccs"] {
        fs::write(artifacts.join(format!("{stem}{ext}")), stem).expect("binary");
    }
    let rust = package.join("dist/native");
    build::stage_dashboard(&rust, &artifacts).expect("stage");
    let expected = node(
        "const b=require('./scripts/build-dashboard-native'),fs=require('fs'),p=require('path');b.stageDashboard(process.argv[1],process.argv[2]);console.log(JSON.stringify(fs.readdirSync(process.argv[1]).sort().map(n=>[n,fs.readFileSync(p.join(process.argv[1],n),'utf8')])))",
        &[
            fixture.path().join("oracle").to_str().expect("oracle"),
            artifacts.to_str().expect("path"),
        ],
    );
    let mut files = fs::read_dir(&rust)
        .expect("files")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    files.sort();
    let rows = files
        .iter()
        .map(|n| json!([n, fs::read_to_string(rust.join(n)).expect("bytes")]))
        .collect::<Vec<_>>();
    assert_eq!(json!(rows), expected);
    build::write_json(
        &package.join("package.json"),
        &json!({"name":"fixture","bin":{}}),
    )
    .expect("package");
    build::stage_manifest(&package).expect("metadata");
    let actual = build::read_json(&package.join("package.json")).expect("read");
    let expected = node(
        "const b=require('./scripts/build-dashboard-native'),fs=require('fs'),p=require('path');b.stageManifest(process.argv[1]);console.log(fs.readFileSync(p.join(process.argv[1],'package.json'),'utf8'))",
        &[package.to_str().expect("package")],
    );
    assert_eq!(actual, expected);
    fs::create_dir(package.join(".git")).expect("git");
    assert!(build::stage_manifest(&package).is_err());
}
