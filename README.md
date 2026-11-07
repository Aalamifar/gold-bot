# Gold Price Monitor Bot

A Telegram bot written in **Rust** that monitors gold-price pages, tracks short-term price movements, and sends alerts when a configured threshold is reached.

The project uses asynchronous HTTP requests, HTML parsing, a sliding observation window, and SQLite to manage subscribers.

> **Important:** Alerts describe price movements detected by the configured source. They are informational only, not investment advice or a recommendation to buy or sell.

## Features

- Periodically fetches prices from configured websites.
- Checks configured sources concurrently; one failed source does not stop the others.
- Extracts prices from HTML using a CSS selector or a text label.
- Normalizes Persian, Arabic, and English digits and common price separators.
- Detects upward and downward movements against the minimum or maximum price in a sliding time window.
- Requires consecutive confirmations before sending an alert.
- Quarantines unusually large one-step changes until a follow-up check confirms them.
- Applies a separate cooldown for upward and downward alerts per source.
- Combines alerts from one monitoring cycle into a single Telegram message.
- Stores subscriber chat IDs in SQLite.
- Removes users who have blocked the bot or whose chats are no longer available.
- Supports separate network settings for Telegram and price websites.

## Telegram Commands

| Command | Description |
|---|---|
| `/start` | Subscribe to price alerts |
| `/stop` | Unsubscribe from alerts |
| `/price` | Show the latest successfully fetched prices |
| `/help` | Show available commands |

## Tech Stack

| Area | Technology |
|---|---|
| Language | Rust (2021 edition) |
| Telegram bot | Teloxide 0.13 |
| Async runtime | Tokio |
| HTTP clients | Reqwest |
| HTML parsing | Scraper |
| Local storage | SQLite via rusqlite |
| Date and time | Chrono, chrono-tz |
| Logging | log, pretty_env_logger |

## How It Works

1. On each monitoring cycle, the bot fetches the configured price sources concurrently.
2. It parses the current price and compares it with the minimum or maximum observed inside the sliding window.
3. A movement must meet the configured percentage threshold.
4. The same direction must be detected for the configured number of consecutive checks.
5. Large single-step changes are held for another check to reduce false alerts caused by parsing errors or unexpected page changes.
6. A per-source, per-direction cooldown prevents repeated notifications.
7. Alerts detected during the same cycle are combined and broadcast to subscribers.

An upward movement is labelled as an upward-price alert; a downward movement is labelled as a downward-price alert. These labels do not imply that a trade should be made.

## Requirements

- Rust stable toolchain and Cargo
- A Telegram bot token from [@BotFather](https://t.me/BotFather)
- Network access to the configured price websites
- Network access to the Telegram Bot API, directly or through a supported proxy/API endpoint

## Setup

### 1. Clone the repository

```bash
git clone https://github.com/Aalamifar/gold-bot.git
cd gold-bot
```

### 2. Configure environment variables

The repository does not include a ready-made `.env.example` file. Create a `.env` file in the project root:

```dotenv
TELOXIDE_TOKEN=your_telegram_bot_token

# Monitoring settings
FLUCT_THRESHOLD=1.5
WINDOW_MINUTES=5
CHECK_INTERVAL_SEC=60
ALERT_COOLDOWN_SEC=900
CONFIRM_TICKS=2
MAX_JUMP_PCT=15.0

# Local SQLite database
DB_PATH=users.db

# Optional network settings
TELEGRAM_PROXY=
SITES_PROXY=
TELEGRAM_API_URL=

# Optional logging
RUST_LOG=info
```

The following defaults are used when optional monitoring variables are omitted:

| Variable | Default | Meaning |
|---|---:|---|
| `FLUCT_THRESHOLD` | `1.5` | Percentage movement required to trigger detection |
| `WINDOW_MINUTES` | `5` | Sliding observation window |
| `CHECK_INTERVAL_SEC` | `60` | Time between monitoring cycles (minimum: 5 seconds) |
| `ALERT_COOLDOWN_SEC` | `900` | Cooldown for each source and direction |
| `CONFIRM_TICKS` | `2` | Consecutive checks required to confirm a movement |
| `MAX_JUMP_PCT` | `15.0` | Large one-step change threshold |
| `DB_PATH` | `users.db` | SQLite database path |

`TELOXIDE_TOKEN` is required. Keep it private and never commit your real `.env` file.

### 3. Configure price sources

Edit `src/sources.rs` and configure each `Source` with:

- A unique display name
- The page URL
- An extraction strategy (`Extract::Css` or `Extract::After`)
- A divisor to normalize the source value to toman (use `10.0` when the page reports rial)

Only the source entries currently enabled in `sources()` are monitored. Additional sources can be added using the same structure.

Before adding a website, review its terms of service and `robots.txt`, and make sure your polling frequency is appropriate.

### 4. Build and run

```bash
cargo run --release
```

The SQLite database is created automatically at the configured `DB_PATH` when the bot starts.

## Network Configuration

Telegram and price websites can use separate network routes. This is useful when they require different connectivity settings.

### Telegram proxy

Set `TELEGRAM_PROXY` to route Telegram API requests through a proxy. Examples:

```dotenv
TELEGRAM_PROXY=socks5h://127.0.0.1:10808
# or
TELEGRAM_PROXY=http://127.0.0.1:7890
```

### Price-site proxy

Set `SITES_PROXY` only when price websites need to be reached through a proxy:

```dotenv
SITES_PROXY=http://127.0.0.1:7890
```

When it is not set, the price-site HTTP client connects directly and explicitly ignores system `HTTP_PROXY` / `ALL_PROXY` settings.

### Alternative Telegram API endpoint

If you operate a trusted Telegram Bot API-compatible proxy, `TELEGRAM_API_URL` can point the bot to that endpoint. Do not use an endpoint unless you trust its operator.

## Adding a Price Source

Add a new `Source` entry to the `sources()` function in `src/sources.rs`.

Example using a CSS selector:

```rust
Source {
    name: "Example Source",
    url: "https://example.com/gold-price",
    extract: Extract::Css(".gold-price"),
    divisor: 1.0,
}
```

You can also use `Extract::After("price label")` when the price appears near a stable text label rather than inside a reliable CSS element.

The parser supports Persian, Arabic, and English digits, as well as common thousands and decimal separators. Prefer a precise selector or label to avoid accidentally extracting unrelated numbers.

## Tests

Run the test suite with:

```bash
cargo test
```

The tests cover price parsing, HTML extraction, proxy configuration, subscriber storage, movement detection, confirmation checks, sliding-window behavior, cooldowns, and suspicious price changes.

## Project Structure

```text
src/
├── bot.rs       # Telegram commands and message handling
├── db.rs        # SQLite subscriber storage
├── main.rs      # Configuration and application startup
├── monitor.rs   # Price monitoring and alert detection
├── sources.rs   # Price sources, HTTP fetching, and HTML parsing
└── util.rs      # Formatting and timezone helpers
```

## License

This project is licensed under the [MIT License](LICENSE).
