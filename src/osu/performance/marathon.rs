use crate::osu::difficulty::skills::strain;
use crate::osu::performance::PERFORMANCE_BASE_MULTIPLIER;

#[derive(Clone, Copy)]
pub struct VanillaMarathonParams {
    pub min_combo_for_marathon: u32,
    pub min_sr_for_consideration: f64,
    pub balance_tolerance: f64,
    pub min_sustained_minutes: usize,
    pub max_combo_cap: u32,
    pub max_buff: f64,
}

impl Default for VanillaMarathonParams {
    fn default() -> Self {
        Self {
            min_combo_for_marathon: 1375,
            min_sr_for_consideration: 4.4,
            balance_tolerance: 0.62,
            min_sustained_minutes: 3,
            max_combo_cap: 8000,
            max_buff: 1.10,
        }
    }
}

#[allow(dead_code)]
fn difficulty_value_from_peaks(peaks: &[f64]) -> f64 {
    let mut v: Vec<f64> = peaks.iter().copied().filter(|x| *x > 0.0).collect();
    if v.is_empty() {
        return 0.0;
    }

    v.sort_by(|a, b| b.partial_cmp(a).unwrap());

    let reduced_section_count = 10usize;
    let reduced_baseline = 0.75;
    let decay_weight = 0.9;

    let take = reduced_section_count.min(v.len());
    for i in 0..take {
        let t = i as f64 / reduced_section_count as f64;
        let scale = (1.0 - (1.0 - reduced_baseline) * t).log10().abs().min(1.0);
        v[i] *= reduced_baseline + (1.0 - reduced_baseline) * scale;
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

#[allow(dead_code)]
fn star_from_peaks(peaks: &[f64]) -> f64 {
    let value = difficulty_value_from_peaks(peaks);
    let rating = value.sqrt() * 0.0675;
    let perf = strain::difficulty_to_performance(rating);

    if perf <= 0.00001 {
        return 0.0;
    }

    PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * 0.027
        * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) * perf).cbrt() + 4.0)
}

#[allow(dead_code)]
pub fn local_sr_per_minute(strains: &[f64]) -> Vec<f64> {
    const PEAK_SECTION_LEN_MS: f64 = 400.0;
    const MINUTE_MS: f64 = 60_000.0;
    let peaks_per_min = (MINUTE_MS / PEAK_SECTION_LEN_MS).round() as usize;

    let n_minutes = strains.len().div_ceil(peaks_per_min);
    let mut out = Vec::with_capacity(n_minutes);

    for k in 0..n_minutes {
        let start = k * peaks_per_min;
        let end = ((k + 1) * peaks_per_min).min(strains.len());
        let slice = &strains[start..end];
        out.push(star_from_peaks(slice));
    }
    out
}

pub fn vanilla_marathon_multiplier(
    local_aim_sr: &[f64],
    local_speed_sr: &[f64],
    map_max_combo: u32,
    player_max_combo: u32,
    acc: f64,
    params: VanillaMarathonParams,
) -> f64 {
    if local_aim_sr.len() < params.min_sustained_minutes 
        || local_aim_sr.len() != local_speed_sr.len() 
        || map_max_combo < params.min_combo_for_marathon 
    {
        return 1.0;
    }

    let mut sustained_good_minutes = 0usize;

    for (aim_sr, speed_sr) in local_aim_sr.iter().zip(local_speed_sr.iter()) {
        if *aim_sr < params.min_sr_for_consideration || *speed_sr < params.min_sr_for_consideration {
            continue;
        }

        let ratio = if *aim_sr > *speed_sr {
            *speed_sr / *aim_sr
        } else {
            *aim_sr / *speed_sr
        };

        if ratio >= params.balance_tolerance {
            sustained_good_minutes += 1;
        }
    }

    if sustained_good_minutes < params.min_sustained_minutes {
        return 1.0;
    }

    // Base buff from sustained balanced high-strain sections
    let base_buff = 1.0 + 0.027 * (sustained_good_minutes as f64 - 2.0).max(0.0).sqrt();

    // Combo scaling
    let effective_combo = (player_max_combo as f64).min(params.max_combo_cap as f64);
    let combo_ratio = (effective_combo / map_max_combo as f64).clamp(0.0, 1.0);

    let length_factor = if map_max_combo > 5000 {
        1.12 + 0.08 * ((map_max_combo as f64 - 5000.0) / 10000.0).clamp(0.0, 1.0)
    } else {
        (map_max_combo as f64 / params.min_combo_for_marathon as f64).clamp(0.75, 1.1)
    };

    let mut mult = base_buff * combo_ratio * length_factor;

    // Accuracy scaling
    let raw_acc_factor = if acc >= 0.985 {
        1.0
    } else {
        (-14.5 * (0.985 - acc)).exp().clamp(0.34, 1.0)
    };

    let combo_exponent = 2.8 * (3500.0 / effective_combo.max(800.0)).clamp(0.65, 2.8);
    let acc_factor = raw_acc_factor.powf(combo_exponent);

    mult *= acc_factor;

    // Final clamps: never nerf below 1.0, hard cap on buff
    mult.clamp(1.0, params.max_buff)
}