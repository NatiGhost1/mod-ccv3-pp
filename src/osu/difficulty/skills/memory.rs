use crate::osu::difficulty::object::OsuDifficultyObject;

/// Estimates the extra memorization burden created by Flashlight.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    object_count: usize,
    hidden: bool,
}

impl Memory {
    pub fn new(hidden: bool) -> Self {
        Self {
            object_count: 0,
            hidden,
        }
    }

    pub fn process(
        &mut self,
        _curr: &OsuDifficultyObject<'_>,
        _objects: &[OsuDifficultyObject<'_>],
    ) {
        self.object_count += 1;
    }

    pub fn difficulty(self) -> f64 {
        if self.object_count < 100 {
            return 0.0;
        }

        let length = ((self.object_count as f64 - 100.0) / 1900.0).clamp(0.0, 1.0);
        let hidden_multiplier = Self::hidden_multiplier(self.hidden);

        length.powf(1.35) * hidden_multiplier
    }

    const fn hidden_multiplier(hidden: bool) -> f64 {
        if hidden { 1.65 } else { 1.0 }
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        25.0 * difficulty.powf(2.0)
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
}
