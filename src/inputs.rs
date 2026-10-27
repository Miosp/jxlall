//! Input discovery: explicit files plus directory scans behind an
//! extension filter.

use std::collections::HashSet;
use std::path::PathBuf;

/// One convertible input. `root` is the directory argument it was found
/// under (None for explicit files); `-o` mirrors paths relative to it.
pub struct InputFile {
    pub path: PathBuf,
    pub root: Option<PathBuf>,
}

/// Collect convertible files. Directories are scanned (one level unless
/// `recursive`); explicit files always pass regardless of extension.
/// Sorted and deduplicated on the canonical path. Also returns the number
/// of inputs that could not be read (reported, counted as failures).
pub fn collect(inputs: &[PathBuf], recursive: bool, pattern: &str) -> (Vec<InputFile>, u64) {
    let exts = parse_pattern(pattern);
    let mut files = Vec::new();
    let mut errors = 0u64;
    for input in inputs {
        let meta = std::fs::metadata(input);
        if meta.as_ref().is_ok_and(|m| m.is_dir()) {
            let mut walker = ignore::WalkBuilder::new(input);
            // Photos are not source code: no .gitignore/.ignore/global
            // excludes. Hidden entries stay skipped: AppleDouble `._*`
            // files and `.thumbnails/` caches are not photos to convert.
            walker.standard_filters(false).hidden(true);
            if !recursive {
                walker.max_depth(Some(1));
            }
            for entry in walker.build() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        eprintln!("error: scanning {}: {e}", input.display());
                        errors += 1;
                        continue;
                    }
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let p = entry.into_path();
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_ascii_lowercase()))
                    .unwrap_or_default();
                if exts.contains(&ext) {
                    files.push(InputFile {
                        path: p,
                        root: Some(input.clone()),
                    });
                }
            }
        } else if input.is_file() {
            files.push(InputFile {
                path: input.clone(),
                root: None,
            });
        } else {
            eprintln!("error: skipping inaccessible {}", input.display());
            errors += 1;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    // `. b.png` yields `./b.png` and `b.png`: one file, two spellings.
    let mut seen = HashSet::new();
    files
        .retain(|f| seen.insert(std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())));
    (files, errors)
}

/// `|`-separated extension list into lowercase `.ext` entries; `jpg`,
/// `.jpg` and `*.jpg` are equivalent.
fn parse_pattern(pattern: &str) -> HashSet<String> {
    pattern
        .split('|')
        .map(|p| {
            p.trim()
                .trim_start_matches('*')
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|p| !p.is_empty())
        .map(|p| format!(".{p}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Fresh scratch directory per test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "photo-convert-inputs-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn pattern_accepts_bare_and_glob_extensions() {
        let exts = parse_pattern("jpg| *.PNG |.Tif||");
        let want: HashSet<String> = [".jpg", ".png", ".tif"].map(String::from).into();
        assert_eq!(exts, want);
    }

    #[test]
    fn scan_matches_extension_case_insensitively() {
        let dir = scratch("case");
        touch(&dir.join("A.JPG"));
        let (files, errors) = collect(std::slice::from_ref(&dir), false, "jpg");
        assert_eq!((files.len(), errors), (1, 0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn same_file_via_dir_and_explicit_path_is_collected_once() {
        let dir = scratch("dedup");
        std::fs::create_dir(dir.join("sub")).unwrap();
        touch(&dir.join("b.png"));
        let other_spelling = dir.join("sub").join("..").join("b.png");
        let (files, _) = collect(&[dir.clone(), other_spelling], false, ".png");
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignore_files_do_not_hide_photos() {
        let dir = scratch("ignore");
        std::fs::write(dir.join(".ignore"), "*.png\n").unwrap();
        touch(&dir.join("a.png"));
        let (files, _) = collect(std::slice::from_ref(&dir), false, ".png");
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hidden_files_and_dirs_are_skipped() {
        let dir = scratch("hidden");
        std::fs::create_dir(dir.join(".thumbnails")).unwrap();
        touch(&dir.join(".thumbnails").join("a.png"));
        touch(&dir.join("._a.png"));
        touch(&dir.join("a.png"));
        let (files, _) = collect(std::slice::from_ref(&dir), true, ".png");
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_input_counts_as_error() {
        let dir = scratch("missing");
        touch(&dir.join("a.png"));
        let (files, errors) = collect(&[dir.join("a.png"), dir.join("nope.png")], false, ".png");
        assert_eq!((files.len(), errors), (1, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subdirectory_counts_as_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("unreadable");
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root bypasses permission bits; nothing to prove then.
        let readable = std::fs::read_dir(&locked).is_ok();
        let (_, errors) = collect(std::slice::from_ref(&dir), true, ".png");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        if !readable {
            assert_eq!(errors, 1);
        }
    }
}
