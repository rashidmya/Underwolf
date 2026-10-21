mod common;

use std::cell::Cell;

use common::Synth;
use uw_spike_dlcollector::module::Module;
use uw_spike_dlcollector::roots::{
    CallerPattern, MAIN_LOOP_NOT_FOUND, resolve_caller, resolve_main_loop,
};
use uw_spike_dlcollector::timing::{
    CallerFilter, Chain, Discovery, Frame, PASS_INTERVAL_MS, TickGate, discovery_json,
};

const IMM: u64 = 0x1_4000_1234;
const OUTER: u64 = 0x1_4000_5678;
/// 1000 ticks per second: one tick is one millisecond.
const FREQ: u64 = 1000;

fn gate(outer: Option<u64>) -> TickGate {
    TickGate::new(
        CallerFilter {
            immediate: IMM,
            outer,
        },
        TickGate::interval_for(FREQ),
    )
}

fn never() -> Option<u64> {
    panic!("outer frame walked for a non-matching caller")
}

#[test]
fn pass_interval_is_20_hz() {
    assert_eq!(PASS_INTERVAL_MS, 50);
    assert_eq!(TickGate::interval_for(FREQ), 50);
    assert_eq!(TickGate::interval_for(10_000_000), 500_000);
    assert_eq!(TickGate::interval_for(1), 1);
}

#[test]
fn non_matching_caller_does_not_run_pass() {
    let mut g = gate(Some(OUTER));
    for (i, ret) in [IMM - 1, IMM + 1, 0, OUTER, u64::MAX]
        .into_iter()
        .enumerate()
    {
        assert!(!g.should_run(ret, i as u64 * 1000, never));
    }
    let mut g = gate(None);
    assert!(!g.should_run(IMM + 5, 0, never));
    // The gate state is untouched: the first matching call still runs.
    assert!(g.should_run(IMM, 0, never));
}

#[test]
fn pass_does_not_reenter() {
    let mut g = gate(None);
    assert!(g.should_run(IMM, 0, || None));
    // Still inside the pass (e.g. the pass itself reaches QPC through the same call site).
    assert!(!g.should_run(IMM, 10_000, || None));
    g.done();
    assert!(g.should_run(IMM, 10_000, || None));
}

#[test]
fn passes_are_at_least_interval_apart() {
    let mut g = gate(None);
    let mut run = |now| {
        let r = g.should_run(IMM, now, || None);
        if r {
            g.done();
        }
        r
    };
    assert_eq!([run(0), run(30), run(60)], [true, false, true]);
    assert_eq!([run(109), run(110)], [false, true]);
}

#[test]
fn outer_frame_must_match_when_set() {
    let mut g = gate(Some(OUTER));
    assert!(!g.should_run(IMM, 0, || Some(OUTER + 8)));
    assert!(!g.should_run(IMM, 0, || None));
    assert!(g.should_run(IMM, 0, || Some(OUTER)));
}

#[test]
fn outer_frame_is_not_walked_before_the_interval_elapsed() {
    let mut g = gate(Some(OUTER));
    assert!(g.should_run(IMM, 0, || Some(OUTER)));
    g.done();
    let walked = Cell::new(false);
    assert!(!g.should_run(IMM, 10, || {
        walked.set(true);
        Some(OUTER)
    }));
    assert!(!walked.get());
}

#[test]
fn discovery_counts_chains_per_thread_and_ranks_them() {
    let mut d = Discovery::new();
    for _ in 0..5 {
        d.record([1, 2, 3], 10);
    }
    for _ in 0..9 {
        d.record([4, 5, 6], 11);
    }
    d.record([1, 2, 3], 12);
    let top = d.top(2);
    assert_eq!(top, vec![([4, 5, 6], 11, 9), ([1, 2, 3], 10, 5)]);
    assert_eq!(d.top(32).len(), 3);
}

#[test]
fn discovery_is_bounded() {
    let mut d = Discovery::new();
    for i in 0..(Discovery::MAX_CHAINS as u64 + 100) {
        d.record([i, 0, 0], 1);
    }
    d.record([0, 0, 0], 1);
    assert_eq!(d.top(usize::MAX).len(), Discovery::MAX_CHAINS);
    assert_eq!(d.dropped(), 100);
    assert_eq!(d.top(1), vec![([0, 0, 0], 1, 2)]);
}

fn chain(n: usize) -> Chain {
    Chain {
        frames: (0..3)
            .map(|i| Frame {
                module: if i == 2 {
                    None
                } else {
                    Some("engine2.dll".into())
                },
                rva: 0x1000 * (n as u64 + 1) + i,
            })
            .collect(),
        per_sec: 240.5,
        window_thread: n == 0,
    }
}

#[test]
fn discovery_json_shape() {
    let json = discovery_json(&[chain(0)], 4096);
    assert_eq!(
        json,
        r#"[{"frames":[{"module":"engine2.dll","rva":"0x1000"},{"module":"engine2.dll","rva":"0x1001"},{"module":null,"rva":"0x1002"}],"per_sec":240.5,"window_thread":true}]"#
    );
}

#[test]
fn discovery_json_drops_chains_that_do_not_fit() {
    let chains: Vec<Chain> = (0..32).map(chain).collect();
    let json = discovery_json(&chains, 4096);
    assert!(json.len() <= 4096, "{}", json.len());
    assert!(json.starts_with(r#"[{"frames""#) && json.ends_with("}]"));
    let kept = json.matches("per_sec").count();
    assert!((1..32).contains(&kept), "{kept}");
    // The kept ones are the highest-ranked (first) chains.
    assert!(json.contains(r#""rva":"0x1000""#));
    assert_eq!(discovery_json(&chains, 10), "[]");
}

const IMM_PAT: &str = "ff 15 ?? ?? ?? ?? 48 8b 4c 24 ?? 48 2b c8";
const OUTER_PAT: &str = "e8 ?? ?? ?? ?? f3 0f 10 05 ?? ?? ?? ?? 0f 2f c1";

fn engine(s: &Synth) -> (uw_spike_dlcollector::mem::FakeMem, Vec<Module>) {
    let (mem, mut module) = s.finish();
    module.name = "engine2.dll".into();
    (mem, vec![module])
}

fn pat(pattern: &'static str, ret_offset: u64) -> CallerPattern {
    CallerPattern {
        module: "engine2.dll",
        pattern,
        ret_offset,
    }
}

#[test]
fn installed_main_loop_patterns_are_not_yet_known() {
    // Step 5 (owner session) fills MAIN_LOOP_IMMEDIATE/OUTER; until then the plugin must fail.
    let (mem, modules) = engine(&Synth::new());
    assert_eq!(
        resolve_main_loop(&mem, &modules),
        Err(MAIN_LOOP_NOT_FOUND.to_string())
    );
    assert_eq!(MAIN_LOOP_NOT_FOUND, "main-loop caller not found");
}

#[test]
fn main_loop_caller_resolves_to_return_addresses() {
    let mut s = Synth::new();
    s.place(0x100, IMM_PAT, &[]);
    s.place(0x400, OUTER_PAT, &[]);
    let (mem, modules) = engine(&s);
    let f = resolve_caller(
        &mem,
        &modules,
        Some(&pat(IMM_PAT, 6)),
        Some(&pat(OUTER_PAT, 5)),
    )
    .unwrap();
    assert_eq!(
        f,
        CallerFilter {
            immediate: s.code_va(0x106),
            outer: Some(s.code_va(0x405)),
        }
    );
    let f = resolve_caller(&mem, &modules, Some(&pat(IMM_PAT, 6)), None).unwrap();
    assert_eq!(f.outer, None);
}

#[test]
fn main_loop_caller_missing_or_ambiguous_is_an_error() {
    let mut s = Synth::new();
    s.place(0x100, IMM_PAT, &[]);
    let (mem, modules) = engine(&s);
    let not_found = Err(MAIN_LOOP_NOT_FOUND.to_string());
    assert_eq!(resolve_caller(&mem, &modules, None, None), not_found);
    // The outer pattern is required when it is defined.
    assert_eq!(
        resolve_caller(
            &mem,
            &modules,
            Some(&pat(IMM_PAT, 6)),
            Some(&pat(OUTER_PAT, 5))
        ),
        not_found
    );
    // The named module is not loaded.
    let other = CallerPattern {
        module: "tier0.dll",
        ..pat(IMM_PAT, 6)
    };
    assert_eq!(
        resolve_caller(&mem, &modules, Some(&other), None),
        not_found
    );

    s.place(0x800, IMM_PAT, &[]);
    let (mem, modules) = engine(&s);
    assert_eq!(
        resolve_caller(&mem, &modules, Some(&pat(IMM_PAT, 6)), None),
        Err("main-loop caller is ambiguous".to_string())
    );
}
