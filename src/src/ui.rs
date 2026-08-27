//! The GPUI-native main shell: a header carrying device identity, a collapsible action palette,
//! and the device's slots with drag-to-reposition and drag-to-swap (the defect this rewrite exists
//! to fix - see PRD §3).

use crate::custom_actions::{self, CustomAction, ImageSpec};
use crate::device_render::{render_state, resolve_image_path};
use crate::events::frontend;
use crate::frontend_events::{self, FrontendEvent};
use crate::shared::{Action, ActionInstance, Category, Context as SlotContext, DeviceInfo, Profile};

use std::rc::Rc;

use gpui::{
    App, Context, Div, Entity, IntoElement, ParentElement, Render, RenderOnce, SharedString, Stateful, Styled, WeakEntity, Window, div, img,
    prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Collapsible, IconName, StyledExt,
    button::Button,
    color_picker::{ColorPicker, ColorPickerState},
    dialog::DialogButtonProps,
    WindowExt, h_flex,
    input::{Input, InputState},
    sidebar::{Sidebar, SidebarGroup},
    v_flex,
};

/// The bundled starterpack action a custom action is built on - it is what actually runs the
/// shell command (see `plugins/com.amansprojects.starterpack.sdPlugin/src/run_command.rs`).
const RUN_COMMAND_UUID: &str = "com.amansprojects.starterpack.runcommand";

const CELL_SIZE: f32 = 72.0;
const GAP: f32 = 8.0;

/// Below this window width the palette collapses to an icon-only rail. Derived from the viewport
/// each frame rather than stored, so resizing is live.
const SIDEBAR_BREAKPOINT: f32 = 900.0;

const KEYPAD_CONTROLLER: &str = "Keypad";
const ENCODER_CONTROLLER: &str = "Encoder";
const INFOBAR_CONTROLLER: &str = "Infobar";

/// Sizes of the physical displays each controller writes to. Keys are square, but the touch strip
/// is one wide LCD written a 200x100 region at a time (one per dial), and the Neo's infobar is a
/// letterbox - so each kind has to be composited at its own aspect, not scaled from a square.
const KEY_IMAGE: (u32, u32) = (144, 144);
const ENCODER_IMAGE: (u32, u32) = (200, 100);
const INFOBAR_IMAGE: (u32, u32) = (248, 58);

/// A profile keeps one array per controller kind; which one a slot lives in is decided by its
/// controller, exactly as the old frontend did it. Touchpoints share the keypad array, appended
/// after the keys.
fn slots<'a>(profile: &'a Profile, controller: &str) -> &'a Vec<Option<ActionInstance>> {
    match controller {
        ENCODER_CONTROLLER => &profile.sliders,
        INFOBAR_CONTROLLER => &profile.infobars,
        _ => &profile.keys,
    }
}

/// An existing instance being dragged between slots.
#[derive(Clone)]
struct DraggedKey {
    context: SlotContext,
    image: Option<String>,
}

/// A palette entry being dragged onto the grid to create a new instance.
#[derive(Clone)]
struct DraggedAction {
    action: Action,
}

/// A user-defined action being dragged onto the grid.
#[derive(Clone)]
struct DraggedCustomAction {
    action: CustomAction,
}

/// Transient state of the create/edit form.
///
/// Deliberately its own entity rather than fields on the shell: the dialog's builder closure runs
/// inside `Root::render_dialog_layer`, which is called from the shell's own `render`, so reading
/// the shell from there panics with "cannot read while it is already being updated". Reading a
/// separate entity is fine.
struct ActionForm {
    name: Entity<InputState>,
    command: Entity<InputState>,
    background: Entity<ColorPickerState>,
    spec: ImageSpec,
    /// Whether the picked image has transparency, so the preview insets it the way the
    /// compositor will.
    image_is_icon: bool,
    /// Artwork already stored on the action being edited, shown until a new pick replaces it.
    existing_image: Option<String>,
    /// Id of the action being edited, or `None` when creating.
    editing: Option<String>,
}

impl ActionForm {
    /// The image the key will end up with if saved right now.
    fn preview(&self) -> Option<String> {
        self.spec
            .file
            .clone()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| self.existing_image.clone())
    }
}

pub struct RustyDeckShell {
    device: Option<DeviceInfo>,
    profile: Option<Profile>,
    devices: Vec<DeviceInfo>,
    /// Plugin actions grouped by category, sorted for a stable palette order.
    categories: Vec<(String, Category)>,
    /// The user's own action library.
    custom: Vec<CustomAction>,
    device_picker_open: bool,
    form: Entity<ActionForm>,
    /// Id of the custom action whose `...` menu is open.
    row_menu_open: Option<String>,
}

impl RustyDeckShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            refresh_catalogue(&this, cx).await;

            let mut events = frontend_events::subscribe();
            while let Ok(event) = events.recv().await {
                match event {
                    FrontendEvent::Devices(_) | FrontendEvent::PluginReloaded(_) => refresh_catalogue(&this, cx).await,
                    _ => {}
                }
            }
        })
        .detach();

        Self {
            device: None,
            profile: None,
            devices: Vec::new(),
            categories: Vec::new(),
            custom: custom_actions::load(),
            device_picker_open: false,
            form: cx.new(|cx| ActionForm {
                name: cx.new(|cx| InputState::new(window, cx).placeholder("Lock screen")),
                command: cx.new(|cx| InputState::new(window, cx).placeholder("loginctl lock-session")),
                background: cx.new(|cx| ColorPickerState::new(window, cx)),
                spec: ImageSpec::default(),
                image_is_icon: false,
                existing_image: None,
                editing: None,
            }),
            row_menu_open: None,
        }
    }

    /// Run a custom action's command directly, without going through the deck.
    ///
    /// Unlike the plugin's own runner - which spawns `sh` and never inspects the result - this
    /// reports a non-zero exit and its stderr, so a broken command says so instead of silently
    /// doing nothing.
    fn execute_command(&mut self, command: String, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        cx.notify();

        crate::spawn(async move {
            log::info!("Executing custom action command: {command}");

            // Run through the user's login shell rather than plain `sh -c`, so aliases and shell
            // functions resolve. Omarchy, for instance, provides `open` as a bash function - it
            // works in a terminal but is invisible to a non-interactive POSIX shell, which makes a
            // perfectly good command look like it silently does nothing.
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_owned());
            match tokio::process::Command::new(&shell).arg("-lic").arg(&command).output().await {
                Ok(output) if output.status.success() => log::info!("Command finished: {command}"),
                Ok(output) => log::error!(
                    "Command failed ({}): {} - {}",
                    output.status,
                    command,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                Err(error) => log::error!("Could not run command {command}: {error}"),
            }
        });
    }

    fn delete_custom_action(&mut self, id: String, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        if let Err(error) = custom_actions::delete(&id) {
            log::error!("Failed to delete custom action: {error}");
            return;
        }
        self.custom.retain(|action| action.slug != id);
        cx.notify();
    }

    /// Open the action form, either blank or prepopulated from an existing action.
    ///
    /// Field state lives on the shell so it survives the dialog's re-renders; the dialog builder
    /// only reads it.
    fn open_action_dialog(&mut self, edit: Option<CustomAction>, window: &mut Window, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        let existing = edit.clone();
        let form = self.form.clone();

        form.update(cx, |form, cx| {
            form.name
                .update(cx, |state, cx| state.set_value(existing.as_ref().map(|a| a.name().to_owned()).unwrap_or_default(), window, cx));
            form.command
                .update(cx, |state, cx| state.set_value(existing.as_ref().map(|a| a.command().to_owned()).unwrap_or_default(), window, cx));
            form.spec = ImageSpec::default();
            form.editing = existing.as_ref().map(|a| a.slug.clone());

            // Preview against the *source* image, not the composited picture - the latter already
            // has the previous background baked in, which would sit opaquely over a new colour.
            let source = existing.as_ref().and_then(|a| a.source_path());
            form.image_is_icon = source.as_deref().map(custom_actions::has_transparency).unwrap_or(false);
            form.existing_image = source
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| existing.as_ref().map(|a| a.image_path().to_string_lossy().into_owned()));

            // Start the picker on the colour the action already has, so opening the form and
            // saving without touching it is a no-op rather than a reset.
            if let Some(colour) = existing.as_ref().and_then(|a| a.config.background.as_deref()).and_then(hex_to_hsla) {
                form.background.update(cx, |state, cx| state.set_value(colour, window, cx));
            }
        });

        let this = cx.entity().downgrade();
        let title = if edit.is_some() { "Edit action" } else { "Create action" };

        window.open_dialog(cx, move |dialog, _window, cx| {
            let this = this.clone();
            let clear_form = form.clone();
            let pick_image = form.clone();
            let ok_form = form.clone();
            let name_state = form.read(cx).name.clone();
            let command_state = form.read(cx).command.clone();
            let background_state = form.read(cx).background.clone();
            let preview = form.read(cx).preview();
            // Show what the key will actually look like: the chosen colour behind the image, with
            // a transparent icon inset so the colour reads as a border - the same rule the
            // compositor applies when writing picture.png.
            let preview_background = background_state.read(cx).value();
            let preview_is_icon = form.read(cx).image_is_icon;

            dialog
                .title(title)
                .button_props(DialogButtonProps::default().ok_text("Save").cancel_text("Cancel"))
                // Footer buttons only render when a footer is set - `button_props` alone is
                // just labels, which is why Save was missing.
                .footer(|ok, cancel, window, cx| vec![cancel(window, cx), ok(window, cx)])
                .child(
                    v_flex()
                        .gap_3()
                        .p_2()
                        .child(field("Name", Input::new(&name_state)))
                        .child(field("Command", Input::new(&command_state)))
                        .child(field("Background", ColorPicker::new(&background_state)))
                        .child(field(
                            "Image",
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    // The tile always shows, so a colour-only action still has a
                                    // preview once the image is cleared.
                                    div()
                                        .size(px(48.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .overflow_hidden()
                                        .when_some(preview_background, |tile, colour| tile.bg(colour))
                                        .children(preview.clone().map(|path| {
                                            let inset = if preview_is_icon { px(10.0) } else { px(0.0) };
                                            div().size_full().p(inset).child(img(resolve_image_path(&path)).size_full())
                                        })),
                                )
                                .children(preview.map(|_| {
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(Button::new("clear-image").icon(IconName::Close).on_click(move |_event, _window, cx| {
                                            clear_form.update(cx, |form, cx| {
                                                form.spec = ImageSpec::default();
                                                form.image_is_icon = false;
                                                form.existing_image = None;
                                                cx.notify();
                                            });
                                        }))
                                }))
                                .child(Button::new("pick-image").label("Choose image…").on_click(move |_event, _window, cx| {
                                    pick_file(pick_image.clone(), cx);
                                })),
                        )),
                )
                .on_ok(move |_event, _window, cx| {
                    let name = name_state.read(cx).value().to_string();
                    let command = command_state.read(cx).value().to_string();
                    if name.trim().is_empty() || command.trim().is_empty() {
                        // Keep the dialog open until both are filled in.
                        return false;
                    }

                    let (mut spec, editing) = ok_form.read_with(cx, |form, _| (form.spec.clone(), form.editing.clone()));
                    spec.background = ok_form.read(cx).background.read(cx).value().map(hsla_to_hex);
                    let _ = this.update(cx, |this, cx| this.save_custom_action(name, command, spec, editing, cx));
                    true
                })
        });
    }

    fn save_custom_action(&mut self, name: String, command: String, spec: ImageSpec, editing: Option<String>, cx: &mut Context<Self>) {
        let result = match &editing {
            Some(id) => custom_actions::update(id, name, command, &spec).inspect(|updated| {
                // An edit can rename the directory, so match on the id we edited, not the new one.
                match self.custom.iter_mut().find(|action| action.slug == *id) {
                    Some(existing) => *existing = updated.clone(),
                    None => self.custom.push(updated.clone()),
                }
            }),
            None => custom_actions::create(name, command, &spec).inspect(|action| self.custom.push(action.clone())),
        };

        match result {
            // Artwork is rewritten to the same `picture.png` path, so GPUI would keep serving the
            // bitmap it decoded earlier - evict it, or the sidebar and any slot already using this
            // action keep showing the old picture.
            Ok(action) => {
                let source: gpui::ImageSource = action.image_path().into();
                source.remove_asset(cx);

                // Slots already carrying this action need re-pushing so the hardware follows too.
                cx.spawn(async move |this, cx| reload_profile(&this, cx).await).detach();
            }
            Err(error) => log::error!("Failed to save custom action: {error}"),
        }
        cx.notify();
    }

    /// Place a custom action on a slot: create the Run Command instance that backs it, then write
    /// its command and image onto that instance.
    fn handle_create_custom(&mut self, action: CustomAction, destination: SlotContext, cx: &mut Context<Self>) {
        let Some(run_command) = self
            .categories
            .iter()
            .flat_map(|(_, category)| category.actions.iter())
            .find(|candidate| candidate.uuid == RUN_COMMAND_UUID)
            .cloned()
        else {
            log::error!("Cannot place custom action: the {RUN_COMMAND_UUID} action is not registered");
            return;
        };

        cx.spawn(async move |this, cx| {
            let Ok(Some(instance)) = crate::bridge(frontend::instances::create_instance(run_command, destination)).await else {
                return;
            };
            let context = instance.context.clone();

            // `settings.down` is the key `RunCommandSettings` reads for the key-press command.
            let payload = serde_json::json!({ "down": action.command() });
            let event = crate::events::inbound::ContextAndPayloadEvent {
                context: context.clone(),
                payload,
            };
            // `true` = "this came from the settings UI", which is what makes the backend forward
            // the settings on to the *plugin*. Passing `false` sends them to the property
            // inspector instead, so the plugin never learns the command and does nothing on press.
            if let Err(error) = crate::bridge(crate::events::inbound::settings::set_settings(event, true)).await {
                log::error!("Failed to set command on custom action: {error}");
            }

            if let Some(mut state) = instance.states.first().cloned() {
                state.image = action.image_path().to_string_lossy().into_owned();
                if let Err(error) = crate::bridge(frontend::instances::set_state(context, 0, state)).await {
                    log::error!("Failed to set image on custom action: {error}");
                }
            }

            reload_profile(&this, cx).await;
        })
        .detach();
    }

    fn select_device(&mut self, device: DeviceInfo, cx: &mut Context<Self>) {
        self.device_picker_open = false;
        if self.device.as_ref().is_some_and(|current| current.id == device.id) {
            cx.notify();
            return;
        }

        self.device = Some(device.clone());
        self.profile = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let Ok(profile) = crate::bridge(frontend::profiles::get_selected_profile(device.id.clone())).await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.profile = Some(profile.clone());
                cx.notify();
            });
            push_device_images(device, profile).await;
        })
        .detach();
    }

    /// Create a new instance from a palette entry dropped onto a slot.
    fn handle_create(&mut self, action: Action, destination: SlotContext, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // `create_instance` returns `None` for an occupied slot or an unsupported controller,
            // so an invalid drop is already a safe no-op.
            let Ok(Some(_)) = crate::bridge(frontend::instances::create_instance(action, destination)).await else {
                return;
            };
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    fn handle_drop(&mut self, source: SlotContext, destination: SlotContext, cx: &mut Context<Self>) {
        if source == destination {
            return;
        }

        // Dropping onto an occupied slot swaps the two; `move_instance` refuses to overwrite an
        // occupied destination, so that case needs `swap_instances` instead.
        let occupied = self
            .profile
            .as_ref()
            .and_then(|profile| slots(profile, &destination.controller).get(destination.position as usize))
            .is_some_and(|slot| slot.is_some());

        cx.spawn(async move |this, cx| {
            let changed = if occupied {
                crate::bridge(frontend::instances::swap_instances(source, destination)).await.unwrap_or(false)
            } else {
                matches!(crate::bridge(frontend::instances::move_instance(source, destination, false)).await, Ok(Some(_)))
            };
            if !changed {
                return;
            }
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    /// Render one slot, unsized - the caller decides its dimensions, since keys, touch-strip
    /// segments and infobars are all shaped differently.
    fn render_slot(&self, controller: &'static str, position: u8, cx: &Context<Self>) -> Stateful<Div> {
        let device = self.device.as_ref().expect("render_slot called without a selected device");
        let profile = self.profile.as_ref().expect("render_slot called without a loaded profile");
        let slot = slots(profile, controller).get(position as usize).cloned().flatten();

        let context = SlotContext {
            device: device.id.clone(),
            profile: profile.id.clone(),
            controller: controller.to_owned(),
            position,
        };

        let mut cell = div()
            .id((controller, position as usize))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x313244))
            .border_1()
            .border_color(rgb(0x45475a));

        if let Some(instance) = &slot {
            let state = instance.states.get(instance.current_state as usize);
            let image_path = state.map(|state| state.image.clone());

            if let Some(path) = image_path.clone() {
                // Match what the hardware shows. `device_render` stretches the artwork to the
                // slot's aspect, so a square icon on a 2:1 strip segment fills it; GPUI's default
                // `Contain` would letterbox it into a centred square with blank sides instead.
                cell = cell.child(img(resolve_image_path(&path)).size_full().object_fit(gpui::ObjectFit::Fill));
            }

            // Clicking a filled slot runs it, exactly as pressing the physical key would -
            // `trigger_virtual_press` drives the same key-down/up path the hardware does.
            // A click and a drag do not conflict: GPUI only starts a drag past its 2px threshold.
            let press_context = context.clone();
            cell = cell.on_click(move |_event, _window, cx| {
                let context = press_context.clone();
                cx.background_spawn(async move {
                    if let Err(error) = crate::bridge(frontend::instances::trigger_virtual_press(context)).await {
                        log::error!("Failed to trigger action: {error}");
                    }
                })
                .detach();
            });

            let drag_context = context.clone();
            let drag_image = image_path;
            cell = cell.on_drag(
                DraggedKey {
                    context: drag_context,
                    image: drag_image,
                },
                |dragged, _cursor_offset, _window, cx| {
                    let image = dragged.image.clone();
                    cx.new(|_| DragPreview { image })
                },
            );
        }

        // Two drop payloads land on the same cell: an existing instance being moved/swapped, and a
        // palette entry creating a new one. GPUI keys drop listeners by payload type, so both
        // coexist and only the matching one fires.
        let move_context = context.clone();
        let create_context = context.clone();
        let custom_context = context;
        cell.on_drop(cx.listener(move |this, dragged: &DraggedKey, _window, cx| {
            this.handle_drop(dragged.context.clone(), move_context.clone(), cx);
        }))
        .on_drop(cx.listener(move |this, dragged: &DraggedAction, _window, cx| {
            this.handle_create(dragged.action.clone(), create_context.clone(), cx);
        }))
        .on_drop(cx.listener(move |this, dragged: &DraggedCustomAction, _window, cx| {
            this.handle_create_custom(dragged.action.clone(), custom_context.clone(), cx);
        }))
    }
}

/// Composite and push every slot's image to the physical device.
///
/// The backend never renders images itself (see `device_render`), so nothing reaches the hardware
/// unless the shell does this - on load and after every mutation, mirroring how the old frontend
/// re-rendered each slot's canvas whenever it re-rendered the grid.
async fn push_device_images(device: DeviceInfo, profile: Profile) {
    // Touchpoints live in the keypad array, appended after the keys.
    let keypad_count = (device.rows as usize) * (device.columns as usize) + device.touchpoints as usize;

    let groups = [
        (KEYPAD_CONTROLLER, keypad_count, KEY_IMAGE),
        (ENCODER_CONTROLLER, device.encoders as usize, ENCODER_IMAGE),
        (INFOBAR_CONTROLLER, device.infobars as usize, INFOBAR_IMAGE),
    ];

    for (controller, count, (width, height)) in groups {
        let slots = slots(&profile, controller);

        for position in 0..count.min(slots.len()) {
            let context = SlotContext {
                device: device.id.clone(),
                profile: profile.id.clone(),
                controller: controller.to_owned(),
                position: position as u8,
            };

            let image = match slots[position].as_ref() {
                Some(instance) => match instance.states.get(instance.current_state as usize) {
                    Some(state) => match render_state(state, width, height) {
                        Ok(image) => Some(image),
                        Err(error) => {
                            log::warn!("Failed to render image for {controller} slot {position}: {error}");
                            continue;
                        }
                    },
                    None => None,
                },
                // `None` clears the slot on the device.
                None => None,
            };

            crate::bridge(frontend::instances::update_image(context, image)).await;
        }
    }
}

/// Re-read the selected profile from the backend and push it to the hardware.
///
/// Cheaper to re-read than to replay the backend's relocation rules up here - contexts, child
/// indices and image paths all shift on a move, and one source of truth is enough.
async fn reload_profile(this: &WeakEntity<RustyDeckShell>, cx: &mut gpui::AsyncApp) {
    let Some(device) = this.update(cx, |this, _| this.device.clone()).ok().flatten() else {
        return;
    };
    let Ok(profile) = crate::bridge(frontend::profiles::get_selected_profile(device.id.clone())).await else {
        return;
    };

    let _ = this.update(cx, |this, cx| {
        this.profile = Some(profile.clone());
        cx.notify();
    });

    push_device_images(device, profile).await;
}

/// Refresh the device list and action palette, auto-selecting a device if none is chosen yet.
async fn refresh_catalogue(this: &WeakEntity<RustyDeckShell>, cx: &mut gpui::AsyncApp) {
    let devices: Vec<DeviceInfo> = crate::bridge(frontend::get_devices()).await.iter().map(|entry| entry.value().clone()).collect();

    let mut categories: Vec<(String, Category)> = crate::bridge(frontend::get_categories()).await.into_iter().collect();
    categories.sort_by(|(a, _), (b, _)| a.cmp(b));

    let newly_selected = this
        .update(cx, |this, cx| {
            this.devices = devices;
            this.categories = categories;

            // Only auto-select when nothing is chosen; the list itself always refreshes so a
            // device arriving later shows up in the picker.
            let selected = match &this.device {
                Some(_) => None,
                None => this.devices.first().cloned(),
            };
            if let Some(device) = &selected {
                this.device = Some(device.clone());
            }
            cx.notify();
            selected
        })
        .ok()
        .flatten();

    if newly_selected.is_some() {
        reload_profile(this, cx).await;
    }
}

/// The inverse of [`hsla_to_hex`], for seeding the picker from a stored colour.
fn hex_to_hsla(hex: &str) -> Option<gpui::Hsla> {
    let colour = crate::device_render::parse_colour(hex);
    Some(gpui::Hsla::from(gpui::Rgba {
        r: colour[0] as f32 / 255.0,
        g: colour[1] as f32 / 255.0,
        b: colour[2] as f32 / 255.0,
        a: 1.0,
    }))
}

/// gpui works in HSLA; the stored config and the compositor both speak `#RRGGBB`.
fn hsla_to_hex(colour: gpui::Hsla) -> String {
    let rgba = gpui::Rgba::from(colour);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", channel(rgba.r), channel(rgba.g), channel(rgba.b))
}

/// A labelled form field in the create dialog.
fn field(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(div().text_sm().child(label)).child(control)
}

/// Ask for a file without blocking the UI thread - the blocking `rfd::FileDialog` used elsewhere
/// in the backend would freeze rendering if called from here.
fn pick_file(form: Entity<ActionForm>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let Some(handle) = rfd::AsyncFileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg", "svg"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = handle.path().to_path_buf();

        let is_icon = custom_actions::has_transparency(&path);
        let _ = form.update(cx, |form, cx| {
            form.spec.file = Some(path);
            form.image_is_icon = is_icon;
            form.existing_image = None;
            cx.notify();
        });
    })
    .detach();
}

/// One row in the palette. A single type covers all three kinds so both sidebar sections share it -
/// `Sidebar` is generic over one child type.
///
/// Implemented as its own component rather than `SidebarMenuItem` so it can carry `on_drag`.
#[derive(IntoElement)]
enum PaletteRow {
    /// Opens the create-action dialog.
    Create { collapsed: bool, on_click: Rc<dyn Fn(&mut Window, &mut App)> },
    Custom {
        action: CustomAction,
        collapsed: bool,
        menu_open: bool,
        on_menu: Rc<dyn Fn(&mut Window, &mut App)>,
        on_execute: Rc<dyn Fn(&mut Window, &mut App)>,
        on_edit: Rc<dyn Fn(&mut Window, &mut App)>,
        on_delete: Rc<dyn Fn(&mut Window, &mut App)>,
    },
    Plugin { action: Action, collapsed: bool },
}

/// One entry in a custom action's `...` menu.
fn menu_entry(id: &'static str, label: &'static str, on_click: Rc<dyn Fn(&mut Window, &mut App)>, cx: &App) -> impl IntoElement {
    div()
        .id(id)
        .w_full()
        .px_2()
        .py_1()
        .rounded_md()
        .text_sm()
        .text_color(cx.theme().foreground)
        .hover(|style| style.bg(cx.theme().accent))
        .child(label)
        .on_click(move |_event, window, cx| on_click(window, cx))
}

impl Collapsible for PaletteRow {
    fn collapsed(mut self, value: bool) -> Self {
        match &mut self {
            PaletteRow::Create { collapsed, .. } | PaletteRow::Custom { collapsed, .. } | PaletteRow::Plugin { collapsed, .. } => *collapsed = value,
        }
        self
    }

    fn is_collapsed(&self) -> bool {
        match self {
            PaletteRow::Create { collapsed, .. } | PaletteRow::Custom { collapsed, .. } | PaletteRow::Plugin { collapsed, .. } => *collapsed,
        }
    }
}

/// Shared chrome for every palette row, so the three kinds line up with each other.
fn palette_row(id: impl Into<gpui::ElementId>, cx: &App) -> Stateful<Div> {
    h_flex()
        .id(id.into())
        .w_full()
        .h(px(32.0))
        .px_2()
        .gap_2()
        .rounded_md()
        .items_center()
        .text_color(cx.theme().foreground)
        .hover(|style| style.bg(cx.theme().accent))
}

impl RenderOnce for PaletteRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self {
            PaletteRow::Create { collapsed, on_click } => palette_row("create-action", cx)
                .child(IconName::Plus.view(cx))
                .when(!collapsed, |row| row.child(div().text_sm().child("Create action")))
                .on_click(move |_event, window, cx| on_click(window, cx))
                .into_any_element(),

            PaletteRow::Custom {
                action,
                collapsed,
                menu_open,
                on_menu,
                on_execute,
                on_edit,
                on_delete,
            } => {
                let id = action.slug.clone();
                palette_row(SharedString::from(action.slug.clone()), cx)
                    .relative()
                    .child(img(action.image_path()).size(px(20.0)).flex_none())
                    .when(!collapsed, |row| {
                        row.child(div().flex_1().text_sm().child(SharedString::from(action.name().to_owned())))
                            .child(
                                div()
                                    .id(SharedString::from(format!("{id}-menu")))
                                    .px_1()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|style| style.text_color(cx.theme().foreground))
                                    .child("…")
                                    .on_click(move |_event, window, cx| on_menu(window, cx)),
                            )
                    })
                    .when(menu_open, |row| {
                        row.child(
                            v_flex()
                                .absolute()
                                .top(px(28.0))
                                .right_0()
                                .w(px(120.0))
                                .p_1()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .child(menu_entry("execute", "Execute", on_execute, cx))
                                .child(menu_entry("edit", "Edit", on_edit, cx))
                                .child(menu_entry("delete", "Delete", on_delete, cx)),
                        )
                    })
                    .on_drag(DraggedCustomAction { action }, |dragged, _offset, _window, cx| {
                        let image = Some(dragged.action.image_path().to_string_lossy().into_owned());
                        cx.new(|_| DragPreview { image })
                    })
                    .into_any_element()
            }

            PaletteRow::Plugin { action, collapsed } => palette_row(SharedString::from(action.uuid.clone()), cx)
                .child(img(resolve_image_path(&action.icon)).size(px(20.0)).flex_none())
                .when(!collapsed, |row| row.child(div().text_sm().child(SharedString::from(action.name.clone()))))
                .on_drag(DraggedAction { action }, |dragged, _offset, _window, cx| {
                    let image = Some(dragged.action.icon.clone());
                    cx.new(|_| DragPreview { image })
                })
                .into_any_element(),
        }
    }
}

struct DragPreview {
    image: Option<String>,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut preview = div().size(px(CELL_SIZE)).bg(rgb(0x45475a)).rounded_md();
        if let Some(path) = self.image.clone() {
            preview = preview.child(img(resolve_image_path(&path)).size_full());
        }
        preview
    }
}

impl RustyDeckShell {
    /// Device identity, with a swap control for choosing another device.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match &self.device {
            Some(device) => SharedString::from(device.name.clone()),
            None => SharedString::from("No device"),
        };

        let picker = self.device_picker_open.then(|| {
            v_flex()
                .absolute()
                .top(px(44.0))
                .right_0()
                .w(px(240.0))
                .p_1()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .children(self.devices.iter().cloned().map(|device| {
                    let label = SharedString::from(device.name.clone());
                    let selected = self.device.as_ref().is_some_and(|current| current.id == device.id);
                    h_flex()
                        .id(SharedString::from(device.id.clone()))
                        .w_full()
                        .p_2()
                        .rounded_md()
                        .text_sm()
                        .when(selected, |row| row.bg(cx.theme().accent))
                        .hover(|style| style.bg(cx.theme().accent))
                        .child(label)
                        .on_click(cx.listener(move |this, _event, _window, cx| this.select_device(device.clone(), cx)))
                }))
        });

        h_flex()
            .w_full()
            .h(px(56.0))
            .px_4()
            .flex_none()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(div().text_xl().font_semibold().text_color(cx.theme().foreground).child(title))
            .child(
                div().relative().child(
                    Button::new("swap-device")
                        .icon(IconName::Replace)
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.device_picker_open = !this.device_picker_open;
                            cx.notify();
                        })),
                ),
            )
            .children(picker)
    }

    /// The action palette: every action the installed plugins expose, dragged onto a slot to
    /// create an instance. Collapses to icons when the window is narrow.
    fn render_sidebar(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let create = entity.clone();
        let open_dialog: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |window, cx| {
            let _ = create.update(cx, |this, cx| this.open_action_dialog(None, window, cx));
        });

        let mut custom_rows = vec![PaletteRow::Create {
            collapsed,
            on_click: open_dialog,
        }];
        custom_rows.extend(self.custom.iter().cloned().map(|action| {
            let id = action.slug.clone();
            let menu_owner = entity.clone();
            let execute_owner = entity.clone();
            let execute_command = action.command().to_owned();
            let edit_owner = entity.clone();
            let delete_owner = entity.clone();
            let (menu_id, edit_action, delete_id) = (id.clone(), action.clone(), id.clone());

            PaletteRow::Custom {
                menu_open: self.row_menu_open.as_deref() == Some(id.as_str()),
                collapsed,
                on_menu: Rc::new(move |_window, cx| {
                    let id = menu_id.clone();
                    let _ = menu_owner.update(cx, |this, cx| {
                        // Clicking the open row's `...` again closes it.
                        this.row_menu_open = if this.row_menu_open.as_deref() == Some(id.as_str()) { None } else { Some(id) };
                        cx.notify();
                    });
                }),
                on_execute: Rc::new(move |_window, cx| {
                    let command = execute_command.clone();
                    let _ = execute_owner.update(cx, |this, cx| this.execute_command(command, cx));
                }),
                on_edit: Rc::new(move |window, cx| {
                    let action = edit_action.clone();
                    let _ = edit_owner.update(cx, |this, cx| this.open_action_dialog(Some(action), window, cx));
                }),
                on_delete: Rc::new(move |_window, cx| {
                    let id = delete_id.clone();
                    let _ = delete_owner.update(cx, |this, cx| this.delete_custom_action(id, cx));
                }),
                action,
            }
        }));

        // Every plugin action, flattened - the sections are what carry meaning here, not the
        // per-plugin categories.
        let predefined_rows: Vec<PaletteRow> = self
            .categories
            .iter()
            .flat_map(|(_, category)| category.actions.iter())
            .cloned()
            .map(|action| PaletteRow::Plugin { action, collapsed })
            .collect();

        Sidebar::left().collapsed(collapsed).children(vec![
            SidebarGroup::new("Custom actions").children(custom_rows),
            SidebarGroup::new("Predefined actions").children(predefined_rows),
        ])
    }

    /// The device's slots - keypad, touch strip, touchpoints and infobar.
    fn render_device(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let base = v_flex().size_full().items_center().justify_center().gap_2().text_color(cx.theme().foreground);

        let Some(device) = self.device.clone() else {
            return base.child("No Stream Deck connected").into_any_element();
        };

        if self.profile.is_none() {
            return base.child(format!("Loading profile for {}...", device.name)).into_any_element();
        }

        let rows = device.rows;
        let columns = device.columns;

        let keypad = div().flex().flex_col().gap(px(GAP)).children((0..rows).map(|row| {
            div().flex().flex_row().gap(px(GAP)).children(
                (0..columns).map(|col| self.render_slot(KEYPAD_CONTROLLER, row * columns + col, cx).w(px(CELL_SIZE)).h(px(CELL_SIZE)).rounded_md()),
            )
        }));

        let keypad_width = columns as f32 * CELL_SIZE + (columns.saturating_sub(1)) as f32 * GAP;
        let mut shell = base.child(keypad);

        // The touch strip: one wide LCD divided into a region per dial, drawn as 2:1 segments
        // spanning the keypad's width - matching the proportions of the physical strip.
        if device.encoders > 0 {
            let encoders = device.encoders;
            let segment_width = (keypad_width - (encoders.saturating_sub(1)) as f32 * GAP) / encoders as f32;

            shell = shell.child(div().flex().flex_row().gap(px(GAP)).children((0..encoders).map(|position| {
                self.render_slot(ENCODER_CONTROLLER, position, cx).w(px(segment_width)).h(px(segment_width / 2.0)).rounded_md()
            })));
        }

        if device.touchpoints > 0 {
            let first_touchpoint = rows * columns;
            shell = shell.child(div().flex().flex_row().gap(px(GAP)).children((0..device.touchpoints).map(|index| {
                self.render_slot(KEYPAD_CONTROLLER, first_touchpoint + index, cx).w(px(CELL_SIZE / 2.0)).h(px(CELL_SIZE / 2.0)).rounded_md()
            })));
        }

        if device.infobars > 0 {
            let infobar_height = keypad_width * INFOBAR_IMAGE.1 as f32 / INFOBAR_IMAGE.0 as f32;
            shell = shell.child(div().flex().flex_row().gap(px(GAP)).children((0..device.infobars).map(|position| {
                self.render_slot(INFOBAR_CONTROLLER, position, cx).w(px(keypad_width)).h(px(infobar_height)).rounded_md()
            })));
        }

        shell.into_any_element()
    }
}

impl Render for RustyDeckShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = window.viewport_size().width < px(SIDEBAR_BREAKPOINT);

        v_flex()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(h_flex().flex_1().min_h_0().child(self.render_sidebar(collapsed, cx)).child(self.render_device(cx)))
            // `Root::render` only draws the app's own view - dialogs and notifications are
            // separate layers the hosting view is expected to place on top of itself.
            .children(gpui_component::Root::render_dialog_layer(window, cx))
            .children(gpui_component::Root::render_notification_layer(window, cx))
    }
}
