use std::time::Duration;

use anyhow::{anyhow, Context};
use scraper::{Html, Selector};

/// روش پیدا کردن قیمت داخل صفحه
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Css فعلاً استفاده نمی‌شود ولی برای سایت‌های بعدی نگه داشته شده
pub enum Extract {
    /// سلکتور CSS عنصر قیمت. سریع و ساده، ولی اگر سایت کلاس‌ها را عوض کند
    /// (مثلاً کلاس‌های هش‌شده‌ی MUI مثل `mui-17xixml`) می‌شکند.
    Css(&'static str),
    /// اولین قیمتِ معتبر (≥ ۱۰۰۰) که بعد از متنی شامل این برچسب بیاید
    /// (مثلاً "قیمت هر گرم" یا "طلای ۱۸ عیار"). مستقل از کلاس‌ها و ساختار تگ‌هاست
    /// و به ارقام فارسی/انگلیسی و نوع ی/ک حساس نیست.
    After(&'static str),
}

/// یک منبع قیمت (صفحه‌ی یک سایت)
#[derive(Clone, Debug)]
pub struct Source {
    pub name: &'static str, // نام نمایشی (باید یکتا باشد)
    pub url: &'static str,
    pub extract: Extract,
    /// قیمت خوانده‌شده بر این عدد تقسیم می‌شود: ۱ برای تومان، ۱۰ برای ریال
    pub divisor: f64,
}

/// منابع پایش‌شونده — قبل از استفاده robots.txt و شرایط هر سایت را ببین.
pub fn sources() -> Vec<Source> {
    vec![
        Source {
            name: "اکسیراز",
            url: "https://exiraz.com/18k-gold-price",
            extract: Extract::After("قیمت هر گرم"),
            divisor: 1.0,
        },
        // TODO: برچسب ردیف ۱۸ عیار والکس را از صفحه بردار (به راهنما مراجعه کن)
        // Source {
        //     name: "والکس",
        //     url: "https://wallex.ir/gold",
        //     extract: Extract::After("<برچسب ردیف>"),
        //     divisor: 1.0,
        // },
    ]
}

/// کلاینت سایت‌های قیمت. بدون `proxy` مستقیم وصل می‌شود (متغیرهای HTTP_PROXY/ALL_PROXY
/// سیستم عمداً نادیده گرفته می‌شوند تا VPN سراسری سایت‌های ایرانی را خراب نکند).
/// timeout دارد تا یک سایت کند بقیه را معطل نکند.
pub fn build_client(proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let b = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (compatible; gold-bot/0.2)");
    let b = match proxy {
        Some(p) => b.proxy(reqwest::Proxy::all(p).with_context(|| "SITES_PROXY نامعتبر است")?),
        None => b.no_proxy(),
    };
    Ok(b.build()?)
}

/// دریافت قیمت یک منبع (به تومان)؛ خطا دلیل شکست را می‌گوید (شبکه، HTTP، پیدا نشدن، پارس)
pub async fn fetch_price(client: &reqwest::Client, src: &Source) -> anyhow::Result<f64> {
    let html = client
        .get(src.url)
        .send()
        .await
        .context("درخواست ناموفق")?
        .error_for_status()
        .context("پاسخ HTTP خطا بود")?
        .text()
        .await
        .context("خواندن بدنه‌ی پاسخ ناموفق")?;
    Ok(extract_price(&html, &src.extract)? / src.divisor)
}

/// بخش هم‌زمان (sync) و قابل تست؛ `Html` نباید across await نگه داشته شود (Send نیست)
pub fn extract_price(html: &str, how: &Extract) -> anyhow::Result<f64> {
    let doc = Html::parse_document(html);
    match how {
        Extract::Css(selector) => extract_css(&doc, selector),
        Extract::After(label) => extract_after(&doc, label),
    }
}

fn extract_css(doc: &Html, selector: &str) -> anyhow::Result<f64> {
    let sel = Selector::parse(selector).map_err(|e| anyhow!("سلکتور نامعتبر {selector:?}: {e}"))?;
    let el = doc
        .select(&sel)
        .next()
        .ok_or_else(|| anyhow!("سلکتور {selector:?} چیزی پیدا نکرد"))?;
    let text: String = el.text().collect();
    parse_price(&text).ok_or_else(|| anyhow!("عددی در متن {text:?} پیدا نشد"))
}

/// قیمت‌های کوچک‌تر از این (درصد تغییر، شماره‌ی ردیف، …) قیمت حساب نمی‌شوند
const MIN_PRICE: f64 = 1_000.0;
/// بعد از برچسب، چند گره‌ی متنی بعدی بررسی شود
const LOOKAHEAD: usize = 6;

fn extract_after(doc: &Html, label: &str) -> anyhow::Result<f64> {
    let want = norm_text(label);

    // گره‌های متنیِ صفحه به ترتیب سند؛ محتوای script/style/noscript نادیده گرفته می‌شود
    let nodes: Vec<String> = doc
        .tree
        .nodes()
        .filter_map(|n| {
            let text = n.value().as_text()?;
            let visible = n
                .parent()
                .and_then(|p| p.value().as_element())
                .map_or(true, |e| !matches!(e.name(), "script" | "style" | "noscript"));
            visible.then(|| norm_text(text))
        })
        .filter(|t| !t.is_empty())
        .collect();

    for (i, node) in nodes.iter().enumerate() {
        let Some(pos) = node.find(&want) else { continue };
        // اول باقیِ همان گره (مثلاً «طلای ۱۸ عیار: ۲۴,۰۰۰,۰۰۰»)، بعد گره‌های بعدی
        let rest = &node[pos + want.len()..];
        let following = nodes[i + 1..].iter().take(LOOKAHEAD).map(String::as_str);
        for cand in std::iter::once(rest).chain(following) {
            if let Some(v) = parse_price(cand).filter(|v| *v >= MIN_PRICE) {
                return Ok(v);
            }
        }
    }
    Err(anyhow!("برچسب {label:?} یا قیمتی بعد از آن پیدا نشد"))
}

/// نرمال‌سازی برای مقایسه: ارقام فارسی/عربی → ASCII، ي/ك عربی → ی/ک،
/// حذف نیم‌فاصله و علائم جهت، و یکی کردن فاصله‌ها
fn norm_text(s: &str) -> String {
    let mapped: String = s
        .chars()
        .filter_map(|c| match c {
            '۰'..='۹' => char::from_digit(c as u32 - '۰' as u32, 10),
            '٠'..='٩' => char::from_digit(c as u32 - '٠' as u32, 10),
            'ي' => Some('ی'),
            'ك' => Some('ک'),
            '\u{200c}' | '\u{200e}' | '\u{200f}' => None,
            c => Some(c),
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// بزرگ‌ترین عدد داخل متن را برمی‌گرداند (تا برچسب‌هایی مثل «۱۸ عیار» با قیمت قاطی نشوند).
/// ارقام فارسی/عربی، جداکننده‌ی هزارگان («,» «٬» «.») و ممیز فارسی («٫») پشتیبانی می‌شوند.
pub fn parse_price(text: &str) -> Option<f64> {
    let mut best: Option<f64> = None;
    let mut cur = String::new();

    for c in text.chars().chain(std::iter::once(' ')) {
        match normalize(c) {
            Some(d) if d.is_ascii_digit() => cur.push(d),
            Some(sep @ (',' | '.')) if !cur.is_empty() => cur.push(sep),
            _ => flush(&mut cur, &mut best),
        }
    }
    best
}

fn normalize(c: char) -> Option<char> {
    match c {
        '۰'..='۹' => char::from_digit(c as u32 - '۰' as u32, 10),
        '٠'..='٩' => char::from_digit(c as u32 - '٠' as u32, 10),
        '٫' => Some('.'),
        '٬' | '،' => Some(','),
        other => Some(other),
    }
}

fn flush(cur: &mut String, best: &mut Option<f64>) {
    if let Some(v) = parse_token(cur) {
        if best.map_or(true, |b| v > b) {
            *best = Some(v);
        }
    }
    cur.clear();
}

/// "3,250,000" / "3.250.000" / "3.250" -> عدد صحیح؛ "1.5" -> ممیزدار.
/// قیمت‌ها (تومان) صحیح‌اند، پس «.» با دقیقاً ۳ رقم بعدش یا تکرار «.» جداکننده‌ی هزارگان است.
fn parse_token(tok: &str) -> Option<f64> {
    let tok = tok.trim_end_matches([',', '.']);
    if tok.is_empty() {
        return None;
    }
    let no_commas = tok.replace(',', "");
    let dots = no_commas.matches('.').count();
    let last_group = no_commas.rsplit('.').next().unwrap_or("");
    let cleaned = if dots > 1 || (dots == 1 && last_group.len() == 3) {
        no_commas.replace('.', "")
    } else {
        no_commas
    };
    cleaned.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_formats() {
        assert_eq!(parse_price("۳,۲۵۰,۰۰۰ تومان"), Some(3_250_000.0));
        assert_eq!(parse_price("3,250,000"), Some(3_250_000.0));
        assert_eq!(parse_price("3.250.000"), Some(3_250_000.0));
        assert_eq!(parse_price("۳٬۲۵۰٬۰۰۰"), Some(3_250_000.0));
        assert_eq!(parse_price("٣٬٢٥٠٬٠٠٠"), Some(3_250_000.0));
        assert_eq!(parse_price("1.5"), Some(1.5));
        assert_eq!(parse_price("۱٫۵"), Some(1.5));
        assert_eq!(parse_price("۲۴٬۳۲۰٬۵۹۵٫۶ تومان"), Some(24_320_595.6));
    }

    #[test]
    fn label_numbers_do_not_pollute_price() {
        assert_eq!(parse_price("۱۸ عیار: ۳,۲۵۰,۰۰۰ تومان"), Some(3_250_000.0));
        assert_eq!(parse_price("بروزرسانی ۱۴۰۵/۰۶/۲۹ قیمت ۳,۲۵۰,۰۰۰"), Some(3_250_000.0));
    }

    #[test]
    fn no_number() {
        assert_eq!(parse_price("ناموجود"), None);
        assert_eq!(parse_price(""), None);
        assert_eq!(parse_price(", ."), None);
    }

    #[test]
    fn client_proxy_settings() {
        assert!(build_client(None).is_ok());
        assert!(build_client(Some("socks5h://127.0.0.1:10808")).is_ok());
        assert!(build_client(Some("http://127.0.0.1:7890")).is_ok());
        assert!(build_client(Some("not a url")).is_err());
    }

    #[test]
    fn css_extraction() {
        let html = r#"<html><body><span id="gold18"> ۳,۲۵۰,۰۰۰ </span></body></html>"#;
        assert_eq!(extract_price(html, &Extract::Css("#gold18")).unwrap(), 3_250_000.0);
        assert!(extract_price(html, &Extract::Css(".nope")).is_err());
        assert!(extract_price(html, &Extract::Css("###")).is_err());
    }

    // تکه‌هایی از HTML واقعی exiraz.com (کپی‌شده از curl)
    const EXIRAZ: &str = r#"<html><body>
      <div class="flex items-end gap-2"><span class="text-4xl font-bold">۲۴٬۳۲۰٬۵۹۵</span><span class="pb-2 text-2xl text-gray-500">تومان</span></div>
      <p class="text-sm text-default-500 mt-4 text-right">قیمت هر گرم:<span class="font-semibold mr-2">۲۴٬۳۲۰٬۵۹۵٫۶<!-- --> <!-- --> تومان</span></p>
      <div class="font-semibold text-slate-700 dark:text-white">طلای ۱۸ عیار</div><div class="text-center text-lg font-extrabold">۲۴٬۳۲۰٬۵۹۵٫۶ تومان</div>
      <div class="text-right"><h3 class=" font-bold">طلای ۱۸ عیار</h3><div class="mt-3 text-lg font-bold">۲۴٬۳۲۰٬۵۹۵٫۶ تومان</div></div>
    </body></html>"#;

    #[test]
    fn after_label_on_real_exiraz_html() {
        let per_gram = extract_price(EXIRAZ, &Extract::After("قیمت هر گرم")).unwrap();
        assert_eq!(per_gram, 24_320_595.6);
        let by_name = extract_price(EXIRAZ, &Extract::After("طلای ۱۸ عیار")).unwrap();
        assert_eq!(by_name, 24_320_595.6);
        // برچسب با ارقام انگلیسی هم پیدا می‌شود
        assert_eq!(extract_price(EXIRAZ, &Extract::After("طلای 18 عیار")).unwrap(), 24_320_595.6);
    }

    #[test]
    fn after_label_table_row_and_skips_percentages() {
        let html = r#"<table><tr><td>طلای ۱۸ عیار</td><td>۱٫۵٪</td><td><span>24,278,460</span><span>(گرم)</span></td></tr></table>"#;
        assert_eq!(extract_price(html, &Extract::After("طلای ۱۸ عیار")).unwrap(), 24_278_460.0);
    }

    #[test]
    fn after_label_same_node_and_arabic_letters() {
        let html = "<div>طلاي ۱۸ عيار: ۲۴,۰۰۰,۰۰۰ تومان</div>";
        assert_eq!(extract_price(html, &Extract::After("طلای ۱۸ عیار")).unwrap(), 24_000_000.0);
    }

    #[test]
    fn after_label_ignores_scripts() {
        let html = r#"<script>{"label":"طلای ۱۸ عیار","p":9999999}</script><p>طلای ۱۸ عیار</p><p>24,000,000</p>"#;
        assert_eq!(extract_price(html, &Extract::After("طلای ۱۸ عیار")).unwrap(), 24_000_000.0);
    }

    #[test]
    fn after_label_missing_is_error() {
        assert!(extract_price("<p>سلام</p>", &Extract::After("طلای ۱۸ عیار")).is_err());
        assert!(extract_price("<p>طلای ۱۸ عیار</p><p>ناموجود</p>", &Extract::After("طلای ۱۸ عیار")).is_err());
    }
}
