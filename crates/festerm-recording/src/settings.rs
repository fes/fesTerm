use std::{fmt, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    IdleCompression, LongOutputLimit, NameTemplates, OutputRevealRate, Pacing, PlaybackSpeed,
    TypingSpeed,
};

/// A device-local destination authorization. Native hosts must revalidate it
/// before capture and publication.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExportDestination {
    DesktopPath(#[serde(with = "desktop_path")] PathBuf),
    AndroidTree(String),
    IosBookmark {
        bookmark: Vec<u8>,
        display_name: String,
    },
}

impl fmt::Debug for ExportDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::DesktopPath(_) => "DesktopPath",
            Self::AndroidTree(_) => "AndroidTree",
            Self::IosBookmark { .. } => "IosBookmark",
        };
        formatter.debug_tuple(kind).field(&"<redacted>").finish()
    }
}

mod desktop_path {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize, Serialize)]
    #[serde(tag = "platform", content = "units", rename_all = "snake_case")]
    enum StoredPath {
        Unix(Vec<u8>),
        Windows(Vec<u16>),
    }

    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        #[cfg(unix)]
        let value = {
            use std::os::unix::ffi::OsStrExt;
            StoredPath::Unix(path.as_os_str().as_bytes().to_vec())
        };
        #[cfg(windows)]
        let value = {
            use std::os::windows::ffi::OsStrExt;
            StoredPath::Windows(path.as_os_str().encode_wide().collect())
        };
        #[cfg(not(any(unix, windows)))]
        return Err(serde::ser::Error::custom(
            "desktop recording destinations are unavailable on this platform",
        ));
        #[cfg(any(unix, windows))]
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        let value = StoredPath::deserialize(deserializer)?;
        match value {
            #[cfg(unix)]
            StoredPath::Unix(bytes) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(OsString::from_vec(bytes)))
            }
            #[cfg(windows)]
            StoredPath::Windows(units) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(PathBuf::from(OsString::from_wide(&units)))
            }
            _ => Err(D::Error::custom(
                "the recording destination belongs to a different platform",
            )),
        }
    }
}

/// A recording-wide primary font. Optional families require verified local
/// faces; an unavailable selection must not be silently substituted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingFont {
    #[default]
    JetBrainsMono,
    JetBrainsMonoLigatures,
    IosevkaTerm,
    JuliaMono,
    MapleMono,
}

impl RecordingFont {
    pub const fn label(self) -> &'static str {
        match self {
            Self::JetBrainsMono => "JetBrains Mono",
            Self::JetBrainsMonoLigatures => "JetBrains Mono with ligatures",
            Self::IosevkaTerm => "Iosevka Term",
            Self::JuliaMono => "JuliaMono",
            Self::MapleMono => "Maple Mono",
        }
    }

    pub const fn bundled(self) -> bool {
        matches!(self, Self::JetBrainsMono | Self::JetBrainsMonoLigatures)
    }
}

/// Built-in, versioned recording color schemes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingTheme {
    #[default]
    FesTermDark,
    FesTermLight,
}

impl RecordingTheme {
    pub const fn label(self) -> &'static str {
        match self {
            Self::FesTermDark => "fesTerm dark",
            Self::FesTermLight => "fesTerm light",
        }
    }
}

/// Appearance applied uniformly to every stream in an export.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingAppearance {
    pub font: RecordingFont,
    pub theme: RecordingTheme,
    pub font_size_points: u8,
    pub width: u16,
    pub height: u16,
}

impl Default for RecordingAppearance {
    fn default() -> Self {
        Self {
            font: RecordingFont::default(),
            theme: RecordingTheme::default(),
            font_size_points: 20,
            width: 1920,
            height: 1080,
        }
    }
}

impl RecordingAppearance {
    pub fn validate(self) -> Result<(), String> {
        if !(8..=64).contains(&self.font_size_points) {
            return Err("Recording font size must be between 8 and 64 points".to_owned());
        }
        if self.width < 2
            || self.height < 2
            || self.width > 3840
            || self.height > 2160
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err("Recording dimensions must be even and at most 3840 by 2160".to_owned());
        }
        Ok(())
    }
}

/// Editable device-local defaults. Validation is required before saving or
/// using a snapshot for a recording.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingSettings {
    pub destination: Option<ExportDestination>,
    pub directory_template: String,
    pub session_template: String,
    pub speed: PlaybackSpeed,
    pub compress_idle: bool,
    pub idle_threshold_seconds: u32,
    pub idle_target_millis: u32,
    pub normalize_typing: bool,
    pub typing_characters_per_second: u8,
    pub limit_long_output: bool,
    pub long_output_threshold_seconds: u32,
    pub long_output_target_millis: u32,
    pub reveal_output: bool,
    pub output_lines_per_second: u16,
    pub appearance: RecordingAppearance,
    pub delete_source_after_export: bool,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        let names = NameTemplates::default();
        Self {
            destination: None,
            directory_template: names.directory_template().to_owned(),
            session_template: names.session_template().to_owned(),
            speed: PlaybackSpeed::Normal,
            compress_idle: true,
            idle_threshold_seconds: 3,
            idle_target_millis: 500,
            normalize_typing: false,
            typing_characters_per_second: 15,
            limit_long_output: false,
            long_output_threshold_seconds: 5,
            long_output_target_millis: 2000,
            reveal_output: false,
            output_lines_per_second: 60,
            appearance: RecordingAppearance::default(),
            delete_source_after_export: true,
        }
    }
}

/// A validated, immutable settings snapshot for one synchronized export.
#[derive(Clone, Debug)]
pub struct ValidatedRecordingSettings {
    pub destination: Option<ExportDestination>,
    pub templates: NameTemplates,
    pub pacing: Pacing,
    pub appearance: RecordingAppearance,
    pub delete_source_after_export: bool,
}

impl RecordingSettings {
    pub fn validate(&self) -> Result<ValidatedRecordingSettings, String> {
        self.appearance.validate()?;
        if let Some(destination) = &self.destination {
            match destination {
                ExportDestination::DesktopPath(path)
                    if path.is_absolute()
                        && path.as_os_str().len() <= 32_768
                        && !path.as_os_str().as_encoded_bytes().contains(&0) => {}
                ExportDestination::AndroidTree(uri)
                    if uri.starts_with("content://")
                        && uri.len() <= 8192
                        && uri["content://".len()..]
                            .split('/')
                            .next()
                            .is_some_and(|authority| !authority.is_empty())
                        && !uri.chars().any(char::is_whitespace)
                        && !uri.chars().any(char::is_control) => {}
                ExportDestination::IosBookmark {
                    bookmark,
                    display_name,
                } if !bookmark.is_empty()
                    && bookmark.len() <= 65_536
                    && !display_name.is_empty()
                    && display_name.len() <= 1024
                    && !display_name.chars().any(char::is_control) => {}
                _ => return Err("The recording export location is invalid".to_owned()),
            }
        }
        let templates = NameTemplates::new(&self.directory_template, &self.session_template)
            .map_err(|error| error.to_string())?;
        // Validate even disabled controls so enabling them cannot activate a
        // previously saved invalid value.
        let idle = IdleCompression::new(
            Duration::from_secs(u64::from(self.idle_threshold_seconds)),
            Duration::from_millis(u64::from(self.idle_target_millis)),
        )
        .map_err(|error| error.to_string())?;
        let typing = TypingSpeed::new(self.typing_characters_per_second)
            .map_err(|error| error.to_string())?;
        let long_output = LongOutputLimit::new(
            Duration::from_secs(u64::from(self.long_output_threshold_seconds)),
            Duration::from_millis(u64::from(self.long_output_target_millis)),
        )
        .map_err(|error| error.to_string())?;
        let reveal = OutputRevealRate::new(self.output_lines_per_second)
            .map_err(|error| error.to_string())?;
        Ok(ValidatedRecordingSettings {
            destination: self.destination.clone(),
            templates,
            pacing: Pacing {
                speed: self.speed,
                idle: self.compress_idle.then_some(idle),
                typing: self.normalize_typing.then_some(typing),
                long_output: self.limit_long_output.then_some(long_output),
                reveal: self.reveal_output.then_some(reveal),
            },
            appearance: self.appearance,
            delete_source_after_export: self.delete_source_after_export,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_safe_retention_and_shared_pacing() {
        let settings = RecordingSettings::default().validate().unwrap();
        assert!(settings.destination.is_none());
        assert!(settings.delete_source_after_export);
        assert_eq!(settings.pacing, Pacing::default());
        assert_eq!(settings.templates, NameTemplates::default());
    }

    #[test]
    fn settings_round_trip_and_validate_all_pacing_and_naming_fields() {
        let settings = RecordingSettings {
            destination: Some(ExportDestination::AndroidTree(
                "content://provider/tree/example".into(),
            )),
            normalize_typing: true,
            typing_characters_per_second: 25,
            limit_long_output: true,
            reveal_output: true,
            delete_source_after_export: false,
            ..RecordingSettings::default()
        };
        let bytes = serde_json::to_vec(&settings).unwrap();
        let restored: RecordingSettings = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, settings);
        let validated = restored.validate().unwrap();
        assert_eq!(validated.pacing.typing.unwrap().characters_per_second(), 25);
        assert!(validated.pacing.long_output.is_some());
        assert!(validated.pacing.reveal.is_some());
        assert!(!validated.delete_source_after_export);

        let mut invalid = restored.clone();
        invalid.normalize_typing = false;
        invalid.typing_characters_per_second = 0;
        assert!(invalid.validate().is_err());
        invalid = restored.clone();
        invalid.idle_target_millis = 4000;
        assert!(invalid.validate().is_err());
        invalid = restored.clone();
        invalid.session_template = "session.mp4".into();
        assert!(invalid.validate().is_err());
        invalid = restored;
        invalid.destination = Some(ExportDestination::AndroidTree("file:///wrong".into()));
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn unknown_settings_are_not_silently_discarded() {
        assert!(serde_json::from_str::<RecordingSettings>(r#"{"unknown":true}"#).is_err());
        let settings: RecordingSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.delete_source_after_export);
        assert!(settings.destination.is_none());
        assert_eq!(settings.appearance, RecordingAppearance::default());
    }

    #[test]
    fn appearance_round_trips_and_rejects_invalid_canvas_and_scale() {
        let appearance = RecordingAppearance {
            font: RecordingFont::JetBrainsMonoLigatures,
            theme: RecordingTheme::FesTermLight,
            font_size_points: 32,
            width: 1280,
            height: 720,
        };
        let settings = RecordingSettings {
            appearance,
            ..RecordingSettings::default()
        };
        let restored: RecordingSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored.validate().unwrap().appearance, appearance);
        for invalid in [
            RecordingAppearance {
                width: 1279,
                ..appearance
            },
            RecordingAppearance {
                height: 2162,
                ..appearance
            },
            RecordingAppearance {
                font_size_points: 7,
                ..appearance
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn ios_bookmarks_are_bounded_and_authorizations_are_redacted() {
        let mut settings = RecordingSettings {
            destination: Some(ExportDestination::IosBookmark {
                bookmark: vec![1, 2, 3],
                display_name: "Demos".to_owned(),
            }),
            ..RecordingSettings::default()
        };
        settings.validate().unwrap();
        let encoded = serde_json::to_vec(&settings).unwrap();
        assert_eq!(
            serde_json::from_slice::<RecordingSettings>(&encoded).unwrap(),
            settings
        );
        let debug = format!("{settings:?}");
        assert!(!debug.contains("Demos"));
        assert!(!debug.contains("[1, 2, 3]"));
        settings.destination = Some(ExportDestination::IosBookmark {
            bookmark: Vec::new(),
            display_name: "Demos".to_owned(),
        });
        assert!(settings.validate().is_err());
        settings.destination = Some(ExportDestination::IosBookmark {
            bookmark: vec![1],
            display_name: "bad\nname".to_owned(),
        });
        assert!(settings.validate().is_err());
        let android = ExportDestination::AndroidTree("content://private/example".to_owned());
        assert!(!format!("{android:?}").contains("private"));
    }

    #[cfg(unix)]
    #[test]
    fn desktop_destination_round_trips_non_utf8_paths() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let settings = RecordingSettings {
            destination: Some(ExportDestination::DesktopPath(PathBuf::from(
                OsString::from_vec(b"/tmp/recordings-\xff".to_vec()),
            ))),
            ..RecordingSettings::default()
        };
        settings.validate().unwrap();
        let serialized = serde_json::to_vec(&settings).unwrap();
        let restored: RecordingSettings = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored, settings);
    }
}
