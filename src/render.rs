//! Verdict timeline: raw signals on top, then one ribbon per rule coloured by
//! its verdict (alarm, ok, not enough data). Drawn from the monitor's frames,
//! so it matches the alerts.

use std::fmt::Write;

use crate::formula::Verdict;
use crate::monitor::Frame;
use crate::rules::{Rule, Severity};

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Draw a dashed line on the KV strip at this fraction.
    pub kv_threshold: Option<f64>,
    /// Only draw this many of the most recent frames.
    pub last_frames: Option<usize>,
}

const W: f64 = 1000.0;
const LEFT: f64 = 176.0;
const RIGHT: f64 = 28.0;
const TOP: f64 = 22.0;
const STRIP_H: f64 = 52.0;
const STRIP_GAP: f64 = 14.0;
const RIBBON_H: f64 = 20.0;
const RIBBON_SLOT: f64 = 46.0;

const STYLE: &str = r#"
svg.ng{--bg:#ffffff;--fg:#1f2328;--muted:#59636e;--grid:#d1d9e0;--panel:#f6f8fa;--line:#0969da;--area:#0969da33;
--ok:#cdeee3;--unk:#8c959f;--crit:#cf222e;--warn:#bf8700}
@media (prefers-color-scheme:dark){svg.ng{--bg:#0d1117;--fg:#e6edf3;--muted:#9198a1;--grid:#30363d;--panel:#151b23;--line:#58a6ff;--area:#58a6ff33;
--ok:#173a33;--unk:#6e7681;--crit:#ff7b72;--warn:#d29922}}
svg.ng .bg{fill:var(--bg)}
svg.ng text{font:11px ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;fill:var(--fg)}
svg.ng .label{font-size:12px;font-weight:600}
svg.ng .muted{fill:var(--muted)}
svg.ng .panel{fill:var(--panel);stroke:var(--grid);stroke-width:1}
svg.ng .line{fill:none;stroke:var(--line);stroke-width:1.6;stroke-linejoin:round}
svg.ng .area{fill:var(--area);stroke:none}
svg.ng .line2{fill:none;stroke:var(--fg);stroke-width:1.4;stroke-dasharray:5 3;opacity:.8}
svg.ng .thr{fill:none;stroke:var(--warn);stroke-width:1;stroke-dasharray:4 3}
svg.ng .r-ok{fill:var(--ok)}
svg.ng .r-unk{fill:url(#ng-unk)}
svg.ng .r-crit{fill:var(--crit)}
svg.ng .r-warn{fill:var(--warn)}
svg.ng .unk-bg{fill:var(--panel)}
svg.ng .unk-line{stroke:var(--unk);stroke-width:1.5}
svg.ng .guide-crit{stroke:var(--crit);stroke-width:1;stroke-dasharray:3 3;fill:none}
svg.ng .guide-warn{stroke:var(--warn);stroke-width:1;stroke-dasharray:3 3;fill:none}
svg.ng .mark-crit{fill:var(--crit)}
svg.ng .mark-warn{fill:var(--warn)}
svg.ng .mark-rec{fill:var(--bg);stroke:var(--fg);stroke-width:1.4}
svg.ng .lens{fill:none;stroke:var(--muted);stroke-width:1.2}
svg.ng .axis{stroke:var(--muted);stroke-width:1}
svg.ng .tick{stroke:var(--grid);stroke-width:1}
"#;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn n(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.1}")
    } else {
        "0.0".into()
    }
}

fn clock(t: f64) -> String {
    if t < 120.0 {
        format!("{t:.0}s")
    } else {
        let total = t.round() as i64;
        format!("{}:{:02}", total / 60, total % 60)
    }
}

/// 30.0 -> "30", 0.5 -> "0.5"
fn trim(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn verdict_words(v: Verdict) -> &'static str {
    match v {
        Verdict::True => "pattern holds (alarm)",
        Verdict::False => "pattern does not hold",
        Verdict::Unknown => "not enough data yet",
    }
}

struct Strip<'a> {
    label: &'a str,
    unit: &'a str,
    y: f64,
    values: Vec<Option<f64>>,
    ymax: f64,
    step: bool,
    area: bool,
    percent: bool,
    /// Largest value actually seen, for the "max" label (the drawn scale has a floor).
    seen_max: f64,
    threshold: Option<f64>,
    /// A second series drawn as a dashed line on the same scale.
    overlay: Option<Vec<Option<f64>>>,
    /// Replaces the generated "now ... max ..." text.
    now_text: Option<String>,
    /// A short line under the "now" text.
    hint: Option<&'a str>,
}

pub fn timeline_svg(frames: &[Frame], rules: &[Rule], opts: &RenderOptions) -> String {
    let frames = match opts.last_frames {
        Some(k) if k > 0 && frames.len() > k => &frames[frames.len() - k..],
        _ => frames,
    };

    let strips_h = 4.0 * STRIP_H + 3.0 * STRIP_GAP;
    let ribbons_top = TOP + strips_h + 34.0;
    let ribbons_h = rules.len() as f64 * RIBBON_SLOT;
    let axis_y = ribbons_top + ribbons_h + 2.0;
    let legend_y = axis_y + 40.0;
    let height = legend_y + 20.0;

    let mut o = String::with_capacity(16 * 1024);
    let _ = write!(
        o,
        r#"<svg class="ng" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {h}" width="{W}" height="{h}" role="img" aria-labelledby="ng-t ng-d">
<title id="ng-t">NexusGuard verdict timeline</title>
<desc id="ng-d">Queue length, token output and KV cache usage over time, with one ribbon per rule showing whether its pattern held, did not hold, or lacked data.</desc>
<style>{STYLE}</style>
<defs><pattern id="ng-unk" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><rect width="6" height="6" class="unk-bg"/><line x1="0" y1="0" x2="0" y2="6" class="unk-line"/></pattern></defs>
<rect class="bg" width="{W}" height="{h}"/>
"#,
        h = n(height)
    );

    if frames.is_empty() {
        let _ = writeln!(
            o,
            r#"<text x="{x}" y="{y}" class="muted" text-anchor="middle">waiting for the first scrape</text></svg>"#,
            x = n(W / 2.0),
            y = n(height / 2.0)
        );
        return o;
    }

    // Time axis. The last sample owns one interval's width, so it stays inside the plot.
    let t0 = frames[0].t;
    let t_last = frames[frames.len() - 1].t;
    let last_dt = if frames.len() > 1 {
        t_last - frames[frames.len() - 2].t
    } else {
        1.0
    };
    let t_end = t_last + last_dt;
    let plot_w = W - LEFT - RIGHT;
    let x = |t: f64| LEFT + (t - t0) / (t_end - t0) * plot_w;
    let right = LEFT + plot_w;

    // Axis ticks and vertical grid.
    let span = t_end - t0;
    let step = [
        1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ]
    .into_iter()
    .find(|s| span / s <= 10.0)
    .unwrap_or(7200.0);
    let mut ticks = Vec::new();
    let mut tick = (t0 / step).ceil() * step;
    while tick <= t_end {
        ticks.push(tick);
        tick += step;
    }
    let grid_bottom = ribbons_top + ribbons_h - (RIBBON_SLOT - RIBBON_H);
    for &tk in &ticks {
        let _ = writeln!(
            o,
            r#"<line class="tick" x1="{x}" y1="{y1}" x2="{x}" y2="{y2}"/>"#,
            x = n(x(tk)),
            y1 = n(TOP),
            y2 = n(grid_bottom)
        );
    }

    // Signal strips.
    let max_of = |vals: &[Option<f64>]| vals.iter().flatten().cloned().fold(0.0_f64, f64::max);
    let waiting: Vec<Option<f64>> = frames.iter().map(|f| Some(f.waiting)).collect();
    let running: Vec<Option<f64>> = frames.iter().map(|f| Some(f.running)).collect();
    let work_max = max_of(&waiting).max(max_of(&running));
    let work_now = format!(
        "now {} waiting · {} running",
        trim(frames[frames.len() - 1].waiting),
        trim(frames[frames.len() - 1].running)
    );
    let tokens: Vec<Option<f64>> = frames.iter().map(|f| f.token_rate).collect();
    let prompts: Vec<Option<f64>> = frames.iter().map(|f| f.prompt_rate).collect();
    let kv: Vec<Option<f64>> = frames.iter().map(|f| f.kv_usage).collect();
    let strips = [
        Strip {
            label: "work in flight",
            unit: "",
            y: TOP,
            ymax: work_max.ceil().max(4.0),
            seen_max: work_max,
            values: waiting,
            step: true,
            area: true,
            percent: false,
            threshold: None,
            overlay: Some(running),
            now_text: Some(work_now),
            hint: Some("fill: waiting, dashed: running"),
        },
        Strip {
            label: "generated tokens/s",
            unit: "/s",
            y: TOP + STRIP_H + STRIP_GAP,
            ymax: (max_of(&tokens) * 1.1).ceil().max(1.0),
            seen_max: max_of(&tokens),
            values: tokens,
            step: false,
            area: true,
            percent: false,
            threshold: None,
            overlay: None,
            now_text: None,
            hint: None,
        },
        Strip {
            label: "prompt tokens/s",
            unit: "/s",
            y: TOP + 2.0 * (STRIP_H + STRIP_GAP),
            ymax: (max_of(&prompts) * 1.1).ceil().max(1.0),
            seen_max: max_of(&prompts),
            values: prompts,
            step: false,
            area: true,
            percent: false,
            threshold: None,
            overlay: None,
            now_text: None,
            hint: None,
        },
        Strip {
            label: "KV cache used",
            unit: "",
            y: TOP + 3.0 * (STRIP_H + STRIP_GAP),
            ymax: 1.0,
            seen_max: max_of(&kv),
            values: kv,
            step: false,
            area: false,
            percent: true,
            threshold: opts.kv_threshold,
            overlay: None,
            now_text: None,
            hint: None,
        },
    ];

    let xs: Vec<f64> = frames.iter().map(|f| x(f.t)).collect();
    for s in &strips {
        let base = s.y + STRIP_H;
        let yv = |v: f64| base - (v / s.ymax).clamp(0.0, 1.0) * (STRIP_H - 6.0) - 3.0;
        let _ = writeln!(
            o,
            r#"<rect class="panel" x="{x}" y="{y}" width="{w}" height="{h}" rx="3"/>"#,
            x = n(LEFT),
            y = n(s.y),
            w = n(plot_w),
            h = n(STRIP_H)
        );
        if let Some(th) = s.threshold {
            let _ = writeln!(
                o,
                r#"<line class="thr" x1="{x1}" y1="{y}" x2="{x2}" y2="{y}"><title>threshold {th:.2}</title></line>"#,
                x1 = n(LEFT),
                x2 = n(right),
                y = n(yv(th))
            );
        }

        // Runs of consecutive present values, so a missing metric leaves a gap.
        let mut run: Vec<(f64, f64)> = Vec::new();
        let flush = |run: &mut Vec<(f64, f64)>, o: &mut String| {
            if run.is_empty() {
                return;
            }
            let mut d = String::new();
            for (i, (px, py)) in run.iter().enumerate() {
                let _ = write!(
                    d,
                    "{}{} {} ",
                    if i == 0 { "M" } else { "L" },
                    n(*px),
                    n(*py)
                );
            }
            if s.area {
                let first = run[0].0;
                let last = run[run.len() - 1].0;
                let _ = writeln!(
                    o,
                    r#"<path class="area" d="{d}L{} {} L{} {} Z"/>"#,
                    n(last),
                    n(base),
                    n(first),
                    n(base)
                );
            }
            let _ = writeln!(o, r#"<path class="line" d="{}"/>"#, d.trim_end());
            run.clear();
        };
        for (i, v) in s.values.iter().enumerate() {
            match v {
                Some(v) => {
                    let end = if i + 1 < xs.len() { xs[i + 1] } else { right };
                    if s.step {
                        run.push((xs[i], yv(*v)));
                        run.push((end, yv(*v)));
                    } else {
                        run.push((xs[i], yv(*v)));
                    }
                }
                None => flush(&mut run, &mut o),
            }
        }
        flush(&mut run, &mut o);

        if let Some(overlay) = &s.overlay {
            let mut d = String::new();
            for (i, v) in overlay.iter().enumerate() {
                if let Some(v) = v {
                    let end = if i + 1 < xs.len() { xs[i + 1] } else { right };
                    let y = n(yv(*v));
                    let _ = write!(
                        d,
                        "{}{} {} L{} {} ",
                        if d.is_empty() { "M" } else { "L" },
                        n(xs[i]),
                        y,
                        n(end),
                        y
                    );
                }
            }
            if !d.is_empty() {
                let _ = writeln!(o, r#"<path class="line2" d="{}"/>"#, d.trim_end());
            }
        }

        let max_label = if s.percent {
            format!("{:.0}%", s.seen_max * 100.0)
        } else {
            format!("{}{}", trim(s.seen_max.round()), s.unit)
        };
        let now = match (&s.now_text, s.values.last().copied().flatten()) {
            (Some(text), _) => text.clone(),
            (None, Some(v)) if s.percent => format!("now {:.0}% · max {max_label}", v * 100.0),
            (None, Some(v)) => format!("now {}{} · max {max_label}", trim(v), s.unit),
            (None, None) => "no data".to_string(),
        };
        let _ = writeln!(
            o,
            r#"<text class="label" text-anchor="end" x="{x}" y="{y}">{l}</text>
<text class="muted" text-anchor="end" x="{x}" y="{y2}">{now}</text>"#,
            x = n(LEFT - 12.0),
            y = n(s.y + 18.0),
            y2 = n(s.y + 33.0),
            l = esc(s.label)
        );
        if let Some(hint) = s.hint {
            let _ = writeln!(
                o,
                r#"<text class="muted" text-anchor="end" x="{x}" y="{y}">{h}</text>"#,
                x = n(LEFT - 12.0),
                y = n(s.y + 47.0),
                h = esc(hint)
            );
        }
    }

    // Rule ribbons.
    let _ = writeln!(
        o,
        r#"<text class="muted" x="{x}" y="{y}">what each rule concluded</text>"#,
        x = n(LEFT),
        y = n(ribbons_top - 12.0)
    );
    for (ri, rule) in rules.iter().enumerate() {
        let y = ribbons_top + ri as f64 * RIBBON_SLOT;
        let (sev_class, sev_word) = match rule.severity {
            Severity::Critical => ("crit", "critical"),
            Severity::Warning => ("warn", "warning"),
        };
        let _ = writeln!(
            o,
            r#"<text class="label" text-anchor="end" x="{x}" y="{y}">{name}</text><text class="muted" text-anchor="end" x="{x}" y="{y2}">{sev_word}</text>"#,
            x = n(LEFT - 12.0),
            y = n(y + 13.0),
            y2 = n(y + 27.0),
            name = esc(&rule.name)
        );

        // Merge neighbouring cells with the same verdict into one rectangle.
        let verdict_at = |i: usize| {
            frames[i]
                .verdicts
                .get(ri)
                .copied()
                .unwrap_or(Verdict::Unknown)
        };
        let mut i = 0;
        while i < frames.len() {
            let v = verdict_at(i);
            let mut j = i;
            while j + 1 < frames.len() && verdict_at(j + 1) == v {
                j += 1;
            }
            let x1 = xs[i];
            let x2 = if j + 1 < xs.len() { xs[j + 1] } else { right };
            let class = match v {
                Verdict::True => format!("r-{sev_class}"),
                Verdict::False => "r-ok".to_string(),
                Verdict::Unknown => "r-unk".to_string(),
            };
            let _ = writeln!(
                o,
                r#"<rect class="{class}" x="{x}" y="{y}" width="{w}" height="{h}"><title>{name}: {words}, {a} to {b}</title></rect>"#,
                x = n(x1),
                y = n(y),
                w = n((x2 - x1).max(0.5)),
                h = n(RIBBON_H),
                name = esc(&rule.name),
                words = verdict_words(v),
                a = clock(frames[i].t),
                b = clock(frames[j].t)
            );
            i = j + 1;
        }

        // Alarm and recovery markers, with a guide line up through the signal strips.
        for (fi, f) in frames.iter().enumerate() {
            if fi == 0 {
                continue; // whatever happened before the first visible sample is unknown
            }
            let now = f.firing.get(ri).copied().unwrap_or(false);
            let before = frames[fi - 1].firing.get(ri).copied().unwrap_or(false);
            let px = xs[fi];
            if now && !before {
                let _ = writeln!(
                    o,
                    r#"<line class="guide-{sev_class}" x1="{px}" y1="{y1}" x2="{px}" y2="{y2}"/><path class="mark-{sev_class}" d="M{a} {b} L{c} {b} L{px} {d} Z"><title>{name} fired at {t}</title></path>"#,
                    px = n(px),
                    y1 = n(TOP),
                    y2 = n(y),
                    a = n(px - 5.0),
                    c = n(px + 5.0),
                    b = n(y - 9.0),
                    d = n(y - 1.0),
                    name = esc(&rule.name),
                    t = clock(f.t)
                );
            } else if !now && before {
                let _ = writeln!(
                    o,
                    r#"<circle class="mark-rec" cx="{px}" cy="{cy}" r="4"><title>{name} recovered at {t}</title></circle>"#,
                    px = n(px),
                    cy = n(y - 5.0),
                    name = esc(&rule.name),
                    t = clock(f.t)
                );
            }
        }

        // The lens: how far back this rule looks from the latest sample.
        let lookback = rule.pattern.lookback();
        let lx1 = x(t_last - lookback).max(LEFT);
        let lx2 = xs[frames.len() - 1];
        let ly = y + RIBBON_H + 8.0;
        let clipped = t_last - lookback < t0;
        let narrow = lx2 - lx1 < 120.0;
        let _ = writeln!(
            o,
            r#"<path class="lens" d="M{a} {t1} V{t2} M{a} {ly} H{b} M{b} {t1} V{t2}"><title>{name} looks back {lb}s from the latest sample</title></path><text class="muted" text-anchor="{anchor}" x="{tx}" y="{ty}">looks back {lb}s{more}</text>"#,
            a = n(lx1),
            b = n(lx2),
            ly = n(ly),
            t1 = n(ly - 3.0),
            t2 = n(ly + 3.0),
            ty = n(ly + 15.0),
            tx = n(if narrow { lx2 } else { lx1 }),
            anchor = if narrow { "end" } else { "start" },
            lb = trim(lookback),
            name = esc(&rule.name),
            more = if clipped { " (longer than shown)" } else { "" }
        );
    }

    // Time axis.
    let _ = writeln!(
        o,
        r#"<line class="axis" x1="{x1}" y1="{y}" x2="{x2}" y2="{y}"/>"#,
        x1 = n(LEFT),
        x2 = n(right),
        y = n(axis_y)
    );
    for &tk in &ticks {
        let _ = writeln!(
            o,
            r#"<line class="axis" x1="{x}" y1="{y1}" x2="{x}" y2="{y2}"/><text class="muted" text-anchor="middle" x="{x}" y="{ty}">{l}</text>"#,
            x = n(x(tk)),
            y1 = n(axis_y),
            y2 = n(axis_y + 4.0),
            ty = n(axis_y + 16.0),
            l = clock(tk)
        );
    }

    // Legend.
    let items = [
        ("r-crit", "alarm: critical rule's pattern holds"),
        ("r-warn", "alarm: warning rule's pattern holds"),
        ("r-ok", "pattern does not hold"),
        ("r-unk", "not enough data yet"),
    ];
    let mut lx = LEFT;
    for (class, text) in items {
        let _ = writeln!(
            o,
            r#"<rect class="{class}" x="{x}" y="{y}" width="14" height="10" rx="2"/><text class="muted" x="{tx}" y="{ty}">{text}</text>"#,
            x = n(lx),
            y = n(legend_y - 9.0),
            tx = n(lx + 20.0),
            ty = n(legend_y),
        );
        lx += 20.0 + 5.6 * text.len() as f64 + 14.0;
    }

    o.push_str("</svg>\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Monitor;
    use crate::rules::{builtin_rules, RuleConfig};
    use crate::sim::{generate, Scenario};

    fn frames_of(s: Scenario, count: usize) -> (Vec<Frame>, Vec<Rule>) {
        let rules = builtin_rules(&RuleConfig::default());
        let mut m = Monitor::new(rules.clone());
        for sample in generate(s, count, 2.0) {
            m.ingest(sample).unwrap();
        }
        (m.frames().iter().cloned().collect(), rules)
    }

    #[test]
    fn empty_input_gives_a_placeholder_not_a_panic() {
        let svg = timeline_svg(
            &[],
            &builtin_rules(&RuleConfig::default()),
            &RenderOptions::default(),
        );
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("waiting for the first scrape"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn a_single_frame_renders() {
        let (frames, rules) = frames_of(Scenario::Healthy, 1);
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(!svg.contains("NaN") && !svg.contains("inf"));
    }

    #[test]
    fn a_stall_shows_the_alarm_and_names_every_rule() {
        let (frames, rules) = frames_of(Scenario::Stall, 60);
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        for r in &rules {
            assert!(svg.contains(&r.name), "missing rule {}", r.name);
        }
        assert!(
            svg.contains("engine_stalled fired at 70s"),
            "alarm marker missing"
        );
        assert!(svg.contains("pattern holds (alarm)"));
    }

    #[test]
    fn attaching_to_an_already_stalled_server_shows_the_not_enough_data_stretch() {
        let (frames, rules) = frames_of(Scenario::AttachedStalled, 40);
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        assert!(svg.contains("engine_stalled: not enough data yet"));
        // The first sample has no rate, so the 30s window is complete one sample later.
        assert!(svg.contains("engine_stalled fired at 32s"));
    }

    #[test]
    fn a_window_that_starts_mid_alarm_does_not_invent_a_firing_time() {
        let (frames, rules) = frames_of(Scenario::Stall, 60);
        let opts = RenderOptions {
            last_frames: Some(10),
            ..Default::default()
        };
        let svg = timeline_svg(&frames, &rules, &opts);
        assert!(svg.contains("pattern holds (alarm)"));
        assert!(
            !svg.contains(" fired at "),
            "the alarm began before this window"
        );
    }

    #[test]
    fn the_max_label_shows_the_real_maximum() {
        let (frames, rules) = frames_of(Scenario::Stall, 60);
        let opts = RenderOptions {
            last_frames: Some(10),
            ..Default::default()
        };
        let svg = timeline_svg(&frames, &rules, &opts);
        assert!(svg.contains("now 0/s · max 0/s"));
        assert!(svg.contains("prompt tokens/s"));
        assert!(svg.contains("work in flight"));
        assert!(
            svg.contains("class=\"line2\""),
            "running requests should be drawn"
        );
    }

    #[test]
    fn healthy_traffic_draws_no_alarm_marker() {
        let (frames, rules) = frames_of(Scenario::Healthy, 100);
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        assert!(!svg.contains(" fired at "));
        assert!(!svg.contains("pattern holds (alarm)"));
    }

    #[test]
    fn neighbouring_cells_are_merged_into_runs() {
        let (frames, rules) = frames_of(Scenario::Healthy, 900);
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        // 900 samples x 3 rules would be 2700 cells; runs of equal verdicts collapse them.
        let ribbon_rects = svg.matches("<rect class=\"r-ok\" x=").count()
            + svg.matches("<rect class=\"r-unk\" x=").count();
        assert!(ribbon_rects < 12, "got {ribbon_rects} ribbon rectangles");
    }

    #[test]
    fn last_frames_limits_how_much_is_drawn() {
        let (frames, rules) = frames_of(Scenario::Healthy, 100);
        let opts = RenderOptions {
            last_frames: Some(10),
            ..Default::default()
        };
        let svg = timeline_svg(&frames, &rules, &opts);
        assert!(
            svg.contains("180s") || svg.contains("3:00"),
            "axis should start near t=180s"
        );
        assert!(!svg.contains(">20s<"));
    }

    #[test]
    fn markup_is_balanced_and_text_is_escaped() {
        let (frames, mut rules) = frames_of(Scenario::Stall, 60);
        rules[0].name = "a<b>&\"c\"".into();
        let svg = timeline_svg(&frames, &rules, &RenderOptions::default());
        assert!(!svg.contains("a<b>"));
        assert!(svg.contains("a&lt;b&gt;&amp;&quot;c&quot;"));
        assert_eq!(svg.matches('<').count(), svg.matches('>').count());
    }
}
