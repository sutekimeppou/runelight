mod apps;
mod complete;
mod config;
mod history;
mod view;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use frizbee::iter::FuzzyMatchExt;
use rune::{
    Align, App, AppHandle, AppSpec, DisplayTarget, Image, ImageCache, Key, KeyEvent, Live,
    MacWindowOptions, OverlayPlacement, ScreenEdge, Shortcut, ShortcutId, Spring, TextEdit, Tx,
    WindowOptions, WindowSpec,
};

use complete::Completion;

const WIDTH: f32 = 700.0;
const SEARCH_HEIGHT: f32 = 52.0;
const ICON_SIZE: f32 = 22.0;
const RADIUS: f32 = 20.0;
const VISIBLE_ROWS: usize = 8;
/// Reopening within this long brings the last query back, selected, so typing replaces it.
const RESUME_WINDOW: Duration = Duration::from_secs(10);

const RESIZE_MOTION: Spring = Spring::new(0.17, 0.0);
const SELECTION_MOTION: Spring = Spring::new(0.11, 0.0);
const ROW_MOTION: Spring = Spring::new(0.1, 0.0);
const ENTER_MOTION: Spring = Spring::new(0.14, 0.0);
const EXIT_MOTION: Spring = Spring::new(0.095, 0.0);
const HIDDEN_SCALE: f32 = 0.98;

/// How the list changed in the latest commit, which decides how rows come and go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    /// A new search or a rescan: everything lands at once.
    Search,
    /// An arrow key moved the selection. `shift` is which way the rows moved: -1 up
    /// (scrolling down), 1 down, 0 not at all.
    Step { shift: i8 },
    /// The selection wrapped around: the other end of the list comes in from `shift`'s side.
    Wrap { shift: i8 },
}

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
    change: Change,
    /// Bumped when the selection wraps around, so the highlight enters at the other end
    /// instead of sweeping across every row.
    wraps: u32,
    scanning: bool,
    visible: bool,
    closing: bool,
    closed_at: Option<Instant>,
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
        let selected = self.selected_entry().cloned();
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
        self.reselect(selected.as_ref());
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.results
            .get(self.selected)
            .map(|&index| &self.entries[index])
    }

    /// Select `entry` again if it's still a result, else the top result.
    fn reselect(&mut self, entry: Option<&Entry>) {
        let position = entry
            .and_then(|entry| {
                self.results
                    .iter()
                    .position(|&index| self.entries[index] == *entry)
            })
            .unwrap_or(0);
        self.select(position);
    }

    fn select(&mut self, position: usize) {
        self.selected = position.min(self.results.len().saturating_sub(1));
        self.first_visible = self
            .first_visible
            .min(self.selected)
            .max(self.selected.saturating_sub(VISIBLE_ROWS - 1));
    }

    /// Arrow keys: step the selection, wrapping at either end.
    fn step(&mut self, down: bool) {
        let count = self.results.len();
        if count == 0 {
            return;
        }
        let wraps = if down {
            self.selected + 1 == count
        } else {
            self.selected == 0
        };
        let first_visible = self.first_visible;
        self.select(if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        });
        self.change = if wraps && count > 1 {
            self.wraps = self.wraps.wrapping_add(1);
            // Wrapping down brings the top of the list in from above, and the other way round.
            Change::Wrap {
                shift: if down { 1 } else { -1 },
            }
        } else {
            Change::Step {
                shift: match self.first_visible.cmp(&first_visible) {
                    std::cmp::Ordering::Greater => -1,
                    std::cmp::Ordering::Less => 1,
                    std::cmp::Ordering::Equal => 0,
                },
            }
        };
    }

    fn search(&mut self) {
        self.change = Change::Search;
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
            .fuzzy_match(query.as_str(), &complete::config())
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

    /// The selected result's completion, while the caret sits at the end of the query.
    fn completion(&self) -> Option<Completion<'_>> {
        let edit = &self.query;
        let (start, end) = edit.range();
        if !edit.focused || start != end || end != edit.text.len() || edit.display().0 != edit.text
        {
            return None;
        }
        complete::completion(&edit.text, &self.selected_entry()?.name)
    }

    /// Tab, or → at the end of the query: take the completion into the field.
    fn accept_completion(&mut self) -> bool {
        let Some(entry) = self.selected_entry().cloned() else {
            return false;
        };
        if matches!(entry.target, Target::Fallback(_)) {
            return false;
        }
        let end = self.query.text.len();
        match self.completion() {
            Some(Completion::Suffix(rest)) => {
                let rest = rest.to_owned();
                self.query.set_caret(end);
                self.query.insert(&rest);
            }
            Some(Completion::Hint(name)) => {
                let name = name.to_owned();
                self.query.set_selection(0, end);
                self.query.insert(&name);
            }
            None => return false,
        }
        self.search();
        self.reselect(Some(&entry));
        true
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
        let resume = self
            .closed_at
            .is_some_and(|closed| closed.elapsed() < RESUME_WINDOW)
            && !self.query.text.is_empty();
        if resume {
            let selected = self.selected_entry().cloned();
            self.query.set_selection(0, self.query.text.len());
            self.search();
            self.reselect(selected.as_ref());
        } else {
            self.query.clear();
            self.selected = 0;
            self.search();
        }
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
        self.closed_at = Some(Instant::now());
        self.opacity.set(0.0);
        self.scale.set(HIDDEN_SCALE);
    }

    fn launch(&mut self, _: &mut Tx) {
        if let Some(entry) = self.selected_entry().cloned() {
            self.launch_entry(entry);
        }
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
}

fn toggle(state: &mut Launcher, tx: &mut Tx) {
    if state.visible {
        state.dismiss()
    } else {
        state.show(tx)
    }
}

fn key(state: &mut Launcher, event: &KeyEvent, tx: &mut Tx) -> bool {
    match &event.key {
        Key::Escape => state.dismiss(),
        Key::Down => state.step(true),
        Key::Up => state.step(false),
        // Tab never leaves the field: with nothing to complete it does nothing.
        Key::Tab => {
            state.accept_completion();
        }
        Key::Right if !event.mods.shift && !event.mods.cmd && !event.mods.alt => {
            return state.accept_completion();
        }
        _ if event.is_command('q') => tx.quit(),
        _ => return false,
    }
    true
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
            ..AppSpec::new(
                "Runelight",
                (WIDTH, SEARCH_HEIGHT),
                || unreachable!(),
                view::view,
            )
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
                change: Change::Search,
                wraps: 0,
                scanning: false,
                visible: false,
                closing: false,
                closed_at: None,
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
