use super::*;
const BEGIN: &str = "<!-- aac-usage rules: start (managed by `aac-usage install-rules`; edits inside are overwritten) -->";
const END: &str = "<!-- aac-usage rules: end -->";
pub(super) fn text(folder: Option<&Path>) -> String {
    let command = folder.map(|p| {
        p.join(if cfg!(windows) {
            "aac-usage.exe"
        } else {
            "aac-usage"
        })
    });
    let cmd = match command {
        Some(p) if cfg!(windows) => format!("& '{}'`", p.display().to_string().replace('\'', "''")),
        Some(p) => format!("'{}'`", p.display().to_string().replace('\'', "'\\''")),
        None => "aac-usage` (<folder> is the shared folder the dashboard writes to)".into(),
    };
    let entries = folder
        .and_then(|p| load(p, "locations.json").ok())
        .unwrap_or(Value::Null);
    let places = rows(&entries, "locations")
        .iter()
        .take(8)
        .filter_map(|v| {
            let name = v["where"].as_str()?.trim();
            let place = v["path"].as_str()?.trim();
            (!name.is_empty()
                && !place.is_empty()
                && name.chars().chain(place.chars()).all(|c| c >= ' '))
            .then(|| format!("`{place}` on {name}"))
        })
        .collect::<Vec<_>>();
    include_str!("rules.txt")
        .replace("__COMMAND__", &cmd)
        .replace(
            "__LOCATIONS__",
            &if places.is_empty() {
                String::new()
            } else {
                format!(" ({})", places.join(", "))
            },
        )
}
/// Update in place so existing hard links and line endings survive.
pub fn install_block(path: &Path, text: &str) -> Result<&'static str, String> {
    use std::io::Write;
    let old = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let eol = if old.contains("\r\n") { "\r\n" } else { "\n" };
    let block = format!("{BEGIN}\n{}{END}\n", text.replace("\r\n", "\n")).replace('\n', eol);
    let new = if let Some((pre, rest)) = old.split_once(BEGIN) {
        let (_, post) = rest
            .split_once(END)
            .ok_or("an aac-usage rules block has no end marker; fix it by hand")?;
        let post = post.trim_start_matches(['\r', '\n']);
        format!(
            "{pre}{block}{}",
            if post.trim().is_empty() {
                String::new()
            } else {
                format!("{eol}{post}")
            }
        )
    } else {
        format!(
            "{}{block}",
            if old.trim().is_empty() {
                String::new()
            } else {
                format!("{}{eol}{eol}", old.trim_end())
            }
        )
    };
    if old == new {
        return Ok("unchanged");
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(new.as_bytes()).map_err(|e| e.to_string())?;
    Ok("updated")
}
pub(super) fn install(folder: Option<&Path>) -> Result<String, String> {
    use std::fmt::Write;
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("aac-usage: home unavailable")?;
    let mut targets = vec![
        (".claude", "CLAUDE.md", true),
        (".codex", "AGENTS.md", true),
        (".omp", "agent/AGENTS.md", true),
        (".gemini", "config/GEMINI.md", true),
        (".zcode", "AGENTS.md", true),
        (".claude-t3", "CLAUDE.md", true),
        (".codex-t3", "AGENTS.override.md", false),
    ]
    .into_iter()
    .map(|(root, name, create)| (root, home.join(root).join(name), create))
    .collect::<Vec<_>>();
    for (base, name) in [
        (".claude-t3", "CLAUDE.md"),
        (".codex-t3", "AGENTS.override.md"),
    ] {
        if let Ok(entries) = std::fs::read_dir(home.join(base)) {
            let mut children = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p.join(name).is_file())
                .collect::<Vec<_>>();
            children.sort();
            targets.extend(children.into_iter().map(|p| (base, p.join(name), false)));
        }
    }
    let mut out = String::new();
    for (root, path, create) in targets {
        if !home.join(root).is_dir() {
            let _ = writeln!(out, "skip      {} (no {root} here)", path.display());
            continue;
        }
        if !create && !path.is_file() {
            continue;
        }
        match install_block(&path, &text(folder)) {
            Ok(status) => {
                let _ = writeln!(out, "{status:9} {}", path.display());
            }
            Err(e) => {
                let _ = writeln!(out, "error     {} ({e})", path.display());
            }
        }
    }
    out.push_str("Cursor keeps user rules in its settings: paste `aac-usage rules` into Settings > Rules > User Rules.\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_rules_keep_crlf_and_existing_hardlinks() {
        let root = tempfile::tempdir()
            .expect("valid offline reader fixture")
            .keep();
        let path = root.join("rules.md");
        let alias = root.join("alias.md");
        std::fs::write(&path, "Before\r\n").expect("valid offline reader fixture");
        std::fs::hard_link(&path, &alias).expect("valid offline reader fixture");
        assert_eq!(
            install_block(&path, "new\n").expect("valid offline reader fixture"),
            "updated"
        );
        let first = std::fs::read(&alias).expect("valid offline reader fixture");
        assert_eq!(
            std::fs::read(&path).expect("valid offline reader fixture"),
            first
        );
        assert!(first.windows(2).any(|w| w == b"\r\n"));
        assert_eq!(
            install_block(&path, "new\n").expect("valid offline reader fixture"),
            "unchanged"
        );
        std::fs::write(&path, format!("Before\n{BEGIN}\nbroken\n"))
            .expect("valid offline reader fixture");
        assert!(install_block(&path, "new\n").is_err());
    }
}
