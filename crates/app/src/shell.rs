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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Context, Div, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels,
    Point, SharedString, Stateful, Window, actions, div, prelude::*, px,
};
use kori_core::capability::CapabilityId;
use kori_core::display::{DisplayMode, LcdMetric, MetricSample};
use kori_core::ipc::ChannelState;
use kori_core::lighting::{EffectDirection, EffectSpeed, LightingCommand};
use kori_core::profile::{
    Channel, CoolingProgram, CurveNodes, MAX_DUTY, MAX_DUTY_PERCENT, Profile, SAFE_PROFILE_NAME,
    duty_from_percent, duty_to_percent,
};
use kori_core::telemetry::{
    Collector, HISTORY_WINDOW_MS, KrakenTelemetry, MetricView, PwmMode, SafetyAlert,
    format_binary_bytes, format_temperature,
};
use kori_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER};

use crate::assets::Icon;
use crate::components::{
    Button, ButtonVariant, ColorField, ControlState, CurveEditor, DeviceRow, ICON_SIZE, Metric,
    Note, NoteLevel, Panel, Select, SelectOption, Slider, Sparkline, chevron, focus_visible, icon,
    node_at, panel_surface, row_panel, set_focus_visible,
};
use crate::cooling::{CoolingEditor, CoolingMode};
use crate::display::{DisplayColorField, DisplayEditor, DisplayScreen};
use crate::feed::{Command, CommandOutcome, Feed, OutcomeSeverity, now_unix_ms};
use crate::lighting::{LightingEditor, LightingMode};
use crate::link::LinkState;
use crate::metrics::MetricBook;
use crate::theme::{
    CARD_INSET, CARD_RADIUS, Color, DEGREE_C, FOCUS_RING, MENU_MAX_HEIGHT, MENU_MAX_WIDTH,
    MENU_MIN_WIDTH, MENU_OFFSET, MENU_RADIUS, MENU_ROW_GAP, MENU_ROW_HEIGHT, META_SEPARATOR,
    RADIUS, RAIL_WIDTH, ROW_RADIUS, SWATCH_RADIUS, SWATCH_SIZE, TARGET_MIN, UNOFFICIAL_NOTICE,
    color, space,
};
use crate::window_chrome::{self, DragLatch};

actions!(
    shell,
    [
        FocusNext,
        FocusPrevious,
        GoMonitoring,
        GoCooling,
        GoLighting,
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
        KeyBinding::new("ctrl-comma", GoSettings, None),
        KeyBinding::new("escape", ClosePopover, None),
    ]
}

/// The only destinations this product has.
///
/// The panel has no destination of its own. It is one device's appearance
/// among the others, so it lives on Lighting next to the channels of the
/// controller, which is also where an operator looking for "what my hardware
/// shows" goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    Monitoring,
    Cooling,
    Lighting,
    Settings,
}

impl Destination {
    /// Primary destinations, in rail order.
    pub const PRIMARY: [Destination; 3] = [
        Destination::Monitoring,
        Destination::Cooling,
        Destination::Lighting,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Monitoring => "Monitoring",
            Self::Cooling => "Cooling",
            Self::Lighting => "Lighting",
            Self::Settings => "Settings",
        }
    }

    /// Icon shown in the rail, so an entry is not identified by color alone.
    pub fn icon(self) -> Icon {
        match self {
            Self::Monitoring => Icon::ChartLine,
            Self::Cooling => Icon::Snowflake,
            Self::Lighting => Icon::Bulb,
            Self::Settings => Icon::Settings,
        }
    }

    /// Tab index of this rail entry. Rail entries come before screen controls.
    pub fn tab_index(self) -> isize {
        match self {
            Self::Monitoring => 1,
            Self::Cooling => 2,
            Self::Lighting => 3,
            Self::Settings => 4,
        }
    }
}

/// How still the Cooling screen has to be before an edit is written.
///
/// Long enough that a drag, a burst of arrow keys or a few stepper presses
/// count as one change, and short enough that letting go feels like the value
/// took. It also has to clear `MIN_PROGRAM_INTERVAL_MS`, the floor the daemon
/// enforces to keep the firmware from discarding writes that arrive too fast.
const AUTOSAVE_QUIET: Duration = Duration::from_millis(400);

/// How still one Lighting row has to be before its edit is written.
///
/// Shorter than [`AUTOSAVE_QUIET`] on purpose. That constant answers to
/// `MIN_PROGRAM_INTERVAL_MS`, the 200 ms floor of the thermal path; the lighting
/// floor is `MIN_COMMAND_INTERVAL_MS`, four times lower, and a color that takes
/// four tenths of a second to appear reads as a control that did not take.
/// Still several times the floor, so a drag or a held arrow key resolves to one
/// command rather than to a run of refusals.
const LIGHTING_QUIET: Duration = Duration::from_millis(150);

/// First tab index available to a screen's own controls.
pub const SCREEN_TAB_BASE: isize = 10;

/// Tab stops of the Cooling screen, in traversal order.
///
/// Each channel row reserves a whole block: the row header itself, then every
/// control its open detail can render. Reserving the block per row rather than
/// sharing one detail range is what keeps keyboard traversal in visual order
/// whichever row is open, since an open row's controls sit above the next row.
pub const COOLING_TAB_MODE: isize = SCREEN_TAB_BASE;
/// Width of the two selects that sit in the Cooling header.
///
/// Narrower than the 260 they used to hold, which is what leaves the coolant
/// readout beside them a column of its own at the 920-pixel target rather than
/// pushing it onto a line by itself.
pub const COOLING_SELECT_WIDTH: Pixels = px(228.0);
/// Width reserved for one readback on a channel row, label and value together.
///
/// A floor rather than a fixed width, and for the same reason: an intrinsic
/// width puts `RPM 964` and `RPM 1785` in different columns, and the two rows
/// then read as two layouts. A floor still lets a reading longer than the target
/// size allows for push the column out instead of being clipped by it, which is
/// what the 200% interface scale needs.
pub const COOLING_READBACK_WIDTH: Pixels = px(124.0);
pub const COOLING_TAB_PROFILE: isize = COOLING_TAB_MODE + 1;
pub const COOLING_TAB_ROW_BASE: isize = COOLING_TAB_PROFILE + 1;
/// Stops one row occupies: its header, the duty slider and the plot.
///
/// Wider than the controls an open row renders, and deliberately: the stops are
/// reserved per row whichever mode it is in, so a mode that hides the duty
/// slider cannot renumber the row below it.
pub const COOLING_ROW_STRIDE: isize = 4;
/// Offsets inside a channel's block, named rather than counted at the call site.
pub const COOLING_OFFSET_DUTY: isize = 1;
pub const COOLING_OFFSET_CURVE: isize = 3;

/// There is no Apply stop: an edit reaches the hardware on its own.
pub const COOLING_TAB_REVERT: isize = COOLING_TAB_ROW_BASE + 2 * COOLING_ROW_STRIDE;
pub const COOLING_TAB_SAVE: isize = COOLING_TAB_REVERT + 1;
pub const COOLING_TAB_DELETE: isize = COOLING_TAB_SAVE + 1;

/// Side of the dot that marks a write still in flight.
const STATUS_DOT: Pixels = px(6.0);

/// First tab stop of one channel row's block.
pub fn cooling_row_tab(channel: Channel) -> isize {
    let index = match channel {
        Channel::Pump => 0,
        Channel::Fan => 1,
    };
    COOLING_TAB_ROW_BASE + index * COOLING_ROW_STRIDE
}

/// Tab stops of the Lighting screen, in traversal order.
///
/// The screen is a list of device rows, so the stops are allocated per row the
/// way the Cooling screen allocates them per channel: one block each, wide
/// enough for every control an open row can render. Reserving the block rather
/// than sharing one detail range is what keeps traversal in visual order
/// whichever row is open, since an open row's controls sit above the next row.
///
/// The offsets inside a block are named rather than counted at the call site,
/// so a control that appears or disappears with the mode cannot renumber the
/// ones after it.
pub const LIGHTING_TAB_ROW_BASE: isize = SCREEN_TAB_BASE;
/// Stops one lighting channel occupies, open or closed.
pub const LIGHTING_ROW_STRIDE: isize = 10;

/// Offsets inside a channel's block.
///
/// The block ends at the effect parameters. There is no Apply stop and no Turn
/// off stop: an edit reaches the controller on its own, and Off is one of the
/// entries in the mode select rather than a button repeating it.
pub const ROW_OFFSET_BRIGHTNESS: isize = 1;
pub const ROW_OFFSET_MODE: isize = 2;
pub const CHANNEL_OFFSET_COLOR: isize = 3;
pub const CHANNEL_OFFSET_SPEED: isize = 4;
pub const CHANNEL_OFFSET_DIRECTION: isize = 5;

/// Offsets inside the panel's block, which is the last one on the screen.
///
/// It is wider than [`LIGHTING_ROW_STRIDE`] and may be: nothing is allocated
/// after it. The color fields keep a fixed stop per field rather than per
/// rendered control, so a mode that hides two of them does not renumber the
/// rest.
pub const LCD_OFFSET_METRIC_ONE: isize = 3;
pub const LCD_OFFSET_METRIC_TWO: isize = 4;
pub const LCD_OFFSET_COLOR_BASE: isize = 5;
/// The one action the panel row still carries, and only while it is faulted.
///
/// Not an Apply: an edit is already on its way. A stopped stream is the one
/// state no edit can clear, because nothing about the preset changed when the
/// transfer failed, so it takes a deliberate activation. The stop is reserved
/// whether or not the control renders, for the same reason every other offset
/// here is.
pub const LCD_OFFSET_RESUME: isize = LCD_OFFSET_COLOR_BASE + DisplayColorField::ALL.len() as isize;

/// First tab stop of the channel row at `index` in the rendered list.
pub fn lighting_row_tab(index: usize) -> isize {
    LIGHTING_TAB_ROW_BASE + index as isize * LIGHTING_ROW_STRIDE
}

/// First tab stop of the panel row, which follows every channel row.
///
/// Derived from how many channels the controller reported rather than from a
/// fixed ceiling: a controller that answers with more channels than expected
/// pushes the panel's block down instead of colliding with it.
pub fn lcd_row_tab(channel_count: usize) -> isize {
    lighting_row_tab(channel_count)
}

/// Width of the preview column, wide enough for the panel plus its padding.
pub const PREVIEW_COLUMN_WIDTH: Pixels = px(276.0);

/// Width of the two controls a device row carries on its right side.
///
/// Sized so the head of the row, the stepper and the mode select fit one line
/// at the 920-pixel target: the row wraps below that rather than clipping, but
/// it must not wrap at the size the layout is designed for.
pub const ROW_BRIGHTNESS_WIDTH: Pixels = px(176.0);
pub const ROW_MODE_WIDTH: Pixels = px(156.0);
/// Smallest the head of a row may become before the line wraps.
pub const ROW_HEAD_MIN_WIDTH: Pixels = px(180.0);
/// Width of one field in an open row's detail, two of which fit side by side
/// in the column left of the preview.
pub const FIELD_WIDTH: Pixels = px(168.0);
/// What the controller's card is headed with.
///
/// What the device does rather than the product string it reports: that string
/// is a vendor wordmark, and this heading says which of the two devices the
/// card is for, which is what the operator needs from it. The reported string
/// is still shown on the Devices panel.
pub const RGB_CONTROLLER_NAME: &str = "RGB & Fan Controller";

/// Left inset of an open row's detail.
///
/// Lines up with what follows the chevron on the line above: the head's own
/// padding, the chevron, and the gap after it. A detail that starts under the
/// chevron reads as another row rather than as the inside of this one.
pub const ROW_DETAIL_INDENT: Pixels = px(32.0);
/// Side of the appearance thumbnail at the head of a device row.
pub const ROW_THUMBNAIL: Pixels = px(34.0);
/// Side of the glyph drawn inside that thumbnail.
///
/// Larger than [`ICON_SIZE`], which is the size of a glyph sitting beside text.
/// This one sits inside a filled tile and has to survive the fill around it, so
/// it takes a little under two thirds of the side, leaving a margin of the
/// color on every edge.
pub const ROW_THUMBNAIL_GLYPH: Pixels = px(20.0);

/// One openable row of the Lighting screen.
///
/// The panel is a row here rather than a destination of its own: it is one
/// device's appearance, configured next to the controller's channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingRow {
    Channel(u8),
    Lcd,
}

impl LightingRow {
    /// Stable fragment for the element ids this row's controls carry.
    pub fn key(self) -> String {
        match self {
            Self::Channel(channel) => format!("channel-{channel}"),
            Self::Lcd => "lcd".to_string(),
        }
    }
}

/// The second fact a device row carries, if it has one.
///
/// A fragment rides on the name's own line, muted and after a separator, and
/// leaves the row one line tall. A sentence takes a line of its own. Which shape
/// a row uses is a property of what it has to say rather than of the row: a
/// channel's is "Channel 1", and a panel's is a mode, an orientation and a
/// brightness.
///
/// One value rather than two optional strings, so a row cannot ask for both and
/// end up back at the two lines the fragment exists to avoid.
enum RowNote {
    Fragment(String),
    Sentence(String),
}

/// When each Lighting row's pending edit should leave the process.
///
/// One deadline per row rather than one for the screen. The daemon tracks its
/// cadence floor per channel, so two rows edited in the same breath are two
/// independent writes and a single deadline would make one of them wait behind
/// the other for no reason.
///
/// A row edited again before its deadline pushes that deadline out, which is
/// how a drag or a held arrow key resolves to one command. Every edit spawns a
/// timer and every timer drains whatever has come due, so a timer that fires
/// into a moved deadline does nothing and no task has to be cancelled.
#[derive(Debug, Default)]
pub struct WriteSchedule {
    due: Vec<(LightingRow, Instant)>,
}

impl WriteSchedule {
    /// Arrange for `row` to be written once it has been still until `at`.
    pub fn touch(&mut self, row: LightingRow, at: Instant) {
        match self.due.iter_mut().find(|(queued, _)| *queued == row) {
            Some((_, deadline)) => *deadline = at,
            None => self.due.push((row, at)),
        }
    }

    /// Every row whose deadline has passed, removed from the schedule.
    pub fn take_due(&mut self, now: Instant) -> Vec<LightingRow> {
        let (ready, waiting) = std::mem::take(&mut self.due)
            .into_iter()
            .partition::<Vec<_>, _>(|(_, deadline)| *deadline <= now);
        self.due = waiting;
        ready.into_iter().map(|(row, _)| row).collect()
    }

    /// Whether anything is waiting to be written.
    pub fn is_pending(&self, row: LightingRow) -> bool {
        self.due.iter().any(|(queued, _)| *queued == row)
    }
}

/// A brightness drag in progress.
///
/// The track's rectangle is captured when the drag starts rather than read
/// again on every move. That is what lets the pointer leave the slider, or the
/// row below it, and keep dragging the value it grabbed: the row that owns the
/// drag is named here, so no other slider can answer for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrightnessDrag {
    pub row: LightingRow,
    pub track: Bounds<Pixels>,
}

/// A curve node drag in progress.
///
/// The node is captured when the pointer goes down and does not change for the
/// rest of the gesture, so a drag moves the node that was grabbed rather than
/// repainting whichever one the pointer happens to be over. `base` is the whole
/// curve as it stood at that moment, which is what
/// [`CoolingEditor::set_node_from`] replays every move against.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CurveDrag {
    channel: Channel,
    node: usize,
    base: CurveNodes,
}

/// A fixed-duty drag in progress, tracked exactly as [`BrightnessDrag`] is.
///
/// Same reasoning: the rectangle is captured at the press so the pointer can
/// leave the slider, and the channel is named so the other channel's track
/// cannot answer for the one being dragged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DutyDrag {
    pub channel: Channel,
    pub track: Bounds<Pixels>,
}

/// Where each operable track was painted, by whatever names the control.
///
/// The window decides which slider a press landed on, rather than the slider
/// hearing about the press itself. A listener on the control was the first
/// arrangement and it never fired: the press has to travel down an element
/// whose interactive state, hover styling and focus ring all sit between it and
/// the handler. The window's own capture handler is the one place an event is
/// guaranteed to arrive, and the same handler already runs there for focus.
///
/// A disabled slider records nothing, so a track that cannot be moved cannot be
/// grabbed either.
///
/// Keyed by whatever names the control rather than by a lighting row: the same
/// arrangement is what the fixed-duty sliders on Cooling need, and two copies of
/// this bookkeeping would be two places for a stale rectangle to survive.
#[derive(Debug)]
pub struct TrackMap<K>(RefCell<Vec<(K, Bounds<Pixels>)>>);

impl<K> Default for TrackMap<K> {
    fn default() -> Self {
        Self(RefCell::new(Vec::new()))
    }
}

impl<K: Copy + PartialEq> TrackMap<K> {
    /// Publish where one key's track was painted, replacing its last position.
    pub fn record(&self, key: K, track: Bounds<Pixels>) {
        let mut tracks = self.0.borrow_mut();
        match tracks.iter_mut().find(|(known, _)| *known == key) {
            Some(entry) => entry.1 = track,
            None => tracks.push((key, track)),
        }
    }

    /// Forget every track, so a row that went away takes its rectangle with it.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The track under a pointer position, if any.
    ///
    /// Dilated a little: the handle is 22 pixels tall inside a taller control,
    /// and a press a pixel above the track is a press on the track as far as
    /// the operator is concerned.
    pub fn at(&self, position: Point<Pixels>) -> Option<(K, Bounds<Pixels>)> {
        self.0
            .borrow()
            .iter()
            .find(|(_, track)| track.dilate(TRACK_GRAB_MARGIN).contains(&position))
            .copied()
    }
}

/// How far outside a track a press still counts as landing on it.
pub const TRACK_GRAB_MARGIN: Pixels = px(6.0);

/// Modes the screen can configure completely.
///
/// [`DisplayMode::Image`] is deliberately absent. The screen has no control
/// that can name a file, and a mode the daemon could only ever refuse would be
/// an entry that says the feature is here and then does nothing. The renderer
/// and the daemon support it, and a saved profile can select it; what is
/// missing is a way to pick the file, which no criterion in US-017 asks the
/// screen for.
pub const SCREEN_MODES: [DisplayMode; 3] = [
    DisplayMode::DualInfographic,
    DisplayMode::SingleReading,
    DisplayMode::Solid,
];

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

/// Whether a control shows the caption naming it.
///
/// A control in an open detail carries its own name, because nothing else
/// around it does. A control on a device row does not: the row already names
/// the device and the control is one of two on the line, so a caption over each
/// one is a second line of text that says what the first line said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caption {
    Shown,
    Hidden,
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
    /// Painted rectangle of each channel's plot, published during paint.
    ///
    /// One per channel, because both rows can be open: a single cell would
    /// hold whichever plot painted last and send every press to that curve.
    pump_curve_bounds: Rc<Cell<Bounds<Pixels>>>,
    fan_curve_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The curve node the pointer is currently dragging, if any.
    curve_drag: Option<CurveDrag>,
    /// When the current cooling edit should be written, once it settles.
    autosave_at: Option<Instant>,
    /// The program the last cooling write sent, held until its outcome arrives.
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
    /// The one row of the Lighting screen whose controls are revealed.
    ///
    /// One at a time, as on Cooling: an open row's controls sit above the next
    /// row, and two open at once would put the panel's editor an arm's length
    /// below the line it belongs to.
    lighting_open: Vec<LightingRow>,
    /// When each edited Lighting row should be written, once it settles.
    lighting_due: WriteSchedule,
    /// The brightness slider the pointer is currently dragging, if any.
    brightness_drag: Option<BrightnessDrag>,
    /// Where this frame painted each operable brightness track.
    tracks: Rc<TrackMap<LightingRow>>,
    /// The fixed-duty slider the pointer is currently dragging, if any.
    duty_drag: Option<DutyDrag>,
    /// Where this frame painted each operable fixed-duty track.
    duty_tracks: Rc<TrackMap<Channel>>,
    /// Set while the pointer holds the title bar, before a move begins.
    window_drag: DragLatch,
    /// Set once a daemon status has seeded the panel editor.
    ///
    /// The seeding happens on the first status that arrives and never again: a
    /// later snapshot arriving mid-edit would replace what the operator is
    /// typing with what the panel is still showing. The first status is also the
    /// only moment where nothing can be lost, since every write control is
    /// disabled until the link reports one.
    lcd_seeded: bool,
    /// The committed program the last status reported, as it was reported.
    ///
    /// Held so a change can be told from a repetition. The panel is seeded once
    /// and never again because its editor holds text a snapshot must not
    /// retype; a cooling program has no such field, and following it is what
    /// puts a newly activated profile's curve on the plot without the client
    /// having to guess what activation did.
    committed: Option<CoolingProgram>,
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
            pump_curve_bounds: Rc::new(Cell::new(Bounds::default())),
            fan_curve_bounds: Rc::new(Cell::new(Bounds::default())),
            curve_drag: None,
            autosave_at: None,
            sent: None,
            outcome: None,
            confirm_delete: false,
            destination: Destination::Monitoring,
            popover: None,
            lcd: DisplayScreen::default(),
            lighting: LightingEditor::default(),
            // The panel opens first: it is the row with something to look at,
            // and a screen where every row is shut hides the editor the last
            // arrangement put on its own destination.
            lighting_open: vec![LightingRow::Lcd],
            lighting_due: WriteSchedule::default(),
            brightness_drag: None,
            tracks: Rc::new(TrackMap::default()),
            duty_drag: None,
            duty_tracks: Rc::new(TrackMap::default()),
            window_drag: DragLatch::default(),
            lcd_seeded: false,
            committed: None,
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
            // What the panel is running outlives this window: the daemon keeps
            // writing the preset it committed, so opening the client again must
            // show that preset rather than the factory arrangement. The first
            // status is what seeds it, and only the first, so a snapshot cannot
            // overwrite an edit in progress.
            if !self.lcd_seeded
                && let Some(status) = self.link.status()
            {
                self.lcd_seeded = true;
                if let Some(preset) = status.display.committed.clone() {
                    self.lcd.adopt(&preset);
                }
            }
            self.adopt_committed_program();
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
            // A confirmed write is what turns a pending curve into the
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

        // The timers above are what make an edit land promptly. This is what
        // makes sure it lands at all: a write held back because another was in
        // flight, or a timer that fired while a drag was still running, is
        // picked up here on the next sample rather than waiting for the
        // operator to touch the screen again.
        self.flush_autosave(cx);
        self.flush_lighting(cx);

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

    /// Open one row of the Lighting screen, or close it if it is open.
    ///
    /// Rows open independently: a controller with three channels and a panel is
    /// four appearances of one machine, and comparing two of them means having
    /// both on screen. Nothing in a lighting detail is shared between rows, so
    /// there is nothing for a second open row to make ambiguous.
    ///
    /// Every control the detail renders is built for the channel whose line it
    /// sits under and carries that channel with it, so opening a row reveals
    /// controls rather than selecting anything.
    fn toggle_lighting_row(&mut self, row: LightingRow, cx: &mut Context<Self>) {
        match self.lighting_open.iter().position(|open| *open == row) {
            Some(index) => {
                self.lighting_open.remove(index);
            }
            None => self.lighting_open.push(row),
        }
        // A popover anchored to a control that just moved would be left
        // pointing at nothing.
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

    fn on_focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        set_focus_visible(true);
        window.focus_next();
        cx.notify();
    }

    fn on_focus_previous(
        &mut self,
        _: &FocusPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        set_focus_visible(true);
        window.focus_prev();
        cx.notify();
    }

    fn on_close_popover(&mut self, _: &ClosePopover, _: &mut Window, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            cx.notify();
        }
    }

    /// The navigation rail, as a card inset from the window it sits in.
    ///
    /// Paneflow's shape: the window's caption buttons at the top of the column,
    /// the destinations below them, and the utility entry pinned to a footer.
    /// Nothing names the product here. The window title carries the name, the
    /// Settings screen carries the qualifier, and a heading repeating either one
    /// would only push the first destination down the column.
    ///
    /// The card is a surface above the window's own, so the two separate by
    /// luminance rather than by a drawn divider, and it is inset far enough on
    /// every side that no corner of it has to know how the window is rounding
    /// its own.
    ///
    /// `reserved_top` is the strip the title bar is laid over. The card runs
    /// under it and starts its own content below, which is what puts the caption
    /// buttons inside the card rather than in a bar above it.
    fn rail(&self, reserved_top: Pixels, cx: &mut Context<Self>) -> Div {
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
            .bg(color::SURFACE.hsla())
            .rounded(CARD_RADIUS)
            // No outline: the card is told apart from the window by its
            // luminance, which is how Paneflow separates its own surfaces.
            .overflow_hidden()
            .pt(reserved_top)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    // The same inset the card keeps from the window, so an entry
                    // and the caption buttons above it sit on one edge.
                    .px(CARD_INSET)
                    .pt(CARD_INSET)
                    .children(primary_entries),
            )
            .child(div().flex_1())
            .child(
                // No divider either: the empty space above it is what sets the
                // utility entry apart from the destinations.
                div()
                    .flex()
                    .flex_col()
                    .px(CARD_INSET)
                    .pb(CARD_INSET)
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
        // The label color, which the icon takes too: an entry whose glyph and
        // word disagreed would read as a word with a decoration beside it.
        let ink = if selected {
            color::TEXT_ON_ACCENT.hsla()
        } else {
            color::TEXT.hsla()
        };
        // A menu row's fills, with one exception. Where a menu marks its current
        // value with a whisper, because choosing there has not happened yet, the
        // rail marks the destination the operator is standing in and takes the
        // accent for it. Everything else is the menu's: no fill at rest, and a
        // 5% wash under the pointer rather than a grey block.
        let resting = if selected {
            color::ACCENT.hsla()
        } else {
            color::TEXT.alpha(0.0)
        };
        let hovered = if selected {
            color::ACCENT_HOVER.hsla()
        } else {
            color::TEXT.alpha(0.05)
        };

        div()
            .id(SharedString::from(destination.label()))
            .tab_index(destination.tab_index())
            .tab_stop(true)
            .flex()
            .items_center()
            .gap(space::SM)
            .w_full()
            // The menu row's padding, but not its height: a menu row is 28 tall
            // because it is one of a list the pointer is already inside, and a
            // rail entry is a target the pointer travels to, so it keeps the
            // floor every pointer target in this interface keeps.
            .min_h(TARGET_MIN)
            .p(space::SM)
            .rounded(RADIUS)
            // The ring is reserved rather than added on focus, as it is on every
            // other control here. The menus add theirs, which is the one part of
            // their appearance not worth copying: added, it would grow the entry
            // by two pixels the moment it took focus.
            .border(FOCUS_RING)
            .border_color(color::SURFACE.alpha(0.0))
            .cursor_pointer()
            .text_xs()
            .text_color(ink)
            .bg(resting)
            .hover(|this| this.bg(hovered))
            .when(focus_visible(), |this| {
                this.focus(|this| this.border_color(color::FOCUS.hsla()))
            })
            .child(icon(destination.icon(), ICON_SIZE, ink))
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
                                .unit(DEGREE_C)
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
                                .unit(DEGREE_C)
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
                                    .unit(DEGREE_C)
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
        let pending = self.cooling.pending(kraken);
        let invalid = self.cooling.validation_error();

        // A program writes every channel it names, so it is refused unless all
        // of them are writable. The capability list is the daemon's own, so a
        // program that passes here is one the daemon would accept. With no
        // Apply button to disable, this is surfaced as a note instead: an
        // autosaving screen that silently saves nothing would be worse than one
        // that refuses out loud.
        let program_state = match &invalid {
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
        )
        .child(
            // The program on its own surface, above the channels it governs.
            // Which program is running and which profile selected it is one
            // question, and it is a different one from what each channel is
            // doing about it, so the two are separate cards rather than one
            // list with a heading.
            panel_surface().child(
                div()
                    .flex()
                    .flex_wrap()
                    // Every caption on one line, whichever of the two
                    // selects is the taller once a message sits under it.
                    .items_start()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .child(div().flex_none().w(COOLING_SELECT_WIDTH).child(self.select(
                        "cooling-mode",
                        "Mode",
                        Caption::Shown,
                        mode_options,
                        self.cooling.mode.value().to_string(),
                        // Choosing a mode is an edit like any other now, so
                        // it carries the same per-capability gate the
                        // profile selector beside it does, and for the same
                        // reason.
                        self.link.write_state(),
                        COOLING_TAB_MODE,
                        cx,
                        |shell, value, cx| {
                            if let Some(mode) = CoolingMode::from_value(value) {
                                shell.cooling.set_mode(mode);
                                shell.schedule_autosave(cx);
                            }
                        },
                    )))
                    .child(div().flex_none().w(COOLING_SELECT_WIDTH).child(self.select(
                        "profile",
                        "Active profile",
                        Caption::Shown,
                        profiles,
                        active,
                        // Activating a profile is a write. It is disabled
                        // for the same reasons every other write control is.
                        self.link.write_state(),
                        COOLING_TAB_PROFILE,
                        cx,
                        |shell, value, _| {
                            shell.feed.send(Command::ActivateProfile(value.to_string()));
                        },
                    ))),
            ),
        );

        for alert in self.link.alerts() {
            surface = surface.child(Note::new(NoteLevel::Critical, alert_message(alert)).render());
        }

        surface
            .child(
                // No heading: the two rows name themselves, and the freshness
                // the subtitle used to carry is on every reading already,
                // through the Stale and N/A qualifiers `readback` adds.
                row_panel()
                    .child(self.channel_row(Channel::Pump, &pump_write, cx))
                    .child(self.channel_row(Channel::Fan, &fan_write, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                // The only way back. An edit is already on the
                                // hardware, so this is not an undo: it puts the
                                // editor back on what the device reports it is
                                // running, which is what an operator needs after
                                // a refusal or after an edit they did not mean
                                // to make.
                                Button::new("cooling-revert", "Revert to hardware")
                                    .tab_index(COOLING_TAB_REVERT)
                                    .render()
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        let kraken = shell.kraken().cloned();
                                        shell.cooling.cancel(kraken.as_ref());
                                        shell.sent = None;
                                        shell.schedule_autosave(cx);
                                    })),
                            )
                            .child(
                                Button::new("cooling-save", "Save as profile")
                                    .state(self.link.write_state())
                                    .tab_index(COOLING_TAB_SAVE)
                                    .render()
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| shell.save_profile(cx)),
                                    ),
                            )
                            .child(self.delete_button(COOLING_TAB_DELETE, cx)),
                    )
                    // Opposite the buttons rather than after them: the status of
                    // a write is not a fourth action, and a sentence that shares
                    // a line with three buttons reads as a label for the last
                    // one. It is the only thing on its side of the line, so it
                    // appears and disappears without moving anything.
                    .children(pending.then(write_status)),
            )
            .children(
                program_state
                    .message()
                    .map(|reason| Note::new(NoteLevel::Warning, reason.to_string()).render()),
            )
            .children(self.outcome_note())
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

    /// One channel as a single line, with its controls one activation away.
    ///
    /// Collapsed, the line is readback only: what the channel is doing, from
    /// which temperature source, in what mode. Opening it reveals the controls
    /// for that channel alone, so the curve editor never has to ask which
    /// channel it is editing.
    fn channel_row(&self, channel: Channel, write: &ControlState, cx: &mut Context<Self>) -> Div {
        let now = self.now_unix_ms;
        let metrics = self.metrics.channel(channel);
        let rpm = metrics.rpm.view(now);
        let duty = metrics.duty.view(now);
        let mode = metrics.mode.view(now).copied();
        let confirmed_percent = self
            .kraken()
            .and_then(|kraken| kraken.channel(channel).duty_percent());
        let alerts: Vec<SafetyAlert> = self
            .link
            .channel_alerts(channel)
            .into_iter()
            .cloned()
            .collect();
        let open = self.cooling.is_expanded(channel);

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p(space::XS)
            // Between the line and what it opened. Only ever applies when a
            // detail is there, since a closed row has a single child.
            .gap(space::SM)
            .rounded(ROW_RADIUS)
            // The same open state the Lighting rows carry: one fill under the
            // line and the curve it revealed, so the two read as one channel.
            .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "cooling-row-{}",
                        channel.label().to_lowercase()
                    )))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .min_h(TARGET_MIN)
                    .px(space::SM)
                    // The head carries two lines, so it needs air of its own: at
                    // the pointer-target floor alone the pair would sit flush
                    // against the top and bottom of the row.
                    .py(space::XS)
                    .rounded(RADIUS)
                    // The ring is reserved rather than added on focus, so
                    // focusing a row does not move the line it sits on.
                    .border(FOCUS_RING)
                    .border_color(color::PANEL.alpha(0.0))
                    .cursor_pointer()
                    .tab_index(cooling_row_tab(channel))
                    .tab_stop(true)
                    .hover(|this| this.bg(color::CONTROL.alpha(0.5)))
                    // Shown to a keyboard user, who has no other way to know
                    // which row Enter would open, and withheld from a pointer
                    // user, who just aimed at it.
                    .when(focus_visible(), |this| {
                        this.focus(|this| this.border_color(color::FOCUS.hsla()))
                    })
                    .child(chevron(open, color::TEXT_MUTED.hsla()))
                    .child(
                        // Two lines where there was one: what the channel is,
                        // and what it has been told to do. The second line is
                        // the reported mode, not the pending edit, so a row that
                        // still says "Onboard" after a mode change is a write
                        // that has not landed rather than a stale label.
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_color(color::TEXT.hsla())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(channel.label()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color::TEXT_MUTED.hsla())
                                    .child(reported_program(mode, confirmed_percent)),
                            ),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .flex_none()
                            .min_w(COOLING_READBACK_WIDTH)
                            .child(readback("RPM", &rpm, |value| format!("{value:.0}"))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_w(COOLING_READBACK_WIDTH)
                            .child(readback("PWM", &duty_view(&duty), move |value| {
                                // The percentage comes from the reported duty,
                                // not from the pending edit: this line is
                                // readback, not intent.
                                match confirmed_percent {
                                    Some(percent) => format!("{value:.0} ({percent:.0}%)"),
                                    None => format!("{value:.0}"),
                                }
                            })),
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
                    }))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.cooling.toggle(channel);
                        // A popover anchored to the row that just moved would
                        // be left pointing at nothing.
                        shell.popover = None;
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(self.channel_detail(channel, write, cx))
            })
    }

    /// What the open row reveals: the curve, plus the stepper the mode uses.
    ///
    /// The curve is editable whenever the hardware accepts one, not only while
    /// the curve mode is selected: an edit sends nothing, and the first one
    /// selects that mode itself. What gates it is the channel's own curve
    /// capability, for the same reason the duty channels are gated separately.
    fn channel_detail(
        &self,
        channel: Channel,
        write: &ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let base = cooling_row_tab(channel);
        let curve_state =
            self.link
                .cooling_state(KRAKEN_BASE, channel.curve_capability(), self.now_unix_ms);

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::MD)
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .when(self.cooling.mode == CoolingMode::Fixed, |this| {
                this.child(self.duty_slider(channel, write, base + COOLING_OFFSET_DUTY, cx))
            })
            .child(self.curve_editor(channel, curve_state, base + COOLING_OFFSET_CURVE, cx))
    }

    /// The fixed duty of one channel, as a slider the pointer drags.
    ///
    /// A track rather than the pair of steppers this used to be. The value runs
    /// over a hundred settings and the steppers moved one of them per press, so
    /// reaching the other end of the range took thirty activations and the
    /// control that did it was three boxes and a trailing sentence on one line.
    /// The track is one object, it says where the value sits without being read,
    /// and it is the same control the Lighting rows already carry.
    ///
    /// The keyboard keeps the precision the steppers had: Left and Right move a
    /// single percentage point, which is exactly one setting on the device, and
    /// Home and End reach the ends of the accepted range in one press.
    fn duty_slider(
        &self,
        channel: Channel,
        write: &ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let target = self.cooling.duty(channel);
        let percent = duty_to_percent(target);
        let floor = duty_to_percent(channel.min_duty());
        let enabled = write.is_enabled();

        // Published per render and captured by this render's listeners, exactly
        // as the brightness tracks are, so a press converts against the
        // rectangle the track was just painted at. Only an operable track is
        // recorded, so a press cannot grab a channel the hardware refused.
        let sink: Option<Rc<dyn Fn(Bounds<Pixels>)>> = enabled.then(|| {
            let tracks = Rc::clone(&self.duty_tracks);
            Rc::new(move |bounds| tracks.record(channel, bounds)) as Rc<dyn Fn(Bounds<Pixels>)>
        });

        let mut slider = Slider::new(
            SharedString::from(format!("{}-duty", channel.label().to_lowercase())),
            "Fixed duty",
            f32::from(percent),
        )
        // In percent, not in raw duty: the driver stores a percentage, so a
        // track over 0-255 would offer two hundred and fifty-six positions for
        // a hundred settings and most moves would change nothing.
        .range(f32::from(floor), f32::from(MAX_DUTY_PERCENT))
        .unit("%")
        .state(write.clone())
        .tab_index(tab_index);
        if let Some(sink) = sink {
            slider = slider.bounds_sink(sink);
        }

        let control = slider.render().when(enabled, |slider| {
            slider.on_key_down(
                cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                    let steps = match event.keystroke.key.as_str() {
                        "left" | "down" => -1,
                        "right" | "up" => 1,
                        "home" => i16::from(floor) - i16::from(percent),
                        "end" => i16::from(MAX_DUTY_PERCENT) - i16::from(percent),
                        _ => return,
                    };
                    shell.cooling.adjust_duty(channel, steps);
                    shell.schedule_autosave(cx);
                }),
            )
        });

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::XS)
            .child(control)
            .child(
                // What the caption cannot hold: the byte the device is actually
                // being told, and the floor this channel refuses to go under.
                // Under the track rather than beside it, so the track keeps the
                // width it needs to be aimed at.
                div()
                    .font(numeric())
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(format!(
                        "{target}/{MAX_DUTY} {META_SEPARATOR} accepted {floor} to \
                         {MAX_DUTY_PERCENT}%",
                    )),
            )
    }

    /// The curve of one channel and everything that drives it.
    fn curve_editor(
        &self,
        channel: Channel,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let nodes = *self.cooling.curve(channel);
        let node = self.cooling.node(channel);
        let editable = state.is_enabled();
        let refusal = state.message().map(str::to_string);
        let liquid_c = self
            .kraken()
            .and_then(|kraken| kraken.liquid_temperature_c.copied());

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::SM)
            .children(refusal.map(|reason| {
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(reason)
            }))
            .child(
                CurveEditor::new(nodes)
                    .selected(node)
                    .state(state.clone())
                    .tab_index(tab_index)
                    .bounds_sink(Rc::clone(self.curve_bounds(channel)))
                    .liquid(liquid_c)
                    .render()
                    // The pointer part of the drag is not handled here. It is
                    // handled by the window, for the reason the brightness
                    // slider already is: a drag routinely leaves the control
                    // that started it, and a plot that only hears its own
                    // hitbox both stutters at the edge and never sees the
                    // release, which leaves the node stuck to the cursor.
                    .when(editable, |plot| {
                        plot.on_key_down(cx.listener(
                            move |shell, event: &gpui::KeyDownEvent, _, cx| {
                                let mut edited = false;
                                let handled = match event.keystroke.key.as_str() {
                                    "left" => {
                                        shell.cooling.step_node(channel, -1);
                                        true
                                    }
                                    "right" => {
                                        shell.cooling.step_node(channel, 1);
                                        true
                                    }
                                    "up" => {
                                        shell.cooling.adjust_node(channel, 1);
                                        edited = true;
                                        true
                                    }
                                    "down" => {
                                        shell.cooling.adjust_node(channel, -1);
                                        edited = true;
                                        true
                                    }
                                    _ => false,
                                };
                                if edited {
                                    shell.schedule_autosave(cx);
                                } else if handled {
                                    cx.notify();
                                }
                            },
                        ))
                    })
                    .into_any_element(),
            )
    }

    /// The channel whose plot a pointer press would edit, if any.
    ///
    /// Deliberately strict. The plot's rectangle is published during paint and
    /// keeps its last value afterwards, so the open row and the current screen
    /// are checked too: without them a press at the same coordinates on another
    /// screen would edit a curve nobody is looking at.
    fn curve_under(&self, position: gpui::Point<Pixels>) -> Option<Channel> {
        if self.destination != Destination::Cooling {
            return None;
        }
        // Every open row is asked, because more than one can be: the press
        // belongs to the plot it landed on, not to the row that opened last.
        self.cooling.expanded().iter().copied().find(|channel| {
            if !self
                .link
                .cooling_state(KRAKEN_BASE, channel.curve_capability(), self.now_unix_ms)
                .is_enabled()
            {
                return false;
            }
            let bounds = self.curve_bounds(*channel).get();
            if bounds.size.width <= px(0.0) {
                return false;
            }
            // The plot is inset inside the frame that carries the two axes. A
            // press on a label is not an edit, so it is ignored rather than
            // clamped onto the nearest node.
            bounds.dilate(px(6.0)).contains(&position)
        })
    }

    /// Where `channel`'s plot was last painted.
    fn curve_bounds(&self, channel: Channel) -> &Rc<Cell<Bounds<Pixels>>> {
        match channel {
            Channel::Pump => &self.pump_curve_bounds,
            Channel::Fan => &self.fan_curve_bounds,
        }
    }

    /// Grab the node nearest a pointer press and start a drag on it.
    fn grab_node_at(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(channel) = self.curve_under(position) else {
            return;
        };
        let (node, duty) = node_at(self.curve_bounds(channel).get(), position);
        let drag = CurveDrag {
            channel,
            node,
            base: *self.cooling.curve(channel),
        };
        self.curve_drag = Some(drag);
        self.cooling.select_node(channel, node);
        self.cooling.set_node_from(channel, drag.base, node, duty);
        self.touch_cooling();
        cx.notify();
    }

    /// Move the grabbed node to a pointer position.
    ///
    /// Only the height is read. The node is the one the press grabbed, so a
    /// gesture that wanders sideways keeps editing the point the operator
    /// aimed at instead of dragging a furrow across the plot. The pointer is
    /// not bounds-checked either: a drag that leaves the plot keeps pinning the
    /// edge it left by, which is what makes 0% and 100% reachable without
    /// landing exactly on the border.
    fn drag_node_to(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.curve_drag else {
            return;
        };
        let bounds = self.curve_bounds(drag.channel).get();
        if bounds.size.width <= px(0.0) {
            return;
        }
        let (_, duty) = node_at(bounds, position);
        let before = *self.cooling.curve(drag.channel);
        self.cooling
            .set_node_from(drag.channel, drag.base, drag.node, duty);
        // A pointer reports far more moves than a 255-step scale has values,
        // and a horizontal drag reports moves that change nothing at all.
        // Repainting the window for those is what makes the gesture feel heavy.
        if *self.cooling.curve(drag.channel) != before {
            self.touch_cooling();
            cx.notify();
        }
    }

    /// Put the daemon's committed program in the editor when it changes.
    ///
    /// This is what makes the Cooling screen open on the machine as it is
    /// rather than on the factory arrangement, and what shows the curve of a
    /// profile the operator just activated: an activation is the daemon
    /// committing a program like any other, so one rule covers both.
    fn adopt_committed_program(&mut self) {
        let editing = self.curve_drag.is_some()
            || self.duty_drag.is_some()
            || self.autosave_at.is_some()
            || self.sent.is_some();
        let Some(program) = program_to_adopt(
            self.link
                .status()
                .and_then(|status| status.cooling.as_ref()),
            self.committed.as_ref(),
            editing,
        ) else {
            return;
        };
        self.cooling.adopt(&program);
        self.committed = Some(program);
    }

    /// Note that the cooling edit changed, without scheduling anything.
    ///
    /// Used by the moves inside a drag: they arrive by the dozen and only the
    /// one the operator stops on is worth writing, so they push the deadline
    /// out and let the release schedule the save.
    fn touch_cooling(&mut self) {
        self.autosave_at = Some(Instant::now() + AUTOSAVE_QUIET);
    }

    /// Note the change and arrange for it to be written once things go quiet.
    ///
    /// Every edit spawns its own timer and every timer runs
    /// [`Shell::flush_autosave`], which does nothing unless the deadline it
    /// finds has passed. A later edit pushes that deadline out, so the earlier
    /// timers fire into nothing and the last one does the work. No task has to
    /// be cancelled and no edit can be lost by cancelling the wrong one.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        self.touch_cooling();
        cx.notify();
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(AUTOSAVE_QUIET).await;
            let _ = shell.update(cx, |shell, cx| shell.flush_autosave(cx));
        })
        .detach();
    }

    /// Write the pending edit, if it has been still long enough.
    ///
    /// The quiet period is not cosmetic. The daemon refuses a program that
    /// arrives within `MIN_PROGRAM_INTERVAL_MS` of the last write, because the
    /// firmware discards changes that come too fast. A screen that wrote on
    /// every pointer move would have most of its writes refused, and the value
    /// the operator actually stopped on could be one of them.
    fn flush_autosave(&mut self, cx: &mut Context<Self>) {
        // Both sliders count, not just the curve. A timer that fired while the
        // duty track was held wrote the value under the cursor and then wrote
        // again on release, which is two commands for one gesture and one duty
        // the operator never chose.
        let gesturing = self.curve_drag.is_some() || self.duty_drag.is_some();
        // Whichever reason holds the write back, the deadline is deliberately
        // left standing: the next refresh runs this again, and by then the
        // gesture has ended or the outcome has landed.
        if !cooling_write_is_due(
            self.autosave_at,
            Instant::now(),
            gesturing,
            self.sent.is_some(),
        ) {
            return;
        }
        self.autosave_at = None;

        // Nothing to write, or nothing the daemon would accept. A refusal is
        // already on screen in both cases, so this stays silent.
        if !self.cooling.pending(self.kraken()) || self.cooling.validation_error().is_some() {
            return;
        }
        self.apply(cx);
    }

    /// Send the pending program, remembering it until its outcome arrives.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let program = self.cooling.program();
        self.sent = Some(program.clone());
        self.feed.send(Command::Apply(program));
        cx.notify();
    }

    /// Store the pending state of the whole machine under a generated name.
    ///
    /// Cooling, lighting and the panel together, because that is what
    /// reactivating a profile puts back: a profile that carried only the curve
    /// would silently leave the lights and the glass on whatever the previous
    /// selection left there, and the operator would have no way to tell which
    /// half of the machine a name refers to.
    ///
    /// A channel whose color is mid-edit is left out rather than stored
    /// half-typed, exactly as it is left out of the write that settles, and the
    /// panel is stored only when this machine has one to write to.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let existing = self.link.profiles().len();
        let name = format!("{} {}", self.cooling.mode.label(), existing);
        let lighting = self
            .lighting
            .channels()
            .iter()
            .filter_map(|editor| {
                editor.program().ok().map(|program| LightingCommand {
                    channel: editor.channel,
                    program,
                })
            })
            .collect();
        let display = self
            .link
            .control_state(KRAKEN_BASE, CapabilityId::LcdFrame)
            .is_enabled()
            .then(|| self.lcd.preset().ok())
            .flatten();
        let profile = Profile {
            name: name.chars().take(48).collect(),
            program: self.cooling.program(),
            device: Some(KRAKEN_BASE),
            lighting,
            display,
        };
        self.feed.send(Command::SaveProfile(profile));
        cx.notify();
    }

    /// The result of the last command, as a note at the foot of the screen.
    ///
    /// Only when something went wrong. A confirmed write already tells the
    /// operator everything it has to: the readings move, the row's reported
    /// program catches up, and the pending state clears. Restating that in a
    /// banner after every edit is noise that trains the eye to skip the banner,
    /// which is the one place a refusal or an unconfirmed write has to be
    /// noticed.
    ///
    /// One note for a screen whose rows all write on their own, so the message
    /// has to name its device. `Channel 2: ...` and `Panel: ...` are what keep
    /// it from being read against whichever row the operator is looking at.
    fn outcome_note(&self) -> Option<Div> {
        let outcome = self.outcome.as_ref()?;
        let level = match outcome.severity {
            OutcomeSeverity::Confirmed => return None,
            OutcomeSeverity::Unconfirmed => NoteLevel::Critical,
            OutcomeSeverity::Refused => NoteLevel::Warning,
        };
        Some(Note::new(level, outcome.message.clone()).render())
    }

    /// The Lighting screen: one card per device, one row per addressable thing.
    ///
    /// The panel used to be a destination of its own, which put the two things
    /// an operator changes about the same machine's appearance two clicks
    /// apart. They are one screen now: the controller card lists its channels,
    /// the Kraken card carries its panel, and each row opens the controls that
    /// belong to it alone.
    fn lighting(&self, cx: &mut Context<Self>) -> Div {
        let channels: Vec<u8> = self
            .link
            .lighting_channels()
            .iter()
            .map(|state| state.channel)
            .collect();

        screen(
            "Lighting",
            "What each device shows: the controller's channels, and the panel on the Kraken.",
        )
        .child(self.controller_card(&channels, cx))
        .child(self.panel_card(channels.len(), cx))
        .children(self.outcome_note())
    }

    /// The controller and one row per channel it reported.
    fn controller_card(&self, channels: &[u8], cx: &mut Context<Self>) -> Div {
        let fixed = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        let card = Self::device_card(RGB_CONTROLLER_NAME);

        // No channel means the controller has not told this daemon what it is.
        // The reason the capability record carries is the whole content of the
        // card: there is nothing to control and nothing to pretend about.
        if channels.is_empty() {
            return card.child(
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
        }

        // A `map` closure would have to hold the mutable context borrow across
        // calls, which the 2024 edition's capture rules reject.
        let mut rows = Vec::with_capacity(channels.len());
        for (index, channel) in channels.iter().enumerate() {
            rows.push(self.channel_row_lighting(*channel, index, cx));
        }
        card.children(rows)
    }

    /// The Kraken and the row its panel occupies.
    ///
    /// Headed with what the cooler reported, unlike the controller: there is
    /// one panel and one pump, so the exact model is what tells the operator
    /// which cooler answered.
    fn panel_card(&self, channel_count: usize, cx: &mut Context<Self>) -> Div {
        let name = self
            .reported_name(KRAKEN_BASE)
            .unwrap_or_else(|| "Kraken".to_string());
        Self::device_card(&name).child(self.lcd_row(lcd_row_tab(channel_count), cx))
    }

    /// The product string a device reported, if it answered at all.
    fn reported_name(&self, device: DeviceId) -> Option<String> {
        self.link
            .device_rows()
            .into_iter()
            .find(|summary| summary.id == device)
            .map(|summary| summary.name)
    }

    /// The card one device gets: what it is, and its rows.
    ///
    /// `name` is what the card is headed with. Where it is a fixed string, the
    /// product string the device reported is deliberately not shown: that
    /// string carries a vendor wordmark this product does not use, and what the
    /// operator needs from the heading is which of the two devices this is. The
    /// reported string is still on the Devices panel, unchanged, which is where
    /// identifying the exact hardware belongs.
    ///
    /// The header carries the name alone. Firmware, kernel binding and state
    /// are all on the Devices panel, which is the screen for identifying
    /// hardware; repeating them over every card put a line of provenance above
    /// controls that are about appearance.
    fn device_card(name: &str) -> Div {
        let name = name.to_string();

        row_panel().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .w_full()
                .min_w_0()
                .child(
                    // Claims the line rather than being sized by its own text.
                    // Left to measure itself, the block collapsed to its
                    // narrowest column and set the device name one letter per
                    // line down the side of the card.
                    div()
                        .flex()
                        .flex_1()
                        .flex_col()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_color(color::TEXT.hsla())
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(name),
                        ),
                ),
        )
    }

    /// One lighting channel as a single line, with its controls one press away.
    ///
    /// Collapsed, the line still carries the two controls an operator reaches
    /// for most: how bright the channel is and what it is doing. Opening it
    /// reveals the color and the effect parameters for that channel alone.
    fn channel_row_lighting(&self, channel: u8, index: usize, cx: &mut Context<Self>) -> Div {
        let base = lighting_row_tab(index);
        let Some(editor) = self.lighting.channel(channel) else {
            return div();
        };

        let effects = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbEffects);
        // Every control on this row now writes by itself, so every one of them
        // carries this state. It is the capability the pending mode needs, not
        // a single "lighting is writable": an effect refused on a controller
        // that accepts a fixed color has to say so on the controls that would
        // have sent the effect.
        let write = if matches!(editor.mode, LightingMode::Effect(_)) {
            effects.clone()
        } else {
            self.link
                .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor)
        };

        // One line, not two. What is plugged in leads, because that is what the
        // operator is looking at inside the case, and the channel follows it as
        // a qualifier because that is what the write is addressed to.
        //
        // The accessory name is what the controller answered for the identifier
        // byte it returned, never a label this product chose. Nothing in that
        // answer says what kind of thing is on the channel, so nothing here
        // calls it one: a strip named "Fan 2" would be a fabricated fact sitting
        // next to a control that writes to real hardware.
        //
        // What the channel last had sent to it moved into the open detail with
        // the accessory summary. Both are sentences, and a line that also
        // carries a slider and a select has room for neither.
        let detected = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .filter(|state| !state.accessories.is_empty());
        let (title, qualifier) = match detected {
            Some(state) => (channel_headline(state), Some(format!("Channel {channel}"))),
            // With nothing detected the headline is already the channel, so the
            // qualifier would repeat it word for word.
            None => (format!("Channel {channel}"), None),
        };
        // The controller answers accessory identifiers, not what kind of thing
        // carries them, so the fan is drawn only where it answered something at
        // all. A channel that answered nothing gets an empty outline: the row
        // exists and can be written to, and nothing on it claims a fan is
        // plugged into it. Same rule as the headline right above, which stays
        // "Channel N" until the controller names something.
        let glyph = match detected {
            Some(_) => Icon::Windmill,
            None => Icon::CircleDashed,
        };

        // The thumbnail is what the channel is pending, so a color chosen and
        // not yet applied is visible on the collapsed line rather than only in
        // the open one. An unusable entry paints nothing instead of a value it
        // never held.
        let swatch = editor
            .mode
            .uses_color()
            .then(|| crate::components::parse_hex_color(&editor.color).ok())
            .flatten();
        let open = self.lighting_open.contains(&LightingRow::Channel(channel));
        let brightness = editor.brightness;
        let mode_value = editor.mode.value();

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p(space::XS)
            // Between the line and what it opened. Only ever applies when a
            // detail is there, since a closed row has a single child.
            .gap(space::SM)
            .rounded(ROW_RADIUS)
            // Open is a state the whole row carries, not a stack of two
            // elements that happen to touch: the line and what it revealed sit
            // on one fill, so the detail is read as the inside of this device
            // rather than as the top of the next one. Held at every state, so
            // opening a row does not move the line that opened it.
            .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    // Centered now that the controls carry no caption above
                    // them: with a label they hung off a shared baseline, and
                    // without one they are boxes of equal height beside a
                    // two-line name.
                    .items_center()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .py(space::SM)
                    // Only on the right: the head of the line carries its own
                    // padding on the left, and the last control would otherwise
                    // sit flush against the edge of the highlight.
                    .pr(space::SM)
                    .rounded(RADIUS)
                    // The whole line lights up, not just the part that opens
                    // it: the controls on the right belong to this device, and
                    // a highlight that stops before them reads as two rows.
                    // What the press does is still decided by what is under
                    // it, which is why only the head carries the handler.
                    .hover(|this| this.bg(color::CONTROL.alpha(0.5)))
                    .child(self.row_disclosure(
                        LightingRow::Channel(channel),
                        base,
                        row_thumbnail(swatch, glyph, false),
                        title,
                        qualifier.map(RowNote::Fragment),
                        cx,
                    ))
                    .child(self.brightness_slider(
                        LightingRow::Channel(channel),
                        brightness,
                        write.clone(),
                        base + ROW_OFFSET_BRIGHTNESS,
                        cx,
                    ))
                    .child(
                        div().flex_none().w(ROW_MODE_WIDTH).child(
                            self.select(
                                format!("lighting-mode-{channel}"),
                                "Mode",
                                Caption::Hidden,
                                LightingMode::all(effects.is_enabled())
                                    .into_iter()
                                    .map(|mode| SelectOption::new(mode.value(), mode.label()))
                                    .collect(),
                                mode_value,
                                write.clone(),
                                base + ROW_OFFSET_MODE,
                                cx,
                                move |shell, value, cx| {
                                    let Some(mode) = LightingMode::from_value(value) else {
                                        return;
                                    };
                                    let Some(editor) = shell.lighting.channel_mut(channel) else {
                                        return;
                                    };
                                    editor.mode = mode;
                                    shell.schedule_lighting(LightingRow::Channel(channel), cx);
                                },
                            ),
                        ),
                    ),
            )
            .when(open, |this| {
                this.child(self.channel_detail_lighting(channel, base, write, cx))
            })
    }

    /// What an open channel row reveals: its color and its effect parameters.
    ///
    /// No action. Every control here writes on its own once the row goes quiet,
    /// so a button repeating what the last edit already did would be a second
    /// way to send the same command.
    fn channel_detail_lighting(
        &self,
        channel: u8,
        base: isize,
        write: ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(editor) = self.lighting.channel(channel) else {
            return div();
        };
        // A pending program that cannot be built is the one refusal no control
        // on this row carries: each field names what is wrong with its own
        // value, and this names what is wrong with the combination. It is a
        // note rather than a disabled button, because there is no longer a
        // button to disable and nothing is sent while it stands.
        let program = editor.program();

        // What the controller was last told to show. The channel answers no
        // report that reads its program back, so this record is the only
        // evidence there is, and saying "sent" rather than "showing" is what
        // keeps it honest.
        //
        // It is also the standing confirmation the Apply button used to give by
        // changing its own label, and it is what US-014 asks the row to show as
        // the current confirmed mode. The panel row carries the same fact on
        // its line; a channel line already holds a slider and a select, so this
        // one lives in the detail.
        let sent = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .and_then(|state| state.committed.as_ref())
            .map(|program| format!("Last sent: {}", program.summary()))
            .unwrap_or_else(|| "Nothing sent to this channel yet".to_string());

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::MD)
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .child(
                div()
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(sent),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .when(editor.mode.uses_color(), |this| {
                        this.child(div().flex_none().w(FIELD_WIDTH).child(
                            self.lighting_color_field(
                                editor,
                                write.clone(),
                                base + CHANNEL_OFFSET_COLOR,
                                cx,
                            ),
                        ))
                    })
                    .when(editor.mode.uses_speed(), |this| {
                        this.child(
                            div().flex_none().w(FIELD_WIDTH).child(
                                self.select(
                                    format!("lighting-speed-{channel}"),
                                    "Speed",
                                    Caption::Shown,
                                    EffectSpeed::ALL
                                        .into_iter()
                                        .map(|speed| SelectOption::new(speed.key(), speed.label()))
                                        .collect(),
                                    editor.speed.key().to_string(),
                                    write.clone(),
                                    base + CHANNEL_OFFSET_SPEED,
                                    cx,
                                    move |shell, value, cx| {
                                        let Some(speed) = EffectSpeed::from_key(value) else {
                                            return;
                                        };
                                        let Some(editor) = shell.lighting.channel_mut(channel)
                                        else {
                                            return;
                                        };
                                        editor.speed = speed;
                                        shell.schedule_lighting(LightingRow::Channel(channel), cx);
                                    },
                                ),
                            ),
                        )
                    })
                    .when(editor.mode.uses_direction(), |this| {
                        this.child(
                            div().flex_none().w(FIELD_WIDTH).child(
                                self.select(
                                    format!("lighting-direction-{channel}"),
                                    "Direction",
                                    Caption::Shown,
                                    EffectDirection::ALL
                                        .into_iter()
                                        .map(|direction| {
                                            SelectOption::new(direction.key(), direction.label())
                                        })
                                        .collect(),
                                    editor.direction.key().to_string(),
                                    write.clone(),
                                    base + CHANNEL_OFFSET_DIRECTION,
                                    cx,
                                    move |shell, value, cx| {
                                        let Some(direction) = EffectDirection::from_key(value)
                                        else {
                                            return;
                                        };
                                        let Some(editor) = shell.lighting.channel_mut(channel)
                                        else {
                                            return;
                                        };
                                        editor.direction = direction;
                                        shell.schedule_lighting(LightingRow::Channel(channel), cx);
                                    },
                                ),
                            ),
                        )
                    }),
            )
            .children(
                program
                    .err()
                    .map(|error| Note::new(NoteLevel::Warning, error.to_string()).render()),
            )
    }

    /// The panel as a single line, with its editor one press away.
    fn lcd_row(&self, base: isize, cx: &mut Context<Self>) -> Div {
        let frame = self.link.control_state(KRAKEN_BASE, CapabilityId::LcdFrame);
        let editor = self.lcd.editor().clone();
        let panel = self
            .link
            .status()
            .and_then(|status| status.display.panel.clone());
        let confirmed = panel.is_some();
        let panel = panel.unwrap_or_else(crate::preview::assumed_panel);

        let subtitle = match self
            .link
            .status()
            .and_then(|status| status.display.committed.clone())
        {
            Some(preset) => format!(
                "{} at {}, {}%",
                preset.mode.label(),
                preset.orientation.label(),
                preset.brightness.percent()
            ),
            None if confirmed => {
                format!("{}x{} panel, nothing sent yet", panel.width, panel.height)
            }
            None => "no panel has answered".to_string(),
        };

        let open = self.lighting_open.contains(&LightingRow::Lcd);
        let background = crate::components::parse_hex_color(
            self.lcd.editor().color_text(DisplayColorField::Background),
        )
        .ok();
        let brightness = editor.brightness;

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p(space::XS)
            // Between the line and what it opened. Only ever applies when a
            // detail is there, since a closed row has a single child.
            .gap(space::SM)
            .rounded(ROW_RADIUS)
            // Open is a state the whole row carries, not a stack of two
            // elements that happen to touch: the line and what it revealed sit
            // on one fill, so the detail is read as the inside of this device
            // rather than as the top of the next one. Held at every state, so
            // opening a row does not move the line that opened it.
            .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    // Centered now that the controls carry no caption above
                    // them: with a label they hung off a shared baseline, and
                    // without one they are boxes of equal height beside a
                    // two-line name.
                    .items_center()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .py(space::SM)
                    // Only on the right: the head of the line carries its own
                    // padding on the left, and the last control would otherwise
                    // sit flush against the edge of the highlight.
                    .pr(space::SM)
                    .rounded(RADIUS)
                    // The whole line lights up, not just the part that opens
                    // it: the controls on the right belong to this device, and
                    // a highlight that stops before them reads as two rows.
                    // What the press does is still decided by what is under
                    // it, which is why only the head carries the handler.
                    .hover(|this| this.bg(color::CONTROL.alpha(0.5)))
                    .child(self.row_disclosure(
                        LightingRow::Lcd,
                        base,
                        row_thumbnail(background, Icon::Photo, true),
                        "LCD display".to_string(),
                        // The panel keeps a second line where a channel does
                        // not: what it is showing is a mode, an orientation and
                        // a brightness, which is a sentence rather than a
                        // fragment, and there is one panel row instead of a list
                        // of them to keep even.
                        Some(RowNote::Sentence(subtitle)),
                        cx,
                    ))
                    .child(self.brightness_slider(
                        LightingRow::Lcd,
                        brightness,
                        // Moving the slider is what sends the brightness now,
                        // so the panel's own refusal belongs on it. It used to
                        // sit on the Apply button alone, which left an operable
                        // control in front of a panel nothing can be written
                        // to.
                        frame.clone(),
                        base + ROW_OFFSET_BRIGHTNESS,
                        cx,
                    ))
                    .child(
                        div().flex_none().w(ROW_MODE_WIDTH).child(
                            self.select(
                                "lcd-mode",
                                "Display mode",
                                Caption::Hidden,
                                SCREEN_MODES
                                    .into_iter()
                                    .map(|mode| SelectOption::new(mode.key(), mode.label()))
                                    .collect(),
                                editor.mode.key().to_string(),
                                frame.clone(),
                                base + ROW_OFFSET_MODE,
                                cx,
                                |shell, value, cx| {
                                    let Some(mode) = DisplayMode::from_key(value) else {
                                        return;
                                    };
                                    shell.lcd.edit(|editor| editor.mode = mode);
                                    shell.schedule_lighting(LightingRow::Lcd, cx);
                                },
                            ),
                        ),
                    ),
            )
            .when(open, |this| {
                this.child(self.lcd_detail(base, &editor, &panel, frame, cx))
            })
    }

    /// What the open panel row reveals: the fields on the left, the frame the
    /// daemon would send on the right.
    fn lcd_detail(
        &self,
        base: isize,
        editor: &DisplayEditor,
        panel: &kori_core::capability::LcdPanel,
        frame: ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        // The preview ages with telemetry exactly as the panel does, from the
        // same samples the daemon renders against.
        let samples = match self.link.telemetry() {
            Some(snapshot) => self.lcd.preview().samples(snapshot),
            None => [
                MetricSample::unavailable(editor.metrics[0]),
                MetricSample::unavailable(editor.metrics[1]),
            ],
        };

        // A stopped stream is the one state on this screen an edit cannot
        // clear: the transfer failed without anything about the preset
        // changing, so nothing an automatic write watches has moved. It takes
        // a deliberate activation, which is what US-018 calls an explicit
        // recoverable state.
        let faulted = self
            .link
            .status()
            .and_then(|status| status.display.faulted.clone())
            .filter(|_| frame.is_enabled());

        // One grid, two columns wide: slot 1 on the left, slot 2 on the right,
        // one line per thing being chosen. Built here rather than inside the
        // element tree because a `map` closure would have to hold the mutable
        // context borrow across calls, which the 2024 edition's capture rules
        // reject.
        let mut lines: Vec<Div> = Vec::new();
        if editor.mode.uses_readings() {
            let mut controls = Vec::new();
            for slot in 0..editor.mode.reading_slots() {
                controls.push(self.metric_select(
                    slot,
                    frame.clone(),
                    base + LCD_OFFSET_METRIC_ONE + slot as isize,
                    cx,
                ));
            }
            lines.push(field_line(controls));
        }
        // The colors follow in the same grid, paired by role: the band of both
        // readings on one line, then what each fades to, then their text, then
        // the panel's own background. A line the current mode draws nothing for
        // is absent rather than empty.
        for (line, fields) in DisplayColorField::ALL.chunks(2).enumerate() {
            let mut controls = Vec::new();
            for (column, field) in fields.iter().enumerate() {
                if !field.is_used_by(editor.mode) {
                    continue;
                }
                let index = (line * 2 + column) as isize;
                controls.push(self.color_field(
                    *field,
                    frame.clone(),
                    base + LCD_OFFSET_COLOR_BASE + index,
                    cx,
                ));
            }
            if !controls.is_empty() {
                lines.push(field_line(controls));
            }
        }

        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(space::LG)
            .w_full()
            .min_w_0()
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    // Every field is the same width and every line the same
                    // gap, so the labels line up down both columns and the
                    // screen reads as one form rather than as three panels.
                    .gap(space::MD)
                    .children(lines)
                    .children(
                        editor
                            .preset()
                            .err()
                            .map(|error| Note::new(NoteLevel::Warning, error.to_string()).render()),
                    )
                    .children(faulted.map(|reason| {
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .pt(space::SM)
                            .gap(space::SM)
                            .child(
                                Button::new("lcd-resume", "Resume display")
                                    .variant(ButtonVariant::Primary)
                                    .tab_index(base + LCD_OFFSET_RESUME)
                                    .render()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        // Acting on the panel dismisses any
                                        // open popover, so a swatch list never
                                        // hides the result.
                                        this.popover = None;
                                        this.resume_display(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(color::WARNING.hsla())
                                    .child(format!("The panel stopped updating: {reason}")),
                            )
                    }))
                    .children(frame.message().map(|reason| {
                        div()
                            .text_xs()
                            .text_color(color::WARNING.hsla())
                            .child(reason.to_string())
                    })),
            )
            .child(
                // Fixed rather than sized to content, so the two field columns
                // beside it keep their width: at 920 logical pixels there is
                // room for both and the disc, and nothing has to be clipped to
                // decide which.
                div()
                    .flex_none()
                    .w(PREVIEW_COLUMN_WIDTH)
                    .child(crate::preview::panel_preview(
                        self.lcd.preview(),
                        &samples,
                        panel,
                    )),
            )
    }

    /// The head of a device row: chevron, thumbnail and two lines of text, as
    /// one target that opens the row.
    ///
    /// Only this part of the line toggles. The brightness and mode controls sit
    /// beside it rather than inside it, so operating one of them cannot also
    /// collapse the row it belongs to.
    ///
    /// `note` is the second fact the row carries, if it has one. See
    /// [`RowNote`] for which of the two shapes to give it.
    fn row_disclosure(
        &self,
        row: LightingRow,
        tab_index: isize,
        thumbnail: Div,
        title: String,
        note: Option<RowNote>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.lighting_open.contains(&row);
        let id = match row {
            LightingRow::Channel(channel) => format!("lighting-row-{channel}"),
            LightingRow::Lcd => "lighting-row-lcd".to_string(),
        };
        let (fragment, sentence) = match note {
            Some(RowNote::Fragment(text)) => (Some(text), None),
            Some(RowNote::Sentence(text)) => (None, Some(text)),
            None => (None, None),
        };

        div()
            .id(SharedString::from(id))
            .flex()
            .flex_1()
            .items_center()
            .gap(space::SM)
            .min_w(ROW_HEAD_MIN_WIDTH)
            .min_h(TARGET_MIN)
            .px(space::SM)
            .rounded(RADIUS)
            // The ring is reserved rather than added on focus, so focusing a
            // row does not move the line it sits on.
            .border(FOCUS_RING)
            .border_color(color::PANEL.alpha(0.0))
            .cursor_pointer()
            .tab_index(tab_index)
            .tab_stop(true)
            // Shown to a keyboard user, who has no other way to know which row
            // Enter would open, and withheld from a pointer user, who just
            // aimed at it.
            .when(focus_visible(), |this| {
                this.focus(|this| this.border_color(color::FOCUS.hsla()))
            })
            .child(chevron(open, color::TEXT_MUTED.hsla()))
            .child(thumbnail)
            .child(
                // Truncated rather than wrapped: a long product string is what
                // the device reported, and letting it wrap makes the line grow
                // until it no longer reads as one row per device.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(space::SM)
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(color::TEXT.hsla())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            // The fragment gives up the width first: the name
                            // is what the row is, and losing the last word of
                            // "Channel 1" costs less than losing the last word
                            // of what is plugged into it.
                            .children(fragment.map(|fragment| {
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_baseline()
                                    .gap(space::SM)
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(color::TEXT_DISABLED.hsla())
                                            .child(META_SEPARATOR),
                                    )
                                    .child(
                                        div().text_color(color::TEXT_MUTED.hsla()).child(fragment),
                                    )
                            })),
                    )
                    .children(sentence.map(|sentence| {
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(sentence)
                    })),
            )
            .on_click(cx.listener(move |shell, _, _, cx| shell.toggle_lighting_row(row, cx)))
    }

    /// The brightness a row carries on its line, as a slider the pointer drags.
    ///
    /// A real slider rather than a pair of buttons: the track publishes the
    /// rectangle it was painted at, the press captures that rectangle, and
    /// every later move is converted against it. Capturing it is what lets a
    /// drag leave the slider and keep working, and what keeps a second row's
    /// slider from answering for the one under the cursor.
    ///
    /// The keyboard reaches the same values: Left and Right move one step,
    /// Home and End go to the ends. A control that only a pointer can operate
    /// would fail the screen's keyboard-only gate.
    fn brightness_slider(
        &self,
        row: LightingRow,
        value: u8,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = state.is_enabled();
        let max = f32::from(kori_core::lighting::MAX_BRIGHTNESS);
        // Created per render and captured by this render's listeners, so the
        // rectangle a press reads is the one the track was just painted at.
        // Only an operable track is published, so a press cannot grab a slider
        // the hardware has refused.
        let sink: Option<Rc<dyn Fn(Bounds<Pixels>)>> = enabled.then(|| {
            let tracks = Rc::clone(&self.tracks);
            Rc::new(move |bounds| tracks.record(row, bounds)) as Rc<dyn Fn(Bounds<Pixels>)>
        });

        let mut slider = Slider::new(
            SharedString::from(format!("brightness-{}", row.key())),
            "Brightness",
            f32::from(value),
        )
        .range(0.0, max)
        .unit("%")
        .icons(Icon::SunLow, Icon::SunHigh)
        // The row names the device and the two glyphs name the axis, so the
        // caption would only repeat the line above it.
        .label_hidden()
        .state(state)
        .tab_index(tab_index);
        if let Some(sink) = sink {
            slider = slider.bounds_sink(sink);
        }

        let control = slider.render().when(enabled, |slider| {
            slider.on_key_down(
                cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                    let step = i16::from(crate::lighting::BRIGHTNESS_STEP);
                    let next = match event.keystroke.key.as_str() {
                        "left" | "down" => i16::from(value) - step,
                        "right" | "up" => i16::from(value) + step,
                        "home" => 0,
                        "end" => i16::from(kori_core::lighting::MAX_BRIGHTNESS),
                        _ => return,
                    };
                    shell.popover = None;
                    shell.set_brightness(row, next);
                    // A held arrow key arrives by the dozen and each press
                    // pushes the deadline out, so the value the operator stops
                    // on is the one that is written.
                    shell.schedule_lighting(row, cx);
                }),
            )
        });

        div().flex_none().w(ROW_BRIGHTNESS_WIDTH).child(control)
    }

    /// Move the dragged row's brightness to a pointer position.
    fn drag_brightness(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.brightness_drag else {
            return;
        };
        if drag.track.size.width <= px(0.0) {
            return;
        }
        let max = f32::from(kori_core::lighting::MAX_BRIGHTNESS);
        let value = Slider::value_at(drag.track, position, 0.0, max);
        self.set_brightness(drag.row, value.round() as i16);
        cx.notify();
    }

    /// Move the dragged channel's fixed duty to a pointer position.
    ///
    /// Read as whole percent for the reason `node_at` reads the plot that way:
    /// the driver stores a percentage, so a finer reading would offer positions
    /// the hardware cannot hold and would let a pixel of hand tremor produce a
    /// write that changes nothing.
    fn drag_duty(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.duty_drag else {
            return;
        };
        if drag.track.size.width <= px(0.0) {
            return;
        }
        let floor = f32::from(duty_to_percent(drag.channel.min_duty()));
        let percent = Slider::value_at(drag.track, position, floor, f32::from(MAX_DUTY_PERCENT));
        self.cooling
            .set_duty(drag.channel, duty_from_percent(percent.round() as u8));
        cx.notify();
    }

    /// Set one row's pending brightness, clamped to what the daemon accepts.
    ///
    /// Takes a signed value so a keyboard step past either end arrives here to
    /// be clamped rather than wrapping around in the caller.
    fn set_brightness(&mut self, row: LightingRow, percent: i16) {
        let percent = percent.clamp(0, i16::from(kori_core::lighting::MAX_BRIGHTNESS)) as u8;
        match row {
            LightingRow::Channel(channel) => {
                if let Some(editor) = self.lighting.channel_mut(channel) {
                    editor.set_brightness(percent);
                }
            }
            LightingRow::Lcd => self.lcd.edit(|editor| editor.set_brightness(percent)),
        }
    }

    /// The metric select for one reading slot.
    fn metric_select(
        &self,
        slot: usize,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = self.lcd.editor().metrics[slot];
        // The metric line heads the grid and the colors under it are named by
        // role, so the slot alone is enough to say what this select chooses.
        let (id, label) = if slot == 0 {
            ("lcd-metric-1", "Reading 1")
        } else {
            ("lcd-metric-2", "Reading 2")
        };
        self.select(
            id,
            label,
            Caption::Shown,
            LcdMetric::ALL
                .into_iter()
                .map(|metric| SelectOption::new(metric.key(), metric.label()))
                .collect(),
            selected.key().to_string(),
            state,
            tab_index,
            cx,
            move |shell, value, cx| {
                let Some(metric) = LcdMetric::from_key(value) else {
                    return;
                };
                shell.lcd.edit(|editor| editor.metrics[slot] = metric);
                shell.schedule_lighting(LightingRow::Lcd, cx);
            },
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
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        caption: Caption,
        options: Vec<SelectOption>,
        selected: String,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> Div {
        // The identifier is owned rather than borrowed: a screen with one
        // select per channel has to name them apart, and the number of channels
        // is whatever the controller reported.
        let id: SharedString = id.into();
        // GPUI has no disabled semantics of its own: a handler left attached
        // still fires. Withholding it is what makes the refusal real rather
        // than a matter of styling.
        let enabled = state.is_enabled();
        let open = enabled && self.popover == Some(Popover::Options { select: id.clone() });
        let current = selected.clone();
        let mut built = Select::new(id.clone(), label);
        if caption == Caption::Hidden {
            built = built.label_hidden();
        }
        let control = built
            .options(options.clone())
            .selected(selected)
            .state(state)
            .tab_index(tab_index)
            .render()
            .when(enabled, |this| {
                let id = id.clone();
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(Popover::Options { select: id.clone() }, cx)
                }))
            });

        let on_select = Rc::new(on_select);

        div().relative().child(control).when(open, |this| {
            this.child(option_menu(
                div()
                    .flex()
                    .flex_col()
                    .gap(MENU_ROW_GAP)
                    .children(options.into_iter().map(|option| {
                        let value = option.value.clone();
                        let chosen = option.value == current;
                        let on_select = Rc::clone(&on_select);
                        // Whisper highlights, the selected row a step stronger
                        // than a hovered one: a menu that marks its current
                        // value with the selection accent reads as if choosing
                        // had already happened.
                        let resting = if chosen {
                            color::TEXT.alpha(0.10)
                        } else {
                            color::TEXT.alpha(0.0)
                        };
                        let hovered = if chosen {
                            color::TEXT.alpha(0.10)
                        } else {
                            color::TEXT.alpha(0.05)
                        };
                        div()
                            .id(SharedString::from(format!("{id}-{}", option.value)))
                            .tab_index(tab_index)
                            .tab_stop(true)
                            .flex_none()
                            .w_full()
                            .h(MENU_ROW_HEIGHT)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .px(space::SM)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .text_xs()
                            .text_color(color::TEXT.hsla())
                            .bg(resting)
                            .hover(|this| this.bg(hovered))
                            .when(focus_visible(), |this| {
                                this.focus(|this| {
                                    this.border(FOCUS_RING).border_color(color::FOCUS.hsla())
                                })
                            })
                            .child(div().flex_1().min_w_0().truncate().child(option.label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                on_select(this, &value, cx);
                                this.popover = None;
                                cx.notify();
                            }))
                    })),
            ))
        })
    }

    /// Note one Lighting row's edit and arrange for it to be written.
    ///
    /// The same shape as [`Shell::schedule_autosave`], kept per row: every edit
    /// spawns its own timer, every timer drains whatever has come due, and a
    /// later edit on the same row pushes that row's deadline out so the earlier
    /// timers fire into nothing. No task has to be cancelled and no edit can be
    /// lost by cancelling the wrong one.
    fn schedule_lighting(&mut self, row: LightingRow, cx: &mut Context<Self>) {
        self.lighting_due
            .touch(row, Instant::now() + LIGHTING_QUIET);
        cx.notify();
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(LIGHTING_QUIET).await;
            let _ = shell.update(cx, |shell, cx| shell.flush_lighting(cx));
        })
        .detach();
    }

    /// Write every Lighting row that has been still long enough.
    ///
    /// The quiet period is not cosmetic. The controller acknowledges nothing,
    /// so the daemon refuses a command arriving within `MIN_COMMAND_INTERVAL_MS`
    /// of the last one on that channel rather than queueing it, and it keeps no
    /// last-value-wins. A screen that wrote on every pointer move would have
    /// most of its writes refused, and the value the operator stopped on could
    /// be one of them.
    fn flush_lighting(&mut self, cx: &mut Context<Self>) {
        for row in self.lighting_due.take_due(Instant::now()) {
            // A gesture still in progress keeps its deadline. Letting go is
            // what says which value the operator meant.
            if self.brightness_drag.map(|drag| drag.row) == Some(row) {
                self.lighting_due
                    .touch(row, Instant::now() + LIGHTING_QUIET);
                continue;
            }
            match row {
                LightingRow::Channel(channel) => self.send_lighting(channel),
                LightingRow::Lcd => self.send_display(),
            }
        }
        cx.notify();
    }

    /// Send one channel's pending program, unless it is already showing it.
    ///
    /// Nothing is sent when the program cannot be built. The control holding
    /// the unusable value already names the problem, and a command the daemon
    /// would refuse would answer it with a sentence about the channel instead.
    ///
    /// The comparison against what the daemon committed does not replace the
    /// daemon's own deduplication, which still runs and is still the authority.
    /// It is what keeps an edit that lands back on the current value from
    /// costing a request, a reply and a polling cycle.
    fn send_lighting(&mut self, channel: u8) {
        let Some(program) = self
            .lighting
            .channel(channel)
            .and_then(|editor| editor.program().ok())
        else {
            return;
        };
        let committed = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .and_then(|state| state.committed.as_ref());
        if committed == Some(&program) {
            return;
        }
        self.feed
            .send(Command::ApplyLighting(LightingCommand { channel, program }));
    }

    /// Send the panel's pending preset, unless it is already showing it.
    ///
    /// Deduplicating here matters more than it does for a channel: an unchanged
    /// preset still costs the daemon a full render before it can compare the
    /// picture it produced.
    fn send_display(&mut self) {
        let Ok(preset) = self.lcd.preset() else {
            return;
        };
        let committed = self
            .link
            .status()
            .and_then(|status| status.display.committed.as_ref());
        if committed == Some(&preset) {
            return;
        }
        self.feed.send(Command::ApplyDisplay(preset));
    }

    /// Send the panel's preset whatever it is showing, to restart a stopped
    /// stream.
    ///
    /// The one write on this screen that is still a deliberate activation. A
    /// faulted stream is the state no edit can clear: the transfer failed
    /// without anything about the preset changing, so there is nothing for an
    /// automatic write to notice.
    fn resume_display(&mut self, cx: &mut Context<Self>) {
        if let Ok(preset) = self.lcd.preset() {
            self.feed.send(Command::ApplyDisplay(preset));
        }
        cx.notify();
    }

    /// One channel's color field plus its swatch popover.
    ///
    /// The field carries the capability refusal itself now that choosing a
    /// swatch is what reaches the controller. A picker that opens over a
    /// channel nothing can be written to would be a control that acts and
    /// changes nothing on the hardware.
    fn lighting_color_field(
        &self,
        editor: &crate::lighting::ChannelEditor,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let channel = editor.channel;
        let enabled = state.is_enabled();
        let open = enabled && self.popover == Some(Popover::LightingSwatches { channel });
        let id = SharedString::from(format!("lighting-color-{channel}"));

        // `ColorField` renders its own error state, so a stored color that
        // cannot be parsed names the problem on the field itself. A refused
        // capability outranks it: correcting the digits would not make the
        // write happen.
        let control = ColorField::new(id.clone(), "Color", editor.color.clone())
            .state(state)
            .tab_index(tab_index)
            .render()
            // GPUI has no disabled semantics of its own: a handler left
            // attached still fires, so withholding it is what makes the
            // refusal real rather than a matter of styling.
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(Popover::LightingSwatches { channel }, cx)
                }))
            });

        div().relative().child(control).when(open, |this| {
            this.child(swatch_menu(swatch_grid(
                SWATCHES
                    .into_iter()
                    .enumerate()
                    .map(|(index, swatch)| {
                        swatch_cell(format!("{id}-swatch-{index}"), swatch, tab_index)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let Some(editor) = this.lighting.channel_mut(channel) else {
                                    return;
                                };
                                editor.color = format!("{:06X}", swatch.0);
                                this.popover = None;
                                this.schedule_lighting(LightingRow::Channel(channel), cx);
                            }))
                            .into_any_element()
                    })
                    .collect(),
            )))
        })
    }

    /// A color field plus its swatch popover.
    ///
    /// The field shows the digits as typed, so an entry the operator has not
    /// finished keeps its own error rather than being replaced by a value it
    /// never held. It carries the panel's refusal for the same reason the
    /// channel fields carry the controller's: choosing a swatch is what sends
    /// the frame.
    fn color_field(
        &self,
        field: DisplayColorField,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = state.is_enabled();
        let open = enabled && self.popover == Some(Popover::Swatches { field });
        let id = SharedString::from(format!("lcd-color-{}", field.key()));

        // The full name: nothing beside the field carries the slot any more,
        // so the label is the only thing that tells the two columns apart.
        let control = ColorField::new(
            id.clone(),
            field.label(),
            self.lcd.editor().color_text(field),
        )
        .state(state)
        .tab_index(tab_index)
        .render()
        .when(enabled, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_popover(Popover::Swatches { field }, cx)
            }))
        });

        div().relative().child(control).when(open, |this| {
            this.child(swatch_menu(swatch_grid(
                SWATCHES
                    .into_iter()
                    .enumerate()
                    .map(|(index, swatch)| {
                        swatch_cell(format!("{id}-swatch-{index}"), swatch, tab_index)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.lcd.edit(|editor| {
                                    editor.set_color_text(field, format!("{:06X}", swatch.0))
                                });
                                this.popover = None;
                                this.schedule_lighting(LightingRow::Lcd, cx);
                            }))
                            .into_any_element()
                    })
                    .collect(),
            )))
        })
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Each frame republishes the tracks it paints. Clearing here is what
        // keeps a row that has gone away, or a screen that no longer shows one,
        // from leaving a rectangle behind that a press could still grab.
        self.tracks.clear();
        self.duty_tracks.clear();

        let content = match self.destination {
            Destination::Monitoring => self.monitoring(),
            Destination::Cooling => self.cooling(cx),
            Destination::Lighting => self.lighting(cx),
            Destination::Settings => self.settings(),
        };

        let title_bar = window_chrome::title_bar(window, &self.window_drag);
        // The bar is laid over the top of the window, so everything under it
        // starts below the strip it occupies. The card and the work surface
        // already begin one gap down, which is that much less to reserve.
        let reserved_top = window_chrome::title_bar_height(window) - CARD_INSET;

        let shell = div()
            .id("shell")
            .track_focus(&self.focus)
            // Any press anywhere in the window means focus is about to move by
            // pointer, so the ring is dropped before the control under the
            // cursor renders with it. Capture phase, so it runs ahead of that
            // control's own handler rather than after it. Enter and Space reach
            // a control through the click listeners without a mouse event, so
            // keyboard activation leaves the ring alone.
            .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if focus_visible() {
                    set_focus_visible(false);
                    cx.notify();
                }
                // An open popover owns the press: it is being dismissed, and a
                // press that lands over a slider or a curve on the way out must
                // not also move a value the operator was not aiming at.
                if this.popover.is_some() {
                    return;
                }
                // A press on a brightness track starts a drag. It is
                // decided here, from where the tracks were painted, rather
                // than by a listener on the slider: an event that has to
                // reach a control nested under its own interactive state
                // never arrived, and the capture phase on the window is the
                // one place it always does.
                if event.button == MouseButton::Left
                    && let Some((row, track)) = this.tracks.at(event.position)
                {
                    this.popover = None;
                    this.brightness_drag = Some(BrightnessDrag { row, track });
                    this.drag_brightness(event.position, cx);
                }
                // A fixed-duty track is grabbed from the same place, for the
                // same reason, and against its own book: the two screens are
                // never on screen together, but a rectangle left behind by one
                // must not be reachable from the other.
                if event.button == MouseButton::Left
                    && let Some((channel, track)) = this.duty_tracks.at(event.position)
                {
                    this.popover = None;
                    this.duty_drag = Some(DutyDrag { channel, track });
                    this.drag_duty(event.position, cx);
                }
                // A press on the curve plot starts a node drag, decided from
                // the same place and for the same reason.
                if event.button == MouseButton::Left {
                    this.grab_node_at(event.position, cx);
                }
            }))
            // A brightness drag is tracked by the window rather than by the
            // slider: the pointer routinely leaves a 200-pixel control while
            // still holding it, and a drag that stopped at the edge would leave
            // the value wherever the cursor crossed it.
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.brightness_drag.is_some() {
                    this.drag_brightness(event.position, cx);
                }
                if this.duty_drag.is_some() {
                    this.drag_duty(event.position, cx);
                }
                this.drag_node_to(event.position, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    // Nothing is written while the pointer holds the track: a
                    // drag names dozens of values and only the one it stops on
                    // is worth a command.
                    if let Some(drag) = this.brightness_drag.take() {
                        this.schedule_lighting(drag.row, cx);
                    }
                    // Releasing ends the gesture wherever the pointer is. A
                    // release the plot never heard is what used to leave a node
                    // following the cursor with no button held.
                    let released = this.duty_drag.take().is_some();
                    if this.curve_drag.take().is_some() || released {
                        // The gesture is over, so the value it stopped on is
                        // the one worth writing.
                        this.schedule_autosave(cx);
                    }
                }),
            )
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
            .on_action(
                cx.listener(|this, _: &GoSettings, _, cx| this.go(Destination::Settings, cx)),
            )
            .size_full()
            .flex()
            // The gap and the padding are what make the rail read as a card
            // laid on the window rather than as a column cut out of it.
            .gap(CARD_INSET)
            .p(CARD_INSET)
            .text_color(color::TEXT.hsla())
            .text_sm()
            .child(self.rail(reserved_top, cx))
            .child(
                div()
                    .flex_1()
                    // Without this the rail plus an unwrapped sentence can
                    // exceed the window width instead of wrapping.
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    // The caption strip carries nothing on this side, and the
                    // band is reserved anyway: it is the window's own bar, so
                    // it stays a place to drag the window from rather than a
                    // place a heading can reach. Rigid and outside the scroll,
                    // as Paneflow reserves it, so a scrolled screen passes
                    // under nothing.
                    .child(div().flex_none().h(reserved_top))
                    .child(
                        div()
                            .id("work-surface")
                            .flex_1()
                            // A flex child floors at its content height unless
                            // it is told it may shrink, which is what turns the
                            // overflow below into a scroll instead of a spill.
                            .min_h_0()
                            // No fill: the window's own surface is the ground
                            // the panels and the rail card sit on.
                            .overflow_y_scroll()
                            // One gap of its own on top of the row's, so a
                            // screen keeps a little more air than the rail card
                            // takes.
                            .px(space::SM)
                            .pb(space::SM)
                            .flex()
                            .flex_col()
                            .gap(space::LG)
                            .children(self.banner())
                            .child(content),
                    ),
            );

        // The chrome color fills the window, so the transparent title bar, the
        // work surface and all four corners read as one ground. The bar is laid
        // over the shell rather than stacked above it: that is what puts the
        // caption buttons inside the rail card while every pixel across the top
        // of the window stays a place to drag it from.
        window_chrome::window_shell(
            div()
                .relative()
                .size_full()
                .child(shell)
                // While a list is open, the rest of the window is a way out of
                // it. Painted under the popover and over everything else, so a
                // press anywhere else closes the list and stops there: the
                // control underneath is not also operated by the press that
                // dismissed the list, which is how a menu behaves everywhere
                // else on the desktop. Escape does the same from the keyboard.
                .children(self.popover.is_some().then(|| {
                    div()
                        .id("popover-dismiss")
                        // Takes the press rather than letting it through, so
                        // the control that opened the list does not reopen it
                        // on the release and no other control is operated by
                        // the press that closed it.
                        .occlude()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.popover = None;
                                cx.notify();
                            }),
                        )
                }))
                .child(div().absolute().top_0().left_0().w_full().child(title_bar)),
            window,
            color::RAIL.hsla(),
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

/// The appearance thumbnail at the head of a device row.
///
/// The fill is still the color, because that is the one thing on the collapsed
/// line that says what the channel is pending. The glyph on top says what the
/// row drives, which a bare rectangle never did: a list of identical squares
/// separated only by hue reads as a palette rather than as hardware.
///
/// `None` is a color the row cannot show: a channel that is off, or an entry
/// the operator has not finished typing. It paints the empty surface rather
/// than a black swatch, because black is also a color a channel can be set to,
/// and dims the glyph to match, so an unset row is quiet rather than absent.
///
/// The ink is chosen against the fill rather than fixed: a white mark vanishes
/// on a pale yellow and a dark one vanishes on the black the panel starts at.
/// [`Color::readable_ink`] carries the measurement.
fn row_thumbnail(color_value: Option<Color>, glyph: Icon, round: bool) -> Div {
    let (fill, ink) = match color_value {
        Some(value) => (value, value.readable_ink()),
        None => (color::SURFACE, color::TEXT_DISABLED),
    };
    div()
        .flex_none()
        .w(ROW_THUMBNAIL)
        .h(ROW_THUMBNAIL)
        .flex()
        .items_center()
        .justify_center()
        .rounded(if round { ROW_THUMBNAIL / 2.0 } else { RADIUS })
        .bg(fill.hsla())
        .child(icon(glyph, ROW_THUMBNAIL_GLYPH, ink.hsla()))
}

/// How many swatches a color list puts on one line.
pub const SWATCH_COLUMNS: usize = 3;

/// One offered color, without what choosing it does.
///
/// Shared by the two lists that offer colors, so a swatch in the panel's list
/// and one in a channel's are the same object rather than two that happen to
/// match today.
fn swatch_cell(id: String, swatch: Color, tab_index: isize) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .tab_index(tab_index)
        .tab_stop(true)
        .flex_none()
        .w(SWATCH_SIZE)
        .h(SWATCH_SIZE)
        .rounded(SWATCH_RADIUS)
        .cursor_pointer()
        // Nothing outlines the color at rest: the swatch is the color and an
        // edge around it is one more thing to read. The ring is reserved rather
        // than added, so pointing at a swatch or focusing it does not move the
        // row it sits in.
        .border(FOCUS_RING)
        .border_color(color::PANEL.alpha(0.0))
        .bg(swatch.hsla())
        .hover(|this| this.border_color(color::FOCUS.alpha(0.6)))
        .when(focus_visible(), |this| {
            this.focus(|this| this.border_color(color::FOCUS.hsla()))
        })
}

/// The offered colors, [`SWATCH_COLUMNS`] to a line.
///
/// Laid out in fixed rows rather than wrapped on the available width, so the
/// shape of the list is the same wherever it opens and however wide the menu
/// around it happens to be.
fn swatch_grid(mut swatches: Vec<AnyElement>) -> Div {
    let mut rows = Vec::new();
    while !swatches.is_empty() {
        let taken = SWATCH_COLUMNS.min(swatches.len());
        let line: Vec<AnyElement> = swatches.drain(..taken).collect();
        rows.push(div().flex().gap(space::SM).children(line));
    }
    div().flex().flex_col().gap(space::SM).children(rows)
}

/// One line of the panel editor's grid.
///
/// Each control keeps the same fixed width whatever it holds, so the second
/// column starts at the same offset on every line and a line that carries one
/// control leaves the other column empty instead of stretching across it.
fn field_line(controls: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap(space::MD).children(
        controls
            .into_iter()
            .map(|control| div().flex_none().w(FIELD_WIDTH).child(control)),
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

/// The one moment a cooling edit is in flight, on its own mark.
///
/// Visible between the edit settling and the daemon confirming it, and gone
/// after. A reading that stays put means a write that did not land, which is the
/// only thing on this line worth looking at, so it carries a fill and a dot
/// rather than being a fourth gray sentence beside three buttons. The fill is
/// the neutral control color, not the warning one: this is the normal path of
/// every edit, and a screen that flashes amber on success teaches its operator
/// to ignore amber.
fn write_status() -> Div {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .rounded(RADIUS)
        .bg(color::CONTROL.alpha(0.6))
        .child(
            div()
                .flex_none()
                .w(STATUS_DOT)
                .h(STATUS_DOT)
                .rounded(STATUS_DOT / 2.0)
                .bg(color::ACCENT.hsla()),
        )
        .child(
            div()
                .min_w_0()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child("Saving. The hardware holds its previous program until this clears."),
        )
}

/// What the device says a channel is running, in the words of the mode select.
///
/// Read back from the hardware rather than taken from the pending edit. A row
/// that still names the previous program after a mode change is a write that has
/// not landed yet, and that is exactly what this line has to be able to say.
/// The failsafe mode is named without a percentage on purpose. The kernel calls
/// it "100% failsafe", but the duty read back beside this line is whatever the
/// firmware is actually running, and a label claiming 100% next to a reading of
/// 52% states a number the hardware is not at.
fn reported_program(mode: Option<PwmMode>, percent: Option<f32>) -> String {
    match (mode, percent) {
        (None, _) => "Mode not reported".to_string(),
        (Some(PwmMode::FullSpeed), _) => "Firmware failsafe".to_string(),
        (Some(PwmMode::Fixed), Some(percent)) => format!("Fixed duty {percent:.0}%"),
        (Some(PwmMode::Fixed), None) => "Fixed duty".to_string(),
        (Some(PwmMode::Curve), _) => "Onboard curve".to_string(),
    }
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

/// One sentence for a safety alert, ready to render.
pub fn alert_message(alert: &SafetyAlert) -> String {
    alert.message()
}

/// Whether a scheduled cooling write should leave now.
///
/// Free and pure, so the rule can be exercised without a window. Each of the
/// three reasons to hold a write back cost a real defect to find: a deadline a
/// later edit moved, a gesture the pointer has not released, and a write whose
/// outcome has not come back yet. The Lighting screen keeps the same three in
/// [`WriteSchedule`] and [`Shell::flush_lighting`].
pub fn cooling_write_is_due(
    deadline: Option<Instant>,
    now: Instant,
    gesturing: bool,
    in_flight: bool,
) -> bool {
    deadline.is_some_and(|deadline| now >= deadline) && !gesturing && !in_flight
}

/// The committed program a fresh status should put in the editor, if any.
///
/// Free and pure, for the same reason [`cooling_write_is_due`] is. `seen` is
/// what the last adoption took, so a status repeating itself adopts nothing and
/// a screen the operator is reading does not twitch once a second.
///
/// Nothing is adopted while an edit is in flight: a write waiting out its quiet
/// period, a gesture still running, or a command whose outcome has not come back
/// is the operator's intent, and what the daemon committed is the state that
/// intent is about to replace. A refused edit stays on screen by the same rule,
/// since a refusal changes nothing the daemon has committed.
pub fn program_to_adopt(
    committed: Option<&CoolingProgram>,
    seen: Option<&CoolingProgram>,
    editing: bool,
) -> Option<CoolingProgram> {
    let committed = committed?;
    if editing || seen == Some(committed) {
        return None;
    }
    Some(committed.clone())
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

/// What a channel row leads with: what the controller says is plugged in.
///
/// The channel number is not repeated here. It sits on the line below, where a
/// row that has nothing plugged in carries it as the heading instead.
pub fn channel_headline(state: &ChannelState) -> String {
    match state.accessories.len() {
        0 => format!("Channel {}", state.channel),
        1 => state.accessories[0].clone(),
        count => format!("{count} accessories"),
    }
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

/// A list of options: the menu skin plus the width clamp a row of text needs.
///
/// Rows are as wide as the menu, so without the floor a menu of short options
/// would be a sliver and one long option would set the width for every other.
fn option_menu(content: impl IntoElement) -> impl IntoElement {
    popover_surface(menu_surface(content).min_w(MENU_MIN_WIDTH))
}

/// A list of swatches: the same skin, sized to the grid inside it.
///
/// The clamp of [`option_menu`] would leave the menu wider than its own
/// content, which is empty surface beside the colors. Paneflow makes the same
/// split, for the same reason: a menu that must size to its content takes the
/// skin without the container.
fn swatch_menu(content: impl IntoElement) -> impl IntoElement {
    popover_surface(menu_surface(content))
}

/// The menu skin: radius, lifted surface, hairline, and the geometry every menu
/// shares. What sets the width stays with the caller.
fn menu_surface(content: impl IntoElement) -> Stateful<Div> {
    div()
        .id("popover-surface")
        // The list keeps the presses that land on it. Without this the
        // dismissal layer under it would also hear the press, close the
        // list on the way down, and the option the operator was
        // pressing would be gone before the release reached it.
        .occlude()
        .flex()
        .flex_col()
        .gap(MENU_ROW_GAP)
        .max_w(MENU_MAX_WIDTH)
        .max_h(MENU_MAX_HEIGHT)
        .overflow_y_scroll()
        .p(space::XS)
        .rounded(MENU_RADIUS)
        // Lifted surface and a hairline at 0.6, no drop shadow: the menu is
        // told apart from the panel by its luminance, which is how Paneflow's
        // menus read in front without casting anything.
        .bg(color::MENU.hsla())
        .border_1()
        .border_color(color::SEPARATOR.alpha(0.6))
        .child(content)
}

/// Float a built menu over the screen.
///
/// `anchored` is what keeps a menu opened near a window edge fully visible: it
/// repositions itself rather than being clipped. `deferred` paints it above
/// panels laid out after it, so a menu is never covered by its neighbor.
fn popover_surface(menu: Stateful<Div>) -> impl IntoElement {
    gpui::deferred(
        gpui::anchored()
            // Off the control rather than against it. Applied before the snap,
            // so a menu opened near the bottom edge still folds back into the
            // window instead of being pushed out of it by the offset.
            .offset(gpui::point(px(0.0), MENU_OFFSET))
            .snap_to_window_with_margin(px(8.0))
            .child(menu),
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

    /// The row is named by what the controller answered, never by a type this
    /// product guessed. The controller returns an accessory identifier and a
    /// name for it, and nothing in that answer says whether the thing is a fan,
    /// a strip or anything else, so the headline never claims one.
    #[test]
    fn a_channel_is_named_by_what_the_controller_answered() {
        let one = channel_state(1, &["HUE 2 LED Strip 300 mm"]);
        assert_eq!(channel_headline(&one), "HUE 2 LED Strip 300 mm");

        let many = channel_state(2, &["AER RGB 2 120 mm", "AER RGB 2 140 mm"]);
        assert_eq!(channel_headline(&many), "2 accessories");

        // Nothing plugged in leaves the channel number as the heading, so a row
        // is never nameless. It is also why the row adds no "Channel 2"
        // qualifier in this case: it would repeat the headline word for word.
        let empty = channel_state(3, &[]);
        assert_eq!(channel_headline(&empty), "Channel 3");
    }

    /// Every stop one channel row emits, in the order it draws them.
    ///
    /// Collected as the screen emits them rather than written as a range: a
    /// stop counted by hand lands on a control that already exists the first
    /// time one of them renders more than a single element.
    fn channel_row_stops(base: isize) -> Vec<isize> {
        vec![
            base,
            base + ROW_OFFSET_BRIGHTNESS,
            base + ROW_OFFSET_MODE,
            base + CHANNEL_OFFSET_COLOR,
            base + CHANNEL_OFFSET_SPEED,
            base + CHANNEL_OFFSET_DIRECTION,
        ]
    }

    /// Every stop the panel row emits, in the order it draws them.
    fn lcd_row_stops(base: isize) -> Vec<isize> {
        let mut stops = vec![
            base,
            base + ROW_OFFSET_BRIGHTNESS,
            base + ROW_OFFSET_MODE,
            base + LCD_OFFSET_METRIC_ONE,
            base + LCD_OFFSET_METRIC_TWO,
        ];
        stops.extend(
            (0..DisplayColorField::ALL.len() as isize)
                .map(|index| base + LCD_OFFSET_COLOR_BASE + index),
        );
        stops.push(base + LCD_OFFSET_RESUME);
        stops
    }

    #[test]
    fn every_lighting_control_keeps_traversal_order_equal_to_visual_order() {
        // However many channels the controller reports, each row's block has to
        // clear the row above it and the panel's block has to clear them all:
        // an open row's controls sit above the next line, so a shared range
        // would make Tab jump past a row and come back to it.
        for channel_count in 0..4usize {
            let mut stops = Vec::new();
            for index in 0..channel_count {
                stops.extend(channel_row_stops(lighting_row_tab(index)));
            }
            stops.extend(lcd_row_stops(lcd_row_tab(channel_count)));

            let sorted = {
                let mut copy = stops.clone();
                copy.sort();
                copy.dedup();
                copy
            };
            assert_eq!(
                stops, sorted,
                "with {channel_count} channels two controls share a stop, or run out of order"
            );
            assert!(
                stops
                    .iter()
                    .all(|stop| *stop >= SCREEN_TAB_BASE
                        && *stop > Destination::Settings.tab_index()),
                "screen controls come after every rail entry"
            );
        }
    }

    #[test]
    fn a_cooling_write_waits_for_the_deadline_the_gesture_and_the_one_in_flight() {
        let now = Instant::now();
        let passed = now - Duration::from_millis(1);
        let moved = now + AUTOSAVE_QUIET;

        assert!(cooling_write_is_due(Some(passed), now, false, false));
        assert!(
            !cooling_write_is_due(None, now, false, false),
            "nothing is scheduled"
        );
        assert!(
            !cooling_write_is_due(Some(moved), now, false, false),
            "a later edit moved the deadline, and its own timer will do this"
        );
        // Either slider holds it back, not just the curve. A timer that fired
        // mid-drag wrote the value under the cursor and then wrote again on
        // release: two commands for one gesture, and a duty nobody chose.
        assert!(!cooling_write_is_due(Some(passed), now, true, false));
        assert!(
            !cooling_write_is_due(Some(passed), now, false, true),
            "one write in flight at a time, or an outcome lands against the wrong program"
        );
    }

    #[test]
    fn the_committed_program_is_adopted_once_and_never_over_an_edit() {
        let committed = CoolingProgram::Fixed { pump: 180, fan: 90 };
        let other = CoolingProgram::Fixed { pump: 200, fan: 90 };

        assert_eq!(
            program_to_adopt(Some(&committed), None, false),
            Some(committed.clone()),
            "a window opening on a running machine takes what it is running"
        );
        assert_eq!(
            program_to_adopt(Some(&committed), Some(&committed), false),
            None,
            "a status repeating itself must not redraw the screen every second"
        );
        assert_eq!(
            program_to_adopt(Some(&other), Some(&committed), false),
            Some(other.clone()),
            "activating a profile is the daemon committing a program, and the \
             plot has to follow it"
        );
        assert_eq!(
            program_to_adopt(Some(&other), Some(&committed), true),
            None,
            "an edit in flight is the operator's intent, and it outranks the \
             state that intent is about to replace"
        );
        assert_eq!(
            program_to_adopt(None, Some(&committed), false),
            None,
            "a daemon that has committed nothing says nothing about the plot"
        );
    }

    #[test]
    fn the_quiet_period_clears_the_floor_the_daemon_enforces() {
        // The daemon refuses a command arriving inside the floor rather than
        // queueing it, and keeps no last-value-wins, so a quiet period shorter
        // than the floor would turn a settled edit into a refusal.
        assert!(
            LIGHTING_QUIET > Duration::from_millis(kori_core::lighting::MIN_COMMAND_INTERVAL_MS),
            "an edit must not be written faster than the controller accepts"
        );
        // And short enough that the whole round trip stays inside the 500 ms
        // the PRD budgets for a write reaching a confirmed state.
        assert!(
            LIGHTING_QUIET < Duration::from_millis(250),
            "an edit that takes a quarter of a second to leave reads as a control that did not take"
        );
    }

    #[test]
    fn each_lighting_row_waits_out_its_own_quiet_period() {
        let start = Instant::now();
        let mut schedule = WriteSchedule::default();
        schedule.touch(LightingRow::Channel(1), start + LIGHTING_QUIET);
        schedule.touch(LightingRow::Lcd, start + LIGHTING_QUIET * 3);

        // Nothing is due before its own deadline, and one row coming due does
        // not drag the other out with it: the daemon tracks its cadence per
        // channel, so two rows edited together are two independent writes.
        assert!(schedule.take_due(start).is_empty());
        assert_eq!(
            schedule.take_due(start + LIGHTING_QUIET),
            vec![LightingRow::Channel(1)]
        );
        assert!(!schedule.is_pending(LightingRow::Channel(1)));
        assert!(schedule.is_pending(LightingRow::Lcd));

        // A row edited again before its deadline is written once, at the later
        // deadline. That is what turns a drag into one command.
        schedule.touch(LightingRow::Lcd, start + LIGHTING_QUIET * 5);
        assert!(schedule.take_due(start + LIGHTING_QUIET * 3).is_empty());
        assert_eq!(
            schedule.take_due(start + LIGHTING_QUIET * 5),
            vec![LightingRow::Lcd]
        );
        assert!(!schedule.is_pending(LightingRow::Lcd));
    }

    #[test]
    fn a_channel_block_never_reaches_the_row_below_it() {
        let widest = channel_row_stops(lighting_row_tab(0))
            .into_iter()
            .max()
            .expect("a channel row emits stops");
        assert!(
            widest < lighting_row_tab(1),
            "the widest channel detail must stay inside its own block"
        );
    }

    #[test]
    fn every_cooling_control_keeps_traversal_order_equal_to_visual_order() {
        // The stops the screen emits, in the order they are drawn, with the
        // Pump row open. Its detail sits between the two rows, so a shared
        // detail range would make Tab jump past the Fan row and back.
        let pump = cooling_row_tab(Channel::Pump);
        let fan = cooling_row_tab(Channel::Fan);
        let indices = [
            COOLING_TAB_MODE,
            COOLING_TAB_PROFILE,
            pump,
            // The widest detail, which is the Fixed mode: the duty slider, then
            // the curve plot. Both carry their own editing through the arrow
            // keys, so each is one stop rather than a row of buttons, and the
            // stops stay reserved whichever mode the row is in so a mode change
            // never renumbers the row below it.
            pump + COOLING_OFFSET_DUTY,
            pump + COOLING_OFFSET_CURVE,
            fan,
            COOLING_TAB_REVERT,
            COOLING_TAB_SAVE,
            COOLING_TAB_DELETE,
        ];

        let mut sorted = indices.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            indices.to_vec(),
            sorted,
            "no two cooling controls share a stop, and they run in visual order"
        );
        assert!(
            indices.iter().all(
                |index| *index >= SCREEN_TAB_BASE && *index > Destination::Settings.tab_index()
            ),
            "screen controls come after every rail entry"
        );
        assert!(
            fan + COOLING_ROW_STRIDE - 1 < COOLING_TAB_REVERT,
            "the last row's detail must not reach the action buttons"
        );
    }

    #[test]
    fn the_shell_exposes_three_primary_destinations_and_one_secondary() {
        // The panel is not one of them. It is a device's appearance, so it is a
        // row on Lighting rather than a destination that would put the two
        // halves of the same question two clicks apart.
        assert_eq!(Destination::PRIMARY.len(), 3);
        assert_eq!(
            Destination::PRIMARY.map(Destination::label),
            ["Monitoring", "Cooling", "Lighting"]
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
        assert_eq!(indices.len(), 4, "every entry needs its own tab stop");
        assert!(indices.iter().all(|index| *index < SCREEN_TAB_BASE));
    }

    #[test]
    fn a_channel_row_names_the_program_the_device_reports_not_the_one_pending() {
        // The reported mode is the whole point of this line: after a mode
        // change it keeps naming the old program until the write lands, which
        // is how an operator sees that it has not.
        assert_eq!(
            reported_program(Some(PwmMode::Fixed), Some(70.6)),
            "Fixed duty 71%"
        );
        assert_eq!(
            reported_program(Some(PwmMode::Curve), None),
            "Onboard curve"
        );
        // The failsafe carries no percentage of its own: the duty printed
        // beside it is what the firmware is running, and the kernel's "100%
        // failsafe" wording routinely disagrees with that reading.
        assert_eq!(
            reported_program(Some(PwmMode::FullSpeed), Some(52.0)),
            "Firmware failsafe"
        );
        // A mode nothing answered for is said to be unreported rather than
        // shown as the safe-looking default, which would be a fabricated fact.
        assert_eq!(reported_program(None, Some(50.0)), "Mode not reported");
    }

    #[test]
    fn a_duty_track_spans_the_range_the_channel_actually_accepts() {
        // The pump refuses to stop, so its track starts at its floor instead of
        // carrying a fifth of its length that no press can reach. Both ends of
        // the track have to be values the daemon would take.
        for channel in [Channel::Pump, Channel::Fan] {
            let floor = duty_to_percent(channel.min_duty());
            let track = Bounds {
                origin: Point {
                    x: px(0.0),
                    y: px(0.0),
                },
                size: gpui::size(px(200.0), px(22.0)),
            };
            for (x, expected) in [(px(-40.0), floor), (px(400.0), MAX_DUTY_PERCENT)] {
                let percent = Slider::value_at(
                    track,
                    Point { x, y: px(10.0) },
                    f32::from(floor),
                    f32::from(MAX_DUTY_PERCENT),
                );
                let duty = duty_from_percent(percent.round() as u8);
                assert_eq!(duty_to_percent(duty), expected, "{channel:?} at {x:?}");
                assert!(duty >= channel.min_duty());
            }
        }
    }

    #[test]
    fn a_duty_track_and_a_brightness_track_never_answer_for_each_other() {
        // Two books rather than one keyed by a sum type: a rectangle left
        // behind by a screen that is no longer drawn must not be grabbable, and
        // the press handler asks each book for its own kind of drag.
        let duty: TrackMap<Channel> = TrackMap::default();
        let at = Point {
            x: px(300.0),
            y: px(200.0),
        };
        duty.record(
            Channel::Pump,
            Bounds {
                origin: Point {
                    x: px(200.0),
                    y: px(190.0),
                },
                size: gpui::size(px(200.0), px(22.0)),
            },
        );
        assert_eq!(duty.at(at).map(|(channel, _)| channel), Some(Channel::Pump));
        duty.clear();
        assert_eq!(duty.at(at), None);
    }

    #[test]
    fn every_destination_has_a_label_and_its_own_icon() {
        let mut icons = Vec::new();
        for destination in Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
        {
            assert!(!destination.label().is_empty());
            icons.push(destination.icon());
        }
        // An entry identified by a shared icon is identified by its label
        // alone, which is what the rail's icons exist to avoid. Compared
        // pairwise rather than sorted: an icon is a name, and giving it an
        // order would invent a ranking the interface never uses.
        for (index, icon) in icons.iter().enumerate() {
            assert!(
                !icons[index + 1..].contains(icon),
                "two rail entries share {icon:?}"
            );
        }
    }

    #[test]
    fn the_screen_offers_only_the_modes_it_can_configure_completely() {
        // A mode the daemon could only ever refuse is absent, not disabled,
        // which is the same rule the Lighting screen applies to an unproven
        // effect. Static images stay in the vocabulary and in the renderer.
        assert_eq!(SCREEN_MODES.len(), 3);
        assert!(SCREEN_MODES.contains(&DisplayMode::DualInfographic));
        assert!(SCREEN_MODES.contains(&DisplayMode::SingleReading));
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
    fn the_editor_exposes_every_color_control_the_story_names() {
        // US-017 names a color per reading, a text color per reading, a
        // background and a logo. The band's second color is the one addition;
        // the logo color is the one removal, since the panel carries no
        // wordmark for it to paint, which AC-6 allows in as many words.
        let labels: Vec<_> = DisplayColorField::ALL
            .map(DisplayColorField::label)
            .to_vec();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            labels.len(),
            "labels must be distinct: {labels:?}"
        );
        for required in [
            "Band 1",
            "Band 2",
            "Fade 1",
            "Fade 2",
            "Text 1",
            "Text 2",
            "Background",
        ] {
            assert!(labels.contains(&required), "{required} is missing");
        }
        assert!(
            !labels.iter().any(|label| label.contains("Wordmark")),
            "nothing draws a wordmark, so nothing colors one: {labels:?}"
        );
    }

    #[test]
    fn the_panel_row_follows_whatever_the_controller_reported() {
        // The panel's block is derived from the channel count rather than from
        // a fixed ceiling, so a controller answering with more channels than
        // expected pushes the panel down instead of colliding with it.
        for channel_count in 0..8usize {
            let base = lcd_row_tab(channel_count);
            let last_channel_stop = channel_count
                .checked_sub(1)
                .and_then(|index| channel_row_stops(lighting_row_tab(index)).into_iter().max());
            if let Some(last) = last_channel_stop {
                assert!(last < base, "the panel row overlaps the last channel row");
            }
            assert!(lcd_row_stops(base).iter().all(|stop| *stop >= base));
        }
    }

    fn track(row: LightingRow, x: f32, y: f32) -> (LightingRow, Bounds<Pixels>) {
        (
            row,
            Bounds {
                origin: Point { x: px(x), y: px(y) },
                size: gpui::size(px(110.0), px(22.0)),
            },
        )
    }

    #[test]
    fn a_press_finds_the_track_it_landed_on_and_no_other() {
        // Three rows' sliders sit in one column, 87 pixels apart, which is what
        // the screen lays out. Picking the wrong one would move a channel the
        // operator never touched.
        let tracks = TrackMap::default();
        for (row, bounds) in [
            track(LightingRow::Channel(1), 576.0, 210.0),
            track(LightingRow::Channel(2), 576.0, 297.0),
            track(LightingRow::Lcd, 576.0, 573.0),
        ] {
            tracks.record(row, bounds);
        }

        let at = |x: f32, y: f32| tracks.at(Point { x: px(x), y: px(y) }).map(|(row, _)| row);

        assert_eq!(at(600.0, 220.0), Some(LightingRow::Channel(1)));
        assert_eq!(at(600.0, 305.0), Some(LightingRow::Channel(2)));
        assert_eq!(at(680.0, 580.0), Some(LightingRow::Lcd));
        // Between two rows, and left of the column: a press there is not a
        // press on a slider, and must not move one.
        assert_eq!(at(600.0, 260.0), None);
        assert_eq!(at(400.0, 220.0), None);

        // A few pixels off the track still counts: the rail is four pixels
        // tall inside a taller control, and aiming at it exactly is not what
        // the operator is doing.
        assert_eq!(at(600.0, 206.0), Some(LightingRow::Channel(1)));
        assert_eq!(at(572.0, 220.0), Some(LightingRow::Channel(1)));
    }

    #[test]
    fn a_row_that_went_away_takes_its_track_with_it() {
        let tracks = TrackMap::default();
        let (row, bounds) = track(LightingRow::Channel(1), 576.0, 210.0);
        tracks.record(row, bounds);
        let inside = Point {
            x: px(600.0),
            y: px(220.0),
        };
        assert!(tracks.at(inside).is_some());

        // Re-recording moves the rectangle rather than adding a second one, so
        // a row cannot accumulate stale hit areas as the screen scrolls.
        let (_, moved) = track(LightingRow::Channel(1), 576.0, 400.0);
        tracks.record(row, moved);
        assert_eq!(tracks.at(inside), None);
        assert!(
            tracks
                .at(Point {
                    x: px(600.0),
                    y: px(410.0)
                })
                .is_some()
        );

        // And a frame that draws no slider leaves nothing to grab.
        tracks.clear();
        assert_eq!(
            tracks.at(Point {
                x: px(600.0),
                y: px(410.0)
            }),
            None
        );
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
