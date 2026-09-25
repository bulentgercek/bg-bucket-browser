//! Local paths as the frontend writes them: `~` stands for the home directory.

use std::fmt;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

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
