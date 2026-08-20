use crate::osu::difficulty::object::OsuDifficultyObject;

/// Measures how strongly a map should punish unstable timing under Relax.
///
/// Slower, tighter patterns are more consistency-sensitive. Large jumps with
/// long gaps are less strict because their timing is naturally harder to keep
/// uniform without being a sign of poor consistency.
#[derive(Clone, Debug, Default)]
pub struct Consistency {
    strictness_sum: f64,
    object_count: usize,
}

impl Consistency {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process(
        &mut self,
        curr: &OsuDifficultyObject<'_>,
        _objects: &[OsuDifficultyObject<'_>],
    ) {
        if curr.delta_time <= 0.0 {
            return;
        }

        self.strictness_sum += Self::object_strictness(curr.delta_time, curr.lazy_jump_dist);
        self.object_count += 1;
    }

    fn object_strictness(delta_time: f64, jump_dist: f64) -> f64 {
        let slow_factor = ((delta_time - 100.0) / 200.0).clamp(0.0, 1.0);
        let jump_relief = ((jump_dist - 100.0) / 120.0).clamp(0.0, 1.0)
            * ((delta_time - 150.0) / 150.0).clamp(0.0, 1.0);

        (0.35 + 0.65 * slow_factor) * (1.0 - 0.65 * jump_relief)
    }

    pub fn strictness(self) -> f64 {
        if self.object_count == 0 {
            0.5
        } else {
            (self.strictness_sum / self.object_count as f64).clamp(0.0, 1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Consistency;

    #[test]
    fn slower_patterns_are_more_strict() {
        let fast = Consistency::object_strictness(80.0, 40.0);
        let slow = Consistency::object_strictness(240.0, 40.0);

        assert!(slow > fast);
    }

    #[test]
    fn long_jumps_are_less_strict() {
        let tight = Consistency::object_strictness(240.0, 40.0);
        let jump = Consistency::object_strictness(240.0, 220.0);

        assert!(jump < tight);
    }
}
