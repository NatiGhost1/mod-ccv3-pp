use crate::osu::difficulty::object::OsuDifficultyObject;

/// Estimates the extra memorization burden created by Flashlight.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    object_count: usize,
    hidden: bool,
    pattern_complexity: f64,
    previous_delta_time: Option<f64>,
    previous_position: Option<rosu_map::util::Pos>,
}

impl Memory {
    pub fn new(hidden: bool) -> Self {
        Self {
            object_count: 0,
            hidden,
            pattern_complexity: 0.0,
            previous_delta_time: None,
            previous_position: None,
        }
    }

    pub fn process(
        &mut self,
        curr: &OsuDifficultyObject<'_>,
        _objects: &[OsuDifficultyObject<'_>],
    ) {
        let position = curr.base.stacked_pos();

        if let (Some(previous_delta_time), Some(previous_position)) =
            (self.previous_delta_time, self.previous_position)
        {
            let rhythm_change = (curr.delta_time.max(1.0) / previous_delta_time.max(1.0))
                .ln()
                .abs()
                .clamp(0.0, 2.0)
                / 2.0;
            let movement = f64::from((position - previous_position).length());
            let spatial_change = (movement / (movement + 80.0)).clamp(0.0, 1.0);

            self.pattern_complexity += 0.6 * rhythm_change + 0.4 * spatial_change;
        }

        self.previous_delta_time = Some(curr.delta_time);
        self.previous_position = Some(position);
        self.object_count += 1;
    }

    pub fn difficulty(self) -> f64 {
        if self.object_count < 100 {
            return 0.0;
        }

        let length = ((self.object_count as f64 - 100.0) / 1900.0).clamp(0.0, 1.0);
        let hidden_multiplier = Self::hidden_multiplier(self.hidden);
        let pattern_multiplier = 1.0
            + 0.35
                * (self.pattern_complexity / (self.object_count.saturating_sub(1) as f64))
                    .clamp(0.0, 1.0);

        length.powf(1.35) * hidden_multiplier * pattern_multiplier
    }

    const fn hidden_multiplier(hidden: bool) -> f64 {
        if hidden {
            1.65
        } else {
            1.0
        }
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        25.0 * difficulty.powf(2.0)
    }

    pub fn performance_value(
        difficulty: f64,
        accuracy: f64,
        combo: f64,
        max_combo: f64,
        misses: f64,
        total_hits: f64,
    ) -> f64 {
        let base = Self::difficulty_to_performance(difficulty);
        let accuracy_factor = (0.4 + 0.6 * accuracy.clamp(0.0, 1.0)).powf(1.5);
        let combo_factor = (0.5 + 0.5 * (combo / max_combo.max(1.0)).clamp(0.0, 1.0)).powf(0.8);
        let miss_factor = if misses > 0.0 {
            0.97 * (1.0 - (misses / total_hits.max(1.0)).clamp(0.0, 1.0)).powf(0.8)
        } else {
            1.0
        };

        base * accuracy_factor * combo_factor * miss_factor
    }
}

#[cfg(test)]
mod tests {
    use super::Memory;

    #[test]
    fn short_maps_have_almost_no_memory_value() {
        assert_eq!(Memory::new(false).difficulty(), 0.0);
    }

    #[test]
    fn hidden_long_maps_have_more_memory_value() {
        assert!(Memory::hidden_multiplier(true) > Memory::hidden_multiplier(false));
    }

    #[test]
    fn memory_performance_scales_with_play_quality() {
        let perfect = Memory::performance_value(1.0, 1.0, 1000.0, 1000.0, 0.0, 1000.0);
        let imperfect = Memory::performance_value(1.0, 0.95, 800.0, 1000.0, 2.0, 1000.0);

        assert!(perfect > imperfect);
    }
}
