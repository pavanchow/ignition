//! The boot configuration format: a tiny `key = value` text file that names the
//! kernel to load and the options to hand it.
//!
//! Grammar, one directive per line:
//! ```text
//! # comments start with a hash and are ignored
//! kernel  = /boot/vmignition   # required, the image path to load
//! cmdline = quiet loglevel=3   # optional, passed to the kernel
//! timeout = 5                  # optional, whole seconds, defaults to 0
//! ```

use crate::error::{BootError, BootResult};

/// A parsed and validated boot configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootConfig {
    /// Path of the kernel image to load.
    pub kernel: String,
    /// Command line handed to the kernel.
    pub cmdline: String,
    /// Menu timeout in seconds.
    pub timeout: u32,
}

/// Parse UTF-8 configuration text into a validated [`BootConfig`].
pub fn parse_config(text: &str) -> BootResult<BootConfig> {
    let mut kernel: Option<String> = None;
    let mut cmdline = String::new();
    let mut timeout: u32 = 0;

    for (lineno, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| {
            BootError::Config(format!("line {}: expected 'key = value'", lineno + 1))
        })?;
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            return Err(BootError::Config(format!(
                "line {}: key '{key}' has an empty value",
                lineno + 1
            )));
        }
        match key {
            "kernel" => kernel = Some(value.to_string()),
            "cmdline" => cmdline = value.to_string(),
            "timeout" => {
                timeout = value.parse::<u32>().map_err(|_| {
                    BootError::Config(format!("line {}: timeout '{value}' is not a number", lineno + 1))
                })?;
            }
            other => {
                return Err(BootError::Config(format!(
                    "line {}: unknown key '{other}'",
                    lineno + 1
                )));
            }
        }
    }

    let kernel = kernel.ok_or_else(|| BootError::Config("missing required 'kernel' key".into()))?;
    if !kernel.starts_with('/') {
        return Err(BootError::Config(format!(
            "kernel path '{kernel}' must be absolute"
        )));
    }

    Ok(BootConfig { kernel, cmdline, timeout })
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_keys() {
        let c = parse_config("# c\nkernel = /a\ncmdline = x y\ntimeout = 7\n").unwrap();
        assert_eq!(c.kernel, "/a");
        assert_eq!(c.cmdline, "x y");
        assert_eq!(c.timeout, 7);
    }

    #[test]
    fn defaults_optional_keys() {
        let c = parse_config("kernel = /a\n").unwrap();
        assert_eq!(c.cmdline, "");
        assert_eq!(c.timeout, 0);
    }

    #[test]
    fn strips_inline_comments() {
        let c = parse_config("kernel = /a # trailing\n").unwrap();
        assert_eq!(c.kernel, "/a");
    }

    #[test]
    fn rejects_missing_kernel() {
        assert!(matches!(parse_config("timeout = 1\n"), Err(BootError::Config(_))));
    }

    #[test]
    fn rejects_relative_kernel() {
        assert!(matches!(parse_config("kernel = boot\n"), Err(BootError::Config(_))));
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(matches!(parse_config("kernel = /a\nfoo = 1\n"), Err(BootError::Config(_))));
    }

    #[test]
    fn rejects_bad_timeout() {
        assert!(matches!(parse_config("kernel = /a\ntimeout = x\n"), Err(BootError::Config(_))));
    }

    #[test]
    fn rejects_missing_equals() {
        assert!(matches!(parse_config("kernel /a\n"), Err(BootError::Config(_))));
    }
}
