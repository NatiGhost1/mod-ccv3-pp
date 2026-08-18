// CC V3 (NoFail): standalone performance scaling (v2).
//
// v2 enhancement: uses accuracy and HP drain rate to estimate WHERE
// the player would have failed without NF, rather than just checking
// if total_hits < n_objects (which only catches AFK/quit).
//
// Fail estimation model:
//   * HP drain rate determines how quickly the health bar depletes on
//     misses and 50s. Higher HP = fails happen faster.
//   * Accuracy determines how much health the player recovers per
//     hit. Low accuracy = health drains faster = earlier fail.
//   * Combined: estimated_fail_point = f(accuracy, hp, misses, total_hits)
//     A player with 85% acc on HP 8 would realistically fail at about
//     60% through the map. Without NF they'd never have reached the end.
//
// When the estimated fail point is < 1.0 (meaning the player would have
// failed), pp is scaled down based on how far they would have gotten.
// When the estimated fail point is > 1.0 (they would have passed),
// no additional penalty beyond the standard NF treatment.

/// Compute the NoFail performance multiplier.
///
/// Returns [0.0, 1.0]:
///   * 0.0 if the player clearly didn't play (total_hits < n_objects/2)
///   * Otherwise: fail_estimation × short_tax × symmetric_mult × miss_decay
pub fn nf_multiplier(
    map_max_combo: u32,
    player_max_combo: u32,
    misses: u32,
    total_hits: u32,
    n_objects: u32,
    accuracy: f64,
    hp: f64,
) -> f64 {
    // ── Hard fail: player hit fewer than half the objects ────────────
    // They were AFK or quit very early. PP = 0.
    if total_hits < n_objects / 2 {
        return 0.0;
    }

    // ── Soft fail: player didn't finish all objects ──────────────────
    // They tried but stopped partway. Small residual pp for partial play.
    if total_hits < n_objects {
        let completion = f64::from(total_hits) / f64::from(n_objects);
        // Partial play gets at most 20% of what they'd earn
        return (completion * 0.20).min(0.20);
    }

    let mc = f64::from(map_max_combo);
    let miss_f = f64::from(misses);

    // ── Fail estimation using accuracy + HP drain ───────────────────
    //
    // Model: health bar starts at 1.0 and drains by hp_drain_per_miss
    // for each miss/50, while recovering hp_recovery_per_hit for each
    // 300/100. We estimate whether the player would have reached 0 HP
    // at any point during the map.
    //
    // hp_drain_per_miss:  0.02 × hp   (HP 5 → 10% per miss, HP 8 → 16%)
    // hp_recovery_per_hit: based on accuracy
    //   acc 1.00 → 0.04 per hit (healthy recovery)
    //   acc 0.95 → 0.03 per hit
    //   acc 0.90 → 0.02 per hit
    //   acc 0.85 → 0.01 per hit (barely staying alive)
    //   acc 0.80 → 0.00 per hit (can't keep up)
    //
    // estimated_fail_fraction: approximate fraction of the map the
    // player would survive before health reaches 0.
    //   > 1.0 means they'd pass → no extra NF penalty
    //   < 1.0 means they'd fail → pp scaled by this fraction
    let fail_mult = if hp > 0.0 {
        let hp_drain_per_miss = 0.02 * hp;
        let hp_recovery_per_hit = ((accuracy - 0.80) / 0.20 * 0.04).clamp(0.0, 0.04);

        let total_hits_f = f64::from(total_hits);
        let non_misses = (total_hits_f - miss_f).max(0.0);

        // Net health change per "cycle" of notes:
        //   recovery from good hits minus drain from misses
        let net_recovery = non_misses * hp_recovery_per_hit;
        let net_drain = miss_f * hp_drain_per_miss;

        if net_drain > net_recovery {
            // Player would have run out of health.
            // Estimate how far they'd get: health_bar / drain_rate
            let drain_rate = (net_drain - net_recovery) / total_hits_f;
            // Starting at 1.0 health, time to reach 0:
            let notes_until_fail = if drain_rate > 0.0 { 1.0 / drain_rate } else { total_hits_f };
            let fail_fraction = (notes_until_fail / total_hits_f).clamp(0.0, 1.0);

            // Scale: estimated_fail_fraction^0.5 (gentle — sqrt curve
            // so 50% survival doesn't halve pp, it gets ~70%)
            fail_fraction.powf(0.5)
        } else {
            // Player would have survived → no extra fail penalty
            1.0
        }
    } else {
        1.0 // HP 0 = can't fail
    };

    // ── Short map tax (map_max_combo < 1000) ────────────────────────
    let short_tax = if map_max_combo < 1000 {
        let base = 0.70 + 0.30 * (mc / 1000.0);
        let miss_relief = 0.015 * miss_f.min(15.0);
        (base + miss_relief).min(1.0)
    } else {
        1.0
    };

    // ── Long map symmetric scaling (map_max_combo >= 1000) ──────────
    let symmetric_mult = if map_max_combo >= 1000 && misses > 0 && mc > 0.0 {
        let combo_ratio = (f64::from(player_max_combo) / mc).clamp(0.0, 1.0);
        let prox = 1.0 - ((combo_ratio - 0.5).abs() / 0.5).min(1.0);
        0.95 - 0.13 * prox
    } else {
        1.0
    };

    // ── Per-miss decay ──────────────────────────────────────────────
    let miss_decay = if misses > 0 {
        0.97_f64.powf(miss_f).max(0.50)
    } else {
        1.0
    };

    // ── Compose ─────────────────────────────────────────────────────
    (fail_mult * short_tax * symmetric_mult * miss_decay).max(0.0)
}
