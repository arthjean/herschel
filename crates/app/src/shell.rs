//! The application shell: a fixed navigation rail and one work surface.
//!
//! Four primary destinations, one secondary Settings entry, and nothing else.
//! The rail never scrolls and never changes width, so the work surface has a
//! known width to lay out against at the 920x640 target size.

use gpui::{
    App, Context, Div, FocusHandle, Focusable, KeyBinding, SharedString, Stateful, Window, actions,
    div, prelude::*, px,
};

use crate::components::{
    Button, ButtonVariant, ColorField, ControlState, CurveEditor, CurveNode, DeviceRow, Gauge,
    Panel, Select, SelectOption, Slider, Toggle,
};
use crate::link::LinkState;
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

    /// Glyph shown in the rail, so an entry is not identified by colour alone.
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

/// Which popover, if any, is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popover {
    /// A colour swatch list anchored to one colour field.
    Swatches { field: LcdColorField },
    /// An option list anchored to one select.
    Options { select: SharedString },
}

/// The four colour controls of the LCD editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LcdColorField {
    Reading,
    Text,
    Background,
    Logo,
}

impl LcdColorField {
    pub const ALL: [LcdColorField; 4] = [
        LcdColorField::Reading,
        LcdColorField::Text,
        LcdColorField::Background,
        LcdColorField::Logo,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Reading => "Reading colour",
            Self::Text => "Text colour",
            Self::Background => "Background",
            Self::Logo => "Wordmark colour",
        }
    }
}

/// Swatches offered by a colour popover.
pub const SWATCHES: [Color; 6] = [
    Color::rgb(0x6f4ef2),
    Color::rgb(0x30c8a0),
    Color::rgb(0xf5c451),
    Color::rgb(0xff8a8a),
    Color::rgb(0xe8eaee),
    Color::rgb(0x14161a),
];

/// The pending, unapplied state of the LCD editor.
///
/// Nothing here reaches hardware in this build: the LCD transport is
/// unvalidated, so the editor drives the preview only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcdEditor {
    pub display_mode: String,
    pub metric: String,
    pub reading: Color,
    pub text: Color,
    pub background: Color,
    pub logo: Color,
    /// Rotation in degrees, always one of 0, 90, 180, 270.
    pub rotation: u16,
}

impl Default for LcdEditor {
    fn default() -> Self {
        Self {
            display_mode: "dual_infographic".into(),
            metric: "cpu_gpu_temperature".into(),
            reading: Color::rgb(0x6f4ef2),
            text: Color::rgb(0xe8eaee),
            background: Color::rgb(0x14161a),
            logo: Color::rgb(0x30c8a0),
            rotation: 0,
        }
    }
}

impl LcdEditor {
    /// Advance the rotation by one validated increment.
    pub fn rotate(&mut self) {
        self.rotation = (self.rotation + 90) % 360;
    }

    pub fn color(&self, field: LcdColorField) -> Color {
        match field {
            LcdColorField::Reading => self.reading,
            LcdColorField::Text => self.text,
            LcdColorField::Background => self.background,
            LcdColorField::Logo => self.logo,
        }
    }

    pub fn set_color(&mut self, field: LcdColorField, value: Color) {
        match field {
            LcdColorField::Reading => self.reading = value,
            LcdColorField::Text => self.text = value,
            LcdColorField::Background => self.background = value,
            LcdColorField::Logo => self.logo = value,
        }
    }

    pub fn display_modes() -> Vec<SelectOption> {
        vec![
            SelectOption::new("dual_infographic", "Dual infographic"),
            SelectOption::new("single_metric", "Single metric"),
            SelectOption::new("solid_colour", "Solid colour"),
        ]
    }

    pub fn metrics() -> Vec<SelectOption> {
        vec![
            SelectOption::new("cpu_gpu_temperature", "CPU and GPU temperature"),
            SelectOption::new("liquid_temperature", "Liquid temperature"),
            SelectOption::new("pump_speed", "Pump speed"),
        ]
    }
}

/// The root view.
pub struct Shell {
    focus: FocusHandle,
    link: LinkState,
    destination: Destination,
    popover: Option<Popover>,
    lcd: LcdEditor,
}

impl Shell {
    pub fn new(link: LinkState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            focus,
            link,
            destination: Destination::Monitoring,
            popover: None,
            lcd: LcdEditor::default(),
        }
    }

    fn go(&mut self, destination: Destination, cx: &mut Context<Self>) {
        self.destination = destination;
        self.popover = None;
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
        Some(
            div()
                .w_full()
                .min_w_0()
                .p(space::MD)
                .rounded(RADIUS)
                .bg(color::WARNING.alpha(0.12))
                .border_1()
                .border_color(color::WARNING.alpha(0.5))
                .text_color(color::WARNING.hsla())
                .child(message),
        )
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
        use nzxt_core::KRAKEN_BASE;
        use nzxt_core::capability::CapabilityId;

        // Live sampling is a telemetry story. Until it lands, every gauge
        // renders its unavailable state with the reason, never a fabricated
        // reading.
        let reason = self
            .link
            .control_state(KRAKEN_BASE, CapabilityId::LiquidTemperature)
            .message()
            .map(str::to_string);

        screen("Monitoring", "System and cooling state at a glance.")
            .child(self.device_panel())
            .child(
                Panel::new("Kraken")
                    .subtitle(reason.unwrap_or_else(|| {
                        "Readings start once telemetry sampling is enabled.".to_string()
                    }))
                    .render()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(space::XL)
                            .child(
                                Gauge::new("Liquid", None)
                                    .unit(" C")
                                    .range(20.0, 60.0)
                                    .render(),
                            )
                            .child(
                                Gauge::new("Pump", None)
                                    .unit(" RPM")
                                    .range(0.0, 3000.0)
                                    .render(),
                            )
                            .child(
                                Gauge::new("Fan", None)
                                    .unit(" RPM")
                                    .range(0.0, 2000.0)
                                    .render(),
                            ),
                    ),
            )
    }

    fn cooling(&self, cx: &mut Context<Self>) -> Div {
        use nzxt_core::KRAKEN_BASE;
        use nzxt_core::capability::CapabilityId;

        let pump = self.link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty);
        let fan = self.link.control_state(KRAKEN_BASE, CapabilityId::FanDuty);
        let curve = self
            .link
            .control_state(KRAKEN_BASE, CapabilityId::PumpCurve);
        let apply = pump.clone();

        let profiles: Vec<SelectOption> = self
            .link
            .profiles()
            .iter()
            .map(|profile| SelectOption::new(profile.name.clone(), profile.name.clone()))
            .collect();
        let active = self
            .link
            .active_profile()
            .unwrap_or("Onboard safe")
            .to_string();

        screen(
            "Cooling",
            "Pump, fan and the onboard liquid-temperature curve.",
        )
        .child(
            Panel::new("Channels")
                .render()
                .child(
                    div()
                        .flex()
                        .gap(space::LG)
                        .child(
                            div().flex_1().min_w_0().child(
                                Slider::new("pump-duty", "Pump duty", 100.0)
                                    .range(20.0, 100.0)
                                    .unit("%")
                                    .state(pump)
                                    .tab_index(SCREEN_TAB_BASE)
                                    .render(),
                            ),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Slider::new("fan-duty", "Fan duty", 100.0)
                                    .range(0.0, 100.0)
                                    .unit("%")
                                    .state(fan)
                                    .tab_index(SCREEN_TAB_BASE + 1)
                                    .render(),
                            ),
                        ),
                )
                .child(self.select(
                    "profile",
                    "Active profile",
                    profiles,
                    active,
                    // Activating a profile is a write. It is disabled for the
                    // same reasons every other write control on this screen is.
                    self.link.write_state(),
                    SCREEN_TAB_BASE + 2,
                    cx,
                )),
        )
        .child(
            Panel::new("Liquid temperature curve")
                .subtitle("Editing changes nothing until Apply is activated.")
                .render()
                .child(CurveEditor::new(default_curve()).state(curve).render())
                .child(
                    div()
                        .flex()
                        .gap(space::SM)
                        .child(
                            Button::new("curve-apply", "Apply")
                                .variant(ButtonVariant::Primary)
                                .state(apply)
                                .tab_index(SCREEN_TAB_BASE + 3)
                                .render(),
                        )
                        .child(
                            Button::new("curve-cancel", "Cancel")
                                .tab_index(SCREEN_TAB_BASE + 4)
                                .render(),
                        ),
                ),
        )
    }

    fn lighting(&self, cx: &mut Context<Self>) -> Div {
        use nzxt_core::RGB_CONTROLLER;
        use nzxt_core::capability::CapabilityId;

        let fixed = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        let effects = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbEffects);

        screen(
            "Lighting",
            "Per-channel colour on the validated controller.",
        )
        .child(
            Panel::new("Channel 1")
                .subtitle("Channel topology is recorded before any write is offered.")
                .render()
                .child(self.color_field(LcdColorField::Reading, SCREEN_TAB_BASE, cx))
                .child(
                    Toggle::new("channel-1-power", "Channel power", false)
                        .state(fixed.clone())
                        .tab_index(SCREEN_TAB_BASE + 1)
                        .render(),
                )
                .child(
                    Button::new("lighting-apply", "Apply")
                        .variant(ButtonVariant::Primary)
                        .state(fixed)
                        .tab_index(SCREEN_TAB_BASE + 2)
                        .render(),
                ),
        )
        .child(
            Panel::new("Effects")
                .subtitle("Only effects proven on this exact controller appear here.")
                .render()
                .child(
                    div().text_color(color::TEXT_MUTED.hsla()).child(
                        effects
                            .message()
                            .unwrap_or("No validated effect.")
                            .to_string(),
                    ),
                ),
        )
    }

    fn lcd(&self, cx: &mut Context<Self>) -> Div {
        use nzxt_core::KRAKEN_BASE;
        use nzxt_core::capability::CapabilityId;

        let frame = self.link.control_state(KRAKEN_BASE, CapabilityId::LcdFrame);
        let editor = self.lcd.clone();

        screen("LCD", "Layout and colours for the Kraken display.").child(
            div()
                .flex()
                .gap(space::LG)
                .child(
                    Panel::new("Editor")
                        .render()
                        .flex_1()
                        .child(self.select(
                            "lcd-mode",
                            "Display mode",
                            LcdEditor::display_modes(),
                            editor.display_mode.clone(),
                            ControlState::Enabled,
                            SCREEN_TAB_BASE,
                            cx,
                        ))
                        .child(self.select(
                            "lcd-metric",
                            "Metric",
                            LcdEditor::metrics(),
                            editor.metric.clone(),
                            ControlState::Enabled,
                            SCREEN_TAB_BASE + 1,
                            cx,
                        ))
                        .children(LcdColorField::ALL.into_iter().enumerate().map(
                            |(index, field)| {
                                self.color_field(field, SCREEN_TAB_BASE + 2 + index as isize, cx)
                            },
                        ))
                        .child(
                            div()
                                .flex()
                                .gap(space::SM)
                                .child(
                                    Button::new("lcd-rotate", "Rotate display")
                                        .tab_index(SCREEN_TAB_BASE + 6)
                                        .render()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            // Acting on the editor dismisses any
                                            // open popover, so a swatch list
                                            // never hides the result.
                                            this.popover = None;
                                            this.lcd.rotate();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("lcd-apply", "Apply")
                                        .variant(ButtonVariant::Primary)
                                        .state(frame)
                                        .tab_index(SCREEN_TAB_BASE + 7)
                                        .render(),
                                ),
                        ),
                )
                .child(
                    Panel::new("Preview")
                        .subtitle(format!("Rotation {} degrees", editor.rotation))
                        .render()
                        .flex_none()
                        .w_auto()
                        .child(crate::preview::circular_preview(editor)),
                ),
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

        screen("Settings", "Local paths, versions and diagnostics.")
            .child(
                Panel::new("Service")
                    .render()
                    .child(setting_row("Socket", socket))
                    .child(setting_row("Daemon version", version))
                    .child(setting_row("Configuration", config))
                    .child(setting_row(
                        "Network",
                        "No network request, no listening TCP or UDP socket.".to_string(),
                    )),
            )
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

        div().relative().child(control).when(open, |this| {
            this.child(popover_surface(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    .children(options.into_iter().map(|option| {
                        div()
                            .id(SharedString::from(format!("{id}-{}", option.value)))
                            .w_full()
                            .min_h(TARGET_MIN)
                            .flex()
                            .items_center()
                            .px(space::MD)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .text_color(color::TEXT.hsla())
                            .hover(|this| this.bg(color::ACCENT.alpha(0.25)))
                            .child(option.label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.popover = None;
                                cx.notify();
                            }))
                    })),
            ))
        })
    }

    /// A colour field plus its swatch popover.
    fn color_field(&self, field: LcdColorField, tab_index: isize, cx: &mut Context<Self>) -> Div {
        let value = self.lcd.color(field);
        let open = self.popover == Some(Popover::Swatches { field });
        let id = SharedString::from(format!("colour-{}", field.label()));

        let control = ColorField::new(id.clone(), field.label(), format!("{:06X}", value.0))
            .tab_index(tab_index)
            .render()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_popover(Popover::Swatches { field }, cx)
            }));

        div().relative().child(control).when(open, |this| {
            this.child(popover_surface(
                div().flex().flex_wrap().gap(space::SM).children(
                    SWATCHES.into_iter().enumerate().map(|(index, swatch)| {
                        div()
                            .id(SharedString::from(format!("{id}-swatch-{index}")))
                            .w(TARGET_MIN)
                            .h(TARGET_MIN)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .border_1()
                            .border_color(color::SEPARATOR.hsla())
                            .bg(swatch.hsla())
                            .hover(|this| this.border_color(color::FOCUS.hsla()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.lcd.set_color(field, swatch);
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

fn setting_row(label: &'static str, value: String) -> Div {
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
/// panels laid out after it, so a popover is never covered by its neighbour.
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

/// The curve shown before a profile defines one.
///
/// Ten control nodes over the kernel's 20-59 C range, which is what the editor
/// exposes regardless of the 40 values the ABI finally receives.
pub fn default_curve() -> Vec<CurveNode> {
    (0..10)
        .map(|index| {
            let temperature_c = 20.0 + index as f32 * (39.0 / 9.0);
            let duty_percent = 40.0 + index as f32 * (60.0 / 9.0);
            CurveNode {
                temperature_c,
                duty_percent,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rotation_cycles_through_the_four_validated_increments() {
        let mut editor = LcdEditor::default();
        assert_eq!(editor.rotation, 0);
        for expected in [90, 180, 270, 0, 90] {
            editor.rotate();
            assert_eq!(editor.rotation, expected);
        }
    }

    #[test]
    fn the_editor_exposes_exactly_four_colour_controls() {
        assert_eq!(LcdColorField::ALL.len(), 4);
        let labels: Vec<_> = LcdColorField::ALL.map(LcdColorField::label).to_vec();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 4, "labels must be distinct: {labels:?}");
    }

    #[test]
    fn the_editor_exposes_two_selects_with_options() {
        assert!(LcdEditor::display_modes().len() >= 2);
        assert!(LcdEditor::metrics().len() >= 2);
        let default = LcdEditor::default();
        assert!(
            LcdEditor::display_modes()
                .iter()
                .any(|option| option.value == default.display_mode)
        );
        assert!(
            LcdEditor::metrics()
                .iter()
                .any(|option| option.value == default.metric)
        );
    }

    #[test]
    fn setting_a_colour_changes_only_that_field() {
        let mut editor = LcdEditor::default();
        let before = editor.clone();
        editor.set_color(LcdColorField::Text, Color::rgb(0x123456));

        assert_eq!(editor.color(LcdColorField::Text), Color::rgb(0x123456));
        assert_eq!(editor.reading, before.reading);
        assert_eq!(editor.background, before.background);
        assert_eq!(editor.logo, before.logo);
    }

    #[test]
    fn the_default_curve_covers_the_kernel_range_with_ten_nodes() {
        let curve = default_curve();
        assert_eq!(curve.len(), 10);
        assert_eq!(curve[0].temperature_c, 20.0);
        assert!((curve[9].temperature_c - 59.0).abs() < 0.001);
        assert!(
            curve
                .windows(2)
                .all(|pair| pair[0].duty_percent <= pair[1].duty_percent)
        );
        assert!(curve.iter().all(|node| node.duty_percent <= 100.0));
    }

    #[test]
    fn swatches_are_distinct() {
        let mut values: Vec<u32> = SWATCHES.iter().map(|colour| colour.0).collect();
        values.sort();
        values.dedup();
        assert_eq!(values.len(), SWATCHES.len());
    }
}
