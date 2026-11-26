/// Return current time as a unix timestamp string (e.g., `"1740000000s-since-epoch"`).
pub(crate) fn unix_timestamp_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", now.as_secs())
}
