//! Balance data: every tuned number, in RON, loaded by both peers.
//!
//! This module exists because of the `decide-data-driven` decision in
//! `tasks/README.org`: the original map's numbers were tuned by hand over
//! years, and this project has to be able to do the same without a recompile.
//! It also has to be able to *prove* that both peers are using the same
//! numbers, which is why [`BalanceData::hash`] exists: a dumb client renders
//! tooltips straight from these tables, so a mismatch means the UI lies.
//!
//! The loader is deliberately plain `std::fs` + `ron::from_str` rather than an
//! `AssetServer` load: it must work in `cargo test` with no window, no GPU and
//! no async asset IO, and the tables have to be readable *before* any plugin is
//! built (see `src/main.rs`). A validation failure is a hard startup error that
//! names the offending field path, e.g. `waves[7].creeps[0].model`.
//!
//! Files, all in the balance directory (default `assets/balance`, overridable
//! with `GREENTD_BALANCE_DIR`):
//!
//! | File               | Table                              |
//! |--------------------+------------------------------------|
//! | `match.ron`        | [`MatchRules`] — gold, cap, timers  |
//! | `towers.ron`       | [`TowerDef`] — indexed by tower kind |
//! | `creeps.ron`       | [`CreepDef`] — creep models         |
//! | `waves.ron`        | [`WaveTable`] — scaling and waves   |
//! | `difficulties.ron` | [`DifficultyDef`]                   |
//! | `modes.ron`        | [`GameModeDef`]                     |

use std::fmt;
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Directory searched when `GREENTD_BALANCE_DIR` is unset.
pub const DEFAULT_BALANCE_DIR: &str = "assets/balance";

/// Environment variable naming the balance directory.
pub const BALANCE_DIR_ENV: &str = "GREENTD_BALANCE_DIR";

/// The balance directory to use when nothing else is configured.
///
/// Prefers `GREENTD_BALANCE_DIR`, then the crate's own `assets/balance` (which
/// is what `cargo run` and `cargo test` see), then a CWD-relative path so a
/// copied-out binary can still find a sibling `assets` directory.
pub fn default_balance_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(BALANCE_DIR_ENV) {
        return PathBuf::from(dir);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_BALANCE_DIR);
    if manifest.is_dir() {
        return manifest;
    }
    PathBuf::from(DEFAULT_BALANCE_DIR)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A balance table could not be read, parsed or trusted.
#[derive(Debug)]
pub enum BalanceError {
    /// The file could not be read.
    Io { path: PathBuf, source: io::Error },
    /// The file is not valid RON for its table type.
    Parse { path: PathBuf, message: String },
    /// The file parsed but a cross-reference or a range check failed.
    Invalid { field: String, message: String },
}

impl BalanceError {
    fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for BalanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Parse { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Invalid { field, message } => write!(f, "balance: {field}: {message}"),
        }
    }
}

impl std::error::Error for BalanceError {}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// Numbers that describe the match itself rather than its content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchRules {
    /// Gold every player starts with.
    pub start_gold: u32,
    /// Lineage A lose condition: more live creeps than this ends the match.
    /// See the lineage section in `tasks/README.org`; `sim-005` makes the rule
    /// itself selectable, this is only the number.
    pub overrun_cap: u32,
    /// Seconds between waves.
    pub wave_interval: f32,
    /// Delay before wave 1, so a lobby is not immediately under attack.
    pub first_wave_delay: f32,
    /// Minimum time between two `CallWave` commands from one player.
    pub call_wave_cooldown: f32,
    /// The match's RNG seed (`found-009`). A seeded match is a reproducible
    /// match: the same seed and the same intents produce the same game, which
    /// is what a test or a replay needs. It is a *match* setting rather than a
    /// process setting, so it lives here and not in `Config`.
    pub seed: u64,
}

/// One tower family, addressed on the wire by its **index** in `towers.ron`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TowerDef {
    pub key: String,
    pub name: String,
    pub cost: u32,
    /// Range in world units.
    pub range: f32,
    pub damage: f32,
    /// Seconds between shots, before tier scaling.
    pub cooldown: f32,
    /// Splash radius in world units; `0.0` means single-target.
    #[serde(default)]
    pub splash: f32,
    /// Speed multiplier applied to hit creeps; `1.0` means no slow.
    #[serde(default = "no_slow")]
    pub slow: f32,
    /// Cost of the first upgrade.
    pub upgrade_base_cost: u32,
    /// Fractional cost growth per further level.
    pub upgrade_growth: f32,
    /// The tower this one upgrades into, if the family branches.
    #[serde(default)]
    pub next: Option<String>,
}

fn no_slow() -> f32 {
    1.0
}

/// One creep model. The wave table names these; `waves-004` grows the set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreepDef {
    pub key: String,
    pub name: String,
    #[serde(default = "unit_mult")]
    pub hp_mult: f32,
    #[serde(default = "unit_mult")]
    pub speed_mult: f32,
    #[serde(default = "unit_mult")]
    pub bounty_mult: f32,
}

fn unit_mult() -> f32 {
    1.0
}

/// The procedural part of the wave generator: the curves a wave number is fed
/// through. `waves-004` replaces this with an explicit table where it matters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveScaling {
    /// Creep health of wave 1.
    pub hp_base: f32,
    /// Fractional health growth per wave.
    pub hp_growth: f32,
    /// Creep speed of wave 1.
    pub speed_base: f32,
    /// Speed added per wave.
    pub speed_growth: f32,
    /// Bounty of wave 1.
    pub bounty_base: f32,
    /// Bounty added per wave.
    pub bounty_per_wave: f32,
    /// Creep count of wave 1, per connected player.
    pub count_base: f32,
    /// Creep count added per wave, per connected player.
    pub count_per_wave: f32,
    /// How far from the exact center of its slot a creep may spawn, as a
    /// fraction of the slot's width (`found-009`). Must be below `0.5`, so a
    /// wave can never reorder itself.
    #[serde(default)]
    pub spawn_jitter: f32,
}

/// One creep line inside one wave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveCreep {
    /// A [`CreepDef::key`].
    pub model: String,
    pub count: u32,
}

/// One wave of the finite table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveDef {
    /// 1-based wave number.
    pub index: u32,
    pub creeps: Vec<WaveCreep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveTable {
    pub scaling: WaveScaling,
    /// Explicit waves. Empty means "every wave comes from `scaling`", which is
    /// what the vertical slice does until `waves-004` lands.
    #[serde(default)]
    pub waves: Vec<WaveDef>,
    /// Highest wave the finite table reaches, if it is finite.
    #[serde(default)]
    pub max_wave: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DifficultyDef {
    pub key: String,
    pub name: String,
    #[serde(default = "unit_mult")]
    pub hp_mult: f32,
    #[serde(default = "unit_mult")]
    pub speed_mult: f32,
    #[serde(default = "unit_mult")]
    pub bounty_mult: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameModeDef {
    pub key: String,
    pub name: String,
    /// A short game is a prefix of the long one, not a different table.
    #[serde(default)]
    pub short_game: bool,
}

/// Every table, validated and ready to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceData {
    pub match_rules: MatchRules,
    pub towers: Vec<TowerDef>,
    pub creeps: Vec<CreepDef>,
    pub waves: WaveTable,
    pub difficulties: Vec<DifficultyDef>,
    pub modes: Vec<GameModeDef>,
}

// ---------------------------------------------------------------------------
// Resource
// ---------------------------------------------------------------------------

/// The balance tables as a Bevy resource, shared by the sim and the UI.
#[derive(Resource, Debug, Clone)]
pub struct Balance(pub Arc<BalanceData>);

impl Balance {
    pub fn load(dir: &Path) -> Result<Self, BalanceError> {
        Ok(Self(Arc::new(BalanceData::load(dir)?)))
    }

    /// Load from [`default_balance_dir`].
    pub fn shipped() -> Result<Self, BalanceError> {
        Self::load(&default_balance_dir())
    }
}

impl Deref for Balance {
    type Target = BalanceData;

    fn deref(&self) -> &BalanceData {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

impl BalanceData {
    /// Read every table from `dir` and validate the lot.
    pub fn load(dir: &Path) -> Result<Self, BalanceError> {
        let data = Self {
            match_rules: read_ron(&dir.join("match.ron"))?,
            towers: read_ron(&dir.join("towers.ron"))?,
            creeps: read_ron(&dir.join("creeps.ron"))?,
            waves: read_ron(&dir.join("waves.ron"))?,
            difficulties: read_ron(&dir.join("difficulties.ron"))?,
            modes: read_ron(&dir.join("modes.ron"))?,
        };
        data.validate()?;
        Ok(data)
    }

    /// Read every table from [`default_balance_dir`].
    pub fn shipped() -> Result<Self, BalanceError> {
        Self::load(&default_balance_dir())
    }

    /// The tower a wire-level `kind` refers to.
    pub fn tower(&self, kind: u8) -> Option<&TowerDef> {
        self.towers.get(kind as usize)
    }

    pub fn tower_by_key(&self, key: &str) -> Option<&TowerDef> {
        self.towers.iter().find(|t| t.key == key)
    }

    pub fn creep_by_key(&self, key: &str) -> Option<&CreepDef> {
        self.creeps.iter().find(|c| c.key == key)
    }

    /// Number of distinct tower kinds, i.e. the exclusive upper bound of a
    /// wire-level `kind`.
    pub fn tower_kind_count(&self) -> u8 {
        self.towers.len().min(u8::MAX as usize) as u8
    }

    /// Creep count for one wave, for a lobby of `players` connected players.
    pub fn creeps_per_wave(&self, wave: u32, players: u32) -> u32 {
        let s = &self.waves.scaling;
        let per_player = (s.count_base + s.count_per_wave * wave as f32).floor();
        (per_player.max(1.0) as u32) * players.max(1)
    }

    /// Creep health for one wave, before difficulty scaling.
    pub fn creep_hp(&self, wave: u32) -> f32 {
        let s = &self.waves.scaling;
        s.hp_base * (1.0 + s.hp_growth * wave.saturating_sub(1) as f32)
    }

    /// Creep speed for one wave, before difficulty scaling.
    pub fn creep_speed(&self, wave: u32) -> f32 {
        let s = &self.waves.scaling;
        s.speed_base + s.speed_growth * wave.saturating_sub(1) as f32
    }

    /// Bounty for one wave creep, before difficulty scaling.
    pub fn creep_bounty(&self, wave: u32) -> u32 {
        let s = &self.waves.scaling;
        (s.bounty_base + s.bounty_per_wave * wave as f32).floor() as u32
    }

    /// Cost of taking `tower` from `level` to `level + 1`.
    pub fn upgrade_cost(&self, tower: &TowerDef, level: u8) -> u32 {
        let steps = level.saturating_sub(1) as f32;
        (tower.upgrade_base_cost as f32 * (1.0 + tower.upgrade_growth * steps)).floor() as u32
    }

    /// Gold returned when a tower is sold: 70% of what it cost to get here.
    pub fn sell_refund(&self, tower: &TowerDef, level: u8) -> u32 {
        let spent = tower.cost as u64 * level as u64;
        (spent * 7 / 10) as u32
    }

    /// A stable hash of the tables, for the protocol version (`net-001`).
    ///
    /// Stable means: independent of the order entries appear in the files, and
    /// of `-0.0` versus `0.0`. Two peers that agree on this hash agree on every
    /// number the client is allowed to display.
    ///
    /// The match seed is deliberately *not* part of it. Peers have to agree on
    /// the rules; which match they are playing is a separate question, and two
    /// peers in two different matches on the same rules are not in disagreement.
    pub fn hash(&self) -> u64 {
        let mut canonical = self.clone();
        canonical.normalise();
        fnv1a_64(format!("{canonical:?}").as_bytes())
    }

    /// Sort every collection and canonicalise floats, so two semantically equal
    /// tables hash the same.
    fn normalise(&mut self) {
        self.towers.sort_by(|a, b| a.key.cmp(&b.key));
        self.creeps.sort_by(|a, b| a.key.cmp(&b.key));
        self.difficulties.sort_by(|a, b| a.key.cmp(&b.key));
        self.modes.sort_by(|a, b| a.key.cmp(&b.key));
        self.waves.waves.sort_by_key(|w| w.index);
        for wave in &mut self.waves.waves {
            wave.creeps.sort_by(|a, b| a.model.cmp(&b.model));
        }

        let m = &mut self.match_rules;
        m.wave_interval = canon_f32(m.wave_interval);
        m.first_wave_delay = canon_f32(m.first_wave_delay);
        m.call_wave_cooldown = canon_f32(m.call_wave_cooldown);
        // See `hash`: the seed is a match setting, not a rule.
        m.seed = 0;

        for t in &mut self.towers {
            t.range = canon_f32(t.range);
            t.damage = canon_f32(t.damage);
            t.cooldown = canon_f32(t.cooldown);
            t.splash = canon_f32(t.splash);
            t.slow = canon_f32(t.slow);
            t.upgrade_growth = canon_f32(t.upgrade_growth);
        }
        for c in &mut self.creeps {
            c.hp_mult = canon_f32(c.hp_mult);
            c.speed_mult = canon_f32(c.speed_mult);
            c.bounty_mult = canon_f32(c.bounty_mult);
        }
        for d in &mut self.difficulties {
            d.hp_mult = canon_f32(d.hp_mult);
            d.speed_mult = canon_f32(d.speed_mult);
            d.bounty_mult = canon_f32(d.bounty_mult);
        }
        let s = &mut self.waves.scaling;
        s.hp_base = canon_f32(s.hp_base);
        s.hp_growth = canon_f32(s.hp_growth);
        s.speed_base = canon_f32(s.speed_base);
        s.speed_growth = canon_f32(s.speed_growth);
        s.bounty_base = canon_f32(s.bounty_base);
        s.bounty_per_wave = canon_f32(s.bounty_per_wave);
        s.count_base = canon_f32(s.count_base);
        s.count_per_wave = canon_f32(s.count_per_wave);
        s.spawn_jitter = canon_f32(s.spawn_jitter);
    }

    // -----------------------------------------------------------------------
    // Validation
    // -----------------------------------------------------------------------

    /// Every cross-reference resolves and every range check holds.
    pub fn validate(&self) -> Result<(), BalanceError> {
        if self.towers.is_empty() {
            return Err(BalanceError::invalid("towers", "the tower table is empty"));
        }
        if self.creeps.is_empty() {
            return Err(BalanceError::invalid("creeps", "the creep table is empty"));
        }
        if self.difficulties.is_empty() {
            return Err(BalanceError::invalid(
                "difficulties",
                "the difficulty table is empty",
            ));
        }
        if self.modes.is_empty() {
            return Err(BalanceError::invalid(
                "modes",
                "the game mode table is empty",
            ));
        }

        check_unique("towers", &keys(&self.towers, |t| &t.key))?;
        check_unique("creeps", &keys(&self.creeps, |c| &c.key))?;
        check_unique("difficulties", &keys(&self.difficulties, |d| &d.key))?;
        check_unique("modes", &keys(&self.modes, |m| &m.key))?;

        self.validate_towers()?;
        self.validate_creeps()?;
        self.validate_waves()?;
        self.validate_scaling()?;
        self.validate_match_rules()
    }

    fn validate_towers(&self) -> Result<(), BalanceError> {
        for (i, t) in self.towers.iter().enumerate() {
            if t.cost == 0 {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].cost"),
                    "cost must be positive",
                ));
            }
            if !above(t.range, 0.0) {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].range"),
                    "range must be positive",
                ));
            }
            if !at_least(t.damage, 0.0) {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].damage"),
                    "damage cannot be negative",
                ));
            }
            if !above(t.cooldown, 0.0) {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].cooldown"),
                    "cooldown must be positive",
                ));
            }
            if !(above(t.slow, 0.0) && at_most(t.slow, 1.0)) {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].slow"),
                    "slow is a speed multiplier in (0.0, 1.0]",
                ));
            }
            if !at_least(t.upgrade_growth, 0.0) {
                return Err(BalanceError::invalid(
                    format!("towers[{i}].upgrade_growth"),
                    "upgrade growth cannot be negative",
                ));
            }
        }

        // Tier chains: every link resolves, none cycles, and going up a tier
        // costs strictly more while never dealing less damage. The field path
        // names the *link* that is wrong, so `towers[1].next` is what you get
        // when `cannon` upgrades into something cheaper.
        for start in 0..self.towers.len() {
            let mut seen: Vec<&str> = vec![self.towers[start].key.as_str()];
            let mut at = start;
            while let Some(key) = self.towers[at].next.as_deref() {
                let field = format!("towers[{at}].next");
                if seen.contains(&key) {
                    return Err(BalanceError::invalid(
                        field,
                        format!("tier chain cycles back to {key:?}"),
                    ));
                }
                let Some(next_index) = self.towers.iter().position(|t| t.key == key) else {
                    return Err(BalanceError::invalid(
                        field,
                        format!("unknown tower {key:?}"),
                    ));
                };
                let (prev, next) = (&self.towers[at], &self.towers[next_index]);
                if next.cost <= prev.cost {
                    return Err(BalanceError::invalid(
                        field,
                        format!(
                            "{:?} costs {} but its predecessor {:?} costs {}",
                            next.key, next.cost, prev.key, prev.cost
                        ),
                    ));
                }
                if next.damage < prev.damage {
                    return Err(BalanceError::invalid(
                        field,
                        format!(
                            "{:?} deals {} but its predecessor {:?} deals {}",
                            next.key, next.damage, prev.key, prev.damage
                        ),
                    ));
                }
                seen.push(next.key.as_str());
                at = next_index;
            }
        }
        Ok(())
    }

    fn validate_creeps(&self) -> Result<(), BalanceError> {
        for (i, c) in self.creeps.iter().enumerate() {
            if !above(c.hp_mult, 0.0) {
                return Err(BalanceError::invalid(
                    format!("creeps[{i}].hp_mult"),
                    "health multiplier must be positive",
                ));
            }
            if !above(c.speed_mult, 0.0) {
                return Err(BalanceError::invalid(
                    format!("creeps[{i}].speed_mult"),
                    "speed multiplier must be positive",
                ));
            }
            if !at_least(c.bounty_mult, 0.0) {
                return Err(BalanceError::invalid(
                    format!("creeps[{i}].bounty_mult"),
                    "bounty multiplier cannot be negative",
                ));
            }
        }
        Ok(())
    }

    fn validate_waves(&self) -> Result<(), BalanceError> {
        for (i, wave) in self.waves.waves.iter().enumerate() {
            if wave.index == 0 {
                return Err(BalanceError::invalid(
                    format!("waves[{i}].index"),
                    "wave indices are 1-based",
                ));
            }
            if i > 0 && self.waves.waves[i - 1].index >= wave.index {
                return Err(BalanceError::invalid(
                    format!("waves[{i}].index"),
                    format!(
                        "wave indices must be strictly increasing, {} comes after {}",
                        wave.index,
                        self.waves.waves[i - 1].index
                    ),
                ));
            }
            match self.waves.max_wave {
                Some(max) if wave.index > max => {
                    return Err(BalanceError::invalid(
                        format!("waves[{i}].index"),
                        format!("wave index {} is past max_wave {max}", wave.index),
                    ));
                }
                _ => {}
            }
            for (j, creep) in wave.creeps.iter().enumerate() {
                if creep.count == 0 {
                    return Err(BalanceError::invalid(
                        format!("waves[{i}].creeps[{j}].count"),
                        "count must be positive",
                    ));
                }
                if self.creep_by_key(&creep.model).is_none() {
                    return Err(BalanceError::invalid(
                        format!("waves[{i}].creeps[{j}].model"),
                        format!("unknown creep model {:?}", creep.model),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_scaling(&self) -> Result<(), BalanceError> {
        let s = &self.waves.scaling;
        for (field, value) in [
            ("hp_base", s.hp_base),
            ("speed_base", s.speed_base),
            ("count_base", s.count_base),
        ] {
            if !above(value, 0.0) {
                return Err(BalanceError::invalid(
                    format!("waves.scaling.{field}"),
                    "must be positive",
                ));
            }
        }
        for (field, value) in [
            ("hp_growth", s.hp_growth),
            ("speed_growth", s.speed_growth),
            ("bounty_base", s.bounty_base),
            ("bounty_per_wave", s.bounty_per_wave),
            ("count_per_wave", s.count_per_wave),
        ] {
            if !at_least(value, 0.0) {
                return Err(BalanceError::invalid(
                    format!("waves.scaling.{field}"),
                    "cannot be negative",
                ));
            }
        }
        // Half a slot is the point at which two creeps can swap places, and a
        // wave that spawns out of order is not the wave the table describes.
        if !(0.0..0.5).contains(&s.spawn_jitter) {
            return Err(BalanceError::invalid(
                "waves.scaling.spawn_jitter",
                "must be at least 0.0 and below 0.5",
            ));
        }
        Ok(())
    }

    fn validate_match_rules(&self) -> Result<(), BalanceError> {
        let r = &self.match_rules;
        if r.overrun_cap == 0 {
            return Err(BalanceError::invalid(
                "match_rules.overrun_cap",
                "overrun cap must be positive",
            ));
        }
        if !above(r.wave_interval, 0.0) {
            return Err(BalanceError::invalid(
                "match_rules.wave_interval",
                "wave interval must be positive",
            ));
        }
        if !at_least(r.first_wave_delay, 0.0) {
            return Err(BalanceError::invalid(
                "match_rules.first_wave_delay",
                "first wave delay cannot be negative",
            ));
        }
        if !at_least(r.call_wave_cooldown, 0.0) {
            return Err(BalanceError::invalid(
                "match_rules.call_wave_cooldown",
                "call wave cooldown cannot be negative",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_ron<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, BalanceError> {
    let text = fs::read_to_string(path).map_err(|source| BalanceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    ron::from_str(&text).map_err(|err| BalanceError::Parse {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

fn keys<T>(items: &[T], key: impl Fn(&T) -> &String) -> Vec<&str> {
    items.iter().map(|item| key(item).as_str()).collect()
}

/// Reject empty and duplicated keys, naming the first offender.
fn check_unique(namespace: &str, keys: &[&str]) -> Result<(), BalanceError> {
    for (i, key) in keys.iter().enumerate() {
        if key.trim().is_empty() {
            return Err(BalanceError::invalid(
                format!("{namespace}[{i}].key"),
                "key must not be empty",
            ));
        }
        if keys[..i].contains(key) {
            return Err(BalanceError::invalid(
                format!("{namespace}[{i}].key"),
                format!("duplicate key {key:?}"),
            ));
        }
    }
    Ok(())
}

// Every numeric guard below is written as `!above(..)` / `!at_least(..)` rather
// than `!(value > bound)`. The two spell the same test -- *reject* NaN as well as
// the out-of-range value, because `NaN > 0.0` is false and so is
// `!(NaN > 0.0)` ... which is to say, `NaN` must fail the guard, not pass it --
// but only the first says so out loud. Clippy's `neg_cmp_op_on_partial_ord`
// exists because `!(a > b)` reads as `a <= b`, which is wrong for a float.

/// `value > bound`, with `NaN` on the *false* side.
fn above(value: f32, bound: f32) -> bool {
    value.partial_cmp(&bound) == Some(std::cmp::Ordering::Greater)
}

/// `value >= bound`, with `NaN` on the *false* side.
fn at_least(value: f32, bound: f32) -> bool {
    matches!(
        value.partial_cmp(&bound),
        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
    )
}

/// `value <= bound`, with `NaN` on the *false* side.
fn at_most(value: f32, bound: f32) -> bool {
    matches!(
        value.partial_cmp(&bound),
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
    )
}

/// `-0.0` and `0.0` compare equal but print differently, so pin them.
fn canon_f32(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

/// FNV-1a, 64 bit. Tiny, stable, and not worth a dependency.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
