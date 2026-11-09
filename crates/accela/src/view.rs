//! The terminal view: owns a [`Term`] and its PTY, translates GPUI input into terminal
//! input, and paints [`crate::frame::Frame`]s.

use crate::config::Config;
use crate::frame::{self, CursorKind, Frame, FrameOptions};
use crate::keymap;
use accela_pty::{Command, Pty};
use accela_vt::input::{InputModes, Key, KeyAction, KeyEvent, Mods, encode_key, encode_paste};
use accela_vt::mouse::{MouseAction, MouseButton, MouseEvent, encode_mouse};
use accela_vt::search::{self, Match};
use accela_vt::selection::{Point as SelPoint, Selection, SelectionKind, URL_SCHEMES, url_at};
use accela_vt::{Event, MouseTracking, Rgb, Term, Underline};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable,
    Font, FontStyle, FontWeight, Hsla, KeyDownEvent, KeyUpEvent, Modifiers, MouseButton as GMouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, StrikethroughStyle, Task, TextRun,
    UTF16Selection, UnderlineStyle, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use std::io::{Read, Write};
use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// Max bytes parsed per UI turn; keeps the UI responsive under `cat /dev/urandom`.
const MAX_BATCH: usize = 1 << 20;
/// Programs get this long to finish a synchronized update (mode 2026) before we draw anyway.
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    font_size: f32,
    cell_w: Pixels,
    cell_h: Pixels,
}

/// What a terminal tells its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The program exited: close this pane.
    Exited,
    TitleChanged,
}

impl gpui::EventEmitter<TerminalEvent> for TerminalView {}

#[derive(Default)]
struct SearchState {
    query: String,
    matches: Vec<Match>,
    active: Option<usize>,
    /// New output arrived; matches must be recomputed before the next paint.
    stale: bool,
}

/// Everything paint needs, computed in prepaint once the pane's size is known.
pub struct PaintData {
    frame: Frame,
    images: Vec<(i64, accela_vt::graphics::Placement, Arc<gpui::RenderImage>)>,
    metrics: Metrics,
    marked: Option<String>,
    scrollbar: Option<(Bounds<Pixels>, Hsla)>,
}

/// Width of the scrollbar's mouse hit zone at the right edge of a pane.
const SCROLLBAR_HIT: f32 = 10.0;

pub struct TerminalView {
    term: Term,
    pty: Pty,
    writer: mpsc::Sender<Vec<u8>>,
    focus: FocusHandle,
    config: Arc<Config>,
    font: Font,
    font_size: f32,
    metrics: Option<Metrics>,
    /// The cell grid's area in window coordinates (from the last prepaint).
    grid_bounds: Bounds<Pixels>,
    search: Option<SearchState>,
    /// Scrollbar drag in progress: grab point as a fraction of the thumb height.
    scrollbar_drag: Option<f32>,
    scrollbar_hover: bool,
    selection: Option<Selection>,
    selecting: bool,
    hover_link: Option<(usize, u16)>,
    hover_pointer: bool,
    last_mouse_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    marked_text: Option<String>,
    cursor_on: bool,
    focused: bool,
    blink_epoch: u64,
    blink_task: Option<Task<()>>,
    sync_task: Option<Task<()>>,
    title: String,
    images: crate::images::ImageCache,
}

impl TerminalView {
    pub fn new(config: Arc<Config>, launch: crate::Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (cols, rows) = (80u16, 24u16);
        // Precedence: `-e` command, then $ACCELA_SHELL, then `shell` in the config, then $SHELL.
        let mut cmd = match launch.command.as_deref() {
            Some([prog, args @ ..]) => args.iter().fold(Command::new(prog.clone()), |c, a| c.arg(a.clone())),
            _ => match std::env::var("ACCELA_SHELL").ok().filter(|s| !s.is_empty()).or_else(|| config.shell.clone()) {
                Some(s) => Command::new(s),
                None => Command::default_shell(),
            },
        };
        if let Some(dir) = launch.cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd = cmd.cwd(dir);
        }
        for var in crate::PRIVATE_ENV.get().into_iter().flatten() {
            cmd = cmd.env_remove(*var);
        }
        let pty = match Pty::spawn(&cmd, cols, rows) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("accela: failed to start {}: {e}; falling back to /bin/sh", cmd.program);
                Pty::spawn(&Command::new("/bin/sh"), cols, rows).expect("cannot spawn /bin/sh")
            }
        };

        // Reader thread → bounded channel: if the UI falls behind, the PTY applies
        // backpressure to the program instead of us buffering unboundedly.
        let mut reader = pty.reader().expect("pty reader");
        let (tx, rx) = async_channel::bounded::<Vec<u8>>(16);
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                const CHUNK: usize = 256 * 1024;
                let mut buf = vec![0u8; CHUNK];
                loop {
                    // Block for the first bytes, then drain whatever is already buffered:
                    // programs that write line by line produce many tiny reads, and one
                    // message per read would wake the UI thread hundreds of thousands of times.
                    let mut len = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let mut eof = false;
                    while len < CHUNK && accela_pty::poll_readable(&reader, 0).unwrap_or(false) {
                        match reader.read(&mut buf[len..]) {
                            Ok(0) | Err(_) => {
                                eof = true;
                                break;
                            }
                            Ok(n) => len += n,
                        }
                    }
                    if tx.send_blocking(buf[..len].to_vec()).is_err() || eof {
                        break;
                    }
                }
            })
            .expect("spawn pty reader");

        // Writer thread: a big paste into a program that isn't reading must not block the UI.
        let mut w = pty.writer().expect("pty writer");
        let (wtx, wrx) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for chunk in wrx {
                    if w.write_all(&chunk).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn pty writer");

        cx.spawn_in(window, async move |this, cx| {
            let mut batch = Vec::with_capacity(64 * 1024);
            while let Ok(chunk) = rx.recv().await {
                batch.clear();
                batch.extend_from_slice(&chunk);
                while batch.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(more) => batch.extend_from_slice(&more),
                        Err(_) => break,
                    }
                }
                if this.update(cx, |view, cx| view.process_output(&batch, cx)).is_err() {
                    return;
                }
            }
            // The program exited: let the container close this pane.
            let _ = this.update(cx, |_, cx| cx.emit(TerminalEvent::Exited));
        })
        .detach();

        let focus = cx.focus_handle();
        focus.focus(window);
        cx.on_focus(&focus, window, |view, _, cx| {
            view.focused = true;
            view.term.focus_changed(true);
            view.flush_output();
            view.restart_blink(cx);
            cx.notify();
        })
        .detach();
        cx.on_blur(&focus, window, |view, _, cx| {
            view.focused = false;
            view.term.focus_changed(false);
            view.flush_output();
            view.blink_task = None;
            view.cursor_on = true;
            cx.notify();
        })
        .detach();

        let mut term = Term::with_scrollback(cols as usize, rows as usize, config.scrollback);
        term.set_theme(config.theme.clone());
        let font_size = config.font_size;
        let mut view = Self {
            term,
            pty,
            writer: wtx,
            focus,
            font: font(config.font_family.clone()),
            config,
            font_size,
            metrics: None,
            grid_bounds: Bounds::default(),
            search: None,
            scrollbar_drag: None,
            scrollbar_hover: false,
            selection: None,
            selecting: false,
            hover_link: None,
            hover_pointer: false,
            last_mouse_cell: None,
            scroll_remainder: 0.0,
            marked_text: None,
            cursor_on: true,
            focused: true,
            blink_epoch: 0,
            blink_task: None,
            sync_task: None,
            title: String::new(),
            images: Default::default(),
        };
        view.restart_blink(cx);
        view
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Working directory reported by the shell (OSC 7), for new tabs and splits.
    pub fn cwd(&self) -> Option<String> {
        self.term.cwd().map(str::to_owned)
    }

    fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.writer.send(bytes);
        }
    }

    fn flush_output(&mut self) {
        if self.term.has_output() {
            let out = self.term.take_output();
            self.write(out);
        }
    }

    fn process_output(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let was_sync = self.term.modes().synchronized;
        let blink_before = self.term.modes().cursor_blink;
        self.term.feed(bytes);
        self.flush_output();
        for ev in self.term.take_events() {
            match ev {
                Event::Title(t) => {
                    if t != self.title {
                        self.title = t;
                        cx.emit(TerminalEvent::TitleChanged);
                    }
                }
                Event::Clipboard { target, data } => {
                    let item = ClipboardItem::new_string(data);
                    if target == 'p' {
                        cx.write_to_primary(item);
                    } else {
                        cx.write_to_clipboard(item);
                    }
                }
                Event::Notify { title, body } => notify(&title, &body),
                Event::Bell | Event::Cwd(_) => {}
            }
        }
        if let Some(s) = &mut self.search {
            s.stale = true;
        }
        if self.term.modes().cursor_blink != blink_before {
            self.restart_blink(cx);
        }
        if self.term.modes().synchronized {
            // Hold the frame until the update completes, but never longer than SYNC_TIMEOUT.
            if !was_sync || self.sync_task.is_none() {
                self.sync_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SYNC_TIMEOUT).await;
                    let _ = this.update(cx, |v, cx| {
                        v.sync_task = None;
                        cx.notify();
                    });
                }));
            }
            return;
        }
        self.sync_task = None;
        if self.term.take_dirty() {
            cx.notify();
        }
    }

    // ---- cursor blink ----

    /// The blink timer only runs while it's visible work: focused and requested.
    fn should_blink(&self) -> bool {
        self.config.cursor_blink && self.term.modes().cursor_blink && self.focused
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_on = true;
        self.blink_epoch += 1;
        if !self.should_blink() {
            self.blink_task = None;
            return;
        }
        let epoch = self.blink_epoch;
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let alive = this.update(cx, |v, cx| {
                    if v.blink_epoch != epoch {
                        return false;
                    }
                    v.cursor_on = !v.cursor_on;
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    return;
                }
            }
        }));
    }

    // ---- geometry ----

    fn metrics(&mut self, window: &mut Window) -> Metrics {
        if let Some(m) = self.metrics
            && m.font_size == self.font_size
        {
            return m;
        }
        let ts = window.text_system();
        let font_id = ts.resolve_font(&self.font);
        let size_px = px(self.font_size);
        let cell_w = ts.advance(font_id, size_px, 'm').map(|s| s.width).unwrap_or(px(self.font_size * 0.6));
        let cell_h = px((self.font_size * self.config.line_height).round().max(1.0));
        let m = Metrics { font_size: self.font_size, cell_w, cell_h };
        self.metrics = Some(m);
        m
    }

    /// Size the grid to `bounds` (the pane's content area).
    fn fit(&mut self, bounds: Bounds<Pixels>, window: &mut Window) -> Metrics {
        let m = self.metrics(window);
        self.grid_bounds = bounds;
        let cols = (f32::from(bounds.size.width) / f32::from(m.cell_w)).floor().max(2.0) as usize;
        let rows = (f32::from(bounds.size.height) / f32::from(m.cell_h)).floor().max(1.0) as usize;
        let (cw, ch) = (f32::from(m.cell_w).round() as u16, f32::from(m.cell_h) as u16);
        self.term.set_cell_size(cw, ch);
        if cols != self.term.cols() || rows != self.term.rows() {
            self.term.resize(cols, rows);
            self.selection = None;
            let _ = self.pty.resize(
                cols as u16,
                rows as u16,
                cw.saturating_mul(cols as u16),
                ch.saturating_mul(rows as u16),
            );
        }
        m
    }

    /// Window position → (viewport row, col, pixel offset inside the grid).
    fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize, (usize, usize)) {
        let Some(m) = self.metrics else { return (0, 0, (0, 0)) };
        let o = self.grid_bounds.origin;
        let x = (f32::from(pos.x) - f32::from(o.x)).max(0.0);
        let y = (f32::from(pos.y) - f32::from(o.y)).max(0.0);
        let col = ((x / f32::from(m.cell_w)) as usize).min(self.term.cols() - 1);
        let row = ((y / f32::from(m.cell_h)) as usize).min(self.term.rows() - 1);
        (row, col, (x as usize, y as usize))
    }

    fn sel_point(&self, row: usize, col: usize) -> SelPoint {
        SelPoint { line: self.term.visible_line_id(row), col }
    }

    // ---- keyboard ----

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if self.search.is_some() && self.search_key(ks, cx) {
            cx.stop_propagation();
            return;
        }
        if self.handle_shortcut(ks.key.as_str(), m, cx) {
            cx.stop_propagation();
            return;
        }
        let Some(key) = keymap::key(ks) else { return };
        let action = if ev.is_held { KeyAction::Repeat } else { KeyAction::Press };
        let event = KeyEvent { key, mods: keymap::mods(ks), text: ks.key_char.as_deref(), action };
        if let Some(bytes) = encode_key(&event, &InputModes::from_term(&self.term)) {
            self.write(bytes);
            self.after_input(cx);
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let modes = InputModes::from_term(&self.term);
        if modes.kitty_flags & accela_vt::input::kitty::REPORT_EVENTS == 0 {
            return;
        }
        let ks = &ev.keystroke;
        let Some(key) = keymap::key(ks) else { return };
        let event = KeyEvent { key, mods: keymap::mods(ks), text: ks.key_char.as_deref(), action: KeyAction::Release };
        if let Some(bytes) = encode_key(&event, &modes) {
            self.write(bytes);
        }
    }

    fn after_input(&mut self, cx: &mut Context<Self>) {
        if self.term.display_offset() != 0 {
            self.term.scroll_to_bottom();
        }
        self.restart_blink(cx);
        cx.notify();
    }

    /// Terminal-level shortcuts (see [`crate::bindings`]). Returns true if handled.
    fn handle_shortcut(&mut self, key: &str, m: &Modifiers, cx: &mut Context<Self>) -> bool {
        use crate::bindings::Action;
        let Some(action) = self.config.bindings.get(key, m) else { return false };
        let page = self.term.rows().saturating_sub(1).max(1) as isize;
        match action {
            Action::Copy => self.copy(cx),
            Action::Paste => self.paste(false, cx),
            Action::PastePrimary => self.paste(true, cx),
            Action::Search => self.open_search(cx),
            Action::ZoomIn => self.zoom(1.0, cx),
            Action::ZoomOut => self.zoom(-1.0, cx),
            Action::ZoomReset => self.zoom(0.0, cx),
            Action::ScrollPageUp => self.scroll_lines(page, cx),
            Action::ScrollPageDown => self.scroll_lines(-page, cx),
            Action::ScrollTop => self.scroll_lines(isize::MAX / 2, cx),
            Action::ScrollBottom => self.scroll_lines(isize::MIN / 2, cx),
            Action::PrevPrompt | Action::NextPrompt => {
                self.term.scroll_to_prompt(action == Action::PrevPrompt);
                cx.notify();
            }
            // Window-level actions are handled by the workspace before we see the key.
            _ => return false,
        }
        true
    }

    // ---- search ----

    fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search.is_none() {
            // Start from the selection, like most editors.
            let query = self.selection_text().filter(|t| !t.contains('\n')).unwrap_or_default();
            self.search = Some(SearchState { query, ..Default::default() });
            self.update_search(true);
        }
        cx.notify();
    }

    /// Keys while the search bar is open. Everything but terminal shortcuts is eaten.
    fn search_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &ks.modifiers;
        let Some(s) = &mut self.search else { return false };
        match ks.key.as_str() {
            "escape" => {
                self.search = None;
                self.term.scroll_to_bottom();
            }
            "enter" => self.step_search(!m.shift),
            "f" if m.control && m.shift => self.step_search(true),
            "g" if m.control => self.step_search(m.shift),
            "backspace" => {
                if m.control || m.alt {
                    let trimmed = s.query.trim_end();
                    let cut = trimmed.rfind(' ').map_or(0, |i| i + 1);
                    s.query.truncate(cut);
                } else {
                    s.query.pop();
                }
                self.update_search(true);
            }
            "u" if m.control && !m.shift => {
                s.query.clear();
                self.update_search(true);
            }
            "v" if m.control && m.shift => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    s.query.extend(text.chars().filter(|c| !c.is_control()));
                    self.update_search(true);
                }
            }
            _ if m.control || m.alt || m.platform => return false,
            _ => match ks.key_char.as_deref() {
                Some(t) if !t.chars().any(char::is_control) => {
                    s.query.push_str(t);
                    self.update_search(true);
                }
                // Let IME / compose deliver text through the input handler.
                _ => return false,
            },
        }
        cx.notify();
        true
    }

    /// Recompute matches. With `jump`, select the match nearest to the bottom of the
    /// view (searching backwards, like `less`) and scroll to it.
    fn update_search(&mut self, jump: bool) {
        let Some(s) = &mut self.search else { return };
        let prev = s.active.and_then(|i| s.matches.get(i)).map(|m| m.start);
        s.matches = search::find_all(&self.term, &s.query);
        s.stale = false;
        if jump {
            let bottom = self.term.visible_line_id(self.term.rows() - 1);
            s.active = search::nearest_above(&s.matches, bottom);
        } else {
            // New output: keep the same match selected if it still exists.
            s.active = prev.and_then(|p| s.matches.iter().position(|m| m.start == p));
        }
        if jump && let Some(m) = s.active.map(|i| s.matches[i]) {
            self.term.scroll_to_line(m.start.line);
        }
    }

    fn step_search(&mut self, older: bool) {
        let Some(s) = &mut self.search else { return };
        s.active = search::step(s.matches.len(), s.active, older);
        if let Some(m) = s.active.map(|i| s.matches[i]) {
            self.term.scroll_to_line(m.start.line);
        }
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.font_size = if delta == 0.0 { self.config.font_size } else { (self.font_size + delta).clamp(6.0, 72.0) };
        cx.notify();
    }

    fn scroll_lines(&mut self, lines: isize, cx: &mut Context<Self>) {
        if self.term.modes().alt_screen {
            return;
        }
        self.term.scroll_display(lines);
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn selection_text(&self) -> Option<String> {
        let text = self.selection?.range(&self.term)?.text(&self.term);
        (!text.is_empty()).then_some(text)
    }

    fn paste(&mut self, primary: bool, cx: &mut Context<Self>) {
        let item = if primary { cx.read_from_primary() } else { cx.read_from_clipboard() };
        if let Some(text) = item.and_then(|i| i.text()) {
            let bytes = encode_paste(&text, self.term.modes().bracketed_paste);
            self.write(bytes);
            self.after_input(cx);
        }
    }

    // ---- mouse ----

    fn mouse_reporting(&self, m: &Modifiers) -> bool {
        self.term.modes().mouse != MouseTracking::None && !m.shift
    }

    fn report_mouse(&mut self, button: MouseButton, action: MouseAction, pos: Point<Pixels>, m: &Modifiers) {
        let (row, col, px) = self.cell_at(pos);
        let ev = MouseEvent {
            button,
            action,
            col,
            row,
            px,
            mods: Mods { shift: m.shift, alt: m.alt, ctrl: m.control, super_: false },
        };
        if let Some(bytes) = encode_mouse(&ev, self.term.modes()) {
            self.write(bytes);
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        if ev.button == GMouseButton::Left && self.scrollbar_start(ev.position, cx) {
            return;
        }
        let (row, col, _) = self.cell_at(ev.position);
        if ev.button == GMouseButton::Left
            && ev.modifiers.control
            && let Some(url) = self.link_at(row, col)
        {
            cx.open_url(&url);
            return;
        }
        if self.mouse_reporting(&ev.modifiers) {
            if let Some(b) = map_button(ev.button) {
                self.report_mouse(b, MouseAction::Press, ev.position, &ev.modifiers);
            }
            return;
        }
        match ev.button {
            GMouseButton::Left => {
                let kind = match (ev.click_count, ev.modifiers.alt) {
                    (_, true) => SelectionKind::Block,
                    (1, _) => SelectionKind::Simple,
                    (2, _) => SelectionKind::Word,
                    _ => SelectionKind::Line,
                };
                self.selection =
                    (ev.click_count > 1 || ev.modifiers.alt).then(|| Selection::new(kind, self.sel_point(row, col)));
                self.selecting = true;
                self.last_mouse_cell = Some((row, col));
                cx.notify();
            }
            GMouseButton::Middle => self.paste(true, cx),
            _ => {}
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.is_some() && ev.pressed_button == Some(GMouseButton::Left) {
            self.scrollbar_move(ev.position, cx);
            return;
        }
        let hover = self.in_scrollbar_zone(ev.position);
        if hover != self.scrollbar_hover {
            self.scrollbar_hover = hover;
            cx.notify();
        }
        let (row, col, _) = self.cell_at(ev.position);
        self.update_hover(row, col, ev.modifiers.control, cx);
        if self.selecting && ev.pressed_button == Some(GMouseButton::Left) {
            if self.last_mouse_cell == Some((row, col)) && self.selection.is_some() {
                return;
            }
            let p = self.sel_point(row, col);
            match &mut self.selection {
                Some(s) => s.update(p),
                None => {
                    let start = self.last_mouse_cell.unwrap_or((row, col));
                    let mut s = Selection::new(SelectionKind::Simple, self.sel_point(start.0, start.1));
                    s.update(p);
                    self.selection = Some(s);
                }
            }
            self.last_mouse_cell = Some((row, col));
            cx.notify();
            return;
        }
        if self.mouse_reporting(&ev.modifiers)
            && matches!(self.term.modes().mouse, MouseTracking::ButtonEvent | MouseTracking::AnyEvent)
        {
            if self.last_mouse_cell == Some((row, col)) {
                return;
            }
            self.last_mouse_cell = Some((row, col));
            let b = ev.pressed_button.and_then(map_button).unwrap_or(MouseButton::None);
            self.report_mouse(b, MouseAction::Motion, ev.position, &ev.modifiers);
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
        if self.selecting && ev.button == GMouseButton::Left {
            self.selecting = false;
            if let Some(text) = self.selection_text() {
                cx.write_to_primary(ClipboardItem::new_string(text));
            }
            return;
        }
        if self.mouse_reporting(&ev.modifiers)
            && let Some(b) = map_button(ev.button)
        {
            self.report_mouse(b, MouseAction::Release, ev.position, &ev.modifiers);
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.metrics else { return };
        let dy = f32::from(ev.delta.pixel_delta(m.cell_h).y) / f32::from(m.cell_h) + self.scroll_remainder;
        let lines = dy.trunc() as isize;
        self.scroll_remainder = dy.fract();
        if lines == 0 {
            return;
        }
        let modes = *self.term.modes();
        if self.mouse_reporting(&ev.modifiers) {
            let b = if lines > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(b, MouseAction::Press, ev.position, &ev.modifiers);
            }
        } else if modes.alt_screen && modes.alternate_scroll {
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let ev = KeyEvent { key, mods: Mods::NONE, text: None, action: KeyAction::Press };
            if let Some(bytes) = encode_key(&ev, &InputModes { kitty_flags: 0, ..InputModes::from_term(&self.term) }) {
                self.write(bytes.repeat(lines.unsigned_abs().min(10)));
            }
        } else {
            self.term.scroll_display(lines * 3);
            cx.notify();
        }
    }

    // ---- scrollbar ----

    fn total_lines(&self) -> usize {
        self.term.scrollback().len() + self.term.rows()
    }

    fn thumb(&self) -> Option<(f32, f32)> {
        if self.term.modes().alt_screen {
            return None;
        }
        frame::scrollbar_thumb(self.total_lines(), self.term.rows(), self.term.display_offset())
    }

    fn in_scrollbar_zone(&self, pos: Point<Pixels>) -> bool {
        let b = self.grid_bounds;
        let right = f32::from(b.origin.x + b.size.width) + self.config.padding;
        self.thumb().is_some()
            && f32::from(pos.x) >= right - SCROLLBAR_HIT
            && pos.y >= b.origin.y
            && pos.y <= b.origin.y + b.size.height
    }

    /// Start a scrollbar drag if `pos` is on the scrollbar. A click on the track jumps there.
    fn scrollbar_start(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if !self.in_scrollbar_zone(pos) {
            return false;
        }
        let Some((top, height)) = self.thumb() else { return false };
        let b = self.grid_bounds;
        let frac = (f32::from(pos.y - b.origin.y) / f32::from(b.size.height)).clamp(0.0, 1.0);
        let grab = if (top..top + height).contains(&frac) { (frac - top) / height } else { 0.5 };
        self.scrollbar_drag = Some(grab);
        self.scrollbar_move(pos, cx);
        true
    }

    fn scrollbar_move(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(grab), Some((_, height))) = (self.scrollbar_drag, self.thumb()) else { return };
        let b = self.grid_bounds;
        let frac = f32::from(pos.y - b.origin.y) / f32::from(b.size.height);
        let offset = frame::scrollbar_offset(self.total_lines(), self.term.rows(), frac - grab * height);
        let delta = offset as isize - self.term.display_offset() as isize;
        if delta != 0 {
            self.term.scroll_display(delta);
            cx.notify();
        }
    }

    fn link_at(&self, row: usize, col: usize) -> Option<String> {
        let url = match self.term.hyperlink_at(row, col) {
            Some(l) => l.uri.clone(),
            None => url_at(&self.term, self.term.visible_line_id(row), col)?,
        };
        // Only open schemes we trust; never `javascript:`, custom handlers, etc.
        URL_SCHEMES.iter().any(|s| url.starts_with(s)).then_some(url)
    }

    fn update_hover(&mut self, row: usize, col: usize, ctrl: bool, cx: &mut Context<Self>) {
        let (link, pointer) = if ctrl {
            let ext = self.term.visible_row(row).cells.get(col).map(|c| c.attrs.ext).unwrap_or(0);
            let osc8 = (ext != 0 && self.term.ext(ext).link.is_some()).then_some((row, ext));
            (osc8, osc8.is_some() || self.link_at(row, col).is_some())
        } else {
            (None, false)
        };
        if link != self.hover_link || pointer != self.hover_pointer {
            self.hover_link = link;
            self.hover_pointer = pointer;
            cx.notify();
        }
    }

    // ---- painting ----

    /// Prepaint: size the grid to the pane and build everything paint needs.
    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window) -> PaintData {
        let m = self.fit(bounds, window);
        if self.search.as_ref().is_some_and(|s| s.stale) {
            self.update_search(false);
        }
        let focused = self.focus.is_focused(window);
        let (matches, active_match) = match &self.search {
            Some(s) => (&s.matches[..], s.active),
            None => (&[][..], None),
        };
        let opts = FrameOptions {
            focused,
            cursor_on: self.cursor_on || !focused,
            selection: self.selection.and_then(|s| s.range(&self.term)),
            hover_link: self.hover_link,
            bold_is_bright: self.config.bold_is_bright,
            matches,
            active_match,
        };
        let frame = frame::build(&self.term, &opts);

        let placements = self.term.visible_placements();
        let mut images = Vec::with_capacity(placements.len());
        for (row, p) in &placements {
            if let Some(tex) = self.term.image(p.image_id).and_then(|img| self.images.get(img)) {
                images.push((*row, *p, tex));
            }
        }
        if placements.is_empty() || images.len() < placements.len() {
            let term = &self.term;
            self.images.retain(|id| term.image(id).is_some());
        }

        // Overlay scrollbar: faint while idle, solid while scrolled back or in use.
        let scrollbar = self.thumb().map(|(top, height)| {
            let track_h = f32::from(bounds.size.height);
            let h = (height * track_h).max(16.0).min(track_h);
            let y = (top * track_h).min(track_h - h);
            let pad = self.config.padding;
            let w = if self.scrollbar_hover || self.scrollbar_drag.is_some() { 6.0 } else { 3.0 };
            let x = f32::from(bounds.origin.x + bounds.size.width) + (pad - w).max(-w) - 1.0;
            let active = self.term.display_offset() > 0 || self.scrollbar_hover || self.scrollbar_drag.is_some();
            let mut color = hsla(self.term.foreground());
            color.a = if active { 0.5 } else { 0.15 };
            (Bounds::new(point(px(x), bounds.origin.y + px(y)), size(px(w), px(h))), color)
        });

        PaintData { frame, images, metrics: m, marked: self.marked_text.clone(), scrollbar }
    }

    fn paint_frame(data: &PaintData, bounds: Bounds<Pixels>, base: &Font, window: &mut Window, cx: &mut App) {
        let PaintData { frame, images, metrics: m, marked, scrollbar } = data;
        let (m, marked) = (*m, marked.as_deref());
        let origin = bounds.origin;
        let (cw, ch) = (m.cell_w, m.cell_h);
        let at = |row: usize, col: usize| point(origin.x + cw * col as f32, origin.y + ch * row as f32);
        for (r, row) in frame.rows.iter().enumerate() {
            for bg in &row.backgrounds {
                window.paint_quad(fill(Bounds::new(at(r, bg.col), size(cw * bg.width as f32, ch)), hsla(bg.color)));
            }
        }
        // Images: z < 0 sit under the text, the rest above it (kitty semantics).
        let paint_images = |below: bool, window: &mut Window| {
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                for (row, p, tex) in images.iter().filter(|(_, p, _)| (p.z < 0) == below) {
                    let o = point(origin.x + cw * p.col as f32, origin.y + ch * (*row as f32));
                    let natural = tex.size(0);
                    let (nw, nh) = (natural.width.0 as f32, natural.height.0 as f32);
                    let (bw, bh) = (f32::from(cw) * p.cols as f32, f32::from(ch) * p.rows as f32);
                    // Auto-sized placements keep their pixel size; explicit c/r stretch to the box.
                    let fits = (nw / f32::from(cw)).ceil() as usize == p.cols
                        && (nh / f32::from(ch)).ceil() as usize == p.rows;
                    let sz = if fits { size(px(nw), px(nh)) } else { size(px(bw), px(bh)) };
                    let _ = window.paint_image(Bounds::new(o, sz), gpui::Corners::default(), tex.clone(), 0, false);
                }
            });
        };
        paint_images(true, window);
        for (r, row) in frame.rows.iter().enumerate() {
            for seg in &row.segments {
                let runs: Vec<TextRun> = seg.runs.iter().map(|(len, st)| text_run(*len, st, base)).collect();
                let line = window.text_system().shape_line(seg.text.clone().into(), px(m.font_size), &runs, Some(cw));
                let _ = line.paint(at(r, seg.col), ch, window, cx);
            }
            for &(col, c, color) in &row.glyphs {
                let o = at(r, col);
                let rgb = rgb(color.to_u32());
                for b in crate::boxdraw::rects(c, f32::from(cw), f32::from(ch)).unwrap_or_default() {
                    let mut color: Hsla = rgb.into();
                    color.a *= b.alpha;
                    let bounds = Bounds::new(point(o.x + px(b.x), o.y + px(b.y)), size(px(b.w), px(b.h)));
                    window.paint_quad(fill(bounds, color));
                }
            }
            for o in &row.overlines {
                window.paint_quad(fill(Bounds::new(at(r, o.col), size(cw * o.width as f32, px(1.0))), hsla(o.color)));
            }
        }
        paint_images(false, window);
        if let Some(c) = frame.cursor {
            let o = at(c.row, c.col);
            let w = cw * c.width as f32;
            let color = hsla(c.color);
            match c.kind {
                CursorKind::Block => {}
                CursorKind::Bar => window.paint_quad(fill(Bounds::new(o, size(px(2.0), ch)), color)),
                CursorKind::Underline => {
                    window.paint_quad(fill(Bounds::new(point(o.x, o.y + ch - px(2.0)), size(w, px(2.0))), color))
                }
                CursorKind::Hollow => {
                    window.paint_quad(gpui::outline(Bounds::new(o, size(w, ch)), color, gpui::BorderStyle::Solid))
                }
            }
            if let Some(text) = marked.filter(|t| !t.is_empty()) {
                // IME preedit: drawn over the cursor, underlined.
                let n = text.chars().count().max(1) as f32;
                window.paint_quad(fill(Bounds::new(o, size(cw * n, ch)), hsla(frame.background)));
                let st = frame::Style {
                    fg: c.color,
                    bold: false,
                    italic: false,
                    underline: Underline::Single,
                    underline_color: c.color,
                    strike: false,
                };
                let runs = [text_run(text.len(), &st, base)];
                let line = window.text_system().shape_line(text.to_string().into(), px(m.font_size), &runs, Some(cw));
                let _ = line.paint(o, ch, window, cx);
            }
        }
        if let Some((b, color)) = scrollbar {
            window.paint_quad(gpui::quad(*b, px(3.0), *color, px(0.0), gpui::transparent_black(), Default::default()));
        }
    }

    fn render_search_bar(&self) -> Option<impl IntoElement> {
        let s = self.search.as_ref()?;
        let status = match (s.active, s.matches.len()) {
            (_, 0) if s.query.is_empty() => String::new(),
            (_, 0) => "no matches".to_string(),
            (Some(i), n) if n >= search::MAX_MATCHES => format!("{}/{}+", i + 1, n),
            (Some(i), n) => format!("{}/{}", i + 1, n),
            (None, n) => format!("{n}"),
        };
        let fg = hsla(self.term.foreground());
        let mut muted = fg;
        muted.a = 0.6;
        Some(
            div()
                .absolute()
                .top(px(4.0))
                .right(px(12.0))
                .flex()
                .flex_row()
                .gap(px(10.0))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .bg(hsla(self.term.theme().selection))
                .text_color(fg)
                .font_family(self.config.font_family.clone())
                .text_size(px((self.font_size * 0.9).round()))
                .child(div().text_color(muted).child("find"))
                .child(div().min_w(px(120.0)).child(format!("{}▏", s.query)))
                .child(div().text_color(muted).child(status)),
        )
    }
}

fn hsla(c: Rgb) -> Hsla {
    rgb(c.to_u32()).into()
}

fn text_run(len: usize, st: &frame::Style, base: &Font) -> TextRun {
    let mut f = base.clone();
    if st.bold {
        f.weight = FontWeight::BOLD;
    }
    if st.italic {
        f.style = FontStyle::Italic;
    }
    let color = hsla(st.fg);
    TextRun {
        len,
        font: f,
        color,
        background_color: None,
        underline: (st.underline != Underline::None).then(|| UnderlineStyle {
            thickness: px(if st.underline == Underline::Double { 2.0 } else { 1.0 }),
            color: Some(hsla(st.underline_color)),
            wavy: st.underline == Underline::Curly,
        }),
        strikethrough: st.strike.then(|| StrikethroughStyle { thickness: px(1.0), color: Some(color) }),
    }
}

fn map_button(b: GMouseButton) -> Option<MouseButton> {
    Some(match b {
        GMouseButton::Left => MouseButton::Left,
        GMouseButton::Middle => MouseButton::Middle,
        GMouseButton::Right => MouseButton::Right,
        GMouseButton::Navigate(gpui::NavigationDirection::Back) => MouseButton::Back,
        GMouseButton::Navigate(gpui::NavigationDirection::Forward) => MouseButton::Forward,
    })
}

/// Desktop notification via `notify-send` (no shell involved; args are passed verbatim).
fn notify(title: &str, body: &str) {
    let title = if title.is_empty() { "accela" } else { title };
    let _ = std::process::Command::new("notify-send")
        .arg("--app-name=accela")
        .arg("--")
        .arg(title)
        .arg(body)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut c| std::thread::spawn(move || c.wait()));
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = hsla(if self.term.modes().reverse_video { self.term.foreground() } else { self.term.background() });
        let base = self.font.clone();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let pad = px(self.config.padding);
        let pointer = self.hover_pointer || self.scrollbar_hover || self.scrollbar_drag.is_some();

        div()
            .id("terminal")
            .relative()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_mouse_down(GMouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(GMouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(GMouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(GMouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(GMouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(GMouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .cursor(if pointer { CursorStyle::PointingHand } else { CursorStyle::IBeam })
            .size_full()
            .overflow_hidden()
            .bg(bg)
            .p(pad)
            .child({
                let entity2 = entity.clone();
                canvas(
                    move |bounds, window, cx| entity.update(cx, |view, _| view.prepare(bounds, window)),
                    move |bounds, data, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity2), cx);
                        Self::paint_frame(&data, bounds, &base, window, cx);
                    },
                )
                .size_full()
            })
            .children(self.render_search_bar())
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        // Committed IME / compose text. Strip controls: it's text, never commands.
        self.marked_text = None;
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        if let Some(s) = &mut self.search {
            s.query.push_str(&clean);
            self.update_search(true);
            cx.notify();
            return;
        }
        self.write(clean.into_bytes());
        self.after_input(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let m = self.metrics?;
        let (row, col) = self.term.cursor();
        let o = self.grid_bounds.origin;
        Some(Bounds::new(point(o.x + m.cell_w * col as f32, o.y + m.cell_h * row as f32), size(m.cell_w, m.cell_h)))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}
