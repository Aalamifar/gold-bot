use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use teloxide::{prelude::*, ApiError, RequestError};
use tokio::sync::RwLock;
use tokio::time::MissedTickBehavior;

use crate::db::Db;
use crate::sources::{build_client, fetch_price, sources, Source};
use crate::util::{fmt_price, tehran};

// ───────────── انواع مشترک ─────────────

/// آخرین قیمتِ معتبر هر منبع؛ بین Monitor و دستور /price مشترک است
#[derive(Clone, Copy)]
pub struct PriceInfo {
    pub price: f64,
    pub at: DateTime<Utc>,
}
pub type SharedPrices = Arc<RwLock<HashMap<&'static str, PriceInfo>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up = 0,   // افزایش  → فرصت فروش
    Down = 1, // کاهش   → فرصت خرید
}

#[derive(Clone, Copy, Debug)]
pub struct Move {
    pub dir: Direction,
    pub base: f64,  // کمینه (برای Up) یا بیشینه (برای Down)‌ی قیمت در پنجره
    pub price: f64, // قیمت فعلی
    pub pct: f64,   // درصد تغییر نسبت به base
}

struct Alert {
    source: &'static str,
    mv: Move,
}

#[derive(Clone, Copy)]
pub struct Params {
    pub threshold: f64,   // درصد
    pub window: Duration, // پنجره‌ی لغزان
    pub cooldown: Duration,
    pub confirm: u32,   // تعداد چکِ پشت‌سرهم برای تأیید
    pub max_jump: f64,  // درصدِ تغییر تک‌مرحله‌ای که «مشکوک» حساب می‌شود
}

// ───────────── منطق تشخیص (خالص و قابل تست) ─────────────

fn pct_change(from: f64, to: f64) -> f64 {
    (to - from) / from * 100.0
}

/// قیمت فعلی را با کمینه/بیشینه‌ی پنجره مقایسه می‌کند
fn detect(history: &VecDeque<(Instant, f64)>, price: f64, threshold: f64) -> Option<Move> {
    let min = history.iter().map(|&(_, p)| p).fold(f64::INFINITY, f64::min);
    let max = history.iter().map(|&(_, p)| p).fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() {
        return None; // تاریخچه خالی
    }
    let up = pct_change(min, price);
    let down = pct_change(max, price); // منفی یا صفر
    if up >= threshold && up >= -down {
        Some(Move { dir: Direction::Up, base: min, price, pct: up })
    } else if -down >= threshold {
        Some(Move { dir: Direction::Down, base: max, price, pct: down })
    } else {
        None
    }
}

#[derive(Default)]
struct SourceState {
    history: VecDeque<(Instant, f64)>,
    last_price: Option<f64>,   // آخرین قیمتِ پذیرفته‌شده
    quarantined: Option<f64>,  // قیمتِ مشکوک منتظر تأیید
    streak: Option<(Direction, u32)>,
    last_alert: [Option<Instant>; 2], // به تفکیک جهت
}

struct Outcome {
    accepted: bool,
    alert: Option<Move>,
}

impl SourceState {
    fn observe(&mut self, price: f64, now: Instant, p: &Params) -> Outcome {
        let reject = Outcome { accepted: false, alert: None };
        if !price.is_finite() || price <= 0.0 {
            return reject;
        }

        // ۱) sanity: پرش ناگهانی احتمالاً خطای پارس/سایت است، مگر اینکه در چک بعدی تأیید شود
        if let Some(last) = self.last_price {
            if pct_change(last, price).abs() > p.max_jump {
                match self.quarantined {
                    Some(q) if pct_change(q, price).abs() <= 2.0 => self.quarantined = None, // تأیید شد
                    _ => {
                        self.quarantined = Some(price);
                        return reject;
                    }
                }
            } else {
                self.quarantined = None;
            }
        }
        self.last_price = Some(price);

        // ۲) پنجره‌ی لغزان: قیمت‌های قدیمی‌تر از window حذف می‌شوند
        while let Some(&(t, _)) = self.history.front() {
            if now.duration_since(t) > p.window {
                self.history.pop_front();
            } else {
                break;
            }
        }
        let detected = detect(&self.history, price, p.threshold);
        self.history.push_back((now, price));

        // ۳) تأیید: نوسان هم‌جهت باید در `confirm` چک پشت‌سرهم دیده شود
        let Some(mv) = detected else {
            self.streak = None;
            return Outcome { accepted: true, alert: None };
        };
        let n = match self.streak {
            Some((d, n)) if d == mv.dir => n + 1,
            _ => 1,
        };
        self.streak = Some((mv.dir, n));
        if n < p.confirm {
            return Outcome { accepted: true, alert: None };
        }

        // ۴) cooldown به تفکیک جهت
        let idx = mv.dir as usize;
        if let Some(t) = self.last_alert[idx] {
            if now.duration_since(t) < p.cooldown {
                return Outcome { accepted: true, alert: None };
            }
        }
        self.last_alert[idx] = Some(now);

        // پنجره را از قیمت فعلی شروع می‌کنیم تا همین نوسان دوباره هشدار ندهد
        self.history.clear();
        self.history.push_back((now, price));
        self.streak = None;
        Outcome { accepted: true, alert: Some(mv) }
    }
}

// ───────────── Monitor ─────────────

pub struct Monitor {
    client: reqwest::Client,
    sources: Vec<Source>,
    states: HashMap<&'static str, SourceState>,
    params: Params,
    prices: SharedPrices,
}

impl Monitor {
    pub fn new(params: Params, prices: SharedPrices, sites_proxy: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self {
            client: build_client(sites_proxy)?,
            sources: sources(),
            states: HashMap::new(),
            params,
            prices,
        })
    }

    pub async fn run(mut self, bot: Bot, db: Arc<Db>, interval_sec: u64) {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_sec));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let alerts = self.tick().await;
            if !alerts.is_empty() {
                broadcast(&bot, &db, &build_alert(&alerts)).await;
            }
        }
    }

    async fn tick(&mut self) -> Vec<Alert> {
        // همه‌ی منابع هم‌زمان؛ خطای یکی بقیه را خراب نمی‌کند
        let client = &self.client;
        let results = futures::future::join_all(
            self.sources
                .iter()
                .map(|s| async move { (s.name, fetch_price(client, s).await) }),
        )
        .await;

        let now = Instant::now();
        let mut alerts = Vec::new();
        for (name, res) in results {
            let price = match res {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("{name}: دریافت قیمت ناموفق: {e:#}");
                    continue;
                }
            };
            log::info!("{name}: {} تومان", fmt_price(price));

            let st = self.states.entry(name).or_default();
            let out = st.observe(price, now, &self.params);
            if !out.accepted {
                log::warn!("{name}: قیمت مشکوک ({}) نادیده گرفته شد", fmt_price(price));
                continue;
            }
            self.prices
                .write()
                .await
                .insert(name, PriceInfo { price, at: Utc::now() });
            if let Some(mv) = out.alert {
                alerts.push(Alert { source: name, mv });
            }
        }
        alerts
    }
}

// ───────────── پیام و ارسال ─────────────

fn build_alert(alerts: &[Alert]) -> String {
    let blocks: Vec<String> = alerts
        .iter()
        .map(|a| {
            let (emoji, headline, base_label) = match a.mv.dir {
                Direction::Up => ("📈", "افزایش شدید — فرصت فروش!", "کمترین قیمت اخیر"),
                Direction::Down => ("📉", "کاهش شدید — فرصت خرید!", "بیشترین قیمت اخیر"),
            };
            format!(
                "{emoji} {}\n{headline}\n{base_label}: {} تومان\nقیمت فعلی: {} تومان\nتغییر: {:+.2}٪",
                a.source,
                fmt_price(a.mv.base),
                fmt_price(a.mv.price),
                a.mv.pct
            )
        })
        .collect();
    format!("{}\n\n⏱ {}", blocks.join("\n\n"), tehran(Utc::now()))
}

async fn broadcast(bot: &Bot, db: &Db, text: &str) {
    let users = match db.all_users() {
        Ok(u) => u,
        Err(e) => {
            log::error!("خواندن کاربران ناموفق: {e:#}");
            return;
        }
    };
    for id in users {
        let mut retries = 0;
        loop {
            match bot.send_message(ChatId(id), text).await {
                Ok(_) => break,
                // تلگرام گفته صبر کن
                Err(RequestError::RetryAfter(wait)) if retries < 2 => {
                    retries += 1;
                    tokio::time::sleep(wait.duration()).await;
                }
                // کاربر ربات را بلاک کرده / چت حذف شده → از لیست حذفش کن
                Err(RequestError::Api(
                    ApiError::BotBlocked | ApiError::ChatNotFound | ApiError::UserDeactivated,
                )) => {
                    log::info!("حذف کاربر {id} (ربات را بلاک کرده یا حذف شده)");
                    if let Err(e) = db.remove_user(id) {
                        log::warn!("حذف کاربر ناموفق: {e:#}");
                    }
                    break;
                }
                Err(e) => {
                    log::warn!("ارسال به {id} ناموفق: {e}");
                    break;
                }
            }
        }
        // سقف تلگرام ~۳۰ پیام در ثانیه است
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(confirm: u32) -> Params {
        Params {
            threshold: 1.5,
            window: Duration::from_secs(300),
            cooldown: Duration::from_secs(900),
            confirm,
            max_jump: 15.0,
        }
    }

    fn secs(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn rise_and_fall_directions() {
        let t0 = Instant::now();
        let p = params(1);

        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        let up = st.observe(102.0, secs(t0, 60), &p).alert.unwrap();
        assert_eq!(up.dir, Direction::Up);
        assert!((up.pct - 2.0).abs() < 1e-9);

        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        let down = st.observe(98.0, secs(t0, 60), &p).alert.unwrap();
        assert_eq!(down.dir, Direction::Down);
    }

    #[test]
    fn below_threshold_is_quiet() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(st.observe(101.0, secs(t0, 60), &p).alert.is_none());
    }

    #[test]
    fn needs_confirmation() {
        let t0 = Instant::now();
        let p = params(2);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(st.observe(102.0, secs(t0, 60), &p).alert.is_none()); // چک اول
        assert!(st.observe(102.5, secs(t0, 120), &p).alert.is_some()); // چک دوم
    }

    #[test]
    fn reverted_move_resets_streak() {
        let t0 = Instant::now();
        let p = params(2);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(st.observe(102.0, secs(t0, 60), &p).alert.is_none());
        assert!(st.observe(100.5, secs(t0, 120), &p).alert.is_none()); // برگشت
        assert!(st.observe(102.0, secs(t0, 180), &p).alert.is_none()); // دوباره از اول
    }

    #[test]
    fn window_slides() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        // ۶ دقیقه بعد؛ قیمت قدیمی از پنجره خارج شده و مبنا ۱۰۱ است
        assert!(st.observe(101.0, secs(t0, 360), &p).alert.is_none());
        assert!(st.observe(102.0, secs(t0, 420), &p).alert.is_none()); // فقط ~۱٪ نسبت به ۱۰۱
    }

    #[test]
    fn move_across_old_window_boundary_is_caught() {
        // پنجره‌ی ثابت اینجا از دست می‌داد؛ پنجره‌ی لغزان نه
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(st.observe(100.8, secs(t0, 240), &p).alert.is_none());
        assert!(st.observe(101.6, secs(t0, 300), &p).alert.is_some()); // ۱.۶٪ از ۱۰۰
    }

    #[test]
    fn cooldown_is_per_direction() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(st.observe(102.0, secs(t0, 60), &p).alert.is_some()); // Up
        // هنوز در cooldown‌ی Up
        assert!(st.observe(104.5, secs(t0, 120), &p).alert.is_none());
        // Down جهت دیگر است و cooldown جدا دارد
        let d = st.observe(100.0, secs(t0, 180), &p).alert;
        assert_eq!(d.map(|m| m.dir), Some(Direction::Down));
    }

    #[test]
    fn glitch_is_quarantined() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        let g = st.observe(1.0, secs(t0, 60), &p); // پارس خراب
        assert!(!g.accepted && g.alert.is_none());
        let back = st.observe(100.2, secs(t0, 120), &p);
        assert!(back.accepted && back.alert.is_none());
        // کمینه‌ی پنجره آلوده نشده
        assert!(st.history.iter().all(|&(_, p)| p > 50.0));
    }

    #[test]
    fn real_big_jump_confirmed_next_tick() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        st.observe(100.0, t0, &p);
        assert!(!st.observe(130.0, secs(t0, 60), &p).accepted); // مشکوک
        let ok = st.observe(130.5, secs(t0, 120), &p); // تأیید
        assert!(ok.accepted);
        assert_eq!(ok.alert.unwrap().dir, Direction::Up);
    }

    #[test]
    fn rejects_garbage_prices() {
        let t0 = Instant::now();
        let p = params(1);
        let mut st = SourceState::default();
        assert!(!st.observe(0.0, t0, &p).accepted);
        assert!(!st.observe(-5.0, t0, &p).accepted);
        assert!(!st.observe(f64::NAN, t0, &p).accepted);
    }
}
