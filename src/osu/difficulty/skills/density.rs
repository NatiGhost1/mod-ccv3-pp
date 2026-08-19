use crate::osu::difficulty::object::OsuDifficultyObject;

/// Estimates how much aim difficulty is affected by the local object density.
///
/// This is deliberately separate from aim strain: density is a map-level
/// modifier and must not make every object in a dense stream contribute more
/// strain just because it is close to another object.
#[derive(Clone, Debug, Default)]
pub struct Density {
    section_start: Option<f64>,
    object_count: usize,
    interval_sum: f64,
    distance_sum: f64,
    close_objects: usize,
    repeated_positions: usize,
    section_effects: Vec<(f64, usize)>,
}

impl Density {
    const SECTION_LENGTH: f64 = 400.0;
    const DIAMETER: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn process(
        &mut self,
        curr: &OsuDifficultyObject<'_>,
        _objects: &[OsuDifficultyObject<'_>],
    ) {
        let section_start = *self.section_start.get_or_insert_with(|| {
            (curr.start_time / Self::SECTION_LENGTH).floor() * Self::SECTION_LENGTH
        });

        if curr.start_time >= section_start + Self::SECTION_LENGTH {
            self.finish_section();
            self.section_start =
                Some((curr.start_time / Self::SECTION_LENGTH).floor() * Self::SECTION_LENGTH);
        }

        if curr.delta_time <= 0.0 {
            return;
        }

        self.object_count += 1;
        self.interval_sum += curr.delta_time;
        self.distance_sum += curr.lazy_jump_dist;

        if curr.lazy_jump_dist < Self::DIAMETER * 0.75 {
            self.close_objects += 1;
        }

        if curr.lazy_jump_dist <= Self::DIAMETER * 0.05 {
            self.repeated_positions += 1;
        }
    }

    fn finish_section(&mut self) {
        if self.object_count == 0 {
            return;
        }

        let average_interval = self.interval_sum / self.object_count as f64;
        let average_distance = self.distance_sum / self.object_count as f64;
        let effect = Self::section_effect(
            self.object_count,
            average_interval,
            average_distance,
            self.close_objects,
            self.repeated_positions,
        );

        self.section_effects.push((effect, self.object_count));
        self.object_count = 0;
        self.interval_sum = 0.0;
        self.distance_sum = 0.0;
        self.close_objects = 0;
        self.repeated_positions = 0;
    }

    fn section_effect(
        object_count: usize,
        average_interval: f64,
        average_distance: f64,
        close_objects: usize,
        repeated_positions: usize,
    ) -> f64 {
        let density = (1000.0 / average_interval).clamp(0.0, 20.0);
        let density_level = ((density - 2.0) / 8.0).clamp(0.0, 1.0);
        let speed_level = ((150.0 - average_interval) / 100.0).clamp(0.0, 1.0);
        let spacing_level = ((average_distance / Self::DIAMETER - 0.75) / 1.25).clamp(0.0, 1.0);
        let close_ratio = close_objects as f64 / object_count as f64;
        let repeated_ratio = repeated_positions as f64 / object_count as f64;

        let wide_bonus = spacing_level * (0.008 + 0.032 * density_level * speed_level);
        let close_penalty = close_ratio * density_level * 0.10;
        let repeated_penalty = repeated_ratio * 0.04;

        (wide_bonus - close_penalty - repeated_penalty).clamp(-0.10, 0.10)
    }

    pub fn multiplier(mut self) -> f64 {
        self.finish_section();

        let (effect_sum, object_count) = self.section_effects.into_iter().fold(
            (0.0, 0usize),
            |(sum, count), (effect, section_count)| {
                (sum + effect * section_count as f64, count + section_count)
            },
        );

        if object_count == 0 {
            1.0
        } else {
            (1.0 + effect_sum / object_count as f64).clamp(0.90, 1.10)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Density;

    #[test]
    fn dense_wide_sections_get_more_than_sparse_wide_sections() {
        let sparse_wide = Density::section_effect(8, 300.0, 150.0, 0, 0);
        let dense_wide = Density::section_effect(8, 75.0, 150.0, 0, 0);

        assert!(sparse_wide > 0.0);
        assert!(dense_wide > sparse_wide);
    }

    #[test]
    fn dense_close_sections_are_reduced() {
        let effect = Density::section_effect(8, 75.0, 20.0, 8, 0);

        assert!(effect < 0.0);
    }

    #[test]
    fn repeated_positions_never_receive_a_bonus() {
        let effect = Density::section_effect(8, 300.0, 0.0, 8, 8);

        assert!(effect <= 0.0);
    }
}
