use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub ranking: Ranking,
    #[serde(default)]
    pub ai: Ai,
    #[serde(default)]
    pub youtube: Youtube,
    #[serde(default)]
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct General {
    #[serde(default = "default_top_n")]
    pub default_top_n: u32,
    #[serde(default = "default_poll_interval_mins")]
    pub poll_interval_mins: u32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            default_top_n: default_top_n(),
            poll_interval_mins: default_poll_interval_mins(),
        }
    }
}

fn default_top_n() -> u32 {
    3
}

fn default_poll_interval_mins() -> u32 {
    30
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Ranking {
    #[serde(default)]
    pub mode: RankingMode,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RankingMode {
    #[default]
    Recency,
    Popularity,
    Claude,
}

impl std::fmt::Display for RankingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RankingMode::Recency => write!(f, "recency"),
            RankingMode::Popularity => write!(f, "popularity"),
            RankingMode::Claude => write!(f, "claude"),
        }
    }
}

impl RankingMode {
    pub const ALL: [RankingMode; 3] = [
        RankingMode::Recency,
        RankingMode::Popularity,
        RankingMode::Claude,
    ];

    pub fn cycle_next(self) -> Self {
        match self {
            RankingMode::Recency => RankingMode::Popularity,
            RankingMode::Popularity => RankingMode::Claude,
            RankingMode::Claude => RankingMode::Recency,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Ai {
    #[serde(default)]
    pub interests: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Youtube {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub show_shorts: bool,
    #[serde(default)]
    pub show_live: bool,
    #[serde(default)]
    pub show_premieres: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Source {
    pub name: String,
    pub url: String,
    pub adapter: AdapterType,
    #[serde(default)]
    pub top_n: Option<u32>,
    #[serde(default)]
    pub ranking: Option<RankingMode>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum AdapterType {
    Rss,
    Youtube,
}

impl AdapterType {
    pub fn as_canonical_str(self) -> &'static str {
        match self {
            AdapterType::Rss => "rss",
            AdapterType::Youtube => "youtube",
        }
    }

    pub fn from_canonical_str(s: &str) -> Option<Self> {
        match s {
            "rss" => Some(AdapterType::Rss),
            "youtube" => Some(AdapterType::Youtube),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no user config directory could be determined for this platform")]
    NoConfigDir,

    #[error("config file not found at {0}")]
    NotFound(PathBuf),

    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("parsing {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("serializing config")]
    Serialize(#[from] toml::ser::Error),

    #[error("writing {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        let dirs = ProjectDirs::from("", "", "feedfold").ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn parse(raw: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(raw)
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Self::default_path()?)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(Self::default_path()?)
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        let raw = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        fs::write(path, raw).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    pub fn youtube_api_key(&self) -> Option<String> {
        self.youtube
            .api_key
            .clone()
            .or_else(|| std::env::var("YOUTUBE_API_KEY").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[general]
default_top_n      = 5
poll_interval_mins = 15

[ranking]
mode = "popularity"

[ai]
interests = "Rust and systems papers."

[[sources]]
name    = "Simon Willison"
url     = "https://simonwillison.net/atom/everything/"
adapter = "rss"
ranking = "recency"

[[sources]]
name    = "Fireship"
url     = "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"
adapter = "youtube"
top_n   = 10
ranking = "popularity"
"#;

    #[test]
    fn parses_full_sample() {
        let config = Config::parse(SAMPLE).expect("sample parses");

        assert_eq!(config.general.default_top_n, 5);
        assert_eq!(config.general.poll_interval_mins, 15);
        assert_eq!(config.ranking.mode, RankingMode::Popularity);
        assert_eq!(config.ai.interests, "Rust and systems papers.");
        assert_eq!(config.sources.len(), 2);

        let simon = &config.sources[0];
        assert_eq!(simon.name, "Simon Willison");
        assert_eq!(simon.adapter, AdapterType::Rss);
        assert_eq!(simon.top_n, None);
        assert_eq!(simon.ranking, Some(RankingMode::Recency));

        let fireship = &config.sources[1];
        assert_eq!(fireship.name, "Fireship");
        assert_eq!(fireship.adapter, AdapterType::Youtube);
        assert_eq!(fireship.top_n, Some(10));
        assert_eq!(fireship.ranking, Some(RankingMode::Popularity));
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config = Config::parse("").expect("empty parses");

        assert_eq!(config.general.default_top_n, 3);
        assert_eq!(config.general.poll_interval_mins, 30);
        assert_eq!(config.ranking.mode, RankingMode::Recency);
        assert!(config.ai.interests.is_empty());
        assert!(config.sources.is_empty());
    }

    #[test]
    fn per_source_top_n_defaults_to_none() {
        let raw = r#"
[[sources]]
name    = "A Blog"
url     = "https://example.com/feed.xml"
adapter = "rss"
"#;
        let config = Config::parse(raw).expect("parses");

        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].top_n, None);
        assert_eq!(config.sources[0].ranking, None);
    }

    #[test]
    fn per_source_ranking_defaults_to_none() {
        let raw = r#"
[[sources]]
name    = "A Blog"
url     = "https://example.com/feed.xml"
adapter = "rss"
"#;
        let config = Config::parse(raw).expect("parses");

        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].ranking, None);
    }

    #[test]
    fn youtube_api_key_from_config() {
        let raw = r#"
[youtube]
api_key = "AIzaTestKey123"
"#;
        let config = Config::parse(raw).expect("parses");
        assert_eq!(
            config.youtube.api_key.as_deref(),
            Some("AIzaTestKey123")
        );
    }

    #[test]
    fn youtube_api_key_defaults_to_none() {
        let config = Config::parse("").expect("parses");
        assert_eq!(config.youtube.api_key, None);
    }
}
