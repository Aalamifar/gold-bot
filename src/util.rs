use chrono::{DateTime, Utc};

/// 3250000.0 -> "3,250,000"
pub fn fmt_price(p: f64) -> String {
    let digits = format!("{:.0}", p);
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 && c != '-' {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// زمان به وقت تهران، مستقل از timezone سرور
pub fn tehran(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono_tz::Asia::Tehran)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_thousands() {
        assert_eq!(fmt_price(3_250_000.0), "3,250,000");
        assert_eq!(fmt_price(999.0), "999");
        assert_eq!(fmt_price(1000.0), "1,000");
    }
}
