mod animation;
mod application_watcher;
mod autostart;
mod backup;
mod custom_actions;
mod device_render;
mod device_sleep;
mod elgato;
mod encoder_layouts;
mod events;
mod frontend_events;
mod pages;
mod power_events;
#[cfg(debug_assertions)]
mod simulator;
mod shared;
mod store;
mod system_actions;
mod ui;

use shared::PRODUCT_NAME;

use std::future::Future;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowOptions, prelude::*, px, size};
use gpui_component::Root;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// What the compositor and the desktop entry's `StartupWMClass` identify the window by.
const APP_ID: &str = "rustydeck";

const SAVE_PROBE: Duration = Duration::from_secs(30);

/// Spawn a future onto the shared Tokio runtime from any thread - unlike ambient `tokio::spawn`,
/// this works even from a plain `std::thread::spawn`'d thread with no runtime context of its own
/// (the old code relied on `tauri::async_runtime::spawn` for that).
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
	F: std::future::Future + Send + 'static,
	F::Output: Send + 'static,
{
	RUNTIME.get().unwrap().spawn(future)
}

/// Run a backend async fn on the Tokio runtime and let a GPUI `cx.spawn` task await the result.
/// `events::frontend::*` needs real Tokio runtime context (`tokio::fs` etc.); GPUI's own executor
/// (used by `cx.spawn`) doesn't provide one. A `tokio::task::JoinHandle` is safely awaitable from
/// any executor, so this just bridges the two.
pub async fn bridge<Fut>(future: Fut) -> Fut::Output
where
	Fut: Future + Send + 'static,
	Fut::Output: Send + 'static,
{
	spawn(future).await.expect("backend task panicked")
}

/// Relaunch the binary as a fresh process and exit this one. The closest native equivalent to
/// Tauri's `AppHandle::restart`, callable from plain backend code with no GPUI context in hand.
pub fn restart_app() -> ! {
	if let Err(error) = RUNTIME.get().unwrap().block_on(store::profiles::flush_stale_profiles()) {
		log::error!("Failed to flush stale profiles before restart: {error}");
	}
	if let Ok(exe) = std::env::current_exe()
		&& let Err(error) = std::process::Command::new(exe).spawn()
	{
		log::error!("Failed to relaunch: {error}");
	}
	std::process::exit(0);
}

fn tray_icon_image() -> tray_icon::Icon {
	let bytes = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/icon.png"));
	let image = image::load_from_memory(bytes).expect("bundled tray icon is a valid image").into_rgba8();
	let (width, height) = image.dimensions();
	tray_icon::Icon::from_rgba(image.into_raw(), width, height).expect("bundled tray icon has valid dimensions")
}

/// The tray icon and its menu need a running GTK event loop on Linux, independent of GPUI's own
/// platform loop - see `tray-icon`'s crate docs. This thread owns that loop for the process
/// lifetime; the tray/menu objects only ever live here, and so does everything that reads or
/// writes them - both draining their event channels and keeping their enabled state current.
fn spawn_tray_thread() {
	std::thread::spawn(|| {
		if let Err(error) = gtk::init() {
			log::error!("Failed to initialise GTK for the tray icon: {error}");
			return;
		}

		use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};

		let label = MenuItem::with_id("label", PRODUCT_NAME, false, None);
		let show = MenuItem::with_id("show", "Show", true, None);
		let hide = MenuItem::with_id("hide", "Hide", true, None);
		let login = MenuItem::with_id("login", login_label(autostart::is_enabled()), true, None);
		let restart = MenuItem::with_id("restart", "Restart", true, None);
		let quit = MenuItem::with_id("quit", "Quit", true, None);

		let menu = Menu::new();
		let _ = menu.append(&label);
		let _ = menu.append(&PredefinedMenuItem::separator());
		let _ = menu.append(&show);
		let _ = menu.append(&hide);
		let _ = menu.append(&PredefinedMenuItem::separator());
		let _ = menu.append(&login);
		let _ = menu.append(&restart);
		let _ = menu.append(&quit);

		// Keep the two window items honest about what they can do. With the window already open
		// "Show" has nothing left to do - a Wayland compositor is free to refuse a raise request and
		// Hyprland does - and with it hidden "Hide" has nothing to do either. Greying the one that
		// does not apply is also the only way the tray can say which state the app is in.
		let (show_item, hide_item, login_item) = (show.clone(), hide.clone(), login.clone());
		let mut shown = autostart::is_enabled();
		let mut tick = 0u32;
		gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
			let showing = WINDOW_WANTED.load(Ordering::Relaxed);
			show_item.set_enabled(!showing);
			hide_item.set_enabled(showing);

			// Right after a toggle so the label answers the click, and on a slow tick besides,
			// because unlike the flag above this state is a file on disk that can be changed from
			// outside the app.
			tick = tick.wrapping_add(1);
			if poll_tray_events() || tick.is_multiple_of(40) {
				let enabled = autostart::is_enabled();
				if enabled != shown {
					shown = enabled;
					login_item.set_text(login_label(enabled));
				}
			}

			gtk::glib::ControlFlow::Continue
		});

		let _tray = match tray_icon::TrayIconBuilder::new()
			.with_id("rustydeck")
			.with_menu(Box::new(menu))
			.with_icon(tray_icon_image())
			.with_tooltip(PRODUCT_NAME)
			.with_menu_on_left_click(false)
			.build()
		{
			Ok(tray) => tray,
			Err(error) => {
				log::error!("Failed to create tray icon: {error}");
				return;
			}
		};

		gtk::main();
	});
}

/// What the process should be doing, decided by the tray, the window's close button and the
/// compositor, and acted on by the window loop in `main`.
///
/// These are plain flags rather than a channel because whoever sets them may be running when there
/// is no GPUI application at all - the tray thread outlives every window - and because the window
/// loop needs to be able to ask "is a window wanted right now?" rather than replay a queue.
static WINDOW_WANTED: AtomicBool = AtomicBool::new(true);
static RAISE_WANTED: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);

/// The thread running the window loop, so a request can wake it out of `park` immediately.
static WINDOW_LOOP: OnceLock<std::thread::Thread> = OnceLock::new();

fn wake_window_loop() {
	if let Some(thread) = WINDOW_LOOP.get() {
		thread.unpark();
	}
}

/// Ask for the window: opened if there is none, brought forward if there is.
fn request_show() {
	RAISE_WANTED.store(true, Ordering::Relaxed);
	WINDOW_WANTED.store(true, Ordering::Relaxed);
	wake_window_loop();
}

/// Ask for the window to go away, leaving the process running for the deck and the tray.
fn request_hide() {
	WINDOW_WANTED.store(false, Ordering::Relaxed);
	wake_window_loop();
}

pub fn request_quit() {
	QUITTING.store(true, Ordering::Relaxed);
	WINDOW_WANTED.store(false, Ordering::Relaxed);
	wake_window_loop();
}

/// What the window's own close control and the compositor's close request both mean: put the
/// window away, and stop the app only if background running has been turned off.
pub fn dismiss_window() {
	if store::get_settings().value.background { request_hide() } else { request_quit() }
}

fn toggle_window() {
	if WINDOW_WANTED.load(Ordering::Relaxed) { request_hide() } else { request_show() }
}

/// The run-at-login item says its state in its own text.
///
/// A checkmark would be the obvious way to show it, and `muda` exports one correctly, but whether
/// anything is drawn for it is up to the tray - and some, quickshell among them, draw nothing. Text
/// is the one thing every tray renders.
fn login_label(enabled: bool) -> String {
	format!("Run at login: {}", if enabled { "on" } else { "off" })
}

/// Drain the tray icon's and menu's global event channels, reporting whether run-at-login changed.
///
/// `tray-icon`/`muda` deliver events through process-global channels meant to be polled with
/// `try_recv` by the embedding app. This used to be polled on GPUI's executor, which only exists
/// while a window does - so the tray stopped responding exactly when it became the only way back
/// in. GTK's loop, which owns the tray for the lifetime of the process, is the one that never goes
/// away.
fn poll_tray_events() -> bool {
	let mut toggled = false;

	while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
		if let tray_icon::TrayIconEvent::Click {
			button: tray_icon::MouseButton::Left,
			button_state: tray_icon::MouseButtonState::Down,
			..
		} = event
		{
			toggle_window();
		}
	}

	while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
		match event.id() {
			id if id == "show" => request_show(),
			id if id == "hide" => request_hide(),
			// The file on disk is the state, so the label is refreshed from it afterwards rather
			// than assumed - a write that fails then shows as the label not moving.
			id if id == "login" => {
				let wanted = !autostart::is_enabled();
				if let Err(error) = autostart::set(wanted) {
					log::error!("Failed to turn running at login {}: {error:#}", if wanted { "on" } else { "off" });
				}
				toggled = true;
			}
			id if id == "restart" => restart_app(),
			id if id == "quit" => request_quit(),
			_ => {}
		}
	}

	toggled
}

/// Act on show/hide requests from inside the running application, where a `Window` can be reached.
fn watch_window_requests(window: WindowHandle<Root>, cx: &mut App) {
	cx.spawn(async move |cx| {
		loop {
			cx.background_executor().timer(Duration::from_millis(100)).await;

			if !WINDOW_WANTED.load(Ordering::Relaxed) {
				let _ = cx.update(|cx| window.update(cx, |_, window, _| window.remove_window()));
				return;
			}

			if RAISE_WANTED.swap(false, Ordering::Relaxed) {
				let _ = cx.update(|cx| window.update(cx, |_, window, _| window.activate_window()));
			}
		}
	})
	.detach();
}

/// Open the window and run GPUI until that window closes, then return.
///
/// Closing the last window ends GPUI's event loop on Linux, so one call to this is one visible
/// session of the app rather than the lifetime of the process. Everything that has to keep running
/// while the window is away - the devices, the animations, the tray - is started by `main` before
/// this is ever called, and none of it is touched by the application being torn down and rebuilt.
fn run_window_session() {
	// `gpui_component_assets::Assets` serves the icon SVGs its components reference; the crate
	// deliberately ships none itself, expecting the host app to register an asset source.
	Application::new().with_assets(gpui_component_assets::Assets).run(|cx: &mut App| {
		gpui_component::init(cx);
		// The component library defaults to light; the deck view is dark artwork on dark keys, so
		// the chrome matches it rather than framing it in white.
		gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);

		let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
		// Nearly all of the delay between asking for the window and seeing it is inside this call -
		// GPUI building its renderer and text system from scratch, the same ~1.8s the app pays on a
		// cold start. Worth keeping an eye on, since it is now paid on every Show.
		let _open = shared::Timed::start("open_window");
		let window = cx
			.open_window(
				WindowOptions {
					window_bounds: Some(WindowBounds::Windowed(bounds)),
					titlebar: Some(TitlebarOptions {
						title: Some(PRODUCT_NAME.into()),
						..Default::default()
					}),
					// What the compositor identifies the window by: window rules, taskbar grouping
					// and the `StartupWMClass` in our desktop entry all match on this, and without
					// it the window arrives anonymous and none of them can name it.
					app_id: Some(APP_ID.into()),
					// Declared transparent purely to halve how long the window takes to appear.
					// GPUI always creates a Wayland surface with `transparent: true` and compiles
					// the whole Blade pipeline set against it, then on the first frame notices an
					// opaque window and destroys and recompiles the lot - about 0.9s of shader
					// compilation, paid twice. Agreeing with how the surface was created skips the
					// second pass. The shell paints an opaque background over the full window, so
					// nothing shows through.
					// ... and paired with client-side decorations, which is the other half of it:
					// with server-side decorations the compositor's answer arrives after the window
					// is built and flips the surface opaque, costing a third compile. Hyprland draws
					// no titlebar either way, so nothing is given up by owning them.
					window_background: WindowBackgroundAppearance::Transparent,
					window_decorations: Some(WindowDecorations::Client),
					..Default::default()
				},
				|window, cx| {
					// `TitlebarOptions::title` is only read by the macOS backend; on Wayland and X11
					// the title has to be set on the window itself or it stays empty.
					window.set_window_title(PRODUCT_NAME);
					window.on_window_should_close(cx, |_, _| {
						// Always let the window close. Refusing and minimising instead used to leave
						// it on screen for good on Wayland: `xdg_toplevel.set_minimized` is a request
						// a compositor may ignore, and Hyprland, having no concept of minimised, does.
						// Closing is the one dismissal every desktop implements, so the window is
						// closed for real and the process stays alive behind the tray.
						dismiss_window();
						true
					});
					let shell = cx.new(|cx| ui::RustyDeckShell::new(window, cx));
					cx.new(|cx| Root::new(shell, window, cx))
				},
			)
			.expect("failed to open the main window");

		drop(_open);

		cx.activate(true);
		watch_window_requests(window, cx);
	});
}

/// Start everything that serves the deck, independently of whether a window is open.
fn start_backend() {
	spawn(async {
		loop {
			elgato::initialise_devices().await;
			tokio::time::sleep(Duration::from_secs(10)).await;
		}
	});
	// Simulated devices exist only in debug builds, so a release binary offers real hardware
	// and nothing else.
	#[cfg(debug_assertions)]
	spawn(simulator::register_all());
	spawn(async {
		loop {
			tokio::time::sleep(SAVE_PROBE).await;
			if let Err(error) = store::profiles::flush_stale_profiles().await {
				log::error!("Failed to flush stale profiles: {error}");
			}
		}
	});
	application_watcher::init_application_watcher();
	device_sleep::init_device_sleep();
	power_events::init_power_events();
}

fn main() {
	log_panics::init();
	let _ = fix_path_env::fix();

	// Before the logger, which is constructed from `log_dir()` inside this tree.
	shared::initialise_config_dir();

	let runtime = tokio::runtime::Runtime::new().expect("failed to start the Tokio runtime");
	let _enter = runtime.enter();
	RUNTIME.set(runtime).ok();

	let log_dir = shared::log_dir();
	let _ = std::fs::create_dir_all(&log_dir);
	let log_file = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("rustydeck.log"));
	let file_logger = log_file.ok().map(|file| simplelog::WriteLogger::new(log::LevelFilter::Debug, simplelog::Config::default(), file));
	let term_logger = simplelog::TermLogger::new(log::LevelFilter::Info, simplelog::Config::default(), simplelog::TerminalMode::Mixed, simplelog::ColorChoice::Auto);
	let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = vec![term_logger];
	if let Some(file_logger) = file_logger {
		loggers.push(file_logger);
	}
	let _ = simplelog::CombinedLogger::init(loggers);

	// How the autostart entry launches us: a login is not a request to be looked at, so the deck is
	// served from the tray and the window waits until it is asked for.
	if std::env::args().any(|argument| argument == "--hidden") {
		WINDOW_WANTED.store(false, Ordering::Relaxed);
	}

	start_backend();
	spawn_tray_thread();

	WINDOW_LOOP.set(std::thread::current()).ok();
	while !QUITTING.load(Ordering::Relaxed) {
		if WINDOW_WANTED.load(Ordering::Relaxed) {
			run_window_session();
		} else {
			// Hidden: the deck is being served by the background tasks and the tray is the way back
			// in. Parking rather than spinning costs nothing; the timeout is only a backstop in case
			// a request lands between the check above and the park.
			std::thread::park_timeout(Duration::from_secs(1));
		}
	}

	if let Err(error) = RUNTIME.get().unwrap().block_on(store::profiles::flush_stale_profiles()) {
		log::error!("Failed to flush stale profiles on exit: {error}");
	}
	RUNTIME.get().unwrap().block_on(elgato::reset_devices());
}
