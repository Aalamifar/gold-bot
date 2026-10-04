use std::sync::Arc;

use teloxide::{prelude::*, utils::command::BotCommands};

use crate::db::Db;
use crate::monitor::SharedPrices;
use crate::util::{fmt_price, tehran};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "دستورها:")]
pub enum Command {
    #[command(description = "عضویت در دریافت هشدارها")]
    Start,
    #[command(description = "لغو عضویت")]
    Stop,
    #[command(description = "نمایش آخرین قیمت همه‌ی منابع")]
    Price,
    #[command(description = "راهنما")]
    Help,
}

/// تا وقتی که task لغو شود (Ctrl-C در main) اجرا می‌شود
pub async fn run(bot: Bot, db: Arc<Db>, prices: SharedPrices) {
    if let Err(e) = bot.set_my_commands(Command::bot_commands()).await {
        log::warn!("set_my_commands ناموفق: {e}");
    }

    let handler = Update::filter_message()
        .filter_command::<Command>()
        .endpoint(handle);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![db, prices])
        .build()
        .dispatch()
        .await;
}

async fn handle(
    bot: Bot,
    msg: Message,
    cmd: Command,
    db: Arc<Db>,
    prices: SharedPrices,
) -> anyhow::Result<()> {
    let chat = msg.chat.id;
    let text = match cmd {
        Command::Start => {
            if db.add_user(chat.0)? {
                "✅ عضو شدی!\nهر وقت نوسان شدید رخ بدهد بهت خبر می‌دهم.\n/price قیمت فعلی · /stop لغو عضویت".to_string()
            } else {
                "از قبل عضو بودی ✅\n/price قیمت فعلی · /stop لغو عضویت".to_string()
            }
        }
        Command::Stop => {
            db.remove_user(chat.0)?;
            "❌ عضویتت لغو شد.".to_string()
        }
        Command::Price => price_text(&prices).await,
        Command::Help => Command::descriptions().to_string(),
    };
    bot.send_message(chat, text).await?;
    Ok(())
}

async fn price_text(prices: &SharedPrices) -> String {
    let map = prices.read().await;
    if map.is_empty() {
        return "⏳ هنوز قیمتی دریافت نشده؛ چند لحظه‌ی دیگر دوباره امتحان کن.".to_string();
    }
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_by_key(|(name, _)| **name);
    rows.iter()
        .map(|(name, info)| {
            format!("🏷 {name}: {} تومان\n   ⏱ {}", fmt_price(info.price), tehran(info.at))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_with_deep_link_payload_still_parses() {
        assert!(matches!(Command::parse("/start ref123", "bot"), Ok(Command::Start)));
        assert!(matches!(Command::parse("/price", "bot"), Ok(Command::Price)));
    }
}
