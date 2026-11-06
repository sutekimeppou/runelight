//! The launcher panel: the search field with its inline completion, and the results.

use rune::{
    Color, Curve, Cx, Easing, El, Icon, Span, TextStyle, Transition, col, el,
    icon, row, text, text_input,
};

use crate::{
    ENTER_MOTION, EXIT_MOTION, ICON_SIZE, Launcher, RADIUS, RESIZE_MOTION,
    SEARCH_HEIGHT, SELECTION_MOTION, Target, VISIBLE_ROWS, complete::Completion,
};

const ROW_HEIGHT: f32 = 36.0;
const ROW_GAP: f32 = 1.0;
const RESULTS_PADDING: f32 = 6.0;
static SEARCH: Icon = Icon {
    name: "runelight-search",
    view: 16.0,
    body: r#"<path d="M14 14L10.3454 10.3454M6.88889 11.7778C9.58889 11.7778 11.7778 9.58889 11.7778 6.88889C11.7778 4.18889 9.58889 2 6.88889 2C4.18889 2 2 4.18889 2 6.88889C2 9.58889 4.18889 11.7778 6.88889 11.7778Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#,
};

struct Theme {
    text: Color,
    secondary: Color,
}

pub fn view(state: &Launcher, cx: &mut Cx<Launcher>) -> El {
    let dark = cx.system_dark();
    let tokens = rune::tok::theme(dark);
    let theme = Theme {
        text: tokens.text,
        secondary: tokens.text_secondary,
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
        .border(1.0, theme.text.alpha(0.1))
        .child(search_row(state, &theme));

    if !cx.is_reduced_motion() {
        panel = panel.live_scale(state.scale.smooth(ENTER_MOTION).rest(EXIT_MOTION));
    }
    if !state.visible {
        panel = panel.inert();
    }
    if state.query.text.trim().is_empty() || (state.results.is_empty() && !state.scanning) {
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
    let mut results = col()
        .key("results")
        .role("listbox")
        .label("Search results")
        .w_full()
        .transition(Transition::none())
        .padding(RESULTS_PADDING)
        .gap(ROW_GAP);
    let start = state.first_visible;
    if !state.results.is_empty() {
        results = results.child(
            el()
                // A new search resets the highlight immediately. Arrow keys move it in place.
                .key(format!("selection-{}", state.query.text))
                .pin(
                    Some(RESULTS_PADDING),
                    Some(RESULTS_PADDING),
                    Some(RESULTS_PADDING),
                    None,
                )
                .h(ROW_HEIGHT)
                .radius(6.0)
                .fill(theme.text.alpha(0.09))
                .offset(
                    0.0,
                    (state.selected - start) as f32 * (ROW_HEIGHT + ROW_GAP),
                )
                .offset_motion(SELECTION_MOTION)
                .transition(Transition::none()),
        );
    }
    for (position, &index) in state
        .results
        .iter()
        .enumerate()
        .skip(start)
        .take(VISIBLE_ROWS)
    {
        let entry = &state.entries[index];
        let selected = position == state.selected;
        let detail = match &entry.target {
            Target::Application(_) => String::new(),
            Target::Quicklink(url) | Target::Fallback(url) => url::Url::parse(url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| url.clone()),
        };
        let label = text(&entry.name)
            .grow(1.0)
            .min_w(0.0)
            .truncate()
            .font(TextStyle::new(13.5, 400.0))
            .color(theme.text);
        let target = entry.clone();
        results = results.child(
            row()
                .key(format!("result-{index}"))
                .role("option")
                .label(&entry.name)
                .instant_changes()
                .transition(Transition::none())
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
                    text(detail)
                        .truncate()
                        .max_w(260.0)
                        .font(TextStyle::new(12.0, 400.0))
                        .color(theme.secondary),
                )
                .child(
                    text("↵")
                        .w(16.0)
                        .font(TextStyle::new(13.0, 400.0))
                        .color(theme.secondary)
                        .opacity(if selected { 1.0 } else { 0.0 }),
                )
                .on_press(move |state: &mut Launcher| {
                    state.launch_entry(target.clone());
                }),
        );
    }
    if state.results.is_empty() {
        results = results.child(
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
    results
}

impl Launcher {
    fn icon(&self, entry: &crate::Entry, color: Color) -> El {
        if let Target::Application(path) = &entry.target
            && let Some(cache) = &self.images
        {
            let mut icons = self.icons.borrow_mut();
            let image = icons
                .entry(path.clone())
                .or_insert_with(|| cache.app_icon(path, ICON_SIZE));
            return el().size(ICON_SIZE, ICON_SIZE).pixels(image.pixels());
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
