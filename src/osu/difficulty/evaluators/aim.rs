use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::{
        difficulty::{milliseconds_to_bpm, reverse_lerp, smootherstep, smoothstep},
        float_ext::FloatExt,
    },
};

pub struct AimEvaluator;

// ─── Windowed statistics helpers (shared with aim_rx) ───────────────

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

/// Detect N/X alternating angle patterns.
fn detect_nx_pattern<'a>(
    curr: &'a OsuDifficultyObject<'a>,
    diff_objects: &'a [OsuDifficultyObject<'a>],
    window: usize,
) -> f64 {
    let mut angles: Vec<f64> = Vec::with_capacity(window + 1);
    if let Some(a) = curr.angle { angles.push(a); }
    for back in 0..window {
        if let Some(prev) = curr.previous(back, diff_objects) {
            if let Some(a) = prev.angle { angles.push(a); }
        } else { break; }
    }
    if angles.len() < 4 { return 0.0; }

    let evens: Vec<f64> = angles.iter().step_by(2).copied().collect();
    let odds: Vec<f64> = angles.iter().skip(1).step_by(2).copied().collect();
    if evens.len() < 2 || odds.len() < 2 { return 0.0; }

    let even_mean = evens.iter().sum::<f64>() / evens.len() as f64;
    let odd_mean = odds.iter().sum::<f64>() / odds.len() as f64;
    let even_stddev = (evens.iter().map(|a| (a - even_mean).powi(2)).sum::<f64>() / evens.len() as f64).sqrt();
    let odd_stddev = (odds.iter().map(|a| (a - odd_mean).powi(2)).sum::<f64>() / odds.len() as f64).sqrt();

    let cluster_tight = even_stddev < 0.25 && odd_stddev < 0.25;
    let clusters_differ = (even_mean - odd_mean).abs() > 0.3;

    if cluster_tight && clusters_differ {
        let tightness = 1.0 - ((even_stddev + odd_stddev) / 0.50).clamp(0.0, 1.0);
        let separation = ((even_mean - odd_mean).abs() / std::f64::consts::PI).clamp(0.0, 1.0);
        tightness * separation
    } else {
        0.0
    }
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

impl AimEvaluator {
    const WIDE_ANGLE_MULTIPLIER: f64 = 1.0; // Wide angles in my opinion are more difficult than acute angles with aim assist, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.
    const ACUTE_ANGLE_MULTIPLIER: f64 = 0.85; // Acute angles in my opinion are less difficult than wide angles with aim assist, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.
    const SLIDER_MULTIPLIER: f64 = 0.70; // Reduced slider bonus to avoid over-boosting aim pp on maps with many complex fast sliders.
    const VELOCITY_CHANGE_MULTIPLIER: f64 = 0.80; // Velocity change bonus is slightly increased to reward technical aim that requires quick adjustments in speed.
    const WIGGLE_MULTIPLIER: f64 = 0.81; // Wiggly patterns are generally less difficult in a cheat environment than straight aim patterns, so I reduced the multiplier to reflect that. This is a subjective adjustment based on my experience and understanding of aim difficulty.

    // VN aim evaluation still uses the legacy ccv3 evaluation structure,
    // whereas aim_rx uses a more modernized structure.
    // This is likely what leads to the discrepancy in aim pp between vanilla and relax, 
    // so just adjusting base aim strain and bonus multipliers won't work bc of the less advanced evaluation structure.
    // Akat calibration is being used to bring vanilla aim pp closer to what it should be worth with aim assist. 
    // This is a subjective adjustment based on my experience and understanding of aim difficulty.
    const AKAT_CALIBRATION: f64 = 0.60;

    // N/X pattern nerf
    const NX_MAX_NERF_VANILLA: f64 = 0.22;

    // Aim slop: max nerf for constant-everything patterns.
    const SLOP_MAX_NERF: f64 = 0.22;

    #[expect(clippy::too_many_lines, reason = "staying in-sync with lazer")]
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

        #[expect(clippy::items_after_statements, reason = "staying in-sync with lazer")]
        const RADIUS: i32 = OsuDifficultyObject::NORMALIZED_RADIUS;
        #[expect(clippy::items_after_statements, reason = "staying in-sync with lazer")]
        const DIAMETER: i32 = OsuDifficultyObject::NORMALIZED_DIAMETER;

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

            // ── CC V3: windowed variance repetition ─────────────────
            // Replaces the pairwise check with a proper 8-note window
            // standard deviation measure. BPM-aware: penalty fades at
            // 410+ BPM and flips to a buff at 500+ BPM.
            let eff_bpm = 30_000.0 / osu_curr_obj.adjusted_delta_time;
            let high_bpm_t = ((eff_bpm - 410.0) / 90.0).clamp(0.0, 1.0);

            let (_win_mean, win_stddev, win_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            let variance_factor = if win_n >= 3 {
                (win_stddev / 1.2).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let rep_strength = 1.0 - variance_factor;

            let wide_rep_raw = wide_angle_bonus
                .min(Self::calc_wide_angle_bonus(last_angle).powf(3.0));
            let wide_penalty = (rep_strength * 0.7 + wide_rep_raw * 0.3) * (1.0 - high_bpm_t);
            let wide_rep_buff = high_bpm_t * 0.15;
            wide_angle_bonus *= angle_bonus
                * smootherstep(osu_curr_obj.lazy_jump_dist, 0.0, f64::from(DIAMETER))
                * ((1.0 - wide_penalty + wide_rep_buff).max(0.0));

            let acute_rep_raw = acute_angle_bonus
                .min(Self::calc_acute_angle_bonus(last_angle).powf(3.0));
            let acute_penalty = (rep_strength * 0.5 + acute_rep_raw * 0.5) * (1.0 - high_bpm_t);
            let acute_rep_buff = high_bpm_t * 0.10;
            acute_angle_bonus *= (0.5
                + 0.5 * (1.0 - acute_penalty)
                + acute_rep_buff)
                .max(0.0);

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

            let bonus_base = (osu_curr_obj.adjusted_delta_time)
                .min(osu_last_obj.adjusted_delta_time)
                / (osu_curr_obj.adjusted_delta_time).max(osu_last_obj.adjusted_delta_time);
            vel_change_bonus *= bonus_base.powf(2.0);
        }

        if osu_last_obj.base.is_slider() {
            slider_bonus = osu_last_obj.travel_dist / osu_last_obj.travel_time;
        }

        aim_strain += wiggle_bonus * Self::WIGGLE_MULTIPLIER;
        aim_strain += vel_change_bonus * Self::VELOCITY_CHANGE_MULTIPLIER;

        aim_strain += (acute_angle_bonus * Self::ACUTE_ANGLE_MULTIPLIER)
            .max(wide_angle_bonus * Self::WIDE_ANGLE_MULTIPLIER);

        aim_strain *= osu_curr_obj.small_circle_bonus;

        if with_slider_travel_dist {
            aim_strain += slider_bonus * Self::SLIDER_MULTIPLIER;
        }

        // ── CC V3: N/X pattern nerf (vanilla — lighter than RX) ─────
        // N/X patterns on vanilla require tapping + aiming simultaneously,
        // so they're genuinely harder than on RX. Only a mild nerf for
        // extreme repetition at low BPM.
        let eff_bpm = 30_000.0 / osu_curr_obj.adjusted_delta_time;
        {
            let nx_strength = detect_nx_pattern(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if nx_strength > 0.1 {
                let (dist_mean, dist_stddev, dist_n) =
                    windowed_dist_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

                let dist_cv = if dist_mean > 0.0 && dist_n >= 3 {
                    dist_stddev / dist_mean
                } else {
                    1.0
                };

                let dist_consistency = (1.0 - (dist_cv / 0.20).clamp(0.0, 1.0)).max(0.0);
                let bpm_fade = 1.0 - ((eff_bpm - 300.0) / 200.0).clamp(0.0, 1.0);

                let nx_severity = nx_strength * dist_consistency * bpm_fade;
                aim_strain *= 0.87 - Self::NX_MAX_NERF_VANILLA * nx_severity; // reducing base aim strain by 13% and then applying the NX nerf on top of that as an attempt to get aim pp close to what it should be worth with aim assist. This is a subjective adjustment based on my experience and understanding of aim difficulty.
            }
        }

        // ── Aim slop detection ──────────────────────────────────────
        // Constant velocity + constant distance + low angle variance
        // = mechanical slop. The cursor follows a predictable path.
        // This catches patterns that N/X detection misses (e.g. linear
        // back-and-forth, square patterns, constant-speed circles).
        let mut slop_nerf = 0.0;
        {
            let (_, angle_stddev, angle_n) =
                windowed_angle_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);
            let (dist_mean, dist_stddev, dist_n) =
                windowed_dist_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);
            let (vel_mean, vel_stddev, vel_n) =
                windowed_vel_stats(osu_curr_obj, diff_objects, ANGLE_WINDOW);

            if angle_n >= 4 && dist_n >= 4 && vel_n >= 4 {
                // Angle uniformity: stddev < 0.3 = highly uniform
                let angle_uniformity = (1.0 - (angle_stddev / 0.3).clamp(0.0, 1.0)).max(0.0);

                // Distance uniformity: CV < 0.15 = very constant spacing
                let dist_cv = if dist_mean > 0.0 { dist_stddev / dist_mean } else { 1.0 };
                let dist_uniformity = (1.0 - (dist_cv / 0.15).clamp(0.0, 1.0)).max(0.0);

                // Velocity uniformity: CV < 0.15 = constant speed
                let vel_cv = if vel_mean > 0.0 { vel_stddev / vel_mean } else { 1.0 };
                let vel_uniformity = (1.0 - (vel_cv / 0.15).clamp(0.0, 1.0)).max(0.0);

                // All three must be uniform for the nerf to fire
                let slop_severity = angle_uniformity * dist_uniformity * vel_uniformity;

                // BPM fade: less severe at high BPM
                let bpm_fade = 1.0 - ((eff_bpm - 400.0) / 150.0).clamp(0.0, 1.0);

                slop_nerf = Self::SLOP_MAX_NERF * slop_severity * bpm_fade;
            }
        }

        aim_strain *= 0.87 - slop_nerf; // reducing base aim strain by 13% and then applying the slop nerf on top of that as an attempt to get aim pp close to what it should be worth with aim assist. This is a subjective adjustment based on my experience and understanding of aim difficulty.

        // ── CC V3: akat calibration ─────────────────────────────────
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
