//! `%LOCALAPPDATA%\jdrgb\ha.log`. The bridge has no window, so this is where
//! every error goes.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Past this, the log is cut to its newest half.
const MAX_BYTES: u64 = 512 * 1024;

pub struct Log {
    path: Option<PathBuf>,
    /// Also write to stderr — for `--unpublish`, which uninstall.ps1 watches.
    stderr: bool,
    /// The last `once` message, so a connection error repeating every second
    /// is logged once rather than every second.
    last_once: Mutex<Option<String>>,
}

impl Log {
    pub fn open(dir: Option<PathBuf>, stderr: bool) -> Log {
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        Log { path: dir.map(|d| d.join("ha.log")), stderr, last_once: Mutex::new(None) }
    }

    pub fn line(&self, msg: &str) {
        let line = format!("{} {msg}\n", timestamp());
        if self.stderr {
            eprint!("{line}");
        }
        let Some(path) = &self.path else { return };
        if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
            trim(path);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Log `msg` unless it is the same as the previous `once` message.
    pub fn once(&self, msg: &str) {
        let mut last = self.last_once.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_deref() == Some(msg) {
            return;
        }
        *last = Some(msg.to_string());
        drop(last);
        self.line(msg);
    }

    /// Something changed for the better; the next repeat of an old problem is
    /// worth logging again.
    pub fn reset_once(&self) {
        *self.last_once.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Keep the newest half, starting at a line boundary.
fn trim(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let half = text.len() / 2;
    let start = text[half..].find('\n').map_or(text.len(), |i| half + i + 1);
    let _ = std::fs::write(path, &text[start..]);
}

/// `2026-10-29T23:02:05Z`. UTC: `std` has no time zones, and a log read
/// alongside the broker's is easier to line up in UTC anyway.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_390), (2025, 10, 29));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29), "a leap day");
    }

    #[test]
    fn trimming_keeps_the_newest_half_on_a_line_boundary() {
        let dir = std::env::temp_dir().join(format!("jdrgb-ha-log-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ha.log");
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        trim(&path);
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.starts_with("line ") && kept.ends_with("line 99\n"), "{kept:?}");
        assert!(kept.len() < text.len() / 2 + 10);
    }

    #[test]
    fn once_logs_a_repeat_only_after_a_reset() {
        let dir = std::env::temp_dir().join(format!("jdrgb-ha-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(Some(dir.clone()), false);
        log.once("broker refused");
        log.once("broker refused");
        log.reset_once();
        log.once("broker refused");
        let text = std::fs::read_to_string(dir.join("ha.log")).unwrap();
        assert_eq!(text.matches("broker refused").count(), 2);
    }
}
