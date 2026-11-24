use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use agyline::{
    ColorDepth, ConfigDoc, EffectiveConfig, ItemStyle, PresetScope, apply_preset, build_preset_doc,
    emit_ansi, find_scenario, parse_stdin_payload, render_lines, render_lines_with_width,
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
        "{:<38}  samples={:<5}  P50={:>7.2} us  P95={:>7.2} us  P99={:>7.2} us",
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

    let raw_json = br#"{"session_id":"bench_1","agent_state":"working","execution_mode":"auto","model":{"display_name":"Claude Opus 4.6","effort":"high"},"cwd":"/home/dev/work/clients/agyline","vcs":{"branch":"main","dirty":true},"sandbox":{"enabled":true,"allow_network":false},"context_window":{"current_usage":{"input_tokens":16000,"output_tokens":524,"cache_read_input_tokens":80000},"total_input_tokens":48000,"total_output_tokens":12000,"context_window_size":200000},"quota":{"3p-5h":{"remaining_fraction":0.82,"reset_in_seconds":8100},"3p-weekly":{"remaining_fraction":0.64,"reset_in_seconds":259200}},"ai_credits":420}"#;

    let default_doc = ConfigDoc {
        color_depth: ColorDepth::TrueColor,
        ..ConfigDoc::default()
    };
    let default_cfg = EffectiveConfig::from_doc(&default_doc, None, &BTreeMap::new(), false);

    let badge_doc = ConfigDoc {
        item_style: ItemStyle::Badge,
        ..default_doc.clone()
    };
    let badge_cfg = EffectiveConfig::from_doc(&badge_doc, None, &BTreeMap::new(), false);

    let pill_doc = ConfigDoc {
        item_style: ItemStyle::Pill,
        ..default_doc.clone()
    };
    let pill_cfg = EffectiveConfig::from_doc(&pill_doc, None, &BTreeMap::new(), false);

    let mut hud_tc_doc = build_preset_doc("hud", None).expect("hud preset");
    hud_tc_doc.color_depth = ColorDepth::TrueColor;
    let hud_tc_cfg = EffectiveConfig::from_doc(&hud_tc_doc, None, &BTreeMap::new(), false);

    let sc_normal = find_scenario("normal").expect("normal scenario");
    let sc_wide = find_scenario("showcase_wide").expect("showcase_wide scenario");
    let sc_crit = find_scenario("showcase_critical").expect("showcase_critical scenario");
    let sc_compact = find_scenario("showcase_compact").expect("showcase_compact scenario");
    let sc_busy = find_scenario("busy_subagents").expect("busy_subagents scenario");
    let sc_narrow = find_scenario("narrow_stress").expect("narrow_stress scenario");

    let mut dash_doc = ConfigDoc::default();
    apply_preset(&mut dash_doc, "dashboard", PresetScope::All, None);
    dash_doc.color_depth = ColorDepth::TrueColor;
    let dash_cfg = EffectiveConfig::from_doc(&dash_doc, None, &BTreeMap::new(), false);

    println!("=== agyline in-memory render microbenchmarks ===");

    run_bench(
        "parse + render + emit (default v0.3)",
        warmup,
        samples,
        || {
            let p = parse_stdin_payload(black_box(raw_json)).expect("parse");
            let mut frame = sc_wide.frame.clone();
            frame.payload = p;
            let rendered = render_lines(black_box(&frame), black_box(&default_cfg));
            emit_ansi(&rendered, &default_cfg).len()
        },
    );

    run_bench(
        "render + emit (default wide gradient)",
        warmup,
        samples,
        || {
            let rendered = render_lines(black_box(&sc_wide.frame), black_box(&default_cfg));
            emit_ansi(&rendered, &default_cfg).len()
        },
    );

    run_bench(
        "render + emit (default crit /compact)",
        warmup,
        samples,
        || {
            let rendered = render_lines(black_box(&sc_crit.frame), black_box(&default_cfg));
            emit_ansi(&rendered, &default_cfg).len()
        },
    );

    run_bench("render + emit (default badge)", warmup, samples, || {
        let rendered = render_lines(black_box(&sc_wide.frame), black_box(&badge_cfg));
        emit_ansi(&rendered, &badge_cfg).len()
    });

    run_bench("render + emit (default pill)", warmup, samples, || {
        let rendered = render_lines(black_box(&sc_wide.frame), black_box(&pill_cfg));
        emit_ansi(&rendered, &pill_cfg).len()
    });

    run_bench(
        "render_with_width(78) (compact <88)",
        warmup,
        samples,
        || {
            let rendered = render_lines_with_width(
                black_box(&sc_compact.frame),
                black_box(&default_cfg),
                Some(78),
            );
            emit_ansi(&rendered, &default_cfg).len()
        },
    );

    run_bench("render + emit (hud 3-row)", warmup, samples, || {
        let rendered = render_lines(black_box(&sc_normal.frame), black_box(&hud_tc_cfg));
        emit_ansi(&rendered, &hud_tc_cfg).len()
    });

    run_bench("render + emit (dashboard 20)", warmup, samples, || {
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
