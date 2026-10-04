use std::env;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use teloxide::{prelude::*, ApiError, RequestError};
use tokio::sync::RwLock;

mod bot;
mod db;
mod monitor;
mod sources;
mod util;

use monitor::{Monitor, Params, SharedPrices};

/// متغیر محیطی را می‌خواند؛ اگر نبود مقدار پیش‌فرض، اگر نامعتبر بود خطای واضح
fn env_or<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env::var(key) {
        Ok(v) => v.trim().parse().with_context(|| format!("مقدار {key} نامعتبر است: {v:?}")),
        Err(_) => Ok(default),
    }
}

/// متغیر محیطی اختیاری؛ مقدار خالی یعنی تنظیم نشده
fn opt_env(key: &str) -> Option<String> {
    env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// کلاینت تلگرام؛ اگر `proxy` داده شود فقط ترافیک تلگرام از آن رد می‌شود
fn telegram_client(proxy: Option<&str>) -> anyhow::Result<reqwest011::Client> {
    let mut b = teloxide::net::default_reqwest_settings();
    if let Some(p) = proxy {
        b = b.proxy(reqwest011::Proxy::all(p).with_context(|| "TELEGRAM_PROXY نامعتبر است")?);
    }
    Ok(b.build()?)
}

/// تا وقتی تلگرام در دسترس نیست (فیلتر/VPN قطع) با backoff دوباره تلاش می‌کند.
/// teloxide اگر موقع شروع به تلگرام نرسد panic می‌کند، پس dispatcher فقط بعد از موفقیت این تابع شروع می‌شود.
/// فقط توکن نامعتبر خطا می‌دهد، چون با صبر کردن درست نمی‌شود.
async fn wait_for_telegram(bot: &Bot) -> anyhow::Result<()> {
    let mut delay = Duration::from_secs(3);
    loop {
        match bot.get_me().await {
            Ok(me) => {
                log::info!("اتصال به تلگرام برقرار شد: @{}", me.username());
                return Ok(());
            }
            Err(RequestError::Api(ApiError::InvalidToken)) => {
                anyhow::bail!("توکن ربات نامعتبر است؛ TELOXIDE_TOKEN را از @BotFather دوباره بگیر");
            }
            Err(e) => {
                log::warn!(
                    "اتصال به تلگرام برقرار نشد ({e}). {} ثانیه‌ی دیگر دوباره تلاش می‌کنم؛ TELEGRAM_PROXY یا VPN را چک کن.",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(60));
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // اول .env، بعد logger؛ تا RUST_LOG داخل .env هم اثر کند
    dotenvy::dotenv().ok();
    pretty_env_logger::formatted_builder()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let token = env::var("TELOXIDE_TOKEN").context("TELOXIDE_TOKEN تنظیم نشده")?;
    let threshold: f64 = env_or("FLUCT_THRESHOLD", 1.5)?;
    let window_min: u64 = env_or("WINDOW_MINUTES", 5)?;
    let interval: u64 = env_or("CHECK_INTERVAL_SEC", 60)?;
    let cooldown: u64 = env_or("ALERT_COOLDOWN_SEC", 900)?;
    let confirm: u32 = env_or("CONFIRM_TICKS", 2)?;
    let max_jump: f64 = env_or("MAX_JUMP_PCT", 15.0)?;
    let db_path: String = env_or("DB_PATH", "users.db".to_string())?;

    ensure!(threshold > 0.0, "FLUCT_THRESHOLD باید مثبت باشد");
    ensure!(interval >= 5, "CHECK_INTERVAL_SEC حداقل ۵ ثانیه");
    ensure!(window_min >= 1, "WINDOW_MINUTES حداقل ۱");
    ensure!(confirm >= 1, "CONFIRM_TICKS حداقل ۱");

    // تلگرام و سایت‌های ایرانی مسیر شبکه‌ی جدا دارند:
    // TELEGRAM_PROXY فقط برای تلگرام، SITES_PROXY فقط برای سایت‌های قیمت (هر دو اختیاری)
    let telegram_proxy = opt_env("TELEGRAM_PROXY");
    let sites_proxy = opt_env("SITES_PROXY");
    log::info!(
        "تلگرام: {} | سایت‌های قیمت: {}",
        if telegram_proxy.is_some() { "با پروکسی" } else { "مستقیم" },
        if sites_proxy.is_some() { "با پروکسی" } else { "مستقیم" },
    );

    let mut bot = Bot::with_client(token, telegram_client(telegram_proxy.as_deref())?);
    // آدرس واسطه‌ی API تلگرام (مثلاً Cloudflare Worker) به‌جای api.telegram.org؛ بدون VPN
    if let Some(u) = opt_env("TELEGRAM_API_URL") {
        let url = reqwest011::Url::parse(&u).with_context(|| "TELEGRAM_API_URL نامعتبر است")?;
        log::info!("API تلگرام از طریق واسطه: {}", url.host_str().unwrap_or("?"));
        bot = bot.set_api_url(url);
    }
    let db = Arc::new(db::Db::new(&db_path)?);
    let prices: SharedPrices = Arc::new(RwLock::new(Default::default()));

    let params = Params {
        threshold,
        window: Duration::from_secs(window_min * 60),
        cooldown: Duration::from_secs(cooldown),
        confirm,
        max_jump,
    };
    let monitor = Monitor::new(params, Arc::clone(&prices), sites_proxy.as_deref())?;
    let monitor_task = tokio::spawn(monitor.run(bot.clone(), Arc::clone(&db), interval));

    log::info!("ربات راه افتاد؛ برای توقف Ctrl-C");

    let bot_task = {
        let (bot, db, prices) = (bot.clone(), Arc::clone(&db), Arc::clone(&prices));
        async move {
            wait_for_telegram(&bot).await?;
            bot::run(bot, db, prices).await;
            anyhow::Ok(())
        }
    };

    // هرکدام زودتر تمام شد، برنامه تمام می‌شود؛ Ctrl-C در همه‌ی حالت‌ها (حتی وسط انتظار برای تلگرام) کار می‌کند
    tokio::select! {
        r = bot_task => { r?; log::info!("ربات متوقف شد"); }
        r = monitor_task => anyhow::bail!("Monitor به‌طور غیرمنتظره تمام شد: {r:?}"),
        _ = tokio::signal::ctrl_c() => log::info!("Ctrl-C دریافت شد؛ خاموش می‌شوم"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_client_proxy_settings() {
        assert!(telegram_client(None).is_ok());
        assert!(telegram_client(Some("socks5h://127.0.0.1:10808")).is_ok());
        assert!(telegram_client(Some("http://127.0.0.1:7890")).is_ok());
        assert!(telegram_client(Some("not a url")).is_err());
    }
}
