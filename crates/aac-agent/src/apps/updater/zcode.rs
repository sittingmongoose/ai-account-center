//! W3 owns this file. Replace each `Unported` with a port of its Python oracle
//! (see the table in `updater/mod.rs`).
use super::Unported;

pub(super) static ZCODE: Unported = Unported("zcode");
pub(super) static ADAPTERS: Unported = Unported("t3-acp-adapters");

/// Python `ubuntu_unit_busy`: while a timed-out wait for Ubuntu's adapter oneshot
/// is unsettled, npm and T3 work must report `busy`. W3 ports the unit check.
pub(super) async fn adapter_unit_busy(cx: &super::Context) -> bool {
    cx.shared
        .adapter_unit_unsettled
        .load(std::sync::atomic::Ordering::SeqCst)
}
