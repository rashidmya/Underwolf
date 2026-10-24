use uw_input::hooks::*;
use uw_input::requests::*;
use uw_pipe_proto::input::*;
#[derive(Default)]
struct Backend {
    created: Vec<usize>,
    disabled: Vec<usize>,
    published: Vec<usize>,
    fail_create: Option<usize>,
    fail_apply: bool,
}
impl HookBackend for Backend {
    fn create(&mut self, target: usize, _detour: usize) -> Result<usize, String> {
        if self.fail_create == Some(target) {
            return Err("create failed".into());
        }
        self.created.push(target);
        Ok(target + 100)
    }
    fn publish(&mut self, target: usize, trampoline: usize) {
        assert_eq!(trampoline, target + 100);
        self.published.push(target);
    }
    fn queue_enable(&mut self, target: usize) -> Result<(), String> {
        assert!(self.published.contains(&target));
        Ok(())
    }
    fn apply(&mut self) -> Result<(), String> {
        if self.fail_apply { Err("enable failed".into()) } else { Ok(()) }
    }
    fn disable(&mut self, target: usize) -> Result<(), String> {
        self.disabled.push(target);
        Ok(())
    }
}
#[test]
fn failed_enable_preserves_callable_trampolines() {
    let mut b = Backend { fail_apply: true, ..Default::default() };
    let e = HookSet::install(&mut b, &[(1, 11), (2, 22)]).unwrap_err();
    assert_eq!(e.retained.len(), 2);
    assert_eq!(b.disabled, vec![2, 1]);
    assert_eq!(b.published, vec![1, 2]);
}
#[test]
fn partial_setup_rolls_back_owned_hooks() {
    let mut b = Backend { fail_create: Some(2), ..Default::default() };
    let e = HookSet::install(&mut b, &[(1, 11), (2, 22)]).unwrap_err();
    assert_eq!(e.retained.len(), 1);
    assert_eq!(b.disabled, vec![1]);
    assert!(!b.disabled.contains(&99));
}
#[test]
fn ui_request_is_not_applied_until_dispatched() {
    let q = RequestQueue::new();
    let r = CaptureRequest { epoch: 1, state_seq: 9, request_id: 1, on: true };
    assert!(q.try_push(r).is_ok());
    let mut p = uw_input::policy::Policy::new();
    for event in [
        uw_input::policy::PolicyEvent::Connected,
        uw_input::policy::PolicyEvent::Foreground(true),
        uw_input::policy::PolicyEvent::Visible(true),
        uw_input::policy::PolicyEvent::Healthy(true),
        uw_input::policy::PolicyEvent::Installed(true),
    ] {
        p.event(event);
    }
    let stale = q.dispatch(&mut p).unwrap();
    assert_eq!(stale.result.unwrap().code, CaptureCode::StaleEpoch);
    q.try_push(CaptureRequest { epoch: p.epoch(), ..r }).unwrap();
    assert!(!p.active());
    let effects = q.dispatch(&mut p).unwrap();
    assert!(effects.result.is_none());
    assert!(!p.active());
    let applied = p.event(uw_input::policy::PolicyEvent::SetupFinished { epoch: p.epoch(), success: true });
    assert!(p.active());
    assert_eq!(applied.result.unwrap().code, CaptureCode::Applied);
    assert_eq!(q.pop(), None);
    for i in 0..32 {
        assert!(q.try_push(CaptureRequest { request_id: i, ..r }).is_ok())
    }
    assert!(q.try_push(r).is_err());
}
