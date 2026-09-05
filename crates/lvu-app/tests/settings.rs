#[path = "../src/settings.rs"]
#[allow(dead_code)]
mod settings;

use settings::*;
use std::{collections::HashMap, ffi::OsString, fs};
use tempfile::TempDir;

fn settings_file(root: &TempDir) -> std::path::PathBuf {
    root.path().join("config/lvu/settings.toml")
}

#[test]
fn defaults_validate_and_emit_the_documented_toml_sections() {
    let value = Settings::default();
    let validated = value.validate().unwrap();
    assert_eq!(validated.row_cache_bytes, 4 * MIB);
    assert_eq!(validated.membership_bytes, 256 * MIB);
    assert_eq!(validated.disk_total_bytes, 5_120 * MIB);
    assert_eq!(validated.index_per_source_bytes, 256 * MIB);
    let text = default_settings_toml().unwrap();
    for section in ["[paseo]", "[appearance]", "[cache.memory]", "[cache.disk]"] {
        assert!(text.contains(section), "missing {section} in:\n{text}");
    }
    assert!(text.contains("provider = \"codex/gpt-5.6-luna\""));
    assert!(text.contains("theme = \"terminal\""));
    assert!(text.contains("total_mib = 5120"));
}

#[test]
fn missing_load_and_unicode_round_trip_report_their_origin() {
    let root = TempDir::new().unwrap();
    let path = settings_file(&root);
    let missing = load_settings(&path).unwrap();
    assert_eq!(missing.origin, SettingsOrigin::Default);
    let mut value = Settings::default();
    value.paseo.provider = "ローカル/模型-α".into();
    value.paseo.mode = "完全アクセス".into();
    value.appearance.theme = Theme::LoveDark;
    value.cache.memory.rows_mib = 1;
    value.cache.disk.total_mib = 512;
    value.cache.disk.index_per_source_mib = 64;
    save_settings(&path, &value).unwrap();
    let loaded = load_settings(&path).unwrap();
    assert_eq!(loaded.origin, SettingsOrigin::GlobalFile);
    assert_eq!(loaded.validated.settings, value);
    assert_eq!(loaded.validated.row_cache_bytes, MIB);
}

#[test]
fn validation_bounds_ids_overflow_and_disk_relationship() {
    let mut value = Settings::default();
    value.paseo.provider.clear();
    assert!(value.validate().is_err());
    value.paseo.provider = "x".repeat(257);
    assert!(value.validate().is_err());
    value.paseo.provider = "arbitrary/local-provider".into();
    value.cache.memory.rows_mib = 0;
    assert!(value.validate().is_err());
    value.cache.memory.rows_mib = u64::MAX;
    assert!(value.validate().is_err());
    value.cache.memory.rows_mib = 1;
    value.cache.disk.total_mib = 8;
    value.cache.disk.index_per_source_mib = 9;
    assert!(value.validate().is_err());
}

#[test]
fn malformed_future_unknown_and_oversized_files_are_never_rewritten() {
    let root = TempDir::new().unwrap();
    let path = settings_file(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for invalid in [
        "not = [valid",
        "schema_version = 2\n[paseo]\nprovider='p'\nmode='m'\nthinking='t'\n[appearance]\ntheme='terminal'\ndelight_enabled=true\nreduced_motion=false\nascii=false\n[cache.memory]\nrows_mib=4\nmembership_mib=256\n[cache.disk]\ntotal_mib=5120\nindex_per_source_mib=256\n",
        "schema_version = 1\nunknown = true\n[paseo]\nprovider='p'\nmode='m'\nthinking='t'\n[appearance]\ntheme='terminal'\ndelight_enabled=true\nreduced_motion=false\nascii=false\n[cache.memory]\nrows_mib=4\nmembership_mib=256\n[cache.disk]\ntotal_mib=5120\nindex_per_source_mib=256\n",
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(load_settings(&path).is_err());
        assert!(save_settings(&path, &Settings::default()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
    }
    let oversized = vec![b' '; MAX_SETTINGS_BYTES as usize + 1];
    fs::write(&path, &oversized).unwrap();
    assert!(matches!(load_settings(&path), Err(SettingsError::TooLarge)));
    assert!(save_settings(&path, &Settings::default()).is_err());
    assert_eq!(fs::read(&path).unwrap(), oversized);
}

#[test]
fn xdg_paths_use_absolute_values_and_fall_back_for_empty_or_relative_values() {
    let home = std::path::Path::new("/home/tester");
    let explicit = HashMap::from([
        ("XDG_CONFIG_HOME", OsString::from("/cfg")),
        ("XDG_CACHE_HOME", OsString::from("/cache")),
        ("XDG_DATA_HOME", OsString::from("/data")),
    ]);
    let paths = resolve_paths_with(|name| explicit.get(name).cloned(), Some(home)).unwrap();
    assert_eq!(
        paths.settings_file,
        std::path::Path::new("/cfg/lvu/settings.toml")
    );
    assert_eq!(paths.cache_dir, std::path::Path::new("/cache/lvu"));
    assert_eq!(paths.data_dir, std::path::Path::new("/data/lvu"));
    assert!(resolve_paths_with(|name| explicit.get(name).cloned(), None).is_ok());

    let ignored = HashMap::from([
        ("XDG_CONFIG_HOME", OsString::from("relative")),
        ("XDG_CACHE_HOME", OsString::from("")),
    ]);
    let fallback = resolve_paths_with(|name| ignored.get(name).cloned(), Some(home)).unwrap();
    assert_eq!(
        fallback.settings_file,
        std::path::Path::new("/home/tester/.config/lvu/settings.toml")
    );
    assert_eq!(
        fallback.cache_dir,
        std::path::Path::new("/home/tester/.cache/lvu")
    );
    assert_eq!(
        fallback.data_dir,
        std::path::Path::new("/home/tester/.local/share/lvu")
    );
    assert!(resolve_paths_with(|_| None, Some(std::path::Path::new("relative"))).is_err());
}

#[test]
fn environment_overrides_are_visible_and_never_persisted() {
    let root = TempDir::new().unwrap();
    let path = settings_file(&root);
    save_settings(&path, &Settings::default()).unwrap();
    let loaded = load_settings(&path).unwrap();
    let overrides = HashMap::from([
        (ENV_AI_PROVIDER, OsString::from("custom/模型")),
        (ENV_AI_MODE, OsString::from("read-only")),
        (ENV_AI_THINKING, OsString::from("high")),
        (ENV_NO_DELIGHT, OsString::from("0")),
        (ENV_REDUCED_MOTION, OsString::from("")),
        (ENV_ASCII, OsString::from("0")),
    ]);
    let effective = loaded
        .effective_with(|name| overrides.get(name).cloned())
        .unwrap();
    assert_eq!(effective.provider.value, "custom/模型");
    assert_eq!(
        effective.provider.source,
        ValueSource::Environment(ENV_AI_PROVIDER)
    );
    assert!(!effective.delight_enabled.value);
    assert!(effective.reduced_motion.value);
    assert!(effective.ascii.value);
    assert_eq!(
        effective.delight_enabled.source,
        ValueSource::Environment(ENV_NO_DELIGHT)
    );
    assert_eq!(
        effective.reduced_motion.source,
        ValueSource::Environment(ENV_REDUCED_MOTION)
    );
    assert_eq!(effective.ascii.source, ValueSource::Environment(ENV_ASCII));
    assert_eq!(effective.theme.source, ValueSource::GlobalFile);
    assert_eq!(
        load_settings(&path).unwrap().validated.settings,
        Settings::default()
    );
}

#[test]
fn restart_required_only_tracks_resource_caps() {
    let applied = Settings::default().validate().unwrap();
    let mut changed = Settings::default();
    changed.paseo.provider = "another/provider".into();
    changed.appearance.theme = Theme::LoveLight;
    changed.cache.memory.membership_mib += 1;
    changed.cache.disk.total_mib += 1;
    let required = restart_required(&applied, &changed.validate().unwrap());
    assert_eq!(
        required,
        RestartRequired {
            membership: true,
            disk_total: true,
            ..RestartRequired::default()
        }
    );
}

#[test]
fn stale_legacy_temporary_does_not_block_atomic_publication() {
    let root = TempDir::new().unwrap();
    let original = Settings::default();
    let path = settings_file(&root);
    save_settings(&path, &original).unwrap();
    let stale = path.parent().unwrap().join(".settings.toml.tmp");
    fs::write(&stale, b"occupied").unwrap();
    let mut replacement = original.clone();
    replacement.appearance.theme = Theme::LoveLight;
    save_settings(&path, &replacement).unwrap();
    assert_eq!(
        load_settings(&path).unwrap().validated.settings,
        replacement
    );
    assert_eq!(fs::read(&stale).unwrap(), b"occupied");
}

#[test]
fn concurrent_saves_cannot_bypass_malformed_file_protection() {
    use std::sync::{Arc, Barrier};

    let root = TempDir::new().unwrap();
    let path = settings_file(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let malformed = b"schema_version = [broken";
    fs::write(&path, malformed).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                save_settings(path, &Settings::default())
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        assert!(thread.join().unwrap().is_err());
    }
    assert_eq!(fs::read(&path).unwrap(), malformed);
}

#[cfg(unix)]
#[test]
fn symlink_is_not_accepted_as_a_settings_file() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = root.path().join("outside.toml");
    fs::write(&outside, "unchanged").unwrap();
    let path = settings_file(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(matches!(
        load_settings(&path),
        Err(SettingsError::NotRegularFile)
    ));
    assert!(save_settings(&path, &Settings::default()).is_err());
    assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
}

#[test]
fn held_global_save_lock_returns_without_replacing_settings() {
    use fs2::FileExt;
    let root = TempDir::new().unwrap();
    let path = settings_file(&root);
    save_settings(&path, &Settings::default()).unwrap();
    let original = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.parent().unwrap().join(".settings.toml.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let started = std::time::Instant::now();
    assert!(save_settings(&path, &Settings::default()).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(fs::read(&path).unwrap(), original);
}
