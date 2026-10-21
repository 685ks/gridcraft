//! This fork's project links and notices (About window, Help menu).
//!
//! GridCraft here is Arthavan Projects' fork of storytold/gridcraft. It is not affiliated with or
//! endorsed by ArtCraft and carries none of its names, marks or community links.

/// The fork's source repository.
pub const GITHUB: &str = "https://github.com/685ks/gridcraft";
/// Where to report a problem.
pub const ISSUES: &str = "https://github.com/685ks/gridcraft/issues";
/// The upstream project this fork follows.
pub const UPSTREAM: &str = "https://github.com/storytold/gridcraft";
/// Provenance, shown in the About window. Legal text: the same in every interface language.
pub const FORK_NOTE: &str = "A fork of storytold/gridcraft, not affiliated with or endorsed by ArtCraft.";
/// Shown when the Arthavan Projects launcher started the app (`ARTHAVAN_LAUNCHER=1`). GridCraft
/// has no updater of its own and makes no update requests; the launcher installs and updates it.
pub const LAUNCHER_UPDATES_NOTE: &str = "Updates are managed by Arthavan Projects";

/// Whether the Arthavan Projects launcher started this process (`ARTHAVAN_LAUNCHER=1`).
pub fn launcher_managed() -> bool {
    launcher_managed_from(std::env::var("ARTHAVAN_LAUNCHER").ok().as_deref())
}

fn launcher_managed_from(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_flag_is_exactly_one() {
        assert!(launcher_managed_from(Some("1")));
        assert!(launcher_managed_from(Some(" 1\n")));
        for v in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            assert!(!launcher_managed_from(v), "{v:?}");
        }
    }

    #[test]
    fn links_point_at_this_fork() {
        for url in [GITHUB, ISSUES] {
            assert!(url.starts_with("https://github.com/685ks/gridcraft"), "{url}");
        }
        assert!(FORK_NOTE.contains("not affiliated"));
    }

    /// No ArtCraft community links or sites anywhere in the shipped sources.
    #[test]
    fn no_artcraft_links_in_the_sources() {
        let banned = [concat!("discord", ".gg"), concat!("getart", "craft"), concat!("story", "teller.ai")];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut stack = vec![root.join("crates"), root.join("apps")];
        let mut hits = Vec::new();
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "target") {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == "rs")
                    && let Ok(text) = std::fs::read_to_string(&p)
                {
                    for b in banned {
                        if text.to_ascii_lowercase().contains(b) {
                            hits.push(format!("{}: {b}", p.display()));
                        }
                    }
                }
            }
        }
        assert!(hits.is_empty(), "{hits:#?}");
    }
}
