mod calib;
mod combo;
mod config;
mod engine;
mod update;

use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::SetTitle;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use engine::{Level, Shared};
use update::Update;

const ACCENT: Color = Color::Rgb(255, 255, 255);
const TEXT: Color = Color::Rgb(224, 224, 230);
const DIM: Color = Color::Rgb(136, 136, 148);
const BORDER: Color = Color::Rgb(62, 62, 74);
const GREEN: Color = Color::Rgb(152, 214, 160);
const YELLOW: Color = Color::Rgb(240, 210, 150);
const RED: Color = Color::Rgb(235, 110, 110);

const MAX_LOGS: usize = 300;

struct LogLine {
    t: u64,
    level: Level,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Hp,
    Toggle,
    Heal,
}

impl Slot {
    fn next(self) -> Slot {
        match self {
            Slot::Hp => Slot::Toggle,
            Slot::Toggle => Slot::Heal,
            Slot::Heal => Slot::Hp,
        }
    }
    fn prev(self) -> Slot {
        match self {
            Slot::Hp => Slot::Heal,
            Slot::Toggle => Slot::Hp,
            Slot::Heal => Slot::Toggle,
        }
    }
}

struct App {
    shared: Arc<Shared>,
    rx: Receiver<engine::Msg>,
    logs: Vec<LogLine>,
    started: Instant,
    heal_text: String,
    toggle_text: String,
    focus: Slot,
    capturing: Option<Slot>,
    hp_box: Rect,
    toggle_box: Rect,
    heal_box: Rect,
    status_hit: Rect,
    update_hit: Rect,
    update: Option<Update>,
    update_rx: Option<Receiver<Option<Update>>>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        let cfg = config::load();
        if !config::path().exists() {
            config::save(&cfg);
        }
        let shared = Arc::new(Shared::new(&cfg));
        let (tx, rx) = mpsc::channel();
        engine::spawn(shared.clone(), tx);

        let (utx, urx) = mpsc::channel();
        thread::spawn(move || {
            let _ = utx.send(update::check());
        });

        let mut app = App {
            shared,
            rx,
            logs: Vec::new(),
            started: Instant::now(),
            heal_text: cfg.key_text.clone(),
            toggle_text: cfg.toggle_text.clone(),
            focus: Slot::Hp,
            capturing: None,
            hp_box: Rect::default(),
            toggle_box: Rect::default(),
            heal_box: Rect::default(),
            status_hit: Rect::default(),
            update_hit: Rect::default(),
            update: None,
            update_rx: Some(urx),
            should_quit: false,
        };
        app.push_log(
            Level::Info,
            format!("Started. {} — on/off (currently off)", pretty(&cfg.toggle_text)),
        );
        if cfg.dry_run {
            app.push_log(
                Level::Warn,
                "TEST mode: key is not pressed. Ctrl+T — switch".into(),
            );
        }
        app
    }

    fn poll_update(&mut self) {
        match self.update_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(found)) => {
                self.update_rx = None;
                if let Some(u) = &found {
                    let text = format!(
                        "Update available: {}. Click it in the top left to open the page",
                        update::short_version(&u.tag)
                    );
                    self.push_log(Level::Info, text);
                }
                self.update = found;
            }
            Some(Err(TryRecvError::Disconnected)) => self.update_rx = None,
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn push_log(&mut self, level: Level, text: String) {
        let t = self.started.elapsed().as_secs();
        self.logs.push(LogLine { t, level, text });
        if self.logs.len() > MAX_LOGS {
            self.logs.remove(0);
        }
    }

    fn drain_logs(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.push_log(m.level, m.text);
        }
    }

    fn save(&self) {
        config::save(&config::Config {
            key_text: self.heal_text.clone(),
            toggle_text: self.toggle_text.clone(),
            percent: self.shared.percent.load(Ordering::Relaxed),
            cooldown_ms: self.shared.cooldown_ms,
            dry_run: self.shared.dry_run.load(Ordering::Relaxed),
            fast_input: self.shared.fast_input,
            confirm_frames: self.shared.confirm_frames,
            confirm_ms: self.shared.confirm_ms,
            learn_ref: self.shared.learn_ref,
            gap_divisor: self.shared.gap_divisor,
            skip_suspect: self.shared.skip_suspect,
            debug_heal: self.shared.debug_heal,
        });
    }

    fn bump(&mut self, delta: i32) {
        let cur = self.shared.percent.load(Ordering::Relaxed) as i32;
        let next = (cur + delta).clamp(1, 99);
        if next != cur {
            self.shared.percent.store(next as u32, Ordering::Relaxed);
            self.save();
        }
    }

    fn toggle_dry(&mut self) {
        let now = !self.shared.dry_run.load(Ordering::Relaxed);
        self.shared.dry_run.store(now, Ordering::Relaxed);
        self.save();
        if now {
            self.push_log(Level::Warn, "TEST mode: key is not pressed".into());
        } else {
            self.push_log(Level::Warn, "LIVE mode: key will be pressed".into());
        }
    }

    fn apply_key(&mut self, slot: Slot, text: String) {
        let other = match slot {
            Slot::Toggle => &self.heal_text,
            _ => &self.toggle_text,
        };
        if *other == text {
            self.push_log(Level::Warn, format!("{} is already used by another key", pretty(&text)));
            return;
        }
        if slot == Slot::Toggle && combo::parse_hotkey(&text).is_err() {
            self.push_log(
                Level::Warn,
                format!("{} can't be used as the toggle key", pretty(&text)),
            );
            return;
        }

        self.capturing = None;
        match slot {
            Slot::Toggle => {
                self.shared.set_toggle_key(text.clone());
                self.toggle_text = text.clone();
                self.push_log(Level::Info, format!("Toggle key: {}", pretty(&text)));
            }
            _ => {
                self.shared.set_key(text.clone());
                self.heal_text = text.clone();
                self.push_log(Level::Info, format!("Heal key: {}", pretty(&text)));
            }
        }
        self.save();
    }

    fn activate(&mut self, slot: Slot) {
        self.focus = slot;
        self.capturing = match slot {
            Slot::Hp => None,
            other => Some(other),
        };
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        let inside = |r: Rect| {
            m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = if inside(self.hp_box) {
                    Some(Slot::Hp)
                } else if inside(self.toggle_box) {
                    Some(Slot::Toggle)
                } else if inside(self.heal_box) {
                    Some(Slot::Heal)
                } else {
                    None
                };
                match hit {
                    Some(slot) => self.activate(slot),
                    None => {
                        self.capturing = None;
                        if inside(self.update_hit) {
                            if let Some(u) = &self.update {
                                update::open(&u.url);
                            }
                        }
                        if inside(self.status_hit) {
                            self.shared.enabled.fetch_xor(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp if inside(self.hp_box) => self.bump(1),
            MouseEventKind::ScrollDown if inside(self.hp_box) => self.bump(-1),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if let Some(slot) = self.capturing {
            if key.code == KeyCode::Esc {
                self.capturing = None;
                self.push_log(Level::Info, "Key selection cancelled".into());
                return;
            }
            match combo::from_event(key) {
                Ok(text) => self.apply_key(slot, text),
                Err(e) => self.push_log(Level::Warn, e),
            }
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if ctrl && !alt {
            if let KeyCode::Char(c) = key.code {
                if matches!(combo::latin_letter(c), 'q' | 'c') {
                    self.should_quit = true;
                    return;
                }
            }
        }

        match key.code {
            KeyCode::Char(c) if ctrl && !alt => {
                if combo::latin_letter(c) == 't' {
                    self.toggle_dry();
                }
            }
            KeyCode::Up => self.bump(1),
            KeyCode::Down => self.bump(-1),
            KeyCode::PageUp => self.bump(5),
            KeyCode::PageDown => self.bump(-5),
            KeyCode::Left | KeyCode::BackTab => self.focus = self.focus.prev(),
            KeyCode::Right | KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::Enter => self.activate(self.focus),
            _ => {}
        }
    }
}

fn main() {
    let mut app = App::new();
    let mut terminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(
        io::stdout(),
        EnableMouseCapture,
        SetTitle("stalzone-heal")
    );
    let result = run(&mut terminal, &mut app);
    let _ = ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    while !app.should_quit {
        app.drain_logs();
        app.poll_update();
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                Event::Mouse(m) => app.handle_mouse(m),
                _ => {}
            }
        }
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin { horizontal: 2, vertical: 0 });

    let [_top, header, _g1, boxes, _g2, live, _g3, logs, _g4, hints, _bottom] =
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

    draw_header(f, app, header);

    let [hp_area, _g1, toggle_area, _g2, heal_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(boxes);
    draw_hp_box(f, app, hp_area);

    let min_w = toggle_area.width.min(heal_area.width);
    app.toggle_box = toggle_area;
    let toggle_text = pretty(&app.toggle_text);
    draw_key_box(f, app, toggle_area, min_w, "Toggle", &toggle_text, Slot::Toggle);

    app.heal_box = heal_area;
    let heal_text = pretty(&app.heal_text);
    draw_key_box(f, app, heal_area, min_w, "Heal", &heal_text, Slot::Heal);

    draw_live(f, app, live.inner(Margin { horizontal: 1, vertical: 0 }));
    let logs_block = box_block("Logs", BORDER);
    let logs_inner = logs_block
        .inner(logs)

        .inner(Margin { horizontal: 2, vertical: 1 });
    f.render_widget(logs_block, logs);
    draw_logs(f, app, logs_inner);
    draw_hints(f, app, hints);
}

fn draw_header(f: &mut Frame, app: &mut App, area: Rect) {
    let title = "Auto-heal";
    let version = update::short_version(concat!("v", env!("CARGO_PKG_VERSION")));
    let mut spans = vec![
        Span::styled(title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {version}"), Style::new().fg(DIM)),
    ];

    app.update_hit = Rect::default();
    if let Some(u) = &app.update {
        let label = format!("↑ {} available", update::short_version(&u.tag));
        let gap = 3;
        let label_w = label.chars().count();
        let used = title.chars().count() + 1 + version.chars().count();
        let right_w = 16;
        if used + gap + label_w + right_w <= area.width as usize {
            app.update_hit = Rect::new(area.x + (used + gap) as u16, area.y, label_w as u16, 1);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(label, Style::new().fg(YELLOW)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    let on = app.shared.enabled.load(Ordering::Relaxed);
    let dry = app.shared.dry_run.load(Ordering::Relaxed);
    let (status, status_color) = if on { ("● ON", GREEN) } else { ("○ OFF", DIM) };
    let (mode, mode_color) = if dry { ("TEST", YELLOW) } else { ("LIVE", RED) };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(status, Style::new().fg(status_color).add_modifier(Modifier::BOLD)),
            Span::styled(" · ", Style::new().fg(BORDER)),
            Span::styled(mode, Style::new().fg(mode_color)),
        ]))
        .alignment(Alignment::Right),
        area,
    );

    let status_w = status.chars().count() as u16;
    let total_w = status_w + 3 + mode.chars().count() as u16;
    app.status_hit = Rect::new(
        area.x + area.width.saturating_sub(total_w),
        area.y,
        status_w,
        1,
    );
}

fn box_block(title: &str, border: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::new().fg(DIM),
        )))
}

fn draw_hp_box(f: &mut Frame, app: &mut App, area: Rect) {
    app.hp_box = area;
    let pct = app.shared.percent.load(Ordering::Relaxed);

    let border = if app.focus == Slot::Hp { DIM } else { BORDER };
    let hint = if area.width >= 26 { "heal below · ↑↓" } else { "↑↓" };
    let block = box_block("HP", border).title_bottom(
        Line::from(Span::styled(format!(" {hint} "), Style::new().fg(DIM))).centered(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [_pad_top, mid, _pad_bottom] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{pct}%"),
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        mid,
    );
}

fn draw_key_box(
    f: &mut Frame,
    app: &App,
    area: Rect,
    min_w: u16,
    title: &str,
    value: &str,
    slot: Slot,
) {
    let capturing = app.capturing == Some(slot);
    let border = if capturing {
        ACCENT
    } else if app.focus == slot {
        DIM
    } else {
        BORDER
    };

    let wide = area.width.min(min_w) >= 24;
    let hint_text = match (capturing, wide) {
        (true, true) => "Esc — cancel",
        (true, false) => "Esc",
        (false, true) => "click to change",
        (false, false) => "change",
    };
    let block = box_block(title, border).title_bottom(
        Line::from(Span::styled(format!(" {hint_text} "), Style::new().fg(DIM))).centered(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [_pad_top, mid, _pad_bottom] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

    let value = if capturing {
        Span::styled("press a key…", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(
            value.to_string(),
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(value)).alignment(Alignment::Center),
        mid,
    );
}

fn draw_live(f: &mut Frame, app: &App, area: Rect) {
    let hp = app.shared.hp_milli.load(Ordering::Relaxed);
    let thr = app.shared.percent.load(Ordering::Relaxed) as usize;

    let mut spans: Vec<Span> = Vec::new();

    if hp == engine::NO_BAR {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "HP bar not found",
                Style::new().fg(DIM),
            )))
            .alignment(Alignment::Center),
            area,
        );
        return;
    } else {
        let bw = (area.width as usize).saturating_sub(5).max(10);
        let pct = (hp as f32 / 10.0).round() as usize;
        let filled = (hp as usize * bw / 1000).min(bw);
        let thr_pos = (thr * bw / 100).min(bw - 1);

        for i in 0..bw {
            if i == thr_pos {
                spans.push(Span::styled("│", Style::new().fg(ACCENT)));
            } else if i < filled {
                spans.push(Span::styled("█", Style::new().fg(RED)));
            } else {
                spans.push(Span::styled("░", Style::new().fg(BORDER)));
            }
        }
        spans.push(Span::styled(
            format!(" {pct:>3}%"),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let h = area.height as usize;
    let text_w = (area.width as usize).saturating_sub(11);
    let start = app.logs.len().saturating_sub(h);

    let lines: Vec<Line> = app.logs[start..]
        .iter()
        .map(|l| {
            let (mark, color) = match l.level {
                Level::Info => ("·", DIM),
                Level::Off => ("○", DIM),
                Level::Ok => ("●", GREEN),
                Level::Heal => ("+", ACCENT),
                Level::Warn => ("!", YELLOW),
                Level::Error => ("✗", RED),
            };
            let time = format!("{:02}:{:02}:{:02} ", l.t / 3600, l.t / 60 % 60, l.t % 60);
            Line::from(vec![
                Span::styled(time, Style::new().fg(BORDER)),
                Span::styled(format!("{mark} "), Style::new().fg(color)),
                Span::styled(truncate(&l.text, text_w), Style::new().fg(color)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn pretty(s: &str) -> String {
    s.split('+')
        .map(|p| {
            let mut ch = p.chars();
            match ch.next() {
                Some(first) => first.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

type Hint = (String, &'static str, &'static str, u8);

fn hints_width(keys: &[Hint], compact: bool) -> usize {
    let text: usize = keys
        .iter()
        .map(|(k, d, short, _)| {
            let d = if compact { short } else { d };
            k.chars().count() + 1 + d.chars().count()
        })
        .sum();
    let sep = if compact { 2 } else { 3 };
    text + sep * keys.len().saturating_sub(1)
}

fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let mut keys: Vec<Hint> = if app.capturing.is_some() {
        vec![
            ("any key".into(), "record", "record", 1),
            ("Esc".into(), "cancel", "cancel", 2),
        ]
    } else {
        vec![
            (pretty(&app.toggle_text), "on/off", "on", 1),
            ("↑↓".into(), "threshold", "thresh", 4),
            ("←→ Enter".into(), "select key", "key", 3),
            ("Ctrl+T".into(), "test/live", "test", 5),
        ]
    };

    let max = area.width as usize;
    let compact = hints_width(&keys, false) > max;
    while hints_width(&keys, compact) > max && keys.len() > 1 {
        let worst = keys
            .iter()
            .enumerate()
            .max_by_key(|(_, h)| h.3)
            .map(|(i, _)| i)
            .unwrap_or(0);
        keys.remove(worst);
    }

    let sep = if compact { "  " } else { " · " };
    let mut spans: Vec<Span> = Vec::new();
    for (i, (k, d, short, _)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(sep, Style::new().fg(BORDER)));
        }
        let d = if compact { short } else { d };
        spans.push(Span::styled(k.clone(), Style::new().fg(ACCENT)));
        spans.push(Span::styled(format!(" {d}"), Style::new().fg(DIM)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}