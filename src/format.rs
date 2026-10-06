//! Small display helpers shared by the GUI and the CLI.

use chrono::{Local, TimeZone};

/// `1.5 MB`, `300 B`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ if value >= 100.0 => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

/// `14:02` for today, `Oct 3, 14:02` otherwise (local time).
pub fn time(millis: i64) -> String {
    let Some(t) = Local.timestamp_millis_opt(millis).single() else {
        return String::new();
    };
    if t.date_naive() == Local::now().date_naive() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%b %-d, %H:%M").to_string()
    }
}

/// A single token that looks like a web link.
pub fn is_link(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_whitespace)
}

/// `AB` for "Anna's Box", `P` for "parents".
pub fn initials(name: &str) -> String {
    let mut words = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty());
    let first = words.next().and_then(|w| w.chars().next());
    let second = words.next().and_then(|w| w.chars().next());
    first
        .into_iter()
        .chain(second)
        .flat_map(char::to_uppercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_500), "1.5 KB");
        assert_eq!(size(250_000_000), "250 MB");
        assert!(is_link("https://example.com/a?b=c"));
        assert!(!is_link("see https://example.com"));
        assert_eq!(initials("Anna's Box"), "AS");
        assert_eq!(initials("parents"), "P");
        assert_eq!(initials(""), "");
    }
}
