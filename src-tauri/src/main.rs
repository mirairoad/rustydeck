mod application_watcher;
mod custom_actions;
mod device_render;
mod device_sleep;
mod elgato;
mod encoder_layouts;
mod events;
mod frontend_events;
mod plugins;
mod power_events;
mod shared;
mod store;
mod ui;
mod zip_extract;

mod built_info {
	include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

use events::frontend;
use shared::PRODUCT_NAME;

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, prelude::*, px, size};
use gpui_component::Root;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
/// gpui-component requires the window's root view to be its `Root`, which hosts dialogs,
/// notifications and popovers - so the handle is to that rather than to our own shell view.
static MAIN_WINDOW: OnceLock<WindowHandle<Root>> = OnceLock::new();

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
pub fn bridge<Fut>(future: Fut) -> impl Future<Output = Fut::Output>
where
	Fut: Future + Send + 'static,
	Fut::Output: Send + 'static,
{
	async move { spawn(future).await.expect("backend task panicked") }
}

/// Relaunch the binary as a fresh process and exit this one. The closest native equivalent to
/// Tauri's `AppHandle::restart`, callable from plain backend code with no GPUI context in hand.
pub fn restart_app() -> ! {
	if let Err(error) = RUNTIME.get().unwrap().block_on(store::profiles::flush_stale_profiles()) {
		log::error!("Failed to flush stale profiles before restart: {error}");
	}
	if let Ok(exe) = std::env::current_exe() {
		if let Err(error) = std::process::Command::new(exe).spawn() {
			log::error!("Failed to relaunch: {error}");
		}
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
/// lifetime; the tray/menu objects only ever live here. Tray/menu *events* still reach GPUI via
/// the crate's global channels, polled from `poll_tray_events` on the GPUI foreground executor.
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
		let restart = MenuItem::with_id("restart", "Restart", true, None);
		let quit = MenuItem::with_id("quit", "Quit", true, None);

		let menu = Menu::new();
		let _ = menu.append(&label);
		let _ = menu.append(&PredefinedMenuItem::separator());
		let _ = menu.append(&show);
		let _ = menu.append(&hide);
		let _ = menu.append(&PredefinedMenuItem::separator());
		let _ = menu.append(&restart);
		let _ = menu.append(&quit);

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

/// Poll the tray icon's and menu's global event channels on GPUI's own foreground executor.
/// `tray-icon`/`muda` deliver events through process-global channels meant to be polled with
/// `try_recv` from the embedding app's own event loop, so this fits GPUI's cooperative
/// single-threaded model without needing to bridge threads for every click.
fn poll_tray_events(cx: &mut App) {
	cx.spawn(async move |cx| {
		loop {
			cx.background_executor().timer(Duration::from_millis(100)).await;

			while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
				if let tray_icon::TrayIconEvent::Click {
					button: tray_icon::MouseButton::Left,
					button_state: tray_icon::MouseButtonState::Down,
					..
				} = event
				{
					let _ = cx.update(|cx| toggle_main_window(cx));
				}
			}

			while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
				match event.id() {
					id if id == "show" => {
						let _ = cx.update(|cx| show_main_window(cx));
					}
					id if id == "hide" => {
						let _ = cx.update(|cx| hide_main_window(cx));
					}
					id if id == "restart" => restart_app(),
					id if id == "quit" => {
						let _ = cx.update(|cx| cx.quit());
					}
					_ => {}
				}
			}
		}
	})
	.detach();
}

fn show_main_window(cx: &mut App) {
	let _ = MAIN_WINDOW.get().unwrap().update(cx, |_, window, _| window.activate_window());
}

fn hide_main_window(cx: &mut App) {
	let _ = MAIN_WINDOW.get().unwrap().update(cx, |_, window, _| window.minimize_window());
}

fn toggle_main_window(cx: &mut App) {
	let active = MAIN_WINDOW.get().unwrap().update(cx, |_, window, _| window.is_window_active()).unwrap_or(false);
	if active {
		hide_main_window(cx);
	} else {
		show_main_window(cx);
	}
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

	// `gpui_component_assets::Assets` serves the icon SVGs its components reference; the crate
	// deliberately ships none itself, expecting the host app to register an asset source.
	Application::new().with_assets(gpui_component_assets::Assets).run(|cx: &mut App| {
		gpui_component::init(cx);
		// The component library defaults to light; the deck view is dark artwork on dark keys, so
		// the chrome matches it rather than framing it in white.
		gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);

		let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
		let window = cx
			.open_window(
				WindowOptions {
					window_bounds: Some(WindowBounds::Windowed(bounds)),
					titlebar: Some(TitlebarOptions {
						title: Some(PRODUCT_NAME.into()),
						..Default::default()
					}),
					..Default::default()
				},
				|window, cx| {
					window.on_window_should_close(cx, |window, cx| {
						if store::get_settings().value.background {
							window.minimize_window();
							false
						} else {
							cx.quit();
							true
						}
					});
					let shell = cx.new(|cx| ui::OpenDeckShell::new(window, cx));
					cx.new(|cx| Root::new(shell, window, cx))
				},
			)
			.expect("failed to open the main window");
		MAIN_WINDOW.set(window).ok();

		cx.activate(true);

		// Backend subsystems - unchanged in substance from the old Tauri `.setup()` closure, just
		// invoked directly instead of from inside Tauri's builder.
		spawn(async {
			loop {
				elgato::initialise_devices().await;
				tokio::time::sleep(Duration::from_secs(10)).await;
			}
		});
		spawn(async {
			loop {
				tokio::time::sleep(SAVE_PROBE).await;
				if let Err(error) = store::profiles::flush_stale_profiles().await {
					log::error!("Failed to flush stale profiles: {error}");
				}
			}
		});
		plugins::initialise_plugins();
		application_watcher::init_application_watcher();
		device_sleep::init_device_sleep();
		power_events::init_power_events();

		spawn_tray_thread();
		poll_tray_events(cx);

		cx.on_app_quit(|_cx| async {
			if let Err(error) = store::profiles::flush_stale_profiles().await {
				log::error!("Failed to flush stale profiles on exit: {error}");
			}
			elgato::reset_devices().await;
		})
		.detach();
	});
}
