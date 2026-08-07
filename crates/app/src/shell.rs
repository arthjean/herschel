// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The application shell: a fixed navigation rail and one work surface.
//!
//! Four primary destinations, one secondary Settings entry, and nothing else.
//! The rail never scrolls and never changes width, so the work surface has a
//! known width to lay out against at the 920x640 target size.
//!
//! The window holds no hardware handle. It repaints when the worker publishes a
//! new snapshot, and every write control is gated on what the daemon reported.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, Context, Div, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels,
    SharedString, Stateful, Window, actions, div, prelude::*, px,
};
use nzxt_core::capability::CapabilityId;
use nzxt_core::display::{DisplayMode, LcdMetric, MetricSample};
use nzxt_core::ipc::ChannelState;
use nzxt_core::lighting::{EffectDirection, EffectSpeed, LightingCommand};
use nzxt_core::profile::{
    CURVE_NODE_COUNT, Channel, CoolingProgram, CurveNodes, MAX_DUTY, Profile, SAFE_PROFILE_NAME,
};
use nzxt_core::telemetry::{
    Collector, HISTORY_WINDOW_MS, KrakenTelemetry, MetricView, PwmMode, SafetyAlert,
    format_binary_bytes, format_temperature,
};
use nzxt_core::{KRAKEN_BASE, RGB_CONTROLLER};

use crate::components::{
    Button, ButtonVariant, ColorField, ControlState, CurveEditor, DeviceRow, Metric, Note,
    NoteLevel, Panel, Select, SelectOption, Sparkline, node_at,
};
use crate::cooling::{CoolingEditor, CoolingMode};
use crate::display::{DisplayColorField, DisplayEditor, DisplayScreen};
use crate::feed::{Command, CommandOutcome, Feed, OutcomeSeverity, now_unix_ms};
use crate::lighting::{LightingEditor, LightingMode};
use crate::link::LinkState;
use crate::metrics::MetricBook;
use crate::theme::{
    Color, FOCUS_RING, PRODUCT_NAME, RADIUS, RAIL_WIDTH, TARGET_MIN, UNOFFICIAL_NOTICE, color,
    space,
};

actions!(
    shell,
    [
        FocusNext,
        FocusPrevious,
        GoMonitoring,
        GoCooling,
        GoLighting,
        GoLcd,
        GoSettings,
        ClosePopover,
    ]
);

/// Key bindings, registered once at startup.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("ctrl-1", GoMonitoring, None),
        KeyBinding::new("ctrl-2", GoCooling, None),
        KeyBinding::new("ctrl-3", GoLighting, None),
        KeyBinding::new("ctrl-4", GoLcd, None),
        KeyBinding::new("ctrl-comma", GoSettings, None),
        KeyBinding::new("escape", ClosePopover, None),
    ]
}

/// The only destinations this product has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    Monitoring,
    Cooling,
    Lighting,
    Lcd,
    Settings,
}

impl Destination {
    /// Primary destinations, in rail order.
    pub const PRIMARY: [Destination; 4] = [
        Destination::Monitoring,
        Destination::Cooling,
        Destination::Lighting,
        Destination::Lcd,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Monitoring => "Monitoring",
            Self::Cooling => "Cooling",
            Self::Lighting => "Lighting",
            Self::Lcd => "LCD",
            Self::Settings => "Settings",
        }
    }

    /// Glyph shown in the rail, so an entry is not identified by color alone.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Monitoring => "▤",
            Self::Cooling => "❄",
            Self::Lighting => "◈",
            Self::Lcd => "◉",
            Self::Settings => "⚙",
        }
    }

    /// Tab index of this rail entry. Rail entries come before screen controls.
    pub fn tab_index(self) -> isize {
        match self {
            Self::Monitoring => 1,
            Self::Cooling => 2,
            Self::Lighting => 3,
            Self::Lcd => 4,
            Self::Settings => 5,
        }
    }
}

/// First tab index available to a screen's own controls.
pub const SCREEN_TAB_BASE: isize = 10;

/// Tab stops of the Lighting screen, in traversal order.
///
/// Named rather than written as offsets at each call site: the brightness
/// stepper takes two stops rather than one, so a hand-counted offset after it
/// lands on a control that already exists. Every stop is asserted distinct by
/// `every_lighting_control_has_its_own_tab_stop_inside_the_screen_range`.
pub const LIGHTING_TAB_CHANNEL: isize = SCREEN_TAB_BASE;
pub const LIGHTING_TAB_MODE: isize = LIGHTING_TAB_CHANNEL + 1;
pub const LIGHTING_TAB_COLOR: isize = LIGHTING_TAB_MODE + 1;
/// First of the stepper's two buttons; the second is this plus one.
pub const LIGHTING_TAB_BRIGHTNESS: isize = LIGHTING_TAB_COLOR + 1;
pub const LIGHTING_TAB_SPEED: isize = LIGHTING_TAB_BRIGHTNESS + 2;
pub const LIGHTING_TAB_DIRECTION: isize = LIGHTING_TAB_SPEED + 1;
pub const LIGHTING_TAB_APPLY: isize = LIGHTING_TAB_DIRECTION + 1;
pub const LIGHTING_TAB_OFF: isize = LIGHTING_TAB_APPLY + 1;

/// Tab stops of the LCD screen, for the same reason the lighting ones are named.
///
/// The color fields keep a fixed stop per field rather than per rendered
/// control, so a mode that hides two of them does not renumber the rest.
pub const LCD_TAB_MODE: isize = SCREEN_TAB_BASE;
pub const LCD_TAB_METRIC_ONE: isize = LCD_TAB_MODE + 1;
pub const LCD_TAB_METRIC_TWO: isize = LCD_TAB_METRIC_ONE + 1;
pub const LCD_TAB_COLOR_BASE: isize = LCD_TAB_METRIC_TWO + 1;
/// The stepper renders two buttons from one constant, so it occupies two stops.
pub const LCD_TAB_BRIGHTNESS: isize = LCD_TAB_COLOR_BASE + DisplayColorField::ALL.len() as isize;
pub const LCD_TAB_ROTATE: isize = LCD_TAB_BRIGHTNESS + 2;
pub const LCD_TAB_APPLY: isize = LCD_TAB_ROTATE + 1;

/// Width of the preview column, wide enough for the panel plus its padding.
pub const PREVIEW_COLUMN_WIDTH: Pixels = px(276.0);

/// Modes the screen can configure completely.
///
/// [`DisplayMode::Image`] is deliberately absent. The screen has no control
/// that can name a file, and a mode whose Apply could only ever refuse would be
/// a control that says the feature is here and then does nothing. The renderer
/// and the daemon support it, and a saved profile can select it; what is
/// missing is a way to pick the file, which no criterion in US-017 asks the
/// screen for.
pub const SCREEN_MODES: [DisplayMode; 2] = [DisplayMode::DualInfographic, DisplayMode::Solid];

/// Which popover, if any, is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popover {
    /// A color swatch list anchored to one LCD color field.
    Swatches { field: DisplayColorField },
    /// A color swatch list anchored to one lighting channel.
    LightingSwatches { channel: u8 },
    /// An option list anchored to one select.
    Options { select: SharedString },
}

/// Swatches offered by a color popover.
pub const SWATCHES: [Color; 6] = [
    Color::rgb(0x6f4ef2),
    Color::rgb(0x30c8a0),
    Color::rgb(0xf5c451),
    Color::rgb(0xff8a8a),
    Color::rgb(0xe8eaee),
    Color::rgb(0x14161a),
];

/// The root view.
pub struct Shell {
    focus: FocusHandle,
    feed: Feed,
    link: LinkState,
    metrics: MetricBook,
    cooling: CoolingEditor,
    /// Plot area of the curve editor, published during paint for hit testing.
    curve_bounds: Rc<Cell<Bounds<Pixels>>>,
    curve_dragging: bool,
    /// The program the last Apply sent, held until its outcome arrives.
    sent: Option<CoolingProgram>,
    outcome: Option<CommandOutcome>,
    /// Set once the operator has armed the profile deletion.
    ///
    /// Deleting a profile is the one destructive action this screen offers, so
    /// it takes two deliberate activations rather than one stray click.
    confirm_delete: bool,
    destination: Destination,
    popover: Option<Popover>,
    /// The LCD editor and the last preset that rendered, held together so an
    /// unfinished field keeps the previous picture on screen instead of
    /// blanking it, and so no control can move one without the other.
    lcd: DisplayScreen,
    lighting: LightingEditor,
    /// Wall clock of the last refresh, used to age every reading.
    now_unix_ms: u64,
}

impl Shell {
    pub fn new(
        feed: Feed,
        notifications: futures::channel::mpsc::UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);

        // The view repaints when the worker publishes, not on a timer of its
        // own: one repaint per sample, and no lag between the two.
        cx.spawn(async move |shell, cx| {
            let mut notifications = notifications;
            use futures::StreamExt;
            while notifications.next().await.is_some() {
                if shell.update(cx, |shell, cx| shell.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus,
            feed,
            link: LinkState::connecting(),
            metrics: MetricBook::new(),
            cooling: CoolingEditor::new(),
            curve_bounds: Rc::new(Cell::new(Bounds::default())),
            curve_dragging: false,
            sent: None,
            outcome: None,
            confirm_delete: false,
            destination: Destination::Monitoring,
            popover: None,
            lcd: DisplayScreen::default(),
            lighting: LightingEditor::default(),
            now_unix_ms: now_unix_ms(),
        }
    }

    /// Take whatever the worker published and repaint.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.now_unix_ms = now_unix_ms();

        if let Some(link) = self.feed.link() {
            if let Some(snapshot) = link.telemetry() {
                self.metrics.observe(snapshot);
            }
            self.link = link;
            // The controller's channel list is the only source of what can be
            // addressed, so the editor follows it rather than assuming three.
            let channels: Vec<u8> = self
                .link
                .lighting_channels()
                .iter()
                .map(|state| state.channel)
                .collect();
            self.lighting.sync(&channels);
        }

        if let Some(outcome) = self.feed.outcome()
            && self.outcome.as_ref() != Some(&outcome)
        {
            // A confirmed Apply is what turns a pending curve into the
            // client's record of what the hardware is running. Curve points
            // cannot be read back, so this record is the only evidence there
            // is, and it is only ever set from a confirmation.
            if outcome.severity == OutcomeSeverity::Confirmed
                && let Some(program) = self.sent.take()
            {
                self.cooling.record_applied(program);
            } else if outcome.severity != OutcomeSeverity::Confirmed {
                self.sent = None;
            }
            self.outcome = Some(outcome);
        }

        cx.notify();
    }

    fn kraken(&self) -> Option<&KrakenTelemetry> {
        self.link.telemetry().map(|snapshot| &snapshot.kraken)
    }

    fn go(&mut self, destination: Destination, cx: &mut Context<Self>) {
        self.destination = destination;
        self.popover = None;
        // An armed deletion does not survive leaving the screen: coming back to
        // a button that already says "Confirm" would delete on one press.
        self.confirm_delete = false;
        cx.notify();
    }

    fn toggle_popover(&mut self, popover: Popover, cx: &mut Context<Self>) {
        self.popover = if self.popover.as_ref() == Some(&popover) {
            None
        } else {
            Some(popover)
        };
        cx.notify();
    }

    fn on_focus_next(&mut self, _: &FocusNext, window: &mut Window, _: &mut Context<Self>) {
        window.focus_next();
    }

    fn on_focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, _: &mut Context<Self>) {
        window.focus_prev();
    }

    fn on_close_popover(&mut self, _: &ClosePopover, _: &mut Window, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            cx.notify();
        }
    }

    fn rail(&self, cx: &mut Context<Self>) -> Div {
        let current = self.destination;
        // A `map` closure would have to hold the mutable context borrow across
        // calls, which the 2024 edition's capture rules reject. A plain loop
        // borrows it once per entry and releases it.
        let mut primary_entries = Vec::with_capacity(Destination::PRIMARY.len());
        for destination in Destination::PRIMARY {
            primary_entries.push(self.rail_entry(destination, current, cx));
        }

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(RAIL_WIDTH)
            .h_full()
            .bg(color::RAIL.hsla())
            .border_r_1()
            .border_color(color::SEPARATOR.hsla())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .p(space::LG)
                    .child(
                        div()
                            .text_color(color::TEXT.hsla())
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(PRODUCT_NAME),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child("Unofficial"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .px(space::SM)
                    .children(primary_entries),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(space::SM)
                    .pb(space::LG)
                    .border_t_1()
                    .border_color(color::SEPARATOR.hsla())
                    .pt(space::SM)
                    .child(self.rail_entry(Destination::Settings, current, cx)),
            )
    }

    /// Returns a concrete element rather than `impl IntoElement`.
    ///
    /// Under the 2024 edition an opaque return type captures every input
    /// lifetime, so an `impl IntoElement` here would keep borrowing the context
    /// and only one entry could be built at a time.
    fn rail_entry(
        &self,
        destination: Destination,
        current: Destination,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = destination == current;

        div()
            .id(SharedString::from(destination.label()))
            .tab_index(destination.tab_index())
            .tab_stop(true)
            .flex()
            .items_center()
            .gap(space::SM)
            .w_full()
            .min_h(TARGET_MIN)
            .px(space::MD)
            .rounded(RADIUS)
            .cursor_pointer()
            .text_color(if selected {
                color::TEXT_ON_ACCENT.hsla()
            } else {
                color::TEXT_MUTED.hsla()
            })
            .when(selected, |this| this.bg(color::ACCENT.hsla()))
            .hover(|this| {
                this.bg(if selected {
                    color::ACCENT_HOVER.hsla()
                } else {
                    color::CONTROL.alpha(0.6)
                })
            })
            .focus(|this| this.border(FOCUS_RING).border_color(color::FOCUS.hsla()))
            .child(destination.glyph())
            .child(destination.label())
            .on_click(cx.listener(move |this, _, _, cx| this.go(destination, cx)))
    }

    fn banner(&self) -> Option<Div> {
        let message = self.link.banner()?;
        Some(Note::new(NoteLevel::Warning, message).render())
    }

    fn device_panel(&self) -> Div {
        let rows = self.link.device_rows();
        let panel = Panel::new("Devices")
            .subtitle("Only the two allowlisted devices are ever opened.")
            .render();

        if rows.is_empty() {
            return panel.child(
                div()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child("No supported NZXT device detected."),
            );
        }

        panel.children(rows.into_iter().map(|summary| {
            DeviceRow::new(summary.name.clone(), summary.id.to_string(), summary.health)
                .detail(summary.detail())
                .render()
        }))
    }

    fn monitoring(&self) -> Div {
        let now = self.now_unix_ms;
        let book = &self.metrics;

        let percent = |value: f32| format!("{value:.0}");
        let temperature = format_temperature;
        let rpm = |value: f32| format!("{value:.0}");

        let memory_detail = match book.memory.view(now).copied() {
            Some(usage) => format!(
                "{} of {} in use",
                format_binary_bytes(usage.used_bytes),
                format_binary_bytes(usage.total_bytes)
            ),
            None => "Installed memory is not readable.".to_string(),
        };

        let gpu_subtitle = match book.gpu_name.view(now).value() {
            Some(name) => format!("{name}, read through {}", self.gpu_source()),
            None => self
                .link
                .collector_failure(Collector::Gpu)
                .map(|failure| failure.detail.clone())
                .unwrap_or_else(|| {
                    "No GPU management interface answered on this machine.".to_string()
                }),
        };

        screen("Monitoring", "System and cooling state at a glance.")
            .child(self.device_panel())
            .child(
                Panel::new("CPU")
                    .subtitle(self.collector_note(Collector::Cpu, "Load and package temperature."))
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Load", &book.cpu_load.view(now), percent)
                                    .unit("%")
                                    .bar(book.cpu_load.view(now).copied().map(|v| v / 100.0))
                                    .render(),
                            )
                            .child(
                                Metric::from_view(
                                    "Temperature",
                                    &book.cpu_temperature.view(now),
                                    temperature,
                                )
                                .unit(" C")
                                .bar(
                                    book.cpu_temperature
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                            ),
                    )
                    .child(Sparkline::new(book.cpu_load.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("GPU")
                    .subtitle(gpu_subtitle)
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Load", &book.gpu_load.view(now), percent)
                                    .unit("%")
                                    .bar(book.gpu_load.view(now).copied().map(|v| v / 100.0))
                                    .render(),
                            )
                            .child(
                                Metric::from_view(
                                    "Temperature",
                                    &book.gpu_temperature.view(now),
                                    temperature,
                                )
                                .unit(" C")
                                .bar(
                                    book.gpu_temperature
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                            ),
                    )
                    .child(Sparkline::new(book.gpu_load.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("Memory")
                    .subtitle(memory_detail)
                    .render()
                    .child(
                        metric_row().child(
                            Metric::from_view("In use", &book.memory_percent.view(now), percent)
                                .unit("%")
                                .bar(
                                    book.memory_percent
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                        ),
                    )
                    .child(Sparkline::new(book.memory_percent.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("Kraken")
                    .subtitle(self.collector_note(
                        Collector::Kraken,
                        "Coolant temperature and both tachometers, through kraken2023.",
                    ))
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Liquid", &book.liquid.view(now), temperature)
                                    .unit(" C")
                                    .bar(
                                        book.liquid
                                            .view(now)
                                            .copied()
                                            .map(|value| (value - 20.0) / 40.0),
                                    )
                                    .render(),
                            )
                            .child(
                                Metric::from_view("Pump", &book.pump.rpm.view(now), rpm)
                                    .unit(" RPM")
                                    .bar(
                                        book.pump
                                            .rpm
                                            .view(now)
                                            .copied()
                                            .map(|value| value / 3000.0),
                                    )
                                    .render(),
                            )
                            .child(
                                Metric::from_view("Fan", &book.fan.rpm.view(now), rpm)
                                    .unit(" RPM")
                                    .bar(
                                        book.fan.rpm.view(now).copied().map(|value| value / 2000.0),
                                    )
                                    .render(),
                            ),
                    )
                    .child(Sparkline::new(book.liquid.history(), 20.0, 60.0).render())
                    .child(
                        div()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(format!(
                                "Rolling {} minute window, held in memory only.",
                                HISTORY_WINDOW_MS / 60_000
                            )),
                    ),
            )
    }

    /// The interface NVML or its absence is reported under.
    fn gpu_source(&self) -> String {
        self.link
            .telemetry()
            .map(|snapshot| snapshot.gpu.source.clone())
            .unwrap_or_else(|| "no interface".to_string())
    }

    /// A section subtitle that names a failed collector instead of its usual
    /// description.
    fn collector_note(&self, collector: Collector, usual: &'static str) -> String {
        match self.link.collector_failure(collector) {
            Some(failure) => failure.detail.clone(),
            None => usual.to_string(),
        }
    }

    fn cooling(&self, cx: &mut Context<Self>) -> Div {
        let now = self.now_unix_ms;
        let kraken = self.kraken();
        // Each channel is gated on its own capability. The probe resolves
        // `pwm1` and `pwm2` independently, so a udev rule covering one channel
        // leaves the other read-only, and a control offered on that channel
        // would be one the hardware never accepts.
        let pump_write = self
            .link
            .cooling_state(KRAKEN_BASE, Channel::Pump.duty_capability(), now);
        let fan_write = self
            .link
            .cooling_state(KRAKEN_BASE, Channel::Fan.duty_capability(), now);
        let curve_write =
            self.link
                .cooling_state(KRAKEN_BASE, self.cooling.channel.curve_capability(), now);
        let pending = self.cooling.pending(kraken);
        let invalid = self.cooling.validation_error();

        // Apply writes every channel the program names, so it is refused unless
        // all of them are writable. The capability list is the daemon's own, so
        // an enabled Apply is one the daemon would accept.
        let apply_state = match &invalid {
            Some(error) => ControlState::error(error.to_string()),
            None => self.link.program_state(
                KRAKEN_BASE,
                &self.cooling.program().required_capabilities(),
                now,
            ),
        };

        let mode_options: Vec<SelectOption> = CoolingMode::ALL
            .into_iter()
            .map(|mode| SelectOption::new(mode.value(), mode.label()))
            .collect();
        let profiles: Vec<SelectOption> = self
            .link
            .profiles()
            .iter()
            .map(|profile| SelectOption::new(profile.name.clone(), profile.name.clone()))
            .collect();
        let active = self
            .link
            .active_profile()
            .unwrap_or(SAFE_PROFILE_NAME)
            .to_string();

        let mut surface = screen(
            "Cooling",
            "Pump, fan and the onboard liquid-temperature curve.",
        );

        for alert in self.link.alerts() {
            surface = surface.child(Note::new(NoteLevel::Critical, alert_message(alert)).render());
        }

        surface
            .child(
                Panel::new("Channels")
                    .subtitle(match self.link.kraken_age_ms(now) {
                        Some(age) => format!(
                            "Temperature source: liquid, as the kernel curve requires. \
                             Readback {:.1} s old.",
                            age as f32 / 1000.0
                        ),
                        None => "No readback has arrived from the device yet.".to_string(),
                    })
                    .render()
                    .child(self.channel_row(Channel::Pump, SCREEN_TAB_BASE, &pump_write, cx))
                    .child(self.channel_row(Channel::Fan, SCREEN_TAB_BASE + 3, &fan_write, cx)),
            )
            .child(
                Panel::new("Program")
                    .subtitle(if pending {
                        "Pending. The hardware still runs its previous program until Apply \
                         succeeds."
                    } else {
                        "The selection below matches what the hardware reports."
                    })
                    .render()
                    .child(self.select(
                        "cooling-mode",
                        "Mode",
                        mode_options,
                        self.cooling.mode.value().to_string(),
                        // Choosing a mode writes nothing: it is Apply that does,
                        // and Apply carries the per-capability gate. This select
                        // needs the same gate the profile selector below it
                        // needs, and for the same reason.
                        self.link.write_state(),
                        SCREEN_TAB_BASE + 6,
                        cx,
                        |shell, value, _| {
                            if let Some(mode) = CoolingMode::from_value(value) {
                                shell.cooling.set_mode(mode);
                            }
                        },
                    ))
                    .child(self.select(
                        "profile",
                        "Active profile",
                        profiles,
                        active,
                        // Activating a profile is a write. It is disabled for
                        // the same reasons every other write control is.
                        self.link.write_state(),
                        SCREEN_TAB_BASE + 7,
                        cx,
                        |shell, value, _| {
                            shell.feed.send(Command::ActivateProfile(value.to_string()));
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(space::SM)
                            .child(
                                Button::new(
                                    "cooling-apply",
                                    if pending { "Apply" } else { "Applied" },
                                )
                                .variant(ButtonVariant::Primary)
                                .state(apply_state)
                                .tab_index(SCREEN_TAB_BASE + 8)
                                .render()
                                .on_click(cx.listener(|shell, _, _, cx| shell.apply(cx))),
                            )
                            .child(
                                Button::new("cooling-cancel", "Cancel")
                                    .tab_index(SCREEN_TAB_BASE + 9)
                                    .render()
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        let kraken = shell.kraken().cloned();
                                        shell.cooling.cancel(kraken.as_ref());
                                        shell.sent = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("cooling-save", "Save as profile")
                                    .state(self.link.write_state())
                                    .tab_index(SCREEN_TAB_BASE + 10)
                                    .render()
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| shell.save_profile(cx)),
                                    ),
                            )
                            .child(self.delete_button(SCREEN_TAB_BASE + 11, cx)),
                    )
                    .children(self.outcome_note()),
            )
            .child(self.curve_panel(curve_write, cx))
    }

    /// Delete the active profile, in two deliberate activations.
    ///
    /// The daemon activates the built-in safe profile before it removes the
    /// file, so the window between the two states is one where the hardware is
    /// on the program that writes nothing rather than on none at all.
    fn delete_button(&self, tab_index: isize, cx: &mut Context<Self>) -> Stateful<Div> {
        Button::new(
            "cooling-delete",
            if self.confirm_delete {
                "Confirm deletion"
            } else {
                "Delete profile"
            },
        )
        .variant(ButtonVariant::Danger)
        .state(delete_state(self.link.active_profile()))
        .tab_index(tab_index)
        .render()
        .on_click(cx.listener(|shell, _, _, cx| shell.delete_profile(cx)))
    }

    /// Arm the deletion, then perform it on the second activation.
    fn delete_profile(&mut self, cx: &mut Context<Self>) {
        let (armed, command) = next_deletion(self.link.active_profile(), self.confirm_delete);
        self.confirm_delete = armed;
        if let Some(command) = command {
            self.feed.send(command);
        }
        cx.notify();
    }

    /// One channel row: readback, mode, and the fixed-duty control.
    fn channel_row(
        &self,
        channel: Channel,
        tab_index: isize,
        write: &ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let now = self.now_unix_ms;
        let metrics = self.metrics.channel(channel);
        let rpm = metrics.rpm.view(now);
        let duty = metrics.duty.view(now);
        let mode = metrics.mode.view(now);
        let target = self.cooling.duty(channel);
        let confirmed_percent = self
            .kraken()
            .and_then(|kraken| kraken.channel(channel).duty_percent());
        let alerts: Vec<SafetyAlert> = self
            .link
            .channel_alerts(channel)
            .into_iter()
            .cloned()
            .collect();
        let adjustable = self.cooling.mode == CoolingMode::Fixed && write.is_enabled();
        let step_state = if adjustable {
            ControlState::Enabled
        } else {
            write.clone()
        };

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::SM)
            .py(space::SM)
            .border_b_1()
            .border_color(color::SEPARATOR.hsla())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex_none()
                            .text_color(color::TEXT.hsla())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(channel.label()),
                    )
                    .child(readback("RPM", &rpm, |value| format!("{value:.0}")))
                    .child(readback("PWM", &duty_view(&duty), move |value| {
                        // The percentage comes from the reported duty, not from
                        // the pending edit: this row is readback, not intent.
                        match confirmed_percent {
                            Some(percent) => format!("{value:.0} ({percent:.0}%)"),
                            None => format!("{value:.0}"),
                        }
                    }))
                    .child(mode_readback(&mode))
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child("Source: liquid"),
                    )
                    .children(alerts.into_iter().map(|alert| {
                        div()
                            .flex_none()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(color::DANGER.hsla())
                            .child(match alert {
                                SafetyAlert::ChannelStalled { .. } => "Critical: not turning",
                                SafetyAlert::LiquidCritical { .. } => "Critical: coolant",
                            })
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child("Fixed duty"),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("{}-duty-down", channel.label())),
                            "−",
                        )
                        .state(step_state.clone())
                        .tab_index(tab_index)
                        .render()
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.cooling.adjust_duty(channel, -1);
                            cx.notify();
                        })),
                    )
                    .child(
                        // Wide enough for the longest reading this can hold,
                        // `255/255 (100%)`, and `flex_none` so it is never
                        // squeezed into wrapping mid-value.
                        div()
                            .font(numeric())
                            .flex_none()
                            .min_w(px(140.0))
                            .text_align(gpui::TextAlign::Center)
                            .text_color(color::TEXT.hsla())
                            .child(format!(
                                "{target}/{MAX_DUTY} ({:.0}%)",
                                target as f32 / MAX_DUTY as f32 * 100.0
                            )),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("{}-duty-up", channel.label())),
                            "+",
                        )
                        .state(step_state)
                        .tab_index(tab_index + 1)
                        .render()
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.cooling.adjust_duty(channel, 1);
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(format!("Accepted range {}-{MAX_DUTY}", channel.min_duty())),
                    ),
            )
    }

    /// The curve editor and everything that drives it by keyboard or pointer.
    fn curve_panel(&self, state: ControlState, cx: &mut Context<Self>) -> Div {
        let channel = self.cooling.channel;
        let nodes = *self.cooling.curve(channel);
        let node = self.cooling.node;
        let editable = state.is_enabled();

        let channel_options = vec![
            SelectOption::new("pump", Channel::Pump.label()),
            SelectOption::new("fan", Channel::Fan.label()),
        ];

        Panel::new("Liquid temperature curve")
            .subtitle(
                "Ten nodes over 20-59 C, interpolated to the 40 values the kernel accepts. \
                 Editing changes nothing until Apply is activated.",
            )
            .render()
            .child(self.select(
                "curve-channel",
                "Channel",
                channel_options,
                match channel {
                    Channel::Pump => "pump".to_string(),
                    Channel::Fan => "fan".to_string(),
                },
                ControlState::Enabled,
                SCREEN_TAB_BASE + 12,
                cx,
                |shell, value, _| {
                    shell.cooling.channel = match value {
                        "fan" => Channel::Fan,
                        _ => Channel::Pump,
                    };
                },
            ))
            .child(
                CurveEditor::new(nodes)
                    .selected(node)
                    .state(state.clone())
                    .tab_index(SCREEN_TAB_BASE + 13)
                    .bounds_sink(Rc::clone(&self.curve_bounds))
                    .render()
                    .when(editable, |plot| {
                        plot.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |shell, event: &gpui::MouseDownEvent, _, cx| {
                                shell.curve_dragging = true;
                                shell.edit_node_at(event.position, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |shell, event: &gpui::MouseMoveEvent, _, cx| {
                                if shell.curve_dragging {
                                    shell.edit_node_at(event.position, cx);
                                }
                            },
                        ))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |shell, _: &gpui::MouseUpEvent, _, _| {
                                shell.curve_dragging = false;
                            }),
                        )
                        .on_key_down(cx.listener(
                            move |shell, event: &gpui::KeyDownEvent, _, cx| {
                                let handled = match event.keystroke.key.as_str() {
                                    "left" => {
                                        shell.cooling.step_node(-1);
                                        true
                                    }
                                    "right" => {
                                        shell.cooling.step_node(1);
                                        true
                                    }
                                    "up" => {
                                        shell.cooling.adjust_node(1);
                                        true
                                    }
                                    "down" => {
                                        shell.cooling.adjust_node(-1);
                                        true
                                    }
                                    _ => false,
                                };
                                if handled {
                                    cx.notify();
                                }
                            },
                        ))
                    })
                    .into_any_element(),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space::SM)
                    .child(
                        div()
                            .flex_none()
                            .font(numeric())
                            .text_color(color::TEXT.hsla())
                            .child(format!(
                                "Node {}/{} at {:.0} C: {}/{MAX_DUTY} ({:.0}%)",
                                node + 1,
                                CURVE_NODE_COUNT,
                                CurveNodes::temperature_at(node),
                                nodes.duty[node],
                                nodes.duty[node] as f32 / MAX_DUTY as f32 * 100.0
                            )),
                    )
                    .child(
                        Button::new("curve-node-previous", "Previous node")
                            .state(state.clone())
                            .tab_index(SCREEN_TAB_BASE + 14)
                            .render()
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.cooling.step_node(-1);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("curve-node-next", "Next node")
                            .state(state.clone())
                            .tab_index(SCREEN_TAB_BASE + 15)
                            .render()
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.cooling.step_node(1);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("curve-duty-down", "−")
                            .state(state.clone())
                            .tab_index(SCREEN_TAB_BASE + 16)
                            .render()
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.cooling.adjust_node(-1);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("curve-duty-up", "+")
                            .state(state)
                            .tab_index(SCREEN_TAB_BASE + 17)
                            .render()
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.cooling.adjust_node(1);
                                cx.notify();
                            })),
                    ),
            )
            .child(div().text_sm().text_color(color::TEXT_MUTED.hsla()).child(
                "Arrow keys move the selection and the duty once the plot has focus. \
                         The firmware runs both channels at 100% at or above 60 C, and this \
                         application never overrides it.",
            ))
    }

    /// Move the selected node to a pointer position.
    fn edit_node_at(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.curve_bounds.get();
        if bounds.size.width <= px(0.0) {
            return;
        }
        let (index, duty) = node_at(bounds, position);
        self.cooling.select_node(index);
        self.cooling.set_node(index, duty);
        cx.notify();
    }

    /// Send the pending program, remembering it until its outcome arrives.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let program = self.cooling.program();
        self.sent = Some(program.clone());
        self.feed.send(Command::Apply(program));
        cx.notify();
    }

    /// Store the pending program under a generated, valid name.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let existing = self.link.profiles().len();
        let name = format!("{} {}", self.cooling.mode.label(), existing);
        let profile = Profile {
            name: name.chars().take(48).collect(),
            program: self.cooling.program(),
            device: Some(KRAKEN_BASE),
            lighting: Vec::new(),
            display: None,
        };
        self.feed.send(Command::SaveProfile(profile));
        cx.notify();
    }

    /// The result of the last command, as a note under the buttons.
    fn outcome_note(&self) -> Option<Div> {
        let outcome = self.outcome.as_ref()?;
        let level = match outcome.severity {
            OutcomeSeverity::Confirmed => NoteLevel::Info,
            OutcomeSeverity::Unconfirmed => NoteLevel::Critical,
            OutcomeSeverity::Refused => NoteLevel::Warning,
        };
        Some(Note::new(level, outcome.message.clone()).render())
    }

    fn lighting(&self, cx: &mut Context<Self>) -> Div {
        let fixed = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        let effects = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbEffects);
        let channels = self.link.lighting_channels();

        let screen = screen("Lighting", "Per-channel color on the validated controller.");

        // No channel means the controller has not told this daemon what it is.
        // The reason the capability record carries is the whole content of the
        // screen: there is nothing to control and nothing to pretend about.
        let Some(editor) = self.lighting.selected().filter(|_| !channels.is_empty()) else {
            return screen.child(
                Note::new(
                    NoteLevel::Warning,
                    fixed
                        .message()
                        .unwrap_or(
                            "No lighting controller answered. Lighting is read-only until it does.",
                        )
                        .to_string(),
                )
                .render(),
            );
        };

        let reported = channels
            .iter()
            .find(|state| state.channel == editor.channel);
        let write = if matches!(editor.mode, LightingMode::Effect(_)) {
            effects.clone()
        } else {
            fixed.clone()
        };
        // Apply is gated on the pending program being buildable as well as on
        // the capability: an unusable color must disable it with its own
        // reason, not with the capability's.
        let apply = match (write.clone(), editor.program()) {
            (ControlState::Enabled, Err(error)) => ControlState::error(error.to_string()),
            (state, _) => state,
        };

        screen
            .child(
                Panel::new("Channels")
                    .subtitle("Each channel is addressed on its own.")
                    .render()
                    .child(
                        self.select(
                            "lighting-channel",
                            "Channel",
                            channels
                                .iter()
                                .map(|state| {
                                    SelectOption::new(
                                        state.channel.to_string(),
                                        channel_label(state),
                                    )
                                })
                                .collect(),
                            editor.channel.to_string(),
                            ControlState::Enabled,
                            LIGHTING_TAB_CHANNEL,
                            cx,
                            |shell, value, _| {
                                if let Ok(channel) = value.parse() {
                                    shell.lighting.select(channel);
                                }
                            },
                        ),
                    )
                    .child(setting_row_owned(
                        "Detected".to_string(),
                        reported
                            .map(accessory_summary)
                            .unwrap_or_else(|| "not reported".to_string()),
                    ))
                    .child(setting_row_owned(
                        "Confirmed".to_string(),
                        match self.link.committed_lighting(editor.channel) {
                            Some(program) => program.summary(),
                            None => "nothing has been sent to this channel".to_string(),
                        },
                    )),
            )
            .child(
                Panel::new(format!("Channel {}", editor.channel))
                    .subtitle(
                        "The controller reports no state to read back, so what is shown here is \
                         what this service last sent.",
                    )
                    .render()
                    .child(
                        self.select(
                            "lighting-mode",
                            "Mode",
                            LightingMode::all(effects.is_enabled())
                                .into_iter()
                                .map(|mode| SelectOption::new(mode.value(), mode.label()))
                                .collect(),
                            editor.mode.value(),
                            write.clone(),
                            LIGHTING_TAB_MODE,
                            cx,
                            |shell, value, _| {
                                if let Some(mode) = LightingMode::from_value(value)
                                    && let Some(channel) = shell.lighting.selected_mut()
                                {
                                    channel.mode = mode;
                                }
                            },
                        ),
                    )
                    .when(editor.mode.uses_color(), |panel| {
                        panel.child(self.lighting_color_field(editor, LIGHTING_TAB_COLOR, cx))
                    })
                    .when(editor.mode.uses_brightness(), |panel| {
                        panel.child(self.brightness_row(
                            editor.brightness,
                            write.clone(),
                            LIGHTING_TAB_BRIGHTNESS,
                            cx,
                        ))
                    })
                    .when(editor.mode.uses_speed(), |panel| {
                        panel.child(
                            self.select(
                                "lighting-speed",
                                "Speed",
                                EffectSpeed::ALL
                                    .into_iter()
                                    .map(|speed| SelectOption::new(speed.key(), speed.label()))
                                    .collect(),
                                editor.speed.key().to_string(),
                                write.clone(),
                                LIGHTING_TAB_SPEED,
                                cx,
                                |shell, value, _| {
                                    if let Some(speed) = EffectSpeed::from_key(value)
                                        && let Some(channel) = shell.lighting.selected_mut()
                                    {
                                        channel.speed = speed;
                                    }
                                },
                            ),
                        )
                    })
                    .when(editor.mode.uses_direction(), |panel| {
                        panel.child(
                            self.select(
                                "lighting-direction",
                                "Direction",
                                EffectDirection::ALL
                                    .into_iter()
                                    .map(|direction| {
                                        SelectOption::new(direction.key(), direction.label())
                                    })
                                    .collect(),
                                editor.direction.key().to_string(),
                                write.clone(),
                                LIGHTING_TAB_DIRECTION,
                                cx,
                                |shell, value, _| {
                                    if let Some(direction) = EffectDirection::from_key(value)
                                        && let Some(channel) = shell.lighting.selected_mut()
                                    {
                                        channel.direction = direction;
                                    }
                                },
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap(space::SM)
                            .child(
                                Button::new("lighting-apply", "Apply")
                                    .variant(ButtonVariant::Primary)
                                    .state(apply)
                                    .tab_index(LIGHTING_TAB_APPLY)
                                    .render()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.popover = None;
                                        this.apply_lighting(cx);
                                    })),
                            )
                            .child(
                                Button::new("lighting-off", "Turn off")
                                    .state(fixed.clone())
                                    .tab_index(LIGHTING_TAB_OFF)
                                    .render()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.popover = None;
                                        // The mode changes, so the color the
                                        // operator chose is still there to come
                                        // back to.
                                        if let Some(channel) = this.lighting.selected_mut() {
                                            channel.mode = LightingMode::Off;
                                        }
                                        this.apply_lighting(cx);
                                    })),
                            ),
                    )
                    .children(self.outcome_note()),
            )
    }

    fn lcd(&self, cx: &mut Context<Self>) -> Div {
        let frame = self.link.control_state(KRAKEN_BASE, CapabilityId::LcdFrame);
        let editor = self.lcd.editor().clone();
        let panel = self
            .link
            .status()
            .and_then(|status| status.display.panel.clone());
        let confirmed = panel.is_some();
        let panel = panel.unwrap_or_else(crate::preview::assumed_panel);

        // The preview ages with telemetry exactly as the panel does, from the
        // same samples the daemon renders against.
        let samples = match self.link.telemetry() {
            Some(snapshot) => self.lcd.preview().samples(snapshot),
            None => [
                MetricSample::unavailable(editor.metrics[0]),
                MetricSample::unavailable(editor.metrics[1]),
            ],
        };

        // Apply is refused for two separate reasons, and they are reported
        // separately: a preset that cannot be built names its own field, and a
        // panel that may not be written names the missing evidence.
        let pending = editor.preset();
        let apply = match (&pending, &frame) {
            (Err(error), _) => ControlState::Disabled {
                reason: error.to_string(),
            },
            _ => frame.clone(),
        };

        let committed = self
            .link
            .status()
            .and_then(|status| status.display.committed.clone());

        screen("LCD", "Layout and colors for the Kraken display.").child(
            div()
                .flex()
                .gap(space::LG)
                .child(
                    Panel::new("Editor")
                        .render()
                        .flex_1()
                        .child(
                            self.select(
                                "lcd-mode",
                                "Display mode",
                                SCREEN_MODES
                                    .into_iter()
                                    .map(|mode| SelectOption::new(mode.key(), mode.label()))
                                    .collect(),
                                editor.mode.key().to_string(),
                                ControlState::Enabled,
                                LCD_TAB_MODE,
                                cx,
                                |shell, value, _| {
                                    if let Some(mode) = DisplayMode::from_key(value) {
                                        shell.lcd.edit(|editor| editor.mode = mode);
                                    }
                                },
                            ),
                        )
                        // One metric select per reading slot. The infographic
                        // draws two, so it configures two.
                        .when(editor.mode.uses_readings(), |this| {
                            this.child(self.metric_select(0, LCD_TAB_METRIC_ONE, cx))
                                .child(self.metric_select(1, LCD_TAB_METRIC_TWO, cx))
                        })
                        // A color the current mode never draws is absent, not
                        // disabled: a control that cannot change the picture
                        // still says the picture has that part.
                        .children(
                            DisplayColorField::ALL
                                .into_iter()
                                .enumerate()
                                .filter(|(_, field)| field.is_used_by(editor.mode))
                                .map(|(index, field)| {
                                    self.color_field(field, LCD_TAB_COLOR_BASE + index as isize, cx)
                                }),
                        )
                        .child(self.lcd_brightness(&editor, cx))
                        .child(
                            div()
                                .flex()
                                .gap(space::SM)
                                .child(
                                    Button::new(
                                        "lcd-rotate",
                                        format!("Rotate display ({})", editor.orientation.label()),
                                    )
                                    .tab_index(LCD_TAB_ROTATE)
                                    .render()
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            // Acting on the editor dismisses any
                                            // open popover, so a swatch list never
                                            // hides the result.
                                            this.popover = None;
                                            this.lcd.edit(DisplayEditor::rotate);
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    Button::new("lcd-apply", "Apply")
                                        .variant(ButtonVariant::Primary)
                                        .state(apply)
                                        .tab_index(LCD_TAB_APPLY)
                                        .render()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.popover = None;
                                            if let Ok(preset) = this.lcd.preset() {
                                                this.feed.send(Command::ApplyDisplay(preset));
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .children(committed.map(|preset| {
                            div()
                                .text_xs()
                                .text_color(color::TEXT_MUTED.hsla())
                                .child(format!(
                                    "Confirmed on the panel: {} at {}, {}% brightness.",
                                    preset.mode.label(),
                                    preset.orientation.label(),
                                    preset.brightness.percent()
                                ))
                        }))
                        .children(frame.message().map(|reason| {
                            div()
                                .text_xs()
                                .text_color(color::WARNING.hsla())
                                .child(reason.to_string())
                        })),
                )
                .child(
                    Panel::new("Preview")
                        .subtitle(format!("Rotation {}", editor.orientation.label()))
                        .render()
                        // Fixed rather than sized to content: the caption under
                        // the preview is a sentence, and letting it set the
                        // column width squeezed the editor until its buttons
                        // were clipped at 920 logical pixels.
                        .flex_none()
                        .w(PREVIEW_COLUMN_WIDTH)
                        .child(crate::preview::panel_preview(
                            self.lcd.preview(),
                            &samples,
                            &panel,
                            confirmed,
                        )),
                ),
        )
    }

    /// The metric select for one reading slot.
    fn metric_select(&self, slot: usize, tab_index: isize, cx: &mut Context<Self>) -> Div {
        let selected = self.lcd.editor().metrics[slot];
        let (id, label) = if slot == 0 {
            ("lcd-metric-1", "Reading 1 metric")
        } else {
            ("lcd-metric-2", "Reading 2 metric")
        };
        self.select(
            id,
            label,
            LcdMetric::ALL
                .into_iter()
                .map(|metric| SelectOption::new(metric.key(), metric.label()))
                .collect(),
            selected.key().to_string(),
            ControlState::Enabled,
            tab_index,
            cx,
            move |shell, value, _| {
                if let Some(metric) = LcdMetric::from_key(value) {
                    shell.lcd.edit(|editor| editor.metrics[slot] = metric);
                }
            },
        )
    }

    /// The panel brightness stepper.
    ///
    /// A stepper rather than a slider, for the reason the Lighting screen
    /// already found: this codebase's slider paints a value and receives no
    /// input, so it would look enabled and do nothing.
    fn lcd_brightness(&self, editor: &DisplayEditor, cx: &mut Context<Self>) -> Div {
        let step = |shell: &mut Self, steps: i16| {
            shell.popover = None;
            shell.lcd.edit(|editor| editor.adjust_brightness(steps));
        };

        div()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child("Brightness"),
            )
            .child(
                Button::new("lcd-brightness-down", "\u{2212}")
                    .tab_index(LCD_TAB_BRIGHTNESS)
                    .render()
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        step(shell, -1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .font(numeric())
                    .flex_none()
                    .min_w(px(72.0))
                    .text_align(gpui::TextAlign::Center)
                    .text_color(color::TEXT.hsla())
                    .child(format!("{}%", editor.brightness)),
            )
            .child(
                Button::new("lcd-brightness-up", "+")
                    .tab_index(LCD_TAB_BRIGHTNESS + 1)
                    .render()
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        step(shell, 1);
                        cx.notify();
                    })),
            )
    }

    fn settings(&self) -> Div {
        let (socket, version, config) = match self.link.status() {
            Some(status) => (
                status.socket_path.clone(),
                status.daemon_version.clone(),
                match status.config.recovery_message() {
                    Some(message) => message,
                    None => "Loaded".to_string(),
                },
            ),
            None => (
                "not connected".to_string(),
                "unknown".to_string(),
                "not available".to_string(),
            ),
        };

        let sampling = match self.link.telemetry() {
            Some(snapshot) => format!(
                "Every {} ms, {} ms since the last complete pass",
                snapshot.interval_ms,
                snapshot.oldest_section_age_ms(self.now_unix_ms)
            ),
            None => "no telemetry".to_string(),
        };

        let mut panel = Panel::new("Service")
            .render()
            .child(setting_row("Socket", socket))
            .child(setting_row("Daemon version", version))
            .child(setting_row("Configuration", config))
            .child(setting_row("Sampling", sampling))
            .child(setting_row(
                "Network",
                "No network request, no listening TCP or UDP socket.".to_string(),
            ));

        for failure in self.link.failed_collectors() {
            panel = panel.child(setting_row_owned(
                format!("{} collector", failure.collector.label()),
                failure.detail.clone(),
            ));
        }

        screen("Settings", "Local paths, versions and diagnostics.")
            .child(panel)
            .child(
                Panel::new("Diagnostics")
                    .subtitle("Serial numbers are redacted before anything leaves this machine.")
                    .render()
                    .child(
                        Button::new("export-diagnostics", "Export diagnostics")
                            .tab_index(SCREEN_TAB_BASE)
                            .state(if self.link.status().is_some() {
                                ControlState::Enabled
                            } else {
                                ControlState::disabled("The background service is not running.")
                            })
                            .render(),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(UNOFFICIAL_NOTICE),
            )
    }

    /// A select plus its popover, placed so it stays inside the window.
    #[allow(clippy::too_many_arguments)]
    fn select(
        &self,
        id: &'static str,
        label: &'static str,
        options: Vec<SelectOption>,
        selected: String,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> Div {
        // GPUI has no disabled semantics of its own: a handler left attached
        // still fires. Withholding it is what makes the refusal real rather
        // than a matter of styling.
        let enabled = state.is_enabled();
        let open = enabled
            && self.popover
                == Some(Popover::Options {
                    select: SharedString::from(id),
                });
        let control = Select::new(id, label)
            .options(options.clone())
            .selected(selected)
            .state(state)
            .tab_index(tab_index)
            .render()
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(
                        Popover::Options {
                            select: SharedString::from(id),
                        },
                        cx,
                    )
                }))
            });

        let on_select = Rc::new(on_select);

        div().relative().child(control).when(open, |this| {
            this.child(popover_surface(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .children(options.into_iter().map(|option| {
                        let value = option.value.clone();
                        let on_select = Rc::clone(&on_select);
                        div()
                            .id(SharedString::from(format!("{id}-{}", option.value)))
                            .tab_index(tab_index)
                            .tab_stop(true)
                            .w_full()
                            .min_h(TARGET_MIN)
                            .flex()
                            .items_center()
                            .px(space::MD)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .text_color(color::TEXT.hsla())
                            .hover(|this| this.bg(color::ACCENT.alpha(0.25)))
                            .focus(|this| this.border(FOCUS_RING).border_color(color::FOCUS.hsla()))
                            .child(option.label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                on_select(this, &value, cx);
                                this.popover = None;
                                cx.notify();
                            }))
                    })),
            ))
        })
    }

    /// Send the selected channel's pending program.
    ///
    /// Nothing is sent when the program cannot be built: the button is already
    /// disabled in that state, and checking again here is what keeps a keyboard
    /// activation from bypassing the same rule.
    fn apply_lighting(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.lighting.selected() else {
            return;
        };
        let channel = editor.channel;
        if let Ok(program) = editor.program() {
            self.feed
                .send(Command::ApplyLighting(LightingCommand { channel, program }));
        }
        cx.notify();
    }

    /// Brightness as a stepper, matching the Cooling screen's fixed duty.
    ///
    /// Not a slider. `Slider` in this codebase paints a value and receives no
    /// input, so rendering one here would put an enabled-looking control on
    /// screen that quietly does nothing. Two buttons and a readout are operable
    /// by pointer and by keyboard alike.
    fn brightness_row(
        &self,
        brightness: u8,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let step = |shell: &mut Self, steps: i16| {
            if let Some(channel) = shell.lighting.selected_mut() {
                channel.adjust_brightness(steps);
            }
        };

        div()
            .flex()
            .items_center()
            .gap(space::SM)
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child("Brightness"),
            )
            .child(
                Button::new("lighting-brightness-down", "\u{2212}")
                    .state(state.clone())
                    .tab_index(tab_index)
                    .render()
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.popover = None;
                        step(shell, -1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .font(numeric())
                    .flex_none()
                    .min_w(px(72.0))
                    .text_align(gpui::TextAlign::Center)
                    .text_color(color::TEXT.hsla())
                    .child(format!("{brightness}%")),
            )
            .child(
                Button::new("lighting-brightness-up", "+")
                    .state(state)
                    .tab_index(tab_index + 1)
                    .render()
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.popover = None;
                        step(shell, 1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(format!(
                        "Accepted range 0-{}",
                        nzxt_core::lighting::MAX_BRIGHTNESS
                    )),
            )
    }

    /// One channel's color field plus its swatch popover.
    fn lighting_color_field(
        &self,
        editor: &crate::lighting::ChannelEditor,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let channel = editor.channel;
        let open = self.popover == Some(Popover::LightingSwatches { channel });
        let id = SharedString::from(format!("lighting-color-{channel}"));

        // `ColorField` renders its own error state, so a stored color that
        // cannot be parsed names the problem on the field rather than only on
        // the disabled Apply button.
        let control = ColorField::new(id.clone(), "Color", editor.color.clone())
            .tab_index(tab_index)
            .render()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_popover(Popover::LightingSwatches { channel }, cx)
            }));

        div().relative().child(control).when(open, |this| {
            this.child(popover_surface(
                div().flex().flex_wrap().gap(space::SM).children(
                    SWATCHES.into_iter().enumerate().map(|(index, swatch)| {
                        div()
                            .id(SharedString::from(format!("{id}-swatch-{index}")))
                            .tab_index(tab_index)
                            .tab_stop(true)
                            .w(TARGET_MIN)
                            .h(TARGET_MIN)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .border_1()
                            .border_color(color::SEPARATOR.hsla())
                            .bg(swatch.hsla())
                            .hover(|this| this.border_color(color::FOCUS.hsla()))
                            .focus(|this| this.border(FOCUS_RING).border_color(color::FOCUS.hsla()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(editor) = this.lighting.channel_mut(channel) {
                                    editor.color = format!("{:06X}", swatch.0);
                                }
                                this.popover = None;
                                cx.notify();
                            }))
                    }),
                ),
            ))
        })
    }

    /// A color field plus its swatch popover.
    ///
    /// The field shows the digits as typed, so an entry the operator has not
    /// finished keeps its own error rather than being replaced by a value it
    /// never held.
    fn color_field(
        &self,
        field: DisplayColorField,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.popover == Some(Popover::Swatches { field });
        let id = SharedString::from(format!("lcd-color-{}", field.key()));

        let control = ColorField::new(
            id.clone(),
            field.label(),
            self.lcd.editor().color_text(field),
        )
        .tab_index(tab_index)
        .render()
        .on_click(
            cx.listener(move |this, _, _, cx| this.toggle_popover(Popover::Swatches { field }, cx)),
        );

        div().relative().child(control).when(open, |this| {
            this.child(popover_surface(
                div().flex().flex_wrap().gap(space::SM).children(
                    SWATCHES.into_iter().enumerate().map(|(index, swatch)| {
                        div()
                            .id(SharedString::from(format!("{id}-swatch-{index}")))
                            .tab_index(tab_index)
                            .tab_stop(true)
                            .w(TARGET_MIN)
                            .h(TARGET_MIN)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .border_1()
                            .border_color(color::SEPARATOR.hsla())
                            .bg(swatch.hsla())
                            .hover(|this| this.border_color(color::FOCUS.hsla()))
                            .focus(|this| this.border(FOCUS_RING).border_color(color::FOCUS.hsla()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.lcd.edit(|editor| {
                                    editor.set_color_text(field, format!("{:06X}", swatch.0))
                                });
                                this.popover = None;
                                cx.notify();
                            }))
                    }),
                ),
            ))
        })
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.destination {
            Destination::Monitoring => self.monitoring(),
            Destination::Cooling => self.cooling(cx),
            Destination::Lighting => self.lighting(cx),
            Destination::Lcd => self.lcd(cx),
            Destination::Settings => self.settings(),
        };

        div()
            .id("shell")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_focus_next))
            .on_action(cx.listener(Self::on_focus_previous))
            .on_action(cx.listener(Self::on_close_popover))
            .on_action(
                cx.listener(|this, _: &GoMonitoring, _, cx| this.go(Destination::Monitoring, cx)),
            )
            .on_action(cx.listener(|this, _: &GoCooling, _, cx| this.go(Destination::Cooling, cx)))
            .on_action(
                cx.listener(|this, _: &GoLighting, _, cx| this.go(Destination::Lighting, cx)),
            )
            .on_action(cx.listener(|this, _: &GoLcd, _, cx| this.go(Destination::Lcd, cx)))
            .on_action(
                cx.listener(|this, _: &GoSettings, _, cx| this.go(Destination::Settings, cx)),
            )
            .size_full()
            .flex()
            .bg(color::SURFACE.hsla())
            .text_color(color::TEXT.hsla())
            .text_sm()
            .child(self.rail(cx))
            .child(
                div()
                    .id("work-surface")
                    .flex_1()
                    // Without this the rail plus an unwrapped sentence can
                    // exceed the window width instead of wrapping.
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .p(space::LG)
                    .flex()
                    .flex_col()
                    .gap(space::LG)
                    .children(self.banner())
                    .child(content),
            )
    }
}

/// The standard heading and column of a destination.
fn screen(title: &'static str, subtitle: &'static str) -> Div {
    div().flex().flex_col().gap(space::LG).w_full().child(
        div()
            .flex()
            .flex_col()
            .gap(space::XS)
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color::TEXT.hsla())
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(subtitle),
            ),
    )
}

/// A row of metric tiles that wraps rather than scrolling sideways.
fn metric_row() -> Div {
    div().flex().flex_wrap().gap(space::XL).w_full().min_w_0()
}

fn numeric() -> gpui::Font {
    crate::theme::numeric_font()
}

/// A compact labeled readback with its freshness qualifier.
fn readback(label: &'static str, view: &MetricView<f32>, format: impl Fn(f32) -> String) -> Div {
    let (value, qualifier) = match view {
        MetricView::Fresh { value } => (format(*value), None),
        MetricView::Stale { value, .. } => (format(*value), Some("Stale")),
        MetricView::Unavailable { .. } => ("--".to_string(), Some("N/A")),
    };

    div()
        .flex()
        .flex_none()
        .items_baseline()
        .gap(space::XS)
        .child(
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label),
        )
        .child(
            div()
                .font(numeric())
                .text_color(if qualifier == Some("N/A") {
                    color::TEXT_DISABLED.hsla()
                } else {
                    color::TEXT.hsla()
                })
                .child(value),
        )
        .children(qualifier.map(|qualifier| {
            div()
                .text_sm()
                .text_color(color::WARNING.hsla())
                .child(qualifier)
        }))
}

/// A duty readback, converted from the tracked byte to a plottable number.
fn duty_view(view: &MetricView<u8>) -> MetricView<f32> {
    match view {
        MetricView::Fresh { value } => MetricView::Fresh {
            value: *value as f32,
        },
        MetricView::Stale { value, age_ms } => MetricView::Stale {
            value: *value as f32,
            age_ms: *age_ms,
        },
        MetricView::Unavailable { cause } => MetricView::Unavailable {
            cause: cause.clone(),
        },
    }
}

/// The channel's control mode, in words.
fn mode_readback(view: &MetricView<PwmMode>) -> Div {
    let (label, muted) = match view {
        MetricView::Fresh { value } => (value.label().to_string(), false),
        MetricView::Stale { value, .. } => (format!("{} (stale)", value.label()), false),
        MetricView::Unavailable { .. } => ("Mode N/A".to_string(), true),
    };

    div()
        .flex()
        .flex_none()
        .items_baseline()
        .gap(space::XS)
        .child(
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child("Mode"),
        )
        .child(
            div()
                .text_color(if muted {
                    color::TEXT_DISABLED.hsla()
                } else {
                    color::TEXT.hsla()
                })
                .child(label),
        )
}

/// One sentence for a safety alert, ready to render.
pub fn alert_message(alert: &SafetyAlert) -> String {
    alert.message()
}

/// State of the delete control for the currently active profile.
///
/// `None` means no daemon answered. The built-in safe profile is refused here
/// for the same reason the daemon refuses it: it is what everything else falls
/// back to, so it has to survive every other profile.
pub fn delete_state(active: Option<&str>) -> ControlState {
    match active {
        None => ControlState::disabled("The background service is not running."),
        Some(SAFE_PROFILE_NAME) => ControlState::disabled(
            "The built-in safe profile cannot be deleted. It is what the daemon falls back to \
             when anything else is unavailable.",
        ),
        Some(_) => ControlState::Enabled,
    }
}

/// What one activation of the delete control does.
///
/// Returns the armed flag to keep and the command to send, if any. The first
/// activation only arms: deleting the configuration an operator is running is
/// not something a stray click should accomplish.
pub fn next_deletion(active: Option<&str>, armed: bool) -> (bool, Option<Command>) {
    let Some(name) = active else {
        return (false, None);
    };
    if name == SAFE_PROFILE_NAME {
        return (false, None);
    }
    if armed {
        (false, Some(Command::DeleteProfile(name.to_string())))
    } else {
        (true, None)
    }
}

/// A channel's entry in the selector: its number and what is plugged into it.
pub fn channel_label(state: &ChannelState) -> String {
    match state.accessories.len() {
        0 => format!("Channel {}", state.channel),
        1 => format!("Channel {} ({})", state.channel, state.accessories[0]),
        count => format!("Channel {} ({count} accessories)", state.channel),
    }
}

/// What the controller reported about one channel's contents.
///
/// The controller reports accessory identifiers and never an LED count, so the
/// summary says so instead of leaving the operator to assume a number was
/// measured.
pub fn accessory_summary(state: &ChannelState) -> String {
    if state.accessories.is_empty() {
        return "nothing detected on this channel".to_string();
    }
    format!(
        "{} (LED count not reported by the controller)",
        state.accessories.join(", ")
    )
}

fn setting_row(label: &'static str, value: String) -> Div {
    setting_row_owned(label.to_string(), value)
}

fn setting_row_owned(label: String, value: String) -> Div {
    div()
        .flex()
        .justify_between()
        .gap(space::LG)
        .py(space::XS)
        .child(
            div()
                .text_color(color::TEXT_MUTED.hsla())
                .flex_none()
                .child(label),
        )
        .child(
            div()
                .text_color(color::TEXT.hsla())
                .flex_1()
                .text_align(gpui::TextAlign::Right)
                .child(value),
        )
}

/// The floating surface a popover renders on.
///
/// `anchored` is what keeps a popover opened near a window edge fully visible:
/// it repositions itself rather than being clipped. `deferred` paints it above
/// panels laid out after it, so a popover is never covered by its neighbor.
fn popover_surface(content: impl IntoElement) -> impl IntoElement {
    gpui::deferred(
        gpui::anchored().snap_to_window_with_margin(px(8.0)).child(
            div()
                .id("popover-surface")
                .w(px(280.0))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .p(space::SM)
                .rounded(RADIUS)
                .bg(color::PANEL.hsla())
                .border_1()
                .border_color(color::FOCUS.alpha(0.45))
                .shadow_lg()
                .child(content),
        ),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_state(channel: u8, accessories: &[&str]) -> ChannelState {
        ChannelState {
            channel,
            accessories: accessories.iter().map(|name| name.to_string()).collect(),
            committed: None,
        }
    }

    #[test]
    fn a_channel_names_what_the_controller_detected_and_never_an_led_count() {
        let one = channel_state(1, &["HUE 2 LED Strip 300 mm"]);
        assert_eq!(channel_label(&one), "Channel 1 (HUE 2 LED Strip 300 mm)");
        let summary = accessory_summary(&one);
        assert!(summary.contains("HUE 2 LED Strip 300 mm"), "{summary}");
        assert!(summary.contains("not reported"), "{summary}");

        let many = channel_state(2, &["AER RGB 2 120 mm", "AER RGB 2 140 mm"]);
        assert_eq!(channel_label(&many), "Channel 2 (2 accessories)");

        let empty = channel_state(3, &[]);
        assert_eq!(channel_label(&empty), "Channel 3");
        assert_eq!(
            accessory_summary(&empty),
            "nothing detected on this channel"
        );
    }

    #[test]
    fn every_lighting_control_has_its_own_tab_stop_inside_the_screen_range() {
        // The stops the screen actually passes, in traversal order. The
        // brightness stepper renders two buttons from one constant, so its
        // second stop is listed here as the screen emits it: a successor
        // counted by hand instead lands on a control that already exists.
        let indices = [
            LIGHTING_TAB_CHANNEL,
            LIGHTING_TAB_MODE,
            LIGHTING_TAB_COLOR,
            LIGHTING_TAB_BRIGHTNESS,
            LIGHTING_TAB_BRIGHTNESS + 1,
            LIGHTING_TAB_SPEED,
            LIGHTING_TAB_DIRECTION,
            LIGHTING_TAB_APPLY,
            LIGHTING_TAB_OFF,
        ];

        let mut sorted = indices.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            indices.to_vec(),
            sorted,
            "no two lighting controls share a tab stop, and they run in order"
        );
        assert!(
            indices
                .iter()
                .all(|index| *index > Destination::Settings.tab_index()),
            "screen controls come after every rail entry"
        );
    }

    #[test]
    fn the_shell_exposes_four_primary_destinations_and_one_secondary() {
        assert_eq!(Destination::PRIMARY.len(), 4);
        assert_eq!(
            Destination::PRIMARY.map(Destination::label),
            ["Monitoring", "Cooling", "Lighting", "LCD"]
        );
        assert!(!Destination::PRIMARY.contains(&Destination::Settings));
    }

    #[test]
    fn rail_tab_order_is_stable_and_precedes_screen_controls() {
        let mut indices: Vec<isize> = Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
            .map(Destination::tab_index)
            .collect();
        let sorted = {
            let mut copy = indices.clone();
            copy.sort();
            copy
        };
        assert_eq!(indices, sorted, "rail order must match visual order");

        indices.dedup();
        assert_eq!(indices.len(), 5, "every entry needs its own tab stop");
        assert!(indices.iter().all(|index| *index < SCREEN_TAB_BASE));
    }

    #[test]
    fn every_destination_has_a_label_and_a_glyph() {
        for destination in Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
        {
            assert!(!destination.label().is_empty());
            assert!(!destination.glyph().is_empty());
        }
    }

    #[test]
    fn the_screen_offers_only_the_modes_it_can_configure_completely() {
        // A mode whose Apply could only ever refuse is absent, not disabled,
        // which is the same rule the Lighting screen applies to an unproven
        // effect. Static images stay in the vocabulary and in the renderer.
        assert_eq!(SCREEN_MODES.len(), 2);
        assert!(SCREEN_MODES.contains(&DisplayMode::DualInfographic));
        assert!(SCREEN_MODES.contains(&DisplayMode::Solid));
        assert!(
            !SCREEN_MODES.contains(&DisplayMode::Image),
            "the screen has no control that can name a file"
        );
        assert!(
            SCREEN_MODES.iter().all(|mode| {
                let mut editor = DisplayEditor::default();
                editor.mode = *mode;
                editor.preset().is_ok()
            }),
            "every offered mode must produce a preset without further input"
        );
    }

    #[test]
    fn the_editor_exposes_the_six_color_controls_the_story_names() {
        assert_eq!(DisplayColorField::ALL.len(), 6);
        let labels: Vec<_> = DisplayColorField::ALL
            .map(DisplayColorField::label)
            .to_vec();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 6, "labels must be distinct: {labels:?}");
        for required in [
            "Reading 1",
            "Reading 2",
            "Text 1",
            "Text 2",
            "Background",
            "Logo",
        ] {
            assert!(labels.contains(&required), "{required} is missing");
        }
    }

    #[test]
    fn every_lcd_control_has_its_own_tab_stop_inside_the_screen_range() {
        // The brightness stepper renders two buttons from one constant, so its
        // successor has to clear both. The values the screen actually passes
        // are collected here rather than a hand-written range.
        let mut stops = vec![LCD_TAB_MODE, LCD_TAB_METRIC_ONE, LCD_TAB_METRIC_TWO];
        stops.extend(
            (0..DisplayColorField::ALL.len() as isize).map(|index| LCD_TAB_COLOR_BASE + index),
        );
        stops.extend([
            LCD_TAB_BRIGHTNESS,
            LCD_TAB_BRIGHTNESS + 1,
            LCD_TAB_ROTATE,
            LCD_TAB_APPLY,
        ]);

        let mut unique = stops.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            stops.len(),
            "two controls share a stop: {stops:?}"
        );
        assert!(stops.iter().all(|stop| *stop >= SCREEN_TAB_BASE));
    }

    #[test]
    fn swatches_are_distinct() {
        let mut values: Vec<u32> = SWATCHES.iter().map(|color| color.0).collect();
        values.sort();
        values.dedup();
        assert_eq!(values.len(), SWATCHES.len());
    }

    #[test]
    fn deleting_a_profile_takes_two_deliberate_activations() {
        // The first activation only arms the control.
        let (armed, command) = next_deletion(Some("Silent"), false);
        assert!(armed);
        assert_eq!(command, None, "one press must not delete anything");

        // The second sends the command the daemon acts on.
        let (armed, command) = next_deletion(Some("Silent"), true);
        assert!(!armed, "the control disarms once it has fired");
        assert_eq!(command, Some(Command::DeleteProfile("Silent".to_string())));
    }

    #[test]
    fn the_built_in_safe_profile_can_never_be_deleted() {
        for armed in [false, true] {
            assert_eq!(next_deletion(Some(SAFE_PROFILE_NAME), armed), (false, None));
            assert_eq!(next_deletion(None, armed), (false, None));
        }

        let state = delete_state(Some(SAFE_PROFILE_NAME));
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("falls back"), "{state:?}");

        assert!(delete_state(None).is_disabled());
        assert!(delete_state(Some("Silent")).is_enabled());
    }

    #[test]
    fn a_stalled_channel_alert_renders_the_channel_and_its_readback() {
        let message = alert_message(&SafetyAlert::ChannelStalled {
            channel: Channel::Pump,
            commanded_duty: 180,
            samples: 3,
            rpm: 0,
        });
        assert!(message.contains("Pump"), "{message}");
        assert!(message.contains("0 RPM"), "{message}");
    }
}
