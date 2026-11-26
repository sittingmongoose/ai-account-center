//! Offline release and guard parity against the retained script oracles.
use aac_xtask::{build::sha, bundle, guards, lineage};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("fixture")
}
fn scratch() -> PathBuf {
    tempfile::Builder::new()
        .prefix("aac-x2-")
        .tempdir()
        .expect("fixture")
        .keep()
}
fn node(code: &str, args: &[&str]) -> Value {
    let o = Command::new("node")
        .current_dir(root())
        .arg("-e")
        .arg(code)
        .args(args)
        .output()
        .expect("fixture");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("fixture")
}
#[test]
fn lineage_parser_oracle() {
    for v in [
        Value::Null,
        json!({"version":1,"commit":null}),
        json!({"version":1,"commit":"a".repeat(40),"dirty":false,"onMain":true,"branch":"topic/x","lineage":["a".repeat(40)]}),
        json!({"version":1,"commit":"a".repeat(40),"dirty":false,"lineage":["b".repeat(40)]}),
    ] {
        let input = v.to_string();
        let expected = node(
            "console.log(JSON.stringify(require('./scripts/deploy-lineage').parseBuildInfo(JSON.parse(process.argv[1]))))",
            &[&input],
        );
        assert_eq!(lineage::parse_build(&v).unwrap_or(Value::Null), expected);
    }
}
#[test]
fn lineage_problem_oracle() {
    for dirty in [false, true] {
        for main in [false, true] {
            let v = json!({"commit":"a".repeat(40),"branch":"topic/x","dirty":dirty,"onMain":main,"lineage":["a".repeat(40)]});
            let expected = node(
                "console.log(JSON.stringify(require('./scripts/deploy-lineage').buildProblem(JSON.parse(process.argv[1]))))",
                &[&v.to_string()],
            );
            assert_eq!(
                lineage::build_problem(&v).map_or(Value::Null, Value::String),
                expected
            );
        }
    }
}
#[test]
fn deploy_refusal_and_override_oracle() {
    let dir = scratch();
    let ccs = dir.join("ccs");
    fs::create_dir_all(dir.join("dist")).expect("fixture");
    fs::create_dir(&ccs).expect("fixture");
    for missing in [false, true] {
        for dirty in [false, true] {
            let c = "a".repeat(40);
            let v = json!({"version":1,"commit":c,"branch":"main","dirty":false,"onMain":true,"lineage":if missing{vec!["a".repeat(40)]}else{vec!["a".repeat(40),"b".repeat(40)]}});
            fs::write(dir.join("dist/build-info.json"), v.to_string()).expect("fixture");
            fs::write(
                ccs.join("deployed-build.json"),
                json!({"version":1,"commit":"b".repeat(40),"branch":"fix/retained","dirty":dirty})
                    .to_string(),
            )
            .expect("fixture");
            for override_allowed in [false, true] {
                let expected = node(
                    "console.log(JSON.stringify(require('./scripts/deploy-lineage').checkDeployLineage({packageRoot:process.argv[1],env:{CCS_DIR:process.argv[2],AAC_ALLOW_DEPLOY_REGRESSION:process.argv[3]}})))",
                    &[
                        dir.to_str().expect("fixture"),
                        ccs.to_str().expect("fixture"),
                        if override_allowed { "1" } else { "0" },
                    ],
                );
                assert_eq!(lineage::check(&dir, &ccs, override_allowed), expected);
            }
        }
    }
}
#[test]
fn private_directory_preserves_existing() {
    let dir = scratch();
    let p = dir.join("storage");
    lineage::initialize(&p).expect("fixture");
    fs::write(p.join("keep"), "existing").expect("fixture");
    lineage::initialize(&p).expect("fixture");
    assert_eq!(
        fs::read_to_string(p.join("keep")).expect("fixture"),
        "existing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            fs::metadata(&p).expect("fixture").permissions().mode() & 0o777,
            0o700
        );
        let bad = dir.join("dangling");
        symlink(dir.join("absent"), &bad).expect("fixture");
        assert!(lineage::initialize(&bad).is_err());
        assert!(fs::symlink_metadata(bad).expect("fixture").is_symlink());
    }
}
#[test]
fn personal_scanner_oracle() {
    let text =
        "workMac2 workMac2\n192.168.10.20 ::ffff:c0a8:0a14\nabc192.168.10.20 1.192.168.10.20\n";
    let words: BTreeSet<_> = ["workmac2", "work", "mac", "2"]
        .map(|s| sha(s.as_bytes()))
        .into_iter()
        .collect();
    let ips: BTreeSet<_> = [sha(b"192.168.10")].into_iter().collect();
    let expected = node(
        "const g=require('./scripts/personal-detail-guard');console.log(JSON.stringify(g.scanText(process.argv[1],{words:new Set(JSON.parse(process.argv[2])),ipv4:new Set(JSON.parse(process.argv[3]))})))",
        &[
            text,
            &serde_json::to_string(&words).expect("fixture"),
            &serde_json::to_string(&ips).expect("fixture"),
        ],
    );
    let actual = guards::scan_text(text, &words, &ips);
    assert_eq!(json!(actual), expected);
}
#[test]
fn inventory_matches_node_bytes() {
    let dir = scratch();
    fs::create_dir_all(dir.join("tests/spec")).expect("fixture");
    fs::write(dir.join("package.json"), "{}").expect("fixture");
    for name in ["a.test.js", "auth.spec.ts", "suite.rs", "helper_test.py"] {
        fs::write(dir.join(name), "#[test]\nfetch('/x'); testCases\n").expect("fixture");
    }
    let o = Command::new("git")
        .arg("init")
        .arg(&dir)
        .output()
        .expect("fixture");
    assert!(o.status.success());
    let o = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["add", "."])
        .output()
        .expect("fixture");
    assert!(o.status.success());
    fs::write(
        dir.join("tests/spec/inventory.mjs"),
        fs::read(root().join("tests/spec/inventory.mjs")).expect("fixture"),
    )
    .expect("fixture");
    let o = Command::new("node")
        .arg(dir.join("tests/spec/inventory.mjs"))
        .arg("--regenerate")
        .output()
        .expect("fixture");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (actual, _) = guards::inventory(&dir).expect("fixture");
    assert_eq!(
        actual,
        fs::read_to_string(dir.join("tests/spec/test-inventory.json")).expect("fixture")
    );
}
#[test]
fn bundle_rejects_manifest_and_traversal() {
    let dir = scratch();
    fs::write(dir.join("ui-build-manifest.json"), "{}").expect("fixture");
    let err = bundle::verify(&dir)
        .expect_err("invalid bundle must be rejected")
        .to_string();
    assert_eq!(
        err,
        "The packaged dashboard must be Leptos 0.9.0-rc1 for WebAssembly."
    );
    for bad in ["../x", "/x", "x//y", "x/./y", "x\\y", ""] {
        assert!(!bundle::safe_path(bad));
    }
}
#[test]
fn bundle_complete_fixture_oracle() {
    let dir = scratch();
    let id = sha(&[0, 97, 115, 109, 1, 0, 0, 0]);
    let id = &id[..12];
    let shell = "123456789abc";
    let assets=[("bridge.js",fs::read(root().join("web-dashboard/public/bridge.js")).expect("fixture")),("app/index.html",format!("<html>/pkg/{id}/aac_web.js /pkg/{id}/aac_web_bg.wasm</html>").into_bytes()),("app/app.css",b"body{}".to_vec()),("app/MartianMono.ttf",b"font".to_vec()),("app/OFL-MartianMono.txt",b"license".to_vec()),("sw-route.js",b"route".to_vec()),("icon.png",b"png".to_vec()),("manifest.webmanifest",json!({"name":"AI Account Center","short_name":"AAC","start_url":"/","display":"standalone","icons":[{"src":"/icon.png","sizes":"192x192"}]}).to_string().into_bytes())];
    for (f, b) in assets {
        let p = dir.join(f);
        fs::create_dir_all(p.parent().expect("fixture")).expect("fixture");
        fs::write(p, b).expect("fixture");
    }
    let pkg = dir.join(format!("pkg/{id}"));
    fs::create_dir_all(&pkg).expect("fixture");
    fs::write(pkg.join("aac_web.js"), "runtime").expect("fixture");
    fs::write(pkg.join("aac_web_bg.wasm"), [0, 97, 115, 109, 1, 0, 0, 0]).expect("fixture");
    let m = json!({"schemaVersion":2,"framework":"leptos","version":"0.9.0-rc1","target":"wasm32-unknown-unknown","entry":"app/index.html","source":{"sha256":"a".repeat(64),"files":1},"buildId":id,"shellBuildId":shell,"shell":{"sha256":format!("{shell}{}","a".repeat(52)),"files":1},"wasm":{"path":format!("pkg/{id}/aac_web_bg.wasm"),"bytes":8,"sha256":sha(&[0,97,115,109,1,0,0,0])},"precompressed":[]});
    fs::write(dir.join("ui-build-manifest.json"), m.to_string()).expect("fixture");
    fs::write(dir.join("sw.js"), "").expect("fixture");
    let list = aac_xtask::ui::precache_list(
        &aac_xtask::ui::list_files(&dir).expect("fixture"),
        b"body{}",
    );
    fs::write(
        dir.join("sw.js"),
        format!(
            "var BUILD_ID = '{shell}';\nvar PRECACHE = {};",
            serde_json::to_string(&list).expect("fixture")
        ),
    )
    .expect("fixture");
    fn modes(dir: &Path) {
        aac_xtask::build::mode(dir, 0o755).expect("fixture");
        for e in fs::read_dir(dir).expect("fixture") {
            let p = e.expect("fixture").path();
            if p.is_dir() {
                modes(&p);
            } else {
                aac_xtask::build::mode(&p, 0o644).expect("fixture");
            }
        }
    }
    modes(&dir);
    let expected = node(
        "console.log(JSON.stringify(require('./scripts/verify-bundle').verifyBundle(process.argv[1])))",
        &[dir.to_str().expect("fixture")],
    );
    let actual = bundle::verify(&dir).expect("fixture");
    assert_eq!(actual["manifest"], expected["manifest"]);
    let baseline: Value =
        serde_json::from_str(include_str!("fixtures/bundle-size-baseline.json")).expect("fixture");
    assert_eq!(actual["totalSize"], baseline["rustGzipBytes"]);
    assert_eq!(expected["totalSize"], baseline["legacyNodeGzipBytes"]);
    let mut seed = 123456789u32;
    let css = (0..600_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            b'A' + (seed % 26) as u8
        })
        .collect::<Vec<_>>();
    let original_sw = fs::read(dir.join("sw.js")).expect("fixture");
    fs::write(dir.join("app/app.css"), css).expect("fixture");
    let oversized_css = fs::read(dir.join("app/app.css")).expect("fixture");
    let oversized_list = aac_xtask::ui::precache_list(
        &aac_xtask::ui::list_files(&dir).expect("fixture"),
        &oversized_css,
    );
    fs::write(
        dir.join("sw.js"),
        format!(
            "var BUILD_ID = '{shell}';\nvar PRECACHE = {};",
            serde_json::to_string(&oversized_list).expect("fixture")
        ),
    )
    .expect("fixture");
    assert!(
        bundle::verify(&dir)
            .expect_err("invalid bundle must be rejected")
            .to_string()
            .starts_with("Bundle entry too large: app/app.css")
    );
    fs::write(dir.join("app/app.css"), b"body{}").expect("fixture");
    fs::write(dir.join("sw.js"), original_sw).expect("fixture");
    let variants = aac_xtask::ui::precompress_ui(&dir).expect("fixture");
    assert!(!variants.is_empty());
    let mut compressed_manifest = m.clone();
    compressed_manifest["precompressed"] = json!(variants);
    fs::write(
        dir.join("ui-build-manifest.json"),
        compressed_manifest.to_string(),
    )
    .expect("fixture");
    modes(&dir);
    assert!(bundle::verify(&dir).is_ok());
    let variant = variants[0]["file"].as_str().expect("fixture");
    fs::write(dir.join(variant), b"corrupt").expect("fixture");
    assert_eq!(
        bundle::verify(&dir)
            .expect_err("invalid bundle must be rejected")
            .to_string(),
        "A precompressed dashboard file does not match its build manifest."
    );
    fs::write(pkg.join("aac_web_bg.wasm"), b"corrupt").expect("fixture");
    assert!(bundle::verify(&dir).is_err());
}

#[test]
fn one_file_policy_oracle() {
    let files = json!([
        [
            "crates/x/src/a.rs",
            "fn f() { File::open(p); }\n#[cfg(test)]\nmod tests { fn t() { fs::write(p, br\"\\\"); } }\nfn g() { fs::read(p); }\n"
        ],
        ["crates/aac-files/src/lib.rs", "fs::write(p, b\"x\");"]
    ]);
    let code = "import {scan} from './scripts/foundation/one-file-policy.mjs'; console.log(JSON.stringify(scan(JSON.parse(process.argv[1]),[],{})))";
    let o = Command::new("node")
        .current_dir(root())
        .args(["--input-type=module", "-e", code, &files.to_string()])
        .output()
        .expect("fixture");
    assert!(o.status.success());
    let expected: Value = serde_json::from_slice(&o.stdout).expect("fixture");
    let files: Vec<(String, String)> = serde_json::from_value(files).expect("fixture");
    assert_eq!(
        aac_xtask::file_policy::scan(&files, &json!([]), &json!({})),
        expected
    );
    let op = "fn f() { File::open(p); }\n";
    let receipt = json!({"crates/x/src/a.rs":{"reason":"An explicitly pinned existing file operation with no extra copies.","uses":[format!("File::open:{}",sha(op.trim().as_bytes()))]}});
    let actual = aac_xtask::file_policy::scan(
        &[("crates/x/src/a.rs".into(), op.repeat(2))],
        &json!([]),
        &receipt,
    );
    assert_eq!(actual["violations"].as_array().expect("fixture").len(), 1);
    assert!(actual["stale"].as_array().expect("fixture").is_empty());
}
#[test]
fn quickstart_detects_drift() {
    let dir = scratch();
    fs::create_dir_all(dir.join("docs")).expect("fixture");
    fs::create_dir(dir.join("docker")).expect("fixture");
    let snippet = "<!-- quickstart-snippet-start -->\nexample\n<!-- quickstart-snippet-end -->\n";
    for f in [
        "docs/quickstart-snippet.md",
        "README.md",
        "docker/README.md",
    ] {
        fs::write(dir.join(f), snippet).expect("fixture");
    }
    assert!(guards::quickstart(&dir).is_ok());
    fs::write(dir.join("README.md"), snippet.replace("example", "drift")).expect("fixture");
    assert!(guards::quickstart(&dir).is_err());
}

#[test]
fn repository_file_policy_matches_node() {
    let root = root();
    let mut files = Vec::new();
    for group in ["crates", "native"] {
        let mut crates = fs::read_dir(root.join(group))
            .expect("fixture")
            .map(|e| e.expect("fixture"))
            .collect::<Vec<_>>();
        crates.sort_by_key(|e| e.file_name());
        for c in crates {
            let src = c.path().join("src");
            if src.is_dir() {
                for relative in aac_xtask::ui::list_files(&src).expect("fixture") {
                    if relative.ends_with(".rs") {
                        let file = src.join(&relative);
                        files.push((
                            file.strip_prefix(&root)
                                .expect("fixture")
                                .to_string_lossy()
                                .replace('\\', "/"),
                            fs::read_to_string(file).expect("fixture"),
                        ));
                    }
                }
            }
        }
    }
    let scratch = scratch();
    let input = scratch.join("inputs.json");
    fs::write(&input, serde_json::to_vec(&files).expect("fixture")).expect("fixture");
    let output=Command::new("node").current_dir(&root).args(["--input-type=module","-e","import {scan,ALLOWLIST} from './scripts/foundation/one-file-policy.mjs';import fs from 'node:fs';console.log(JSON.stringify(scan(JSON.parse(fs.readFileSync(process.argv[1])),ALLOWLIST,JSON.parse(fs.readFileSync('scripts/foundation/one-file-cut-receipt.json')))))"]).arg(input).output().expect("fixture");
    assert!(output.status.success());
    let expected: Value = serde_json::from_slice(&output.stdout).expect("fixture");
    let policy: Value =
        serde_json::from_str(include_str!("../src/file-policy.json")).expect("fixture");
    assert_eq!(
        aac_xtask::file_policy::scan(&files, &policy["allowlist"], &policy["receipt"]),
        expected
    );
}
