use super::*;

#[test]
fn release_invokes_installed_hook_per_call() {
    test_support::install_counting_hook();
    let before = test_support::calls();
    release_retained_memory("test");
    release_retained_memory("test");
    assert_eq!(
        test_support::calls(),
        before + 2,
        "each release must invoke the installed hook exactly once"
    );
}

#[test]
#[serial_test::serial(MEMORY_RELEASE_DEFER)]
fn deferred_request_coalesces_and_drains_once() {
    test_support::install_counting_hook();
    run_deferred_release();

    let before = test_support::calls();
    run_deferred_release();
    assert_eq!(
        test_support::calls(),
        before,
        "drain without request is inert"
    );

    request_release_after_draw("test");
    request_release_after_draw("test");
    assert_eq!(
        test_support::calls(),
        before,
        "requesting must not purge synchronously"
    );
    run_deferred_release();
    assert_eq!(test_support::calls(), before + 1, "one drain, one purge");
    run_deferred_release();
    assert_eq!(
        test_support::calls(),
        before + 1,
        "flag cleared by the drain"
    );
}
