//! Configuration shared by the daemon, the CLI and `--doctor`.
//!
//! Nothing site-specific is compiled in: the receiver address is deployment
//! configuration and only ever arrives from the environment or from a
//! `raop-sender.env` file next to the executable.

use std::path::PathBuf;

/// Directory holding the running executable, which is also where
/// `raop-sender.env` and `daemon.log` live.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn env_file() -> PathBuf {
    exe_dir().join("raop-sender.env")
}

/// Read `raop-sender.env` next to the executable and export it, without
/// overriding anything already present in the real environment.
///
/// Returns how many settings were applied, so the caller can log it.
pub fn load_env_file() -> usize {
    let Ok(text) = std::fs::read_to_string(env_file()) else {
        return 0;
    };
    let mut applied = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || std::env::var_os(key).is_some() {
                continue;
            }
            // SAFETY: called once at startup, before any thread reads the
            // environment.
            std::env::set_var(key, value);
            applied += 1;
        }
    }
    applied
}

/// `ip:port` receiver address, or `None` when unset.
pub fn host() -> Option<String> {
    std::env::var("RAOP_HOST").ok().filter(|h| !h.is_empty())
}

/// Capture device name substring; defaults to `CABLE`.
pub fn device_filter() -> String {
    std::env::var("RAOP_DEVICE")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "CABLE".to_string())
}

/// Name of the scheduled task and firewall rule created by `install.ps1`.
pub fn task_name() -> String {
    std::env::var("RAOP_TASK_NAME")
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "AirPlay Sender".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_paths_are_next_to_the_executable() {
        let dir = exe_dir();
        assert_eq!(env_file(), dir.join("raop-sender.env"));
    }

    #[test]
    fn host_is_none_when_the_variable_is_absent() {
        // The test harness does not set it; this only asserts the empty-string
        // case is filtered, which is the failure mode of an unexpanded variable.
        std::env::remove_var("RAOP_HOST");
        assert_eq!(host(), None);
    }

    #[test]
    fn device_filter_defaults_to_cable() {
        std::env::remove_var("RAOP_DEVICE");
        assert_eq!(device_filter(), "CABLE");
    }
}
