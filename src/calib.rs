use std::fs;
use std::path::PathBuf;

use crate::config;

#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub sw: u32,
    pub sh: u32,
}

pub struct Detected {
    pub bar: Bar,
    pub red: u32,
    pub stamina: u32,
    pub approx: bool,
}

type Px = [i32; 3];

#[inline]
fn px(img: &[u8], w: u32, x: u32, y: u32) -> Px {
    let i = ((y * w + x) * 4) as usize;
    [img[i] as i32, img[i + 1] as i32, img[i + 2] as i32]
}

pub fn is_red(p: Px) -> bool {
    p[0] >= 90 && p[0] - p[1] >= 45 && p[0] - p[2] >= 45
}

fn is_khaki(p: Px) -> bool {
    (60..=135).contains(&p[0])
        && (p[0] - p[1]).abs() <= 18
        && (8..=50).contains(&(p[0] - p[2]))
}

fn keep(best: &mut Option<(u32, u32)>, cur: Option<(u32, u32)>) {
    if let Some(c) = cur {
        if best.map_or(true, |b| c.1 - c.0 > b.1 - b.0) {
            *best = Some(c);
        }
    }
}

fn longest_run(
    img: &[u8],
    w: u32,
    y: u32,
    x0: u32,
    x1: u32,
    f: fn(Px) -> bool,
    gap: u32,
) -> Option<(u32, u32)> {
    let mut best = None;
    let mut cur: Option<(u32, u32)> = None;
    for x in x0..x1 {
        if !f(px(img, w, x, y)) {
            continue;
        }
        match cur {
            Some((s, l)) if x - l <= gap + 1 => cur = Some((s, x)),
            _ => {
                keep(&mut best, cur);
                cur = Some((x, x));
            }
        }
    }
    keep(&mut best, cur);
    best
}

pub fn detect(img: &[u8], w: u32, h: u32) -> Result<Detected, String> {
    if img.len() < (w * h * 4) as usize {
        return Err("bad frame".into());
    }
    let rx0 = w / 2;
    let ry0 = h / 2;
    let min_len = (w / 40).max(12);

    let mut bands: Vec<[u32; 4]> = Vec::new();
    for y in ry0..h {
        let Some((s, e)) = longest_run(img, w, y, rx0, w, is_khaki, 1) else { continue };
        if e - s + 1 < min_len {
            continue;
        }
        let extend = matches!(bands.last(), Some(b) if y == b[1] + 1 && s.abs_diff(b[2]) <= 3);
        if extend {
            let b = bands.last_mut().unwrap();
            b[1] = y;
            b[3] = b[3].max(e);
        } else {
            bands.push([y, y, s, e]);
        }
    }

    for [top, bot, s, e] in bands {
        let bh = bot - top + 1;
        if bh < 3 || bh > h / 20 {
            continue;
        }
        let xc = s + 3;
        if xc >= w {
            continue;
        }

        let lim = top.saturating_sub(bh * 4);
        let mut y = top;
        let mut red_bot = None;
        while y > lim {
            y -= 1;
            if is_red(px(img, w, xc, y)) {
                red_bot = Some(y);
                break;
            }
        }
        let Some(rb) = red_bot else { continue };

        let mut rt = rb;
        let mut gap = 0;
        let mut yy = rb;
        while yy > 0 {
            yy -= 1;
            if is_red(px(img, w, xc, yy)) {
                rt = yy;
                gap = 0;
            } else {
                gap += 1;
                if gap > 1 {
                    break;
                }
            }
        }
        let rh = rb - rt + 1;
        if rh * 3 < bh || rh > bh * 2 {
            continue;
        }
        let yr = rt + (rh / 5).max(1);

        let mut xl = xc;
        let mut miss = 0;
        let mut x = xc;
        while x > 0 {
            x -= 1;
            if is_red(px(img, w, x, yr)) {
                xl = x;
                miss = 0;
            } else {
                miss += 1;
                if miss > 1 {
                    break;
                }
            }
        }
        if xl.abs_diff(s) > 4 {
            xl = s;
        }
        let mut xr = xc;
        miss = 0;
        x = xc;
        while x + 1 < w {
            x += 1;
            if is_red(px(img, w, x, yr)) {
                xr = x;
                miss = 0;
            } else {
                miss += 1;
                if miss > 1 {
                    break;
                }
            }
        }

        let red = xr - xl + 1;
        let stamina = e - s + 1;
        let expected = h as f32 * 0.1729;
        let approx = red + 2 < stamina || (red as f32) < expected * 0.97;
        let len = if approx { stamina } else { red.max(stamina) };
        return Ok(Detected {
            bar: Bar { x: xl, y: yr, w: len, sw: w, sh: h },
            red,
            stamina,
            approx,
        });
    }
    Err("HP bar not found (is the HUD visible?)".into())
}

pub fn fallback(sw: u32, sh: u32) -> Bar {
    let k = sh as f32 / 1440.0;
    Bar {
        x: sw.saturating_sub((349.0 * k) as u32),
        y: sh.saturating_sub((165.0 * k) as u32),
        w: ((248.0 * k) as u32).max(12),
        sw,
        sh,
    }
}

fn path() -> PathBuf {
    config::path().with_file_name("bar.ini")
}

pub fn save(b: &Bar) {
    let text = format!(
        "# Written by calibration (press the calibrate key in-game). Delete to reset.\n\
         screen_w={}\nscreen_h={}\nbar_x={}\nbar_y={}\nbar_w={}\n",
        b.sw, b.sh, b.x, b.y, b.w
    );
    let _ = fs::write(path(), text);
}

pub fn load(sw: u32, sh: u32) -> Option<Bar> {
    let text = fs::read_to_string(path()).ok()?;
    let (mut s_w, mut s_h, mut x, mut y, mut w) = (0, 0, 0, 0, 0);
    for line in text.lines() {
        let Some((k, v)) = line.trim().split_once('=') else { continue };
        let Ok(n) = v.trim().parse::<u32>() else { continue };
        match k.trim() {
            "screen_w" => s_w = n,
            "screen_h" => s_h = n,
            "bar_x" => x = n,
            "bar_y" => y = n,
            "bar_w" => w = n,
            _ => {}
        }
    }
    if s_w != sw || s_h != sh || w < 12 || y == 0 || x + w > sw || y >= sh {
        return None;
    }
    Some(Bar { x, y, w, sw, sh })
}