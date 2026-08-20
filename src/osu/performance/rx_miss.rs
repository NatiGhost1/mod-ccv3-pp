// CC V3 (Relax): strain-peak based miss model (v3).
//
// Complete rework of the relax miss handling. The previous system only
// applied a small secondary multiplier; the dominant relax miss penalty was
// still the generic `calculate_miss_penalty(literal_misses, ...)`. v3 instead
// produces a STRAIN-WEIGHTED effective miss count that REPLACES the literal
// miss count fed into that main penalty for relax scores, so the strain model
// is the primary lever.
//
// `rx_strain_weighted_misses` works off the per-chunk strain peaks
// (`rx_chunk_hardness`, a map-ordered array; each entry = summed 1/delta
// hardness of a 4-note chunk). Misses are assigned to the hardest remaining
// chunks (worst-case, the standard pp assumption) and each is costed by which
// strain tier its chunk falls in:
//
//   * HIGH peaks (strain >= HIGH_FRAC * max): the FIRST 2 misses here are
//     softened (HIGH_FIRST2_MULT) — missing the single hardest spike once or
//     twice is expected even of strong players, so it should bleed less pp.
//     The 3rd+ high-peak miss is full value.
//   * MID peaks (LOW_FRAC..HIGH_FRAC): full, normal value (MID_MULT = 1.0) —
//     the calibration anchor.
//   * LOW peaks (< LOW_FRAC * max): softened (LOW_UNLUCKY_MULT) ONLY when the
//     miss looks unlucky rather than symptomatic — a single isolated miss
//     (total misses <= LOW_UNLUCKY_MAX_MISSES) or an accuracy that is NOT
//     dropping fast. If acc IS dropping fast AND there are several misses, that
//     signals a genuinely inconsistent player (not bad luck) → full value.
//
// n100 / n50 inflation
// --------------------
// Oks and Mehs inflate the effective miss count, each worth a fraction of a
// real miss (N100_PER, N50_PER). The count that may contribute is hard-capped
// (N100_CAP = 700000, N50_CAP = 700000); on top of that a variance-aware soft stop
// tapers the contribution once the 50:100 ratio climbs past VARI_RATIO (the
// mix stops looking like "a few slipped edges" and starts looking like a
// uniformly low-acc play). See `count_inflation`.

const HIGH_FRAC: f64 = 0.70;
const LOW_FRAC: f64 = 0.30;

const HIGH_FIRST2_MULT: f64 = 0.45;
const MID_MULT: f64 = 1.00;
const LOW_UNLUCKY_MULT: f64 = 0.40;

const LOW_UNLUCKY_MAX_MISSES: u32 = 1;
const LOW_ACC_OK: f64 = 0.99; // .99 instead of .97

const N100_PER: f64 = 1.0 / 4.0; // 1.0 / 6.0 but harsher bc aim assist
const N50_PER: f64 = 1.0 / 2.0; // in legit ccv3 this is 1.0 / 3.0 but its harsher here bc aim assist
const N100_CAP: u32 = 700000; // basically no cap
const N50_CAP: u32 = 700000; // basically no cap
const VARI_RATIO: f64 = 0.50;

/// Strain-weighted effective miss count for a relax score. This REPLACES the
/// literal miss count that feeds the main aim miss-penalty. Always >= 0.
/// On a clean FC with no Oks/Mehs this returns 0.0.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rx_strain_weighted_misses(
    chunk_hardness: &[f64],
    n300: u32,
    n100: u32,
    n50: u32,
    misses: u32,
) -> f64 {
    let count_infl = count_inflation(n100, n50);

    if misses == 0 {
        return count_infl;
    }

    let chunks_n = chunk_hardness.len();
    let max_strain = chunk_hardness.iter().copied().fold(0.0_f64, f64::max);
    if chunks_n == 0 || max_strain <= 0.0 {
        // No strain info: fall back to literal misses (+ count inflation).
        return f64::from(misses) + count_infl;
    }

    // Accuracy over judged objects and whether it is dropping fast.
    let judged = f64::from(n300 + n100 + n50 + misses).max(1.0);
    let acc = f64::from(n300 * 300 + n100 * 100 + n50 * 50) / (judged * 300.0);
    let non300_rate = f64::from(n100 + n50 + misses) / judged;
    let acc_dropping_fast = non300_rate > 0.10 || acc < LOW_ACC_OK;

    // Hardest-first chunk order (worst-case miss placement).
    let mut order: Vec<usize> = (0..chunks_n).collect();
    order.sort_by(|&a, &b| {
        chunk_hardness[b]
            .partial_cmp(&chunk_hardness[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut high_seen = 0u32;
    let mut weighted = 0.0_f64;
    for k in 0..misses {
        let idx = order[(k as usize).min(chunks_n - 1)];
        let rel = chunk_hardness[idx] / max_strain;
        let mult = if rel >= HIGH_FRAC {
            high_seen += 1;
            if high_seen <= 2 {
                HIGH_FIRST2_MULT
            } else {
                MID_MULT
            }
        } else if rel >= LOW_FRAC {
            MID_MULT
        } else {
            let unlucky = misses <= LOW_UNLUCKY_MAX_MISSES || !acc_dropping_fast;
            if unlucky {
                LOW_UNLUCKY_MULT
            } else {
                MID_MULT
            }
        };
        // Apply a small late-map boost so misses near the end still matter.
        // Boost scales from 0 at 70% into the map up to `late_multiplier`
        // at the very end. This helps avoid misses near the map end
        // being effectively ignored.
        let late_multiplier: f64 = 0.35; // tuning: max +35% penalty at map end
        let pos = if chunks_n > 1 {
            idx as f64 / (chunks_n as f64 - 1.0)
        } else {
            0.0
        };
        let late_boost = ((pos - 0.70) / 0.30).clamp(0.0, 1.0);
        let mult = mult * (1.0 + late_boost * late_multiplier);

        weighted += mult;
    }

    weighted + count_infl
}

/// n100/n50 -> effective-miss inflation with hard caps and a variance-aware
/// soft stop. Returns an additive effective-miss contribution (>= 0).
fn count_inflation(n100: u32, n50: u32) -> f64 {
    if n100 == 0 && n50 == 0 {
        return 0.0;
    }
    let c100 = n100.min(N100_CAP);
    let c50 = n50.min(N50_CAP);

    let ratio = if c100 > 0 {
        f64::from(c50) / f64::from(c100)
    } else {
        VARI_RATIO * 2.0
    };
    let damp = if ratio <= VARI_RATIO {
        1.0
    } else {
        (1.0 - 0.5 * ((ratio - VARI_RATIO) / VARI_RATIO).min(1.0)).max(0.5)
    };

    (f64::from(c100) * N100_PER + f64::from(c50) * N50_PER) * damp
}
