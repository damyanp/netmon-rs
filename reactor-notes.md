# windows-reactor notes

Friction encountered building netmon-rs on `windows-rs` / `windows-reactor`
(WinUI 3). Recorded here so the next person (or the next feature) doesn't have to
rediscover it. Reactor source referenced is the pinned rev `c166957`,
`crates/libs/reactor/src/`.

## 1. Nested components don't re-render from their own state

### Symptom

The target editor kept saving empty `name`/`host` even though text was typed.
Instrumentation showed the setter ran but the form never re-rendered:

```
render name="" host="" mac=""      <- edit_form mounted once
on_text_changed NAME="TestRouter"  <- setter fired with the right value
(no further render line)            <- edit_form NEVER re-rendered
```

So the Save closure kept the stale empty value it captured at mount.

### Root cause

Reactor re-renders the root each pass, then reconciles and prunes subtrees that
look structurally unchanged (`reconciler.rs`, `Reconciler::update`). The
per-component "is this dirty?" check (`is_component_state_dirty`) is only
consulted for the node *currently being visited* — it never runs for a component
buried under structurally-unchanged non-component parents (here
`scroll_viewer` → `grid`), because the subtree is pruned before we descend to it.

Net effect: **a component that mutates only its own `use_state`, nested under
unchanged non-component parents, never re-renders.** Its dirty flag is set,
`request_rerender` fires, a pass runs — and prunes right over it.

Context doesn't have this problem: a provider value change sets a global
`force_component_rerender` flag (`force_context_subscribers`) that punches through
pruning to reach consumers anywhere. Plain `use_state` writes have no equivalent
path — the asymmetry is the whole bug.

Why it wasn't obvious: the latency chart re-rendered fine, but only because its
`revision` prop changes every 1 s tick, forcing descent along its path for an
unrelated reason and masking the issue.

### Contributing trap: controlled TextBox + per-keystroke persistence

Reactor's `TextBox` is *controlled* (pushes `Prop::Value` every render). Our first
design wrote each keystroke through a setter into shared settings and re-derived
the field from there. With controlled inputs this races persistence and can
truncate/reset text mid-edit (worst on long values like MACs). Lesson: **don't
round-trip a controlled text field through global state on every keystroke.**

### What we did (workaround)

1. **Edit-in-progress values live in `use_ref`, not `use_state`.** `HookRef` is
   `Rc<RefCell<T>>` — interior-mutable, no re-render needed to be correct.
   `on_text_changed` writes the ref; Save reads it. The boxes are effectively
   uncontrolled (WinUI holds the visible text), so the missing re-render no longer
   matters. This fixed the save bug.
2. **When an in-place visual update is genuinely required** (the "Resolve MAC"
   button, which populates the field for you), force a re-render by bumping an
   app-level counter threaded into the form's props: `form_tick: i32` in
   `FormProps` (part of its `PartialEq`). The changed prop makes the form's path
   unequal, so it re-renders and reads the updated ref.

Both are the same lesson: **you can't rely on a nested component re-rendering
itself via its own state; change something on its prop path instead.**

### Suggested upstream fixes

- Give `use_state` writes the same force-descent treatment context already gets
  (e.g. at the start of a reconcile, if any component instance has
  `peek_state_dirty()`, set `force_component_rerender` / seed `forced_components`).
- `debug_assert!` that no component instance is still dirty after a pass — a
  dropped re-render is almost always a bug and today fails silently.
- Document the constraint, and/or offer a first-class uncontrolled TextBox mode.

## 2. Window title-bar / taskbar icon

WinUI 3 does **not** adopt the exe's embedded icon for the window caption; the
taskbar icon updated from the embedded resource but the title-bar mini-icon stayed
the default placeholder.

Reactor doesn't expose a way to set it: there's no `.icon()` on the `App` builder,
and both `IAppWindow::SetIcon` and `IWindowNative::WindowHandle` are `pub(crate)`,
so the HWND reactor already holds internally is unreachable from app code.

### Workaround

Set it via Win32 once the window exists (see `src/ui/window_icon.rs`, called from
a one-shot `use_effect` in `src/ui/mod.rs`):

1. Find our top-level window on the UI thread with `EnumThreadWindows`
   (`GetCurrentThreadId`), matching by window title — `FindWindowW` by title
   returns nothing for WinUI windows.
2. `LoadImageW` the embedded icon (resource id `1`, from `assets/app.rc`) at small
   and large sizes.
3. Push both in with `SendMessageW(hwnd, WM_SETICON, ICON_SMALL/ICON_BIG, ...)`.

This needs the `Win32_System_LibraryLoader` and `Win32_System_Threading` features
on the `windows` dependency, in addition to `Win32_UI_WindowsAndMessaging`.

### Suggested upstream fix

Expose either the window HWND / `AppWindow` to app code, or a public `.icon()` on
the `App` builder, so callers don't have to re-find their own window and poke it
with Win32.

## 3. Swapping layouts orphans component subtrees — key them

For the responsive layout we switch between a wide dashboard (per-target cards +
chart) and a compact one (color legend + full-height chart), driven by
`cx.use_inner_size()` (which re-renders subscribers on resize — this part works
well).

First cut just returned a different `vstack` per mode with no keys. Crossing the
breakpoint left **orphaned sparkline surfaces** on screen: the reconciler diffed
the old tree against the new one positionally, and when a card `vstack` (5
children, incl. a `component(spark_view)`) was reconciled in place against a
shorter legend `hstack` (2 children), the trailing spark **component** subtree
wasn't fully torn down. So the compact view showed a couple of leftover
mini-charts where the legend should be.

Fix: give the two layouts distinct keys — `.with_key("dashboard-normal")` vs
`.with_key("dashboard-compact")`. A changed key makes the reconciler fully
unmount the old subtree and mount the new one instead of diffing in place, so no
orphans survive. The full remount only happens when actually crossing the
breakpoint, so the cost is negligible.

General lesson: **when you conditionally swap one subtree for a structurally
different one, key them** so the swap is a clean remount rather than an in-place
positional diff (which, at least here, doesn't reliably tear down nested
components when the child count shrinks).

## 4. No built-in app-level GPU context/recovery glue

`windows-canvas` now provides the rendering primitives we need (`GpuDevice`,
`CanvasImageSource`, drawing/session APIs), but it does **not** provide reactor
app glue for:

- sharing one device instance through app-specific context values,
- identity-based "device changed" dependency semantics for effect keys, and
- app-managed recovery signaling from leaf components back to the root.

That glue still lives in `src/device.rs` (`Device`, `Gpu`, `gpu_context()`): it
is intentionally thin, but still necessary to integrate canvas drawing with this
app's state/reconcile flow.
