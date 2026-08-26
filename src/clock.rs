/// Wall-clock access is isolated from the pure domain layer.
pub fn now_ms() -> i64 {
    crate::domain::normalize::timestamp_ms(chrono::Utc::now())
}
