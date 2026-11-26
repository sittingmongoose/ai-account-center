//! Packaged Leptos payload verification, independent of Node.
use crate::{
    build::{Result, read_json, sha},
    ui,
};
use serde_json::Value;
use std::{collections::BTreeSet, fs, io::Read, path::Path};
fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn hex(value: &Value, n: usize) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn positive(v: &Value) -> bool {
    v.as_u64()
        .is_some_and(|n| n > 0 && n <= 9_007_199_254_740_991)
}
/// Whether a manifest path stays inside its package directory.
pub fn safe_path(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 512
        && !v.starts_with('/')
        && !v.contains(['\\', '\0'])
        && v.split('/').all(|s| !["", ".", ".."].contains(&s))
}
fn gzip(bytes: &[u8]) -> Result<usize> {
    let mut encoder = flate2::Compress::new(flate2::Compression::default(), false);
    let mut compressed = Vec::with_capacity(bytes.len() + bytes.len() / 1000 + 128);
    let status = encoder.compress_vec(bytes, &mut compressed, flate2::FlushCompress::Finish)?;
    if status != flate2::Status::StreamEnd {
        return Err("gzip size calculation did not finish".into());
    }
    Ok(compressed.len() + 18)
}
fn modes(_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = _dir;
        use std::os::unix::fs::PermissionsExt;
        require(
            fs::metadata(dir)?.permissions().mode() & 0o777 == 0o755,
            "Packaged dashboard folders must be 0755 and files 0644.",
        )?;
        for entry in fs::read_dir(dir)? {
            let e = entry?;
            if e.file_type()?.is_dir() {
                modes(&e.path())?;
            } else {
                require(
                    fs::symlink_metadata(e.path())?.permissions().mode() & 0o777 == 0o644,
                    "Packaged dashboard folders must be 0755 and files 0644.",
                )?;
            }
        }
    }
    Ok(())
}
/// Verify the complete packaged Leptos payload and gzip budgets.
pub fn verify(dir: &Path) -> Result<Value> {
    let m = read_json(&dir.join("ui-build-manifest.json"))?;
    let id = m["buildId"].as_str().unwrap_or("");
    let shell = m["shellBuildId"].as_str().unwrap_or("");
    require(
        m["schemaVersion"] == 2
            && m["framework"] == "leptos"
            && m["version"] == ui::LEPTOS_VERSION
            && m["target"] == "wasm32-unknown-unknown"
            && m["entry"] == "app/index.html"
            && hex(&m["source"]["sha256"], 64)
            && positive(&m["source"]["files"])
            && hex(&m["buildId"], 12)
            && hex(&m["shellBuildId"], 12)
            && hex(&m["shell"]["sha256"], 64)
            && positive(&m["shell"]["files"])
            && m["shell"]["sha256"]
                .as_str()
                .is_some_and(|s| s.starts_with(shell))
            && m["wasm"]["path"] == format!("pkg/{id}/aac_web_bg.wasm"),
        "The packaged dashboard must be Leptos 0.9.0-rc1 for WebAssembly.",
    )?;
    for file in [
        "app/index.html",
        "app/app.css",
        "app/MartianMono.ttf",
        "app/OFL-MartianMono.txt",
        &format!("pkg/{id}/aac_web.js"),
    ] {
        require(
            dir.join(file).exists(),
            &format!("Missing browser asset: {file}."),
        )?;
    }
    for f in ["pkg/aac_web.js", "pkg/aac_web_bg.wasm"] {
        require(
            !dir.join(f).exists(),
            "The packaged dashboard must not keep an unversioned WebAssembly runtime.",
        )?;
    }
    let files = ui::list_files(dir)?;
    let retired = ["index.html", "renderer.mjs", "shell-frame.mjs"]
        .into_iter()
        .find(|f| dir.join(f).exists())
        .map(str::to_owned)
        .or_else(|| {
            files
                .iter()
                .find(|f| f.starts_with("pkg/") && f.contains("ccs_account_dashboard"))
                .cloned()
        });
    if let Some(f) = retired {
        return Err(
            format!("The packaged dashboard still carries the retired Slint app: {f}.").into(),
        );
    }
    require(
        dir.join("bridge.js").exists(),
        "Missing browser asset: bridge.js.",
    )?;
    let found = sha(&fs::read(dir.join("bridge.js"))?);
    let pinned = "a38ef887dc3d89c2657765705b4c30503f64ea1d729cc57d20337839b682854c";
    require(
        found == pinned,
        &format!(
            "The packaged bridge.js must be the classic page's upgrade module byte for byte (sha256 {pinned}), not the retired bridge or an edited copy (found {found})."
        ),
    )?;
    let wasm = fs::read(dir.join(format!("pkg/{id}/aac_web_bg.wasm")))?;
    let hash = sha(&wasm);
    require(
        wasm.starts_with(&[0, 97, 115, 109, 1, 0, 0, 0])
            && m["wasm"]["bytes"] == wasm.len()
            && m["wasm"]["sha256"] == hash
            && hash.starts_with(id),
        "The packaged WebAssembly binary does not match its build manifest.",
    )?;
    let page = fs::read_to_string(dir.join("app/index.html"))?;
    require(
        page.contains(&format!("/pkg/{id}/aac_web.js"))
            && page.contains(&format!("/pkg/{id}/aac_web_bg.wasm"))
            && !page.contains("{{PKG}}")
            && !page.contains("{{CSS}}"),
        "The packaged app/index.html must start the versioned WebAssembly runtime.",
    )?;
    for f in ["sw.js", "sw-route.js"] {
        require(
            dir.join(f).exists(),
            &format!("Missing browser asset: {f}."),
        )?;
    }
    let sw = fs::read_to_string(dir.join("sw.js"))?;
    require(
        sw.matches(&format!("'{shell}'")).count() == 1 && !sw.contains("__AAC_BUILD_ID__"),
        "The packaged sw.js must carry the stamped shell build id.",
    )?;
    let web = read_json(&dir.join("manifest.webmanifest")).unwrap_or(Value::Null);
    require(
        web["name"] == "AI Account Center"
            && web["short_name"] == "AAC"
            && web["start_url"] == "/"
            && web["display"] == "standalone"
            && web["icons"].as_array().is_some_and(|a| !a.is_empty()),
        "The packaged manifest.webmanifest is not installable.",
    )?;
    for icon in web["icons"].as_array().ok_or("icons")? {
        let src = icon["src"].as_str().unwrap_or("");
        let rel = src.trim_start_matches('/');
        require(
            icon["sizes"].as_str().is_some_and(|s| !s.is_empty()) && safe_path(rel),
            "The packaged manifest.webmanifest names an icon that is not served.",
        )?;
        require(
            dir.join(rel).exists(),
            &format!("Missing browser asset: {src}."),
        )?;
    }
    let originals: Vec<String> = files
        .iter()
        .filter(|f| !f.ends_with(".br") && !f.ends_with(".gz"))
        .cloned()
        .collect();
    let expected = ui::precache_list(&originals, &fs::read(dir.join("app/app.css"))?);
    let marker = "var PRECACHE = ";
    let stamped = sw
        .find(marker)
        .and_then(|i| sw[i + marker.len()..].split(';').next())
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());
    require(
        stamped.as_ref() == Some(&expected),
        "The packaged sw.js must download the whole app before it replaces an installed copy.",
    )?;
    let mismatch = "A precompressed dashboard file does not match its build manifest.";
    let mut listed = BTreeSet::new();
    for e in m["precompressed"].as_array().ok_or(mismatch)? {
        let suffix = match e["encoding"].as_str() {
            Some("br") => ".br",
            Some("gzip") => ".gz",
            _ => return Err(mismatch.into()),
        };
        let path = e["path"].as_str().unwrap_or("");
        let file = e["file"].as_str().unwrap_or("");
        require(
            safe_path(path)
                && file == format!("{path}{suffix}")
                && positive(&e["bytes"])
                && hex(&e["sha256"], 64)
                && listed.insert(file.to_string()),
            mismatch,
        )?;
        let variant = fs::read(dir.join(file))?;
        let original = fs::read(dir.join(path))?;
        let mut decoded = Vec::new();
        let result = if suffix == ".gz" {
            flate2::read::GzDecoder::new(&variant[..]).read_to_end(&mut decoded)
        } else {
            brotli_decompressor::Decompressor::new(&variant[..], 4096).read_to_end(&mut decoded)
        };
        require(
            result.is_ok()
                && decoded == original
                && variant.len() < original.len()
                && e["bytes"] == variant.len()
                && e["sha256"] == sha(&variant),
            mismatch,
        )?;
    }
    for f in &files {
        require(
            !(f.ends_with(".br") || f.ends_with(".gz")) || listed.contains(f),
            mismatch,
        )?;
    }
    modes(dir)?;
    for (f, max) in [
        ("app/index.html".to_owned(), 64 * 1024),
        ("app/app.css".into(), 256 * 1024),
        ("app/MartianMono.ttf".into(), 128 * 1024),
        (format!("pkg/{id}/aac_web.js"), 512 * 1024),
        (format!("pkg/{id}/aac_web_bg.wasm"), 3 * 1024 * 1024),
    ] {
        let n = gzip(&fs::read(dir.join(&f))?)?;
        require(
            n <= max,
            &format!("Bundle entry too large: {f} {n} bytes gzipped (max: {max} bytes)."),
        )?;
    }
    let total = originals
        .iter()
        .try_fold(0usize, |sum, f| -> Result<usize> {
            Ok(sum + gzip(&fs::read(dir.join(f))?)?)
        })?;
    require(
        total <= 4 * 1024 * 1024,
        &format!(
            "Bundle too large: {:.1}KB gzipped (max: 4096KB).",
            total as f64 / 1024.0
        ),
    )?;
    Ok(serde_json::json!({"totalSize":total,"manifest":m}))
}
