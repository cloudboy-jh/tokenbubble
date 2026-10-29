use tokenbubble_core::config::{BarFill, ConfigError, RefreshInterval, ResetDisplay, Settings};
use tokenbubble_core::model::ProviderId;

#[test]
fn missing_file_yields_defaults_and_partial_files_fill_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    assert_eq!(Settings::load(&path).unwrap(), Settings::default());

    std::fs::write(&path, "refresh = \"manual\"\nfuture_option = 3\n").unwrap();
    let settings = Settings::load(&path).unwrap();
    assert_eq!(settings.refresh, RefreshInterval::Manual);
    assert!(
        settings.refresh_on_open,
        "unspecified keys keep their defaults"
    );
}

#[test]
fn saved_settings_round_trip_and_replace_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("settings.toml");
    let settings = Settings {
        provider: ProviderId::Claude,
        refresh: RefreshInterval::FifteenMinutes,
        refresh_on_open: false,
        bar_fill: BarFill::Remaining,
        reset_display: ResetDisplay::Clock,
        show_credits: false,
    };
    settings.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap(), settings);
    assert!(!path.with_extension("toml.partial").exists());
}

#[test]
fn invalid_values_are_reported_not_silently_defaulted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    for contents in ["refresh = \"7m\"\n", "provider = \"unknown\"\n"] {
        std::fs::write(&path, contents).unwrap();
        assert!(matches!(Settings::load(&path), Err(ConfigError::Parse(_))));
    }
}
