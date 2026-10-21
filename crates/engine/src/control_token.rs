//! The control channel's per-launch bearer token.
//!
//! The desktop app's loopback control port (`gridcraft --control PORT`) drives the whole app,
//! files included, so it only serves clients that prove they can read the current user's
//! settings directory. Each launch makes a random 256-bit token, writes it as 64 hex digits to
//! `<settings dir>/control-<PORT>.token` (mode 0600 on Unix; on Windows the file inherits the
//! per-user `%APPDATA%` ACL) and removes the file on exit. A client sends it as the `token` field
//! of its first request on a connection (`{"id":1,"method":"…","token":"<hex>"}`), or reads it
//! from [`ENV`] instead of the file. [`lookup`] is what `gridcraft-cli` and the MCP client use.

use std::path::{Path, PathBuf};

/// Overrides the token file for clients (scripts, tests, a sandboxed agent given the token).
pub const ENV: &str = "GRIDCRAFT_CONTROL_TOKEN";

/// Length of a token in hex digits (256 bits).
pub const HEX_LEN: usize = 64;

/// The per-user settings directory (`ui.json`, `logs/` and the token files), the same folder the
/// desktop app uses.
pub fn settings_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/GridCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("GridCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("gridcraft"))
    }
}

/// Where the token for `port` lives inside `dir`.
pub fn token_path(dir: &Path, port: u16) -> PathBuf {
    dir.join(format!("control-{port}.token"))
}

/// 32 random bytes as 64 lowercase hex digits.
pub fn to_hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(HEX_LEN), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A well-formed token: exactly [`HEX_LEN`] hex digits.
pub fn is_well_formed(token: &str) -> bool {
    token.len() == HEX_LEN && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Write `token` to `path`, readable and writable by the owner only. The file is written beside
/// its final name and renamed into place, so a reader never sees half a token and a file another
/// user created there first is replaced rather than reused.
pub fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("token.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The token stored at `path`, if the file holds a well-formed one.
pub fn read_token(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim();
    is_well_formed(token).then(|| token.to_ascii_lowercase())
}

/// The token a client should send to the app on `port`: [`ENV`] when set, else the app's file.
pub fn lookup(port: u16) -> Option<String> {
    if let Some(t) = std::env::var(ENV).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        return Some(t);
    }
    read_token(&token_path(&settings_dir()?, port))
}

/// Compare a presented token with the expected one in time that does not depend on where they
/// first differ.
pub fn matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn hex_is_64_lowercase_digits() {
        let t = to_hex(&[0xab; 32]);
        assert_eq!(t.len(), HEX_LEN);
        assert!(is_well_formed(&t));
        assert_eq!(&t[..4], "abab");
        assert!(!is_well_formed("abc"));
        assert!(!is_well_formed(&"g".repeat(64)));
    }

    #[test]
    fn matches_needs_the_exact_token() {
        let t = to_hex(&[7; 32]);
        assert!(matches(&t, &t));
        assert!(!matches(&t, &t[..63]));
        assert!(!matches(&t, &to_hex(&[8; 32])));
        assert!(!matches("", ""));
    }

    #[test]
    fn token_file_round_trips_and_is_private() {
        let dir = std::env::temp_dir().join(format!("gridcraft-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = token_path(&dir, 4242);
        let t = to_hex(&[0x5a; 32]);
        write_token(&path, &t).unwrap();
        assert_eq!(read_token(&path).as_deref(), Some(t.as_str()));
        // A second launch replaces the token.
        let t2 = to_hex(&[0x11; 32]);
        write_token(&path, &t2).unwrap();
        assert_eq!(read_token(&path).as_deref(), Some(t2.as_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "token file mode {mode:o}");
        }
        std::fs::write(&path, "not a token").unwrap();
        assert_eq!(read_token(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
