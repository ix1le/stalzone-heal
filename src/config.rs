use std::fs;
use std::path::PathBuf;

pub struct Config {
    pub key_text: String,
    pub toggle_text: String,
    pub percent: u32,
    pub cooldown_ms: u64,
    pub dry_run: bool,
    pub fast_input: bool,
    pub confirm_frames: u32,
    pub confirm_ms: u64,
    pub learn_ref: bool,
    pub gap_divisor: u32,
    pub skip_suspect: bool,
    pub debug_heal: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            key_text: "c".into(),
            toggle_text: "f1".into(),
            percent: 75,
            cooldown_ms: 3000,
            dry_run: true,
            fast_input: false,
            confirm_frames: 2,
            confirm_ms: 120,
            learn_ref: true,
            gap_divisor: 25,
            skip_suspect: true,
            debug_heal: false,
        }
    }
}

pub fn path() -> PathBuf {
    let mut p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    p.pop();
    p.push("config.ini");
    p
}

pub fn load() -> Config {
    let mut cfg = Config::default();
    let Ok(text) = fs::read_to_string(path()) else {
        return cfg;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "heal_key" => cfg.key_text = v.to_string(),
            "toggle_key" => cfg.toggle_text = v.to_string(),
            "heal_below_percent" => {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.percent = n.clamp(1, 99);
                }
            }
            "cooldown_ms" => {
                if let Ok(n) = v.parse::<u64>() {
                    cfg.cooldown_ms = n;
                }
            }
            "dry_run" => cfg.dry_run = v == "true",
            "fast_input" => cfg.fast_input = v == "true",
            "confirm_frames" => {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.confirm_frames = n.max(1);
                }
            }
            "confirm_ms" => {
                if let Ok(n) = v.parse::<u64>() {
                    cfg.confirm_ms = n;
                }
            }
            "learn_ref" => cfg.learn_ref = v == "true",
            "gap_divisor" => {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.gap_divisor = n;
                }
            }
            "skip_suspect" => cfg.skip_suspect = v == "true",
            "debug_heal" => cfg.debug_heal = v == "true",
            _ => {}
        }
    }
    cfg
}

pub fn save(cfg: &Config) {
    let text = format!(
        r#"# ======================================================================
#  stalzone-heal : settings
#  Lines starting with # are comments. Delete this file to reset to defaults.
# ======================================================================

[Keys]

# Key (or combo) the program presses to use the medkit.
# Examples: c, 1, f5, space, ctrl+c, shift+1, alt+f
heal_key = {heal_key}

# Key that turns the program on/off (works in-game too).
toggle_key = {toggle_key}

[Healing]

# Use the medkit when HP drops below this percent (1-99).
heal_below_percent = {percent}

# Pause between medkit presses, ms.
cooldown_ms = {cooldown}

# true  = only writes to the log, presses nothing (test mode)
# false = really presses the key
dry_run = {dry_run}

# true  = fast input: SendInput with no delays (press and release at once)
# false = the key is held ~40 ms (more reliable if the game misses presses)
fast_input = {fast_input}

[Confirmation]
# Protection against one-frame glitches: HP must stay below the
# threshold for BOTH of these before the medkit is used.

# ...for this many reads in a row (min 1).
confirm_frames = {frames}

# ...and continuously for at least this many ms (0 = don't check time).
confirm_ms = {ms}

[Bar reading]
# How the HP bar is read from the screen. The HUD is transparent, so the
# background (red objects, bright spots) can fool the reader.

# Remember the bar color at ~full HP and compare against it.
# false = old behaviour: take the color of the first two pixels of the bar.
learn_ref = {learn}

# Allowed gap inside the bar = bar length / this number.
# Bigger number = smaller gap. 0 = old behaviour (fixed 2 px).
gap_divisor = {gap}

# Skip frames where foreign red (background) lies on the bar,
# instead of treating them as "low HP".
skip_suspect = {suspect}

[Debug]

# Adds to the heal log line: reference color, where the bar was cut off,
# number of foreign-red pixels. Also logs skipped frames.
debug_heal = {debug}
"#,
        heal_key = cfg.key_text,
        toggle_key = cfg.toggle_text,
        percent = cfg.percent,
        cooldown = cfg.cooldown_ms,
        dry_run = cfg.dry_run,
        fast_input = cfg.fast_input,
        frames = cfg.confirm_frames,
        ms = cfg.confirm_ms,
        learn = cfg.learn_ref,
        gap = cfg.gap_divisor,
        suspect = cfg.skip_suspect,
        debug = cfg.debug_heal,
    );
    let _ = fs::write(path(), text);
}