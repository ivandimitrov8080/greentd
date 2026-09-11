//! Run configuration: precedence, keys and refusals (`found-002`).
//!
//! Every test here drives `config` directly with the layers it would have built
//! from the command line, the environment and a file, so no test reads the
//! process's real arguments or environment and none of them needs a window, a
//! socket or an `App`.

use std::path::{Path, PathBuf};

use greentd::config::{self, ConfigError, Mode, Settings, Startup};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

/// One layer of overrides, as a source would have written it.
fn layer(pairs: &[(&str, &str)]) -> Settings {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// A scratch directory of this test's own, so tests cannot collide.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("greentd-config-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    dir
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("the scratch file");
}

fn cleanup(dir: &Path) {
    std::fs::remove_dir_all(dir).ok();
}

// ---------------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------------

#[test]
fn the_command_line_beats_the_environment_beats_the_file_beats_the_default() {
    let cli = layer(&[("tick-hz", "60")]);
    let env = layer(&[("tick-hz", "50"), ("server", "10.0.0.5:5000")]);
    let file = layer(&[
        ("tick-hz", "40"),
        ("server", "10.0.0.6:5000"),
        ("map", "twin-lane"),
    ]);

    let config = config::resolve(&[cli, env, file]).expect("the three layers resolve");
    assert_eq!(config.tick_hz, 60.0, "the command line answers first");
    assert_eq!(
        config.server_addr.to_string(),
        "10.0.0.5:5000",
        "then the environment"
    );
    assert_eq!(config.map, "twin-lane", "then the file");

    let env_and_file =
        config::resolve(&[layer(&[]), layer(&[("tick-hz", "50")]), layer(&[("tick-hz", "40")])])
            .expect("resolves");
    assert_eq!(env_and_file.tick_hz, 50.0, "then the environment");

    let file_only = config::resolve(&[layer(&[]), layer(&[]), layer(&[("tick-hz", "40")])])
        .expect("resolves");
    assert_eq!(file_only.tick_hz, 40.0, "then the file");

    let nothing = config::resolve(&[]).expect("the defaults alone resolve");
    assert_eq!(nothing.tick_hz, config::DEFAULT_TICK_HZ, "then the defaults");
    assert_eq!(nothing.mode, Mode::Host);
}

#[test]
fn the_mode_decides_which_socket_is_bound_by_default() {
    let server = config::resolve(&[layer(&[("mode", "server")])]).expect("server resolves");
    assert_eq!(server.bind_addr.to_string(), config::DEFAULT_SERVER_BIND);
    assert_eq!(server.server_addr.to_string(), config::DEFAULT_SERVER_ADDR);
    assert!(server.mode.runs_server() && !server.mode.runs_client());

    let client = config::resolve(&[layer(&[("mode", "client")])]).expect("client resolves");
    assert_eq!(client.bind_addr.to_string(), config::DEFAULT_CLIENT_BIND);
    assert!(client.mode.runs_client() && !client.mode.runs_server());

    let host = config::resolve(&[]).expect("the default mode resolves");
    assert_eq!(host.mode, Mode::Host);
    assert!(host.mode.is_host() && host.mode.runs_server() && host.mode.runs_client());
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn a_bad_value_names_the_key_and_the_value() {
    let err = config::resolve(&[layer(&[("tick-hz", "fast")])])
        .expect_err("`fast` is not a number");
    assert_eq!(
        err,
        ConfigError::BadValue {
            key: "tick-hz".to_string(),
            value: "fast".to_string(),
            expected: "a number",
        }
    );

    let message = err.to_string();
    assert!(message.contains("tick-hz"), "{message}");
    assert!(message.contains("fast"), "{message}");
}

#[test]
fn an_address_that_does_not_parse_names_the_key_and_the_value() {
    let err = config::resolve(&[layer(&[("server", "not-an-address")])])
        .expect_err("`not-an-address` is not an address");
    let message = err.to_string();
    assert!(message.contains("server"), "{message}");
    assert!(message.contains("not-an-address"), "{message}");
}

#[test]
fn a_key_that_nothing_reads_is_refused() {
    let err = config::resolve(&[layer(&[("tick", "60")])]).expect_err("`tick` is not a key");
    assert_eq!(
        err,
        ConfigError::UnknownKey {
            key: "tick".to_string(),
        }
    );
}

#[test]
fn a_nonsense_mode_is_refused() {
    let err = config::resolve(&[layer(&[("mode", "spectator")])]).expect_err("no such mode");
    assert!(matches!(err, ConfigError::BadValue { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// The command line
// ---------------------------------------------------------------------------

#[test]
fn the_command_line_reaches_the_config() {
    let cli = config::parse_args(&args(&["client", "--server", "10.0.0.5:5000"]))
        .expect("the arguments parse");
    assert!(!cli.help);

    let config = config::resolve(&[cli.settings]).expect("resolves");
    assert_eq!(config.mode, Mode::Client);
    assert_eq!(config.server_addr.to_string(), "10.0.0.5:5000");
    assert_eq!(config.bind_addr.to_string(), config::DEFAULT_CLIENT_BIND);
}

#[test]
fn a_positional_port_shortens_the_bind_address() {
    let cli = config::parse_args(&args(&["client", "5200"])).expect("the arguments parse");
    let config = config::resolve(&[cli.settings]).expect("resolves");
    assert_eq!(config.mode, Mode::Client);
    assert_eq!(config.bind_addr.to_string(), "127.0.0.1:5200");
}

#[test]
fn an_explicit_mode_beats_the_positional_one() {
    let cli =
        config::parse_args(&args(&["client", "--mode", "server"])).expect("the arguments parse");
    let config = config::resolve(&[cli.settings]).expect("resolves");
    assert_eq!(config.mode, Mode::Server);
}

#[test]
fn an_equals_sign_is_the_same_as_a_space() {
    let cli = config::parse_args(&args(&["--tick-hz=60"])).expect("the arguments parse");
    let config = config::resolve(&[cli.settings]).expect("resolves");
    assert_eq!(config.tick_hz, 60.0);
}

#[test]
fn help_is_recorded_rather_than_resolved() {
    let cli = config::parse_args(&args(&["--help"])).expect("the arguments parse");
    assert!(cli.help);
    assert!(cli.settings.is_empty());
}

#[test]
fn a_flag_without_a_value_is_refused() {
    let err =
        config::parse_args(&args(&["client", "--server"])).expect_err("--server needs a value");
    assert_eq!(
        err,
        ConfigError::MissingValue {
            key: "server".to_string(),
        }
    );
}

#[test]
fn a_third_positional_is_refused() {
    let err = config::parse_args(&args(&["client", "5200", "extra"]))
        .expect_err("a mode and a port are the only positionals");
    assert_eq!(
        err,
        ConfigError::UnexpectedArgument {
            value: "extra".to_string(),
        }
    );
}

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

#[test]
fn the_environment_names_keys_after_the_prefix() {
    let env = config::settings_from_env(vec![
        ("GREENTD_TICK_HZ".to_string(), "60".to_string()),
        ("GREENTD_BALANCE_DIR".to_string(), "/tmp/balance".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
        ("HOME".to_string(), "/root".to_string()),
    ]);

    assert_eq!(env.get("tick-hz").map(String::as_str), Some("60"));
    assert_eq!(env.get("balance-dir").map(String::as_str), Some("/tmp/balance"));
    assert_eq!(env.len(), 2, "only the prefixed variables are ours");

    let config = config::resolve(&[env]).expect("the environment layer resolves");
    assert_eq!(config.tick_hz, 60.0);
    assert_eq!(config.balance_dir, PathBuf::from("/tmp/balance"));
}

// ---------------------------------------------------------------------------
// The config file
// ---------------------------------------------------------------------------

#[test]
fn a_config_file_is_key_value_with_comments_and_quotes() {
    let dir = scratch_dir("file");
    let path = dir.join("greentd.conf");
    write(
        &path,
        "# a comment\n\ntick-hz = 45\nmap = twin-lane   # trailing comment\nbalance-dir = \"/tmp/balance\"\n",
    );

    let settings = config::file_settings(&path).expect("the file parses");
    assert_eq!(settings.get("tick-hz").map(String::as_str), Some("45"));
    assert_eq!(settings.get("map").map(String::as_str), Some("twin-lane"));
    assert_eq!(
        settings.get("balance-dir").map(String::as_str),
        Some("/tmp/balance"),
        "a value may be quoted"
    );

    let config = config::resolve(&[settings]).expect("the file layer resolves");
    assert_eq!(config.tick_hz, 45.0);
    assert_eq!(config.map, "twin-lane");

    cleanup(&dir);
}

#[test]
fn a_malformed_line_names_the_file_and_the_line() {
    let dir = scratch_dir("malformed");
    let path = dir.join("greentd.conf");
    write(&path, "tick-hz = 45\nnonsense\n");

    let err = config::file_settings(&path).expect_err("line 2 is not `key = value`");
    match err {
        ConfigError::Line {
            line,
            path: reported,
            ..
        } => {
            assert_eq!(line, 2);
            assert_eq!(reported, path);
        }
        other => panic!("expected a line error, got {other:?}"),
    }

    cleanup(&dir);
}

#[test]
fn a_config_file_that_cannot_be_read_is_a_startup_error() {
    let err = config::file_settings(Path::new("/nonexistent/greentd.conf"))
        .expect_err("there is no such file");
    assert!(matches!(err, ConfigError::Io { .. }), "{err:?}");
}

#[test]
fn the_environment_can_name_the_config_file() {
    let dir = scratch_dir("env-file");
    let path = dir.join("greentd.conf");
    write(&path, "tick-hz = 15\n");

    let mut env = Settings::new();
    env.insert(config::KEY_CONFIG.to_string(), path.display().to_string());

    let startup = config::startup_from(&args(&[]), &env).expect("the run resolves");
    let config = match startup {
        Startup::Run(config) => config,
        Startup::Help => panic!("no help was asked for"),
    };
    assert_eq!(config.tick_hz, 15.0);
    assert_eq!(config.mode, Mode::Host, "no mode anywhere means host");

    cleanup(&dir);
}

#[test]
fn help_short_circuits_before_any_config_file_is_read() {
    let startup = config::startup_from(
        &args(&["--help", "--config", "/nonexistent/greentd.conf"]),
        &Settings::new(),
    )
    .expect("--help is not an error");
    assert!(matches!(startup, Startup::Help));
}
