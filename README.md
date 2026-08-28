# RustyDeck

A native Linux app for Elgato Stream Deck devices, written in Rust with
[GPUI](https://www.gpui.rs/). No webview, no plugin runtime, one 26 MiB binary.

## What it does

- Lays out keys, touch-strip rectangles and dials for every Stream Deck model
- Runs shell commands on a key press, a strip tap, and each direction of a dial
- Pages, so one deck holds several layouts and you step between them
- A library of custom actions with their own artwork, dragged onto any slot

## How a deck is modelled

The physical controls fall into two halves, configured separately:

| control | scope | gestures |
| --- | --- | --- |
| **Dial** | per device - survives page changes | turn each way, press |
| **Rectangle** above a dial | per page | tap; owns the artwork |
| **Key** | per page | press; owns the artwork |

A dial is a fixed control, so it keeps doing the same thing whichever page is
showing. The rectangle above it is the page-scoped half. They do not share a
slot, so neither can overwrite the other.

Dials are rearranged by dragging one knob onto another, which exchanges the two -
dropping onto an unset dial is how one gets moved. A dial running your own
commands can be given a name to caption the knob with; leave it blank and it
stays "Custom".

Artwork is composed twice from one source - a square face for keys and a 2:1
face for the strip - because scaling one into the other stretches it. The create
dialog previews both.

## Actions

Everything runs in-process. Commands go through the user's login shell
(`$SHELL -lic`) rather than `sh -c`, so aliases and shell functions resolve the
way they do in a terminal - on Omarchy, for instance, `open` is a bash function
that a non-interactive POSIX shell would never see.

There is no plugin system and no marketplace. Actions are first-party; adding
one means writing it here.

## Building

```sh
cd src
cargo build --release        # target/release/rustydeck, ~26 MiB
```

Linux only. The release profile is tuned for size: `opt-level = "s"`, fat LTO,
one codegen unit, symbols stripped. That last stretch pegs a single core - it is
worth about 5 MB of binary against 5 seconds of build time, so it stays.

Debug builds compile dependencies optimised (`[profile.dev.package."*"]`). The
image codecs are the hot path when composing artwork and are roughly eighty
times slower unoptimised, which is enough to make a large photo feel broken in
development. Your own crate stays unoptimised and debuggable.

## Installing

```sh
sudo install -Dm755 src/target/release/rustydeck /usr/local/bin/rustydeck
sudo install -Dm644 src/bundle/40-streamdeck.rules /usr/lib/udev/rules.d/40-streamdeck.rules
sudo install -Dm644 src/bundle/rustydeck.desktop  /usr/share/applications/rustydeck.desktop
sudo install -Dm644 src/icons/icon.png            /usr/share/icons/hicolor/512x512/apps/rustydeck.png
sudo udevadm control --reload-rules && sudo udevadm trigger
```

The udev rules are what let the app open the device without root. Replug the
deck after installing them.

Configuration lives in `~/.rustydeck`: `profiles/` (one file per device plus a
directory of pages), `customs/` and `predefined/` (action libraries, each action
a directory with its config and artwork), and `settings.json`.

## The simulator

Debug builds register a simulated deck for every model - Mini, Stream Deck,
MK.2, XL, +, XL+, Neo and Pedal - so layouts can be built and actions driven
with nothing plugged in. They appear in the device picker under a `Simulated`
heading, tinted purple, and each knob gets `◀ ● ▶` controls.

Simulated input enters at the same points the driver uses, so the debounce, the
dial config and the command that runs are the code paths real hardware takes.
Ids are prefixed `sim-` rather than `sd-`, which is what stops any hardware
write reaching a device that is not there.

None of it is compiled into a release build.

## Development

`shared::Timed` logs how long a step took and compiles its logging out of
release builds:

```rust
let _timed = Timed::start("compose");
```

Timings appear as `[timing] …` lines. Worth reaching for before optimising
anything: the artwork pipeline turned out to be slow for reasons nobody guessed
correctly, and the numbers settled it in one run.

`.claude/skills/gpui-patterns/` collects the GPUI rules this UI was built on -
z-ordering, threading, dialog state, and the traps that cost time here.

## Where this came from

RustyDeck began as a fork of
[OpenDeck](https://github.com/nekename/OpenDeck) by Aman Khanna (nekename), and
still uses its device I/O over
[`elgato-streamdeck`](https://crates.io/crates/elgato-streamdeck) and its
profile and settings storage.

Two things have since diverged. OpenDeck's Svelte/Tauri webview UI is replaced
by a native GPUI shell, and the Stream Deck / OpenAction plugin protocol is gone
- the WebSocket transport, the property-inspector webserver, plugin installation
and the bundled `com.amansprojects.starterpack` plugin process were all removed,
and the one action anyone used became internal.

Licensed GPL-3.0-or-later, as OpenDeck is. See [LICENSE.md](LICENSE.md).
