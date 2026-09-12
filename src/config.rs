//! Run configuration: everything a process can be pointed at, in one place.
//!
//! `found-002`: before this module existed, `main.rs` carried `TICK_HZ` and two
//! hard-coded socket addresses, `server.rs` carried `SERVER_ADDR`, and the log
//! filter was Bevy's default, so no run could be pointed at another host, an
//! ephemeral port or a different balance directory without a recompile. Every
//! one of those is a [`Config`] field now.
//!
//! A value comes from the first of four places that names it:
//!
//! 1. the command line -- `--server 10.0.0.5:5000`, `--tick-hz 60`
//! 2. the environment -- `GREENTD_SERVER=10.0.0.5:5000`
//! 3. a config file -- `server = 10.0.0.5:5000`, named by `--config`, then
//!    `GREENTD_CONFIG`, then `greentd.conf` beside the working directory
//! 4. the built-in defaults in [`defaults`]
//!
//! Deciding that precedence is [`resolve`]'s whole job, and `tests/config.rs`
//! tests it. A key that nothing reads is an error rather than a silent no-op,
//! and so is a value that cannot be read as the type its key requires: both
//! name the offender, and `main` turns either into a non-zero exit.
//!
//! The mode is resolved before anything else, because it decides two of the
//! defaults: a dedicated server binds the port it serves on, while a client
//! binds a second port so a server and a client can coexist on one machine.
//! A *host* is both roles at once, so its two halves need two sockets; see
//! [`Config::server_bind_addr`] for which one each half takes.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy::prelude::Resource;

use crate::data::balance::{Balance, BalanceError, default_balance_dir};

// ---------------------------------------------------------------------------
// Keys, environment names and defaults
// ---------------------------------------------------------------------------

/// Prefix of every environment variable that names a config key:
/// `GREENTD_TICK_HZ` is the key `tick-hz`. `GREENTD_CONFIG` is the one
/// exception, and the key below says why.
pub const ENV_PREFIX: &str = "GREENTD_";

/// Config file read when neither `--config` nor `GREENTD_CONFIG` names one and
/// this file exists beside the working directory.
pub const DEFAULT_CONFIG_FILE: &str = "greentd.conf";

pub const KEY_MODE: &str = "mode";
pub const KEY_SERVER: &str = "server";
pub const KEY_BIND: &str = "bind";
pub const KEY_PORT: &str = "port";
pub const KEY_TICK_HZ: &str = "tick-hz";
pub const KEY_MAP: &str = "map";
pub const KEY_BALANCE_DIR: &str = "balance-dir";
pub const KEY_LOG: &str = "log";
pub const KEY_LOG_DIR: &str = "log-dir";
pub const KEY_COMMANDS_PER_SECOND: &str = "commands-per-second";
pub const KEY_COMMAND_BURST: &str = "command-burst";
/// Pins this client's player identity (`net-002`). A client setting, unread by
/// a dedicated server, which never generates or owns an identity.
pub const KEY_PLAYER_ID: &str = "player-id";

/// Selects a layer rather than being a [`Config`] field, so it is the one key
/// allowed to name another file. It is read by [`startup_from`], not by
/// [`resolve`].
pub const KEY_CONFIG: &str = "config";

/// Simulation rate, and lightyear's tick rate, so replication is 1:1 with sim
/// steps.
pub const DEFAULT_TICK_HZ: f64 = 30.0;

/// The address a client joins by default.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:5000";

/// The socket a dedicated server binds by default.
pub const DEFAULT_SERVER_BIND: &str = "127.0.0.1:5000";

/// The socket a client (or a host) binds by default: one port above the
/// server's, so both can run on one machine.
pub const DEFAULT_CLIENT_BIND: &str = "127.0.0.1:5100";

/// Map identifier. Only one map exists until `03-map.org`.
pub const DEFAULT_MAP: &str = "green-ring";

/// Directory this match's log file is written to. An empty value turns the file
/// off; see [`Config::log_dir`].
pub const DEFAULT_LOG_DIR: &str = "logs";

/// Command budget: sustained commands per second, per peer (`audit-007`).
pub const DEFAULT_COMMANDS_PER_SECOND: f32 = 20.0;

/// Command budget: how many commands one peer may spend in one frame
/// (`audit-007`).
pub const DEFAULT_COMMAND_BURST: u32 = 16;

/// Printed by `--help` (and by `cargo run -- --help`).
pub const USAGE: &str = concat!(
    "greentd [mode] [options]\n",
    "\n",
    "Modes:\n",
    "  host              server and client in one process (default)\n",
    "  server            dedicated server\n",
    "  client            a client, joining a host\n",
    "\n",
    "Options:\n",
    "  --mode <mode>              the mode, instead of the positional form\n",
    "  --server <addr>            the host a client joins         [default: 127.0.0.1:5000]\n",
    "  --bind <addr>              the local socket to bind\n",
    "  --port <n>                 --bind, shortened to this port\n",
    "  --tick-hz <n>              simulation rate                 [default: 30]\n",
    "  --map <id>                 the map to play                 [default: green-ring]\n",
    "  --balance-dir <path>       directory holding the *.ron tables\n",
    "  --log <filter>             tracing filter, e.g. info,greentd::sim=debug\n",
    "  --log-dir <path>           where this match's log file goes  [default: logs]\n",
    "                             (an empty value writes no file)\n",
    "  --commands-per-second <n>  per-peer command budget         [default: 20]\n",
    "  --command-burst <n>        per-peer per-frame burst        [default: 16]\n",
    "  --player-id <n>            pin this client's identity, instead of generating\n",
    "                             one (useful for exercising a reconnect)  [default: random]\n",
    "  --config <path>            the config file to read         [default: greentd.conf]\n",
    "  --help                     print this text\n",
    "\n",
    "Every option is also an environment variable: upper-case it and prefix it with\n",
    "GREENTD_, so --tick-hz is GREENTD_TICK_HZ. A config file holds one `key = value`\n",
    "per line, with the key spelled as it appears above without the leading dashes\n",
    "and `#` starting a comment. The command line beats the environment, which beats\n",
    "the file, which beats the defaults.\n",
);

/// A layer of overrides: config key -> the value as the user wrote it.
pub type Settings = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

/// Which roles one process plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A dedicated server: authoritative, and nothing else.
    Server,
    /// A client: no simulation, no authority (`decide-authority`).
    Client,
    /// Server and client in one process, which is what `cargo run` does.
    #[default]
    Host,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Server, Mode::Client, Mode::Host];

    /// Parse the positional form, `--mode`, or `GREENTD_MODE`.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim() {
            "server" => Ok(Mode::Server),
            "client" => Ok(Mode::Client),
            "host" => Ok(Mode::Host),
            _ => Err(ConfigError::bad_value(
                KEY_MODE,
                value,
                "server | client | host",
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Server => "server",
            Mode::Client => "client",
            Mode::Host => "host",
        }
    }

    /// True when this process owns the authoritative `Sim`.
    pub fn runs_server(self) -> bool {
        matches!(self, Mode::Server | Mode::Host)
    }

    /// True when this process renders the match, which a dedicated server does
    /// not (`found-001`).
    pub fn runs_client(self) -> bool {
        matches!(self, Mode::Client | Mode::Host)
    }

    /// True when both roles share one world, so a client cannot be addressed as
    /// a remote peer (`audit-003`).
    pub fn is_host(self) -> bool {
        matches!(self, Mode::Host)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong before a plugin exists.
///
/// This is a startup failure, not a `Reject`: `Reject` is an expected refusal
/// reported to a peer, while this is a programmer or configuration mistake that
/// is fatal and is never sent anywhere (`found-006` folds the balance loader
/// into the same type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A value cannot be read as the type its key requires.
    BadValue {
        key: String,
        value: String,
        expected: &'static str,
    },
    /// A key that nothing reads. Reported rather than ignored, so a typo is
    /// loud instead of surprising.
    UnknownKey { key: String },
    /// A flag that needs a value was given without one.
    MissingValue { key: String },
    /// An argument that is neither an option nor a mode or port.
    UnexpectedArgument { value: String },
    /// The config file could not be read.
    Io { path: PathBuf, message: String },
    /// A config-file line is not `key = value`.
    Line {
        path: PathBuf,
        line: usize,
        message: String,
    },
    /// A key has neither a user value nor a default. Unreachable while
    /// [`defaults`] covers every key, but [`resolve`] stays total.
    Missing { key: String },
}

impl ConfigError {
    fn bad_value(key: &str, value: impl Into<String>, expected: &'static str) -> Self {
        Self::BadValue {
            key: key.to_string(),
            value: value.into(),
            expected,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadValue {
                key,
                value,
                expected,
            } => write!(f, "config: --{key} {value:?}: expected {expected}"),
            Self::UnknownKey { key } => write!(f, "config: unknown key {key:?}"),
            Self::MissingValue { key } => write!(f, "config: --{key} needs a value"),
            Self::UnexpectedArgument { value } => {
                write!(f, "config: unexpected argument {value:?}")
            }
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Line {
                path,
                line,
                message,
            } => {
                write!(f, "{}:{line}: {message}", path.display())
            }
            Self::Missing { key } => {
                write!(f, "config: {key:?} has no value and no default")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Everything that can stop a run before it starts (`found-006`).
///
/// This is the one error type `main` has to name, and it deliberately folds two
/// failures that used to abort from two different places with two different
/// messages.
///
/// It is *not* [`Reject`](crate::data::reject::Reject), and the distinction is
/// the point: a `Reject` is an expected refusal inside a running match, it
/// carries a reason the player can act on, and it is sent to that player. A
/// `StartupError`
/// is a programmer or configuration mistake, it is fatal, it is printed to
/// whoever started the process, and it never travels over the network.
#[derive(Debug)]
pub enum StartupError {
    /// The configuration could not be read or resolved.
    Config(ConfigError),
    /// The balance tables could not be read, parsed or trusted.
    Balance(BalanceError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(err) => write!(f, "{err}"),
            Self::Balance(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(err) => Some(err),
            Self::Balance(err) => Some(err),
        }
    }
}

impl From<ConfigError> for StartupError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
    }
}

impl From<BalanceError> for StartupError {
    fn from(err: BalanceError) -> Self {
        Self::Balance(err)
    }
}

// ---------------------------------------------------------------------------
// The resolved config
// ---------------------------------------------------------------------------

/// The configuration of one run, as a resource so any system can read it.
#[derive(Resource, Debug, Clone)]
pub struct Config {
    /// Which roles this process plays.
    pub mode: Mode,

    /// Simulation rate, in ticks per second.
    pub tick_hz: f64,

    /// The host a client joins. Unread by a dedicated server.
    pub server_addr: SocketAddr,

    /// The local UDP socket this process binds.
    pub bind_addr: SocketAddr,

    /// Map identifier. Only one map exists until `03-map.org`.
    pub map: String,

    /// Directory holding the `*.ron` balance tables (`found-004`), loaded by
    /// [`startup_from`] into [`Run::balance`].
    pub balance_dir: PathBuf,

    /// A tracing filter in `EnvFilter` syntax. Empty keeps Bevy's own default,
    /// which is the filter that silences wgpu and naga noise.
    pub log_filter: String,

    /// Directory this match's log file goes in, or `None` for no file.
    ///
    /// The file is named after the start time, so a directory is enough to
    /// identify one match; [`crate::logging::match_log_path`] builds the name.
    pub log_dir: Option<PathBuf>,

    /// Command budget: sustained commands per second, per peer (`audit-007`).
    pub commands_per_second: f32,

    /// Command budget: the burst one peer may spend in one frame
    /// (`audit-007`).
    pub command_burst: u32,

    /// This client's pinned player identity (`net-002`), or `None` to generate
    /// one. Read only by the client half of a run, so a dedicated server ignores
    /// it; its presence is what makes a reconnect from a new port reproducible
    /// by hand (`cargo run -- client --bind 127.0.0.1:5101 --player-id 42`).
    ///
    /// `NonZeroU64` because zero is not an identity: the sim reserves
    /// `PlayerKey(0)` for "no player" (`Creep::last_hit_by`).
    pub player_id: Option<NonZeroU64>,
}

impl Config {
    /// One tick, in the form `Time<Fixed>` and lightyear both want.
    pub fn tick(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.tick_hz)
    }

    /// The socket the server half of this process binds.
    ///
    /// A dedicated server binds [`Config::bind_addr`], which defaults to the
    /// standard server port. A *host*, which is both roles in one process,
    /// instead serves on [`Config::server_addr`] -- the very address its own
    /// client dials -- because a host that tried to bind its client's socket
    /// would fail with `Address already in use` before it ever rendered a
    /// frame. Serving on the dialled address is also what makes a host reachable
    /// from the LAN by passing `--server <lan-ip>:5000`.
    pub fn server_bind_addr(&self) -> SocketAddr {
        match self.mode {
            Mode::Host => self.server_addr,
            Mode::Server | Mode::Client => self.bind_addr,
        }
    }
}

/// Everything a run needs, resolved and loaded before any plugin exists.
#[derive(Debug)]
pub struct Run {
    /// The configuration of this process.
    pub config: Config,
    /// The balance tables, loaded and validated.
    pub balance: Balance,
}

/// What a run resolved to, before any plugin exists.
#[derive(Debug)]
pub enum Startup {
    /// Run with this configuration and these tables.
    Run(Run),
    /// `--help`: the caller prints [`USAGE`] and exits zero.
    Help,
}

/// Resolve a run from the real command line and the real environment.
pub fn startup() -> Result<Startup, StartupError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    startup_from(&args, &env_settings())
}

/// The body of [`startup`], with the process's inputs passed in, so a test can
/// drive it without touching either.
///
/// The balance tables are loaded here rather than by the caller because a run
/// that has a `Config` but no tables is not a run: it would fail a few lines
/// later, in the middle of building an `App`, with a second error type and a
/// second exit path. One function, one error, one message.
pub fn startup_from(args: &[String], env: &Settings) -> Result<Startup, StartupError> {
    let cli = parse_args(args)?;
    // Answered before the config file is read, so `--help` works even when the
    // file it would otherwise have read is broken or missing.
    if cli.help {
        return Ok(Startup::Help);
    }

    let file_path = cli
        .config_file
        .clone()
        .or_else(|| env.get(KEY_CONFIG).map(PathBuf::from))
        .or_else(|| {
            let default = PathBuf::from(DEFAULT_CONFIG_FILE);
            default.is_file().then_some(default)
        });

    let file = match &file_path {
        Some(path) => file_settings(path)?,
        None => Settings::new(),
    };

    let layers: [Settings; 3] = [cli.settings, env.clone(), file];
    let config = resolve(&layers)?;
    let balance = Balance::load(&config.balance_dir)?;
    Ok(Startup::Run(Run { config, balance }))
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// The built-in defaults, as the lowest-priority layer.
pub fn defaults(mode: Mode) -> Settings {
    let mut settings = Settings::new();
    settings.insert(KEY_MODE.to_string(), mode.as_str().to_string());
    settings.insert(KEY_TICK_HZ.to_string(), DEFAULT_TICK_HZ.to_string());
    settings.insert(KEY_SERVER.to_string(), DEFAULT_SERVER_ADDR.to_string());
    settings.insert(
        KEY_BIND.to_string(),
        match mode {
            Mode::Server => DEFAULT_SERVER_BIND.to_string(),
            Mode::Client | Mode::Host => DEFAULT_CLIENT_BIND.to_string(),
        },
    );
    settings.insert(KEY_MAP.to_string(), DEFAULT_MAP.to_string());
    settings.insert(
        KEY_BALANCE_DIR.to_string(),
        default_balance_dir().display().to_string(),
    );
    // Empty means "leave Bevy's default filter alone".
    settings.insert(KEY_LOG.to_string(), String::new());
    settings.insert(KEY_LOG_DIR.to_string(), DEFAULT_LOG_DIR.to_string());
    settings.insert(
        KEY_COMMANDS_PER_SECOND.to_string(),
        DEFAULT_COMMANDS_PER_SECOND.to_string(),
    );
    settings.insert(
        KEY_COMMAND_BURST.to_string(),
        DEFAULT_COMMAND_BURST.to_string(),
    );
    settings
}

/// What one command line asked for.
#[derive(Debug, Default, Clone)]
pub struct Cli {
    /// Overrides, keyed as in a config file.
    pub settings: Settings,
    /// `--config`: the file to read, if any.
    pub config_file: Option<PathBuf>,
    /// `--help`.
    pub help: bool,
}

/// Parse a command line: options in any order, then the positionals, which are
/// a mode and optionally a port (`client 5200`, as the header comment in
/// `main.rs` has always documented).
///
/// Options are `--key value` or `--key=value`. A positional mode never
/// overrides an explicit `--mode`, and a positional port never overrides
/// `--bind`.
pub fn parse_args(args: &[String]) -> Result<Cli, ConfigError> {
    let mut cli = Cli::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut rest = args.iter();

    while let Some(arg) = rest.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            positionals.push(arg.clone());
            continue;
        };
        let (key, inline) = match flag.split_once('=') {
            Some((key, value)) => (key, Some(value.to_string())),
            None => (flag, None),
        };
        if key == "help" {
            cli.help = true;
            continue;
        }
        let value = match inline {
            Some(value) => value,
            None => rest
                .next()
                .cloned()
                .ok_or_else(|| ConfigError::MissingValue {
                    key: key.to_string(),
                })?,
        };
        if key == KEY_CONFIG {
            cli.config_file = Some(PathBuf::from(value));
        } else {
            cli.settings.insert(key.to_string(), value);
        }
    }

    if let Some(mode) = positionals.first() {
        cli.settings
            .entry(KEY_MODE.to_string())
            .or_insert_with(|| mode.clone());
    }
    if let Some(port) = positionals.get(1) {
        cli.settings
            .entry(KEY_PORT.to_string())
            .or_insert_with(|| port.clone());
    }
    if let Some(extra) = positionals.get(2) {
        return Err(ConfigError::UnexpectedArgument {
            value: extra.clone(),
        });
    }

    Ok(cli)
}

/// The `GREENTD_*` variables of this process, as a layer.
pub fn env_settings() -> Settings {
    // `vars_os` rather than `vars`: the latter panics on a variable that is not
    // valid Unicode, and that is not this program's business to decide.
    settings_from_env(std::env::vars_os().filter_map(|(name, value)| {
        Some((name.to_str()?.to_string(), value.to_str()?.to_string()))
    }))
}

/// Turn `GREENTD_TICK_HZ=60` into the key `tick-hz`. Variables outside the
/// prefix are ignored, because the environment is shared; variables inside it
/// name keys, and an unknown one is an error, because the prefix is ours.
pub fn settings_from_env<I>(vars: I) -> Settings
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut settings = Settings::new();
    for (name, value) in vars {
        let Some(key) = name.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        settings.insert(key.to_ascii_lowercase().replace('_', "-"), value);
    }
    settings
}

/// Read a config file: one `key = value` per line, `#` starts a comment, blank
/// lines are ignored, and a value may be wrapped in double quotes.
pub fn file_settings(path: &Path) -> Result<Settings, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|err| ConfigError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;

    let mut settings = Settings::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ConfigError::Line {
                path: path.to_path_buf(),
                line: index + 1,
                message: format!("expected `key = value`, found {line:?}"),
            });
        };
        settings.insert(
            key.trim().to_ascii_lowercase(),
            unquote(value.trim()).to_string(),
        );
    }
    Ok(settings)
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolve the effective config from layers of overrides, highest priority
/// first, with [`defaults`] filling whatever they leave out.
///
/// The mode is read before the defaults are built, because it decides which
/// socket a client binds. Everything else is a straight first-layer-wins merge,
/// and a key still standing at the end is one that nothing reads.
pub fn resolve(layers: &[Settings]) -> Result<Config, ConfigError> {
    let mode = match layers.iter().find_map(|layer| layer.get(KEY_MODE)) {
        Some(value) => Mode::parse(value)?,
        None => Mode::default(),
    };

    let default_layer = defaults(mode);
    let mut merged = Settings::new();
    for layer in layers.iter().chain(std::iter::once(&default_layer)) {
        for (key, value) in layer {
            merged.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    // Neither of these is a setting: the mode has been resolved, and the config
    // file has already been read (or decided against).
    merged.remove(KEY_MODE);
    merged.remove(KEY_CONFIG);

    let tick_hz = take_f64(&mut merged, KEY_TICK_HZ)?;
    let server_addr = take_addr(&mut merged, KEY_SERVER)?;
    let bind = take_addr(&mut merged, KEY_BIND)?;
    let port = take_port(&mut merged, KEY_PORT)?;
    let map = take(&mut merged, KEY_MAP)?;
    let balance_dir = take(&mut merged, KEY_BALANCE_DIR)?;
    let log_filter = take(&mut merged, KEY_LOG)?;
    let log_dir = take(&mut merged, KEY_LOG_DIR)?;
    let commands_per_second = take_f32(&mut merged, KEY_COMMANDS_PER_SECOND)?;
    let command_burst = take_u32(&mut merged, KEY_COMMAND_BURST)?;
    let player_id = take_optional_id(&mut merged, KEY_PLAYER_ID)?;

    if !tick_hz.is_finite() || tick_hz <= 0.0 {
        return Err(ConfigError::bad_value(
            KEY_TICK_HZ,
            tick_hz.to_string(),
            "a positive number of ticks per second",
        ));
    }
    if !commands_per_second.is_finite() || commands_per_second < 0.0 {
        return Err(ConfigError::bad_value(
            KEY_COMMANDS_PER_SECOND,
            commands_per_second.to_string(),
            "a non-negative number of commands per second",
        ));
    }
    if command_burst == 0 {
        return Err(ConfigError::bad_value(
            KEY_COMMAND_BURST,
            command_burst.to_string(),
            "a positive number of commands",
        ));
    }
    if let Some(key) = merged.keys().next() {
        return Err(ConfigError::UnknownKey { key: key.clone() });
    }

    Ok(Config {
        mode,
        tick_hz,
        server_addr,
        bind_addr: match port {
            Some(port) => SocketAddr::new(bind.ip(), port),
            None => bind,
        },
        map,
        balance_dir: PathBuf::from(balance_dir),
        log_filter,
        log_dir: (!log_dir.trim().is_empty()).then(|| PathBuf::from(log_dir)),
        commands_per_second,
        command_burst,
        player_id,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Take a key out of the merged settings, or report that nothing supplied it.
fn take(map: &mut Settings, key: &str) -> Result<String, ConfigError> {
    map.remove(key).ok_or_else(|| ConfigError::Missing {
        key: key.to_string(),
    })
}

fn take_f64(map: &mut Settings, key: &str) -> Result<f64, ConfigError> {
    let value = take(map, key)?;
    let parsed = value.trim().parse::<f64>();
    parsed.map_err(|_| ConfigError::bad_value(key, value, "a number"))
}

fn take_f32(map: &mut Settings, key: &str) -> Result<f32, ConfigError> {
    let value = take(map, key)?;
    let parsed = value.trim().parse::<f32>();
    parsed.map_err(|_| ConfigError::bad_value(key, value, "a number"))
}

fn take_u32(map: &mut Settings, key: &str) -> Result<u32, ConfigError> {
    let value = take(map, key)?;
    let parsed = value.trim().parse::<u32>();
    parsed.map_err(|_| ConfigError::bad_value(key, value, "a non-negative integer"))
}

/// Take an optional identity (`net-002`), defaulting to `None`. Unlike [`take`],
/// absence is not an error: it is how a key with no built-in default is spelled.
/// A non-numeric or zero value is refused, because zero is the sim's "no player"
/// sentinel and not an identity.
fn take_optional_id(map: &mut Settings, key: &str) -> Result<Option<NonZeroU64>, ConfigError> {
    let Some(value) = map.remove(key) else {
        return Ok(None);
    };
    value
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .map(Some)
        .ok_or_else(|| ConfigError::bad_value(key, value, "a non-zero integer"))
}

fn take_port(map: &mut Settings, key: &str) -> Result<Option<u16>, ConfigError> {
    let Some(value) = map.remove(key) else {
        return Ok(None);
    };
    let parsed = value.trim().parse::<u16>();
    parsed
        .map(Some)
        .map_err(|_| ConfigError::bad_value(key, value, "a port number"))
}

fn take_addr(map: &mut Settings, key: &str) -> Result<SocketAddr, ConfigError> {
    let value = take(map, key)?;
    let parsed = value.trim().parse::<SocketAddr>();
    parsed.map_err(|_| ConfigError::bad_value(key, value, "an address like 127.0.0.1:5000"))
}

/// Drop one matching pair of surrounding double quotes, if there is one.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}
