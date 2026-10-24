/// What we need to pick a dropped session back up without a fresh READY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeState {
    pub session_id: String,
    /// Discord asks clients to resume on this URL instead of the normal one.
    pub resume_url: String,
}
