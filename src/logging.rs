//! Log targets, levels, and the per-match log file (`found-003`).
//!
//! Bevy's [`LogPlugin`](bevy::log::LogPlugin) already bridges `tracing`, so this
//! module does not install a subscriber of its own. It does three things the
//! plan asked for: it names the targets, it states the level policy, and it
//! supplies the extra layer that writes a match to a file.
//!
//! # Targets
//!
//! Every log line in this crate names one of the constants in [`target`], so
//! `RUST_LOG` can turn any subsystem up or down without a rebuild:
//!
//! #+BEGIN_EXAMPLE
//! RUST_LOG=warn,greentd::commands=debug cargo run -- host
//! #+END_EXAMPLE
//!
//! A line with no target is a bug in this crate, not a style choice: it is a
//! line nobody can silence.
//!
//! # Levels
//!
//! | Level   | What belongs there |
//! |---------+--------------------|
//! | `error` | The match cannot continue, or the operator has to act: a socket that will not bind, a peer whose traffic cannot be parsed, an invariant that has been broken. Nothing at this level is recoverable by a player. |
//! | `warn`  | Something is wrong and the match goes on anyway: the match is over, a peer keeps overspending its command budget, a client asked for a tower kind that does not exist. |
//! | `info`  | One line per lifecycle event, and no more: listening, a player joining or leaving, a wave starting, the match ending. A whole match should be a few hundred lines at `info`; if it is thousands, a line belongs a level down. |
//! | `debug` | Per-command detail that is only interesting once something is already wrong: every refusal and its reason, every dropped command, every command over budget. |
//! | `trace` | Per-entity, per-tick detail: a creep spawning, a creep dying, a tower's shot. Expect hundreds of lines a second; this level exists to be turned on for ten seconds. |
//!
//! The rule behind the table: a player never sees a log line, so nothing a
//! player *needs* belongs above `info`, and nothing an operator needs *rarely*
//! belongs below `debug`.
//!
//! # The per-match file
//!
//! [`file_layer`] is what `main` hands to `LogPlugin::custom_layer`. It writes
//! the same lines as the terminal, without colour, into
//! `<log-dir>/greentd-<stamp>.log`, where `<stamp>` is the UTC start time
//! (`20260911-194347`). `--log-dir` names the directory; an empty value turns
//! the file off.
//!
//! The clock is read here and nowhere else in this crate. A log file has to be
//! named after the moment the process started, and that is a wall-clock
//! question; the *simulation* may not read a clock at all (`found-009`), which
//! is why the seed lives in the balance tables instead.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::app::App;
use bevy::log::tracing_subscriber::Layer as _;
use bevy::log::{BoxedLayer, tracing_subscriber};

/// The stable log targets. Every log line in the crate names one of these.
pub mod target {
    /// Sockets and connections: binding, listening, a peer arriving or leaving.
    pub const NET: &str = "greentd::net";
    /// The authoritative state machine: waves, the lose condition, sim events.
    pub const SIM: &str = "greentd::sim";
    /// The server's mirror: what was replicated, and to whom.
    pub const REPLICATION: &str = "greentd::replication";
    /// Client intents, and what the server did with them.
    pub const COMMANDS: &str = "greentd::commands";
}

/// The layer `LogPlugin::custom_layer` is pointed at.
///
/// `custom_layer` is a bare `fn`, so this cannot capture the log directory; it
/// reads [`Config`](crate::config::Config) out of the world instead, which is
/// where `main` has already put it. A `None` return is Bevy's "no extra layer"
/// and is the right answer for a run that does not want a file.
pub fn file_layer(app: &mut App) -> Option<BoxedLayer> {
    let config = app.world().get_resource::<crate::config::Config>()?;
    let dir = config.log_dir.as_ref()?;

    match open_match_log(dir) {
        Ok((file, path)) => {
            // The subscriber does not exist yet -- this layer is what the
            // subscriber is being built out of -- so this one line goes to
            // stderr by hand. It is the only way to tell the operator where the
            // file they asked for actually landed.
            eprintln!("greentd: logging this match to {}", path.display());
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(Mutex::new(file))
                .with_ansi(false)
                .boxed();
            Some(layer)
        }
        Err(err) => {
            // Not fatal: a run with no log file is still a run. It is worth a
            // line because the operator asked for a file and will not get one.
            eprintln!("greentd: no match log in {}: {err}", dir.display());
            None
        }
    }
}

/// Create (or append to) this match's log file, and say where it is.
fn open_match_log(dir: &Path) -> io::Result<(File, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let path = match_log_path(dir, SystemTime::now());
    // Append rather than truncate: two hosts started within the same second
    // share a name, and losing the first one's log to the second is worse than
    // a file with two headers in it.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    Ok((file, path))
}

/// `<dir>/greentd-<YYYYMMDD>-<HHMMSS>.log`, in UTC.
pub fn match_log_path(dir: &Path, at: SystemTime) -> PathBuf {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    dir.join(format!("greentd-{}.log", utc_stamp(seconds)))
}

/// `YYYYMMDD-HHMMSS` for a Unix timestamp.
fn utc_stamp(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let rest = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a calendar date, by Howard Hinnant's `civil_from_days`.
///
/// Hand-rolled rather than pulled in, for the same reason the balance hash and
/// the sim's RNG are: it is fifteen lines with no policy in it, and a date
/// library is a large dependency to add for one filename.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_epoch_is_the_epoch() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1), "19700101-000001");
        assert_eq!(utc_stamp(86_399), "19700101-235959");
    }

    #[test]
    fn a_known_instant_reads_back_as_itself() {
        // 2023-11-14T22:13:20Z.
        assert_eq!(utc_stamp(1_700_000_000), "20231114-221320");
        // And a leap day, which is where a hand-rolled calendar usually breaks.
        // 2024-02-29T12:20:00Z, and the day after it.
        assert_eq!(utc_stamp(1_709_209_200), "20240229-122000");
        assert_eq!(utc_stamp(1_709_251_200), "20240301-000000");
    }

    #[test]
    fn the_file_name_carries_the_start_time() {
        let dir = Path::new("/var/log/greentd");
        let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            match_log_path(dir, at),
            PathBuf::from("/var/log/greentd/greentd-20231114-221320.log")
        );
        // A clock before the epoch is not a panic, it is 1970.
        assert_eq!(
            match_log_path(dir, UNIX_EPOCH - Duration::from_secs(5)),
            PathBuf::from("/var/log/greentd/greentd-19700101-000000.log")
        );
    }
}
