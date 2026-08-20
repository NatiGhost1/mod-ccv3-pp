use crate::osu::performance::PERFORMANCE_BASE_MULTIPLIER;
use crate::osu::{
    attributes::OsuDifficultyAttributes,
    difficulty::{
        rating::OsuRatingCalculator,
        skills::{flashlight::Flashlight, strain},
    },
};

#[derive(Clone, Copy)]
pub struct MarathonDecayParams {
    pub tau: f64,       // tolerance in stars, e.g. 0.50
    pub b: f64,         // e.g. 0.02
    pub q: f64,         // e.g. 1.35
    pub double_at: u32, // minutes, e.g. 5
}

impl Default for MarathonDecayParams {
    fn default() -> Self {
        Self {
            tau: 1.5,
            b: 0.02,
            q: 1.35,
            double_at: 3,
        }
    }
}

pub fn decay_divisor(r: u32, p: MarathonDecayParams) -> f64 {
    let rf = r as f64;
    let base = 1.0 + p.b * rf.powf(p.q);
    let smooth_factor = (rf / p.double_at.max(1) as f64).powf(0.65).clamp(0.0, 1.0);

    // Avoid a hard jump at `double_at`; instead apply a smooth scaling for
    // longer repeated sections.
    base * (1.0 + 0.15 * smooth_factor)
}

const STAR_RATING_MULTIPLIER: f64 = 0.0265;
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
    if v.is_empty() {
        return 0.0;
    }

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

fn star_from_skills(
    aim_peaks: &[f64],
    speed_peaks: &[f64],
    flashlight_peaks: &[f64],
    memory_rating: f64,
) -> f64 {
    // difficulty values (weighted strain sums)
    let aim_dv = difficulty_value_from_peaks(aim_peaks);
    let speed_dv = difficulty_value_from_peaks(speed_peaks);

    // convert to "ratings"
    let aim_rating = OsuRatingCalculator::calculate_difficulty_rating(aim_dv);
    let speed_rating = OsuRatingCalculator::calculate_difficulty_rating(speed_dv);
    let flashlight_rating = OsuRatingCalculator::calculate_difficulty_rating(
        difficulty_value_from_peaks(flashlight_peaks),
    );

    // convert to "performance"
    let base_aim_perf = strain::difficulty_to_performance(aim_rating);
    let base_speed_perf = strain::difficulty_to_performance(speed_rating);
    let base_flashlight_perf = Flashlight::difficulty_to_performance(flashlight_rating);
    let base_memory_perf = strain::difficulty_to_performance(memory_rating);

    let base_perf = (base_aim_perf.powf(1.1)
        + base_speed_perf.powf(1.1)
        + base_flashlight_perf.powf(1.1)
        + base_memory_perf.powf(1.1))
    .powf(1.0 / 1.1);

    if base_perf <= 0.00001 {
        return 0.0;
    }

    // same star mapping used in difficulty::eval
    PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * STAR_RATING_MULTIPLIER
        * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) * base_perf).cbrt() + 4.0)
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

fn subsection_star_ratings(
    aim_peaks: &[f64],
    speed_peaks: &[f64],
    flashlight_peaks: &[f64],
    memory_rating: f64,
) -> Vec<f64> {
    let mut stars = Vec::with_capacity(SUBSECTIONS_PER_MINUTE);
    for (start, end) in partition_sections(aim_peaks.len(), SUBSECTIONS_PER_MINUTE) {
        stars.push(star_from_skills(
            &aim_peaks[start..end],
            &speed_peaks[start..end],
            &flashlight_peaks[start..end],
            memory_rating / SUBSECTIONS_PER_MINUTE as f64,
        ));
    }
    stars
}

fn star_from_subsection_stars(stars: &[f64]) -> f64 {
    const STAR_OFFSET: f64 = 4.0;
    let star_factor = PERFORMANCE_BASE_MULTIPLIER.cbrt() * STAR_RATING_MULTIPLIER;
    let performance_factor = 100_000.0 / 2.0_f64.powf(1.0 / 1.1);
    let performance = stars
        .iter()
        .map(|star| {
            let normalized = (*star / star_factor - STAR_OFFSET).max(0.0);
            normalized.powi(3) / performance_factor
        })
        .fold(0.0, |total, value| total + value.powf(1.1))
        .powf(1.0 / 1.1);

    if performance <= 0.00001 {
        return 0.0;
    }

    PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * STAR_RATING_MULTIPLIER
        * ((performance_factor * performance).cbrt() + STAR_OFFSET)
}

pub fn local_sr_per_minute(
    strains_aim: &[f64],
    strains_speed: &[f64],
    strains_flashlight: &[f64],
    memory_rating: f64,
) -> Vec<f64> {
    let peaks_per_min = (MINUTE_MS / PEAK_SECTION_LEN_MS).round() as usize; // 150
    let n_minutes = strains_aim.len().div_ceil(peaks_per_min);
    let mut out = Vec::with_capacity(n_minutes);
    for k in 0..n_minutes {
        let start = k * peaks_per_min;
        let end = ((k + 1) * peaks_per_min).min(strains_aim.len());
        let aim_slice = &strains_aim[start..end];
        let speed_slice = &strains_speed[start..end];

        let flashlight_slice = &strains_flashlight[start..end];
        let subsection_stars = subsection_star_ratings(
            aim_slice,
            speed_slice,
            flashlight_slice,
            memory_rating / n_minutes as f64,
        );
        out.push(star_from_subsection_stars(&subsection_stars));
    }

    out
}

pub fn weighted_star_rating(attrs: &OsuDifficultyAttributes) -> f64 {
    let sections = &attrs.local_sr_per_minute;
    if sections.is_empty() {
        return attrs.stars;
    }

    let average = sections.iter().sum::<f64>() / sections.len() as f64;
    let variance = sections
        .iter()
        .map(|stars| (stars - average).powi(2))
        .sum::<f64>()
        / sections.len() as f64;
    let variation = (variance.sqrt() / average.max(0.1)).clamp(0.0, 1.0);
    let hard_section_ratio =
        sections.iter().filter(|stars| **stars > average).count() as f64 / sections.len() as f64;
    let repetition = if sections.len() > 1 {
        sections
            .windows(2)
            .map(|pair| (-(pair[1] - pair[0]).abs() / (average.max(0.1) * 0.35)).exp())
            .sum::<f64>()
            / (sections.len() - 1) as f64
    } else {
        0.0
    };

    average * (1.0 + 0.12 * variation + 0.08 * hard_section_ratio) * (1.0 - 0.20 * repetition)
}

pub fn relax_marathon_multiplier(
    local_sr: &[f64],
    local_bpm: &[f64],
    params: MarathonDecayParams,
) -> f64 {
    if local_sr.len() < 2 || local_sr.len() != local_bpm.len() {
        return 1.0;
    }

    let mut r: u32 = 0;
    let mut weighted = 0.0;
    let mut total = 0.0;

    for (k, &sr) in local_sr.iter().enumerate() {
        if k > 0 && (sr - local_sr[k - 1]).abs() <= params.tau {
            r += 1;
        } else {
            r = 0;
        }

        let mut lambda = 1.0 / decay_divisor(r, params);

        // High-BPM 1/2 sections like 410 BPM are hard on relax even if they
        // look marathon-like. So soften the relax marathon nerf as BPM climbs.
        let bpm = local_bpm[k];
        if bpm >= 400.0 {
            let soften = ((bpm - 400.0) / 40.0).clamp(0.0, 1.0) * 0.15;
            lambda += (1.0 - lambda) * soften;
        }

        // Weight by SR so "dead minutes" don't dominate.
        weighted += sr * lambda;
        total += sr;
    }

    if total > 0.0 {
        (weighted / total).clamp(0.0, 1.0)
    } else {
        1.0
    }
}
