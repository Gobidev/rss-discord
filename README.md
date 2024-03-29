# rss-discord

A simple rust program to scan rss feeds and send discord pings for new events.

## Installation

Make sure you have [rust](https://rustup.rs/) installed, then clone the repo and
run `cargo build --release` from the project root, the resulting binary will be
located at ./target/release/

## Usage

Run the binary with the following environment variables set:

- `WEBHOOK_URL`: A discord webhook url to send the notifications to
- `FEED_URL`: A url pointing to the rss feed that you want to monitor
- `FEED_NAME`: A name for the rss feed
- `MESSAGE_CONTENT`: Optional content for the message, useful for pinging users
  or roles with `<@user_id>` or `<@&role_id>`
- `FEED_IS_HTML`: If this variable is set to any value, the feed description
  will be handled as html
- `RSS_REPLACEMENTS`: A list, separated by `:`, where each entry specifies a
  replacement to apply on the embed's description with the syntax
  `<search>/<replacement>`. Backslashes are used to escape `:` and `/`
  characters.

I recommend running the program periodically (i.e. with cron) to receive updates
of the feed. To check for updates on multiple feeds at once, create multiple
cron jobs with different environment variables.
