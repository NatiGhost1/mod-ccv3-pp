use crate::{
    GameMods,
    taiko::{TaikoDifficultyAttributes, TaikoPerformanceAttributes, TaikoScoreState},
    util::{
        difficulty::{erf, erf_inv, logistic, reverse_lerp},
        float_ext::FloatExt,
    },
};

pub(super) struct TaikoPerformanceCalculator<'mods> {
    attrs: TaikoDifficultyAttributes,
    mods: &'mods GameMods,
    state: TaikoScoreState,
    is_classic: bool,
    // Provide vectors with note and miss timestamps (in milliseconds)
    object_timestamps: Vec<f64>,
    miss_timestamps: Vec<f64>,
}

impl<'a> TaikoPerformanceCalculator<'a> {
    pub const fn new(
        attrs: TaikoDifficultyAttributes,
        mods: &'a GameMods,
        state: TaikoScoreState,
        is_classic: bool,
    ) -> Self {
        Self {
            attrs,
            mods,
            state,
            is_classic,
            object_timestamps: Vec::new(),
            miss_timestamps: Vec::new(),
        }
    }

    pub fn with_timestamps(
        mut self,
        object_timestamps: Vec<f64>,
        miss_timestamps: Vec<f64>,
    ) -> Self {
        self.object_timestamps = object_timestamps;
        self.miss_timestamps = miss_timestamps;
        self
    }
}

impl TaikoPerformanceCalculator<'_> {
    pub fn calculate(self) -> TaikoPerformanceAttributes {
        let estimated_unstable_rate =
            if self.state.hitresults.n300 == 0 || self.attrs.great_hit_window <= 0.0 {
                None
            } else {
                Some(
                    self.compute_deviation_upper_bound(
                        f64::from(self.state.hitresults.n300) / self.total_hits(),
                    ) * 10.0,
                )
            };

        let total_difficult_hits = self.total_hits() * self.attrs.consistency_factor;

        let difficulty_value =
            self.compute_difficulty_value(total_difficult_hits, estimated_unstable_rate) * 1.08;
        let accuracy_value =
            self.compute_accuracy_value(total_difficult_hits, estimated_unstable_rate) * 1.1;

        let pp = difficulty_value + accuracy_value;

        TaikoPerformanceAttributes {
            difficulty: self.attrs,
            pp,
            pp_acc: accuracy_value,
            pp_difficulty: difficulty_value,
            estimated_unstable_rate,
        }
    }

    // Delta-Time Tapping Speed Analysis

    /// Converts a millisecond gap between notes into an effective rhythmic speed (BPM-ish scale)
    fn delta_to_effective_speed(&self, delta_ms: f64) -> f64 {
        if delta_ms <= 0.0 {
            return 0.0;
        }
        // Example: a 150ms gap at 1/4 streaming rate represents roughly 100,000 / 150 = 666ms per beat window.
        // Adjust the scaling factor (e.g., 15000.0) to line up cleanly with the desired 80.0 - 320.0 target range.
        let speed = 15000.0 / delta_ms;
        speed.clamp(80.0, 320.0)
    }

    /// Computes all consecutive object speed values across the map
    fn compute_all_effective_speeds(&self) -> Vec<f64> {
        if self.object_timestamps.len() < 2 {
            return vec![80.0]; // Fallback baseline
        }

        self.object_timestamps
            .windows(2)
            .map(|window| {
                let delta = window[1] - window[0];
                self.delta_to_effective_speed(delta)
            })
            .collect()
    }

    /// Replaces estimate_average_effective_bpm using real delta context
    fn estimate_average_effective_speed(&self) -> f64 {
        // Best available proxy using difficulty attributes
        // mono_stamina_factor correlates strongly with required tapping speed
        // rough conversion to BPM-ish scale
        let speeds = self.compute_all_effective_speeds();
        if speeds.is_empty() {
            return 80.0;
        }
        let total: f64 = speeds.iter().sum();
        total / speeds.len() as f64
    }

    /// Replaces estimate_peak_tapping_bpm using real delta context
    fn estimate_peak_effective_speed(&self) -> f64 {
        // Assume peak is noticeably higher than average
        // og = 1.35 but i think that could be too high 
        let speeds = self.compute_all_effective_speeds();
        // Return the highest historical speed reached in the map configuration
        speeds.into_iter().fold(80.0, f64::max)
    }

    /// Extracts the absolute lowest effective speed valley found across the map layout
    fn lowest_effective_speed(&self) -> f64 {
        let speeds = self.compute_all_effective_speeds();
        speeds.into_iter().fold(320.0, f64::min)
    }

    /// Evaluates if a given miss timestamp happened during a significantly slow section
    fn is_miss_on_slow_section(&self, miss_time: f64) -> bool {
        if self.object_timestamps.len() < 2 {
            return false;
        }

        // Find the object gap closest to when the player missed
        let mut closest_speed = 80.0;
        let mut min_diff = f64::MAX;

        for window in self.object_timestamps.windows(2) {
            let mid_point = (window[0] + window[1]) / 2.0;
            let diff = (miss_time - mid_point).abs();
            if diff < min_diff {
                min_diff = diff;
                let delta = window[1] - window[0];
                closest_speed = self.delta_to_effective_speed(delta);
            }
        }

        let avg_speed = self.estimate_average_effective_speed();
        let lowest_speed = self.lowest_effective_speed();

        // - Map has relatively low average tapping speed (more likely to have slower sections/breaks)
        // - Significantly slower than peak speed maps
        // The section is considered a "slow break valley" if it sits near your absolute minimum boundary,
        // or drops drastically below the overall map average.
        closest_speed <= lowest_speed * 1.15 || closest_speed < (avg_speed * 0.7)
    }

    fn compute_difficulty_value(
        &self,
        total_difficult_hits: f64,
        estimated_unstable_rate: Option<f64>,
    ) -> f64 {
        let Some(estimated_unstable_rate) = estimated_unstable_rate else {
            return 0.0;
        };

        if FloatExt::eq(total_difficult_hits, 0.0) {
            return 0.0;
        }

        let attrs = &self.attrs;
        let rhythm_expected_unstable_rate = self.compute_deviation_upper_bound(1.0) * 10.0;
        let rhythm_maximum_unstable_rate = self.compute_deviation_upper_bound(0.8) * 10.0;
        let rhythm_factor = reverse_lerp(attrs.rhythm / attrs.stars, 0.15, 0.4);

        let rhythm_penalty = 1.0
            - logistic(
                estimated_unstable_rate,
                (rhythm_expected_unstable_rate + rhythm_maximum_unstable_rate) / 2.0,
                10.0 / (rhythm_maximum_unstable_rate - rhythm_expected_unstable_rate),
                Some(0.25 * f64::powf(rhythm_factor, 3.0)),
            );

        let base_difficulty = 5.0 * f64::max(1.0, attrs.stars * rhythm_penalty / 0.11) - 4.0;
        let mut difficulty_value = f64::min(
            f64::powf(base_difficulty, 3.0) / 69052.51,
            f64::powf(base_difficulty, 2.25) / 1250.0,
        );

        difficulty_value *= 1.0 + 0.10 * f64::max(0.0, self.attrs.stars - 10.0);
        let length_bonus = 1.0 + 0.25 * total_difficult_hits / (total_difficult_hits + 4000.0);
        difficulty_value *= length_bonus;

        // Dynamic Miss Penalty Application
        // Unlucky miss on slower section
        let misses = self.state.hitresults.misses as usize;
        let accuracy = f64::from(self.state.hitresults.n300) / self.total_hits();

        // Standard penalty baseline based on map composition length
        let standard_penalty = 0.97 + 0.03 * total_difficult_hits / (total_difficult_hits + 1500.0);

        // Map over each individual miss timestamp. 
        // Allows high-performing play profiles to withstand isolated, accidental combo drops on structural valleys.
        for i in 0..misses {
            // Check if we have tracking data for this specific miss occurrence
            if let Some(&miss_time) = self.miss_timestamps.get(i) {
                // - Few misses
                // - High overall accuracy (good player)
                // If a high accuracy player drops combo on a slow map sequence, shield them from major losses
                if accuracy >= 0.94 && self.is_miss_on_slow_section(miss_time) && misses <= 3 {
                    difficulty_value *= 0.9995; // Extremely soft penalty — unlucky miss barely hurts PP Soft penalty for unlucky break misfires
                } else {
                    difficulty_value *= standard_penalty; // Normal mechanical skill drop penalty
                }
            } else {
                // Fallback penalty calculation if missing granular timing properties
                difficulty_value *= standard_penalty;
            }
        }

        if self.mods.hd() {
            let mut hidden_bonus = if self.attrs.is_convert { 0.025 } else { 0.1 };
            if !self.mods.fl() {
                if !self.is_classic {
                    hidden_bonus *= 0.2;
                }
                if self.mods.ez() && self.is_classic {
                    hidden_bonus *= 0.5;
                }
            }
            difficulty_value *= 1.0 + hidden_bonus;
        }

        if self.mods.fl() {
            difficulty_value *= f64::max(
                1.0,
                1.05 - f64::min(self.attrs.mono_stamina_factor / 50.0, 1.0) * length_bonus,
            );
        }

        let mono_acc_scaling_exponent = f64::from(2) + self.attrs.mono_stamina_factor;
        let mono_acc_scaling_shift =
            f64::from(500) - f64::from(100) * (self.attrs.mono_stamina_factor * f64::from(3));

        difficulty_value
            * (erf(mono_acc_scaling_shift / (f64::sqrt(2.0) * estimated_unstable_rate)))
                .powf(mono_acc_scaling_exponent)
    }

    // compute_accuracy_value, compute_deviation_upper_bound, and total_hits remain unchanged
    fn compute_accuracy_value(
        &self,
        total_difficult_hits: f64,
        estimated_unstable_rate: Option<f64>,
    ) -> f64 {
        let Some(estimated_unstable_rate) = estimated_unstable_rate else {
            return 0.0;
        };

        if self.attrs.great_hit_window <= 0.0 {
            return 0.0;
        }

        let mut accuracy_value = 470.0 * f64::powf(0.9885, estimated_unstable_rate);

        accuracy_value *= 1.0
            + f64::powf(50.0 / estimated_unstable_rate, 2.0) * f64::powf(self.attrs.stars, 2.8)
                / 600.0;

        if self.mods.hd() && !self.attrs.is_convert {
            accuracy_value *= 1.075;
        }

        accuracy_value *= 1.0 + 0.3 * total_difficult_hits / (total_difficult_hits + 4000.0);
        let memory_length_bonus = f64::min(1.15, f64::powf(self.total_hits() / 1500.0, 0.3));

        if self.mods.fl() && self.mods.hd() && !self.attrs.is_convert {
            accuracy_value *= f64::max(1.0, 1.05 * memory_length_bonus);
        }

        accuracy_value
    }

    fn compute_deviation_upper_bound(&self, accuracy: f64) -> f64 {
        #[expect(clippy::unreadable_literal, reason = "staying in-sync with lazer")]
        const Z: f64 = 2.32634787404;
        let n = self.total_hits();
        let p = accuracy;

        let p_lower_bound = (n * p + Z * Z / 2.0) / (n + Z * Z)
            - Z / (n + Z * Z) * f64::sqrt(n * p * (1.0 - p) + Z * Z / 4.0);

        self.attrs.great_hit_window / (f64::sqrt(2.0) * erf_inv(p_lower_bound))
    }

    const fn total_hits(&self) -> f64 {
        self.state.hitresults.total_hits() as f64
    }
}