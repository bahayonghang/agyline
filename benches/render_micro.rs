use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use agyline::{
    ColorDepth, ConfigDoc, EffectiveConfig, PresetScope, apply_preset, emit_ansi, find_scenario,
    parse_stdin_payload, render_lines, render_lines_with_width,
};

fn percentile_us(sorted: &[Duration], numer: usize, denom: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) * numer) / denom;
    sorted[idx].as_nanos() as f64 / 1_000.0
}

fn run_bench<F: FnMut() -> usize>(label: &str, warmup: usize, samples: usize, mut f: F) {
    for _ in 0..warmup {
        black_box(f());
    }
    let mut times = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = Instant::now();
        let len = f();
        black_box(len);
        times.push(t0.elapsed());
    }
    times.sort_unstable();
    println!(
        "{:<34}  samples={:<5}  P50={:>7.2} us  P95={:>7.2} us  P99={:>7.2} us",
        label,
        samples,
        percentile_us(&times, 50, 100),
        percentile_us(&times, 95, 100),
        percentile_us(&times, 99, 100),
    );
}

fn main() {
    let warmup = 200;
    let samples = 1_000;

    let raw_json = br#"{"session_id":"bench_1","agent_state":"working","execution_mode":"auto","model":{"display_name":"Gemini 2.5 Pro","effort":"high"},"cwd":"/workspace/agyline","vcs":{"branch":"main","dirty":true},"sandbox":{"enabled":true,"allow_network":false},"context_window":{"current_usage":{"input_tokens":9600,"output_tokens":524,"cache_read_input_tokens":230400},"total_output_tokens":524,"context_window_size":1000000},"quota":{"gemini-5h":{"remaining_fraction":0.68,"reset_in_seconds":11520},"3p-5h":{"remaining_fraction":0.53,"reset_in_seconds":6300}},"cost":{"total_usd":1.42,"subagent_usd":0.64}}"#;

    let default_cfg = EffectiveConfig::default();
    let hud_tc_doc = ConfigDoc {
        color_depth: ColorDepth::TrueColor,
        ..ConfigDoc::default()
    };
    let hud_tc_cfg = EffectiveConfig::from_doc(&hud_tc_doc, None, &BTreeMap::new(), false);
    let sc_normal = find_scenario("normal").expect("normal scenario");
    let sc_busy = find_scenario("busy_subagents").expect("busy_subagents scenario");
    let sc_narrow = find_scenario("narrow_stress").expect("narrow_stress scenario");

    let mut dash_doc = ConfigDoc::default();
    apply_preset(&mut dash_doc, "dashboard", PresetScope::All, None);
    dash_doc.color_depth = ColorDepth::TrueColor;
    let dash_cfg = EffectiveConfig::from_doc(&dash_doc, None, &BTreeMap::new(), false);

    println!("=== agyline in-memory render microbenchmarks ===");

    run_bench(
        "parse + render + emit_ansi (default)",
        warmup,
        samples,
        || {
            let p = parse_stdin_payload(black_box(raw_json)).expect("parse");
            let mut frame = sc_normal.frame.clone();
            frame.payload = p;
            let rendered = render_lines(black_box(&frame), black_box(&default_cfg));
            emit_ansi(&rendered, &default_cfg).len()
        },
    );

    run_bench("render + emit_ansi (hud 3-row)", warmup, samples, || {
        let rendered = render_lines(black_box(&sc_normal.frame), black_box(&hud_tc_cfg));
        emit_ansi(&rendered, &hud_tc_cfg).len()
    });

    run_bench("render + emit_ansi (dashboard 20)", warmup, samples, || {
        let rendered = render_lines(black_box(&sc_busy.frame), black_box(&dash_cfg));
        emit_ansi(&rendered, &dash_cfg).len()
    });

    run_bench("render_with_width(74) (hud 3-row)", warmup, samples, || {
        let rendered = render_lines_with_width(
            black_box(&sc_narrow.frame),
            black_box(&hud_tc_cfg),
            Some(74),
        );
        emit_ansi(&rendered, &hud_tc_cfg).len()
    });

    run_bench("render_with_width(74) (narrowing)", warmup, samples, || {
        let rendered =
            render_lines_with_width(black_box(&sc_narrow.frame), black_box(&dash_cfg), Some(74));
        emit_ansi(&rendered, &dash_cfg).len()
    });
}
