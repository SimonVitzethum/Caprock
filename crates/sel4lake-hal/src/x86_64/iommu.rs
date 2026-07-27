//! **Arch-neutrale IOMMU-Fassade** (x86_64 → VT-d). Gegenstück zu `aarch64::iommu`.

use super::vtd;
pub use crate::fault::{FaultKind, FaultRecord};

pub fn faults_empty() -> bool {
    vtd::faults_empty()
}
pub fn peek_fault() -> Option<FaultRecord> {
    vtd::peek_fault()
}
pub fn drain_faults() -> u32 {
    vtd::drain_faults()
}
pub fn config_errors() -> u32 {
    vtd::config_errors()
}
