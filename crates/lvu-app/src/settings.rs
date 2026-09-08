//! Strict global settings persisted at the XDG config path.
//!
//! Saving emits canonical TOML and therefore does not retain comments or original
//! formatting. Malformed, future-version, non-regular, and unknown-key files are
//! rejected before any replacement, so a canonical rewrite is never silent.
//! `load_settings` and `save_settings` perform blocking filesystem work and must be
//! called by the application's settings worker, never directly by the UI thread.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use thiserror::Error;

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_FILE: &str = "settings.toml";
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
pub const MIB: u64 = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256;
const MAX_MEMORY_MIB: u64 = 64 * 1024;
const MAX_DISK_MIB: u64 = 1024 * 1024;
const MAX_RETENTION_DAYS: u64 = 36_500;
const MAX_SOURCE_RETENTION_RULES: usize = 32;
const SECONDS_PER_DAY: u64 = 86_400;
const TEMP_CREATE_ATTEMPTS: u64 = 16;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const ENV_AI_PROVIDER: &str = "LVU_AI_PROVIDER";
pub const ENV_AI_MODE: &str = "LVU_AI_MODE";
pub const ENV_AI_THINKING: &str = "LVU_AI_THINKING";
pub const ENV_NO_DELIGHT: &str = "LVU_NO_DELIGHT";
pub const ENV_REDUCED_MOTION: &str = "LVU_REDUCED_MOTION";
pub const ENV_ASCII: &str = "LVU_ASCII";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub settings_file: PathBuf,
}

pub fn resolve_paths() -> Result<AppPaths, SettingsError> {
    let home = env::var_os("HOME").map(PathBuf::from);
    resolve_paths_with(|name| env::var_os(name), home.as_deref())
}

pub fn resolve_paths_with(
    mut lookup: impl FnMut(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Result<AppPaths, SettingsError> {
    let home = home.filter(|path| path.is_absolute());
    let config_base = xdg_base(&mut lookup, "XDG_CONFIG_HOME")
        .or_else(|| home.map(|path| path.join(".config")))
        .ok_or(SettingsError::MissingAbsoluteHome)?;
    let cache_base = xdg_base(&mut lookup, "XDG_CACHE_HOME")
        .or_else(|| home.map(|path| path.join(".cache")))
        .ok_or(SettingsError::MissingAbsoluteHome)?;
    let data_base = xdg_base(&mut lookup, "XDG_DATA_HOME")
        .or_else(|| home.map(|path| path.join(".local/share")))
        .ok_or(SettingsError::MissingAbsoluteHome)?;
    let config_dir = config_base.join("lvu");
    Ok(AppPaths {
        settings_file: config_dir.join(SETTINGS_FILE),
        config_dir,
        cache_dir: cache_base.join("lvu"),
        data_dir: data_base.join("lvu"),
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub paseo: PaseoSettings,
    pub appearance: AppearanceSettings,
    pub cache: CacheSettings,
    /// Durable-storage governance. Absent in files written before this
    /// section existed, which is the same as the default: no automatic
    /// deletion of captured data.
    #[serde(default)]
    pub storage: StorageSettings,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaseoSettings {
    pub provider: String,
    pub mode: String,
    pub thinking: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    Terminal,
    LoveDark,
    LoveLight,
    Dracula,
    Nord,
    GruvboxDark,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppearanceSettings {
    pub theme: Theme,
    /// Fixed UTC offset the log viewport draws timestamps in, as a token
    /// (`"Z"`, `"+02:00"`).
    ///
    /// There is no timezone database in this build, so a named zone and its
    /// daylight-saving transitions cannot be honoured; the Settings help line
    /// says so, and every displayed time carries its offset. Absent in files
    /// written before the field existed, which reads as UTC — what they showed.
    #[serde(default = "default_display_zone")]
    pub display_zone: String,
    pub delight_enabled: bool,
    pub reduced_motion: bool,
    pub ascii: bool,
}

fn default_display_zone() -> String {
    lvu::app::DEFAULT_DISPLAY_ZONE.to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheSettings {
    pub memory: MemoryCacheSettings,
    pub disk: DiskCacheSettings,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCacheSettings {
    pub rows_mib: u64,
    pub membership_mib: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskCacheSettings {
    pub total_mib: u64,
    pub index_per_source_mib: u64,
}

/// Policy over durable captured data. This is not a cache budget: the values
/// under `[cache]` bound disposable caches, while these authorize deleting
/// captured records, which never happens unless it is configured here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSettings {
    /// Free disk space kept in reserve. Acquisition stops with a visible error
    /// rather than filling the disk and losing records silently.
    pub reserve_mib: u64,
    pub retention: RetentionSettings,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionSettings {
    /// Off by default. Captured data is kept until deleted explicitly.
    pub enabled: bool,
    /// Total size of all captures. 0 means no limit.
    pub maximum_total_capture_mib: u64,
    /// Age since the last captured record. 0 means no limit.
    pub maximum_age_days: u64,
    #[serde(default)]
    pub source: Vec<SourceRetentionSettings>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRetentionSettings {
    /// Source name or source UUID.
    #[serde(rename = "match")]
    pub matches: String,
    /// 0 means no limit.
    pub maximum_capture_mib: u64,
    /// 0 means no limit.
    pub maximum_age_days: u64,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            reserve_mib: 256,
            retention: RetentionSettings {
                enabled: false,
                maximum_total_capture_mib: 0,
                maximum_age_days: 0,
                source: Vec::new(),
            },
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            paseo: PaseoSettings {
                provider: "codex/gpt-5.6-luna".into(),
                mode: "full-access".into(),
                thinking: "medium".into(),
            },
            appearance: AppearanceSettings {
                theme: Theme::Terminal,
                display_zone: default_display_zone(),
                delight_enabled: true,
                reduced_motion: false,
                ascii: false,
            },
            cache: CacheSettings {
                memory: MemoryCacheSettings {
                    rows_mib: 4,
                    membership_mib: 256,
                },
                disk: DiskCacheSettings {
                    total_mib: 5_120,
                    index_per_source_mib: 256,
                },
            },
            storage: StorageSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<ValidatedSettings, SettingsError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema(self.schema_version));
        }
        validate_text("paseo.provider", &self.paseo.provider)?;
        validate_text("paseo.mode", &self.paseo.mode)?;
        validate_text("paseo.thinking", &self.paseo.thinking)?;
        // An offset this build cannot read would silently display UTC. Refusing
        // it at load says which value is wrong instead.
        if lvu::app::time_zone_offset_minutes(&self.appearance.display_zone).is_none() {
            return Err(SettingsError::InvalidValue {
                field: "appearance.display_zone",
                message: "must be Z or a fixed UTC offset such as +02:00".into(),
            });
        }
        validate_mib(
            "cache.memory.rows_mib",
            self.cache.memory.rows_mib,
            MAX_MEMORY_MIB,
        )?;
        validate_mib(
            "cache.memory.membership_mib",
            self.cache.memory.membership_mib,
            MAX_MEMORY_MIB,
        )?;
        validate_mib(
            "cache.disk.total_mib",
            self.cache.disk.total_mib,
            MAX_DISK_MIB,
        )?;
        validate_mib(
            "cache.disk.index_per_source_mib",
            self.cache.disk.index_per_source_mib,
            MAX_DISK_MIB,
        )?;
        if self.cache.disk.total_mib < self.cache.disk.index_per_source_mib {
            return Err(SettingsError::InvalidValue {
                field: "cache.disk.total_mib",
                message: "must be at least cache.disk.index_per_source_mib".into(),
            });
        }
        let storage = self.validate_storage()?;
        let rows_bytes = checked_mib(self.cache.memory.rows_mib)?;
        if rows_bytes < 64 * 1024 {
            return Err(SettingsError::InvalidValue {
                field: "cache.memory.rows_mib",
                message: "must provide at least the 64 KiB maximum display projection".into(),
            });
        }
        Ok(ValidatedSettings {
            settings: self.clone(),
            row_cache_bytes: rows_bytes,
            membership_bytes: checked_mib(self.cache.memory.membership_mib)?,
            disk_total_bytes: checked_mib(self.cache.disk.total_mib)?,
            index_per_source_bytes: checked_mib(self.cache.disk.index_per_source_mib)?,
            storage,
        })
    }

    fn validate_storage(&self) -> Result<ValidatedStorage, SettingsError> {
        if self.storage.reserve_mib > MAX_DISK_MIB {
            return Err(SettingsError::InvalidValue {
                field: "storage.reserve_mib",
                message: format!("must be between 0 and {MAX_DISK_MIB} MiB"),
            });
        }
        let retention = &self.storage.retention;
        if retention.maximum_total_capture_mib > MAX_DISK_MIB {
            return Err(SettingsError::InvalidValue {
                field: "storage.retention.maximum_total_capture_mib",
                message: format!("must be between 0 and {MAX_DISK_MIB} MiB"),
            });
        }
        validate_days(
            "storage.retention.maximum_age_days",
            retention.maximum_age_days,
        )?;
        if retention.source.len() > MAX_SOURCE_RETENTION_RULES {
            return Err(SettingsError::InvalidValue {
                field: "storage.retention.source",
                message: format!("at most {MAX_SOURCE_RETENTION_RULES} rules are supported"),
            });
        }
        let mut per_source = Vec::with_capacity(retention.source.len());
        let mut seen = std::collections::BTreeSet::new();
        for rule in &retention.source {
            validate_text("storage.retention.source.match", &rule.matches)?;
            if !seen.insert(rule.matches.clone()) {
                return Err(SettingsError::InvalidValue {
                    field: "storage.retention.source.match",
                    message: format!("duplicate rule for {}", rule.matches),
                });
            }
            if rule.maximum_capture_mib > MAX_DISK_MIB {
                return Err(SettingsError::InvalidValue {
                    field: "storage.retention.source.maximum_capture_mib",
                    message: format!("must be between 0 and {MAX_DISK_MIB} MiB"),
                });
            }
            validate_days(
                "storage.retention.source.maximum_age_days",
                rule.maximum_age_days,
            )?;
            if rule.maximum_capture_mib == 0 && rule.maximum_age_days == 0 {
                return Err(SettingsError::InvalidValue {
                    field: "storage.retention.source",
                    message: format!(
                        "the rule for {} sets no limit; remove it or give it one",
                        rule.matches
                    ),
                });
            }
            per_source.push((
                rule.matches.clone(),
                optional_mib(rule.maximum_capture_mib)?,
                optional_days(rule.maximum_age_days),
            ));
        }
        let global_maximum_capture_bytes = optional_mib(retention.maximum_total_capture_mib)?;
        let global_maximum_age = optional_days(retention.maximum_age_days);
        // Enabling retention without a limit would read as "deletion is on"
        // while nothing is ever selected. Reject it instead.
        if retention.enabled
            && global_maximum_capture_bytes.is_none()
            && global_maximum_age.is_none()
            && per_source.is_empty()
        {
            return Err(SettingsError::InvalidValue {
                field: "storage.retention.enabled",
                message: "set a size or age limit, globally or per source, before enabling \
retention"
                    .into(),
            });
        }
        Ok(ValidatedStorage {
            reserve_bytes: self.storage.reserve_mib.saturating_mul(MIB),
            retention_enabled: retention.enabled,
            global_maximum_capture_bytes,
            global_maximum_age,
            per_source,
        })
    }
}

/// Runtime form of `[storage]`, with 0 already resolved to "no limit".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedStorage {
    pub reserve_bytes: u64,
    pub retention_enabled: bool,
    pub global_maximum_capture_bytes: Option<u64>,
    pub global_maximum_age: Option<Duration>,
    /// Source name or UUID, size limit, age limit.
    pub per_source: Vec<(String, Option<u64>, Option<Duration>)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedSettings {
    pub settings: Settings,
    pub row_cache_bytes: u64,
    pub membership_bytes: u64,
    pub disk_total_bytes: u64,
    pub index_per_source_bytes: u64,
    pub storage: ValidatedStorage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsOrigin {
    Default,
    GlobalFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSettings {
    pub validated: ValidatedSettings,
    pub origin: SettingsOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueSource {
    Default,
    GlobalFile,
    Environment(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveValue<T> {
    pub value: T,
    pub source: ValueSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveSettings {
    pub provider: EffectiveValue<String>,
    pub mode: EffectiveValue<String>,
    pub thinking: EffectiveValue<String>,
    pub delight_enabled: EffectiveValue<bool>,
    pub reduced_motion: EffectiveValue<bool>,
    pub ascii: EffectiveValue<bool>,
    pub theme: EffectiveValue<Theme>,
    pub display_zone: EffectiveValue<String>,
    pub row_cache_bytes: EffectiveValue<u64>,
    pub membership_bytes: EffectiveValue<u64>,
    pub disk_total_bytes: EffectiveValue<u64>,
    pub index_per_source_bytes: EffectiveValue<u64>,
}

impl LoadedSettings {
    pub fn effective(&self) -> Result<EffectiveSettings, SettingsError> {
        self.effective_with(|name| env::var_os(name))
    }

    pub fn effective_with(
        &self,
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Result<EffectiveSettings, SettingsError> {
        let base = match self.origin {
            SettingsOrigin::Default => ValueSource::Default,
            SettingsOrigin::GlobalFile => ValueSource::GlobalFile,
        };
        let settings = &self.validated.settings;
        Ok(EffectiveSettings {
            provider: effective_text(
                &mut lookup,
                ENV_AI_PROVIDER,
                &settings.paseo.provider,
                &base,
            )?,
            mode: effective_text(&mut lookup, ENV_AI_MODE, &settings.paseo.mode, &base)?,
            thinking: effective_text(
                &mut lookup,
                ENV_AI_THINKING,
                &settings.paseo.thinking,
                &base,
            )?,
            delight_enabled: effective_presence(
                &mut lookup,
                ENV_NO_DELIGHT,
                settings.appearance.delight_enabled,
                false,
                &base,
            ),
            reduced_motion: effective_presence(
                &mut lookup,
                ENV_REDUCED_MOTION,
                settings.appearance.reduced_motion,
                true,
                &base,
            ),
            ascii: effective_presence(
                &mut lookup,
                ENV_ASCII,
                settings.appearance.ascii,
                true,
                &base,
            ),
            theme: EffectiveValue {
                value: settings.appearance.theme,
                source: base.clone(),
            },
            // No environment override: a display offset is a deliberate choice,
            // and an env var that silently reinterpreted every timestamp is
            // exactly the ambiguity this setting exists to remove.
            display_zone: EffectiveValue {
                value: settings.appearance.display_zone.clone(),
                source: base.clone(),
            },
            row_cache_bytes: EffectiveValue {
                value: self.validated.row_cache_bytes,
                source: base.clone(),
            },
            membership_bytes: EffectiveValue {
                value: self.validated.membership_bytes,
                source: base.clone(),
            },
            disk_total_bytes: EffectiveValue {
                value: self.validated.disk_total_bytes,
                source: base.clone(),
            },
            index_per_source_bytes: EffectiveValue {
                value: self.validated.index_per_source_bytes,
                source: base,
            },
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestartRequired {
    pub row_cache: bool,
    pub membership: bool,
    pub disk_total: bool,
    pub index_per_source: bool,
}

pub fn restart_required(applied: &ValidatedSettings, saved: &ValidatedSettings) -> RestartRequired {
    RestartRequired {
        row_cache: applied.row_cache_bytes != saved.row_cache_bytes,
        membership: applied.membership_bytes != saved.membership_bytes,
        disk_total: applied.disk_total_bytes != saved.disk_total_bytes,
        index_per_source: applied.index_per_source_bytes != saved.index_per_source_bytes,
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings I/O: {0}")]
    Io(#[from] io::Error),
    #[error("settings TOML: {0}")]
    Toml(String),
    #[error("unsupported settings schema version {0}")]
    UnsupportedSchema(u32),
    #[error("invalid {field}: {message}")]
    InvalidValue {
        field: &'static str,
        message: String,
    },
    #[error("settings file exceeds 64 KiB")]
    TooLarge,
    #[error("settings path exists but is not a regular file")]
    NotRegularFile,
    #[error("an absolute home directory is required for XDG fallbacks")]
    MissingAbsoluteHome,
}

pub fn default_settings_toml() -> Result<String, SettingsError> {
    toml::to_string_pretty(&Settings::default())
        .map_err(|error| SettingsError::Toml(error.to_string()))
}

pub fn load_settings(path: impl AsRef<Path>) -> Result<LoadedSettings, SettingsError> {
    let path = path.as_ref();
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedSettings {
                validated: Settings::default().validate()?,
                origin: SettingsOrigin::Default,
            });
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    validate_regular(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?
        .take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|error| SettingsError::Toml(error.to_string()))?;
    let settings: Settings =
        toml::from_str(text).map_err(|error| SettingsError::Toml(error.to_string()))?;
    Ok(LoadedSettings {
        validated: settings.validate()?,
        origin: SettingsOrigin::GlobalFile,
    })
}

pub fn save_settings(path: impl AsRef<Path>, settings: &Settings) -> Result<(), SettingsError> {
    settings.validate()?;
    let path = path.as_ref();
    let directory = path.parent().ok_or_else(|| {
        SettingsError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "settings path has no parent",
        ))
    })?;
    fs::create_dir_all(directory)?;
    let lock_path = directory.join(".settings.toml.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "settings are being saved by another process; retry after it finishes",
                    )
                    .into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }

    // This check and the eventual rename share the cooperative lock. A second
    // application instance therefore cannot validate one version and replace a
    // malformed or future version published before its rename.
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let _ = load_settings(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let encoded =
        toml::to_string_pretty(settings).map_err(|error| SettingsError::Toml(error.to_string()))?;
    if encoded.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    let (temporary, mut file) = create_owned_temporary(directory)?;
    let result = (|| {
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(directory)?;
        Ok::<_, io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(SettingsError::Io)
}

fn create_owned_temporary(directory: &Path) -> io::Result<(PathBuf, fs::File)> {
    let process = std::process::id();
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".settings.toml.tmp.{process}.{sequence}"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an owned settings temporary file",
    ))
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

fn xdg_base(lookup: &mut impl FnMut(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    lookup(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn validate_regular(path: &Path) -> Result<(), SettingsError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(SettingsError::NotRegularFile);
    }
    Ok(())
}
fn validate_text(field: &'static str, value: &str) -> Result<(), SettingsError> {
    if value.trim().is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(SettingsError::InvalidValue {
            field,
            message: format!(
                "must be nonempty, control-free, and at most {MAX_TEXT_BYTES} UTF-8 bytes"
            ),
        });
    }
    Ok(())
}
fn validate_mib(field: &'static str, value: u64, maximum: u64) -> Result<(), SettingsError> {
    if value == 0 || value > maximum {
        return Err(SettingsError::InvalidValue {
            field,
            message: format!("must be between 1 and {maximum} MiB"),
        });
    }
    Ok(())
}
fn validate_days(field: &'static str, value: u64) -> Result<(), SettingsError> {
    if value > MAX_RETENTION_DAYS {
        return Err(SettingsError::InvalidValue {
            field,
            message: format!("must be between 0 and {MAX_RETENTION_DAYS} days"),
        });
    }
    Ok(())
}
fn optional_mib(value: u64) -> Result<Option<u64>, SettingsError> {
    if value == 0 {
        return Ok(None);
    }
    checked_mib(value).map(Some)
}
fn optional_days(value: u64) -> Option<Duration> {
    (value != 0).then(|| Duration::from_secs(value.saturating_mul(SECONDS_PER_DAY)))
}
fn checked_mib(value: u64) -> Result<u64, SettingsError> {
    value.checked_mul(MIB).ok_or(SettingsError::InvalidValue {
        field: "cache",
        message: "MiB conversion overflow".into(),
    })
}
fn effective_text(
    lookup: &mut impl FnMut(&str) -> Option<OsString>,
    name: &'static str,
    fallback: &str,
    source: &ValueSource,
) -> Result<EffectiveValue<String>, SettingsError> {
    let Some(value) = lookup(name) else {
        return Ok(EffectiveValue {
            value: fallback.into(),
            source: source.clone(),
        });
    };
    let value = value
        .into_string()
        .map_err(|_| SettingsError::InvalidValue {
            field: "environment",
            message: format!("{name} is not UTF-8"),
        })?;
    validate_text("environment", &value)?;
    Ok(EffectiveValue {
        value,
        source: ValueSource::Environment(name),
    })
}
fn effective_presence(
    lookup: &mut impl FnMut(&str) -> Option<OsString>,
    name: &'static str,
    fallback: bool,
    when_present: bool,
    source: &ValueSource,
) -> EffectiveValue<bool> {
    if lookup(name).is_none() {
        EffectiveValue {
            value: fallback,
            source: source.clone(),
        }
    } else {
        EffectiveValue {
            value: when_present,
            source: ValueSource::Environment(name),
        }
    }
}
