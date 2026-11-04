use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use device_query::{DeviceQuery, DeviceState};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use xcap::Monitor;

use crate::calib::{self, Bar};
use crate::combo::{self, Combo, Hotkey};
use crate::config::Config;

const CALIB_HOTKEY: &str = "end";
const AUTO_CALIB_EVERY: Duration = Duration::from_secs(3);

const POLL_ON: Duration = Duration::from_millis(15);
const POLL_OFF: Duration = Duration::from_millis(30);
const IDLE_READ: Duration = Duration::from_millis(150);

static ON_SND: &[u8] = include_bytes!("on.wav");
static OFF_SND: &[u8] = include_bytes!("off.wav");
const VOLUME: f32 = 0.5;

pub const NO_BAR: u32 = u32::MAX;

const GAME_EXE: &str = "stalzonew.exe";

fn game_in_focus() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd as usize == 0 {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return false;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h as usize == 0 {
            return true;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return true;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\')
            .next()
            .map_or(false, |name| name.eq_ignore_ascii_case(GAME_EXE))
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Off,
    Ok,
    Heal,
    Warn,
    Error,
}

pub struct Msg {
    pub level: Level,
    pub text: String,
}

pub struct Shared {
    pub enabled: AtomicBool,
    pub percent: AtomicU32,
    pub dry_run: AtomicBool,
    pub hp_milli: AtomicU32,
    pub key_text: Mutex<String>,
    pub key_version: AtomicU32,
    pub toggle_text: Mutex<String>,
    pub toggle_version: AtomicU32,
    pub cooldown_ms: u64,
    pub fast_input: bool,
    pub confirm_frames: u32,
    pub confirm_ms: u64,
    pub learn_ref: bool,
    pub gap_divisor: u32,
    pub skip_suspect: bool,
    pub debug_heal: bool,
}

impl Shared {
    pub fn new(cfg: &Config) -> Self {
        Shared {
            enabled: AtomicBool::new(false),
            percent: AtomicU32::new(cfg.percent.clamp(1, 99)),
            dry_run: AtomicBool::new(cfg.dry_run),
            hp_milli: AtomicU32::new(NO_BAR),
            key_text: Mutex::new(cfg.key_text.clone()),
            key_version: AtomicU32::new(0),
            toggle_text: Mutex::new(cfg.toggle_text.clone()),
            toggle_version: AtomicU32::new(0),
            cooldown_ms: cfg.cooldown_ms,
            fast_input: cfg.fast_input,
            confirm_frames: cfg.confirm_frames.max(1),
            confirm_ms: cfg.confirm_ms,
            learn_ref: cfg.learn_ref,
            gap_divisor: cfg.gap_divisor,
            skip_suspect: cfg.skip_suspect,
            debug_heal: cfg.debug_heal,
        }
    }

    pub fn set_key(&self, text: String) {
        *self.key_text.lock().unwrap() = text;
        self.key_version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn key(&self) -> String {
        self.key_text.lock().unwrap().clone()
    }

    pub fn set_toggle_key(&self, text: String) {
        *self.toggle_text.lock().unwrap() = text;
        self.toggle_version.fetch_add(1, Ordering::SeqCst);
    }

    pub fn toggle_key(&self) -> String {
        self.toggle_text.lock().unwrap().clone()
    }
}

pub fn spawn(shared: Arc<Shared>, tx: Sender<Msg>) {
    thread::spawn(move || run(shared, tx));
}

struct LowTracker {
    need_frames: u32,
    need_time: Duration,
    frames: u32,
    since: Option<Instant>,
}

impl LowTracker {
    fn new(need_frames: u32, need_ms: u64) -> Self {
        LowTracker {
            need_frames: need_frames.max(1),
            need_time: Duration::from_millis(need_ms),
            frames: 0,
            since: None,
        }
    }

    fn reset(&mut self) {
        self.frames = 0;
        self.since = None;
    }

    fn update(&mut self, low: bool) -> bool {
        if !low {
            self.reset();
            return false;
        }
        self.frames += 1;
        let since = *self.since.get_or_insert_with(Instant::now);
        self.frames >= self.need_frames && since.elapsed() >= self.need_time
    }
}

const SUSPECT_FRAC: f32 = 0.08;

#[derive(Clone, Copy)]
struct Reading {
    frac: f32,
    color: [i32; 3],
    reference: [i32; 3],
    end_x: u32,
    odd: u32,
}

struct Probe {
    reading: Option<Reading>,
    suspect: Option<u32>,
}

fn read_hp(
    monitor: &Monitor,
    bar: &Bar,
    learned: Option<[i32; 3]>,
    max_gap: u32,
    skip_suspect: bool,
) -> Result<Probe, String> {
    const ROWS: u32 = 2;
    let img = monitor
        .capture_region(bar.x, bar.y.saturating_sub(1), bar.w, ROWS)
        .map_err(|e| e.to_string())?;

    let mut best: Option<Reading> = None;
    let mut suspect: Option<u32> = None;
    for row in 0..ROWS {
        let at = |x: u32| {
            let p = img.get_pixel(x, row);
            [p[0] as i32, p[1] as i32, p[2] as i32]
        };

        let reference = match learned {
            Some(r) => r,
            None => {
                let a = at(1);
                let b = at(2);
                [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2]
            }
        };
        if !calib::is_red(reference) {
            continue;
        }

        let mut last_red: Option<u32> = None;
        let mut misses = 0u32;
        let mut closed = false;
        let mut sum = [0i32; 3];
        let mut cnt = 0i32;
        let mut odd = 0u32;
        for x in 0..bar.w {
            let p = at(x);
            let red = calib::is_red(p);
            let same = red && (0..3).all(|i| (p[i] - reference[i]).abs() <= 45);
            if red && !same {
                odd += 1;
            }
            if closed {
                continue;
            }
            if same {
                last_red = Some(x);
                misses = 0;
                for i in 0..3 {
                    sum[i] += p[i];
                }
                cnt += 1;
            } else {
                misses += 1;
                if misses > max_gap {
                    closed = true;
                }
            }
        }

        if skip_suspect && odd as f32 > bar.w as f32 * SUSPECT_FRAC {
            suspect = Some(suspect.map_or(odd, |s| s.max(odd)));
            continue;
        }

        if let (Some(x), true) = (last_red, cnt > 0) {
            let frac = (x + 1) as f32 / bar.w as f32;
            let r = Reading {
                frac,
                color: [sum[0] / cnt, sum[1] / cnt, sum[2] / cnt],
                reference,
                end_x: x,
                odd,
            };
            if best.map_or(true, |b| r.frac > b.frac) {
                best = Some(r);
            }
        }
    }
    let suspect = if best.is_some() { None } else { suspect };
    Ok(Probe { reading: best, suspect })
}

fn calibrate(monitor: &Monitor) -> Result<calib::Detected, String> {
    let img = monitor.capture_image().map_err(|e| e.to_string())?;
    calib::detect(img.as_raw(), img.width(), img.height())
}

fn play(handle: Option<&OutputStreamHandle>, data: &'static [u8]) {
    let Some(handle) = handle else { return };
    if let Ok(sink) = Sink::try_new(handle) {
        if let Ok(src) = Decoder::new(Cursor::new(data)) {
            sink.set_volume(VOLUME);
            sink.append(src);
            sink.detach();
        }
    }
}

fn run(shared: Arc<Shared>, tx: Sender<Msg>) {
    let log = |level: Level, text: String| {
        let _ = tx.send(Msg { level, text });
    };

    let monitor = match Monitor::all().ok().and_then(|v| v.into_iter().next()) {
        Some(m) => m,
        None => {
            log(Level::Error, "Monitor not found".into());
            return;
        }
    };
    let screen_w = match monitor.width() {
        Ok(w) => w,
        Err(e) => {
            log(Level::Error, format!("Failed to get monitor width: {e}"));
            return;
        }
    };
    let screen_h = match monitor.height() {
        Ok(h) => h,
        Err(e) => {
            log(Level::Error, format!("Failed to get monitor height: {e}"));
            return;
        }
    };
    let ds = DeviceState::new();
    let audio = OutputStream::try_default().ok();
    if audio.is_none() {
        log(Level::Warn, "No audio device — sounds disabled".into());
    }

    let (mut bar, mut calibrated) = match calib::load(screen_w, screen_h) {
        Some(b) => {
            log(Level::Info, format!("Calibration loaded: x={} y={} len={}", b.x, b.y, b.w));
            (b, true)
        }
        None => {
            log(
                Level::Warn,
                format!(
                    "Not calibrated — in game with full HP press {}",
                    CALIB_HOTKEY.to_uppercase()
                ),
            );
            (calib::fallback(screen_w, screen_h), false)
        }
    };
    let calib_hotkey = combo::parse_hotkey(CALIB_HOTKEY).ok();
    let mut calib_was_down = false;
    let mut last_auto = Instant::now();
    let mut approx_warned = false;

    let cooldown = Duration::from_millis(shared.cooldown_ms);
    let mut last_heal: Option<Instant> = None;
    let mut last_read: Option<Instant> = None;
    let mut prev_enabled = false;
    let mut toggle_was_down = false;
    let mut low = LowTracker::new(shared.confirm_frames, shared.confirm_ms);
    let mut err_logged = false;

    let mut learned: Option<[i32; 3]> = None;
    let mut none_since: Option<Instant> = None;
    let mut last_suspect_log = Instant::now();
    if shared.debug_heal {
        log(
            Level::Info,
            format!(
                "Reader: learn_ref={} gap_divisor={} skip_suspect={}",
                shared.learn_ref, shared.gap_divisor, shared.skip_suspect
            ),
        );
    }

    let mut combo: Option<Combo> = None;
    let mut combo_ver = u32::MAX;

    let mut hotkey: Option<Hotkey> = None;
    let mut toggle_ver = u32::MAX;

    loop {
        let tver = shared.toggle_version.load(Ordering::SeqCst);
        if tver != toggle_ver {
            toggle_ver = tver;
            let text = shared.toggle_key();
            hotkey = match combo::parse_hotkey(&text) {
                Ok(h) => Some(h),
                Err(e) => {
                    log(Level::Warn, format!("Toggle key '{text}': {e}"));
                    None
                }
            };
        }

        let toggle_down = match &hotkey {
            Some(h) => h.is_down(&ds.get_keys()),
            None => false,
        };
        if toggle_down && !toggle_was_down {
            shared.enabled.fetch_xor(true, Ordering::SeqCst);
        }
        toggle_was_down = toggle_down;

        let in_game = game_in_focus();

        let calib_down = match &calib_hotkey {
            Some(h) => h.is_down(&ds.get_keys()),
            None => false,
        };
        let manual = in_game && calib_down && !calib_was_down;
        calib_was_down = calib_down;
        let auto = in_game && !calibrated && last_auto.elapsed() >= AUTO_CALIB_EVERY;
        if manual || auto {
            last_auto = Instant::now();
            match calibrate(&monitor) {
                Ok(d) if d.approx => {
                    if manual || !approx_warned {
                        approx_warned = true;
                        log(
                            Level::Warn,
                            format!(
                                "Bar found, but HP is not full — not saved. \
                                 Heal to 100% and press {}",
                                CALIB_HOTKEY.to_uppercase()
                            ),
                        );
                    }
                }
                Ok(d) => {
                    bar = d.bar;
                    learned = None;
                    calibrated = true;
                    calib::save(&bar);
                    let how = if manual { "Calibrated" } else { "Auto-calibrated" };
                    log(
                        Level::Ok,
                        format!("{how}: x={} y={} len={} px", bar.x, bar.y, bar.w),
                    );
                }
                Err(e) if manual => log(Level::Warn, format!("Calibration: {e}")),
                Err(_) => {}
            }
        }

        let enabled = shared.enabled.load(Ordering::SeqCst);
        if enabled != prev_enabled {
            prev_enabled = enabled;
            low.reset();
            let h = audio.as_ref().map(|(_, h)| h);
            if enabled {
                log(Level::Ok, "Enabled".into());
                play(h, ON_SND);
            } else {
                log(Level::Off, "Disabled".into());
                play(h, OFF_SND);
            }
        }

        let ver = shared.key_version.load(Ordering::SeqCst);
        if ver != combo_ver {
            combo_ver = ver;
            let text = shared.key();
            combo = match combo::parse(&text) {
                Ok(c) => Some(c),
                Err(e) => {
                    log(Level::Warn, format!("Key '{text}': {e}"));
                    None
                }
            };
        }

        if !in_game {
            low.reset();
            shared.hp_milli.store(NO_BAR, Ordering::Relaxed);
            thread::sleep(POLL_OFF);
            continue;
        }

        let due = enabled || last_read.map_or(true, |t| t.elapsed() >= IDLE_READ);
        if !due {
            thread::sleep(POLL_OFF);
            continue;
        }
        last_read = Some(Instant::now());

        let t = Instant::now();
        let max_gap = if shared.gap_divisor == 0 {
            2
        } else {
            (bar.w / shared.gap_divisor).max(2)
        };
        let probe = match read_hp(&monitor, &bar, learned, max_gap, shared.skip_suspect) {
            Ok(p) => {
                err_logged = false;
                p
            }
            Err(e) => {
                if !err_logged {
                    log(Level::Error, format!("Screen capture: {e}"));
                    err_logged = true;
                }
                shared.hp_milli.store(NO_BAR, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(300));
                continue;
            }
        };
        let reading = probe.reading;

        if let Some(odd) = probe.suspect {
            if enabled && shared.debug_heal && last_suspect_log.elapsed() >= Duration::from_secs(1) {
                last_suspect_log = Instant::now();
                log(
                    Level::Warn,
                    format!("Frame skipped: {odd} foreign-red px on the bar (len {})", bar.w),
                );
            }
        }

        if shared.learn_ref {
            if let Some(r) = reading {
                let ok = match learned {
                    Some(l) => r.frac >= 0.9 && (0..3).all(|i| (r.color[i] - l[i]).abs() <= 30),
                    None => r.frac >= 0.97,
                };
                if ok {
                    learned = Some(match learned {
                        Some(l) => [
                            (l[0] * 3 + r.color[0]) / 4,
                            (l[1] * 3 + r.color[1]) / 4,
                            (l[2] * 3 + r.color[2]) / 4,
                        ],
                        None => r.color,
                    });
                }
            }
            if reading.is_some() || probe.suspect.is_some() {
                none_since = None;
            } else if learned.is_some() {
                let since = *none_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(5) {
                    learned = None;
                    none_since = None;
                    if shared.debug_heal {
                        log(Level::Info, "Reference color reset".into());
                    }
                }
            }
        }

        let hp = reading.map(|r| r.frac);
        shared.hp_milli.store(
            hp.map_or(NO_BAR, |h| (h * 1000.0) as u32),
            Ordering::Relaxed,
        );

        if !enabled {
            thread::sleep(POLL_OFF);
            continue;
        }

        let pct = shared.percent.load(Ordering::Relaxed);
        let threshold = pct as f32 / 100.0;
        let confirmed = low.update(matches!(hp, Some(h) if h < threshold));

        let ready = last_heal.map_or(true, |t| t.elapsed() >= cooldown);
        if confirmed && ready {
            let h = hp.unwrap_or(0.0) * 100.0;
            let key_text = shared.key();
            let dbg = match (shared.debug_heal, reading) {
                (true, Some(r)) => format!(
                    " | ref=({},{},{}) {} end={}/{} gap={} odd={}",
                    r.reference[0],
                    r.reference[1],
                    r.reference[2],
                    if learned.is_some() { "learned" } else { "px" },
                    r.end_x,
                    bar.w,
                    max_gap,
                    r.odd
                ),
                _ => String::new(),
            };
            low.reset();
            last_heal = Some(Instant::now());

            match (&combo, shared.dry_run.load(Ordering::Relaxed)) {
                (Some(c), false) => {
                    combo::press(c, shared.fast_input);
                    log(
                        Level::Heal,
                        format!("HP {h:.0}% < {pct}% → pressed [{key_text}] · loop {:?}{dbg}", t.elapsed()),
                    );
                }
                (Some(_), true) => log(
                    Level::Heal,
                    format!("TEST · HP {h:.0}% < {pct}% → [{key_text}] (not pressed){dbg}"),
                ),
                (None, _) => log(Level::Warn, "Key not set".into()),
            }
        }

        thread::sleep(POLL_ON);
    }
}