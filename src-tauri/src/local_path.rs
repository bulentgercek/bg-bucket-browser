//! Local paths as the frontend writes them: `~` stands for the home directory.

use std::fmt;
use std::path::{Path, PathBuf};

/// The home directory is unknown or empty, so `~` has nothing to expand to.
#[derive(Debug)]
pub struct HomeNotFound;

impl fmt::Display for HomeNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("home directory not found")
    }
}

impl From<HomeNotFound> for String {
    fn from(e: HomeNotFound) -> Self {
        e.to_string()
    }
}

/// The current user's home directory.
pub fn home() -> Result<PathBuf, HomeNotFound> {
    std::env::home_dir()
        .filter(|h| !h.as_os_str().is_empty())
        .ok_or(HomeNotFound)
}

/// Resolves a pane path to a real one: `""` and `"~"` are the home directory,
/// `~/…` lies under it, and anything else is used as given.
/// The path is not trimmed: a name ending in a space is a different file.
pub fn resolve_local(path: &str) -> Result<PathBuf, HomeNotFound> {
    let p = path;
    if p.is_empty() || p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return Ok(home()?.join(rest));
    }
    Ok(PathBuf::from(p))
}

/// The way a pane writes a real path: the reverse of `resolve_local`. A path
/// under the home directory becomes `~/…` with `/` between its parts; any
/// other path is returned as it came.
///
/// A folder handed over from outside (a file manager's "open in", a command
/// line) arrives as a real path, and the same folder reached from inside the
/// app is written with `~`: without this the two are different places to
/// everything that remembers a path.
pub fn pane_path(real: &str) -> String {
    let Ok(home) = home() else {
        return real.to_string();
    };
    // By component, so a neighbour named like the home folder is not under it.
    let Ok(rest) = Path::new(real).strip_prefix(&home) else {
        return real.to_string();
    };
    let parts: Vec<_> = rest.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    if parts.is_empty() { "~".to_string() } else { format!("~/{}", parts.join("/")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_path_under_home_is_written_the_way_a_pane_writes_it() {
        let home = home().unwrap();
        let h = home.to_string_lossy();
        assert_eq!(pane_path(&h), "~");
        assert_eq!(pane_path(&home.join("Pictures").join("2026").to_string_lossy()), "~/Pictures/2026");
        assert_eq!(pane_path(&format!("{h}/Pictures/")), "~/Pictures");
        // What a pane path resolves to comes back as that pane path.
        assert_eq!(pane_path(&resolve_local("~/a b/c").unwrap().to_string_lossy()), "~/a b/c");
    }

    #[test]
    fn a_path_outside_home_is_left_as_it_is() {
        let h = home().unwrap().to_string_lossy().into_owned();
        // A neighbour whose name only starts like the home folder's.
        assert_eq!(pane_path(&format!("{h}-backup/x")), format!("{h}-backup/x"));
        assert_eq!(pane_path("/"), "/");
        assert_eq!(pane_path(""), "");
    }

    #[test]
    fn empty_and_tilde_are_home() {
        let h = home().unwrap();
        for p in ["", "~"] {
            assert_eq!(resolve_local(p).unwrap(), h, "{p:?}");
        }
    }

    #[test]
    fn tilde_prefix_expands_under_home() {
        let h = home().unwrap();
        assert_eq!(resolve_local("~/Downloads").unwrap(), h.join("Downloads"));
        assert_eq!(resolve_local("~/a/b c").unwrap(), h.join("a/b c"));
    }

    #[test]
    fn other_paths_are_used_as_given() {
        for p in ["/tmp", "/tmp/~/x", "~user", "relative/dir"] {
            assert_eq!(resolve_local(p).unwrap(), PathBuf::from(p), "{p:?}");
        }
    }
}
