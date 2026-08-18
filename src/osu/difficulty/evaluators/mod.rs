pub use self::{
    aim::AimEvaluator, aim_rx::AimRxEvaluator, flashlight::FlashlightEvaluator,
    rhythm::RhythmEvaluator, speed::SpeedEvaluator,
};

mod aim;
pub mod aim_rx;
mod flashlight;
mod rhythm;
mod speed;