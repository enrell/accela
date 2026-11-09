# accela patch to gpui 0.2.2

Upstream gpui re-arms the Wayland frame callback on **every** frame, so a visible
window wakes the main thread at the display refresh rate (~60/s) forever, even when
nothing changes. Measured on accela: ~0.4% of a core while idle.

This patch makes frames demand-driven on Wayland:

- `PlatformWindow::frame_waker()` (new, default `None`): a platform may return a
  function that requests one frame callback.
- `WindowInvalidator` calls that waker whenever the window becomes dirty
  (`invalidate_view`, `set_dirty(true)`) and `Window::on_next_frame` calls it too, so
  every path that used to rely on the continuous loop still gets a frame.
- The Wayland window only requests a frame callback when woken (and never twice
  while one is pending). X11 and other platforms return `None` and are unchanged.

Verified on Hyprland: idle wakeups 63/s -> 0/s; output, typing, tabs, cursor blink
(23 toggles in 12 s, all 0.52–0.54 s apart) and compositor resizes all redraw.
Re-check with `scripts/idle-wakeups.sh`.

Second change: `Keystroke::from_xkb` used to strip the `kp_` prefix from every
keypad keysym, so keypad digits and Enter were indistinguishable from the main
keys. They now keep it (`kp_7`, `kp_enter`); other keypad keys are unchanged. This
lets the terminal send the application-keypad codes terminfo declares (ka1, kent...).

To update gpui: copy the new release here and re-apply the diff below.

```diff
--- a/src/platform.rs
+++ b/src/platform.rs
@@ -488,6 +488,12 @@
     fn toggle_fullscreen(&self);
     fn is_fullscreen(&self) -> bool;
     fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>);
+    /// accela patch: a function that asks the platform for a frame callback. Platforms
+    /// that return one stop requesting frames on their own while the window is idle;
+    /// the window calls it whenever it becomes dirty. `None` keeps the old behavior.
+    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
+        None
+    }
     fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>);
     fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>);
     fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>);
@@ -867,6 +873,7 @@
         Self { cx, handler }
     }
 
+    #[allow(dead_code)]
     fn selected_text_range(&mut self, ignore_disabled_input: bool) -> Option<UTF16Selection> {
         self.cx
             .update(|window, cx| {
--- a/src/window.rs
+++ b/src/window.rs
@@ -93,6 +93,8 @@
 
 struct WindowInvalidatorInner {
     pub dirty: bool,
+    /// accela patch: asks the platform for a frame when the window becomes dirty.
+    pub waker: Option<Rc<dyn Fn()>>,
     pub draw_phase: DrawPhase,
     pub dirty_views: FxHashSet<EntityId>,
 }
@@ -107,6 +109,7 @@
         WindowInvalidator {
             inner: Rc::new(RefCell::new(WindowInvalidatorInner {
                 dirty: true,
+                waker: None,
                 draw_phase: DrawPhase::None,
                 dirty_views: FxHashSet::default(),
             })),
@@ -118,19 +121,39 @@
         inner.dirty_views.insert(entity);
         if inner.draw_phase == DrawPhase::None {
             inner.dirty = true;
+            let waker = inner.waker.clone();
+            drop(inner);
             cx.push_effect(Effect::Notify { emitter: entity });
+            if let Some(wake) = waker {
+                wake();
+            }
             true
         } else {
             false
         }
     }
 
+    pub fn set_waker(&self, waker: Option<Rc<dyn Fn()>>) {
+        self.inner.borrow_mut().waker = waker;
+    }
+
+    /// Ask the platform for a frame (no-op on platforms that drive frames themselves).
+    pub fn wake(&self) {
+        let waker = self.inner.borrow().waker.clone();
+        if let Some(wake) = waker {
+            wake();
+        }
+    }
+
     pub fn is_dirty(&self) -> bool {
         self.inner.borrow().dirty
     }
 
     pub fn set_dirty(&self, dirty: bool) {
-        self.inner.borrow_mut().dirty = dirty
+        self.inner.borrow_mut().dirty = dirty;
+        if dirty {
+            self.wake();
+        }
     }
 
     pub fn set_phase(&self, phase: DrawPhase) {
@@ -987,6 +1010,7 @@
         let appearance = platform_window.appearance();
         let text_system = Arc::new(WindowTextSystem::new(cx.text_system().clone()));
         let invalidator = WindowInvalidator::new();
+        invalidator.set_waker(platform_window.frame_waker());
         let active = Rc::new(Cell::new(platform_window.is_active()));
         let hovered = Rc::new(Cell::new(platform_window.is_hovered()));
         let needs_present = Rc::new(Cell::new(false));
@@ -1643,6 +1667,7 @@
     /// Schedule the given closure to be run directly after the current frame is rendered.
     pub fn on_next_frame(&self, callback: impl FnOnce(&mut Window, &mut App) + 'static) {
         RefCell::borrow_mut(&self.next_frame_callbacks).push(Box::new(callback));
+        self.invalidator.wake();
     }
 
     /// Schedule a frame to be drawn on the next animation frame.
--- a/src/platform/linux/platform.rs
+++ b/src/platform/linux/platform.rs
@@ -880,7 +880,15 @@
             _ => {
                 let name = xkb::keysym_get_name(key_sym).to_lowercase();
                 if key_sym.is_keypad_key() {
-                    name.replace("kp_", "")
+                    // accela patch: keypad digits and Enter keep their `kp_` prefix so a
+                    // terminal can send the application-keypad codes for them; other
+                    // keypad keys (navigation with NumLock off, operators) are unchanged.
+                    let stripped = name.replace("kp_", "");
+                    if stripped == "enter" || (stripped.len() == 1 && stripped.as_bytes()[0].is_ascii_digit()) {
+                        name
+                    } else {
+                        stripped
+                    }
                 } else if let Some(key) = key_utf8.chars().next()
                     && key_utf8.len() == 1
                     && key.is_ascii()
--- a/src/platform/linux/wayland/window.rs
+++ b/src/platform/linux/wayland/window.rs
@@ -111,6 +111,8 @@
     hovered: bool,
     in_progress_configure: Option<InProgressConfigure>,
     resize_throttle: bool,
+    /// accela patch: a frame callback is requested and not yet delivered.
+    frame_pending: bool,
     in_progress_window_controls: Option<WindowControls>,
     window_controls: WindowControls,
     client_inset: Option<Pixels>,
@@ -181,6 +183,7 @@
             window_bounds: options.bounds,
             in_progress_configure: None,
             resize_throttle: false,
+            frame_pending: false,
             client,
             appearance,
             handle,
@@ -353,9 +356,12 @@
         Rc::ptr_eq(&self.state, &other.state)
     }
 
+    /// accela patch: frame callbacks are no longer re-armed unconditionally (that woke the
+    /// app at the display refresh rate forever, even with nothing to draw). The window
+    /// asks for one through [`Self::request_frame_callback`] whenever it becomes dirty.
     pub fn frame(&self) {
         let mut state = self.state.borrow_mut();
-        state.surface.frame(&state.globals.qh, state.surface.id());
+        state.frame_pending = false;
         state.resize_throttle = false;
         drop(state);
 
@@ -365,6 +371,24 @@
         }
     }
 
+    /// Ask the compositor for a frame callback, unless one is already pending.
+    pub fn request_frame_callback(&self) {
+        let Ok(mut state) = self.state.try_borrow_mut() else {
+            // GPUI releases the window state before running app code, so this shouldn't
+            // happen; if it does, the next input/configure event will request a frame.
+            if std::env::var_os("ACCELA_DEBUG_FRAMES").is_some() {
+                eprintln!("accela: frame wake while window state is borrowed");
+            }
+            return;
+        };
+        if state.frame_pending {
+            return;
+        }
+        state.frame_pending = true;
+        state.surface.frame(&state.globals.qh, state.surface.id());
+        state.surface.commit();
+    }
+
     pub fn handle_xdg_surface_event(&self, event: xdg_surface::Event) {
         if let xdg_surface::Event::Configure { serial } = event {
             {
@@ -978,6 +1002,11 @@
         self.borrow().fullscreen
     }
 
+    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
+        let this = self.0.clone();
+        Some(Rc::new(move || this.request_frame_callback()))
+    }
+
     fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
         self.0.callbacks.borrow_mut().request_frame = Some(callback);
     }
--- a/src/taffy.rs
+++ b/src/taffy.rs
@@ -309,7 +309,7 @@
             unit: &Option<u16>,
         ) -> Vec<taffy::GridTemplateComponent<T>> {
             // grid-template-columns: repeat(<number>, minmax(0, 1fr));
-            unit.map(|count| vec![repeat(count, vec![minmax(length(0.0), fr(1.0))])])
+            unit.map(|count| vec![repeat(count, vec![minmax(length(0.0_f32), fr(1.0_f32))])])
                 .unwrap_or_default()
         }
 
```
