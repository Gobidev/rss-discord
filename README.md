# rss-discord

A simple rust program to scan rss feeds and send discord pings for new events.

## Installation

Make sure you have [rust](https://rustup.rs/) installed, then clone the repo and
run `cargo build --release` from the project root, the resulting binary will be
located at ./target/release/

## Usage

Run the binary with the following environment variables set:

-   `WEBHOOL_URL`: A discord webhook url to send the notifications to
-   `FEED_URL`: A url pointing to the rss feed that you want to monitor
-   `FEED_NAME`: A name for the rss feed