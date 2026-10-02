#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionState {
    Pending,
    Approved,
    Revoked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    None,
    Approved,
    Denied,
    Revoked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub admission_state: AdmissionState,
    pub review_decision: ReviewDecision,
    pub revision: i64,
}
