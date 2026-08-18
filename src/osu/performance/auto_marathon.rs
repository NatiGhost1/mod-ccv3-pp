use crate::osu::difficulty::skills::strain;
use crate::osu::performance::PERFORMANCE_BASE_MULTIPLIER;

#[derive(Clone, Copy)]
pub struct AutopilotDecayParams {
    #[allow(dead_code)]
    pub tau: f64, // legacy SR tolerance; kept for compatibility but not used by the current AP decay logic.
    pub bpm_tau: f64, // BPM tolerance for grouping nearby minutes into a single streak.
    pub b: f64,   // base decay scaling factor.
    pub q: f64,   // exponent for streak length decay.
    #[allow(dead_code)]
    pub double_at: u32, // streak length in minutes after which decay is slightly larger.
}

impl Default for AutopilotDecayParams {
    fn default() -> Self {
        Self {
            tau: 4.0, // tau is 4 stars, so that almost all minutes are affected (legacy value was 0.50 but that may be too little for accurate nerfs on autopilot)
            bpm_tau: 5.0,
            b: 0.05,
            q: 1.35,
            double_at: 3,
        }
    }
}

pub fn decay_divisor(r: u32, average_bpm: f64, p: AutopilotDecayParams) -> f64 {
    // Exponential BPM scaling: higher BPM reduces the nerf factor
    let base_bpm_factor = ((360.0 - average_bpm) * 0.01).exp().clamp(0.3, 4.0);

    // For high BPM (>365), nerf decreases exponentially with streak length
    let streak_factor = if average_bpm > 365.0 {
        (-(r as f64) * 0.08).exp()  // More aggressive exponential decay
    } else {
        1.0
    };

    let bpm_factor = base_bpm_factor * streak_factor;

    let base = 1.0 + p.b * (r as f64).powf(p.q) * bpm_factor;
    base
}

const DIFFICULTY_MULTIPLIER: f64 = 0.0675;
const PEAK_SECTION_LEN_MS: f64 = 400.0;
const MINUTE_MS: f64 = 60_000.0;
const SUBSECTION_MS: f64 = 15_000.0;
const SUBSECTIONS_PER_MINUTE: usize = 4;

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

// Replicates OsuStrainSkill::difficulty_value logic but on a slice of peaks.
fn difficulty_value_from_peaks(peaks: &[f64]) -> f64 {
    let mut v: Vec<f64> = peaks.iter().copied().filter(|x| *x > 0.0).collect();
    if v.is_empty() { return 0.0; }
    
    v.sort_by(|a, b| b.partial_cmp(a).unwrap());

    // same intent as reduced top peaks (see OsuStrainSkill constants)
    let reduced_section_count = 10usize;
    let reduced_baseline = 0.75;
    let decay_weight = 0.9;

    let take = reduced_section_count.min(v.len());
    for i in 0..take {
        // 1. Calc porgress thru top sections
        let t = i as f64 / reduced_section_count as f64;
        // 2. Apply the logarithmic reduction (OsuStrainSkill style)
        let scale = lerp(1.0, reduced_baseline, t).log10().abs();
        v[i] *= lerp(reduced_baseline, 1.0, scale.min(1.0));
    }

    v.sort_by(|a, b| b.partial_cmp(a).unwrap());

    let mut difficulty = 0.0;
    let mut w = 1.0;
    for s in v {
        difficulty += s * w;
        w *= decay_weight;
    }
    difficulty
}

fn star_from_peaks(peaks: &[f64]) -> f64 {
    // Convert raw per-minute strain peaks into a star-like rating.
    // This is used for autopilot so the decay is driven by speed/rhythm only.
    let dv = difficulty_value_from_peaks(peaks);
    let rating = dv.sqrt() * DIFFICULTY_MULTIPLIER;
    let perf = strain::difficulty_to_performance(rating);

    if perf <= 0.00001 {
        return 0.0;
    }

    // same star mapping used in difficulty::eval
    PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * 0.027
        * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) * perf).cbrt() + 4.0)
}

fn partition_sections(len: usize, parts: usize) -> Vec<(usize, usize)> {
    if parts == 0 || len == 0 {
        return Vec::new();
    }

    let base = len / parts;
    let remainder = len % parts;
    let mut sections = Vec::with_capacity(parts);
    let mut offset = 0;

    for part in 0..parts {
        let size = base + usize::from(part < remainder);
        sections.push((offset, offset + size));
        offset += size;
    }

    sections
}

fn subsection_star_ratings(peaks: &[f64]) -> Vec<f64> {
    let mut stars = Vec::with_capacity(SUBSECTIONS_PER_MINUTE);
    for (start, end) in partition_sections(peaks.len(), SUBSECTIONS_PER_MINUTE) {
        stars.push(star_from_peaks(&peaks[start..end]));
    }
    stars
}

pub fn compute_local_bpm_per_minute(
    diff_objects: &[crate::osu::difficulty::object::OsuDifficultyObject],
    delta_times: &[f64],
) -> Vec<f64> {
    let total_time = diff_objects.last().map(|obj| obj.base.start_time as f64).unwrap_or(0.0);
    let n_minutes = ((total_time / MINUTE_MS).ceil() as usize).max(1);

    let mut out = Vec::with_capacity(n_minutes);
    for k in 0..n_minutes {
        let start_time = k as f64 * MINUTE_MS;
        let end_time = (k + 1) as f64 * MINUTE_MS;
        let mut sum_delta = 0.0;
        let mut count = 0;

        for (i, obj) in diff_objects.iter().enumerate() {
            let t = obj.base.start_time as f64;
            if t >= start_time && t < end_time {
                sum_delta += delta_times[i];
                count += 1;
            }
        }

        if count > 0 {
            let avg_delta = sum_delta / count as f64;
            let bpm = 30_000.0 / avg_delta;
            out.push(bpm);
        } else {
            out.push(0.0);
        }
    }

    out
}

pub fn local_sr_per_minute(strains_speed: &[f64]) -> Vec<f64> {
    let peaks_per_min = (MINUTE_MS / PEAK_SECTION_LEN_MS).round() as usize; // 150
    let n_minutes = strains_speed.len().div_ceil(peaks_per_min);

    let mut out = Vec::with_capacity(n_minutes);
    for k in 0..n_minutes {
        let start = k * peaks_per_min;
        let end = ((k + 1) * peaks_per_min).min(strains_speed.len());
        let speed_slice = &strains_speed[start..end];

        let subsection_stars = subsection_star_ratings(speed_slice);
        out.push(star_from_peaks(&subsection_stars));
    }

    out
}

pub fn local_aim_per_minute(strains_aim: &[f64]) -> Vec<f64> {
    let peaks_per_min = (MINUTE_MS / PEAK_SECTION_LEN_MS).round() as usize; // 150
    let n_minutes = strains_aim.len().div_ceil(peaks_per_min);

    let mut out = Vec::with_capacity(n_minutes);
    for k in 0..n_minutes {
        let start = k * peaks_per_min;
        let end = ((k + 1) * peaks_per_min).min(strains_aim.len());
        let aim_slice = &strains_aim[start..end];

        let subsection_stars = subsection_star_ratings(aim_slice);
        out.push(star_from_peaks(&subsection_stars));
    }

    out
}

pub fn autopilot_marathon_multiplier(
    local_sr: &[f64],
    local_bpm: &[f64],
    local_aim: &[f64],
    params: AutopilotDecayParams,
) -> f64 {
    if local_sr.len() < 2 || local_sr.len() != local_bpm.len() || local_sr.len() != local_aim.len() {
        return 1.0;
    }

    let mut r: u32 = 0;
    let mut weighted = 0.0;
    let mut total = 0.0;
    let mut sum_bpm = 0.0;
    let mut count = 0;

    let mut low_bpm_aim_streak: usize = 0;
    let mut balanced_streak: usize = 0;
    let mut best_bonus: f64 = 1.0;
    let mut previous_sr = local_sr[0];

    for (k, (&sr, &bpm)) in local_sr.iter().zip(local_bpm.iter()).enumerate() {
        if k > 0 && (bpm - local_bpm[k - 1]).abs() <= params.bpm_tau {
            r += 1;
            sum_bpm += bpm;
            count += 1;
        } else {
            r = 0;
            sum_bpm = bpm;
            count = 1;
        }

        let average_bpm = sum_bpm / count as f64;
        let mut lambda = 1.0 / decay_divisor(r, average_bpm, params);

        // If a section is low BPM but shows high aim intensity, since AP is only tapping, 
        // we can be more confident it's a relax-style section and less autopilot-like, 
        // so apply an extra decay factor. The extra decay scales up with the aim ratio and with 
        // consecutive low-BPM aim-heavy minutes, capped at 25% total extra decay. 
        // This helps preserve more pp on maps with some relax-style sections, 
        // while still applying a strong marathon nerf to maps that are consistently low BPM and aim-heavy.
        let aim = local_aim[k];
        let aim_ratio = if sr > 0.0 { aim / sr } else { 0.0 };
        if average_bpm <= 360.0 && aim_ratio > 0.7 {
            low_bpm_aim_streak += 1;
            let streak_t = (low_bpm_aim_streak.min(3) as f64) / 3.0;
            let extra_decay = 0.10 + 0.15 * streak_t; // 0.10 -> 0.25
            lambda *= 1.0 - extra_decay.clamp(0.0, 0.30);
        } else {
            low_bpm_aim_streak = 0;
        }

        let balanced = sr >= 3.0
            && local_aim[k] >= 3.0
            && (aim_ratio >= 0.7 && aim_ratio <= 1.3)
            && (k == 0 || sr >= previous_sr * 0.75);

        if balanced {
            balanced_streak += 1;
            let bonus = 1.0 + 0.01 * (balanced_streak.saturating_sub(2) as f64);
            best_bonus = best_bonus.max(bonus.min(1.08));
        } else {
            balanced_streak = 0;
        }

        previous_sr = sr;

        // Weight by SR so "dead minutes" don't dominate.
        weighted += sr * lambda;
        total += sr;
    }

    let mut mult = if total > 0.0 {
        (weighted / total).clamp(0.0, 1.0)
    } else {
        1.0
    };

    if best_bonus > 1.0 {
        mult = 1.0 - (1.0 - mult) / best_bonus;
    }

    if local_bpm.len() > 2 {
        let length_softener = ((local_bpm.len() as f64 - 2.0) / 8.0).clamp(0.0, 1.0);
        let nerf_reduction = 0.10 * length_softener;
        mult = 1.0 - (1.0 - mult) * (1.0 - nerf_reduction);
    }

    mult
}
