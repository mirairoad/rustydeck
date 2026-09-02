//! The GPUI-native main shell: a header carrying device identity, a collapsible action palette,
//! and the device's slots with drag-to-reposition and drag-to-swap (the defect this rewrite exists
//! to fix - see PRD §3).

use crate::animation::{Animation, Effect, Speed, Trigger};
use crate::custom_actions::{self, CustomAction, ImageSpec};
use crate::device_render::{INFOBAR_IMAGE, resolve_image_path};
use crate::events::frontend;
use crate::frontend_events::{self, FrontendEvent};
use crate::shared::{Action, ActionInstance, Category, Context as SlotContext, DeviceInfo, Profile};
use crate::shared::{BUILTIN_PLUGIN, RUN_COMMAND_UUID};
use crate::store::profiles::DialConfig;

use std::rc::Rc;

use gpui::{
    App, Context, Div, Entity, IntoElement, ParentElement, Render, RenderOnce, SharedString, Stateful, Styled, WeakEntity, Window, div, img,
    deferred, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Collapsible, Disableable, IconName, StyledExt,
    button::Button,
    color_picker::{ColorPicker, ColorPickerState},
    dialog::DialogButtonProps,
    WindowExt, h_flex,
    input::{Input, InputState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    sidebar::{Sidebar, SidebarGroup},
    v_flex,
};


const CELL_SIZE: f32 = 72.0;
const GAP: f32 = 8.0;

/// Below this window width the palette collapses to an icon-only rail. Derived from the viewport
/// each frame rather than stored, so resizing is live.
const SIDEBAR_BREAKPOINT: f32 = 900.0;

/// Width of a custom action's `...` menu, which opens to the right of the sidebar.
const ROW_MENU_WIDTH: f32 = 120.0;

/// Diameter of the dial knobs drawn beneath the touch strip.
const DIAL_SIZE: f32 = 44.0;

/// The predefined group holding first-party dial actions. Hidden from the palette - see
/// `render_sidebar`.
const SYSTEM_CATEGORY: &str = "System";

/// Tint for simulated devices and their controls, so a fake deck never reads as a real one.
const SIMULATED_TINT: u32 = 0x7C3AED;

/// Height of an artwork preview tile. The key face is square at this size and the strip twice as
/// wide, matching the 2:1 region the hardware writes.
const PREVIEW_HEIGHT: f32 = 48.0;

/// A theme colour forced fully opaque.
///
/// The theme's `background` carries alpha, which is right for the window itself but wrong for a
/// surface that floats over the app's own content: the picker and the right-click menus would let
/// whatever sits behind them bleed through their panel and muddy the text on it.
fn opaque(colour: gpui::Hsla) -> gpui::Hsla {
    gpui::Hsla { a: 1.0, ..colour }
}

/// Simulated dial rotation. Compiled out of a release build, which has no simulated device.
fn simulate_rotate(device: &str, dial: u8, ticks: i16) {
    #[cfg(debug_assertions)]
    crate::simulator::rotate(device, dial, ticks);
    #[cfg(not(debug_assertions))]
    let _ = (device, dial, ticks);
}

/// Simulated dial press. Compiled out of a release build.
fn simulate_press(device: &str, dial: u8) {
    #[cfg(debug_assertions)]
    crate::simulator::press_dial(device, dial);
    #[cfg(not(debug_assertions))]
    let _ = (device, dial);
}

/// Whether a device is simulated. Always false in a release build, which has none.
fn is_simulated(device_id: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        crate::simulator::is_simulated(device_id)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = device_id;
        false
    }
}

const KEYPAD_CONTROLLER: &str = "Keypad";
const ENCODER_CONTROLLER: &str = "Encoder";
const INFOBAR_CONTROLLER: &str = "Infobar";

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

/// A configured dial being dragged onto another dial to exchange the two.
///
/// Carries the position rather than the config: dials are device-scoped, so the store is the one
/// authority on what each knob does and the swap is resolved there.
#[derive(Clone)]
struct DraggedDial {
    dial: u8,
    label: SharedString,
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

/// What a library entry does when it lands on a slot.
#[derive(Clone, PartialEq)]
enum ActionKind {
    /// The user's own shell command, run through Run Command.
    Command,
    /// A built-in action placed as it is, named by its UUID - the entry supplies only the artwork.
    Builtin(SharedString),
}

/// One entry in the create-action dialog's kind picker.
#[derive(Clone)]
struct ActionChoice {
    label: SharedString,
    kind: ActionKind,
}

impl SelectItem for ActionChoice {
    type Value = ActionKind;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.kind
    }
}

/// The picker's entries: a shell command, then every built-in action that can sit on a key.
///
/// Read off the registered catalogue rather than a hand-written list, so a new first-party keypad
/// action appears here without anything being added. Three exclusions, each for its own reason:
/// Run Command *is* the "Run command" entry; anything from another plugin namespace is an upstream
/// leftover with no handler behind it; and an encoder-only action would be unplaceable, since a
/// dial is configured from its own dialog rather than by dropping a library entry on it.
fn action_choices(categories: &[(String, Category)]) -> Vec<ActionChoice> {
    let mut choices = vec![ActionChoice {
        label: "Run command".into(),
        kind: ActionKind::Command,
    }];

    let mut builtins: Vec<ActionChoice> = categories
        .iter()
        .flat_map(|(_, category)| category.actions.iter())
        .filter(|action| action.plugin == BUILTIN_PLUGIN && action.uuid != RUN_COMMAND_UUID)
        .filter(|action| action.controllers.iter().any(|controller| controller == KEYPAD_CONTROLLER))
        .map(|action| ActionChoice {
            label: SharedString::from(action.name.clone()),
            kind: ActionKind::Builtin(SharedString::from(action.uuid.clone())),
        })
        .collect();

    // The catalogue is a map, so its order is not stable between runs.
    builtins.sort_by(|a, b| a.label.cmp(&b.label));
    choices.extend(builtins);
    choices
}

/// What the animation picker offers: no animation, or one of the generated effects.
#[derive(Clone, PartialEq)]
enum AnimationKind {
    None,
    Generated(Effect),
}

#[derive(Clone)]
struct AnimationChoice {
    label: SharedString,
    kind: AnimationKind,
}

impl SelectItem for AnimationChoice {
    type Value = AnimationKind;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.kind
    }
}

/// "None" first, then every generated effect.
///
/// Generated rather than imported: an effect is computed from the artwork the action already has,
/// so it needs no second file and works on a library that was built before animation existed.
fn animation_choices() -> Vec<AnimationChoice> {
    let mut choices = vec![AnimationChoice {
        label: "None".into(),
        kind: AnimationKind::None,
    }];
    choices.extend(Effect::ALL.iter().map(|effect| AnimationChoice {
        label: SharedString::from(effect.label()),
        kind: AnimationKind::Generated(*effect),
    }));
    choices
}

#[derive(Clone)]
struct TriggerChoice {
    label: SharedString,
    trigger: Trigger,
}

impl SelectItem for TriggerChoice {
    type Value = Trigger;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.trigger
    }
}

fn trigger_choices() -> Vec<TriggerChoice> {
    Trigger::ALL
        .iter()
        .map(|trigger| TriggerChoice {
            label: SharedString::from(trigger.label()),
            trigger: *trigger,
        })
        .collect()
}

#[derive(Clone)]
struct SpeedChoice {
    label: SharedString,
    speed: Speed,
}

impl SelectItem for SpeedChoice {
    type Value = Speed;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.speed
    }
}

fn speed_choices() -> Vec<SpeedChoice> {
    Speed::ALL
        .iter()
        .map(|speed| SpeedChoice {
            label: SharedString::from(format!("{} - {} fps", speed.label(), speed.fps())),
            speed: *speed,
        })
        .collect()
}

/// Everything the form collected, on its way to being written.
///
/// A struct rather than a parameter list: the same six values travel together from the dialog's
/// Save through to the worker, and named fields at the call site beat six positional arguments
/// where three of them are `Option<String>`.
struct ActionDraft {
    name: String,
    command: String,
    /// The built-in action to place instead of Run Command, if one was chosen.
    builtin: Option<String>,
    animation: Option<Animation>,
    spec: ImageSpec,
    /// The slug being edited, or `None` when creating.
    editing: Option<String>,
}

/// Transient state of the create/edit form.
///
/// Deliberately its own entity rather than fields on the shell: the dialog's builder closure runs
/// inside `Root::render_dialog_layer`, which is called from the shell's own `render`, so reading
/// the shell from there panics with "cannot read while it is already being updated". Reading a
/// separate entity is fine.
struct ActionForm {
    name: Entity<InputState>,
    /// Whether this entry runs a command or places a built-in action.
    kind: Entity<SelectState<Vec<ActionChoice>>>,
    /// Which effect the artwork plays, if any.
    animation: Entity<SelectState<Vec<AnimationChoice>>>,
    /// How fast it plays. Only shown once an effect is chosen.
    speed: Entity<SelectState<Vec<SpeedChoice>>>,
    /// Whether it loops or runs once on a press.
    trigger: Entity<SelectState<Vec<TriggerChoice>>>,
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
    /// Why the last pick or save was refused, shown in the dialog.
    error: Option<SharedString>,
    /// Whether a save has been attempted, so required fields left blank can be marked.
    invalid: bool,
    /// Whether a picked image is still being read, so the form can say so.
    probing: bool,
    /// Whether artwork is being composited right now, so the form can say so and refuse a second
    /// save on top of the first.
    saving: bool,
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

/// What a dial is set to.
#[derive(Clone, PartialEq)]
enum DialKind {
    /// Nothing - the dial does not act.
    None,
    /// A first-party action the app implements itself, named by its UUID.
    System(SharedString),
    /// Shell commands typed into the dial dialog, run through Run Command.
    Custom,
}

/// One entry in the dial dialog's action picker.
#[derive(Clone)]
struct DialChoice {
    label: SharedString,
    kind: DialKind,
}

impl SelectItem for DialChoice {
    type Value = DialKind;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.kind
    }
}

/// The picker's entries: nothing, then every system action that is actually implemented, then the
/// custom-commands entry.
fn dial_choices() -> Vec<DialChoice> {
    let mut choices = vec![DialChoice {
        label: "None".into(),
        kind: DialKind::None,
    }];

    choices.extend(crate::system_actions::CATALOGUE.iter().map(|(label, uuid)| DialChoice {
        label: SharedString::from(*label),
        kind: DialKind::System(SharedString::from(*uuid)),
    }));

    choices.push(DialChoice {
        label: "Custom".into(),
        kind: DialKind::Custom,
    });
    choices
}

/// The shell commands a custom dial runs, one per gesture the knob itself produces.
///
/// The tap is deliberately absent: it belongs to the rectangle above the dial, which is page-scoped
/// and configured by dropping an action onto it.
struct DialCommands {
    /// What to caption the knob with. Blank falls back to "Custom".
    name: String,
    /// Turning the dial anticlockwise.
    left: String,
    /// Turning it clockwise.
    right: String,
    /// Pressing it in.
    centre: String,
}

/// Transient state of the dial dialog, kept in its own entity for the same reason as
/// [`ActionForm`]: the dialog's builder runs inside the shell's own render, so it cannot read the
/// shell back.
struct DialForm {
    /// Which dial the open dialog is configuring.
    dial: u8,
    kind: Entity<SelectState<Vec<DialChoice>>>,
    name: Entity<InputState>,
    left: Entity<InputState>,
    right: Entity<InputState>,
    centre: Entity<InputState>,
}

pub struct RustyDeckShell {
    device: Option<DeviceInfo>,
    profile: Option<Profile>,
    devices: Vec<DeviceInfo>,
    /// Plugin actions grouped by category, sorted for a stable palette order.
    categories: Vec<(String, Category)>,
    /// The user's own action library.
    custom: Vec<CustomAction>,
    /// Shipped entries, living on disk in the same shape so they can be re-themed or replaced.
    predefined: Vec<CustomAction>,
    device_picker_open: bool,
    /// Whether [`Self::device`] was chosen automatically rather than by the user.
    ///
    /// Simulated devices register instantly while real hardware takes a moment to enumerate, so the
    /// first auto-selection in a debug build lands on a simulated deck. This is what lets that
    /// choice be upgraded when the real one arrives, without ever overriding a deliberate pick.
    device_auto_selected: bool,
    /// Pages on the current device, in display order, and which one is showing.
    pages: Vec<String>,
    current_page: String,
    form: Entity<ActionForm>,
    dial_form: Entity<DialForm>,
    /// What each dial on the current device does. Device-scoped, so unlike `profile` it survives a
    /// page change.
    dials: Vec<Option<DialConfig>>,
    /// Id of the custom action whose `...` menu is open.
    row_menu_open: Option<String>,
    /// Slot whose right-click menu is open.
    slot_menu_open: Option<SlotContext>,
    /// Index of the dial whose right-click menu is open.
    dial_menu_open: Option<u8>,
}

impl RustyDeckShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            // Subscribe before the first read, not after: registration is under way while this
            // runs, and an event emitted into the gap is lost for good. That window is wide enough
            // to matter - it is how a deck that enumerated during startup could stay invisible
            // until something else happened to refresh.
            let mut events = frontend_events::subscribe();
            refresh_catalogue(&this, cx).await;

            while let Ok(event) = events.recv().await {
                match event {
                    FrontendEvent::Devices => refresh_catalogue(&this, cx).await,
                    // A page changed on the deck - follow it in the window.
                    // The page swap repainted the deck on its way through `pages::show`, so this
                    // only has to bring the window into line - pushing again would stop and
                    // restart the animation players that push just started.
                    FrontendEvent::SwitchProfile => reload_page(&this, cx, Repaint::IfStale).await,
                }
            }
        })
        .detach();

        let dial_form = cx.new(|cx| DialForm {
            dial: 0,
            kind: cx.new(|cx| SelectState::new(dial_choices(), None, window, cx)),
            // Blank is a valid name, so the placeholder is the fallback caption itself.
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Custom")),
            left: cx.new(|cx| InputState::new(window, cx).placeholder("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-")),
            right: cx.new(|cx| InputState::new(window, cx).placeholder("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+")),
            centre: cx.new(|cx| InputState::new(window, cx).placeholder("wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle")),
        });

        // The dialog's fields are built during the shell's own render, so picking "Custom" only
        // reveals the command inputs if the shell is told to draw again.
        let dial_kind = dial_form.read(cx).kind.clone();
        cx.subscribe(&dial_kind, |_this, _state, _event: &SelectEvent<Vec<DialChoice>>, cx| cx.notify())
            .detach();

        let form = cx.new(|cx| ActionForm {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Lock screen")),
            // The catalogue has not loaded yet, so this starts with only "Run command" and is
            // refilled from it each time the dialog opens.
            kind: cx.new(|cx| SelectState::new(action_choices(&[]), None, window, cx)),
            animation: cx.new(|cx| SelectState::new(animation_choices(), None, window, cx)),
            speed: cx.new(|cx| SelectState::new(speed_choices(), None, window, cx)),
            trigger: cx.new(|cx| SelectState::new(trigger_choices(), None, window, cx)),
            command: cx.new(|cx| InputState::new(window, cx).placeholder("loginctl lock-session")),
            background: cx.new(|cx| ColorPickerState::new(window, cx)),
            spec: ImageSpec::default(),
            image_is_icon: false,
            existing_image: None,
            editing: None,
            error: None,
            invalid: false,
            probing: false,
            saving: false,
        });

        // Same reason as the dial picker: choosing a built-in action hides the command field, and
        // that only shows if the shell redraws.
        let action_kind = form.read(cx).kind.clone();
        cx.subscribe(&action_kind, |_this, _state, _event: &SelectEvent<Vec<ActionChoice>>, cx| cx.notify())
            .detach();

        // Choosing an effect reveals the speed field, and the previews start playing - neither
        // happens unless the shell is told to draw again.
        let animation_kind = form.read(cx).animation.clone();
        cx.subscribe(&animation_kind, |_this, _state, _event: &SelectEvent<Vec<AnimationChoice>>, cx| cx.notify())
            .detach();

        Self {
            device: None,
            profile: None,
            devices: Vec::new(),
            categories: Vec::new(),
            custom: custom_actions::load(),
            predefined: custom_actions::load_predefined(),
            device_picker_open: false,
            device_auto_selected: false,
            pages: Vec::new(),
            current_page: String::new(),
            form,
            dial_form,
            dials: Vec::new(),
            row_menu_open: None,
            slot_menu_open: None,
            dial_menu_open: None,
        }
    }

    /// Remove the action occupying a slot, clearing it on screen and on the hardware.
    fn delete_slot(&mut self, context: SlotContext, cx: &mut Context<Self>) {
        self.slot_menu_open = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let context = crate::shared::ActionContext::from_context(context, 0);
            if let Err(error) = crate::bridge(frontend::instances::remove_instance(context)).await {
                log::error!("Failed to remove action: {error}");
                return;
            }
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    /// Step, add or remove a page, then re-read the profile so the window and the hardware both
    /// follow. Pages are profiles, so this is the same reload path every other mutation uses.
    fn page_action(&mut self, action: PageAction, cx: &mut Context<Self>) {
        let Some(device) = self.device.as_ref().map(|device| device.id.clone()) else {
            return;
        };
        let current = self.current_page.clone();

        cx.spawn(async move |this, cx| {
            let result = crate::bridge(async move {
                match action {
                    PageAction::Step(delta) => crate::pages::step(&device, delta).await,
                    PageAction::Add => crate::pages::add(&device).await.map(|_| ()),
                    PageAction::Remove => crate::pages::remove(&device, &current).await,
                }
            })
            .await;

            if let Err(error) = result {
                log::error!("Page operation failed: {error}");
                return;
            }
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    /// Configure one dial: a system action, or the user's own command per gesture of the knob.
    ///
    /// A dial is device-scoped - the knob is a fixed control, so it keeps doing the same thing
    /// whichever page is showing. The rectangle above it is the page-scoped half: it owns the
    /// artwork and the tap, and is configured by dropping an action onto it. The two no longer
    /// share a slot, so neither can overwrite the other.
    fn open_dial_dialog(&mut self, dial: u8, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.dials.get(dial as usize).cloned().flatten();

        let kind = match &existing {
            None => DialKind::None,
            Some(DialConfig::System { uuid }) => DialKind::System(SharedString::from(uuid.clone())),
            Some(DialConfig::Custom { .. }) => DialKind::Custom,
        };

        // Prefill from what the dial already does, so opening the modal and saving without
        // touching anything leaves it exactly as it was.
        let (name, left, right, centre) = match &existing {
            Some(DialConfig::Custom { name, left, right, centre }) => (name.clone(), left.clone(), right.clone(), centre.clone()),
            _ => (String::new(), String::new(), String::new(), String::new()),
        };

        let form = self.dial_form.clone();
        form.update(cx, |form, cx| {
            form.dial = dial;
            form.kind.update(cx, |state, cx| state.set_selected_value(&kind, window, cx));
            form.name.update(cx, |state, cx| state.set_value(name, window, cx));
            form.left.update(cx, |state, cx| state.set_value(left, window, cx));
            form.right.update(cx, |state, cx| state.set_value(right, window, cx));
            form.centre.update(cx, |state, cx| state.set_value(centre, window, cx));
        });

        let this = cx.entity().downgrade();
        let title = SharedString::from(format!("Dial {}", dial + 1));

        window.open_dialog(cx, move |dialog, window, cx| {
            let this = this.clone();
            let ok_form = form.clone();
            let kind_state = form.read(cx).kind.clone();
            let name_state = form.read(cx).name.clone();
            let left_state = form.read(cx).left.clone();
            let right_state = form.read(cx).right.clone();
            let centre_state = form.read(cx).centre.clone();
            // Only the custom entry takes commands; a system action configures itself.
            let is_custom = kind_state.read(cx).selected_value() == Some(&DialKind::Custom);

            dialog
                .title(title.clone())
                .button_props(DialogButtonProps::default().ok_text("Save").cancel_text("Cancel"))
                // Footer buttons only render when a footer is set - see `open_action_dialog`.
                .footer(|ok, cancel, window, cx| vec![cancel(window, cx), ok(window, cx)])
                .child(
                    v_flex()
                        .id("dial-form")
                        .max_h(form_max_height(window))
                        .overflow_y_scroll()
                        .gap_3()
                        .p_2()
                        .child(field("Action", Select::new(&kind_state).placeholder("Choose an action")))
                        .when(is_custom, |body| {
                            body.child(field("Name", Input::new(&name_state)))
                                .child(field("Left", Input::new(&left_state)))
                                .child(field("Right", Input::new(&right_state)))
                                .child(field("Centre", Input::new(&centre_state)))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Applies on every page. The strip above the dial is set per page, by dropping an action on it."),
                        ),
                )
                .on_ok(move |_event, _window, cx| {
                    let kind = kind_state.read(cx).selected_value().cloned().unwrap_or(DialKind::None);
                    let commands = DialCommands {
                        name: name_state.read(cx).value().trim().to_string(),
                        left: left_state.read(cx).value().to_string(),
                        right: right_state.read(cx).value().to_string(),
                        centre: centre_state.read(cx).value().to_string(),
                    };
                    let dial = ok_form.read(cx).dial;
                    let _ = this.update(cx, |this, cx| this.apply_dial(dial, kind, commands, cx));
                    true
                })
        });
    }

    /// What to caption a knob with: the system action's name, or "Custom" for shell commands.
    fn dial_label(&self, dial: u8) -> SharedString {
        match self.dials.get(dial as usize).and_then(|slot| slot.as_ref()) {
            None => SharedString::from("Unset"),
            // A name is optional, so an unnamed custom dial keeps the caption it always had.
            Some(DialConfig::Custom { name, .. }) if !name.is_empty() => SharedString::from(name.clone()),
            Some(DialConfig::Custom { .. }) => SharedString::from("Custom"),
            Some(DialConfig::System { uuid }) => crate::system_actions::CATALOGUE
                .iter()
                .find(|(_, candidate)| candidate == uuid)
                .map(|(label, _)| SharedString::from(*label))
                .unwrap_or_else(|| SharedString::from(uuid.clone())),
        }
    }

    /// Clear a dial from its right-click menu, leaving the rectangle above it untouched.
    fn unset_dial(&mut self, dial: u8, cx: &mut Context<Self>) {
        self.dial_menu_open = None;
        self.apply_dial(
            dial,
            DialKind::None,
            DialCommands {
                name: String::new(),
                left: String::new(),
                right: String::new(),
                centre: String::new(),
            },
            cx,
        );
    }

    /// Exchange what two dials do, from dragging one knob onto another.
    ///
    /// Always a swap rather than a move: a dial is its position on the device, so the destination's
    /// config has to go somewhere, and the source is the only knob free to take it. Dropping onto
    /// an unset dial is how one gets moved.
    fn swap_dials(&mut self, source: u8, destination: u8, cx: &mut Context<Self>) {
        if source == destination {
            return;
        }

        let Some(device) = self.device.as_ref() else { return };
        let (id, encoders) = (device.id.clone(), device.encoders as usize);

        cx.spawn(async move |this, cx| {
            let result = crate::bridge(async move {
                let mut locks = crate::store::profiles::acquire_locks_mut().await;
                locks.device_stores.swap_dials(&id, encoders, source, destination)
            })
            .await;

            if let Err(error) = result {
                log::error!("Failed to swap dials {source} and {destination}: {error}");
                return;
            }
            reload_dials(&this, cx).await;
        })
        .detach();
    }

    /// Save the dial dialog's result to the device's own store.
    fn apply_dial(&mut self, dial: u8, kind: DialKind, commands: DialCommands, cx: &mut Context<Self>) {
        let Some(device) = self.device.as_ref() else { return };
        let (id, encoders) = (device.id.clone(), device.encoders as usize);

        let config = match kind {
            DialKind::None => None,
            DialKind::System(uuid) => Some(DialConfig::System { uuid: uuid.to_string() }),
            DialKind::Custom => Some(DialConfig::Custom {
                name: commands.name,
                left: commands.left,
                right: commands.right,
                centre: commands.centre,
            }),
        };

        cx.spawn(async move |this, cx| {
            let result = crate::bridge(async move {
                let mut locks = crate::store::profiles::acquire_locks_mut().await;
                locks.device_stores.set_dial(&id, encoders, dial, config)
            })
            .await;

            if let Err(error) = result {
                log::error!("Failed to save dial {dial}: {error}");
                return;
            }
            reload_dials(&this, cx).await;
        })
        .detach();
    }

    /// A one-message dialog, for reporting something that finished without one.
    fn notice(&self, title: &'static str, message: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, move |dialog, _window, cx| {
            let message = message.clone();
            dialog
                .title(title)
                .button_props(DialogButtonProps::default().ok_text("OK"))
                // Footer buttons only render when a footer is set - see `open_action_dialog`.
                .footer(|ok, _cancel, window, cx| vec![ok(window, cx)])
                .child(div().p_2().text_sm().text_color(cx.theme().foreground).child(message))
        });
    }

    /// Write every part of the configuration to a zip the user names.
    fn export_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .set_file_name(crate::backup::archive_name())
                .add_filter("Backup", &["zip"])
                .save_file()
                .await
            else {
                return;
            };

            let destination = handle.path().to_path_buf();
            let result = crate::bridge(crate::backup::export(destination.clone())).await;

            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.notice("Backup saved", SharedString::from(format!("Saved to {}", destination.display())), window, cx),
                Err(error) => this.notice("Backup failed", SharedString::from(error.to_string()), window, cx),
            });
        })
        .detach();
    }

    /// Pick a backup to restore from, then ask before replacing anything.
    fn choose_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new().add_filter("Backup", &["zip"]).pick_file().await else {
                return;
            };

            let archive = handle.path().to_path_buf();
            let _ = this.update_in(cx, |this, window, cx| this.confirm_restore(archive, window, cx));
        })
        .detach();
    }

    /// Confirm a restore before it happens.
    ///
    /// This replaces the whole configuration rather than merging into it, which is not something to
    /// discover afterwards - so it is spelled out, and the button says what it does.
    fn confirm_restore(&mut self, archive: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let name = SharedString::from(archive.file_name().unwrap_or_default().to_string_lossy().into_owned());

        window.open_dialog(cx, move |dialog, _window, cx| {
            let this = this.clone();
            let archive = archive.clone();
            let name = name.clone();

            dialog
                .title("Restore from backup")
                .button_props(DialogButtonProps::default().ok_text("Replace everything").cancel_text("Cancel"))
                .footer(|ok, cancel, window, cx| vec![cancel(window, cx), ok(window, cx)])
                .child(
                    v_flex()
                        .gap_2()
                        .p_2()
                        .child(div().text_sm().text_color(cx.theme().foreground).child(name))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
                            "Every action, image, dial and page is replaced by what this backup holds. Your current configuration is moved aside, not deleted - the restore will say where.",
                        )),
                )
                .on_ok(move |_event, window, cx| {
                    let _ = this.update(cx, |this, cx| this.run_restore(archive.clone(), window, cx));
                    true
                })
        });
    }

    /// Replace the configuration, then rebuild everything on screen from what landed on disk.
    fn run_restore(&mut self, archive: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let result = crate::bridge(crate::backup::restore(archive)).await;

            let aside = match result {
                Ok(aside) => aside,
                Err(error) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.notice("Restore failed", SharedString::from(error.to_string()), window, cx);
                    });
                    return;
                }
            };

            // The stores were dropped as part of the swap, so everything held here is now stale:
            // the library, the catalogue, the visible profile, the dials, and the artwork on the
            // deck itself. Rebuild all of it from the restored files.
            let _ = this.update(cx, |this, cx| {
                this.custom = custom_actions::load();
                this.predefined = custom_actions::load_predefined();
                cx.notify();
            });
            refresh_catalogue(&this, cx).await;
            reload_profile(&this, cx).await;
            reload_dials(&this, cx).await;

            let _ = this.update_in(cx, |this, window, cx| {
                this.notice(
                    "Restored",
                    SharedString::from(format!("Your previous configuration was moved to {}", aside.display())),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Run a custom action's command directly, without going through the deck.
    fn execute_command(&mut self, command: String, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        cx.notify();
        crate::system_actions::run_shell(command);
    }

    fn delete_custom_action(&mut self, id: String, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        // Read before the directory goes: slots reference the entry by its stable id, not by the
        // slug the row is keyed on.
        let custom_id = self.custom.iter().find(|action| action.slug == id).map(|action| action.id().to_owned());
        if let Err(error) = custom_actions::delete(&id) {
            log::error!("Failed to delete custom action: {error}");
            return;
        }
        self.custom.retain(|action| action.slug != id);
        cx.notify();

        // The slots made from it have to go with it. A slot carries its own copy of the command and
        // artwork, so deleting the entry alone leaves a face that draws black - its artwork went
        // with the directory - while still running when pressed and still holding the slot against
        // anything else being put there.
        let Some(custom_id) = custom_id else { return };
        cx.spawn(async move |this, cx| {
            let touched = crate::bridge(crate::store::profiles::discard_custom_action(custom_id)).await;
            crate::bridge(crate::animation::forget_frames()).await;
            for device in touched {
                crate::bridge(crate::device_render::repaint(device)).await;
            }
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    /// Open the action form, either blank or prepopulated from an existing action.
    ///
    /// Field state lives on the shell so it survives the dialog's re-renders; the dialog builder
    /// only reads it.
    fn open_action_dialog(&mut self, edit: Option<CustomAction>, window: &mut Window, cx: &mut Context<Self>) {
        self.row_menu_open = None;
        let existing = edit.clone();
        let form = self.form.clone();

        // Built from the live catalogue, which is empty when the shell is constructed.
        let kinds = action_choices(&self.categories);
        let kind = match existing.as_ref().and_then(|action| action.action_uuid()) {
            Some(uuid) => ActionKind::Builtin(SharedString::from(uuid.to_owned())),
            None => ActionKind::Command,
        };

        let stored = existing.as_ref().and_then(|action| action.config.animation.clone());
        let animation_kind = match &stored {
            Some(animation) => AnimationKind::Generated(animation.effect),
            None => AnimationKind::None,
        };
        let speed = stored.as_ref().map(|animation| animation.speed).unwrap_or(Speed::Normal);
        let trigger = stored.as_ref().map(|animation| animation.trigger).unwrap_or(Trigger::Always);

        form.update(cx, |form, cx| {
            // Items first: `set_items` swaps the list without touching the selection, so selecting
            // before refilling would look up the value in the old list.
            form.kind.update(cx, |state, cx| {
                state.set_items(kinds, window, cx);
                state.set_selected_value(&kind, window, cx);
            });
            form.animation.update(cx, |state, cx| state.set_selected_value(&animation_kind, window, cx));
            form.speed.update(cx, |state, cx| state.set_selected_value(&speed, window, cx));
            form.trigger.update(cx, |state, cx| state.set_selected_value(&trigger, window, cx));
            form.name
                .update(cx, |state, cx| state.set_value(existing.as_ref().map(|a| a.name().to_owned()).unwrap_or_default(), window, cx));
            form.command
                .update(cx, |state, cx| state.set_value(existing.as_ref().map(|a| a.command().to_owned()).unwrap_or_default(), window, cx));
            form.spec = ImageSpec::default();
            form.editing = existing.as_ref().map(|a| a.slug.clone());
            form.error = None;
            form.invalid = false;
            form.probing = false;
            form.saving = false;

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

        window.open_dialog(cx, move |dialog, window, cx| {
            let this = this.clone();
            let clear_form = form.clone();
            let pick_image = form.clone();
            let ok_form = form.clone();
            let name_state = form.read(cx).name.clone();
            let kind_state = form.read(cx).kind.clone();
            let command_state = form.read(cx).command.clone();
            // Only a command entry takes a command; a built-in action carries its own behaviour and
            // takes nothing from this form but the artwork.
            let is_command = !matches!(kind_state.read(cx).selected_value(), Some(ActionKind::Builtin(_)));
            let background_state = form.read(cx).background.clone();
            let animation_state = form.read(cx).animation.clone();
            let speed_state = form.read(cx).speed.clone();
            let trigger_state = form.read(cx).trigger.clone();
            // Speed only means something once there is something to play.
            let animated = !matches!(animation_state.read(cx).selected_value(), Some(AnimationKind::None) | None);
            let preview = form.read(cx).preview();
            // Show what the key will actually look like: the chosen colour behind the image, with
            // a transparent icon inset so the colour reads as a border - the same rule the
            // compositor applies when writing picture.png.
            let preview_background = background_state.read(cx).value();
            let preview_is_icon = form.read(cx).image_is_icon;
            let error = form.read(cx).error.clone();
            let saving = form.read(cx).saving;
            let probing = form.read(cx).probing;
            // Only mark a blank field once a save has actually been attempted - a form that opens
            // covered in red is shouting before the user has done anything wrong.
            let invalid = form.read(cx).invalid;
            let name_blank = invalid && name_state.read(cx).value().trim().is_empty();
            let command_blank = invalid && is_command && command_state.read(cx).value().trim().is_empty();

            dialog
                .title(title)
                .button_props(DialogButtonProps::default().ok_text("Save").cancel_text("Cancel"))
                // Footer buttons only render when a footer is set - `button_props` alone is
                // just labels, which is why Save was missing.
                .footer(|ok, cancel, window, cx| vec![cancel(window, cx), ok(window, cx)])
                .child(
                    v_flex()
                        // Scrolls rather than overflowing when the window is too short for every
                        // field. Needs an id: scroll position is element state.
                        .id("action-form")
                        .max_h(form_max_height(window))
                        .overflow_y_scroll()
                        .gap_3()
                        .p_2()
                        .child(field("Name", Input::new(&name_state).border_color(required(name_blank, cx))))
                        .child(field("Action", Select::new(&kind_state).placeholder("Choose an action")))
                        .when(is_command, |body| {
                            body.child(field("Command", Input::new(&command_state).border_color(required(command_blank, cx))))
                        })
                        .child(field("Background", ColorPicker::new(&background_state)))
                        .child(field(
                            "Image",
                            h_flex()
                                .gap_2()
                                .items_center()
                                // Both faces are shown, because the same source is composited twice
                                // and a photo that suits a square key can be cropped unrecognisably
                                // by the strip's 2:1 rectangle. They always render, so a
                                // colour-only action still previews once the image is cleared.
                                .child(preview_tile(
                                    "Key",
                                    PREVIEW_HEIGHT,
                                    PREVIEW_HEIGHT,
                                    preview.as_deref(),
                                    preview_background,
                                    preview_is_icon,
                                    cx,
                                ))
                                .child(preview_tile(
                                    "Strip",
                                    PREVIEW_HEIGHT * 2.0,
                                    PREVIEW_HEIGHT,
                                    preview.as_deref(),
                                    preview_background,
                                    preview_is_icon,
                                    cx,
                                ))
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
                        ))
                        .child(field("Animation", Select::new(&animation_state).placeholder("None")))
                        .when(animated, |body| {
                            body.child(field("Speed", Select::new(&speed_state)))
                                .child(field("Plays", Select::new(&trigger_state)))
                        })
                        // Compositing happens on a worker, so say that it is happening rather than
                        // leaving the dialog looking inert.
                        .when(probing, |body| {
                            body.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Reading image…"))
                        })
                        .when(saving, |body| {
                            body.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Composing artwork…"))
                        })
                        .children(error.map(|message| div().text_sm().text_color(cx.theme().danger).child(message))),
                )
                .on_ok(move |_event, _window, cx| {
                    let name = name_state.read(cx).value().to_string();
                    // A built-in action needs no command, so it does not have to have one: the
                    // field is not even shown, and requiring it would make the entry unsaveable.
                    let builtin = match kind_state.read(cx).selected_value() {
                        Some(ActionKind::Builtin(uuid)) => Some(uuid.to_string()),
                        _ => None,
                    };
                    let command = if builtin.is_some() { String::new() } else { command_state.read(cx).value().to_string() };
                    if name.trim().is_empty() || (builtin.is_none() && command.trim().is_empty()) {
                        // Keep the dialog open until the required fields are filled in, and mark
                        // which of them are blank.
                        ok_form.update(cx, |form, cx| {
                            form.invalid = true;
                            cx.notify();
                        });
                        return false;
                    }
                    if ok_form.read(cx).saving {
                        // A save is already in flight; a second one would race it onto the same
                        // directory.
                        return false;
                    }

                    let animation = match animation_state.read(cx).selected_value() {
                        Some(AnimationKind::Generated(effect)) => Some(Animation {
                            effect: *effect,
                            speed: speed_state.read(cx).selected_value().copied().unwrap_or(Speed::Normal),
                            trigger: trigger_state.read(cx).selected_value().copied().unwrap_or(Trigger::Always),
                        }),
                        _ => None,
                    };

                    let (mut spec, editing) = ok_form.read_with(cx, |form, _| (form.spec.clone(), form.editing.clone()));
                    spec.background = ok_form.read(cx).background.read(cx).value().map(hsla_to_hex);
                    let draft = ActionDraft {
                        name,
                        command,
                        builtin,
                        animation,
                        spec,
                        editing,
                    };
                    let _ = this.update(cx, |this, cx| this.save_custom_action(draft, cx));
                    true
                })
        });
    }

    /// Composite and store an action, then fold the result back into the library.
    ///
    /// The compositing runs on a worker: it decodes the source, resizes it twice and re-encodes
    /// both faces, which on a large photo is seconds of work. Doing that inline froze the window
    /// until it finished - and because nothing else could run in the meantime, the form appeared to
    /// accept only one action per launch.
    fn save_custom_action(&mut self, draft: ActionDraft, cx: &mut Context<Self>) {
        let ActionDraft {
            name,
            command,
            builtin,
            animation,
            spec,
            editing,
        } = draft;

        let form = self.form.clone();
        form.update(cx, |form, cx| {
            form.saving = true;
            form.error = None;
            cx.notify();
        });

        cx.spawn(async move |this, cx| {
            let edited = editing.clone();
            let result = crate::bridge(async move {
                tokio::task::spawn_blocking(move || match &editing {
                    Some(id) => custom_actions::update(id, name, command, builtin, animation, &spec),
                    None => custom_actions::create(name, command, builtin, animation, &spec),
                })
                .await
                .unwrap_or_else(|error| Err(anyhow::anyhow!("compositing panicked: {error}")))
            })
            .await;

            let _ = form.update(cx, |form, cx| {
                form.saving = false;
                if let Err(error) = &result {
                    form.error = Some(SharedString::from(format!("Could not save: {error}")));
                }
                cx.notify();
            });

            let action = match result {
                Ok(action) => action,
                Err(error) => {
                    log::error!("Failed to save custom action: {error}");
                    return;
                }
            };

            let _ = this.update(cx, |this, cx| {
                match &edited {
                    // An edit can rename the directory, so match on the id we edited, not the new one.
                    Some(id) => match this.custom.iter_mut().find(|candidate| candidate.slug == *id) {
                        Some(existing) => *existing = action.clone(),
                        None => this.custom.push(action.clone()),
                    },
                    None => this.custom.push(action.clone()),
                }

                // Artwork is rewritten to the same `picture.png` path, so GPUI would keep serving
                // the bitmap it decoded earlier - evict it, or the sidebar and any slot already
                // using this action keep showing the old picture.
                for path in [action.image_path(), action.strip_path()] {
                    let source: gpui::ImageSource = path.into();
                    source.remove_asset(cx);
                }
                cx.notify();
            });

            // The frames on disk have just been rewritten, so anything already rendered from them
            // is stale for exactly the same reason the GPUI asset cache above is.
            crate::bridge(crate::animation::forget_frames()).await;

            // Slots already carrying this action need re-pushing so the hardware follows too.
            reload_profile(&this, cx).await;
        })
        .detach();
    }

    /// Place a custom action on a slot: create the Run Command instance that backs it, then write
    /// its command and image onto that instance.
    fn handle_create_custom(&mut self, action: CustomAction, destination: SlotContext, cx: &mut Context<Self>) {
        // A predefined entry names the built-in action it places; a user's own runs a shell
        // command through Run Command.
        let wanted = action.action_uuid().unwrap_or(RUN_COMMAND_UUID).to_owned();
        let Some(base_action) = self
            .categories
            .iter()
            .flat_map(|(_, category)| category.actions.iter())
            .find(|candidate| candidate.uuid == wanted)
            .cloned()
        else {
            log::error!("Cannot place action: {wanted} is not registered");
            return;
        };
        let is_command = action.action_uuid().is_none();

        cx.spawn(async move |this, cx| {
            let Ok(Some(instance)) = crate::bridge(frontend::instances::create_instance(base_action, destination)).await else {
                return;
            };
            let context = instance.context.clone();

            // The action's one command is written to both gestures a slot can receive: `down` for
            // a key press, `touch` for a tap of the strip rectangle. A key never receives a tap and
            // a rectangle never receives a press (dials are device-scoped and do not reach plugins
            // at all), so exactly one of the two ever fires and the action needs only one command.
            //
            // `rustydeck_custom` links the slot back to the library entry it came from, so editing
            // that entry later updates this slot rather than leaving a stale copy behind.
            // `RunCommandSettings` is `#[serde(default)]` and ignores keys it does not know.
            let payload = if is_command {
                serde_json::json!({
                    "down": action.command(),
                    "touch": action.command(),
                    "rustydeck_custom": action.id(),
                })
            } else {
                serde_json::json!({ "rustydeck_custom": action.id() })
            };
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
                // The strip rectangle is 2:1, so it gets the face composed for that shape - showing
                // the square key face there would stretch the artwork.
                let artwork = if context.controller == ENCODER_CONTROLLER { action.strip_path() } else { action.image_path() };
                state.image = artwork.to_string_lossy().into_owned();
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
        self.device_auto_selected = false;
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
            reload_dials(&this, cx).await;
            push_device_images(device, profile);
        })
        .detach();
    }

    /// Create a new instance from a palette entry dropped onto a slot.
    fn handle_create(&mut self, action: Action, destination: SlotContext, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // `create_instance` returns `None` for an occupied slot or an unsupported controller,
            // so an invalid drop is already a safe no-op.
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
            // Actions without artwork (the page commands, for now) carry an empty path, which
            // would resolve to the config directory and fail to load.
            let image_path = state.map(|state| state.image.clone()).filter(|path| !path.is_empty());

            if let Some(path) = image_path.clone() {
                // Match what the hardware shows. `device_render` stretches the artwork to the
                // slot's aspect, so a square icon on a 2:1 strip segment fills it; GPUI's default
                // `Contain` would letterbox it into a centred square with blank sides instead.
                cell = cell.child(img(resolve_image_path(&path)).size_full().object_fit(gpui::ObjectFit::Fill));
            }

            // Clicking a filled slot runs it, exactly as using the physical control would -
            // both of these drive the same entry points the driver does.
            // A click and a drag do not conflict: GPUI only starts a drag past its 2px threshold.
            //
            // A strip segment's gesture is a tap, not a press, on every device rather than only a
            // simulated one: the rectangle owns the tap command and the dial beneath it owns the
            // press, so sending the click down the press path runs the dial's command instead.
            let press_context = context.clone();
            let taps = controller == ENCODER_CONTROLLER;
            cell = cell.on_click(move |_event, _window, cx| {
                let context = press_context.clone();
                cx.background_spawn(async move {
                    let result = if taps {
                        crate::bridge(frontend::instances::trigger_virtual_tap(context)).await
                    } else {
                        crate::bridge(frontend::instances::trigger_virtual_press(context)).await
                    };
                    if let Err(error) = result {
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

        if slot.is_some() {
            let menu_context = context.clone();
            cell = cell.on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _event, _window, cx| {
                    this.slot_menu_open = Some(menu_context.clone());
                    cx.notify();
                }),
            );

            if self.slot_menu_open.as_ref() == Some(&context) {
                let delete_context = context.clone();
                cell = cell.relative().child(
                    // Same layering as the sidebar row menu: painted last so nothing covers it,
                    // and occluding so the click cannot fall through to the slot beneath.
                    deferred(
                        v_flex()
                            .occlude()
                            .absolute()
                            .top_full()
                            .left_0()
                            .w(px(ROW_MENU_WIDTH))
                            .p_1()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(opaque(cx.theme().background))
                            .shadow_lg()
                            .child(
                                div()
                                    .id("slot-delete")
                                    .w_full()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .hover(|style| style.bg(cx.theme().accent))
                                    .child("Delete")
                                    .on_click(cx.listener(move |this, _event, _window, cx| {
                                        this.delete_slot(delete_context.clone(), cx);
                                    })),
                            ),
                    )
                    .with_priority(1),
                );
            }
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
/// Runs on the Tokio runtime rather than being awaited on the window's executor: compositing a
/// profile full of artwork is long enough to stall the window - it was why the palette took a
/// moment to catch up after saving an action.
fn push_device_images(device: DeviceInfo, profile: Profile) {
    crate::spawn(crate::device_render::push_profile(device, profile));
}

/// Re-read the selected profile from the backend and push it to the hardware.
///
/// Cheaper to re-read than to replay the backend's relocation rules up here - contexts, child
/// indices and image paths all shift on a move, and one source of truth is enough.
/// Re-read the device's dials into the shell.
///
/// Separate from [`reload_profile`] because dials are device-scoped: they do not change when the
/// page does, and they outlive any one profile.
async fn reload_dials(this: &WeakEntity<RustyDeckShell>, cx: &mut gpui::AsyncApp) {
    let Some(device) = this.update(cx, |this, _| this.device.clone()).ok().flatten() else {
        return;
    };
    let (id, encoders) = (device.id.clone(), device.encoders as usize);

    let Ok(dials) = crate::bridge(async move {
        let mut locks = crate::store::profiles::acquire_locks_mut().await;
        locks.device_stores.get_dials(&id, encoders)
    })
    .await
    else {
        return;
    };

    let _ = this.update(cx, |this, cx| {
        this.dials = dials;
        cx.notify();
    });
}

/// Whether re-reading the page also has to repaint the deck.
enum Repaint {
    /// The window changed something the hardware cannot know about, so it is told either way.
    Always,
    /// Something else has already painted this page - a page swap repaints from `pages::show`,
    /// which is what a deck with no window open relies on. Only push if the sync below moved the
    /// page out from under that.
    IfStale,
}

async fn reload_profile(this: &WeakEntity<RustyDeckShell>, cx: &mut gpui::AsyncApp) {
    reload_page(this, cx, Repaint::Always).await;
}

async fn reload_page(this: &WeakEntity<RustyDeckShell>, cx: &mut gpui::AsyncApp, repaint: Repaint) {
    let _timed = crate::shared::Timed::start("reload_page");
    let Some(device) = this.update(cx, |this, _| this.device.clone()).ok().flatten() else {
        return;
    };
    let Ok(profile) = crate::bridge(frontend::profiles::get_selected_profile(device.id.clone())).await else {
        return;
    };

    // Bring any key created from a custom action back in line with the library, in case that
    // action was edited since. Only the visible page needs this: the deck shows one page at a
    // time, and every page switch comes back through here. Re-read afterwards, or the copy in
    // hand - and so the window and the images pushed to the deck - would still be the stale one.
    let stale = sync_custom_instances(&device, &profile).await;
    let profile = if stale {
        match crate::bridge(frontend::profiles::get_selected_profile(device.id.clone())).await {
            Ok(refreshed) => refreshed,
            Err(_) => profile,
        }
    } else {
        profile
    };

    let device_id = device.id.clone();
    let device_id_for_current = device.id.clone();
    let pages = crate::bridge(async move { crate::pages::list(&device_id) }).await;
    let current_page = crate::bridge(async move { crate::pages::current(&device_id_for_current).await }).await;

    let _ = this.update(cx, |this, cx| {
        this.profile = Some(profile.clone());
        this.pages = pages;
        this.current_page = current_page;
        cx.notify();
    });

    if stale || matches!(repaint, Repaint::Always) {
        push_device_images(device, profile);
    }
}

/// Re-apply the current definition of any custom action that a slot was created from.
///
/// Instances store a copy of the command and artwork so the plugin and the device renderer can
/// work without consulting us; `rustydeck_custom` is what lets that copy be refreshed instead of
/// silently drifting from the library.
/// Returns whether anything was changed, so the caller knows to re-read the profile.
async fn sync_custom_instances(device: &DeviceInfo, profile: &Profile) -> bool {
    let _timed = crate::shared::Timed::start("sync_custom_instances");
    let mut library = custom_actions::load();
    library.extend(custom_actions::load_predefined());
    if library.is_empty() {
        return false;
    }
    let mut changed = false;

    for controller in [KEYPAD_CONTROLLER, ENCODER_CONTROLLER, INFOBAR_CONTROLLER] {
        for (position, slot) in slots(profile, controller).iter().enumerate() {
            let Some(instance) = slot else { continue };
            let Some(id) = instance.settings.get("rustydeck_custom").and_then(|id| id.as_str()) else {
                continue;
            };
            let Some(action) = library.iter().find(|action| action.id() == id) else {
                continue;
            };

            // The strip rectangle is 2:1 and gets the face composed for that shape.
            let artwork = if controller == ENCODER_CONTROLLER { action.strip_path() } else { action.image_path() };
            let image = artwork.to_string_lossy().into_owned();

            // Predefined entries place a built-in action and have no command to keep in step.
            // Both gestures carry the same command - see `handle_create_custom` - so both have to
            // match, or a slot created before one of them existed never gets brought back in line.
            let setting = |key: &str| instance.settings.get(key).and_then(|value| value.as_str());
            let command_matches =
                action.action_uuid().is_some() || (setting("down") == Some(action.command()) && setting("touch") == Some(action.command()));
            let image_matches = instance.states.first().map(|state| state.image.as_str()) == Some(image.as_str());
            if command_matches && image_matches {
                continue;
            }

            let context = crate::shared::ActionContext::from_context(
                SlotContext {
                    device: device.id.clone(),
                    profile: profile.id.clone(),
                    controller: controller.to_owned(),
                    position: position as u8,
                },
                0,
            );

            if !command_matches {
                let event = crate::events::inbound::ContextAndPayloadEvent {
                    context: context.clone(),
                    payload: serde_json::json!({
                        "down": action.command(),
                        "touch": action.command(),
                        "rustydeck_custom": id,
                    }),
                };
                if let Err(error) = crate::bridge(crate::events::inbound::settings::set_settings(event, true)).await {
                    log::error!("Failed to re-sync custom action command: {error}");
                }
            }

            if !image_matches && let Some(mut state) = instance.states.first().cloned() {
                state.image = image;
                if let Err(error) = crate::bridge(frontend::instances::set_state(context, 0, state)).await {
                    log::error!("Failed to re-sync custom action image: {error}");
                }
            }

            changed = true;
        }
    }

    changed
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

            // Only auto-select when nothing is chosen, or when we are still showing a simulated
            // deck we picked ourselves and real hardware has since enumerated. A device the user
            // chose is never overridden.
            let hardware = this.devices.iter().find(|device| !is_simulated(&device.id)).cloned();
            let selected = match &this.device {
                None => hardware.or_else(|| this.devices.first().cloned()),
                Some(current) if this.device_auto_selected && is_simulated(&current.id) => hardware,
                Some(_) => None,
            };
            if let Some(device) = &selected {
                this.device = Some(device.clone());
                this.device_auto_selected = true;
            }
            cx.notify();
            selected
        })
        .ok()
        .flatten();

    if newly_selected.is_some() {
        reload_profile(this, cx).await;
        reload_dials(this, cx).await;
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

#[derive(Clone, Copy)]
enum PageAction {
    /// Move by this many pages, wrapping.
    Step(i32),
    Add,
    Remove,
}

/// How the artwork will look on one face, captioned with which face it is.
///
/// Mirrors what `custom_actions::compose_canvas` does rather than approximating it: the background
/// is painted first, then a transparent icon is inset by a tenth and scaled to fit so the colour
/// reads as a border, while an opaque picture is centre-cropped to fill. Getting that wrong here
/// would be worse than showing nothing, because the preview is the only thing telling the user
/// what they are about to save.
fn preview_tile(
    label: &'static str,
    width: f32,
    height: f32,
    image: Option<&str>,
    background: Option<gpui::Hsla>,
    is_icon: bool,
    cx: &App,
) -> impl IntoElement {
    let (inset_x, inset_y) = if is_icon { (width * 0.1, height * 0.1) } else { (0.0, 0.0) };
    let fit = if is_icon { gpui::ObjectFit::Contain } else { gpui::ObjectFit::Cover };

    v_flex()
        .gap_1()
        .items_center()
        .child(
            div()
                .w(px(width))
                .h(px(height))
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .overflow_hidden()
                // The inset is a margin on all four sides, as in `compose_canvas`, so the image box
                // is centred here rather than expressed as padding on a wrapper.
                .flex()
                .items_center()
                .justify_center()
                .when_some(background, |tile, colour| tile.bg(colour))
                .children(image.map(|path| {
                    // Both dimensions given explicitly, and deliberately: `Img` sets `aspect_ratio`
                    // from the image itself, which with a `size_full` box wins over the tile's own
                    // shape. A square image in the 2:1 strip tile then laid itself out as wide as
                    // the tile and twice as tall, from the top - so the preview showed the top of
                    // the artwork rather than the middle. A wide photo was close enough to 2:1 to
                    // look right, which is why only small square images showed it.
                    img(resolve_image_path(path))
                        .w(px(width - inset_x * 2.0))
                        .h(px(height - inset_y * 2.0))
                        .object_fit(fit)
                })),
        )
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label))
}

/// The border a required field should wear: red when it has been left blank, otherwise the input's
/// ordinary one.
fn required(blank: bool, cx: &App) -> gpui::Hsla {
    if blank { cx.theme().danger } else { cx.theme().input }
}

/// A labelled form field in the create dialog.
fn field(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(div().text_sm().child(label)).child(control)
}

/// How tall a dialog's fields may be before they scroll.
///
/// A dialog grows with the fields in it and the window does not: the create form is eight fields
/// tall now, and in a short window the last of them - along with the footer holding Save - had
/// nowhere to go. Bounded against the viewport rather than a fixed number, so a tall window still
/// shows the whole form with no scrollbar at all.
///
/// A fraction rather than the height minus a constant, because the constant would have to be the
/// dialog's title, its footer, its padding *and* the margin it keeps off the window edge - four
/// numbers owned by the component library, any of which can change under us. A share of the window
/// needs none of them and cannot drift.
///
/// The floor stops a very short window collapsing the fields to nothing, which would be worse than
/// the overflow it is meant to fix.
fn form_max_height(window: &Window) -> gpui::Pixels {
    const SHARE: f32 = 0.62;
    const FLOOR: f32 = 200.0;
    let height: f32 = window.viewport_size().height.into();
    px((height * SHARE).max(FLOOR))
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

        // Refuse before decoding anything, on the axis that actually costs time. The header gives
        // the dimensions without touching the pixels.
        let (within_limit, megapixels) = custom_actions::within_pixel_limit(&path);
        if !within_limit {
            let message = SharedString::from(format!(
                "That image is {megapixels:.0} megapixels. The limit is {:.0} - resize it and try again.",
                custom_actions::MAX_MEGAPIXELS,
            ));
            let _ = form.update(cx, |form, cx| {
                form.error = Some(message);
                form.probing = false;
                cx.notify();
            });
            return;
        }

        // Deciding icon-vs-photo decodes the file and scans every pixel, so it belongs on a worker
        // rather than the thread painting the window. It still takes a moment on a large photo, so
        // say so.
        let _ = form.update(cx, |form, cx| {
            form.probing = true;
            form.error = None;
            cx.notify();
        });

        let probe = path.clone();
        let is_icon = crate::bridge(async move {
            tokio::task::spawn_blocking(move || custom_actions::has_transparency(&probe))
                .await
                .unwrap_or(false)
        })
        .await;

        let _ = form.update(cx, |form, cx| {
            form.spec.file = Some(path);
            form.image_is_icon = is_icon;
            form.existing_image = None;
            form.error = None;
            form.probing = false;
            cx.notify();
        });
    })
    .detach();
}

/// What a palette row does when a control on it is clicked.
///
/// `Rc` rather than a boxed closure because one row hands the same handler to several places, and
/// the rows are rebuilt on every render.
type RowAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row in the palette. A single type covers all three kinds so both sidebar sections share it -
/// `Sidebar` is generic over one child type.
///
/// Implemented as its own component rather than `SidebarMenuItem` so it can carry `on_drag`.
#[derive(IntoElement)]
enum PaletteRow {
    /// Opens the create-action dialog.
    Create { collapsed: bool, on_click: RowAction },
    Custom {
        action: CustomAction,
        collapsed: bool,
        menu_open: bool,
        on_menu: RowAction,
        on_execute: RowAction,
        on_edit: RowAction,
        on_delete: RowAction,
    },
    Predefined { action: CustomAction, collapsed: bool },
}

/// One entry in a custom action's `...` menu.
fn menu_entry(id: &'static str, label: &'static str, on_click: RowAction, cx: &App) -> impl IntoElement {
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
            PaletteRow::Create { collapsed, .. }
            | PaletteRow::Custom { collapsed, .. }
            | PaletteRow::Predefined { collapsed, .. } => *collapsed = value,
        }
        self
    }

    fn is_collapsed(&self) -> bool {
        match self {
            PaletteRow::Create { collapsed, .. }
            | PaletteRow::Custom { collapsed, .. }
            | PaletteRow::Predefined { collapsed, .. } => *collapsed,
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
                let background = action.config.background.as_deref().and_then(hex_to_hsla);
                palette_row(SharedString::from(action.slug.clone()), cx)
                    .relative()
                    .child(
                        div()
                            .size(px(20.0))
                            .flex_none()
                            .rounded_sm()
                            .overflow_hidden()
                            .when_some(background, |tile, colour| tile.bg(colour))
                            .child(img(action.image_path()).size_full()),
                    )
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
                            // `deferred` paints the menu after everything else, so later sidebar
                            // rows cannot cover it - and since GPUI hit-tests in paint order, this
                            // is also what stops clicks landing on the row underneath. `occlude`
                            // blocks the mouse from reaching whatever is behind it.
                            deferred(
                                v_flex()
                                    .occlude()
                                    .absolute()
                                    .top_0()
                                    // Opens to the right of the sidebar rather than over it.
                                    .right(px(-(ROW_MENU_WIDTH + GAP)))
                                    .w(px(ROW_MENU_WIDTH))
                                    .p_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(opaque(cx.theme().background))
                                    .shadow_lg()
                                    .child(menu_entry("execute", "Execute", on_execute, cx))
                                    .child(menu_entry("edit", "Edit", on_edit, cx))
                                    .child(menu_entry("delete", "Delete", on_delete, cx)),
                            )
                            .with_priority(1),
                        )
                    })
                    .on_drag(DraggedCustomAction { action }, |dragged, _offset, _window, cx| {
                        let image = Some(dragged.action.image_path().to_string_lossy().into_owned());
                        cx.new(|_| DragPreview { image })
                    })
                    .into_any_element()
            }

            PaletteRow::Predefined { action, collapsed } => {
                let background = action.config.background.as_deref().and_then(hex_to_hsla);
                palette_row(SharedString::from(action.slug.clone()), cx)
                    .child(
                        div()
                            .size(px(20.0))
                            .flex_none()
                            .rounded_sm()
                            .overflow_hidden()
                            .when_some(background, |tile, colour| tile.bg(colour))
                            .child(img(action.image_path()).size_full()),
                    )
                    .when(!collapsed, |row| row.child(div().text_sm().child(SharedString::from(action.name().to_owned()))))
                    .on_drag(DraggedCustomAction { action }, |dragged, _offset, _window, cx| {
                        let image = Some(dragged.action.image_path().to_string_lossy().into_owned());
                        cx.new(|_| DragPreview { image })
                    })
                    .into_any_element()
            }

        }
    }
}

struct DragPreview {
    image: Option<String>,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut preview = div().size(px(CELL_SIZE)).bg(rgb(0x45475a)).rounded_md();
        if let Some(path) = self.image.clone().filter(|path| !path.is_empty()) {
            preview = preview.child(img(resolve_image_path(&path)).size_full());
        }
        preview
    }
}

/// What follows the cursor while a dial is being dragged: the knob itself, captioned as on screen.
struct DialPreview {
    label: SharedString,
}

impl Render for DialPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .items_center()
            .gap_1()
            .child(div().size(px(DIAL_SIZE)).rounded_full().bg(cx.theme().accent).border_1().border_color(cx.theme().primary))
            .child(div().text_xs().text_color(cx.theme().foreground).child(self.label.clone()))
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
            // `deferred` paints this after the rest of the tree, so it sits above the device slots
            // instead of sliding behind them when the window is small enough for them to overlap;
            // `occlude` stops a click passing through to whatever is underneath. The same pairing
            // the row and slot menus use.
            deferred(
                v_flex()
                    .occlude()
                    .absolute()
                    // Held off the window edge rather than flush against it.
                    .top(px(49.0))
                    .right(px(5.0))
                    .w(px(240.0))
                    .p_1()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(opaque(cx.theme().background))
                    .shadow_lg()
                    .children({
                        // Real hardware first, then the simulated models under a heading, so a fake
                        // deck is never mistaken for something that is actually plugged in.
                        let mut devices: Vec<DeviceInfo> = self.devices.to_vec();
                        devices.sort_by_key(|device| (is_simulated(&device.id), device.name.clone()));
                        let first_simulated = devices.iter().position(|device| is_simulated(&device.id));

                        let mut rows = Vec::with_capacity(devices.len());
                        for (index, device) in devices.into_iter().enumerate() {
                            let simulated = is_simulated(&device.id);
                            let selected = self.device.as_ref().is_some_and(|current| current.id == device.id);
                            let label = SharedString::from(device.name.clone());

                            rows.push(
                                v_flex()
                                    .w_full()
                                    // The divider carries the heading, so it only shows when there is
                                    // real hardware above it to divide from.
                                    .when(first_simulated == Some(index) && index > 0, |column| {
                                        column.child(
                                            div()
                                                .w_full()
                                                .mt_1()
                                                .pt_1()
                                                .px_2()
                                                .border_t_1()
                                                .border_color(cx.theme().border)
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Simulated"),
                                        )
                                    })
                                    .child(
                                        h_flex()
                                            .id(SharedString::from(device.id.clone()))
                                            .w_full()
                                            .p_2()
                                            .rounded_md()
                                            .text_sm()
                                            .when(simulated, |row| row.bg(rgb(SIMULATED_TINT)))
                                            .when(selected, |row| row.bg(cx.theme().accent))
                                            .hover(|style| style.bg(cx.theme().accent))
                                            .child(label)
                                            .on_click(cx.listener(move |this, _event, _window, cx| this.select_device(device.clone(), cx))),
                                    ),
                            );
                        }
                        rows
                    })
            )
            .with_priority(1)
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
                h_flex()
                    .gap_1()
                    .items_center()
                    // Restore reads a backup in; export writes one out. Arrows rather than the
                    // tray icons, because the pair has to read as opposite directions at 16px.
                    .child(
                        Button::new("restore-backup")
                            .icon(IconName::ArrowUp)
                            .tooltip("Restore from a backup")
                            .on_click(cx.listener(|this, _event, window, cx| this.choose_backup(window, cx))),
                    )
                    .child(
                        Button::new("export-backup")
                            .icon(IconName::ArrowDown)
                            .tooltip("Export a backup")
                            .on_click(cx.listener(|this, _event, window, cx| this.export_backup(window, cx))),
                    )
                    .child(
                        div().relative().child(
                            Button::new("swap-device")
                                .icon(IconName::Replace)
                                .tooltip("Switch device")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.device_picker_open = !this.device_picker_open;
                                    cx.notify();
                                })),
                        ),
                    )
                    // The same thing the compositor's own close does: the window goes away and the
                    // deck carries on being served from the tray. Under Hyprland there is no
                    // titlebar to close from, so without this there is no close control at all.
                    .child(
                        Button::new("close-window")
                            .icon(IconName::Close)
                            .tooltip("Close window")
                            .on_click(|_event, _window, _cx| crate::dismiss_window()),
                    ),
            )
            .children(picker)
    }

    /// The action palette: every action the installed plugins expose, dragged onto a slot to
    /// create an instance. Collapses to icons when the window is narrow.
    fn render_sidebar(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let create = entity.clone();
        let open_dialog: RowAction = Rc::new(move |window, cx| {
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

        // Predefined entries carry their own section name, so the shipped groups sit apart from
        // each other without needing a second mechanism.
        //
        // System actions are deliberately absent: they belong to dials, which are device-scoped and
        // configured from the dial modal rather than by dragging. Listing them here would offer a
        // drag onto a key or rectangle that cannot do anything with them.
        let mut sections: Vec<(String, Vec<PaletteRow>)> = Vec::new();
        for action in self.predefined.iter().filter(|action| action.category() != SYSTEM_CATEGORY) {
            let category = action.category().to_owned();
            let row = PaletteRow::Predefined {
                action: action.clone(),
                collapsed,
            };
            match sections.iter_mut().find(|(name, _)| *name == category) {
                Some((_, rows)) => rows.push(row),
                None => sections.push((category, vec![row])),
            }
        }

        let mut groups = vec![SidebarGroup::new("Custom actions").children(custom_rows)];
        groups.extend(sections.into_iter().map(|(name, rows)| SidebarGroup::new(SharedString::from(name)).children(rows)));

        Sidebar::left().collapsed(collapsed).children(groups)
    }

    /// Page controls, sat in the bottom-right of the device view.
    fn render_pager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.pages.len().max(1);
        let index = self.pages.iter().position(|page| *page == self.current_page).unwrap_or(0) + 1;
        let only_page = total < 2;

        h_flex()
            .gap_1()
            .items_center()
            .child(
                Button::new("page-prev")
                    .icon(IconName::ChevronLeft)
                    .disabled(only_page)
                    .on_click(cx.listener(|this, _event, _window, cx| this.page_action(PageAction::Step(-1), cx))),
            )
            .child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!("{index} / {total}"))),
            )
            .child(
                Button::new("page-next")
                    .icon(IconName::ChevronRight)
                    .disabled(only_page)
                    .on_click(cx.listener(|this, _event, _window, cx| this.page_action(PageAction::Step(1), cx))),
            )
            .child(
                Button::new("page-add")
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _event, _window, cx| this.page_action(PageAction::Add, cx))),
            )
            .child(
                Button::new("page-remove")
                    .icon(IconName::Delete)
                    .disabled(only_page)
                    .on_click(cx.listener(|this, _event, _window, cx| this.page_action(PageAction::Remove, cx))),
            )
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

        // The physical knobs. These are device-scoped and no longer share a slot with the strip
        // segment above them, so the segment's artwork says nothing about what the dial does - each
        // knob has to caption itself, or a configured dial would be invisible.
        if device.encoders > 0 {
            let encoders = device.encoders;
            let segment_width = (keypad_width - (encoders.saturating_sub(1)) as f32 * GAP) / encoders as f32;

            shell = shell.child(div().flex().flex_row().gap(px(GAP)).mt(px(GAP * 2.0)).children((0..encoders).map(|dial| {
                let configured = self.dials.get(dial as usize).is_some_and(|slot| slot.is_some());
                let label = self.dial_label(dial);
                let device_id = device.id.clone();
                let simulated = is_simulated(&device.id);

                let mut knob = div()
                    .id(("dial", dial as usize))
                    .size(px(DIAL_SIZE))
                    .rounded_full()
                    .bg(if configured { cx.theme().accent } else { cx.theme().secondary })
                    .border_1()
                    .border_color(if configured { cx.theme().primary } else { cx.theme().border })
                    .hover(|style| style.bg(cx.theme().accent))
                    .on_click(cx.listener(move |this, _event, window, cx| this.open_dial_dialog(dial, window, cx)));

                // Only a configured dial has anything to clear, or anything to carry elsewhere.
                // A click and a drag do not conflict: GPUI only starts a drag past its 2px
                // threshold, so a plain click still opens the dialog.
                if configured {
                    knob = knob
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(move |this, _event, _window, cx| {
                                this.dial_menu_open = Some(dial);
                                cx.notify();
                            }),
                        )
                        .on_drag(
                            DraggedDial {
                                dial,
                                label: label.clone(),
                            },
                            |dragged, _cursor_offset, _window, cx| {
                                let label = dragged.label.clone();
                                cx.new(|_| DialPreview { label })
                            },
                        );
                }

                v_flex()
                    .relative()
                    .w(px(segment_width))
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    // The drop lands anywhere in the dial's column rather than only on the knob:
                    // a 44px circle is a mean target, and the column maps to one dial unambiguously.
                    .drag_over::<DraggedDial>(|style, _dragged, _window, cx| style.bg(cx.theme().accent))
                    .on_drop(cx.listener(move |this, dragged: &DraggedDial, _window, cx| {
                        this.swap_dials(dragged.dial, dial, cx);
                    }))
                    .child(knob)
                    .child(
                        div()
                            .text_xs()
                            .text_color(if configured { cx.theme().foreground } else { cx.theme().muted_foreground })
                            .child(label),
                    )
                    // A simulated deck has no knob to turn, so it gets one: these go in through the
                    // same driver entry points the hardware uses, debounce and all.
                    .when(simulated, |column| {
                        let turn = |id: &'static str, glyph: &'static str, ticks: i16| {
                            let device = device_id.clone();
                            div()
                                .id((id, dial as usize))
                                .px_1()
                                .rounded_sm()
                                .text_xs()
                                .bg(rgb(SIMULATED_TINT))
                                .hover(|style| style.opacity(0.8))
                                .child(glyph)
                                .on_click(move |_event, _window, _cx| simulate_rotate(&device, dial, ticks))
                        };

                        let press_device = device_id.clone();
                        column.child(
                            h_flex()
                                .gap_1()
                                .child(turn("dial-left", "◀", -1))
                                .child(
                                    div()
                                        .id(("dial-press", dial as usize))
                                        .px_1()
                                        .rounded_sm()
                                        .text_xs()
                                        .bg(rgb(SIMULATED_TINT))
                                        .hover(|style| style.opacity(0.8))
                                        .child("●")
                                        .on_click(move |_event, _window, _cx| simulate_press(&press_device, dial)),
                                )
                                .child(turn("dial-right", "▶", 1)),
                        )
                    })
                    .when(self.dial_menu_open == Some(dial), |wrapper| {
                        // Same layering as the slot menu: painted last so nothing covers it, and
                        // occluding so the click cannot fall through to the knob beneath.
                        wrapper.child(
                            deferred(
                                v_flex()
                                    .occlude()
                                    .absolute()
                                    .top_full()
                                    .w(px(ROW_MENU_WIDTH))
                                    .p_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(opaque(cx.theme().background))
                                    .shadow_lg()
                                    .child(
                                        div()
                                            .id(("dial-unset", dial as usize))
                                            .w_full()
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .hover(|style| style.bg(cx.theme().accent))
                                            .child("Unset")
                                            .on_click(cx.listener(move |this, _event, _window, cx| this.unset_dial(dial, cx))),
                                    ),
                            )
                            .with_priority(1),
                        )
                    })
            })));
        }

        if device.infobars > 0 {
            let infobar_height = keypad_width * INFOBAR_IMAGE.1 as f32 / INFOBAR_IMAGE.0 as f32;
            shell = shell.child(div().flex().flex_row().gap(px(GAP)).children((0..device.infobars).map(|position| {
                self.render_slot(INFOBAR_CONTROLLER, position, cx).w(px(keypad_width)).h(px(infobar_height)).rounded_md()
            })));
        }

        shell.child(div().mt_4().child(self.render_pager(cx))).into_any_element()
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
            // Click-outside-to-close for the row menu and the device picker. A plain last child
            // covers the window and sits above everything normal, while the menus themselves are
            // `deferred` and so still paint above this.
            .when(
                self.row_menu_open.is_some() || self.device_picker_open || self.slot_menu_open.is_some() || self.dial_menu_open.is_some(),
                |shell| {
                shell.child(
                    div()
                        .id("dismiss-overlay")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.row_menu_open = None;
                            this.slot_menu_open = None;
                            this.dial_menu_open = None;
                            this.device_picker_open = false;
                            cx.notify();
                        })),
                    )
                },
            )
            // `Root::render` only draws the app's own view - dialogs and notifications are
            // separate layers the hosting view is expected to place on top of itself.
            .children(gpui_component::Root::render_dialog_layer(window, cx))
            .children(gpui_component::Root::render_notification_layer(window, cx))
    }
}
