pub(super) struct TurnUpdate<'a> {
    pub(super) turn_id: &'a str,
    pub(super) status: &'a str,
    pub(super) started: i64,
    pub(super) completed: Option<i64>,
    pub(super) durable_started: bool,
}
