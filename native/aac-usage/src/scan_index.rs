//! Durable, bounded optional-root discovery. Only directory names and traversal progress are
//! saved; log contents, credentials and reader fingerprints never enter this index.
use crate::{discover, fsx, util};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const KINDS: [&str; 4] = ["codex", "claude", "omp", "muse"];

#[derive(Default, Serialize, Deserialize)]
struct Listing {
    stamp: String,
    names: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Frame {
    path: PathBuf,
    depth: usize,
    after: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    home: PathBuf,
    pending: Vec<Frame>,
    listings: BTreeMap<PathBuf, Listing>,
    used: BTreeSet<PathBuf>,
    found: BTreeMap<String, BTreeSet<PathBuf>>,
    failed: BTreeMap<String, BTreeSet<PathBuf>>,
}
fn private_open(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    options.open(path)
}
fn stamp(path: &Path) -> std::io::Result<String> {
    let meta = fsx::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec(),
            util::mtime_ns(&meta)
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(format!("{}:{:?}:{}", util::mtime_ns(&meta), meta.created(), meta.len()))
    }
}
fn excluded(name: &str) -> bool {
    name.starts_with('.') || fsx::skipped(name) || name == "PM-Experiments" || fsx::personal_or_cloud(name)
}
fn within_home(home: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(home) else {
        return false;
    };
    let mut cursor = home.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        cursor.push(name);
        if fsx::is_link(&cursor) || (cursor != path && fsx::exists(&cursor.join(".git"))) {
            return false;
        }
    }
    true
}
impl Index {
    fn failure(&mut self, path: &Path) {
        for kind in KINDS {
            self.failed.entry(kind.into()).or_default().insert(path.into());
        }
    }
    fn walk(&mut self, home: &Path, budget: Duration, cap: usize) {
        let deadline = Instant::now() + budget;
        if self.pending.is_empty() {
            self.found.clear();
            self.failed.clear();
            self.used.clear();
            self.pending.push(Frame {
                path: home.into(),
                depth: 0,
                after: None,
            });
        }
        // A parent already processed earlier in this cycle may have changed between pulls.
        // Requeue that directory so a new entry before the lexical cursor cannot be missed.
        let changed: Vec<_> = self
            .used
            .iter()
            .take_while(|_| Instant::now() < deadline)
            .filter(|path| {
                self.listings
                    .get(*path)
                    .is_some_and(|listing| stamp(path).ok().as_ref() != Some(&listing.stamp))
            })
            .cloned()
            .collect();
        for path in changed {
            let depth = path.strip_prefix(home).map_or(0, |p| p.components().count());
            self.pending.retain(|frame| frame.path != path);
            self.pending.push(Frame {
                path,
                depth,
                after: None,
            });
        }
        let mut entries = 0;
        while entries < cap && Instant::now() < deadline {
            let Some(frame) = self.pending.pop() else { break };
            if !within_home(home, &frame.path) || (frame.depth > 0 && fsx::exists(&frame.path.join(".git"))) {
                continue;
            }
            let current = match stamp(&frame.path) {
                Ok(stamp) => stamp,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    self.failure(&frame.path);
                    continue;
                }
            };
            let cached = self.listings.get(&frame.path).filter(|l| l.stamp == current);
            let after = if cached.is_some() { frame.after.as_ref() } else { None };
            let mut names = match cached {
                Some(listing) if cfg!(unix) => listing.names.clone(),
                _ => match fsx::list_dir(&frame.path) {
                    Ok(names) => names,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        self.failure(&frame.path);
                        continue;
                    }
                },
            };
            names.sort();
            self.used.insert(frame.path.clone());
            // Save the complete listing before cutting its processing. The lexical cursor is
            // stable across process restarts; new names before it are picked up next cycle.
            self.listings.insert(
                frame.path.clone(),
                Listing {
                    stamp: current,
                    names: names.clone(),
                },
            );
            let mut iter = names
                .into_iter()
                .filter(|n| after.is_none_or(|after| n > after))
                .peekable();
            let mut ignored = BTreeSet::new();
            while let Some(name) = iter.next() {
                entries += 1;
                let path = frame.path.join(&name);
                if !excluded(&name) && !fsx::is_link(&path) {
                    match fsx::metadata(&path) {
                        Ok(meta) if meta.is_dir() => {
                            if frame.depth >= 1 && KINDS.contains(&name.as_str()) {
                                let (holds, failed) = discover::holds_logs(&name, &path);
                                if failed {
                                    self.failed.entry(name.clone()).or_default().insert(path.clone());
                                }
                                if holds {
                                    self.found.entry(name.clone()).or_default().insert(path);
                                }
                            } else if frame.depth + 1 < discover::EXPERIMENT_MAX_DEPTH {
                                self.pending.push(Frame {
                                    path,
                                    depth: frame.depth + 1,
                                    after: None,
                                });
                            }
                        }
                        Ok(_) => {
                            ignored.insert(name.clone());
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => self.failure(&path),
                    }
                }
                if excluded(&name) {
                    ignored.insert(name.clone());
                }
                if entries >= cap || Instant::now() >= deadline {
                    if iter.peek().is_some() {
                        self.pending.push(Frame {
                            path: frame.path.clone(),
                            depth: frame.depth,
                            after: Some(name),
                        });
                    }
                    break;
                }
            }
            if let Some(listing) = self.listings.get_mut(&frame.path) {
                listing.names.retain(|name| !ignored.contains(name));
            }
        }
        if self.pending.is_empty() {
            self.listings.retain(|path, _| self.used.contains(path));
        }
    }
    fn result(&self) -> discover::ExperimentRoots {
        let mut result = discover::ExperimentRoots {
            optional_limited: !self.pending.is_empty(),
            found: Default::default(),
            failed: Default::default(),
            limits: Default::default(),
        };
        for kind in KINDS {
            if let Some(paths) = self.found.get(kind) {
                result.found.insert(
                    kind,
                    paths
                        .iter()
                        .filter(|path| within_home(&self.home, path))
                        .take(discover::EXPERIMENT_MAX_ROOTS)
                        .cloned()
                        .collect(),
                );
                if paths.len() > discover::EXPERIMENT_MAX_ROOTS {
                    result
                        .limits
                        .insert(kind, (discover::EXPERIMENT_MAX_ROOTS, Some(paths.len())));
                }
            }
            if let Some(paths) = self.failed.get(kind) {
                result.failed.insert(kind, paths.iter().cloned().collect());
            }
        }
        result
    }
}

pub(crate) fn walk(home: &Path, cache: &Path, budget: Duration, cap: usize) -> discover::ExperimentRoots {
    let attempt = || -> std::io::Result<discover::ExperimentRoots> {
        if fs::symlink_metadata(cache).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(cache)?;
        let lock = private_open(&cache.join("index.lock"))?;
        lock.try_lock().map_err(|_| std::io::ErrorKind::WouldBlock)?;
        let saved = cache.join("index.json");
        let mut bytes = Vec::new();
        private_open(&saved)?.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        let mut index: Index = if bytes.len() as u64 <= MAX_BYTES {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Index::default()
        };
        if index.version != 1 || index.home != home {
            index = Index {
                version: 1,
                home: home.into(),
                ..Index::default()
            };
        }
        index.walk(home, budget, cap);
        let result = index.result();
        let bytes = serde_json::to_vec(&index)?;
        if bytes.len() as u64 <= MAX_BYTES {
            let temp = cache.join("index.next");
            let mut file = private_open(&temp)?;
            file.set_len(0)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(temp, saved)?;
            #[cfg(unix)]
            File::open(cache)?.sync_all()?;
        }
        Ok(result)
    };
    attempt().unwrap_or_else(|_| discover::experiment_roots(home, budget))
}
