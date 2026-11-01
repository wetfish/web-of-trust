//! Finds irssi log files under one or more log roots.
//!
//! irssi's default `autolog_path = ~/irclogs/$tag/$0.log` writes one file per
//! network tag and target, so each root holds `<tag>/<target>.log`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Whether a log file belongs to a channel or a private message target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Channel,
    Query,
}

impl TargetKind {
    fn of(target: &str) -> Self {
        if target.starts_with(['#', '&', '!', '+']) {
            TargetKind::Channel
        } else {
            TargetKind::Query
        }
    }
}

/// One `<tag>/<target>.log` file.
pub struct LogFile {
    /// Index into `Inventory::roots`.
    pub root: usize,
    pub path: PathBuf,
    pub tag: String,
    pub target: String,
    pub kind: TargetKind,
    pub bytes: u64,
}

/// A path that was found but not read, and why.
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

pub struct Inventory {
    pub roots: Vec<PathBuf>,
    /// Sorted by tag, then target, then root.
    pub files: Vec<LogFile>,
    pub skipped: Vec<Skipped>,
}

impl Inventory {
    fn skip(&mut self, path: PathBuf, reason: impl Into<String>) {
        self.skipped.push(Skipped {
            path,
            reason: reason.into(),
        });
    }
}

/// Lists the log files under each root. A root that cannot be read is an
/// error; anything inside a root that does not fit the layout is skipped.
pub fn discover(roots: &[PathBuf]) -> Result<Inventory, String> {
    let mut inventory = Inventory {
        roots: roots.to_vec(),
        files: Vec::new(),
        skipped: Vec::new(),
    };

    for (root, root_path) in roots.iter().enumerate() {
        let entries = read_dir_sorted(root_path)
            .map_err(|err| format!("cannot read log folder {}: {err}", root_path.display()))?;

        for path in entries {
            match fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => scan_tag_dir(&mut inventory, root, &path),
                Ok(_) => inventory.skip(
                    path,
                    "file directly in a log folder; expected <tag>/<target>.log",
                ),
                Err(err) => inventory.skip(path, format!("cannot read: {err}")),
            }
        }
    }

    inventory
        .files
        .sort_by(|a, b| (&a.tag, &a.target, a.root).cmp(&(&b.tag, &b.target, b.root)));
    Ok(inventory)
}

fn scan_tag_dir(inventory: &mut Inventory, root: usize, dir: &Path) {
    let tag = file_name(dir);

    let entries = match read_dir_sorted(dir) {
        Ok(entries) => entries,
        Err(err) => {
            inventory.skip(dir.to_path_buf(), format!("cannot read folder: {err}"));
            return;
        }
    };

    for path in entries {
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                inventory.skip(path, format!("cannot read: {err}"));
                continue;
            }
        };

        if meta.is_dir() {
            inventory.skip(path, "folder inside a network tag folder");
            continue;
        }

        if path.extension().is_none_or(|ext| ext != "log") {
            inventory.skip(path, "not a .log file");
            continue;
        }

        let target = match path.file_stem() {
            Some(stem) => stem.to_string_lossy().into_owned(),
            None => {
                inventory.skip(path, "no target name in file name");
                continue;
            }
        };

        inventory.files.push(LogFile {
            root,
            path,
            tag: tag.clone(),
            kind: TargetKind::of(&target),
            target,
            bytes: meta.len(),
        });
    }
}

fn read_dir_sorted(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A scratch folder that is removed when the test ends.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("wot-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn target_kind_follows_channel_prefixes() {
        assert_eq!(TargetKind::of("#analysis"), TargetKind::Channel);
        assert_eq!(TargetKind::of("&local"), TargetKind::Channel);
        assert_eq!(TargetKind::of("rachel"), TargetKind::Query);
    }

    #[test]
    fn finds_logs_in_tag_folders_and_skips_the_rest() {
        let root = TempDir::new("layout");
        fs::create_dir_all(root.0.join("wetfish/nested")).unwrap();
        fs::write(root.0.join("wetfish/#analysis.log"), "a\nb\nc").unwrap();
        fs::write(root.0.join("wetfish/rachel.log"), "hello\n").unwrap();
        fs::write(root.0.join("wetfish/notes.txt"), "").unwrap();
        fs::write(root.0.join("stray.log"), "").unwrap();

        let inventory = discover(&[root.0.clone()]).unwrap();

        let found: Vec<(&str, &str, TargetKind, u64)> = inventory
            .files
            .iter()
            .map(|f| (f.tag.as_str(), f.target.as_str(), f.kind, f.bytes))
            .collect();
        assert_eq!(
            found,
            [
                ("wetfish", "#analysis", TargetKind::Channel, 5),
                ("wetfish", "rachel", TargetKind::Query, 6),
            ]
        );
        assert_eq!(
            inventory.files[0].path,
            root.0.join("wetfish/#analysis.log")
        );
        // stray.log, the nested folder and notes.txt
        assert_eq!(inventory.skipped.len(), 3);
    }

    #[test]
    fn missing_root_is_an_error() {
        let root = TempDir::new("missing");
        assert!(discover(&[root.0.join("does-not-exist")]).is_err());
    }
}
