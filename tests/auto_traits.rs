//! Published auto-trait contracts for public cache owners.

use std::panic::{RefUnwindSafe, UnwindSafe};

use girt::Objects;
use girt::fetch::KnownHistory;
use girt::retention::RetentionPlan;

#[test]
fn public_cache_owners_preserve_auto_traits() {
    fn assert_traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}

    assert_traits::<Objects>();
    assert_traits::<KnownHistory>();
}

#[test]
fn retention_diagnostics_remain_thread_safe() {
    fn assert_traits<T: Send + Sync>() {}

    // Retention plans own source errors for diagnostics, including I/O errors. Those errors do
    // not promise unwind safety; callers catching panics must choose their own recovery boundary.
    assert_traits::<RetentionPlan>();
    assert_traits::<girt::retention::RetentionError>();
}
