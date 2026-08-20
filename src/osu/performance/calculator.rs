use std::{cmp, f64::consts::PI};

use crate::{
    osu::{
        difficulty::{
            rating::OsuRatingCalculator,
            skills::{
                aim::Aim, flashlight::Flashlight, memory::Memory, speed::Speed,
                strain::OsuStrainSkill,
            },
        },
        legacy_score_miss_calc::OsuLegacyScoreMissCalculator,
        OsuDifficultyAttributes, OsuPerformanceAttributes, OsuScoreState,
    },
    util::{
        difficulty::{erf, erf_inv, logistic, reverse_lerp, smoothstep},
        float_ext::FloatExt,
    },
    GameMods,
};

// * This is being adjusted to keep the final pp value scaled around what it used to be when changing things.
pub const PERFORMANCE_BASE_MULTIPLIER: f64 = 0.92; // 0.92 for ONLY vanilla osu!standard (and ap) not Relax.
pub(super) struct OsuPerformanceCalculator<'mods> {
    attrs: OsuDifficultyAttributes,
    mods: &'mods GameMods,
    acc: f64,
    state: OsuScoreState,
    using_classic_slider_acc: bool,
}

impl<'a> OsuPerformanceCalculator<'a> {
    pub const fn new(
        attrs: OsuDifficultyAttributes,
        mods: &'a GameMods,
        acc: f64,
        state: OsuScoreState,
        using_classic_slider_acc: bool,
    ) -> Self {
        Self {
            attrs,
            mods,
            acc,
            state,
            using_classic_slider_acc,
        }
    }
}

impl OsuPerformanceCalculator<'_> {
    pub fn calculate(self) -> OsuPerformanceAttributes {
        let total_hits = self.state.hitresults.total_hits();

        if total_hits == 0 {
            return OsuPerformanceAttributes {
                difficulty: self.attrs,
                ..Default::default()
            };
        }

        let acc = self.acc;
        let state = &self.state;
        let attrs = &self.attrs;
        let mods = self.mods;
        let using_classic_slider_acc = self.using_classic_slider_acc;

        let combo_based_estimated_miss_count = self.calculate_combo_based_estimated_miss_count();
        let mut score_based_estimated_miss_count = None;

        let mut effective_miss_count = if using_classic_slider_acc
            && state.legacy_total_score.is_some()
        {
            let legacy_score_miss_calc = OsuLegacyScoreMissCalculator::new(state, acc, mods, attrs);

            *score_based_estimated_miss_count.insert(legacy_score_miss_calc.calculate())
        } else {
            // * Use combo-based miss count if this isn't a legacy score
            combo_based_estimated_miss_count
        };

        effective_miss_count = effective_miss_count.max(f64::from(state.hitresults.misses));
        effective_miss_count = effective_miss_count.min(f64::from(state.hitresults.total_hits()));

        let total_hits = f64::from(total_hits);

        let mut multiplier = if self.mods.rx() {
            1.0
        } else {
            PERFORMANCE_BASE_MULTIPLIER
        }; // Relax retains the 1.0 multiplier because the relation between skill and pp already felt accurate.

        if self.mods.nf() {
            // CC V3: NoFail has its own standalone system (see nofail.rs).
            // The old per-miss multiplier is removed — the NF module handles
            // everything including short-map tax, symmetric scaling, and
            // per-miss decay. This line is kept as a no-op placeholder.
        }

        if self.mods.so() && total_hits > 0.0 {
            multiplier *= 1.0 - (f64::from(self.attrs.n_spinners) / total_hits).powf(0.85);
        }

        if self.mods.rx() {
            let od = self.attrs.od();

            // * https://www.desmos.com/calculator/vspzsop6td
            // * we use OD13.3 as maximum since it's the value at which great hitwidow becomes 0
            // * this is well beyond currently maximum achievable OD which is 12.17 (DTx2 + DA with OD11)
            let (n100_mult, n50_mult) = if od > 0.0 {
                (
                    0.75 * (1.0 - od / 13.33).max(0.0),
                    (1.0 - (od / 13.33).powf(5.0)).max(0.0),
                )
            } else {
                (1.0, 1.0)
            };

            // * As we're adding Oks and Mehs to an approximated number of combo breaks the result can be
            // * higher than total hits in specific scenarios (which breaks some calculations) so we need to clamp it.
            effective_miss_count = (effective_miss_count
                + f64::from(self.state.hitresults.n100) * n100_mult
                + f64::from(self.state.hitresults.n50) * n50_mult)
                .min(total_hits);
        }
        let penalty_miss_count = if self.mods.rx() {
            // CC V3 v3: strain-peak based relax miss model (see rx_miss.rs).
            // This REPLACES the literal miss count as the primary lever for
            // relax: misses on the highest strain peaks (first 2) and on
            // unlucky low peaks cost less; mid peaks are normal; Oks/Mehs
            // inflate it with hard caps + a variance-aware soft stop.
            super::rx_miss::rx_strain_weighted_misses(
                &self.attrs.rx_chunk_hardness,
                self.state.hitresults.n300,
                self.state.hitresults.n100,
                self.state.hitresults.n50,
                self.state.hitresults.misses,
            )
        } else {
            effective_miss_count
                + f64::from(self.state.hitresults.n50)
                    * n50_effective_miss_multiplier(self.attrs.od())
        };

        let speed_deviation = self.calculate_speed_deviation();

        let mut aim_estimated_slider_breaks = 0.0;
        let mut speed_estimated_slider_breaks = 0.0;

        let mut aim_value =
            self.compute_aim_value(penalty_miss_count, &mut aim_estimated_slider_breaks);
        let mut speed_value = self.compute_speed_value(
            speed_deviation,
            penalty_miss_count,
            &mut speed_estimated_slider_breaks,
        );
        let mut acc_value = self.compute_accuracy_value();
        let mut flashlight_value = self.compute_flashlight_value(penalty_miss_count);

        let mut pp = (aim_value.powf(1.1)
            + speed_value.powf(0.90) // speed is significantly less important than aim in cheat servers
            + acc_value.powf(1.1)
            + flashlight_value.powf(1.1))
        .powf(1.0 / 1.1)
            * multiplier;

        // ═════════════════════════════════════════════════════════════
        // CC V3 additions to the performance pass
        // ═════════════════════════════════════════════════════════════

        // ── Speed rework multiplier ─────────────────────────────────
        let speed_mult = if self.mods.ap() {
            if self.attrs.speed_rework_mult_autopilot > 0.0 {
                self.attrs.speed_rework_mult_autopilot
            } else {
                1.0
            }
        } else if self.attrs.speed_rework_mult_vanilla > 0.0 {
            self.attrs.speed_rework_mult_vanilla
        } else {
            1.0
        };
        speed_value *= speed_mult;

        // Recompute pp with the speed rework applied
        pp = (aim_value.powf(1.1)
            + speed_value.powf(0.90) // speed is significantly less important than aim in cheat servers
            + acc_value.powf(1.1)
            + flashlight_value.powf(1.1))
        .powf(1.0 / 1.1)
            * multiplier;

        // ── Relax marathon decay ────────────────────────────────────
        if self.mods.rx() {
            let params = super::relax_marathon::MarathonDecayParams::default();
            let mult = super::relax_marathon::relax_marathon_multiplier(
                &self.attrs.local_sr_per_minute,
                &self.attrs.local_bpm_per_minute,
                params,
            );
            aim_value *= mult.powf(0.92); // Soften the aim nerf a bit so marathon maps don't receive too harsh of a nerf in relax.
            flashlight_value *= mult; // Nerf remains at full weight for flashlight.
        }

        // ── Autopilot marathon decay ────────────────────────────────────
        if self.mods.ap() {
            let params = super::auto_marathon::AutopilotDecayParams::default();
            let mult = super::auto_marathon::autopilot_marathon_multiplier(
                &self.attrs.local_autopilot_sr_per_minute,
                &self.attrs.local_bpm_per_minute,
                &self.attrs.local_aim_per_minute,
                params,
            );
            speed_value *= mult;
            flashlight_value *= mult.powf(1.2); // Harshen the nerf on flashlight since flashlight doesn't matter on ap.
        }

        // ── Vanilla marathon buff ────────────────────────────────────
        if !self.mods.rx() && !self.mods.ap() {
            let params = super::marathon::VanillaMarathonParams::default();
            let mult = super::marathon::vanilla_marathon_multiplier(
                &self.attrs.local_aim_per_minute,
                &self.attrs.local_sr_per_minute,
                self.attrs.max_combo,
                self.state.max_combo,
                self.acc,
                params,
            );
            if mult > 1.0 {
                aim_value *= mult.powf(0.88); // aim gets the same reduction as acc
                speed_value *= mult.powf(0.88); // speed gets the same reduction as acc
                acc_value *= mult.powf(0.88); // acc gets a bit less of the buff
            }
        }

        // ── CC V3 consistency multiplier (non-RX, non-AP) ───────────
        let ccv3_mult = self.apply_cc_v3_multiplier(effective_miss_count);
        let combo_tax = self.combo_ratio_tax();
        let ccv3_scale = ccv3_mult * combo_tax;

        pp *= ccv3_scale;
        aim_value *= ccv3_scale;
        speed_value *= ccv3_scale;
        acc_value *= ccv3_scale;
        flashlight_value *= ccv3_scale;

        // ── AP standalone miss system ───────────────────────────────
        if self.mods.ap() {
            let ap_mult = super::ap_miss::ap_miss_multiplier(
                self.attrs.od(),
                self.attrs.dominant_tap_bpm,
                &self.attrs.rx_chunk_hardness,
                &self.attrs.rx_chunk_avg_delta,
                self.state.hitresults.n300,
                self.state.hitresults.n100,
                self.state.hitresults.n50,
                self.state.hitresults.misses,
                self.state.max_combo,
                self.attrs.max_combo,
            );
            pp *= ap_mult;
            aim_value *= ap_mult;
            speed_value *= ap_mult;
            acc_value *= ap_mult;
            flashlight_value *= ap_mult;
        }

        // ── RX miss handling ────────────────────────────────────────
        // Relax misses are handled primarily via `penalty_miss_count` above
        // (the v3 strain-peak model in rx_miss.rs), which feeds the main aim
        // miss-penalty. No secondary multiplier is applied here anymore.

        // ── NF standalone system ─────────────────────────────────────
        // NoFail has its own performance model (see nofail.rs):
        //   * Incomplete plays (didn't finish) → pp = 0
        //   * Short maps (combo < 1000) → map-combo-based tax, lightened
        //     with more misses but pp always monotonically decreasing
        //   * Long maps → symmetric midpoint-weighted miss scaling
        //   * Per-miss gentle decay (0.97^n, floor 0.50)
        //
        // This replaces the old NF multiplier (1 − 0.02×misses) and the
        // CC V3 exponential (which returns 1.0 on NF).
        if self.mods.nf() {
            let nf_mult = super::nofail::nf_multiplier(
                self.attrs.max_combo,
                self.state.max_combo,
                self.state.hitresults.misses,
                self.state.hitresults.total_hits(),
                self.attrs.n_objects(),
                acc,
                self.attrs.hp,
            );

            pp *= nf_mult;
            aim_value *= nf_mult;
            speed_value *= nf_mult;
            acc_value *= nf_mult;
            flashlight_value *= nf_mult;
        }

        // Apply the common miss-position tax after each mode's standalone
        // miss system. Lower achieved combo means the miss occurred earlier
        // relative to the map's total possible combo.
        let miss_combo_mult = Self::miss_combo_multiplier(
            self.attrs.max_combo,
            self.state.max_combo,
            effective_miss_count,
        );
        pp *= miss_combo_mult;
        aim_value *= miss_combo_mult;
        speed_value *= miss_combo_mult;
        acc_value *= miss_combo_mult;
        flashlight_value *= miss_combo_mult;

        // CCV3 targeted PP-layer nerfs

        // OD < 9 accuracy nerf
        if self.attrs.od() < 9.0 && !self.mods.rx() {
            let below = (9.0 - self.attrs.od()).min(3.0);
            let od_nerf = 1.0 - 0.073 * below;
            acc_value *= od_nerf;
        }

        // AR 10.1-10.5 band nerf
        if self.attrs.ar > 10.1 && self.attrs.ar <= 10.5 && !self.mods.rx() {
            let mid = 10.3;
            let half = 0.2;
            let t = 1.0 - ((self.attrs.ar - mid).abs() / half).min(1.0);
            aim_value *= 1.0 - 0.06 * t;
        }

        // CS + mid-BPM 1/2 nerf
        if self.attrs.median_delta_time > 0.0 {
            let md = self.attrs.median_delta_time;
            let in_band = md >= 176.0 && md <= 250.0;
            let cs = self.attrs.cs;
            if in_band && cs >= 4.6 && cs <= 6.4 {
                let cs_t = 1.0 - ((cs - 5.5).abs() / 0.9).min(1.0);
                let bpm_1_2 = 30_000.0 / md;
                let bpm_t = 1.0 - ((bpm_1_2 - 145.0).abs() / 25.0).min(1.0);
                aim_value *= 1.0 - 0.10 * cs_t * bpm_t;
            }
        }

        // Recompute final pp with all nerfs
        pp = (aim_value.powf(1.05) // aim is slightly less important than accuracy in cheat servers
            + speed_value.powf(0.90) // speed is significantly less important than aim in cheat servers
            + acc_value.powf(1.1) // accuracy is slightly more important than aim and speed in cheat servers
            + flashlight_value.powf(1.1)) // flashlight wont be removed so it stays the same as accuracy in cheat servers
        .powf(1.0 / 1.1)
            * multiplier; // Changed values to more accurately reflect the importance of each skill in cheat servers

        if !self.mods.ap() {
            let consistency_mult = Self::calculate_consistency_multiplier(
                self.attrs.consistency_strictness,
                speed_deviation,
            );
            pp *= consistency_mult;
            aim_value *= consistency_mult;
            speed_value *= consistency_mult;
            acc_value *= consistency_mult;
            flashlight_value *= consistency_mult;
        }

        if self.mods.td() {
            pp *= 0.90;
            aim_value *= 0.90;
            speed_value *= 0.90;
            acc_value *= 0.90;
            flashlight_value *= 0.90;
        }

        OsuPerformanceAttributes {
            difficulty: self.attrs,
            pp_acc: acc_value,
            pp_aim: aim_value,
            pp_flashlight: flashlight_value,
            pp_speed: speed_value,
            pp,
            effective_miss_count,
            speed_deviation,
            combo_based_estimated_miss_count,
            score_based_estimated_miss_count,
            aim_estimated_slider_breaks,
            speed_estimated_slider_breaks,
        }
    }

    fn compute_aim_value(
        &self,
        effective_miss_count: f64,
        aim_estimated_slider_breaks: &mut f64,
    ) -> f64 {
        if self.mods.ap() && !self.mods.fl() {
            return 0.0;
        }

        let mut aim_difficulty = self.attrs.aim;

        if self.mods.ap() {
            let jump_factor = ((self.attrs.avg_jump_dist - 80.0) / 120.0).clamp(0.0, 1.0);
            let bpm = if self.attrs.median_delta_time > 0.0 {
                15_000.0 / self.attrs.median_delta_time
            } else {
                0.0
            };
            let bpm_factor = ((bpm - 180.0) / 100.0).clamp(0.0, 1.0);
            aim_difficulty *= 0.20 + 0.80 * jump_factor * bpm_factor;
        }

        if self.attrs.n_sliders > 0 && self.attrs.aim_difficult_slider_count > 0.0 {
            let estimate_improperly_followed_difficult_sliders = if self.using_classic_slider_acc {
                // * When the score is considered classic (regardless if it was made on old client or not)
                // * we consider all missing combo to be dropped difficult sliders
                let maximum_possible_dropped_sliders = self.total_imperfect_hits();

                f64::clamp(
                    f64::min(
                        maximum_possible_dropped_sliders,
                        f64::from(self.attrs.max_combo - self.state.max_combo),
                    ),
                    0.0,
                    self.attrs.aim_difficult_slider_count,
                )
            } else {
                // * We add tick misses here since they too mean that the player didn't follow the slider properly
                // * We however aren't adding misses here because missing slider heads has a harsh penalty
                // * by itself and doesn't mean that the rest of the slider wasn't followed properly
                f64::clamp(
                    f64::from(self.n_slider_ends_dropped() + self.n_large_tick_miss()),
                    0.0,
                    self.attrs.aim_difficult_slider_count,
                )
            };

            let slider_nerf_factor = (1.0 - self.attrs.slider_factor)
                * f64::powf(
                    1.0 - estimate_improperly_followed_difficult_sliders
                        / self.attrs.aim_difficult_slider_count,
                    3.0,
                )
                + self.attrs.slider_factor;
            aim_difficulty *= slider_nerf_factor;
        }

        let mut aim_value = Aim::difficulty_to_performance(aim_difficulty);
        let density_multiplier = if self.attrs.density_multiplier > 0.0 {
            self.attrs.density_multiplier
        } else {
            1.0
        };
        aim_value *= density_multiplier;

        let total_hits = self.total_hits();

        let len_bonus = Self::length_bonus(total_hits, self.acc);

        aim_value *= len_bonus;

        if effective_miss_count > 0.0 {
            *aim_estimated_slider_breaks = self.calculate_estimated_slider_breaks(
                self.attrs.aim_top_weighted_slider_factor,
                effective_miss_count,
            );

            let relevant_miss_count = (effective_miss_count + *aim_estimated_slider_breaks)
                .min(self.total_imperfect_hits() + f64::from(self.n_large_tick_miss()));

            aim_value *= Self::calculate_miss_penalty(
                relevant_miss_count,
                self.attrs.aim_difficult_strain_count,
            );
            aim_value *= Self::low_od_miss_multiplier(relevant_miss_count, self.attrs.od());
        }

        // * TC bonuses are excluded when blinds is present as the increased visual difficulty is unimportant when notes cannot be seen.
        if self.mods.bl() {
            aim_value *= 1.3
                + (total_hits
                    * (0.0016 / (1.0 + 2.0 * effective_miss_count))
                    * self.acc.powf(16.0))
                    * (1.0 - 0.003 * self.attrs.hp * self.attrs.hp);
        } else if self.mods.tc() {
            aim_value *= 1.0
                + OsuRatingCalculator::calculate_visibility_bonus(
                    self.mods,
                    self.attrs.ar,
                    Some(self.attrs.slider_factor),
                    None,
                );
        }

        aim_value *= self.acc;

        aim_value
    }

    fn compute_speed_value(
        &self,
        speed_deviation: Option<f64>,
        effective_miss_count: f64,
        speed_estimated_slider_breaks: &mut f64,
    ) -> f64 {
        let Some(speed_deviation) = speed_deviation.filter(|_| !self.mods.rx()) else {
            return 0.0;
        };

        let mut speed_value = Speed::difficulty_to_performance(self.attrs.speed);

        let total_hits = self.total_hits();

        let len_bonus = Self::length_bonus(total_hits, self.acc);

        speed_value *= len_bonus;

        if effective_miss_count > 0.0 {
            *speed_estimated_slider_breaks = self.calculate_estimated_slider_breaks(
                self.attrs.speed_top_weighted_slider_factor,
                effective_miss_count,
            );

            let relevant_miss_count = (effective_miss_count + *speed_estimated_slider_breaks)
                .min(self.total_imperfect_hits() + f64::from(self.n_large_tick_miss()));

            speed_value *= Self::calculate_miss_penalty(
                relevant_miss_count,
                self.attrs.speed_difficult_strain_count,
            );
            speed_value *= Self::low_od_miss_multiplier(relevant_miss_count, self.attrs.od());
        }

        // * TC bonuses are excluded when blinds is present as the increased visual difficulty is unimportant when notes cannot be seen.
        if self.mods.bl() {
            // * Increasing the speed value by object count for Blinds isn't
            // * ideal, so the minimum buff is given.
            speed_value *= 1.12;
        } else if self.mods.tc() {
            speed_value *= 1.0
                + OsuRatingCalculator::calculate_visibility_bonus(
                    self.mods,
                    self.attrs.ar,
                    None,
                    None,
                );
        }

        let speed_high_deviation_mult = self.calculate_speed_high_deviation_nerf(speed_deviation);
        speed_value *= speed_high_deviation_mult;

        // * Calculate accuracy assuming the worst case scenario
        let relevant_total_diff = f64::max(0.0, total_hits - self.attrs.speed_note_count);
        let hitresults = &self.state.hitresults;
        let relevant_n300 = (f64::from(hitresults.n300) - relevant_total_diff).max(0.0);
        let relevant_n100 = (f64::from(hitresults.n100)
            - (relevant_total_diff - f64::from(hitresults.n300)).max(0.0))
        .max(0.0);
        let relevant_n50 = (f64::from(hitresults.n50)
            - (relevant_total_diff - f64::from(hitresults.n300 + hitresults.n100)).max(0.0))
        .max(0.0);

        let relevant_acc = if self.attrs.speed_note_count.eq(0.0) {
            0.0
        } else {
            (relevant_n300 * 6.0 + relevant_n100 * 2.0 + relevant_n50)
                / (self.attrs.speed_note_count * 6.0)
        };

        let od = self.attrs.od();

        // * Scale the speed value with accuracy and OD.
        speed_value *= f64::powf((self.acc + relevant_acc) / 2.0, (14.5 - od) / 2.0);

        speed_value
    }

    fn compute_accuracy_value(&self) -> f64 {
        let mut amount_hit_objects_with_acc = self.attrs.n_circles;

        if !self.using_classic_slider_acc {
            amount_hit_objects_with_acc += self.attrs.n_sliders;
        }

        let hitresults = &self.state.hitresults;

        let mut better_acc_percentage = if amount_hit_objects_with_acc > 0 {
            f64::from(
                (hitresults.n300 as i32
                    - (cmp::max(
                        hitresults.total_hits() as i32 - amount_hit_objects_with_acc as i32,
                        0,
                    )))
                    * 6
                    + hitresults.n100 as i32 * 2
                    + hitresults.n50 as i32,
            ) / f64::from(amount_hit_objects_with_acc * 6)
        } else {
            0.0
        };

        if better_acc_percentage < 0.0 {
            better_acc_percentage = 0.0;
        }

        let mut acc_value =
            1.52163_f64.powf(self.attrs.od()) * better_acc_percentage.powf(24.0) * 2.83;

        acc_value *= (f64::from(amount_hit_objects_with_acc) / 1000.0)
            .powf(0.3)
            .min(1.15);

        // DO NOT EVER TOUCH THIS AGAIN UNDER ANY SCENARIO SO HELP ME GOD IF THIS GETS OUT OF HAND ON EXTREME MAPS I WILL PERSONALLY COME AFTER YOU AND UNPLUG YOU. I SWEAR TO GOD. I DONT CARE IF THIS GETS OUT OF HAND ON EXTREME MAPS, THIS IS THE FINAL FORMULA AND I WILL NOT CHANGE IT. CLAUDE KEEP YOUR ROBOTOIC SLIMY GREASY HANDS OFF THIS FORMULA.
        if self.mods.bl() {
            acc_value *= 1.14;
        } else if self.mods.hd() || self.mods.tc() {
            let mut hd_bonus = 1.0; // HD bonus set to 1.0 (no bonus) because of the exsistance of HD remover.

            acc_value *= hd_bonus;
        }

        if self.mods.rx() {
            let mut accuracy_multiplier = 0.62 + 0.18 * better_acc_percentage.powf(10.0); // Increased accuracy multiplier for relax so high accuracy scores are rewarded more.
            acc_value *= accuracy_multiplier
        } else {
            let mut accuracy_multiplier = 0.75 + 0.25 * better_acc_percentage.powf(10.0); // Accuracy multiplier for non-relax scores.
            acc_value *= accuracy_multiplier;
        } else if self.mods.ap() {
            let mut accuracy_multiplier = 1.0; // No accuracy multiplier for AP.
            acc_value *= accuracy_multiplier;
        } else if self.mods.fl() && !self.mods.ap() && !self.mods.rx() {
            acc_value *= 1.02;
        }

        acc_value
    }

    fn compute_flashlight_value(&self, effective_miss_count: f64) -> f64 {
        if !self.mods.fl() {
            return 0.0;
        }

        let mut flashlight_value = Flashlight::difficulty_to_performance(self.attrs.flashlight);
        let memory_value = Memory::performance_value(
            self.attrs.memory,
            self.acc,
            f64::from(self.state.max_combo),
            f64::from(self.attrs.max_combo),
            effective_miss_count,
            self.total_hits(),
        );
        flashlight_value = (flashlight_value.powf(1.1) + memory_value.powf(1.1)).powf(1.0 / 1.1);

        let total_hits = self.total_hits();

        // * Penalize misses by assessing # of misses relative to the total # of objects. Default a 3% reduction for any # of misses.
        if effective_miss_count > 0.0 {
            flashlight_value *= 0.97
                * (1.0 - (effective_miss_count / total_hits).powf(0.775))
                    .powf(effective_miss_count.powf(0.875));
        }

        flashlight_value *= self.get_combo_scaling_factor();

        // * Scale the flashlight value with accuracy _slightly_.
        flashlight_value *= 0.5 + self.acc / 2.0;

        flashlight_value
    }

    fn calculate_combo_based_estimated_miss_count(&self) -> f64 {
        let Self {
            state,
            attrs,
            using_classic_slider_acc,
            ..
        } = self;

        if attrs.n_sliders == 0 {
            return f64::from(state.hitresults.misses);
        }

        let mut miss_count = f64::from(state.hitresults.misses);

        if *using_classic_slider_acc {
            // * Consider that full combo is maximum combo minus dropped slider tails since they don't contribute to combo but also don't break it
            // * In classic scores we can't know the amount of dropped sliders so we estimate to 10% of all sliders on the map
            let full_combo_threshold =
                f64::from(attrs.max_combo) - 0.1 * f64::from(attrs.n_sliders);

            if f64::from(state.max_combo) < full_combo_threshold {
                miss_count = full_combo_threshold / f64::from(state.max_combo).max(1.0);
            }

            // * In classic scores there can't be more misses than a sum of all non-perfect judgements
            miss_count = miss_count.min(self.total_imperfect_hits());

            // * Every slider has *at least* 2 combo attributed in classic mechanics.
            // * If they broke on a slider with a tick, then this still works since they would have lost at least 2 combo (the tick and the end)
            // * Using this as a max means a score that loses 1 combo on a map can't possibly have been a slider break.
            // * It must have been a slider end.
            let max_possible_slider_breaks = cmp::min(
                attrs.n_sliders,
                (attrs.max_combo.saturating_sub(state.max_combo)) / 2,
            );

            let slider_breaks = miss_count - f64::from(state.hitresults.misses);

            if slider_breaks > f64::from(max_possible_slider_breaks) {
                miss_count = f64::from(state.hitresults.misses + max_possible_slider_breaks);
            }
        } else {
            let full_combo_threshold = f64::from(attrs.max_combo - self.n_slider_ends_dropped());

            if f64::from(state.max_combo) < full_combo_threshold {
                miss_count = full_combo_threshold / f64::from(state.max_combo).max(1.0);
            }

            // * Combine regular misses with tick misses since tick misses break combo as well
            miss_count = miss_count.min(f64::from(
                self.n_large_tick_miss() + state.hitresults.misses,
            ));
        }

        miss_count
    }

    fn calculate_estimated_slider_breaks(
        &self,
        top_weighted_slider_factor: f64,
        effective_miss_count: f64,
    ) -> f64 {
        let Self {
            attrs,
            state,
            using_classic_slider_acc,
            ..
        } = self;

        if !using_classic_slider_acc || state.hitresults.n100 == 0 {
            return 0.0;
        }

        let missed_combo_percent = 1.0 - f64::from(state.max_combo) / f64::from(attrs.max_combo);
        let mut estimated_slider_breaks = (effective_miss_count * top_weighted_slider_factor)
            .min(f64::from(state.hitresults.n100));

        // * Scores with more Oks are more likely to have slider breaks.
        let ok_adjustment = ((f64::from(state.hitresults.n100) - estimated_slider_breaks) + 0.5)
            / f64::from(state.hitresults.n100);

        // * There is a low probability of extra slider breaks on effective miss counts close to 1, as score based calculations are good at indicating if only a single break occurred.
        estimated_slider_breaks *= smoothstep(effective_miss_count, 1.0, 2.0);

        estimated_slider_breaks * ok_adjustment * logistic(missed_combo_percent, 0.33, 15.0, None)
    }

    fn calculate_speed_deviation(&self) -> Option<f64> {
        if self.total_successful_hits() == 0 {
            return None;
        }

        let hitresults = &self.state.hitresults;

        // * Calculate accuracy assuming the worst case scenario
        let mut speed_note_count = self.attrs.speed_note_count;
        speed_note_count +=
            (f64::from(hitresults.total_hits()) - self.attrs.speed_note_count) * 0.1;

        // * Assume worst case: all mistakes were on speed notes
        let relevant_count_miss = f64::min(f64::from(hitresults.misses), speed_note_count);
        let relevant_count_meh = f64::min(
            f64::from(hitresults.n50),
            speed_note_count - relevant_count_miss,
        );
        let relevant_count_ok = f64::min(
            f64::from(hitresults.n100),
            speed_note_count - relevant_count_miss - relevant_count_meh,
        );
        let relevant_count_great = f64::max(
            0.0,
            speed_note_count - relevant_count_miss - relevant_count_meh - relevant_count_ok,
        );

        self.calculate_deviation(relevant_count_great, relevant_count_ok, relevant_count_meh)
    }

    fn calculate_deviation(
        &self,
        relevant_count_great: f64,
        relevant_count_ok: f64,
        relevant_count_meh: f64,
    ) -> Option<f64> {
        if relevant_count_great + relevant_count_ok + relevant_count_meh <= 0.0 {
            return None;
        }

        // * The sample proportion of successful hits.
        let n = f64::max(1.0, relevant_count_great + relevant_count_ok);
        let p = relevant_count_great / n;

        #[expect(
            clippy::items_after_statements,
            clippy::unreadable_literal,
            reason = "staying in-sync with lazer"
        )]
        // * 99% critical value for the normal distribution (one-tailed).
        const Z: f64 = 2.32634787404;

        // * We can be 99% confident that the population proportion is at least this value.
        let p_lower_bound = ((n * p + Z * Z / 2.0) / (n + Z * Z)
            - Z / (n + Z * Z) * f64::sqrt(n * p * (1.0 - p) + Z * Z / 4.0))
        .min(p);

        let great_hit_window: f64 = self.attrs.great_hit_window;
        let ok_hit_window: f64 = self.attrs.ok_hit_window;
        let meh_hit_window: f64 = self.attrs.meh_hit_window;

        let mut deviation;

        if p_lower_bound > 0.01 {
            deviation = great_hit_window / (f64::sqrt(2.0) * erf_inv(p_lower_bound));

            // * Subtract the deviation provided by tails that land outside the ok hit window from the deviation computed above.
            // * This is equivalent to calculating the deviation of a normal distribution truncated at +-okHitWindow.
            let ok_hit_window_tail_amount = f64::sqrt(2.0 / PI)
                * ok_hit_window
                * f64::exp(-0.5 * f64::powf(ok_hit_window / deviation, 2.0))
                / (deviation * erf(ok_hit_window / (f64::sqrt(2.0) * deviation)));

            deviation *= f64::sqrt(1.0 - ok_hit_window_tail_amount);
        } else {
            // * A tested limit value for the case of a score only containing oks.
            deviation = ok_hit_window / f64::sqrt(3.0);
        }

        // * Compute and add the variance for mehs, assuming that they are uniformly distributed.
        let meh_variance = (meh_hit_window * meh_hit_window
            + ok_hit_window * meh_hit_window
            + ok_hit_window * ok_hit_window)
            / 3.0;

        let deviation = f64::sqrt(
            ((relevant_count_great + relevant_count_ok) * f64::powf(deviation, 2.0)
                + relevant_count_meh * meh_variance)
                / (relevant_count_great + relevant_count_ok + relevant_count_meh),
        );

        Some(deviation)
    }

    fn calculate_speed_high_deviation_nerf(&self, speed_deviation: f64) -> f64 {
        let speed_value = Speed::difficulty_to_performance(self.attrs.speed);

        // * Decides a point where the PP value achieved compared to the speed deviation is assumed to be tapped improperly. Any PP above this point is considered "excess" speed difficulty.
        // * This is used to cause PP above the cutoff to scale logarithmically towards the original speed value thus nerfing the value.
        let excess_speed_difficulty_cutoff = 100.0 + 220.0 * f64::powf(22.0 / speed_deviation, 6.5);

        if speed_value <= excess_speed_difficulty_cutoff {
            return 1.0;
        }

        #[expect(clippy::items_after_statements, reason = "staying in-sync with lazer")]
        const SCALE: f64 = 50.0;

        let mut adjusted_speed_value = SCALE
            * (f64::ln((speed_value - excess_speed_difficulty_cutoff) / SCALE + 1.0)
                + excess_speed_difficulty_cutoff / SCALE);

        // * 220 UR and less are considered tapped correctly to ensure that normal scores will be punished as little as possible
        let lerp = 1.0 - reverse_lerp(speed_deviation, 22.0, 27.0);
        adjusted_speed_value = f64::lerp(adjusted_speed_value, speed_value, lerp);

        adjusted_speed_value / speed_value
    }

    fn calculate_consistency_multiplier(strictness: f64, speed_deviation: Option<f64>) -> f64 {
        let Some(speed_deviation) = speed_deviation else {
            return 1.0;
        };

        let excess_ur = (speed_deviation * 10.0 - 10.0).max(0.0);
        let ur_factor = (excess_ur / 40.0).clamp(0.0, 1.0);

        (1.0 - 0.12 * strictness * ur_factor).clamp(0.88, 1.0)
    }

    // * Miss penalty assumes that a player will miss on the hardest parts of a map,
    // * so we use the amount of relatively difficult sections to adjust miss penalty
    // * to make it more punishing on maps with lower amount of hard sections.
    fn calculate_miss_penalty(miss_count: f64, diff_strain_count: f64) -> f64 {
        0.96 / ((miss_count / (4.0 * diff_strain_count.ln().powf(0.94))) + 1.0)
    }

    fn length_bonus(total_hits: f64, accuracy: f64) -> f64 {
        let base = 0.95;
        let raw = base
            + 0.4 * (total_hits / 2000.0).min(1.0)
            + f64::from(u8::from(total_hits > 2000.0)) * (total_hits / 2000.0).log10() * 0.5;
        base + (raw - base) * accuracy.clamp(0.0, 1.0).powf(2.0)
    }

    fn low_od_miss_multiplier(misses: f64, od: f64) -> f64 {
        if misses <= 0.0 || od >= 7.0 {
            return 1.0;
        }

        let severity = ((7.0 - od) / 7.0).clamp(0.0, 1.0);
        (1.0 - 0.14 * severity).powf(misses).max(0.30)
    }

    fn get_combo_scaling_factor(&self) -> f64 {
        if self.attrs.max_combo == 0 {
            1.0
        } else {
            (f64::from(self.state.max_combo).powf(0.8) / f64::from(self.attrs.max_combo).powf(0.8))
                .min(1.0)
        }
    }

    const fn total_hits(&self) -> f64 {
        self.state.hitresults.total_hits() as f64
    }

    const fn total_successful_hits(&self) -> u32 {
        self.state.hitresults.n300 + self.state.hitresults.n100 + self.state.hitresults.n50
    }

    fn total_imperfect_hits(&self) -> f64 {
        f64::from(
            self.state.hitresults.n100 + self.state.hitresults.n50 + self.state.hitresults.misses,
        )
    }

    const fn n_slider_ends_dropped(&self) -> u32 {
        self.attrs.n_sliders - self.state.hitresults.slider_end_hits
    }

    const fn n_large_tick_miss(&self) -> u32 {
        if self.using_classic_slider_acc {
            0
        } else {
            self.attrs.n_large_ticks - self.state.hitresults.large_tick_hits
        }
    }

    /// CC V3 combo-ratio tax. Light tax based on achieved combo ratio.
    /// FC passes through untouched.
    fn combo_ratio_tax(&self) -> f64 {
        if self.attrs.max_combo == 0 {
            return 1.0;
        }
        let ratio =
            (f64::from(self.state.max_combo) / f64::from(self.attrs.max_combo)).clamp(0.0, 1.0);
        (0.85 + 0.15 * ratio.powf(0.35)).min(1.0)
    }

    /// Shared combo scaling for every miss system.
    ///
    /// A miss on a low-combo portion of a long map is more damaging than the
    /// same miss after most of the map has already been completed. The factor
    /// is relative to the map's total possible combo and leaves FCs untouched.
    fn miss_combo_multiplier(
        map_max_combo: u32,
        player_max_combo: u32,
        effective_miss_count: f64,
    ) -> f64 {
        if effective_miss_count <= 0.0 || map_max_combo == 0 {
            return 1.0;
        }

        let combo_ratio = (f64::from(player_max_combo) / f64::from(map_max_combo)).clamp(0.0, 1.0);

        (0.75 + 0.25 * combo_ratio.powf(0.7)).clamp(0.75, 1.0)
    }

    /// CC V3 exponential consistency multiplier (non-RX, non-AP).
    /// RX and AP use their own standalone miss systems and bypass this.
    ///
    /// Includes n50 effective miss inflation:
    ///   * OD scaling — exponential, steep below OD 5. At OD ≤ 1 each
    ///     n50 counts as 1 full effective miss. At OD 10 they don't count.
    ///   * AR scaling — AR ≥ 9 = full n50 misses (hard to read = more 50s
    ///     expected from aim, not timing). AR 7–9 = linear taper. AR < 7 = 0.
    ///   * Combo factor — for maps ≥ 1300 max_combo, the n50 miss count
    ///     decreases as combo grows, reaching 0 at max_combo 10000.
    ///     Maps under 1300 get full n50 misses.
    ///   * EZ and NF — n50 misses removed entirely. EZ is low AR (hard to
    ///     read), NF is meant to make the game easier.
    fn apply_cc_v3_multiplier(&self, effective_miss_count: f64) -> f64 {
        if effective_miss_count <= 0.0 && self.state.hitresults.n50 == 0 {
            return 1.0;
        }

        // RX, AP, and NF use standalone systems
        if self.mods.rx() || self.mods.ap() || self.mods.nf() {
            return 1.0;
        }

        let od = self.attrs.od();
        let ar = self.attrs.ar;
        let map_max_combo = self.attrs.max_combo;
        let n50 = self.state.hitresults.n50;
        let is_ez = self.mods.ez();
        let is_nf = self.mods.nf();

        // ── n50 effective miss inflation ─────────────────────────────
        //
        // n50_eff_misses = n50 × od_factor × ar_factor × combo_factor
        //
        // OD factor: ((10 − od) / 9)³, clamped to [0, 1].
        //   OD ≤ 1 → 1.000     (each n50 = full miss)
        //   OD  3  → 0.470
        //   OD  5  → 0.171     (steep drop-off below here)
        //   OD  7  → 0.037
        //   OD  9  → 0.001
        //   OD 10  → 0.000
        //
        // AR factor:
        //   AR ≥ 9 → 1.0       (always max n50 misses)
        //   AR  8  → 0.5       (linear taper)
        //   AR ≤ 7 → 0.0       (n50 misses don't count — low AR hard to read)
        //
        // Combo factor (maps ≥ 1300 max_combo only):
        //   Scales linearly from 1.0 at combo 1300 to 0.0 at combo 10000.
        //   Maps under 1300: combo_factor = 1.0 (no reduction).
        //
        // EZ or NF: n50 misses removed entirely.

        let n50_eff_misses = if (is_ez || is_nf) || n50 == 0 {
            0.0
        } else {
            // Smoothly derive an effective guaranteed miss threshold from OD and AR.
            // Low OD + high AR should yield a higher guaranteed miss floor,
            // but the result should be continuous rather than stepped.
            let od_factor = ((7.0 - od).clamp(0.0, 4.0) / 4.0).powf(1.4);
            let ar_factor = ((ar - 7.0).clamp(0.0, 2.0) / 2.0).powf(0.9);

            let guaranteed_threshold = 1.0 + 2.0 * (od_factor * ar_factor).clamp(0.0, 1.0);
            let n50_f = f64::from(n50);

            let guaranteed_count = n50_f.min(guaranteed_threshold);
            let remaining_n50 = (n50_f - guaranteed_count).max(0.0);

            // Use an exponent on the remaining 50s so they fade out smoothly
            // instead of behaving like a hard count.
            let remaining_scale = 0.55 + 0.45 * (od_factor * ar_factor);
            let remaining_scaled = remaining_n50.powf(1.12) * remaining_scale;

            guaranteed_count + remaining_scaled;

            // OD factor: exponential, steep below OD 5
            let od_factor = if od <= 1.0 {
                1.0
            } else {
                ((10.0 - od) / 9.0).powf(3.0).clamp(0.0, 1.0)
            };

            // AR factor: AR >= 9 full, AR 7-9 linear, AR < 7 zero
            let ar_factor = if ar >= 9.0 {
                1.0
            } else if ar >= 7.0 {
                (ar - 7.0) / 2.0
            } else {
                0.0
            };

            // Combo factor: maps >= 1300 combo scale down, 0 at 10000
            let combo_factor = if map_max_combo >= 1300 {
                (1.0 - (f64::from(map_max_combo) - 1300.0) / (10000.0 - 1300.0)).clamp(0.0, 1.0)
            } else {
                1.0
            };

            // Total = (First X weighted at 1.0) + (The rest scaled down)
            guaranteed_count + (remaining_n50 * od_factor * ar_factor * combo_factor)
        };

        let misses = effective_miss_count + n50_eff_misses;

        if misses <= 0.0 {
            return 1.0;
        }

        // ═════════════════════════════════════════════════════════════
        // CC V3: Reworked exponential miss decay (continuous dynamic).
        //
        // Replaces the stepped exponent tiers (1.5/1.7/2.1/2.3/2.4 at
        // fixed thresholds) with a smooth, continuously evolving curve:
        //
        //   miss_exp = 1.5 + 0.9 × (1 − e^(−misses / 8))
        //
        //   misses  1 → 1.62    (was 1.5 in old system)
        //   misses  2 → 1.72    (was 1.7)
        //   misses  4 → 1.95    (was 2.1)
        //   misses  6 → 2.12    (was 2.3)
        //   misses 10 → 2.31    (was 2.3)
        //   misses 14 → 2.37    (was 2.4)
        //   misses 20 → 2.39    (asymptote at 2.4)
        //
        // No more discrete jumps — the curve is smooth and each
        // additional miss increases the exponent by a diminishing
        // amount. This eliminates the "cliff" at 2/4/6/14 misses
        // where one extra miss could jump the exponent by 0.2-0.4.
        //
        // Marathon softening: for maps with high max_combo, the
        // exponent is gently reduced because long maps have more
        // notes and each miss is proportionally less significant:
        //
        //   combo_softening = 1.0 − 0.15 × clamp((combo−1000)/4000, 0, 1)
        //
        //   combo 1000:  no softening (1.00)
        //   combo 3000:  ×0.925
        //   combo 5000+: ×0.85
        //
        // Accuracy calibration: high accuracy (>95%) on long maps
        // gets a small relief (up to 8%) on the final multiplier.
        // The logic: sustaining 95%+ acc while dropping a few notes
        // means the player is genuinely consistent and the misses
        // were isolated incidents, not a collapse.
        //
        //   acc_relief = 0.08 × clamp((acc−0.95)/0.05, 0, 1)
        //              × clamp(combo/2000, 0, 1)
        // ═════════════════════════════════════════════════════════════

        let mut p: f64 = 0.998;

        // Continuous exponent: smooth exponential rise from 1.5 to ~2.4 (not anymore)
        let base_exp = 2.1 + 0.9 * (1.0 - (-misses / 8.0).exp()); // Strictened the base miss exponent to better emphasize the importance of accuracy in a cheat environment.

        // Marathon softening: longer maps get a gentler exponent
        let combo_f = f64::from(map_max_combo);
        let combo_softening = 1.0 - 0.15 * ((combo_f - 1000.0) / 4000.0).clamp(0.0, 1.0);

        let miss_exp = base_exp * combo_softening;

        // Compute the miss weight using the continuous exponent
        let miss_weight = misses.powf(miss_exp);

        // Base multiplier from exponential decay
        let mut result = p.powf(miss_weight);

        // Accuracy calibration: high acc on long maps → small relief
        let acc = self.acc;
        let acc_relief =
            0.0 * ((acc - 0.95) / 0.05).clamp(0.0, 1.0) * (combo_f / 4000.0).clamp(0.0, 1.0); // Strictened combo factor before it was too lenient in a cheat environment where accuracy is the primary measurement of skill.

        result += acc_relief;

        result.min(1.0)
    }
}

fn n50_effective_miss_multiplier(od: f64) -> f64 {
    if od <= 1.0 {
        1.0
    } else if od <= 7.0 {
        1.0 - (od - 1.0) / 6.0 * 0.5
    } else if od <= 10.0 {
        0.5 - (od - 7.0) / 3.0 * 0.25
    } else {
        (0.25 * (1.0 - (od - 10.0) / 3.33)).clamp(0.0, 0.25)
    }
}

#[cfg(test)]
mod tests {
    use super::OsuPerformanceCalculator;

    #[test]
    fn miss_combo_multiplier_leaves_fc_untouched() {
        assert_eq!(
            OsuPerformanceCalculator::miss_combo_multiplier(1000, 1000, 1.0),
            1.0
        );
    }

    #[test]
    fn earlier_relative_misses_are_harsher() {
        let early = OsuPerformanceCalculator::miss_combo_multiplier(1000, 64, 1.0);
        let late = OsuPerformanceCalculator::miss_combo_multiplier(1000, 900, 1.0);

        assert!(early < late);
    }

    #[test]
    fn no_misses_bypass_combo_scaling() {
        assert_eq!(
            OsuPerformanceCalculator::miss_combo_multiplier(1000, 64, 0.0),
            1.0
        );
    }

    #[test]
    fn length_bonus_rewards_accuracy() {
        assert!(
            OsuPerformanceCalculator::length_bonus(3000.0, 1.0)
                > OsuPerformanceCalculator::length_bonus(3000.0, 0.80)
        );
    }

    #[test]
    fn low_od_misses_are_harsher() {
        assert!(
            OsuPerformanceCalculator::low_od_miss_multiplier(3.0, 5.0)
                < OsuPerformanceCalculator::low_od_miss_multiplier(3.0, 8.0)
        );
    }
}
