use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Default, Deserialize)]
pub struct Ranking {
    #[serde(default)]
    pub mode: RankingMode,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RankingMode {
    #[default]
    Recency,
    Popularity,
    Claude,
}

#[derive(Debug, Default, Deserialize)]
pub struct Ai {
    #[serde(default)]
    pub interests: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Youtube {}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub name: String,
    pub url: String,
    pub adapter: AdapterType,
    #[serde(default)]
    pub top_n: Option<u32>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AdapterType {
    Rss,
    Youtube,
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

[[sources]]
name    = "Fireship"
url     = "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"
adapter = "youtube"
top_n   = 10
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

        let fireship = &config.sources[1];
        assert_eq!(fireship.name, "Fireship");
        assert_eq!(fireship.adapter, AdapterType::Youtube);
        assert_eq!(fireship.top_n, Some(10));
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
    }
}
