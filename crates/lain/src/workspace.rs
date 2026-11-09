//! The window's root: tabs, each holding a tree of split terminal panes.

use crate::Launch;
use crate::bindings::Action;
use crate::config::Config;
use crate::frame::blend;
use crate::panes::{Axis, Dir, Node, Rect};
use crate::view::{TerminalEvent, TerminalView};
use gpui::{
    AnyElement, Context, Entity, EntityId, FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, Subscription,
    Window, div, prelude::*, px, relative, rgb,
};
use lain_vt::Rgb;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const RESIZE_STEP: f32 = 0.05;

struct Tab {
    root: Node<EntityId>,
    /// The pane that had focus last (restored when switching back to this tab).
    focused: EntityId,
}

pub struct Workspace {
    config: Arc<Config>,
    tabs: Vec<Tab>,
    active: usize,
    views: HashMap<EntityId, (Entity<TerminalView>, Subscription)>,
    window_title: String,
    /// Area holding the panes (measured during prepaint), for divider dragging.
    body_bounds: Rc<Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Path of the split whose divider is being dragged.
    drag: Option<Vec<bool>>,
    /// Tab being dragged to a new position.
    tab_drag: Option<usize>,
}

/// A window-level command.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Command {
    NewTab,
    NewWindow,
    ClosePane,
    Split(Axis),
    Focus(Dir),
    Resize(Axis, f32),
    NextTab,
    PrevTab,
    MoveTab(isize),
}

fn command_for(action: Action) -> Option<Command> {
    Some(match action {
        Action::NewTab => Command::NewTab,
        Action::NewWindow => Command::NewWindow,
        Action::ClosePane => Command::ClosePane,
        Action::SplitRight => Command::Split(Axis::Row),
        Action::SplitDown => Command::Split(Axis::Column),
        Action::FocusLeft => Command::Focus(Dir::Left),
        Action::FocusDown => Command::Focus(Dir::Down),
        Action::FocusUp => Command::Focus(Dir::Up),
        Action::FocusRight => Command::Focus(Dir::Right),
        Action::ResizeLeft => Command::Resize(Axis::Row, -RESIZE_STEP),
        Action::ResizeRight => Command::Resize(Axis::Row, RESIZE_STEP),
        Action::ResizeUp => Command::Resize(Axis::Column, -RESIZE_STEP),
        Action::ResizeDown => Command::Resize(Axis::Column, RESIZE_STEP),
        Action::NextTab => Command::NextTab,
        Action::PrevTab => Command::PrevTab,
        Action::MoveTabLeft => Command::MoveTab(-1),
        Action::MoveTabRight => Command::MoveTab(1),
        _ => return None,
    })
}

/// Move `items[from]` to position `to`, returning where an item that was at `tracked`
/// ends up (used to keep the active tab selected).
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize, tracked: usize) -> usize {
    if from >= items.len() || to >= items.len() || from == to {
        return tracked;
    }
    let item = items.remove(from);
    items.insert(to, item);
    if tracked == from {
        to
    } else if from < tracked && tracked <= to {
        tracked - 1
    } else if to <= tracked && tracked < from {
        tracked + 1
    } else {
        tracked
    }
}

impl Workspace {
    pub fn new(config: Arc<Config>, launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut ws = Self {
            config,
            tabs: Vec::new(),
            active: 0,
            views: HashMap::new(),
            window_title: String::new(),
            body_bounds: Rc::default(),
            drag: None,
            tab_drag: None,
        };
        let id = ws.spawn(launch, window, cx);
        ws.tabs.push(Tab { root: Node::Leaf(id), focused: id });
        ws
    }

    fn spawn(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> EntityId {
        let config = self.config.clone();
        let view = cx.new(|cx| TerminalView::new(config, launch, window, cx));
        let sub = cx.subscribe_in(&view, window, Self::on_terminal_event);
        let id = view.entity_id();
        self.views.insert(id, (view, sub));
        id
    }

    fn on_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        ev: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TerminalEvent::Exited => self.close_pane(view.entity_id(), window, cx),
            TerminalEvent::TitleChanged => cx.notify(),
        }
    }

    fn focus_handle_of(&self, id: EntityId, cx: &gpui::App) -> Option<FocusHandle> {
        self.views.get(&id).map(|(v, _)| v.read(cx).focus_handle(cx))
    }

    /// The focused pane of the active tab.
    fn current(&self, window: &Window, cx: &gpui::App) -> Option<EntityId> {
        let tab = self.tabs.get(self.active)?;
        let leaves = tab.root.leaves();
        leaves
            .iter()
            .copied()
            .find(|id| self.focus_handle_of(*id, cx).is_some_and(|f| f.is_focused(window)))
            .or(Some(tab.focused).filter(|f| leaves.contains(f)))
            .or(leaves.first().copied())
    }

    fn focus(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.root.contains(id)) {
            tab.focused = id;
        }
        if let Some(f) = self.focus_handle_of(id, cx) {
            f.focus(window);
        }
        cx.notify();
    }

    fn current_cwd(&self, window: &Window, cx: &gpui::App) -> Option<String> {
        let id = self.current(window, cx)?;
        self.views.get(&id).and_then(|(v, _)| v.read(cx).cwd())
    }

    fn run(&mut self, cmd: Command, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.current_cwd(window, cx);
        let current = self.current(window, cx);
        match cmd {
            Command::NewTab => {
                let id = self.spawn(Launch { command: None, cwd }, window, cx);
                self.tabs.push(Tab { root: Node::Leaf(id), focused: id });
                self.active = self.tabs.len() - 1;
                self.focus(id, window, cx);
            }
            Command::NewWindow => crate::open_window(self.config.clone(), Launch { command: None, cwd }, cx),
            Command::ClosePane => {
                if let Some(id) = current {
                    self.close_pane(id, window, cx);
                }
            }
            Command::Split(axis) => {
                let Some(target) = current else { return };
                let id = self.spawn(Launch { command: None, cwd }, window, cx);
                self.tabs[self.active].root.split(target, id, axis);
                self.focus(id, window, cx);
            }
            Command::Focus(dir) => {
                if let Some(next) = current.and_then(|id| self.tabs[self.active].root.neighbor(id, dir)) {
                    self.focus(next, window, cx);
                }
            }
            Command::Resize(axis, delta) => {
                if let Some(id) = current
                    && self.tabs[self.active].root.resize(id, axis, delta)
                {
                    cx.notify();
                }
            }
            Command::NextTab | Command::PrevTab => {
                let n = self.tabs.len();
                if n > 1 {
                    let step = if cmd == Command::NextTab { 1 } else { n - 1 };
                    self.activate((self.active + step) % n, window, cx);
                }
            }
            Command::MoveTab(delta) => {
                let to = self.active as isize + delta;
                if (0..self.tabs.len() as isize).contains(&to) {
                    self.move_tab(self.active, to as usize, cx);
                }
            }
        }
    }

    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.active = move_item(&mut self.tabs, from, to, self.active);
        cx.notify();
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active = index;
            let id = self.tabs[index].focused;
            self.focus(id, window, cx);
        }
    }

    /// Remove a pane (its terminal and process are dropped). Closes the tab when it was
    /// the last pane, and the window when it was the last tab.
    fn close_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ti) = self.tabs.iter().position(|t| t.root.contains(id)) else { return };
        let was_focused = self.tabs[ti].focused == id;
        let neighbor =
            [Dir::Left, Dir::Up, Dir::Right, Dir::Down].into_iter().find_map(|d| self.tabs[ti].root.neighbor(id, d));
        self.views.remove(&id);
        let root = std::mem::replace(&mut self.tabs[ti].root, Node::Leaf(id));
        match root.remove(id) {
            Some(rest) => {
                let next = neighbor.or_else(|| rest.leaves().first().copied()).expect("non-empty tree");
                self.tabs[ti].root = rest;
                if was_focused {
                    self.tabs[ti].focused = next;
                }
                if ti == self.active {
                    let f = self.tabs[ti].focused;
                    self.focus(f, window, cx);
                }
            }
            None => self.close_tab(ti, window, cx),
        }
        cx.notify();
    }

    fn close_tab(&mut self, ti: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(ti);
        for id in tab.root.leaves() {
            self.views.remove(&id);
        }
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        if self.active > ti || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.activate(self.active, window, cx);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var_os("LAIN_DEBUG_KEYS").is_some() {
            eprintln!(
                "lain: key {:?} char {:?} mods {:?}",
                ev.keystroke.key, ev.keystroke.key_char, ev.keystroke.modifiers
            );
        }
        let action = self.config.bindings.get(ev.keystroke.key.as_str(), &ev.keystroke.modifiers);
        if let Some(cmd) = action.and_then(command_for) {
            self.run(cmd, window, cx);
            cx.stop_propagation();
        }
    }

    // ---- rendering ----

    fn colors(&self) -> (Rgb, Rgb) {
        (self.config.theme.background, self.config.theme.foreground)
    }

    fn on_drag_move(&mut self, ev: &gpui::MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = &self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let b = self.body_bounds.get();
        let area = Rect {
            x: f32::from(b.origin.x),
            y: f32::from(b.origin.y),
            w: f32::from(b.size.width),
            h: f32::from(b.size.height),
        };
        let pos = (f32::from(ev.position.x), f32::from(ev.position.y));
        if let Some(tab) = self.tabs.get_mut(self.active)
            && tab.root.drag_divider(path, area, pos)
        {
            cx.notify();
        }
    }

    fn render_node(&self, node: &Node<EntityId>, path: &mut Vec<bool>, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(id) => match self.views.get(id) {
                Some((view, _)) => div().size_full().overflow_hidden().child(view.clone()).into_any_element(),
                None => div().size_full().into_any_element(),
            },
            Node::Split { axis, ratio, a, b } => {
                let (bg, fg) = self.colors();
                let line_color: Hsla = rgb(blend(bg, fg, 0.18).to_u32()).into();
                path.push(false);
                let first = div().flex_none().overflow_hidden().child(self.render_node(a, path, cx));
                path.pop();
                path.push(true);
                let second = div().flex_1().overflow_hidden().child(self.render_node(b, path, cx));
                path.pop();
                // A 5px grab area around a 1px line.
                let my_path = path.clone();
                let handle = div()
                    .id(gpui::ElementId::Name(format!("divider-{my_path:?}").into()))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.drag = Some(my_path.clone());
                            cx.stop_propagation();
                        }),
                    );
                let (first, handle) = match axis {
                    Axis::Row => (
                        first.h_full().w(relative(*ratio)),
                        handle
                            .w(px(5.0))
                            .h_full()
                            .cursor(gpui::CursorStyle::ResizeLeftRight)
                            .child(div().w(px(1.0)).h_full().bg(line_color)),
                    ),
                    Axis::Column => (
                        first.w_full().h(relative(*ratio)),
                        handle
                            .h(px(5.0))
                            .w_full()
                            .cursor(gpui::CursorStyle::ResizeUpDown)
                            .child(div().h(px(1.0)).w_full().bg(line_color)),
                    ),
                };
                let container = div().size_full().flex();
                let container = if *axis == Axis::Row { container.flex_row() } else { container.flex_col() };
                container.child(first).child(handle).child(second).into_any_element()
            }
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (bg, fg) = self.colors();
        let bar_bg: Hsla = rgb(blend(bg, Rgb(0, 0, 0), 0.3).to_u32()).into();
        let active_bg: Hsla = rgb(bg.to_u32()).into();
        let text: Hsla = rgb(fg.to_u32()).into();
        let mut muted = text;
        muted.a = 0.55;
        let tabs = self.tabs.iter().enumerate().map(|(i, tab)| {
            let title = self
                .views
                .get(&tab.focused)
                .map(|(v, _)| v.read(cx).title().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "lain".into());
            let panes = tab.root.leaves().len();
            let label = if panes > 1 { format!("{title}  [{panes}]") } else { title };
            let active = i == self.active;
            div()
                .id(("tab", i))
                .flex_1()
                .max_w(px(260.0))
                .min_w(px(60.0))
                .h_full()
                .px(px(12.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .bg(if active { active_bg } else { bar_bg })
                .text_color(if active { text } else { muted })
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.tab_drag = Some(i);
                        this.activate(i, window, cx);
                    }),
                )
                // Drop a dragged tab here.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        if let Some(from) = this.tab_drag.take()
                            && from != i
                        {
                            this.move_tab(from, i, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        if i < this.tabs.len() {
                            this.close_tab(i, window, cx);
                            cx.notify();
                        }
                    }),
                )
        });
        div()
            .flex_none()
            .h(px((self.config.font_size * 2.0).round()))
            .w_full()
            .flex()
            .flex_row()
            .bg(bar_bg)
            .font_family(self.config.font_family.clone())
            .text_size(px((self.config.font_size * 0.9).round()))
            .children(tabs)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Remember which pane has focus, and title the window after it.
        if let Some(id) = self.current(window, cx)
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            tab.focused = id;
            let title = self.views.get(&id).map(|(v, _)| v.read(cx).title().to_string()).unwrap_or_default();
            let title = if title.is_empty() { "lain".to_string() } else { title };
            if title != self.window_title {
                window.set_window_title(&title);
                self.window_title = title;
            }
        }
        let (bg, _) = self.colors();
        let body = match self.tabs.get(self.active) {
            Some(tab) => self.render_node(&tab.root, &mut Vec::new(), cx),
            None => div().into_any_element(),
        };
        let bounds_cell = self.body_bounds.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(bg.to_u32()))
            .capture_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_drag_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.drag = None;
                    this.tab_drag = None;
                }),
            )
            .when(self.tabs.len() > 1, |d| d.child(self.render_tab_bar(cx)))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(gpui::canvas(move |b, _, _| bounds_cell.set(b), |_, _, _, _| {}).absolute().size_full())
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(ctrl: bool, shift: bool, alt: bool) -> gpui::Modifiers {
        gpui::Modifiers { control: ctrl, shift, alt, ..Default::default() }
    }

    #[test]
    fn default_shortcuts_map_to_window_commands() {
        let b = crate::bindings::Bindings::default();
        let cmd = |k: &str, m: gpui::Modifiers| b.get(k, &m).and_then(command_for);
        assert_eq!(cmd("t", m(true, true, false)), Some(Command::NewTab));
        assert_eq!(cmd("d", m(true, true, false)), Some(Command::Split(Axis::Row)));
        assert_eq!(cmd("e", m(true, true, false)), Some(Command::Split(Axis::Column)));
        assert_eq!(cmd("l", m(true, true, false)), Some(Command::Focus(Dir::Right)));
        assert_eq!(cmd("l", m(true, true, true)), Some(Command::Resize(Axis::Row, RESIZE_STEP)));
        assert_eq!(cmd("tab", m(true, false, false)), Some(Command::NextTab));
        assert_eq!(cmd("tab", m(true, true, false)), Some(Command::PrevTab));
        assert_eq!(cmd("pagedown", m(true, true, true)), Some(Command::MoveTab(1)));
        // Plain Ctrl+letter and Shift+letter belong to the program.
        assert_eq!(cmd("t", m(true, false, false)), None);
        assert_eq!(cmd("w", m(false, true, false)), None);
        // Terminal-level actions are not window commands.
        assert_eq!(cmd("c", m(true, true, false)), None);
        assert_eq!(cmd("f", m(true, true, false)), None);
    }

    #[test]
    fn every_window_level_action_has_a_command() {
        let all = [
            Action::NewTab,
            Action::NewWindow,
            Action::ClosePane,
            Action::SplitRight,
            Action::SplitDown,
            Action::FocusLeft,
            Action::FocusDown,
            Action::FocusUp,
            Action::FocusRight,
            Action::ResizeLeft,
            Action::ResizeDown,
            Action::ResizeUp,
            Action::ResizeRight,
            Action::NextTab,
            Action::PrevTab,
            Action::MoveTabLeft,
            Action::MoveTabRight,
            Action::Copy,
            Action::Search,
        ];
        for a in all {
            assert_eq!(command_for(a).is_some(), a.is_window_level(), "{a:?}");
        }
    }

    #[test]
    fn moving_tabs_keeps_the_active_one() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        // Move the active tab itself.
        assert_eq!(move_item(&mut v, 1, 3, 1), 3);
        assert_eq!(v, ['a', 'c', 'd', 'b']);
        // Active tab shifts left when one is moved from before it to after it.
        assert_eq!(move_item(&mut v, 0, 2, 1), 0);
        assert_eq!(v, ['c', 'd', 'a', 'b']);
        // ...and right in the opposite case.
        assert_eq!(move_item(&mut v, 3, 0, 1), 2);
        assert_eq!(v, ['b', 'c', 'd', 'a']);
        // Out of range / no-op.
        assert_eq!(move_item(&mut v, 9, 0, 2), 2);
        assert_eq!(move_item(&mut v, 1, 1, 2), 2);
        assert_eq!(v, ['b', 'c', 'd', 'a']);
    }
}
