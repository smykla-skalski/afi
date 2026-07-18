use super::*;

#[test]
fn late_approval_is_cancelled_after_job_cancel() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(update::approval_is_cancelled(false, Some(&cancel)));
    assert!(update::approval_is_cancelled(true, None));
    assert!(!update::approval_is_cancelled(false, None));
}

#[test]
fn repeated_updates_coalesce_into_one_frame() {
    let mut render = RenderGate::default();
    render.request();
    render.request();

    assert!(render.take());
    assert!(!render.take());
}
