//! Published auto-trait contracts for public cache owners.

use std::panic::{RefUnwindSafe, UnwindSafe};

use girt::Objects;
use girt::fetch::KnownHistory;
use girt::retention::RetentionPlan;

#[test]
fn public_cache_owners_preserve_auto_traits() {
    fn assert_traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}

    assert_traits::<Objects>();
    assert_traits::<RetentionPlan>();
    assert_traits::<KnownHistory>();
}
