# RustyDeck

A native Linux app for Elgato Stream Deck devices, written in Rust with
[GPUI](https://www.gpui.rs/) - no webview, no plugin runtime, one binary.

## What it does

- Lays out keys, touch-strip rectangles and dials for every Stream Deck model
- Runs shell commands on a key press, a strip tap, and each direction of a dial
- Pages, so one deck holds several layouts and you step between them
- A library of custom actions with their own artwork, dragged onto any slot

## How a deck is modelled

The physical controls fall into two halves, and they are configured separately:

| control | scope | gestures |
| --- | --- | --- |
| **Dial** | per device - survives page changes | turn each way, press |
| **Rectangle** above a dial | per page | tap; owns the artwork |
| **Key** | per page | press; owns the artwork |

A dial is a fixed control, so it keeps doing the same thing whichever page is
showing. The rectangle above it is the page-scoped half. They no longer share a
slot, so neither can overwrite the other.

Artwork is composed twice from the same source - a square face for keys and a
2:1 face for the strip - because scaling one into the other stretches it.

## Actions

Everything is implemented in-process. Commands run through the user's login
shell (`$SHELL -lic`) rather than `sh -c`, so aliases and shell functions
resolve the way they do in a terminal - on Omarchy, for instance, `open` is a
bash function that a non-interactive POSIX shell would never see.

There is no plugin system and no marketplace. Actions are first-party, and
adding one means writing it here.

## Building

```sh
cd src
cargo build --release      # ~26 MiB binary
cargo run                  # debug build, with the simulator
```

Linux only. The release profile is tuned for size: `opt-level = "s"`, fat LTO,
one codegen unit, symbols stripped.

## The simulator

Debug builds register a simulated deck for every model - Mini, Stream Deck,
MK.2, XL, +, XL+, Neo and Pedal - so layouts can be built and actions driven
without the hardware plugged in. They appear in the device picker under a
`Simulated` heading, tinted purple.

Simulated input enters at the same points the driver uses, so the debounce, the
dial config and the command that runs are the code paths real hardware takes.
Their ids are prefixed `sim-` rather than `sd-`, which is what keeps every
hardware write from reaching a device that is not there.

None of it is compiled into a release build.

## Where this came from

RustyDeck began as a fork of
[OpenDeck](https://github.com/nekename/OpenDeck) by Aman Khanna (nekename), and
still uses its device I/O over
[`elgato-streamdeck`](https://crates.io/crates/elgato-streamdeck) and its
profile and settings storage.

Two things have since diverged. OpenDeck's Svelte/Tauri webview UI is replaced
by a native GPUI shell, and the Stream Deck / OpenAction plugin protocol is
gone - the WebSocket transport, the property-inspector webserver, plugin
installation and the bundled `com.amansprojects.starterpack` plugin process were
all removed, and the one action anyone used became internal.

Licensed GPL-3.0-or-later, as OpenDeck is. See [LICENSE.md](LICENSE.md).
