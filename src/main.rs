use anyhow::{bail, Context};
use discord_webhook::client::WebhookClient;
use rss::Enclosure;
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    fs,
    hash::{Hash, Hasher},
    mem,
    path::PathBuf,
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
            .map_err(anyhow::Error::from)
            .or_else(|err| {
                atom_syndication::Feed::read_from(bytes)
                    .map(Self::Atom)
                    .map_err(anyhow::Error::from)
                    .with_context(|| format!("RSS error: {err}"))
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
            Item::Atom(entry) => entry.summary().map(|text| text.value.as_str()),
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let feed_url = dotenvy::var("FEED_URL").expect("FEED_URL environment variable not set");
    let feed_name = dotenvy::var("FEED_NAME").expect("FEED_NAME environment variable not set");

    check_feed(
        get_feed_from_url(&feed_url).await?,
        &feed_name,
        &parse_replacements(),
    )
    .await?;
    Ok(())
}

async fn get_feed_from_url(feed_url: &str) -> anyhow::Result<Feed> {
    let content = reqwest::get(feed_url).await?.bytes().await?;
    let feed = Feed::from_bytes(&content)?;
    Ok(feed)
}

fn calculate_item_hash(item: &Item) -> u64 {
    let mut hasher = DefaultHasher::new();
    item.link().hash(&mut hasher);
    hasher.finish()
}

async fn send_webhook(
    content: &Item,
    feed_name: &str,
    replacements: &[(String, String)],
) -> anyhow::Result<()> {
    let webhook_url = dotenvy::var("WEBHOOK_URL")?;
    let client = WebhookClient::new(&webhook_url);
    if let Err(err) = client
        .send(|mut message| {
            if let Ok(content) = dotenvy::var("MESSAGE_CONTENT") {
                message = message.content(&content);
            }
            message.username(feed_name).embed(|embed| {
                let description = content.description().unwrap_or("Unknown");
                let mut description = match dotenvy::var("FEED_IS_HTML").is_ok() {
                    true => html2md::parse_html(description),
                    false => description.to_owned(),
                };
                for (search, replacement) in replacements {
                    description = description.replace(search, replacement);
                }
                embed
                    .title(content.title().unwrap_or("Unknown"))
                    .description(&description)
                    .url(content.link().unwrap_or("https://youtu.be/dQw4w9WgXcQ"))
                    .footer(&format!("{:?}", chrono::offset::Local::now()), None);
                if let Some(enclosure) = content.enclosure() {
                    if enclosure.mime_type.starts_with("image") {
                        embed.image(&enclosure.url);
                    }
                };
                embed
            })
        })
        .await
    {
        bail!(err)
    };
    Ok(())
}

async fn check_feed(
    feed: Feed,
    feed_name: &str,
    replacements: &[(String, String)],
) -> anyhow::Result<()> {
    let file_path = PathBuf::from(format!("./{feed_name}.cache"));

    let hashes: Vec<_> = feed.items().iter().map(calculate_item_hash).collect();
    if file_path.exists() {
        let stored_hashes: Vec<u64> = ron::from_str(&fs::read_to_string(&file_path)?)?;
        let stored_hashes: HashSet<u64> = stored_hashes.into_iter().collect();
        for (index, _) in hashes
            .iter()
            .enumerate()
            .filter(|(_, hash)| !stored_hashes.contains(hash))
        {
            send_webhook(&feed.items()[index], feed_name, replacements).await?;
        }
    }
    fs::write(file_path, ron::to_string(&hashes)?)?;
    Ok(())
}

fn parse_replacements() -> Vec<(String, String)> {
    let mut escaped = false;
    let mut search = String::new();
    let mut replacement = String::new();
    let mut replacements = vec![];
    for char in dotenvy::var("RSS_REPLACEMENTS").unwrap_or_default().chars() {
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
    replacements
}
