//! Minimal OPML parser for bulk subscription import.
//!
//! OPML files are XML but the only shape that matters here is `<outline>`
//! elements with an `xmlUrl` attribute. We pull those out with a small
//! hand-rolled scanner so we do not take on a full XML dependency.

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpmlFeed {
    pub url: String,
    pub title: Option<String>,
}

impl OpmlFeed {
    pub fn display_name(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.url)
    }
}

pub fn parse(raw: &str) -> Result<Vec<OpmlFeed>> {
    if !raw.contains("<opml") && !raw.contains("<OPML") {
        return Err(anyhow!("file does not look like OPML (missing <opml> root)"));
    }

    let mut feeds = Vec::new();
    let mut cursor = 0;
    while let Some(start) = find_next_outline(raw, cursor) {
        let tag_start = start;
        let tag_end = match raw[tag_start..].find('>') {
            Some(offset) => tag_start + offset + 1,
            None => break,
        };
        let tag = &raw[tag_start..tag_end];
        cursor = tag_end;

        let Some(url) = extract_attr(tag, "xmlUrl") else {
            continue;
        };
        let url = decode_entities(&url);
        if url.is_empty() {
            continue;
        }

        let title = extract_attr(tag, "title")
            .or_else(|| extract_attr(tag, "text"))
            .map(|v| decode_entities(&v))
            .filter(|v| !v.is_empty());

        feeds.push(OpmlFeed { url, title });
    }

    deduplicate(&mut feeds);
    Ok(feeds)
}

pub fn looks_like_youtube_feed(url: &str) -> bool {
    url.contains("youtube.com/feeds/videos.xml")
}

fn find_next_outline(raw: &str, from: usize) -> Option<usize> {
    let haystack = raw.get(from..)?;
    let lowered = haystack.to_ascii_lowercase();
    let idx = lowered.find("<outline")?;
    let after = haystack.as_bytes().get(idx + "<outline".len()).copied();
    match after {
        Some(b) if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'/' || b == b'>' => {
            Some(from + idx)
        }
        _ => None,
    }
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let lowered = tag.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(pos) = lowered[search_from..].find(&needle) {
        let abs = search_from + pos;
        let before_ok = abs == 0
            || matches!(
                lowered.as_bytes()[abs - 1],
                b' ' | b'\t' | b'\n' | b'\r'
            );
        let after = lowered.as_bytes().get(abs + needle.len()).copied();
        if before_ok && matches!(after, Some(b'=') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')) {
            let mut cursor = abs + needle.len();
            while cursor < lowered.len()
                && matches!(
                    lowered.as_bytes()[cursor],
                    b' ' | b'\t' | b'\n' | b'\r'
                )
            {
                cursor += 1;
            }
            if lowered.as_bytes().get(cursor).copied() != Some(b'=') {
                search_from = abs + needle.len();
                continue;
            }
            cursor += 1;
            while cursor < lowered.len()
                && matches!(
                    lowered.as_bytes()[cursor],
                    b' ' | b'\t' | b'\n' | b'\r'
                )
            {
                cursor += 1;
            }
            let quote = lowered.as_bytes().get(cursor).copied()?;
            if quote != b'"' && quote != b'\'' {
                return None;
            }
            cursor += 1;
            let rest = &tag[cursor..];
            let end = rest.find(quote as char)?;
            return Some(rest[..end].to_string());
        }
        search_from = abs + needle.len();
    }
    None
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn deduplicate(feeds: &mut Vec<OpmlFeed>) {
    let mut seen = std::collections::HashSet::new();
    feeds.retain(|feed| seen.insert(feed.url.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_outline_list() {
        let raw = r#"<?xml version="1.0"?>
<opml version="1.0">
<head><title>Subs</title></head>
<body>
<outline text="Simon Willison" title="Simon Willison" type="rss" xmlUrl="https://simonwillison.net/atom/everything/" />
<outline text="Fireship" type="rss" xmlUrl="https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"/>
</body>
</opml>"#;

        let feeds = parse(raw).expect("parse");
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].url, "https://simonwillison.net/atom/everything/");
        assert_eq!(feeds[0].title.as_deref(), Some("Simon Willison"));
        assert_eq!(
            feeds[1].url,
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCsBjURrPoezykLs9EqgamOA"
        );
        assert_eq!(feeds[1].title.as_deref(), Some("Fireship"));
    }

    #[test]
    fn parses_nested_categories() {
        let raw = r#"<opml version="2.0"><body>
<outline text="Tech">
<outline text="Inner" type="rss" xmlUrl="https://inner.example/feed.xml"/>
</outline>
<outline text="Solo" type="rss" xmlUrl="https://solo.example/rss"/>
</body></opml>"#;

        let feeds = parse(raw).expect("parse");
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].url, "https://inner.example/feed.xml");
        assert_eq!(feeds[1].url, "https://solo.example/rss");
    }

    #[test]
    fn skips_outlines_without_xmlurl() {
        let raw = r#"<opml><body>
<outline text="Category only"/>
<outline text="Real" xmlUrl="https://real.example/feed"/>
</body></opml>"#;

        let feeds = parse(raw).expect("parse");
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].url, "https://real.example/feed");
    }

    #[test]
    fn deduplicates_repeated_urls() {
        let raw = r#"<opml><body>
<outline xmlUrl="https://a.example/feed"/>
<outline xmlUrl="https://a.example/feed"/>
</body></opml>"#;

        let feeds = parse(raw).expect("parse");
        assert_eq!(feeds.len(), 1);
    }

    #[test]
    fn decodes_xml_entities_in_title() {
        let raw = r#"<opml><body>
<outline text="Foo &amp; Bar" xmlUrl="https://foo.example/feed?x=1&amp;y=2"/>
</body></opml>"#;

        let feeds = parse(raw).expect("parse");
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].title.as_deref(), Some("Foo & Bar"));
        assert_eq!(feeds[0].url, "https://foo.example/feed?x=1&y=2");
    }

    #[test]
    fn rejects_non_opml_content() {
        let err = parse("<rss><channel/></rss>").unwrap_err();
        assert!(err.to_string().contains("does not look like OPML"));
    }

    #[test]
    fn detects_youtube_feed_urls() {
        assert!(looks_like_youtube_feed(
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC123"
        ));
        assert!(!looks_like_youtube_feed("https://example.com/rss"));
    }
}
