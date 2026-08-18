// CC V3: Relax-specific aim evaluator.
//
// Purpose:
//   * N/X pattern detection — detects alternating zigzag and crossover patterns
//     that are trivial on RX via angle pair analysis and distance consistency.
//   * Aim slop detection — identifies constant velocity + constant distance +
//     low angle variance windows (mechanical patterns) for progressive nerfing.
//   * Tech pattern boost — rewards high angle variance + high velocity change
//     ratio as genuine technical aim.
//   * Neutral flow protection — prevents accidental tech buffs on common flow
//     patterns that stay inside typical distance bands (112-90, 90-70, etc.).
//   * Delayed tech buff — avoids immediate tech application right after farm
//     sections to prevent disproportionate strain from isolated anomalies.
//   * Tech boost overall cap — limits maximum positive adjustment.
//   * Akat calibration — scales output to approximately match ccv3-pp-rs
//     despite rosu's higher SKILL_MULTIPLIER and wider angle bonus shapes.
//
// Akat calibration details:
//   Derivation: akat_rx / akat_vanilla × rosu_vanilla × (25.18/26.0)
//     Wide:  1.56/1.45 × 1.5 × 0.968 = 1.56
//     Acute: 2.05/1.90 × 2.55 × 0.968 = 2.66
//     Slider: 1.20/1.35 × 1.35 × 0.968 = 1.16
//     VelCh: 0.78/0.70 × 0.75 × 0.968 = 0.81
//   Final AKAT_CALIBRATION (0.92) accounts for cumulative angle shape
//   differences (smoothstep broader than sin²) + SKILL_MULTIPLIER gap.
//
// Uses rosu's formula base (smoothstep/smootherstep, adjusted_delta_time,
// wiggle_bonus, small_circle_bonus, DIAMETER-based gating).

use std::f64::consts::PI;

use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::{
        difficulty::{milliseconds_to_bpm, reverse_lerp, smootherstep, smoothstep},
        float_ext::FloatExt,
    },
};

pub struct AimRxEvaluator;

// ─── Windowed statistics helpers ────────────────────────────────────

const ANGLE_WINDOW: usize = 8;

fn windowed_angle_stats<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> (f64, f64, usize) {
    let mut angles: Vec<f64> = Vec::with_capacity(window + 1);

    if let Some(a) = curr.angle {
        angles.push(a);
    }
    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            if let Some(a) = prev.angle {
                angles.push(a);
            }
        } else {
            break;
        }
    }

    let n = angles.len();
    if n < 3 {
        return (0.0, 0.0, n);
    }

    let mean: f64 = angles.iter().sum::<f64>() / n as f64;
    let variance: f64 = angles.iter().map(|a| (a - mean).powi(2)).sum::<f64>() / n as f64;
    (mean, variance.sqrt(), n)
}

fn windowed_dist_stats<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> (f64, f64, usize) {
    let mut dists: Vec<f64> = Vec::with_capacity(window + 1);
    dists.push(curr.lazy_jump_dist);

    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            dists.push(prev.lazy_jump_dist);
        } else {
            break;
        }
    }

    let n = dists.len();
    if n < 2 {
        return (0.0, 0.0, n);
    }
    let mean = dists.iter().sum::<f64>() / n as f64;
    let var = dists.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n as f64;
    (mean, var.sqrt(), n)
}

fn windowed_vel_stats<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> (f64, f64, usize) {
    let mut vels: Vec<f64> = Vec::with_capacity(window + 1);
    if curr.adjusted_delta_time > 0.0 {
        vels.push(curr.lazy_jump_dist / curr.adjusted_delta_time);
    }

    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            if prev.adjusted_delta_time > 0.0 {
                vels.push(prev.lazy_jump_dist / prev.adjusted_delta_time);
            }
        } else {
            break;
        }
    }

    let n = vels.len();
    if n < 2 {
        return (0.0, 0.0, n);
    }
    let mean = vels.iter().sum::<f64>() / n as f64;
    let var = vels.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    (mean, var.sqrt(), n)
}

/// Windowed stats over the gap (delta) TIMES of recent objects — used to spot
/// a "stream signature": a consistent, fast 1/4 rhythm. Returns
/// (mean_delta_ms, stddev_delta_ms, count).
fn windowed_delta_stats<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> (f64, f64, usize) {
    let mut deltas: Vec<f64> = Vec::with_capacity(window + 1);
    if curr.adjusted_delta_time > 0.0 {
        deltas.push(curr.adjusted_delta_time);
    }

    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            if prev.adjusted_delta_time > 0.0 {
                deltas.push(prev.adjusted_delta_time);
            }
        } else {
            break;
        }
    }

    let n = deltas.len();
    if n < 2 {
        return (0.0, 0.0, n);
    }
    let mean = deltas.iter().sum::<f64>() / n as f64;
    let var = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n as f64;
    (mean, var.sqrt(), n)
}

/// Detect N/X alternating patterns: look at consecutive angle pairs
/// and check if they alternate between two values (±tolerance).
fn detect_nx_pattern<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> f64 {
    let mut angles: Vec<f64> = Vec::with_capacity(window + 1);
    if let Some(a) = curr.angle {
        angles.push(a);
    }
    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            if let Some(a) = prev.angle {
                angles.push(a);
            }
        } else {
            break;
        }
    }

    if angles.len() < 4 {
        return 0.0;
    }

    let evens: Vec<f64> = angles.iter().step_by(2).copied().collect();
    let odds: Vec<f64> = angles.iter().skip(1).step_by(2).copied().collect();

    if evens.len() < 2 || odds.len() < 2 {
        return 0.0;
    }

    let even_mean = evens.iter().sum::<f64>() / evens.len() as f64;
    let odd_mean = odds.iter().sum::<f64>() / odds.len() as f64;

    let even_var = evens.iter().map(|a| (a - even_mean).powi(2)).sum::<f64>() / evens.len() as f64;
    let odd_var = odds.iter().map(|a| (a - odd_mean).powi(2)).sum::<f64>() / odds.len() as f64;

    let even_stddev = even_var.sqrt();
    let odd_stddev = odd_var.sqrt();

    let cluster_tight = even_stddev < 0.25 && odd_stddev < 0.25;
    let clusters_differ = (even_mean - odd_mean).abs() > 0.3;

    if cluster_tight && clusters_differ {
        let tightness = 1.0 - ((even_stddev + odd_stddev) / 0.50).clamp(0.0, 1.0);
        let separation = ((even_mean - odd_mean).abs() / PI).clamp(0.0, 1.0);
        tightness * separation
    } else {
        0.0
    }
}

impl AimRxEvaluator {
    // Recalibrated constants to produce akat-equivalent pp output.
    const WIDE_ANGLE_MULTIPLIER: f64 = 1.15; // Wide angles in my opinion are more difficult than acute angles with aim assist, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.
    const ACUTE_ANGLE_MULTIPLIER: f64 = 1.0; // Acute angles in my opinion are less difficult than wide angles with aim assist, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.
    const SLIDER_MULTIPLIER: f64 = 0.55; // Reduced slider bonus to avoid over-boosting aim pp on maps with many complex fast sliders.
    const VELOCITY_CHANGE_MULTIPLIER: f64 = 0.85; // Velocity change bonus is slightly increased to reward technical aim that requires quick adjustments in speed. 
    const WIGGLE_MULTIPLIER: f64 = 0.81; // Wiggly patterns are generally less difficult in a cheat environment than straight aim patterns, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.

    const AKAT_CALIBRATION: f64 = 1.0; // No Akat calibration applied in this version; adjust if needed.

    // ── Stream-density nerf (CC V3) ─────────────────────────────────
    // Heavily nerfs fast, tightly-spaced 1/4 stream aim under Relax. These
    // maps (high-BPM symphonic/speedcore streams) are pure tapping in vanilla
    // but become huge "aim" pp under RX where the tapping is free. The nerf
    // keys off effective stream BPM (from adjusted_delta_time) and spacing
    // (lazy_jump_dist): a note only counts as farm-stream when it is BOTH fast
    // AND tightly spaced, so genuine jump/flow aim (large spacing) is untouched.
    //
    // ramp from no nerf at STREAM_BPM_MIN up to full at STREAM_BPM_MAX, gated
    // by a spacing window that fades the nerf out once notes are jump-spaced.
    const STREAM_BPM_MIN: f64 = 200.0; // below this eff-BPM: no stream nerf
    const STREAM_BPM_MAX: f64 = 280.0; // at/above this: full BPM weight
    const STREAM_DIST_FULL: f64 = 80.0; // <= this spacing (px): full nerf weight
    const STREAM_DIST_EXEMPT: f64 = 170.0; // >= this spacing: exempt (real jumps)
    const STREAM_MAX_NERF: f64 = 0.55; // up to 55% strain removed on the worst streams

    // ── Stream-signature tech-buff gate (CC V3) ─────────────────────
    // A "stream signature" is a consistent, fast 1/4 RHYTHM — regardless of
    // spatial spacing. Spaced streams (similar patterns but spaced) have the
    // same even rhythm as a normal stream but larger jumps, which creates
    // velocity variety and can otherwise trip the tech buff. Under Relax that's
    // still just a stream, so the tech buff must NOT apply. Detection is purely
    // rhythmic: short mean gap (fast) + low gap-time variance (even rhythm).
    const STREAM_SIG_BPM_MIN: f64 = 180.0; // mean 1/4 BPM at/above which it's "fast"
    const STREAM_SIG_CV_MAX: f64 = 0.18; // gap-time coeff. of variation below which it's "even"
    const STREAM_SIG_MIN_NOTES: usize = 5; // need a real run, not 2-3 notes

    const SLOW_SLIDER_VEL_FLOOR: f64 = 0.55;

    const FOLLOW_POINT_DISTANCE: f64 = 112.0;
    const FARM_MAX_NERF: f64 = 0.35;
    const RELAX_REPEAT_NERF_RADIUS_START: f64 = 36.9;
    const RELAX_REPEAT_NERF_RADIUS_END: f64 = 24.4;
    const RELAX_REPEAT_NERF_EXPONENT: f64 = 9.0;
    const CONSTANT_DIST_RATIO: f64 = 0.18;
    const EDGE_TO_EDGE_THRESHOLD: f64 = 360.0;
    const CONSTANT_DIST_BPM_STRAIN_TIME: f64 = 85.7;

    const FLOW_MIN_EFF_BPM: f64 = 210.25;
    const FLOW_MEAN_ANGLE_THRESHOLD: f64 = 2.0;
    const FLOW_STDDEV_THRESHOLD: f64 = 0.3;
    const FLOW_MAX_NERF: f64 = 0.50;
    const FLOW_DIST_FULL_NERF: f64 = 50.0;
    const FLOW_DIST_EXEMPT: f64 = 97.0;

    #[warn(dead_code)]
    const NX_MAX_NERF: f64 = 0.30;
    #[warn(dead_code)]
    const SLOP_MAX_NERF: f64 = 0.35;
    
    const TECH_MAX_BOOST: f64 = 0.08;

    // Additional tuning constants
    const TECH_OVERALL_CAP: f64 = 1.08;

    const NEUTRAL_FLOW_DIST_RANGES: [(f64, f64); 5] = [
        (90.0, 112.0),
        (70.0, 90.0),
        (50.0, 70.0),
        (30.0, 50.0),
        (10.0, 30.0),
    ];

    /// True when recent objects form a stream signature: a consistent, fast
    /// 1/4 rhythm. Spacing is intentionally ignored, so spaced streams count.
    fn is_stream_pattern<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
    ) -> bool {
        let (delta_mean, delta_stddev, n) =
            windowed_delta_stats(curr, diff_objects, ANGLE_WINDOW);
        if n < Self::STREAM_SIG_MIN_NOTES || delta_mean <= 0.0 {
            return false;
        }
        let eff_bpm = milliseconds_to_bpm(delta_mean, None);
        let cv = delta_stddev / delta_mean;
        eff_bpm >= Self::STREAM_SIG_BPM_MIN && cv <= Self::STREAM_SIG_CV_MAX
    }

    fn relax_repeat_nerf_radius_factor(circle_radius: f64) -> f64 {
        if circle_radius >= Self::RELAX_REPEAT_NERF_RADIUS_START {
            return 1.0;
        }

        let ratio = ((Self::RELAX_REPEAT_NERF_RADIUS_START - circle_radius)
            / (Self::RELAX_REPEAT_NERF_RADIUS_START - Self::RELAX_REPEAT_NERF_RADIUS_END))
            .clamp(0.0, 1.0);

        f64::exp(-Self::RELAX_REPEAT_NERF_EXPONENT * ratio)
    }

    fn is_neutral_flow_pattern<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
    ) -> bool {
        let (dist_mean, dist_stddev, n) = windowed_dist_stats(curr, diff_objects, ANGLE_WINDOW);
        if n < 6 || dist_mean < 5.0 {
            return false;
        }

        let cv = dist_stddev / dist_mean;
        if cv > 0.25 {
            return false;
        }

        Self::NEUTRAL_FLOW_DIST_RANGES.iter().any(|&(low, high)| {
            dist_mean >= low && dist_mean <= high
        })
    }

    // Farm streak considers all farm-related nerfs (N/X, slop, cross-screen).
    // Flow aim is excluded.
    fn recent_farm_streak<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        window: usize,
    ) -> usize {
        let mut streak = 0;
        let mut current = Some(curr);

        while streak < window {
            if let Some(obj) = current {
                let nx_strength = detect_nx_pattern(obj, diff_objects, 4);

                // Angle consistency
                let (_, angle_stddev, angle_n) = windowed_angle_stats(obj, diff_objects, 4);

                // Distance consistency
                let (dist_mean, dist_stddev, dist_n) = windowed_dist_stats(obj, diff_objects, 4);

                // Velocity consistency
                let (vel_mean, vel_stddev, vel_n) = windowed_vel_stats(obj, diff_objects, 4);

                let slop_like = (angle_n >= 3 && angle_stddev < 0.30)
                    || (dist_n >= 3 && dist_mean > 0.0 && dist_stddev / dist_mean < 0.16)
                    || (vel_n >= 3 && vel_mean > 0.0 && vel_stddev / vel_mean < 0.16);

                // Cross-screen constant distance
                let cross_like = {
                    let curr_d = obj.lazy_jump_dist;
                    if let Some(prev) = obj.previous(0, diff_objects) {
                        let prev_d = prev.lazy_jump_dist;
                        let max_d = curr_d.max(prev_d);
                        if max_d > 80.0 {
                            let change_ratio = (max_d - prev_d.min(curr_d)) / max_d;
                            change_ratio < 0.20
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                let is_farm = nx_strength > 0.07 || slop_like || cross_like;

                if is_farm {
                    streak += 1;
                    current = obj.previous(0, diff_objects);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        streak
    }

    fn recent_no_followpoint_streak<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        window: usize,
    ) -> usize {
        let mut streak = 0;
        let mut current = Some(curr);

        while streak < window {
            if let Some(obj) = current {
                if obj.lazy_jump_dist <= Self::FOLLOW_POINT_DISTANCE {
                    streak += 1;
                    current = obj.previous(0, diff_objects);
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        streak
    }

    fn combine_farm_severity(nx_strength: f64, slop_severity: f64, cross_severity: f64) -> f64 {
        1.0 - (1.0 - nx_strength) * (1.0 - slop_severity) * (1.0 - cross_severity)
    }

    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        with_slider_travel_dist: bool,
    ) -> f64 {
        let osu_curr_obj = curr;

        let Some((osu_last_last_obj, osu_last_obj)) = curr
            .previous(1, diff_objects)
            .zip(curr.previous(0, diff_objects))
            .filter(|(_, last)| !(curr.base.is_spinner() || last.base.is_spinner()))
        else {
            return 0.0;
        };

        const RADIUS: i32 = OsuDifficultyObject::NORMALIZED_RADIUS;
        const DIAMETER: i32 = OsuDifficultyObject::NORMALIZED_DIAMETER;

        // ── Velocities ──────────────────────────────────────────────
        let mut curr_vel = osu_curr_obj.lazy_jump_dist / osu_curr_obj.adjusted_delta_time;

        if osu_last_obj.base.is_slider() && with_slider_travel_dist {
            let travel_vel = osu_last_obj.travel_dist / osu_last_obj.travel_time;
            let movement_vel = osu_curr_obj.min_jump_dist / osu_curr_obj.min_jump_time;
            curr_vel = curr_vel.max(movement_vel + travel_vel);
        }

        let mut prev_vel = osu_last_obj.lazy_jump_dist / osu_last_obj.adjusted_delta_time;

        if osu_last_last_obj.base.is_slider() && with_slider_travel_dist {
            let travel_vel = osu_last_last_obj.travel_dist / osu_last_last_obj.travel_time;
            let movement_vel = osu_last_obj.min_jump_dist / osu_last_obj.min_jump_time;
            prev_vel = prev_vel.max(movement_vel + travel_vel);
        }

        let mut wide_angle_bonus = 0.0;
        let mut acute_angle_bonus = 0.0;
        let mut slider_bonus = 0.0;
        let mut vel_change_bonus = 0.0;
        let mut wiggle_bonus = 0.0;

        let mut aim_strain = curr_vel;

        // ── Angle bonuses ───────────────────────────────────────────
        if let Some((curr_angle, last_angle)) = osu_curr_obj.angle.zip(osu_last_obj.angle) {
            let angle_bonus = curr_vel.min(prev_vel);

            if osu_curr_obj
                .adjusted_delta_time
                .max(osu_last_obj.adjusted_delta_time)
                < 1.25
                    * osu_curr_obj
                        .adjusted_delta_time
                        .min(osu_last_obj.adjusted_delta_time)
            {
                acute_angle_bonus = Self::calc_acute_angle_bonus(curr_angle);

                acute_angle_bonus *= 0.08
                    + 0.92
                        * (1.0
                            - f64::min(
                                acute_angle_bonus,
                                f64::powf(Self::calc_acute_angle_bonus(last_angle), 3.0),
                            ));

                acute_angle_bonus *= angle_bonus
                    * smootherstep(
                        milliseconds_to_bpm(osu_curr_obj.adjusted_delta_time, Some(2)),
                        300.0,
                        400.0,
                    )
                    * smootherstep(
                        osu_curr_obj.lazy_jump_dist,
                        f64::from(DIAMETER),
                        f64::from(DIAMETER * 2),
                    );
            }

            wide_angle_bonus = Self::calc_wide_angle_bonus(curr_angle);

            let (_win_mean, win_stddev, win_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            let variance_factor = if win_n >= 3 {
                (win_stddev / 1.2).clamp(0.0, 1.0)
            } else {
                1.0
            };

            let rep_strength = 1.0 - variance_factor;
            let wide_rep_nerf = rep_strength * 0.25;

            wide_angle_bonus *= 
                angle_bonus 
                * smootherstep(osu_curr_obj.lazy_jump_dist, 0.0, f64::from(DIAMETER))
                * (1.0 - wide_rep_nerf).max(0.0);
            
            wiggle_bonus = angle_bonus
                * smootherstep(
                    osu_curr_obj.lazy_jump_dist,
                    f64::from(RADIUS),
                    f64::from(DIAMETER),
                )
                * f64::powf(
                    reverse_lerp(
                        osu_curr_obj.lazy_jump_dist,
                        f64::from(DIAMETER * 3),
                        f64::from(DIAMETER),
                    ),
                    1.8,
                )
                * smootherstep(curr_angle, f64::to_radians(110.0), f64::to_radians(60.0))
                * smootherstep(
                    osu_last_obj.lazy_jump_dist,
                    f64::from(RADIUS),
                    f64::from(DIAMETER),
                )
                * f64::powf(
                    reverse_lerp(
                        osu_last_obj.lazy_jump_dist,
                        f64::from(DIAMETER * 3),
                        f64::from(DIAMETER),
                    ),
                    1.8,
                )
                * smootherstep(last_angle, f64::to_radians(110.0), f64::to_radians(60.0));

            if let Some(osu_last_2_obj) = curr.previous(2, diff_objects) {
                let distance =
                    (osu_last_2_obj.base.stacked_pos() - osu_last_obj.base.stacked_pos()).length();
                if distance < 1.0 {
                    wide_angle_bonus *= 1.0 - 0.35 * f64::from(1.0 - distance);
                }
            }
        }

        // ── Velocity change bonus ───────────────────────────────────
        if prev_vel.max(curr_vel).not_eq(0.0) {
            prev_vel = (osu_last_obj.lazy_jump_dist + osu_last_last_obj.travel_dist)
                / osu_last_obj.adjusted_delta_time;
            curr_vel = (osu_curr_obj.lazy_jump_dist + osu_last_obj.travel_dist)
                / osu_curr_obj.adjusted_delta_time;

            let dist_ratio = smoothstep(
                (prev_vel - curr_vel).abs() / prev_vel.max(curr_vel),
                0.0,
                1.0,
            );

            let overlap_vel_buff = (f64::from(DIAMETER) * 1.25
                / osu_curr_obj
                    .adjusted_delta_time
                    .min(osu_last_obj.adjusted_delta_time))
            .min((prev_vel - curr_vel).abs());

            vel_change_bonus = overlap_vel_buff * dist_ratio;

            let bonus_base = osu_curr_obj
                .adjusted_delta_time
                .min(osu_last_obj.adjusted_delta_time)
                / osu_curr_obj
                    .adjusted_delta_time
                    .max(osu_last_obj.adjusted_delta_time);
            vel_change_bonus *= bonus_base.powf(2.0);
        }

        // ── Slider bonus with slow-slider taper ─────────────────────
        if osu_last_obj.base.is_slider() {
            let travel_vel = osu_last_obj.travel_dist / osu_last_obj.travel_time;
            slider_bonus = travel_vel;

            if travel_vel < Self::SLOW_SLIDER_VEL_FLOOR {
                let ratio = (travel_vel / Self::SLOW_SLIDER_VEL_FLOOR).clamp(0.0, 1.0);
                slider_bonus *= 0.55 + 0.45 * ratio;
            }
        }

        // ── Combine ─────────────────────────────────────────────────
        aim_strain += wiggle_bonus * Self::WIGGLE_MULTIPLIER;
        aim_strain += vel_change_bonus * Self::VELOCITY_CHANGE_MULTIPLIER;

        aim_strain += (acute_angle_bonus * Self::ACUTE_ANGLE_MULTIPLIER)
            .max(wide_angle_bonus * Self::WIDE_ANGLE_MULTIPLIER);

        aim_strain *= osu_curr_obj.small_circle_bonus;

        if with_slider_travel_dist {
            aim_strain += slider_bonus * Self::SLIDER_MULTIPLIER;
        }

        // ═════════════════════════════════════════════════════════════
        // CC V3 RX-specific nerfs and boosts (post-combine)
        // ═════════════════════════════════════════════════════════════

        let eff_bpm = 30_000.0 / osu_curr_obj.adjusted_delta_time;
        let no_followpoint_streak =
            Self::recent_no_followpoint_streak(osu_curr_obj, diff_objects, 6);
        let is_flow_candidate = eff_bpm > Self::FLOW_MIN_EFF_BPM && no_followpoint_streak >= 6;
        let skip_farm_detection = no_followpoint_streak < 6;

        let mut cross_screen_nerf = 0.0;
        let mut flow_nerf = 0.0;
        let mut flow_active = false;

        // ── N/X alternating pattern severity ─────────────────────────────
        let nx_severity = if !skip_farm_detection {
            let nx_strength = detect_nx_pattern(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if nx_strength > 0.05 {
                let (dist_mean, dist_stddev, dist_n) =
                    windowed_dist_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

                let dist_cv = if dist_mean > 0.0 && dist_n >= 3 {
                    dist_stddev / dist_mean
                } else {
                    1.0
                };

                let dist_consistency = (1.0 - (dist_cv / 0.20).clamp(0.0, 1.0)).max(0.0);
                let bpm_fade = 1.0 - ((eff_bpm - 350.0) / 150.0).clamp(0.0, 1.0);

                nx_strength * dist_consistency * bpm_fade
            } else {
                0.0
            }
        } else {
            0.0
        };

        // ── Aim slop detection ──────────────────────────────────────
        let slop_severity = if !skip_farm_detection {
            let (_, angle_stddev, angle_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);
            let (dist_mean, dist_stddev, dist_n) =
                windowed_dist_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);
            let (vel_mean, vel_stddev, vel_n) =
                windowed_vel_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if angle_n >= 4 && dist_n >= 4 && vel_n >= 4 {
                let angle_uniformity = (1.0 - (angle_stddev / 0.3).clamp(0.0, 1.0)).max(0.0);
                let dist_cv = if dist_mean > 0.0 { dist_stddev / dist_mean } else { 1.0 };
                let dist_uniformity = (1.0 - (dist_cv / 0.15).clamp(0.0, 1.0)).max(0.0);
                let vel_cv = if vel_mean > 0.0 { vel_stddev / vel_mean } else { 1.0 };
                let vel_uniformity = (1.0 - (vel_cv / 0.15).clamp(0.0, 1.0)).max(0.0);

                let slop_severity = angle_uniformity * dist_uniformity * vel_uniformity;
                let bpm_fade = 1.0 - ((eff_bpm - 400.0) / 150.0).clamp(0.0, 1.0);

                slop_severity * bpm_fade
            } else {
                0.0
            }
        } else {
            0.0
        };

        // ── Cross-screen constant-distance nerf ─────────────────────
        if !flow_active && !skip_farm_detection && osu_curr_obj.adjusted_delta_time >= Self::CONSTANT_DIST_BPM_STRAIN_TIME {
            let curr_d = osu_curr_obj.lazy_jump_dist;
            let prev_d = osu_last_obj.lazy_jump_dist;
            let max_d = curr_d.max(prev_d);
            let min_d = curr_d.min(prev_d);

            if max_d > 80.0 {
                let change_ratio = if max_d > 0.0 {
                    (max_d - min_d) / max_d
                } else {
                    1.0
                };

                let is_edge_to_edge = max_d >= Self::EDGE_TO_EDGE_THRESHOLD;

                if !is_edge_to_edge && change_ratio < Self::CONSTANT_DIST_RATIO {
                    let dist_factor = 1.0
                        - ((max_d - 80.0) / (Self::EDGE_TO_EDGE_THRESHOLD - 80.0))
                            .clamp(0.0, 1.0);
                    let severity = (1.0 - (change_ratio / Self::CONSTANT_DIST_RATIO)) * dist_factor;
                    cross_screen_nerf = 0.15 * severity;
                }
            }
        }

        // ── Extreme flow aim nerf (distance-gated) ──────────────────
        if is_flow_candidate {
            let (flow_mean, flow_stddev, flow_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if flow_n >= 4 {
                let mean_ok = flow_mean >= Self::FLOW_MEAN_ANGLE_THRESHOLD;
                let stddev_ok = flow_stddev <= Self::FLOW_STDDEV_THRESHOLD;

                if mean_ok && stddev_ok {
                    let stddev_severity =
                        (1.0 - (flow_stddev / Self::FLOW_STDDEV_THRESHOLD)).powi(2);
                    let mean_range = PI - Self::FLOW_MEAN_ANGLE_THRESHOLD;
                    let mean_severity = ((flow_mean - Self::FLOW_MEAN_ANGLE_THRESHOLD)
                        / mean_range)
                        .clamp(0.0, 1.0);
                    let angle_severity = stddev_severity * mean_severity;

                    let (avg_dist, _, dist_n) =
                        windowed_dist_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

                    let dist_factor = if dist_n < 3 {
                        0.5
                    } else if avg_dist <= Self::FLOW_DIST_FULL_NERF {
                        1.0
                    } else if avg_dist >= Self::FLOW_DIST_EXEMPT {
                        0.0
                    } else {
                        1.0 - ((avg_dist - Self::FLOW_DIST_FULL_NERF)
                            / (Self::FLOW_DIST_EXEMPT - Self::FLOW_DIST_FULL_NERF))
                    };

                    let combined = angle_severity * dist_factor;
                    flow_nerf = Self::FLOW_MAX_NERF * combined;
                    if flow_nerf > 0.0 {
                        flow_active = true;
                    }
                }
            }
        }

        let mut tech_boost = 0.0;
        if !flow_active && !skip_farm_detection {
            let (_, angle_stddev, angle_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);
            let (vel_mean, vel_stddev, vel_n) =
                windowed_vel_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if angle_n >= 4 && vel_n >= 4 {
                let angle_variety = ((angle_stddev - 0.6) / 0.4).clamp(0.0, 1.0);
                let vel_cv = if vel_mean > 0.0 { vel_stddev / vel_mean } else { 0.0 };
                let vel_variety = ((vel_cv - 0.25) / 0.25).clamp(0.0, 1.0);
                let tech_signal = angle_variety * vel_variety;
                tech_boost = Self::TECH_MAX_BOOST * tech_signal;
            }
        }

        let farm_severity = Self::combine_farm_severity(nx_severity, slop_severity, cross_screen_nerf / 0.15);
        let farm_nerf = (Self::FARM_MAX_NERF * farm_severity).clamp(0.0, Self::FARM_MAX_NERF);
        let farm_nerf = farm_nerf * Self::relax_repeat_nerf_radius_factor(osu_curr_obj.circle_radius);

        let recent_farm = Self::recent_farm_streak(osu_curr_obj, diff_objects, 5);

        if flow_active {
            aim_strain *= 0.50 - flow_nerf;
        } else {
            aim_strain *= 0.75 - farm_nerf;

            // Delayed tech buff after farm + neutral pattern protection + overall cap
            let apply_tech = !(recent_farm >= 3 && farm_nerf > 0.12)
                && !Self::is_neutral_flow_pattern(osu_curr_obj, diff_objects)
                && !Self::is_stream_pattern(osu_curr_obj, diff_objects);

            if apply_tech {
                aim_strain *= (1.0 + tech_boost).min(Self::TECH_OVERALL_CAP);
            }
        }

        // ── Stream-density nerf ─────────────────────────────────────
        // Fast + tightly-spaced => farm stream under RX. Both conditions
        // required, so jump-spaced aim at the same BPM is unaffected.
        if osu_curr_obj.adjusted_delta_time > 0.0 {
            let eff_bpm = milliseconds_to_bpm(osu_curr_obj.adjusted_delta_time, None);
            let bpm_weight = reverse_lerp(eff_bpm, Self::STREAM_BPM_MIN, Self::STREAM_BPM_MAX);
            if bpm_weight > 0.0 {
                // 1.0 at/under STREAM_DIST_FULL, fading to 0.0 at STREAM_DIST_EXEMPT.
                let dist = osu_curr_obj.lazy_jump_dist;
                let dist_weight = 1.0
                    - reverse_lerp(dist, Self::STREAM_DIST_FULL, Self::STREAM_DIST_EXEMPT);
                let stream_severity = bpm_weight * dist_weight;
                aim_strain *= 0.50 - Self::STREAM_MAX_NERF * stream_severity;
            }
        }

        // ── Akat calibration ────────────────────────────────────────
        aim_strain *= Self::AKAT_CALIBRATION;

        aim_strain
    }

    const fn calc_wide_angle_bonus(angle: f64) -> f64 {
        smoothstep(angle, f64::to_radians(40.0), f64::to_radians(140.0))
    }

    const fn calc_acute_angle_bonus(angle: f64) -> f64 {
        smoothstep(angle, f64::to_radians(140.0), f64::to_radians(40.0))
    }
}
