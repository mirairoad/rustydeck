# RustyDeck

A native Linux app for Elgato Stream Deck devices, written in Rust with
[GPUI](https://www.gpui.rs/) - no webview.

## Where this came from

RustyDeck vendors and builds on top of
[OpenDeck](https://github.com/nekename/OpenDeck) by Aman Khanna (nekename).
Its Rust backend is reused largely as-is:

- device I/O over [`elgato-streamdeck`](https://crates.io/crates/elgato-streamdeck)
- the Stream Deck / OpenAction plugin protocol
- profile and settings storage
- the bundled `com.amansprojects.starterpack` plugin

What is new here is the frontend: OpenDeck's Svelte/Tauri WebView UI is replaced
by a native GPUI shell, and the app is Linux-only.

Licensed GPL-3.0-or-later, as OpenDeck is. See [LICENSE.md](LICENSE.md).
