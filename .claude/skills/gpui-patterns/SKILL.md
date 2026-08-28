---
name: gpui-patterns
description: >
  Patterns and traps for building desktop UI with GPUI and gpui-component. Use when
  writing or debugging a GPUI view, dialog, overlay or menu; when something renders
  behind other content, shows through, or will not update; when the window freezes or
  stutters; or when a closure will not compile against `cx`. Covers z-ordering,
  translucency, dialog state, event subscription, threading, and image assets.
---

# GPUI patterns

Hard-won rules from building a native GPUI shell. Each one cost real debugging
time; the reasoning matters more than the snippet, so it is included.

## Z-ordering: anything that floats needs `deferred`

An absolutely-positioned child still takes part in normal paint order. It will
slide **behind** later siblings as soon as the window is small enough for them
to overlap - which is exactly when a dropdown matters most.

```rust
deferred(
    v_flex()
        .occlude()          // clicks stop here instead of falling through
        .absolute()
        .top(px(49.0))
        .right(px(5.0))
        .bg(opaque(cx.theme().background))
        .shadow_lg()
        .children(rows),
)
.with_priority(1)           // painted after everything ordinary
```

Use this for every dropdown, popover and right-click menu. `occlude` is not
optional: without it the click reaches whatever sits underneath as well.

**Symptom to recognise:** the overlay looks fine in a maximised window and
disappears behind content when the window shrinks.

## Translucency: theme colours carry alpha

`cx.theme().background` has alpha. That is correct for the window itself and
wrong for a surface floating over the app's own content - the content shows
through the panel and muddies its text.

```rust
/// A theme colour forced fully opaque.
fn opaque(colour: gpui::Hsla) -> gpui::Hsla {
    gpui::Hsla { a: 1.0, ..colour }
}
```

Apply to floating panels only. Leave the root and header translucent or you
throw away the window's whole look.

**Symptom to recognise:** a menu's own coloured rows look fine while its
background panel looks washed out - the rows paint their own opaque colour.

## Dialog state lives in its own entity

A `window.open_dialog` builder closure runs **inside the parent view's own
`render`**, via `Root::render_dialog_layer`. Reading the parent from there
panics with *"cannot read while it is already being updated"*.

Put form state in a separate `Entity` that both the view and the builder can
read:

```rust
struct ActionForm {
    name: Entity<InputState>,
    error: Option<SharedString>,
    saving: bool,
}
```

Two more dialog traps:

- **Footer buttons only render when `.footer()` is set.** `button_props` alone
  sets labels, so Save silently does not appear.
- **A `Select` inside a dialog will not redraw the dialog on its own.** The
  builder runs from the parent's render, so subscribe and notify the parent:

```rust
cx.subscribe(&select_state, |_this, _state, _event: &SelectEvent<Vec<Choice>>, cx| cx.notify())
    .detach();
```

## Subscribe before the first read

Reading state and *then* subscribing loses every event emitted in between, and
nothing arrives later to correct it.

```rust
cx.spawn(async move |this, cx| {
    let mut events = frontend_events::subscribe();   // first
    refresh_catalogue(&this, cx).await;              // then
    while let Ok(event) = events.recv().await { … }
})
```

This cost a day here: devices registering during startup emitted into a channel
with no receiver, so whichever won the race was the one you were stuck with.

## `cx.spawn` is the foreground executor

`cx.spawn` runs on the thread that paints the window. Any CPU work in there
freezes the UI. Decoding an image, compositing, encoding - all of it must go to
a worker:

```rust
let result = crate::bridge(async move {
    tokio::task::spawn_blocking(move || do_expensive_thing()).await
})
.await;
```

Then apply the result back through `this.update(cx, …)`.

**Symptom to recognise:** the window stops responding *and* every later
interaction appears to do nothing - users report it as "I can only do this
once", because their subsequent clicks queued behind the frozen frame.

Give long work a visible state (`saving: bool` → "Working…") and refuse a second
submit while one is in flight.

## Async that needs a `Window`

`AsyncApp` does not implement `VisualContext`, so `update_in` will not compile
from a plain `cx.spawn`. Use `spawn_in`:

```rust
cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
    let _ = this.update_in(cx, |this, window, cx| this.open_dialog(window, cx));
})
.detach();
```

## Images

- **Evict the cache when rewriting a path.** GPUI keys decoded bitmaps by path,
  so re-saving `picture.png` keeps serving the old pixels:

```rust
let source: gpui::ImageSource = path.into();
source.remove_asset(cx);
```

- **Set `object_fit` deliberately.** The default will not match whatever your
  compositor does. `ObjectFit::Cover` crops to fill, `Contain` fits inside.
  A preview that lies about the crop is worse than no preview.

## Closures and `cx`

Two failure shapes, both common:

- **`move` closures capturing `cx`.** `cx.listener(…)` and `cx.theme()` inside a
  `move` closure will fight over `cx`. Drop the `move`, or hoist the values out
  first.
- **Lazy iterators borrowing block locals.** `.children({ … iter.map(…) })`
  returns a lazy adaptor that outlives the block's locals. Build a `Vec` inside
  the block and return that.

## Debug-only features without `cfg` noise

Gate the module, then wrap it in a shim that folds away. Call sites stay
unconditional and neither profile warns about unused variables:

```rust
fn simulate_rotate(device: &str, dial: u8, ticks: i16) {
    #[cfg(debug_assertions)]
    crate::simulator::rotate(device, dial, ticks);
    #[cfg(not(debug_assertions))]
    let _ = (device, dial, ticks);
}
```

A `#[cfg]` body inside a closure still lets the closure capture its arguments,
so the release build warns about them - the shim is what avoids that.

## Build profile

```toml
[profile.dev.package."*"]
opt-level = 3          # dependencies optimised; your crate stays debuggable
```

Image codecs, compression and parsers run ~80x slower unoptimised. Without this,
work that is fine in release looks broken in development and you will optimise
the wrong thing.

## Measure before optimising

```rust
pub struct Timed { label: Cow<'static, str>, start: Instant }
impl Drop for Timed {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        log::info!("[timing] {} took {:.1?}", self.label, self.start.elapsed());
        #[cfg(not(debug_assertions))]
        let _ = (&self.label, self.start);
    }
}
```

`let _timed = Timed::start("step");` at the top of a function. Here it showed
that a "slow because the file is big" theory was wrong - a 1.3MB JPEG was 13x
the work of a 3.5MB PNG because it held 13x the pixels. Nobody guesses that.

## Verifying UI you cannot click

Screenshot the window (`grim -g` on Wayland, geometry from `hyprctl clients -j`)
and **look at it** - a blank frame is a failed launch. For state behind an
interaction, temporarily force it in the constructor, screenshot, then revert:

```rust
device_picker_open: true, // TEMP-VERIFY
```

Grep the marker to prove it is gone before committing. This caught a real bug: a
picker that auto-selected the wrong device, which no amount of reading the code
had revealed.

## Editing GPUI code with regex: don't

Deeply nested builder chains defeat line-oriented patterns. A regex meant to
delete one match arm ran greedily to the next occurrence hundreds of lines later
and took an unrelated struct with it. Prefer exact-string replacement of a whole
block, and if a file is already committed, `git checkout HEAD -- file` and
re-apply is faster than repairing it.
