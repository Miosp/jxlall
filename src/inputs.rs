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
/// Sorted and deduplicated.
pub fn collect(inputs: &[PathBuf], recursive: bool, pattern: &str) -> Vec<InputFile> {
    let exts: HashSet<String> = pattern
        .split('|')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let mut files = Vec::new();
    for input in inputs {
        let meta = std::fs::metadata(input);
        if meta.as_ref().is_ok_and(|m| m.is_dir()) {
            let mut walker = ignore::WalkBuilder::new(input);
            walker.hidden(false).git_ignore(true);
            if !recursive {
                walker.max_depth(Some(1));
            }
            for entry in walker.build().flatten() {
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
            eprintln!("warning: skipping inaccessible {}", input.display());
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    files
}
