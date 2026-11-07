//! The launcher panel: the search field with its inline completion, and the results.

use std::f32::consts::PI;

use rune::{
    Color, Curve, Cx, Easing, El, Icon, Span, Spring, SpringPair, TextStyle, Transition, col, el,
    icon, row, text, text_input,
};

use crate::{
    Change, ENTER_MOTION, EXIT_MOTION, ICON_SIZE, Launcher, RADIUS, RESIZE_MOTION, ROW_MOTION,
    SEARCH_HEIGHT, SELECTION_MOTION, Target, VISIBLE_ROWS, complete::Completion,
};

const ROW_HEIGHT: f32 = 36.0;
const ROW_GAP: f32 = 1.0;
const ROW_STEP: f32 = ROW_HEIGHT + ROW_GAP;
const RESULTS_PADDING: f32 = 6.0;

/// How far, in rows, a wrapped-around page slides in.
const WRAP_TRAVEL: f32 = 0.35;
/// Rows scrolling in and out fade quickly, and leave faster than they arrive.
const ROW_FADE: SpringPair = SpringPair::new(Spring::new(0.14, 0.0), Spring::new(0.08, 0.0));
/// The light that follows the selection along the panel's edges.
const RIM_HEIGHT: f32 = ROW_HEIGHT * 1.7;

static SEARCH: Icon = Icon {
    name: "runelight-search",
    view: 16.0,
    body: r#"<path d="M14 14L10.3454 10.3454M6.88889 11.7778C9.58889 11.7778 11.7778 9.58889 11.7778 6.88889C11.7778 4.18889 9.58889 2 6.88889 2C4.18889 2 2 4.18889 2 6.88889C2 9.58889 4.18889 11.7778 6.88889 11.7778Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#,
};

struct Theme {
    text: Color,
    secondary: Color,
    dark: bool,
}

impl Theme {
    /// The edge light: white, so it reads as light on both appearances.
    fn light(&self) -> Color {
        Color::WHITE.alpha(if self.dark { 0.5 } else { 0.95 })
    }
}

pub fn view(state: &Launcher, cx: &mut Cx<Launcher>) -> El {
    let dark = cx.system_dark();
    let tokens = rune::tok::theme(dark);
    let theme = Theme {
        text: tokens.text,
        secondary: tokens.text_secondary,
        dark,
    };

    let mut panel = col()
        .key("panel")
        .w_full()
        .radius(RADIUS)
        .continuous_corners()
        .clip_rounded(RADIUS)
        .motion(RESIZE_MOTION)
        .live_opacity(state.opacity.smooth(ENTER_MOTION).rest(EXIT_MOTION))
        .hover_motion_within(
            Curve::tween(0.06, Easing::EASE_OUT),
            Curve::tween(0.1, Easing::EASE_OUT),
        )
        .system_material(rune::SystemMaterial::Popover)
        .fill(tokens.card.alpha(0.6))
        .border(1.0, theme.text.alpha(0.1));
    let idle =
        state.query.text.trim().is_empty() || (state.results.is_empty() && state.catalogue_ready);

    // The light round the border stays mounted and pinned to the panel, under everything,
    // and only fades: a slot that left would keep its old rect, and its corners would show
    // inside the growing panel.
    panel = panel
        .child(
            crate::glow::slot(theme.light().a, cx.is_reduced_motion(), RADIUS)
                .key("border-light")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .opacity(if idle { 1.0 } else { 0.0 })
                .opacity_motion(Curve::tween(
                    if idle { 0.4 } else { 0.12 },
                    Easing::EASE_OUT,
                )),
        )
        .child(search_row(state, &theme));

    if !cx.is_reduced_motion() {
        panel = panel.live_scale(state.scale.smooth(ENTER_MOTION).rest(EXIT_MOTION));
    }
    if !state.visible {
        panel = panel.inert();
    }

    if idle {
        return panel;
    }

    panel
        .child(
            el().w_full()
                .h(1.0)
                .fill(theme.text.alpha(0.08))
                .transition(Transition::none()),
        )
        .child(results(state, &theme))
}

fn search_row(state: &Launcher, theme: &Theme) -> El {
    let style = TextStyle::new(19.0, 400.0).lh(26.0);
    let input = text_input(&state.query, style)
        .key("search")
        .label("Search apps and quicklinks")
        .single_line()
        .w_full()
        .color(theme.text)
        .placeholder("Search", theme.secondary)
        .on_edit(|state: &mut Launcher| &mut state.query)
        .on_input_key(crate::key)
        .on_input_change(|state: &mut Launcher, tx| {
            state.selected = 0;
            state.search();

            tx.animated();
            tx.motion(RESIZE_MOTION);
        })
        .on_submit(Launcher::launch);

    let mut field = col().grow(1.0).min_w(0.0);
    if let Some(ghost) = ghost(state, theme, style) {
        field = field.child(ghost);
    }

    row()
        .instant_changes()
        .w_full()
        .h(SEARCH_HEIGHT)
        .px(18.0)
        .gap(10.0)
        .center_y()
        .child(icon(&SEARCH, 18.0).color(theme.secondary))
        .child(field.child(input))
}

/// The selected result's completion, drawn under the field: the typed text again, invisible,
/// so the rest of the name lands exactly after the caret in the same frame as the keystroke.
fn ghost(state: &Launcher, theme: &Theme, style: TextStyle) -> Option<El> {
    let typed = &state.query.text;
    let (shown, color) = match state.completion()? {
        Completion::Suffix(rest) => (format!("{typed}{rest}"), theme.secondary),
        Completion::Hint(name) => (format!("{typed}  —  {name}"), theme.secondary.alpha(0.7)),
    };

    Some(
        text(shown)
            .key("ghost")
            .font(style)
            .color(color)
            .spans(vec![Span::new(0, typed.len()).color(Color::TRANSPARENT)])
            .truncate_fade()
            .pin(Some(0.0), Some(0.0), Some(0.0), None)
            .transition(Transition::none()),
    )
}

fn results(state: &Launcher, theme: &Theme) -> El {
    let travel = |rows: f32| {
        Transition::offset(0.0, rows * ROW_STEP)
            .combined(Transition::opacity())
            .springs(ROW_FADE)
    };

    // A new search lands at once. Scrolling, the rows move together: the row leaving one edge
    // goes with them and the next comes in from the other. Wrapping around replaces the page,
    // so it goes at once and the other end arrives with a short slide from its side.
    let (enter, leave) = match state.change {
        Change::Search | Change::Step { shift: 0 } => (Transition::none(), Transition::none()),
        Change::Step { shift } => (travel(-f32::from(shift)), travel(f32::from(shift))),
        Change::Wrap { shift } => (travel(-f32::from(shift) * WRAP_TRAVEL), Transition::none()),
    };

    let instant = state.change == Change::Search;
    let mut list = col()
        .key("results")
        .role("listbox")
        .label("Search results")
        .w_full()
        .transition(Transition::none())
        .padding(RESULTS_PADDING)
        .gap(ROW_GAP)
        .child_exit(leave)
        .clip_fade(0.0, RESULTS_PADDING, 0.0, RESULTS_PADDING);
    if state.results.is_empty() {
        return list.child(
            text("Finding applications…")
                .instant_changes()
                .transition(Transition::none())
                .w_full()
                .px(16.0)
                .py(10.0)
                .font(TextStyle::new(13.0, 400.0))
                .color(theme.secondary),
        );
    }

    let start = state.first_visible;
    let slot = (state.selected - start) as f32 * ROW_STEP;
    let light = theme.light();

    // The highlight and its light glide with the arrow keys and jump with a new search. Only
    // a wrap mounts new ones, which come in from the far end.
    let follow = |element: El| {
        let element = element
            .offset_motion(SELECTION_MOTION)
            .transition(match state.change {
                Change::Wrap { .. } => enter,
                _ => Transition::none(),
            });

        if instant {
            element.instant_changes()
        } else {
            element
        }
    };

    for (side, left) in [("left", true), ("right", false)] {
        list = list.child(follow(
            el().key(format!("rim-{side}-{}", state.wraps))
                .pin(left.then_some(0.0), Some(0.0), (!left).then_some(0.0), None)
                .size(1.0, RIM_HEIGHT)
                .linear_gradient(PI, beam(light))
                .offset(
                    0.0,
                    RESULTS_PADDING + slot + (ROW_HEIGHT - RIM_HEIGHT) / 2.0,
                ),
        ));
    }

    list = list.child(follow(
        el().key(format!("selection-{}", state.wraps))
            .pin(
                Some(RESULTS_PADDING),
                Some(RESULTS_PADDING),
                Some(RESULTS_PADDING),
                None,
            )
            .h(ROW_HEIGHT)
            .radius(6.0)
            .fill(theme.text.alpha(0.09))
            // A faint top edge, as if the light along the rims caught a bevel.
            .border_gradient(1.0, light.alpha(light.a * 0.36), Color::TRANSPARENT)
            .offset(0.0, slot),
    ));

    for (position, &index) in state
        .results
        .iter()
        .enumerate()
        .skip(start)
        .take(VISIBLE_ROWS)
    {
        let entry = &state.entries[index];
        let selected = position == state.selected;
        let label = text(&entry.name)
            .grow(1.0)
            .min_w(0.0)
            .truncate()
            .font(TextStyle::new(13.5, 400.0))
            .color(theme.text);

        let entries = state.entries.clone();
        let mut row = row()
            .key(format!("result-{index}"))
            .role("option")
            .label(&entry.name)
            .motion(ROW_MOTION)
            .transition(enter)
            .w_full()
            .h(ROW_HEIGHT)
            .px(10.0)
            .gap(10.0)
            .center_y()
            .radius(6.0)
            .hover(if selected {
                Color::TRANSPARENT
            } else {
                theme.text.alpha(0.04)
            })
            .child(state.icon(entry, theme.text))
            .child(label)
            .child(
                text(&entry.detail)
                    .truncate()
                    .max_w(260.0)
                    .font(TextStyle::new(12.0, 400.0))
                    .color(theme.secondary),
            )
            .child(
                text("↵")
                    .instant_changes()
                    .w(16.0)
                    .font(TextStyle::new(13.0, 400.0))
                    .color(theme.secondary)
                    .opacity(if selected { 1.0 } else { 0.0 }),
            )
            .on_press(move |state: &mut Launcher| {
                state.launch_entry(entries[index].clone());
            });

        if instant {
            row = row.instant_changes();
        }

        list = list.child(row);
    }

    list
}

/// A line of light, brightest in the middle and fading to nothing at both ends.
fn beam(light: Color) -> [(f32, Color); 3] {
    [
        (0.0, Color::TRANSPARENT),
        (0.5, light),
        (1.0, Color::TRANSPARENT),
    ]
}

impl Launcher {
    fn icon(&self, entry: &crate::Entry, color: Color) -> El {
        if let Target::Application(path) = &entry.target
            && let Some(cache) = &self.images
        {
            let mut icons = self.icons.borrow_mut();
            if let Some(image) = icons.get(path) {
                return el().size(ICON_SIZE, ICON_SIZE).pixels(image.pixels());
            }

            let image = cache.app_icon(path, ICON_SIZE);
            let element = el().size(ICON_SIZE, ICON_SIZE).pixels(image.pixels());
            icons.insert(path.clone(), image);
            return element;
        }

        el().size(ICON_SIZE, ICON_SIZE)
            .radius(5.0)
            .fill(color.alpha(0.08))
            .center()
            .child(
                text("↗")
                    .font(TextStyle::new(14.0, 400.0))
                    .color(color.alpha(0.7)),
            )
    }
}
