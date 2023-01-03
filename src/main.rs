use anyhow::bail;
use discord_webhook::client::WebhookClient;
use rss::{Channel, Item};
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let feed_url = dotenv::var("FEED_URL").expect("FEED_URL environment variable not set");
    let feed_name = dotenv::var("FEED_NAME").expect("FEED_NAME environment variable not set");

    check_feed(get_channel_from_url(&feed_url).await?, &feed_name).await?;
    Ok(())
}

async fn get_channel_from_url(feed_url: &str) -> anyhow::Result<Channel> {
    let content = reqwest::get(feed_url).await?.bytes().await?;
    let channel = Channel::read_from(&content[..])?;
    Ok(channel)
}

fn calculate_item_hash(item: &Item) -> u64 {
    let mut hasher = DefaultHasher::new();
    item.link().hash(&mut hasher);
    hasher.finish()
}

async fn send_webhook(content: &Item, feed_name: &str) -> anyhow::Result<()> {
    let webhook_url = dotenv::var("WEBHOOK_URL")?;
    let client = WebhookClient::new(&webhook_url);
    if let Err(err) = client
        .send(|message| {
            message.username(feed_name).embed(|embed| {
                embed
                    .title(content.title().unwrap_or("Unknown"))
                    .description(content.description().unwrap_or("Unknown"))
                    .url(content.link().unwrap_or("https://youtu.be/dQw4w9WgXcQ"))
                    .footer(&format!("{:?}", chrono::offset::Local::now()), None)
            })
        })
        .await
    {
        bail!(err)
    };
    Ok(())
}

async fn check_feed(channel: Channel, feed_name: &str) -> anyhow::Result<()> {
    let file_path = PathBuf::from(format!("./{feed_name}"));

    let hashes: Vec<_> = channel.items().iter().map(calculate_item_hash).collect();
    if file_path.exists() {
        let stored_hashes: Vec<u64> = ron::from_str(&fs::read_to_string(&file_path)?)?;
        let stored_hashes: HashSet<u64> = stored_hashes.into_iter().collect();
        for (index, _) in hashes
            .iter()
            .enumerate()
            .filter(|(_, hash)| !stored_hashes.contains(hash))
        {
            send_webhook(&channel.items()[index], feed_name).await?;
        }
    }
    fs::write(file_path, ron::to_string(&hashes)?)?;
    Ok(())
}