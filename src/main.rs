use anyhow::{bail, Context};
use rss::Enclosure;
use serde_json::json;
use std::{
    collections::HashSet,
    fs, mem,
    path::{Path, PathBuf},
    time::Duration,
};

#[allow(clippy::large_enum_variant)] // we only have a single instance of this anyways
enum Feed {
    Rss(rss::Channel),
    Atom(atom_syndication::Feed),
}

impl Feed {
    fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        rss::Channel::read_from(bytes)
            .map(Self::Rss)
            .or_else(|rss_err| {
                atom_syndication::Feed::read_from(bytes)
                    .map(Self::Atom)
                    .map_err(anyhow::Error::from)
                    .with_context(|| format!("feed is neither valid RSS ({rss_err}) nor Atom"))
            })
    }

    fn items(&self) -> Vec<Item> {
        match self {
            Feed::Rss(channel) => channel.items().iter().cloned().map(Item::Rss).collect(),
            Feed::Atom(feed) => feed.entries().iter().cloned().map(Item::Atom).collect(),
        }
    }
}

enum Item {
    Rss(rss::Item),
    Atom(atom_syndication::Entry),
}

impl Item {
    fn title(&self) -> Option<&str> {
        match self {
            Item::Rss(item) => item.title(),
            Item::Atom(entry) => Some(entry.title().value.as_str()),
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            Item::Rss(item) => item.description(),
            Item::Atom(entry) => entry.content().and_then(|content| content.value.as_deref()),
        }
    }

    fn link(&self) -> Option<&str> {
        match self {
            Item::Rss(item) => item.link(),
            Item::Atom(entry) => entry
                .links()
                .iter()
                .find(|link| link.rel == "alternate")
                .map(|link| link.href.as_str()),
        }
    }

    /// A stable identity used to tell which items we already notified about. Prefer the
    /// link, then the guid/id, then the title, so feeds without links still deduplicate.
    fn id(&self) -> Option<&str> {
        match self {
            Item::Rss(item) => self
                .link()
                .or_else(|| item.guid().map(|guid| guid.value()))
                .or_else(|| item.title()),
            Item::Atom(entry) => self
                .link()
                .or_else(|| Some(entry.id()).filter(|id| !id.is_empty()))
                .or_else(|| Some(entry.title().value.as_str())),
        }
    }

    fn enclosure(&self) -> Option<Enclosure> {
        match self {
            Item::Rss(item) => item.enclosure().cloned(),
            Item::Atom(entry) => entry
                .links()
                .iter()
                .find(|link| link.rel == "enclosure")
                .map(|link| Enclosure {
                    url: link.href().to_owned(),
                    length: link.length().unwrap_or("0").to_owned(),
                    mime_type: link
                        .mime_type()
                        .unwrap_or("application/octet-stream")
                        .to_owned(),
                }),
        }
    }
}

struct Config {
    feed_url: String,
    feed_name: String,
    webhook_url: String,
    message_content: Option<String>,
    feed_is_html: bool,
    replacements: Vec<(String, String)>,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            feed_url: required_var("FEED_URL")?,
            feed_name: required_var("FEED_NAME")?,
            webhook_url: required_var("WEBHOOK_URL")?,
            message_content: dotenvy::var("MESSAGE_CONTENT").ok(),
            feed_is_html: env_flag("FEED_IS_HTML"),
            replacements: parse_replacements(&dotenvy::var("RSS_REPLACEMENTS").unwrap_or_default()),
        })
    }
}

fn required_var(key: &str) -> anyhow::Result<String> {
    dotenvy::var(key).with_context(|| format!("{key} environment variable not set"))
}

/// Treats the variable as enabled only for explicit truthy values, so `FEED_IS_HTML=false`
/// does what it says instead of silently turning the feature on.
fn env_flag(key: &str) -> bool {
    dotenvy::var(key)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("rss-discord/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;

    let feed = get_feed_from_url(&client, &config.feed_url).await?;
    check_feed(&client, feed, &config).await
}

async fn get_feed_from_url(client: &reqwest::Client, feed_url: &str) -> anyhow::Result<Feed> {
    let response = client
        .get(feed_url)
        .send()
        .await
        .with_context(|| format!("failed to fetch feed {feed_url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("feed {feed_url} returned {status}");
    }
    Feed::from_bytes(&response.bytes().await?)
}

fn cache_path(feed_name: &str) -> PathBuf {
    let safe: String = feed_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    PathBuf::from(format!("./{safe}.cache"))
}

/// Writes through a temporary file so an interrupted run cannot leave a corrupt cache behind.
fn write_cache(path: &Path, ids: &[String]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, ron::to_string(ids)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

async fn check_feed(client: &reqwest::Client, feed: Feed, config: &Config) -> anyhow::Result<()> {
    let cache_path = cache_path(&config.feed_name);
    let items = feed.items();
    let ids: Vec<String> = items
        .iter()
        .filter_map(|item| item.id().map(str::to_owned))
        .collect();

    // A missing or unreadable/outdated cache means first run after install or upgrade:
    // seed it silently instead of spamming every item.
    let stored: Option<HashSet<String>> = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|content| ron::from_str::<Vec<String>>(&content).ok())
        .map(|ids| ids.into_iter().collect());

    let Some(stored) = stored else {
        write_cache(&cache_path, &ids)?;
        return Ok(());
    };

    let mut handled = HashSet::new();
    let mut failed = HashSet::new();
    for item in &items {
        let Some(id) = item.id() else { continue };
        // Skip items we already sent, or duplicates within the same feed.
        if stored.contains(id) || !handled.insert(id) {
            continue;
        }
        if let Err(err) = send_webhook(client, item, config).await {
            eprintln!("error: could not send {id:?}: {err:#}");
            failed.insert(id.to_owned());
        }
    }

    // Keep failed items out of the cache so the next run retries them.
    let sent: Vec<String> = ids
        .iter()
        .filter(|id| !failed.contains(*id))
        .cloned()
        .collect();
    write_cache(&cache_path, &sent)?;

    if !failed.is_empty() {
        bail!("{} item(s) failed to send", failed.len());
    }
    Ok(())
}

async fn send_webhook(
    client: &reqwest::Client,
    item: &Item,
    config: &Config,
) -> anyhow::Result<()> {
    let mut description = match item.description() {
        Some(description) if config.feed_is_html => html2md::parse_html(description),
        Some(description) => description.to_owned(),
        None => "Unknown".to_owned(),
    };
    for (search, replacement) in &config.replacements {
        description = description.replace(search, replacement);
    }

    let mut payload = json!({
        "username": truncate(&config.feed_name, 80),
        "embeds": [{
            "title": truncate(item.title().unwrap_or("Unknown"), 256),
            "description": truncate(&description, 4096),
            "url": item.link().unwrap_or("https://youtu.be/dQw4w9WgXcQ"),
            "footer": { "text": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string() },
        }],
    });
    if let Some(content) = &config.message_content {
        payload["content"] = json!(truncate(content, 2000));
    }
    if let Some(enclosure) = item.enclosure() {
        if enclosure.mime_type.starts_with("image") {
            payload["embeds"][0]["image"] = json!({ "url": enclosure.url });
        }
    }

    // Discord rate limits webhooks; respect `retry_after` and retry a couple of times.
    let mut attempt = 0;
    loop {
        let response = client
            .post(&config.webhook_url)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
            attempt += 1;
            let retry_after = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value["retry_after"].as_f64())
                .unwrap_or(1.0);
            tokio::time::sleep(Duration::from_secs_f64(retry_after.max(0.1))).await;
            continue;
        }
        bail!("discord webhook returned {status}: {}", body.trim());
    }
}

/// Discord rejects over-long embeds, so trim by characters (not bytes) to stay under the limit.
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Parses `RSS_REPLACEMENTS` (`search/replacement:search/replacement`), where `\` escapes
/// the `:` and `/` separators.
fn parse_replacements(input: &str) -> Vec<(String, String)> {
    let mut escaped = false;
    let mut search = String::new();
    let mut replacement = String::new();
    let mut replacements = vec![];
    for char in input.chars() {
        match (char, escaped) {
            ('\\', false) => {
                escaped = true;
            }
            (':', false) => {
                replacements.push((mem::take(&mut search), mem::take(&mut replacement)));
            }
            ('/', false) => {
                mem::swap(&mut search, &mut replacement);
            }
            (_, true) => {
                replacement.push(char);
                escaped = false;
            }
            (_, false) => {
                replacement.push(char);
            }
        }
    }
    replacements.push((mem::take(&mut search), mem::take(&mut replacement)));
    replacements.retain(|(search, _)| !search.is_empty());
    replacements
}

#[cfg(test)]
mod tests {
    use super::parse_replacements;

    #[test]
    fn parses_multiple_replacements() {
        assert_eq!(
            parse_replacements("a/b:c/d"),
            vec![("a".into(), "b".into()), ("c".into(), "d".into())]
        );
    }

    #[test]
    fn escapes_separators() {
        assert_eq!(
            parse_replacements("a\\/b/c"),
            vec![("a/b".into(), "c".into())]
        );
        assert_eq!(
            parse_replacements("a/b\\:c"),
            vec![("a".into(), "b:c".into())]
        );
    }

    #[test]
    fn ignores_empty_entries() {
        assert!(parse_replacements("").is_empty());
        assert!(parse_replacements("/x:").is_empty());
    }
}
