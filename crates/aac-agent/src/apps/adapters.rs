//! Resolve the adapter copies T3 actually runs before considering default install roots.
use super::*;
use std::cmp::Ordering;

const PACKAGES: [(&str, &str, &[&str]); 2] = [
    ("muse-acp", "@brokkai/muse-acp", &["muse-acp"]),
    (
        "zcode-acp-server",
        "zcode-acp-server",
        &["zcode-acp-server", "zcode-acp"],
    ),
];

static PART: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?\z")
        .expect("ACP adapter release pattern")
});

pub(super) fn detect(host: &Host) -> Option<Install> {
    let default = match host.platform {
        Platform::Windows => host.roaming.join("npm/node_modules"),
        Platform::Mac => host.system("/opt/homebrew/lib/node_modules"),
        Platform::Ubuntu => host.home.join(".local/lib/node_modules"),
    };
    let mut candidates = vec![default.clone()];
    let suffix = if host.platform == Platform::Windows {
        "node_modules"
    } else {
        "lib/node_modules"
    };
    if let Some(prefix) = npmrc_prefix(host) {
        candidates.push(prefix.join(suffix));
    }
    if let Some(node) = host.which("node") {
        let native = real(&node);
        let prefix = if host.platform == Platform::Windows {
            native.parent()
        } else {
            native.parent().and_then(Path::parent)
        };
        if let Some(prefix) = prefix {
            candidates.push(prefix.join(suffix));
        }
    }
    if host.platform == Platform::Windows {
        candidates.push(host.program_files.join("nodejs/node_modules"));
    } else {
        candidates.extend([
            host.home.join(".local/lib/node_modules"),
            host.home.join(".npm-global/lib/node_modules"),
            host.system("/opt/homebrew/lib/node_modules"),
            host.system("/usr/local/lib/node_modules"),
            host.system("/usr/lib/node_modules"),
        ]);
    }
    candidates.extend([
        host.home.join(".bun/install/global/node_modules"),
        host.home.join(".local/share/pnpm/global/5/node_modules"),
        host.home.join("Library/pnpm/global/5/node_modules"),
        host.home.join(".config/yarn/global/node_modules"),
    ]);
    if host.platform == Platform::Windows {
        candidates.push(host.local.join("pnpm/global/5/node_modules"));
    }
    let path = host.effective_path();
    let on_path: [Option<PathBuf>; 2] =
        std::array::from_fn(|index| path_consumer(host, &path, index));
    let mut owners = configured_consumers(host, &on_path, &default);
    for (index, owner) in owners.iter_mut().enumerate() {
        if owner.is_empty() {
            owner.extend(on_path[index].clone().or_else(|| {
                candidates
                    .iter()
                    .find(|root| manifest(root, index).is_file())
                    .cloned()
            }));
        }
    }
    if owners.iter().all(Vec::is_empty) {
        return None;
    }
    let mut parts = serde_json::Map::new();
    for (index, sources) in owners.iter().enumerate() {
        let part = sources
            .iter()
            .map(|root| {
                let info = read_json(&manifest(root, index), 1024 * 1024)?;
                if info["name"] != PACKAGES[index].1 {
                    return None;
                }
                checked_version(info["version"].as_str(), &PART, 64)
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|versions| versions.into_iter().min_by(|a, b| compare_versions(a, b)));
        parts.insert(PACKAGES[index].0.into(), json!(part));
    }
    Some(Install {
        parts: Some(Value::Object(parts)),
        ..Install::new("npm", None)
    })
}

fn npmrc_prefix(host: &Host) -> Option<PathBuf> {
    let from_file;
    let prefix = if let Some(prefix) = &host.npm_prefix {
        prefix.to_str()?
    } else {
        from_file = read_text(&host.home.join(".npmrc"), 1024 * 1024)?;
        from_file
            .lines()
            .take(200)
            .filter_map(|line| line.split_once('='))
            .find_map(|(key, text)| {
                (key.trim() == "prefix" && !text.trim().is_empty()).then(|| text.trim())
            })?
    };
    if let Some(relative) = prefix.strip_prefix("~/") {
        return Some(host.home.join(relative));
    }
    let path = PathBuf::from(prefix);
    path.is_absolute().then_some(path)
}

fn manifest(root: &Path, index: usize) -> PathBuf {
    root.join(PACKAGES[index].1).join("package.json")
}

fn path_consumer(host: &Host, path: &[PathBuf], index: usize) -> Option<PathBuf> {
    let suffixes: &[&str] = if host.platform == Platform::Windows {
        &[".cmd", ".ps1", ""]
    } else {
        &[""]
    };
    for folder in path {
        for stem in PACKAGES[index].2 {
            for suffix in suffixes {
                let candidate = folder.join(format!("{stem}{suffix}"));
                if candidate.is_file()
                    && let Some((modules, adapter)) = package_in(&candidate)
                    && adapter == index
                {
                    return Some(modules);
                }
            }
        }
    }
    None
}

fn configured_consumers(
    host: &Host,
    on_path: &[Option<PathBuf>; 2],
    default: &Path,
) -> [Vec<PathBuf>; 2] {
    let mut owners = [Vec::new(), Vec::new()];
    let Some(settings) = read_json(
        &host.home.join(".t3/userdata/settings.json"),
        4 * 1024 * 1024,
    ) else {
        return owners;
    };
    let Some(instances) = settings["providerInstances"].as_object() else {
        return owners;
    };
    for instance in instances
        .values()
        .filter(|instance| instance["enabled"] != false)
    {
        let config = &instance["config"];
        let command = config["commandPath"].as_str();
        if let Some(cmd) = command.filter(|cmd| !cmd.contains(['/', '\\']))
            && let Some(stem) = Path::new(cmd).file_stem().and_then(OsStr::to_str)
        {
            for (index, (_, _, bins)) in PACKAGES.iter().enumerate() {
                if bins.contains(&stem.to_ascii_lowercase().as_str()) {
                    owners[index].push(
                        on_path[index]
                            .clone()
                            .unwrap_or_else(|| default.to_path_buf()),
                    );
                }
            }
        }
        let extra = config["commandArgs"]
            .as_array()
            .into_iter()
            .flatten()
            .take(16)
            .filter_map(Value::as_str);
        for text in command
            .into_iter()
            .chain(extra)
            .filter(|text| text.len() <= 1024 && text.contains(['/', '\\']))
        {
            if let Some((modules, index)) = package_in(Path::new(text)) {
                owners[index].push(modules);
            }
        }
    }
    owners
}

fn package_in(path: &Path) -> Option<(PathBuf, usize)> {
    let lexical = aac_files::path::python::normpath(path);
    modules_of(&lexical)
        .or_else(|| modules_of(&real(path)))
        .or_else(|| {
            let suffix = path
                .extension()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !matches!(suffix.as_str(), "" | "cmd" | "ps1" | "exe") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
            let modules = path.parent()?.join("node_modules");
            let index = PACKAGES
                .iter()
                .position(|(_, _, bins)| bins.contains(&stem.as_str()))?;
            manifest(&modules, index)
                .is_file()
                .then_some((modules, index))
        })
}

fn modules_of(path: &Path) -> Option<(PathBuf, usize)> {
    let components: Vec<_> = path.components().collect();
    let store = components
        .iter()
        .position(|part| part.as_os_str() == ".pnpm");
    for (pos, part) in components.iter().enumerate().rev() {
        if part.as_os_str() != "node_modules" {
            continue;
        }
        let tail: PathBuf = components[pos + 1..].iter().collect();
        if let Some(index) = PACKAGES
            .iter()
            .position(|(_, package, _)| tail.starts_with(package))
        {
            let end = if let Some(store) = store {
                if store == 0 || store >= pos || components[store - 1].as_os_str() != "node_modules"
                {
                    return None;
                }
                store
            } else {
                pos + 1
            };
            return Some((components[..end].iter().collect(), index));
        }
    }
    None
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    fn split(version: &str) -> (&str, Option<&str>) {
        version
            .split_once('-')
            .map_or((version, None), |(core, pre)| (core, Some(pre)))
    }
    fn digits(text: &str) -> (usize, &str) {
        let text = text.trim_start_matches('0');
        (text.len(), text)
    }
    fn identifier(text: &str) -> (bool, (usize, &str)) {
        if !text.is_empty() && text.bytes().all(|ch| ch.is_ascii_digit()) {
            (false, digits(text))
        } else {
            (true, (0, text))
        }
    }
    let (a, a_pre) = split(a);
    let (b, b_pre) = split(b);
    a.split('.')
        .map(digits)
        .cmp(b.split('.').map(digits))
        .then_with(|| match (a_pre, b_pre) {
            (Some(a), Some(b)) => a
                .split('.')
                .map(identifier)
                .cmp(b.split('.').map(identifier)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
}
