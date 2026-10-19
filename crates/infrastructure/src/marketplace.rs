//! The marketplace: Open VSX (open-vsx.org), where extensions are searched and downloaded.
//! Microsoft's Marketplace does not allow other programs to use it.

use std::path::Path;

use rustcode_application::MarketExtension;
use serde_json::Value;

use crate::server_install::curl;

const SEARCH_URL: &str = "https://open-vsx.org/api/-/search";

/// Extensions matching `query` (the most downloaded ones when it is empty).
pub(crate) fn search(query: &str) -> Result<Vec<MarketExtension>, String> {
    let encoded: String = query
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    let url = format!("{SEARCH_URL}?query={encoded}&size=30&sortBy=relevance&sortOrder=desc");
    let body = curl(&["--max-time", "30", "-H", "User-Agent: rustcode", &url])?;
    parse_search(&String::from_utf8_lossy(&body))
}

pub(crate) fn parse_search(body: &str) -> Result<Vec<MarketExtension>, String> {
    let root: Value = serde_json::from_str(body).map_err(|e| format!("the marketplace answered with something unexpected: {e}"))?;
    let list = root["extensions"].as_array().ok_or("the marketplace answered with something unexpected")?;
    Ok(list
        .iter()
        .filter_map(|item| {
            let text = |key: &str| item[key].as_str().map(str::to_owned);
            let (namespace, name) = (text("namespace")?, text("name")?);
            Some(MarketExtension {
                id: format!("{namespace}.{name}"),
                name: text("displayName").unwrap_or_else(|| name.clone()),
                publisher: namespace,
                version: text("version").unwrap_or_default(),
                description: text("description").unwrap_or_default(),
                download_url: item["files"]["download"].as_str()?.to_owned(),
            })
        })
        .collect())
}

/// Fetch the package at `url` into `into`.
pub(crate) fn download(url: &str, into: &Path) -> Result<(), String> {
    let target = into.to_string_lossy();
    curl(&["--max-time", "300", "-H", "User-Agent: rustcode", "--output", &target, url])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_answers_are_read() {
        let body = r#"{"extensions":[{"namespace":"dart-code","name":"dart-code","displayName":"Dart","version":"3.1.0","description":"Dart language support","files":{"download":"https://x/y.vsix"}},{"name":"no-namespace"}]}"#;
        let found = parse_search(body).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "dart-code.dart-code");
        assert_eq!(found[0].name, "Dart");
        assert_eq!(found[0].download_url, "https://x/y.vsix");
        assert!(parse_search("[]").is_err());
    }
}
