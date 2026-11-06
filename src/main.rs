mod apps;
mod config;
mod history;

use std::{cell::RefCell, collections::HashMap, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use frizbee::iter::FuzzyMatchExt;
use rune::{
    Align, App, AppHandle, AppSpec, Color, Curve, Cx, DisplayTarget, Easing, El, Icon, Image,
    ImageCache, Key, KeyEvent, Live, MacWindowOptions, OverlayPlacement, ScreenEdge, Shortcut,
    ShortcutId, Spring, SystemMaterial, TextEdit, TextStyle, Transition, Tx, WindowOptions,
    WindowSpec, col, el, icon, row, text, text_input,
};

const WIDTH: f32 = 700.0;
const SEARCH_HEIGHT: f32 = 52.0;
const ROW_HEIGHT: f32 = 36.0;
const ICON_SIZE: f32 = 22.0;
const RADIUS: f32 = 20.0;
const RESULTS_PADDING: f32 = 6.0;
const ROW_GAP: f32 = 1.0;
const RESIZE_MOTION: Spring = Spring::new(0.17, 0.0);
const SELECTION_MOTION: Spring = Spring::new(0.11, 0.0);
const ENTER_MOTION: Spring = Spring::new(0.14, 0.0);
const EXIT_MOTION: Spring = Spring::new(0.095, 0.0);
const HIDDEN_SCALE: f32 = 0.98;

static SEARCH: Icon = Icon {
    name: "runelight-search",
    view: 16.0,
    body: r#"<path d="M14 14L10.3454 10.3454M6.88889 11.7778C9.58889 11.7778 11.7778 9.58889 11.7778 6.88889C11.7778 4.18889 9.58889 2 6.88889 2C4.18889 2 2 4.18889 2 6.88889C2 9.58889 4.18889 11.7778 6.88889 11.7778Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>"#,
};
const VISIBLE_ROWS: usize = 8;

#[derive(Clone, PartialEq, Eq)]
enum Target {
    Application(PathBuf),
    Quicklink(String),
    Fallback(String),
}

impl Target {
    fn history_key(&self) -> Option<String> {
        match self {
            Self::Application(path) => Some(format!("app:{}", path.display())),
            Self::Quicklink(url) => Some(format!("url:{url}")),
            Self::Fallback(_) => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    sort_name: String,
    target: Target,
}

impl Entry {
    fn new(name: String, target: Target) -> Self {
        let sort_name = name.to_lowercase();
        Self {
            name,
            sort_name,
            target,
        }
    }

    fn open(&self, query: &str) -> Result<()> {
        let mut command = Command::new("/usr/bin/open");
        command.arg("--");
        match &self.target {
            Target::Application(path) => command.arg(path),
            Target::Quicklink(url) => command.arg(url),
            Target::Fallback(template) => {
                let encoded = url::form_urlencoded::byte_serialize(query.trim().as_bytes())
                    .collect::<String>();
                command.arg(template.replace("{query}", &encoded))
            }
        };
        let output = command.output().context("Could not run macOS open")?;
        if !output.status.success() {
            bail!(
                "Could not open {}: {}",
                self.name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

struct Launcher {
    me: AppHandle<Self>,
    query: TextEdit,
    config_path: PathBuf,
    config: config::Config,
    history: history::History,
    shortcut: Option<(Shortcut, ShortcutId)>,
    entries: Vec<Entry>,
    results: Vec<usize>,
    selected: usize,
    first_visible: usize,
    scanning: bool,
    visible: bool,
    closing: bool,
    opacity: Live<f32>,
    scale: Live<f32>,
    images: Option<ImageCache>,
    icons: RefCell<HashMap<PathBuf, Image>>,
}

impl Launcher {
    fn load_initial_config(&mut self) {
        match config::load(&self.config_path) {
            Ok(config) => self.config = config,
            Err(error) => eprintln!("runelight: {error:#}"),
        }
        if let Err(error) = self.bind_hotkey() {
            eprintln!("runelight: {error:#}");
        }
        self.rebuild_entries(Vec::new());
    }

    fn bind_hotkey(&mut self) -> Result<()> {
        let shortcut: Shortcut = self.config.hotkey.parse()?;
        if self
            .shortcut
            .as_ref()
            .is_some_and(|(current, _)| *current == shortcut)
        {
            return Ok(());
        }
        let id = self
            .me
            .register_shortcut(shortcut.clone(), toggle)
            .with_context(|| format!("Could not register {}", self.config.hotkey))?;
        if let Some((_, old)) = self.shortcut.replace((shortcut, id)) {
            self.me.unregister_shortcut(old)?;
        }
        Ok(())
    }

    fn rebuild_entries(&mut self, apps: Vec<apps::Application>) {
        let selected = self
            .results
            .get(self.selected)
            .map(|&index| self.entries[index].clone());
        self.entries = apps
            .into_iter()
            .map(|app| Entry::new(app.name, Target::Application(app.path)))
            .collect();
        self.entries.extend(
            self.config
                .quicklinks
                .iter()
                .map(|link| Entry::new(link.name.clone(), Target::Quicklink(link.url.clone()))),
        );
        self.entries.extend(
            self.config
                .fallbacks
                .iter()
                .map(|link| Entry::new(link.name.clone(), Target::Fallback(link.url.clone()))),
        );
        self.search();
        let selected = selected
            .and_then(|selected| {
                self.results
                    .iter()
                    .position(|&index| self.entries[index] == selected)
            })
            .unwrap_or(0);
        self.select(selected);
    }

    fn select(&mut self, position: usize) {
        self.selected = position.min(self.results.len().saturating_sub(1));
        self.first_visible = self
            .first_visible
            .min(self.selected)
            .max(self.selected.saturating_sub(VISIBLE_ROWS - 1));
    }

    fn search(&mut self) {
        let query = self.query.text.trim().to_lowercase();
        if query.is_empty() {
            self.results.clear();
            self.select(0);
            return;
        }
        let mut matches: Vec<_> = self
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .fuzzy_match(
                query.as_str(),
                &frizbee::Config {
                    max_typos: Some(2),
                    ..Default::default()
                },
            )
            .filter(|matched| {
                !matches!(
                    self.entries[matched.index as usize].target,
                    Target::Fallback(_)
                )
            })
            .map(|matched| {
                let index = matched.index as usize;
                let usage = self.entries[index]
                    .target
                    .history_key()
                    .map(|target| self.history.score(&query, &target))
                    .unwrap_or_default();
                (index, matched.score, usage)
            })
            .collect();
        matches.sort_by(|(a, a_score, a_usage), (b, b_score, b_usage)| {
            b_usage
                .cmp(a_usage)
                .then_with(|| b_score.cmp(a_score))
                .then_with(|| self.entries[*a].sort_name.cmp(&self.entries[*b].sort_name))
        });
        self.results = matches.into_iter().map(|(index, _, _)| index).collect();
        if self.results.is_empty() && !self.scanning {
            self.results = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    matches!(entry.target, Target::Fallback(_)).then_some(index)
                })
                .collect();
        }
        self.select(self.selected);
    }

    fn refresh(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let me = self.me.clone();
        let config_path = self.config_path.clone();
        std::thread::spawn(move || {
            let config = config::load(&config_path);
            let discovery = apps::discover();
            me.update(move |state, tx| {
                tx.motion(RESIZE_MOTION);
                let mut can_prune = discovery.complete;
                match config {
                    Ok(config) => state.config = config,
                    Err(error) => {
                        can_prune = false;
                        eprintln!("runelight: {error:#}");
                    }
                }
                if let Err(error) = state.bind_hotkey() {
                    eprintln!("runelight: {error:#}");
                }
                state.scanning = false;
                state.rebuild_entries(discovery.apps);
                if can_prune {
                    let targets = state
                        .entries
                        .iter()
                        .filter_map(|entry| entry.target.history_key())
                        .collect();
                    state.history.prune(&targets);
                }
            });
        });
    }

    fn show(&mut self, tx: &mut Tx) {
        tx.settle();
        self.query.clear();
        self.selected = 0;
        self.search();
        self.refresh();
        self.show_panel();
    }

    fn show_panel(&mut self) {
        self.query.focused = true;
        self.visible = true;
        self.closing = false;
        self.me.place(
            DisplayTarget::UnderPointer,
            OverlayPlacement::Edge {
                edge: ScreenEdge::Top,
                margin: 200.0,
                align: Align::Center,
                inset: 0.0,
            },
        );
        self.me.open();
    }

    fn dismiss(&mut self) {
        self.visible = false;
        self.closing = true;
        self.opacity.set(0.0);
        self.scale.set(HIDDEN_SCALE);
    }

    fn launch(&mut self, _: &mut Tx) {
        let Some(&index) = self.results.get(self.selected) else {
            return;
        };
        self.launch_entry(self.entries[index].clone());
    }

    fn launch_entry(&mut self, entry: Entry) {
        if let Some(position) = self
            .results
            .iter()
            .position(|&index| self.entries[index] == entry)
        {
            self.select(position);
        }
        self.dismiss();
        let me = self.me.clone();
        let query = self.query.text.clone();
        std::thread::spawn(move || {
            let result = entry.open(&query);
            me.update(move |state, _| match result {
                Ok(()) => {
                    if let Some(target) = entry.target.history_key() {
                        state.history.record(query, target);
                    }
                }
                Err(error) => {
                    eprintln!("runelight: {error:#}");
                    state.show_panel();
                }
            });
        });
    }

    fn icon(&self, entry: &Entry, color: Color) -> El {
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

fn toggle(state: &mut Launcher, tx: &mut Tx) {
    if state.visible {
        state.dismiss()
    } else {
        state.show(tx)
    }
}

fn key(state: &mut Launcher, event: &KeyEvent, tx: &mut Tx) -> bool {
    match event.key {
        Key::Escape => state.dismiss(),
        Key::Down if !state.results.is_empty() => {
            state.select((state.selected + 1) % state.results.len());
        }
        Key::Up if !state.results.is_empty() => {
            state.select((state.selected + state.results.len() - 1) % state.results.len());
        }
        _ if event.is_command('q') => tx.quit(),
        _ => return false,
    }
    true
}

fn view(state: &Launcher, cx: &mut Cx<Launcher>) -> El {
    let theme = rune::tok::theme(cx.system_dark());
    let input = text_input(&state.query, TextStyle::new(19.0, 400.0).lh(26.0))
        .key("search")
        .label("Search apps and quicklinks")
        .single_line()
        .grow(1.0)
        .color(theme.text)
        .placeholder("Search", theme.text_secondary)
        .on_edit(|state: &mut Launcher| &mut state.query)
        .on_input_key(key)
        .on_input_change(|state: &mut Launcher, tx| {
            state.selected = 0;
            state.search();
            tx.animated();
            tx.motion(RESIZE_MOTION);
        })
        .on_submit(Launcher::launch);
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
        .system_material(SystemMaterial::Popover)
        .fill(theme.card.alpha(0.6))
        .border(1.0, theme.text.alpha(0.1))
        .child(
            row()
                .instant_changes()
                .w_full()
                .h(SEARCH_HEIGHT)
                .px(18.0)
                .gap(10.0)
                .center_y()
                .child(icon(&SEARCH, 18.0).color(theme.text_secondary))
                .child(input),
        );

    if !cx.is_reduced_motion() {
        panel = panel.live_scale(state.scale.smooth(ENTER_MOTION).rest(EXIT_MOTION));
    }
    if !state.visible {
        panel = panel.inert();
    }
    if state.query.text.trim().is_empty() || (state.results.is_empty() && !state.scanning) {
        return panel;
    }
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
                        .color(theme.text_secondary),
                )
                .child(
                    text(if selected { "↵" } else { "" })
                        .font(TextStyle::new(13.0, 400.0))
                        .color(theme.text_secondary),
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
                .color(theme.text_secondary),
        );
    }
    panel
        .child(
            el().w_full()
                .h(1.0)
                .fill(theme.text.alpha(0.08))
                .transition(Transition::none()),
        )
        .child(results)
}

fn main() -> Result<()> {
    let config_path = config::path()?;
    let history = history::History::load().unwrap_or_else(|error| {
        eprintln!("runelight: could not load launch history: {error:#}");
        history::History::default()
    });
    rune::try_run_windows_with(rune::RunOptions { accessory: true }, move |_| {
        let spec = AppSpec {
            families: ("\"System Font\", \".SF NS\", Inter", "ui-monospace"),
            setup: Some(|state: &mut Launcher, waker| state.images = Some(ImageCache::new(waker))),
            on_key: Some(key),
            ..AppSpec::new("Runelight", (WIDTH, SEARCH_HEIGHT), || unreachable!(), view)
        };
        let app = App::new(spec)
            .keep_running()
            .fit_content_height()
            .srgb_blend(true);
        let me = app.handle();
        let ready = me.clone();
        let app = app
            .init(move || Launcher {
                me,
                config_path,
                query: TextEdit::new(""),
                config: config::Config::default(),
                history,
                shortcut: None,
                entries: Vec::new(),
                results: Vec::new(),
                selected: 0,
                first_visible: 0,
                scanning: false,
                visible: false,
                closing: false,
                opacity: Live::new(0.0),
                scale: Live::new(HIDDEN_SCALE),
                images: None,
                icons: RefCell::new(HashMap::new()),
            })
            .on_ready(move |_| {
                ready.update(|state, tx| {
                    state.load_initial_config();
                    state.show(tx);
                })
            })
            .on_reopen(|state, _, tx| {
                state.show(tx);
                true
            })
            .on_focus(|state, focused, _| {
                if !focused && state.visible {
                    state.dismiss();
                }
            })
            .on_frame_drawn(|rt| {
                if rt.state.visible {
                    // Start after the hidden pose has drawn, so reopening can animate.
                    rt.state.opacity.set(1.0);
                    rt.state.scale.set(1.0);
                } else if rt.state.closing
                    && rt
                        .presented("panel")
                        .is_some_and(|(_, opacity)| opacity <= 0.001)
                {
                    rt.state.me.update_if(|state, tx| {
                        if state.closing && !state.visible {
                            state.closing = false;
                            tx.close_window();
                        }
                        false
                    });
                }
            })
            .on_close(|state, _| {
                state.visible = false;
                state.closing = false;
                state.opacity.snap(0.0);
                state.scale.snap(HIDDEN_SCALE);
            });
        let options = WindowOptions {
            transparent: true,
            borderless: true,
            resizable: false,
            min_size: Some((0.0, 0.0)),
            ..Default::default()
        };
        // Keep the native panel and GPU alive between openings. A normal panel orders in
        // immediately; Rune's launcher preset waits for a hidden render before revealing.
        vec![
            WindowSpec::from_app("launcher", app).configured_options(
                options.macos(
                    MacWindowOptions::default()
                        .nonactivating_panel(true)
                        .corner_radius(RADIUS),
                ),
            ),
        ]
    })
    .map_err(anyhow::Error::msg)
}
