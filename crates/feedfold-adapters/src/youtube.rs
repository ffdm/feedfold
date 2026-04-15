use feedfold_core::adapter::{AdapterError, FetchedFeed, SourceAdapter};
use feedfold_core::config::AdapterType;

use crate::rss::RssAdapter;

pub struct YoutubeAdapter {
    rss: RssAdapter,
}

impl YoutubeAdapter {
    pub fn new() -> Self {
        Self {
            rss: RssAdapter::new(),
        }
    }
}

impl Default for YoutubeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for YoutubeAdapter {
    fn kind(&self) -> AdapterType {
        AdapterType::Youtube
    }

    async fn fetch(&self, url: &str) -> Result<FetchedFeed, AdapterError> {
        self.rss.fetch(url).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_reports_kind_as_youtube() {
        let adapter = YoutubeAdapter::new();
        assert_eq!(adapter.kind(), AdapterType::Youtube);
    }
}
