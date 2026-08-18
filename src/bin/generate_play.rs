use std::{env, fmt::Display, path::PathBuf, process::exit, time::SystemTime};

use rosu_mods::GameModsLegacy;
use ccv3_pp::{Beatmap, Difficulty, GameMods, Performance};

const DEFAULT_PLAYS: usize = 5;

#[derive(Default)]
struct SpecificPlay {
    mods: Option<(String, GameModsLegacy)>,
    combo: Option<u32>,
    misses: Option<u32>,
    accuracy: Option<f64>,
}

impl SpecificPlay {
    fn is_empty(&self) -> bool {
        self.mods.is_none() && self.combo.is_none() && self.misses.is_none() && self.accuracy.is_none()
    }
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} <beatmap.osu> [plays] [seed] [options]");
    eprintln!("Options:");
    eprintln!("  --mods <mods>       Specify mods like HDDT, HR, EZHD, RX, AP, or NoMod");
    eprintln!("  --combo <combo>     Specify play combo");
    eprintln!("  --misses <misses>   Specify play misses");
    eprintln!("  --accuracy <acc>    Specify play accuracy in %");
    eprintln!("  --plays <plays>     Number of plays to generate");
    eprintln!("  --seed <seed>       Seed for random generation of unspecified fields");
    eprintln!("  -h, --help          Show this help message");
    eprintln!("Example: {program} ./resources/5553026.osu 1 12345 --mods HDDT --misses 150 --accuracy 95.42");
}

struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, lower: u32, upper: u32) -> u32 {
        if lower >= upper {
            lower
        } else {
            let range = (upper - lower) as u64;
            let value = self.next_u64();
            lower + (value % range) as u32
        }
    }

    fn choose<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        let index = self.gen_range(0, values.len() as u32) as usize;
        &values[index]
    }

    fn choose_optional<'a, T>(&mut self, values: &'a [T]) -> Option<&'a T> {
        if self.gen_range(0, 2) == 0 {
            None
        } else {
            Some(self.choose(values))
        }
    }
}

#[derive(Clone, Copy)]
struct ModOption {
    name: &'static str,
    mod_value: GameModsLegacy,
}

impl Display for ModOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

const SPEED_MODS: &[ModOption] = &[
    ModOption {
        name: "NoMod",
        mod_value: GameModsLegacy::NoMod,
    },
    ModOption {
        name: "DoubleTime",
        mod_value: GameModsLegacy::DoubleTime,
    },
    ModOption {
        name: "Nightcore",
        mod_value: GameModsLegacy::Nightcore,
    },
    ModOption {
        name: "HalfTime",
        mod_value: GameModsLegacy::HalfTime,
    },
];

const DIFFICULTY_MODS: &[ModOption] = &[
    ModOption {
        name: "Easy",
        mod_value: GameModsLegacy::Easy,
    },
    ModOption {
        name: "HardRock",
        mod_value: GameModsLegacy::HardRock,
    },
];

const VISUAL_MODS: &[ModOption] = &[
    ModOption {
        name: "Hidden",
        mod_value: GameModsLegacy::Hidden,
    },
    ModOption {
        name: "Flashlight",
        mod_value: GameModsLegacy::Flashlight,
    },
    ModOption {
        name: "Mirror",
        mod_value: GameModsLegacy::Mirror,
    },
];

const UTILITY_MODS: &[ModOption] = &[
    ModOption {
        name: "NoFail",
        mod_value: GameModsLegacy::NoFail,
    },
    ModOption {
        name: "SpunOut",
        mod_value: GameModsLegacy::SpunOut,
    },
];

const SPECIAL_MODS: &[ModOption] = &[
    ModOption {
        name: "Relax",
        mod_value: GameModsLegacy::Relax,
    },
    ModOption {
        name: "Autopilot",
        mod_value: GameModsLegacy::Autopilot,
    },
];

const ENDURANCE_MODS: &[ModOption] = &[
    ModOption {
        name: "SuddenDeath",
        mod_value: GameModsLegacy::SuddenDeath,
    },
    ModOption {
        name: "Perfect",
        mod_value: GameModsLegacy::Perfect,
    },
];

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        exit(1);
    }

    let map_path = PathBuf::from(&args[1]);
    let mut plays = DEFAULT_PLAYS;
    let mut seed = None;
    let mut explicit = SpecificPlay::default();
    let mut positional = 0;

    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage(&args[0]);
                exit(0);
            }
            "--mods" | "-m" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --mods");
                    print_usage(&args[0]);
                    exit(1);
                });
                explicit.mods = Some(parse_mods(value).unwrap_or_else(|| {
                    eprintln!("Invalid mod string: {value}");
                    print_usage(&args[0]);
                    exit(1);
                }));
            }
            "--combo" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --combo");
                    print_usage(&args[0]);
                    exit(1);
                });
                explicit.combo = Some(value.parse().unwrap_or_else(|_| {
                    eprintln!("Invalid combo: {value}");
                    print_usage(&args[0]);
                    exit(1);
                }));
            }
            "--misses" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --misses");
                    print_usage(&args[0]);
                    exit(1);
                });
                explicit.misses = Some(value.parse().unwrap_or_else(|_| {
                    eprintln!("Invalid misses: {value}");
                    print_usage(&args[0]);
                    exit(1);
                }));
            }
            "--accuracy" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --accuracy");
                    print_usage(&args[0]);
                    exit(1);
                });
                explicit.accuracy = Some(parse_accuracy(value).unwrap_or_else(|| {
                    eprintln!("Invalid accuracy: {value}");
                    print_usage(&args[0]);
                    exit(1);
                }));
            }
            "--plays" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --plays");
                    print_usage(&args[0]);
                    exit(1);
                });
                plays = value.parse().unwrap_or_else(|_| {
                    eprintln!("Invalid plays: {value}");
                    print_usage(&args[0]);
                    exit(1);
                });
            }
            "--seed" => {
                let value = iter.next().unwrap_or_else(|| {
                    eprintln!("Missing value for --seed");
                    print_usage(&args[0]);
                    exit(1);
                });
                seed = Some(value.parse().unwrap_or_else(|_| {
                    eprintln!("Invalid seed: {value}");
                    print_usage(&args[0]);
                    exit(1);
                }));
            }
            _ if arg.starts_with('-') => {
                eprintln!("Unknown option: {arg}");
                print_usage(&args[0]);
                exit(1);
            }
            _ => {
                if positional == 0 {
                    plays = arg.parse().unwrap_or_else(|_| {
                        eprintln!("Invalid plays: {arg}");
                        print_usage(&args[0]);
                        exit(1);
                    });
                    positional += 1;
                } else if positional == 1 {
                    seed = Some(arg.parse().unwrap_or_else(|_| {
                        eprintln!("Invalid seed: {arg}");
                        print_usage(&args[0]);
                        exit(1);
                    }));
                    positional += 1;
                } else {
                    eprintln!("Unexpected argument: {arg}");
                    print_usage(&args[0]);
                    exit(1);
                }
            }
        }
    }

    let seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    });

    let mut rng = SimpleRng::new(seed);
    let map = match Beatmap::from_path(&map_path) {
        Ok(map) => map,
        Err(err) => {
            eprintln!("Failed to parse beatmap '{}': {err}", map_path.display());
            exit(1);
        }
    };

    println!("Beatmap: {}", map_path.display());
    println!("Mode: {:?}", map.mode);
    println!("BPM: {:.2}", map.bpm());
    println!("Total hitobjects: {}", map.hit_objects.len());
    println!("Seed: {seed}");

    if explicit.is_empty() {
        println!("Generating {plays} random plays...\n");
    } else {
        println!("Generating {plays} plays with explicit parameters...\n");
    }

    for index in 1..=plays {
        let (mod_list, mods_legacy) = if let Some((ref list, mods)) = explicit.mods {
            (list.clone(), mods)
        } else {
            build_random_mod_combo(&mut rng)
        };

        let mods = GameMods::from(mods_legacy);

        let diff_attrs = Difficulty::new().mods(mods.clone()).calculate(&map);
        let max_combo = diff_attrs.max_combo();
        let misses = explicit.misses.unwrap_or_else(|| rng.gen_range(0, (max_combo / 15).max(1) + 1));
        let combo = explicit.combo.unwrap_or_else(|| {
            if misses == 0 {
                max_combo
            } else {
                let lower_combo = (max_combo / 2).max(1);
                rng.gen_range(lower_combo, max_combo + 1)
            }
        });
        let accuracy = explicit.accuracy.unwrap_or_else(|| 90.0 + (rng.gen_range(0, 1001) as f64 / 100.0));

        let perf_attrs = Performance::new(diff_attrs)
            .mods(mods)
            .combo(combo)
            .misses(misses)
            .accuracy(accuracy)
            .calculate();

        println!("Play #{index}");
        println!("  Mods: {mod_list}");
        println!("  Stars: {:.2}", perf_attrs.stars());
        println!("  PP: {:.2}", perf_attrs.pp());
        println!("  Combo: {combo}/{max_combo}");
        println!("  Misses: {misses}");
        println!("  Accuracy: {accuracy:.2}%\n");
    }
}

fn build_random_mod_combo(rng: &mut SimpleRng) -> (String, GameModsLegacy) {
    let speed = rng.choose(SPEED_MODS);
    let difficulty = rng.choose_optional(DIFFICULTY_MODS);
    let visual = rng.choose_optional(VISUAL_MODS);
    let utility = rng.choose_optional(UTILITY_MODS);
    let special = rng.choose_optional(SPECIAL_MODS);
    let endurance = rng.choose_optional(ENDURANCE_MODS);

    let mut mods = speed.mod_value;
    let mut names = vec![speed.name.to_owned()];

    if let Some(difficulty) = difficulty {
        mods |= difficulty.mod_value;
        names.push(difficulty.name.to_owned());
    }

    if let Some(visual) = visual {
        mods |= visual.mod_value;
        names.push(visual.name.to_owned());
    }

    if let Some(utility) = utility {
        mods |= utility.mod_value;
        names.push(utility.name.to_owned());
    }

    if let Some(special) = special {
        if special.mod_value != GameModsLegacy::Autopilot || !mods.contains(GameModsLegacy::Relax) {
            mods |= special.mod_value;
            names.push(special.name.to_owned());
        }
    }

    if let Some(endurance) = endurance {
        if endurance.mod_value != GameModsLegacy::Perfect
            || !mods.contains(GameModsLegacy::SuddenDeath)
        {
            mods |= endurance.mod_value;
            names.push(endurance.name.to_owned());
        }
    }

    if names.len() == 1 && names[0] == "NoMod" {
        names[0] = "NoMod".to_owned();
    }

    let mod_list = if names.iter().all(|name| name == "NoMod") {
        "NoMod".to_string()
    } else {
        names
            .into_iter()
            .filter(|name| name != "NoMod")
            .collect::<Vec<String>>()
            .join(" + ")
    };

    (mod_list, mods)
}

fn parse_mods(input: &str) -> Option<(String, GameModsLegacy)> {
    let mut s = input.to_lowercase();
    s.retain(|c| !c.is_whitespace() && c != '+' && c != ',' && c != '/');

    if s.is_empty() {
        return None;
    }

    let mut mods = GameModsLegacy::NoMod;
    let mut names: Vec<String> = Vec::new();

    if s == "nomod" || s == "nm" {
        return Some(("NoMod".to_string(), mods));
    }

    while !s.is_empty() {
        let next = if let Some(rest) = s.strip_prefix("suddendeath") {
            mods |= GameModsLegacy::SuddenDeath;
            names.push("SuddenDeath".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("suddende") {
            mods |= GameModsLegacy::SuddenDeath;
            names.push("SuddenDeath".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("perfect") {
            mods |= GameModsLegacy::Perfect;
            names.push("Perfect".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("autopilot") {
            mods |= GameModsLegacy::Autopilot;
            names.push("Autopilot".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("flashlight") {
            mods |= GameModsLegacy::Flashlight;
            names.push("Flashlight".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("hardrock") {
            mods |= GameModsLegacy::HardRock;
            names.push("HardRock".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("doubletime") {
            mods |= GameModsLegacy::DoubleTime;
            names.push("DoubleTime".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("nightcore") {
            mods |= GameModsLegacy::Nightcore;
            names.push("Nightcore".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("halftime") {
            mods |= GameModsLegacy::HalfTime;
            names.push("HalfTime".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("nofail") {
            mods |= GameModsLegacy::NoFail;
            names.push("NoFail".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("spunout") {
            mods |= GameModsLegacy::SpunOut;
            names.push("SpunOut".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("mirror") {
            mods |= GameModsLegacy::Mirror;
            names.push("Mirror".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("relax") {
            mods |= GameModsLegacy::Relax;
            names.push("Relax".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("hidden") {
            mods |= GameModsLegacy::Hidden;
            names.push("Hidden".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("easy") {
            mods |= GameModsLegacy::Easy;
            names.push("Easy".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("hardrock") {
            mods |= GameModsLegacy::HardRock;
            names.push("HardRock".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("hr") {
            mods |= GameModsLegacy::HardRock;
            names.push("HardRock".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("hd") {
            mods |= GameModsLegacy::Hidden;
            names.push("Hidden".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("nf") {
            mods |= GameModsLegacy::NoFail;
            names.push("NoFail".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("ez") {
            mods |= GameModsLegacy::Easy;
            names.push("Easy".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("fl") {
            mods |= GameModsLegacy::Flashlight;
            names.push("Flashlight".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("so") {
            mods |= GameModsLegacy::SpunOut;
            names.push("SpunOut".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("nc") {
            mods |= GameModsLegacy::Nightcore;
            names.push("Nightcore".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("dt") {
            mods |= GameModsLegacy::DoubleTime;
            names.push("DoubleTime".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("ht") {
            mods |= GameModsLegacy::HalfTime;
            names.push("HalfTime".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("rx") {
            mods |= GameModsLegacy::Relax;
            names.push("Relax".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("ap") {
            mods |= GameModsLegacy::Autopilot;
            names.push("Autopilot".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("sd") {
            mods |= GameModsLegacy::SuddenDeath;
            names.push("SuddenDeath".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("pf") {
            mods |= GameModsLegacy::Perfect;
            names.push("Perfect".to_string());
            rest
        } else if let Some(rest) = s.strip_prefix("mr") {
            mods |= GameModsLegacy::Mirror;
            names.push("Mirror".to_string());
            rest
        } else {
            return None;
        };

        s = next.to_string();
    }

    let mod_list = if names.is_empty() {
        "NoMod".to_string()
    } else {
        names.into_iter().collect::<Vec<String>>().join(" + ")
    };

    Some((mod_list, mods))
}

fn parse_accuracy(value: &str) -> Option<f64> {
    let normalized = value.strip_suffix('%').unwrap_or(value);
    let acc = normalized.parse::<f64>().ok()?;
    if (0.0..=100.0).contains(&acc) {
        Some(acc)
    } else {
        None
    }
}
