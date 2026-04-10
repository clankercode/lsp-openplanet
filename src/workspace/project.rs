use std::path::{Path, PathBuf};

/// Find the workspace root by walking up from the given path to find info.toml.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        if current.join("info.toml").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Discover all .as source files under the workspace root.
pub fn discover_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    discover_recursive(root, &mut files);
    files.sort();
    files
}

fn discover_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            discover_recursive(&path, files);
        } else if path.extension().map_or(false, |ext| ext == "as") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_workspace_root() {
        // Uses a real plugin directory if available
        let counter = PathBuf::from(env!("HOME")).join("src/openplanet/tm-counter");
        if counter.exists() {
            let root = find_workspace_root(&counter.join("src/Main.as"));
            assert_eq!(root, Some(counter.clone()));
        }
    }
}
