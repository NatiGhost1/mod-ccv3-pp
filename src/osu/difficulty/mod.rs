use std::{cmp, pin::Pin};

use rosu_map::section::general::GameMode;
use skills::{aim::Aim, flashlight::Flashlight, speed::Speed, strain::OsuStrainSkill};

use crate::{
    Beatmap,
    any::{
        CalculateError,
        difficulty::{Difficulty, skills::StrainSkill},
    },
    model::{beatmap::BeatmapAttributes, mode::ConvertError, mods::GameMods},
    osu::{
        convert::{convert_objects, prepare_map},
        difficulty::{
            object::OsuDifficultyObject, rating::OsuRatingCalculator,
            scaling_factor::ScalingFactor, skills::strain::count_top_weighted_sliders,
        },
        legacy_score_simulator::OsuLegacyScoreSimulator,
        object::OsuObject,
        performance::PERFORMANCE_BASE_MULTIPLIER,
        utils::legacy_score::NestedScorePerObject,
    },
};

use self::skills::OsuSkills;

use super::attributes::OsuDifficultyAttributes;

pub mod evaluators;
pub mod gradual;
pub mod object;
pub mod rating;
pub mod scaling_factor;
pub mod skills;

// CC V3 modules
pub mod tap_bpm;
pub mod speed_precal;

const STAR_RATING_MULTIPLIER: f64 = 0.0265;

const HD_FADE_IN_DURATION_MULTIPLIER: f64 = 0.4;
const HD_FADE_OUT_DURATION_MULTIPLIER: f64 = 0.3;

pub fn difficulty(
    difficulty: &Difficulty,
    map: &Beatmap,
) -> Result<OsuDifficultyAttributes, ConvertError> {
    let map = prepare_map(difficulty, map)?;

    Ok(calculate_difficulty(difficulty, &map))
}

pub fn checked_difficulty(
    difficulty: &Difficulty,
    map: &Beatmap,
) -> Result<OsuDifficultyAttributes, CalculateError> {
    let map = prepare_map(difficulty, map)?;
    map.check_suspicion()?;

    Ok(calculate_difficulty(difficulty, &map))
}

fn calculate_difficulty(difficulty: &Difficulty, map: &Beatmap) -> OsuDifficultyAttributes {
    debug_assert_eq!(map.mode, GameMode::Osu);

    let DifficultyValues {
        osu_objects,
        skills,
        mut attrs,
    } = DifficultyValues::calculate(difficulty, map);

    let mods = difficulty.get_mods();
    let passed_objects = difficulty.get_passed_objects();

    DifficultyValues::eval(&mut attrs, mods, &skills);

    let mut simulator = OsuLegacyScoreSimulator::new(&osu_objects, map, passed_objects);

    let score_attrs = simulator.simulate();
    attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;

    let map_attrs = map.attributes().difficulty(difficulty).build();

    attrs.legacy_score_base_multiplier = f64::from(OsuLegacyScoreSimulator::score_multiplier(
        map,
        &map_attrs,
        passed_objects,
    ));

    let slider_nested_score_per_object =
        NestedScorePerObject::calculate(&osu_objects, passed_objects);
    attrs.nested_score_per_object = slider_nested_score_per_object;

    // ═══ CC V3 post-processing ══════════════════════════════════════
    // Store CS for the performance pass nerfs.
    attrs.cs = f64::from(map_attrs.cs());

    attrs
}

pub struct OsuDifficultySetup {
    scaling_factor: ScalingFactor,
    map_attrs: BeatmapAttributes,
    attrs: OsuDifficultyAttributes,
    time_preempt: f64,
}

impl OsuDifficultySetup {
    pub fn new(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let clock_rate = difficulty.get_clock_rate();
        let map_attrs = map.attributes().difficulty(difficulty).build();
        let hit_windows = map_attrs.hit_windows();
        let scaling_factor = ScalingFactor::new(map_attrs.cs());

        let attrs = OsuDifficultyAttributes {
            ar: map_attrs.apply_clock_rate().ar,
            hp: f64::from(map_attrs.hp()),
            great_hit_window: hit_windows.od_great.unwrap_or(0.0),
            ok_hit_window: hit_windows.od_ok.unwrap_or(0.0),
            meh_hit_window: hit_windows.od_meh.unwrap_or(0.0),
            ..Default::default()
        };

        let time_preempt = f64::from((hit_windows.ar.unwrap_or(0.0) * clock_rate) as f32);

        Self {
            scaling_factor,
            map_attrs,
            attrs,
            time_preempt,
        }
    }
}

pub struct DifficultyValues {
    pub osu_objects: Box<[OsuObject]>,
    pub skills: OsuSkills,
    pub attrs: OsuDifficultyAttributes,
}

impl DifficultyValues {
    pub fn calculate(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let mods = difficulty.get_mods();
        let take = difficulty.get_passed_objects();

        let OsuDifficultySetup {
            scaling_factor,
            map_attrs,
            mut attrs,
            time_preempt,
        } = OsuDifficultySetup::new(difficulty, map);

        let mut osu_objects = convert_objects(
            map,
            &scaling_factor,
            mods.reflection(),
            time_preempt,
            take,
            &mut attrs,
        );

        let osu_object_iter = osu_objects.iter_mut().map(Pin::new);

        let diff_objects =
            Self::create_difficulty_objects(difficulty, &scaling_factor, osu_object_iter);

        let great_hit_window = map_attrs.hit_windows().od_great.unwrap_or(0.0);

        let mut skills = OsuSkills::new(mods, &scaling_factor, great_hit_window, time_preempt);

        // The first hit object has no difficulty object
        let take_diff_objects = cmp::min(map.hit_objects.len(), take).saturating_sub(1);

        for hit_object in diff_objects.iter().take(take_diff_objects) {
            skills.process(hit_object, &diff_objects);
        }

        // ═══ CC V3: extract data from diff_objects before they drop ═
        // Build SpeedObjectData for tap_bpm and speed_precal.
        // Also extract per-object speed strains from the speed skill.
        let _clock_rate = difficulty.get_clock_rate();

        let speed_object_data: Vec<tap_bpm::SpeedObjectData> = diff_objects
            .iter()
            .map(|obj| tap_bpm::SpeedObjectData {
                delta_time: obj.delta_time,
                pos_x: obj.base.stacked_pos().x,
                pos_y: obj.base.stacked_pos().y,
            })
            .collect();

        // Object strains from the speed skill (for tap_bpm top-10% filtering)
        let object_strains: Vec<f64> = skills.speed.object_strains().to_vec();

        // Strain peaks for local_sr_per_minute (marathon decay)
        let aim_peaks: Vec<f64> = skills.aim.clone().into_current_strain_peaks();
        let speed_peaks: Vec<f64> = skills.speed.clone().into_current_strain_peaks();

        // Compute dominant_tap_bpm
        if !object_strains.is_empty() && !speed_object_data.is_empty() {
            attrs.dominant_tap_bpm =
                tap_bpm::dominant_tap_bpm_from_owned(&object_strains, &speed_object_data, 0.10);
        }

        // Compute speed rework multipliers
        let (vanilla_mult, autopilot_mult) =
            speed_precal::precompute_speed_rework_from_owned(&speed_object_data, attrs.dominant_tap_bpm);
        attrs.speed_rework_mult_vanilla = vanilla_mult;
        attrs.speed_rework_mult_autopilot = autopilot_mult;

        // Compute local_sr_per_minute for marathon decay
        attrs.local_sr_per_minute = crate::osu::performance::relax_marathon::local_sr_per_minute(
            &aim_peaks,
            &speed_peaks,
        );

        // Compute AP-only speed/rhythm local SR for autopilot marathon decay.
        attrs.local_autopilot_sr_per_minute =
            crate::osu::performance::auto_marathon::local_sr_per_minute(&speed_peaks);

        // Compute AP-only aim intensity per minute used only to classify
        // low-BPM, high-aim sections that should not be treated like marathons.
        attrs.local_aim_per_minute =
            crate::osu::performance::auto_marathon::local_aim_per_minute(&aim_peaks);

        // Compute local_bpm_per_minute for autopilot marathon decay
        let delta_times: Vec<f64> = diff_objects.iter().map(|obj| obj.adjusted_delta_time).collect();
        attrs.local_bpm_per_minute = crate::osu::performance::auto_marathon::compute_local_bpm_per_minute(&diff_objects, &delta_times);

        // Compute avg_jump_dist and median_delta_time
        let mut dist_sum = 0.0;
        let mut dist_count = 0u32;
        for obj in &diff_objects {
            if obj.lazy_jump_dist > 0.0 {
                dist_sum += obj.lazy_jump_dist;
                dist_count += 1;
            }
        }
        attrs.avg_jump_dist = if dist_count > 0 {
            dist_sum / f64::from(dist_count)
        } else {
            0.0
        };

        // Median delta_time
        let mut deltas: Vec<f64> = speed_object_data
            .iter()
            .map(|o| o.delta_time)
            .filter(|d| *d > 0.0)
            .collect();
        if !deltas.is_empty() {
            deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = deltas.len() / 2;
            attrs.median_delta_time = if deltas.len() % 2 == 1 {
                deltas[m]
            } else {
                (deltas[m - 1] + deltas[m]) / 2.0
            };
        }

        // Compute rx_chunk_hardness and rx_chunk_avg_delta
        let mut hardness_chunks: Vec<f64> = Vec::new();
        let mut avg_delta_chunks: Vec<f64> = Vec::new();
        let mut chunk_hardness = 0.0;
        let mut chunk_delta_sum = 0.0;
        let mut chunk_count: u32 = 0;
        for obj in &speed_object_data {
            if obj.delta_time > 0.0 {
                chunk_hardness += 1.0 / obj.delta_time;
                chunk_delta_sum += obj.delta_time;
                chunk_count += 1;
                if chunk_count == 4 {
                    hardness_chunks.push(chunk_hardness);
                    avg_delta_chunks.push(chunk_delta_sum / 4.0);
                    chunk_hardness = 0.0;
                    chunk_delta_sum = 0.0;
                    chunk_count = 0;
                }
            }
        }
        if chunk_count > 0 {
            hardness_chunks.push(chunk_hardness);
            avg_delta_chunks.push(chunk_delta_sum / chunk_count as f64);
        }
        attrs.rx_chunk_hardness = hardness_chunks;
        attrs.rx_chunk_avg_delta = avg_delta_chunks;

        // ═══ End CC V3 post-processing ══════════════════════════════

        Self {
            osu_objects,
            skills,
            attrs,
        }
    }

    /// Process the difficulty values and store the results in `attrs`.
    pub fn eval(attrs: &mut OsuDifficultyAttributes, mods: &GameMods, skills: &OsuSkills) {
        let OsuSkills {
            aim,
            aim_no_sliders,
            speed,
            flashlight,
        } = skills;

        let aim_difficulty_value = aim.cloned_difficulty_value();

        let aim_difficult_strain_count = aim.count_top_weighted_strains(aim_difficulty_value);

        let difficult_sliders = aim.get_difficult_sliders();

        let aim_no_sliders_difficulty_value = aim_no_sliders.cloned_difficulty_value();

        let aim_no_sliders_top_weighted_slider_count = count_top_weighted_sliders(
            aim_no_sliders.slider_strains(),
            aim_no_sliders_difficulty_value,
        );

        let aim_no_sliders_difficult_strain_count =
            aim_no_sliders.count_top_weighted_strains(aim_no_sliders_difficulty_value);

        let aim_top_weighted_slider_factor = aim_no_sliders_top_weighted_slider_count
            / (aim_no_sliders_difficult_strain_count - aim_no_sliders_top_weighted_slider_count)
                .max(1.0);

        let slider_factor = if aim_difficulty_value > 0.0 {
            OsuRatingCalculator::calculate_difficulty_rating(aim_no_sliders_difficulty_value)
                / OsuRatingCalculator::calculate_difficulty_rating(aim_difficulty_value)
        } else {
            1.0
        };

        let speed_difficulty_value = speed.cloned_difficulty_value();
        let speed_top_weighted_slider_count =
            count_top_weighted_sliders(speed.slider_strains(), speed_difficulty_value);

        let speed_difficult_strain_count = speed.count_top_weighted_strains(speed_difficulty_value);

        let speed_top_weighted_slider_factor = speed_top_weighted_slider_count
            / (speed_difficult_strain_count - speed_top_weighted_slider_count).max(1.0);

        let mechanical_difficulty_rating =
            calculate_mechanical_difficulty_rating(aim_difficulty_value, speed_difficulty_value);

        let osu_rating_calculator = OsuRatingCalculator::new(
            mods,
            attrs.n_objects(),
            attrs.ar,
            attrs.od(),
            mechanical_difficulty_rating,
            slider_factor,
        );

        let aim_rating = osu_rating_calculator.compute_aim_rating(aim_difficulty_value);
        let speed_rating = osu_rating_calculator.compute_speed_rating(speed_difficulty_value);

        let flashlight_rating = if mods.fl() {
            let flashlight_difficulty_value = flashlight.cloned_difficulty_value();

            osu_rating_calculator.compute_flashlight_rating(flashlight_difficulty_value)
        } else {
            0.0
        };

        let base_aim_performance = Aim::difficulty_to_performance(aim_rating);
        let base_speed_performance = Speed::difficulty_to_performance(speed_rating);
        let base_flashlight_performance = Flashlight::difficulty_to_performance(flashlight_rating);

        let base_performance = ((base_aim_performance).powf(1.1)
            + (base_speed_performance).powf(1.1)
            + (base_flashlight_performance).powf(1.1))
        .powf(1.0 / 1.1);

        let star_rating = calculate_star_rating(base_performance);

        attrs.aim = aim_rating;
        attrs.aim_difficult_slider_count = difficult_sliders;
        attrs.speed = speed_rating;
        attrs.flashlight = flashlight_rating;
        attrs.slider_factor = slider_factor;
        attrs.aim_top_weighted_slider_factor = aim_top_weighted_slider_factor;
        attrs.speed_top_weighted_slider_factor = speed_top_weighted_slider_factor;
        attrs.aim_difficult_strain_count = aim_difficult_strain_count;
        attrs.speed_difficult_strain_count = speed_difficult_strain_count;
        attrs.stars = star_rating;
        attrs.speed_note_count = speed.relevant_note_count();
    }

    pub fn create_difficulty_objects<'a>(
        difficulty: &Difficulty,
        scaling_factor: &ScalingFactor,
        osu_objects: impl ExactSizeIterator<Item = Pin<&'a mut OsuObject>>,
    ) -> Vec<OsuDifficultyObject<'a>> {
        let take = difficulty.get_passed_objects();
        let clock_rate = difficulty.get_clock_rate();

        let mut osu_objects_iter = osu_objects.map(Pin::into_ref);

        let Some(mut last) = osu_objects_iter.next().filter(|_| take > 0) else {
            return Vec::new();
        };

        let mut diff_objects = Vec::with_capacity(osu_objects_iter.len());

        for (idx, h) in osu_objects_iter.enumerate() {
            let last_diff = if idx > 0 {
                diff_objects.get(idx - 1)
            } else {
                None
            };

            let last_last_diff = if idx > 1 {
                diff_objects.get(idx - 2)
            } else {
                None
            };

            let diff_object = OsuDifficultyObject::new(
                h.get_ref(),
                last.get_ref(),
                last_diff,
                last_last_diff,
                clock_rate,
                idx,
                scaling_factor,
            );

            last = h;

            diff_objects.push(diff_object);
        }

        diff_objects
    }
}

fn calculate_mechanical_difficulty_rating(
    aim_difficulty_value: f64,
    speed_difficulty_value: f64,
) -> f64 {
    let aim_value = Aim::difficulty_to_performance(
        OsuRatingCalculator::calculate_difficulty_rating(aim_difficulty_value),
    );
    let speed_value = Speed::difficulty_to_performance(
        OsuRatingCalculator::calculate_difficulty_rating(speed_difficulty_value),
    );

    let total_value = (aim_value.powf(1.1) + speed_value.powf(1.1)).powf(1.0 / 1.1);

    calculate_star_rating(total_value)
}

fn calculate_star_rating(base_performance: f64) -> f64 {
    if base_performance <= 0.00001 {
        return 0.0;
    }

    PERFORMANCE_BASE_MULTIPLIER.cbrt()
        * STAR_RATING_MULTIPLIER
        * ((100_000.0 / 2.0_f64.powf(1.0 / 1.1) * base_performance).cbrt() + 4.0)
}