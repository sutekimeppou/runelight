mod apps;
mod complete;
mod config;
mod glow;
mod history;
mod view;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use frizbee::{Matcher, SortStrategy};
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

/// Config reloads on each opening; app scans are spaced from the previous scan's completion.
const SCAN_COOLDOWN: Duration = Duration::from_secs(30);

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
    history_key: Option<String>,
    detail: String,
}

impl Entry {
    fn new(name: String, target: Target) -> Self {
        let sort_name = name.to_lowercase();
        let history_key = target.history_key();
        let detail = match &target {
            Target::Application(_) => String::new(),
            Target::Quicklink(url) | Target::Fallback(url) => url::Url::parse(url)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https"))
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default(),
        };

        Self {
            name,
            sort_name,
            target,
            history_key,
            detail,
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

impl AsRef<str> for Entry {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

/// Applications come first, followed by quicklinks, then fallbacks.
fn build_entries(apps: impl IntoIterator<Item = Entry>, config: &config::Config) -> Arc<[Entry]> {
    let mut entries: Vec<_> = apps.into_iter().collect();

    entries.extend(
        config
            .quicklinks
            .iter()
            .map(|link| Entry::new(link.name.clone(), Target::Quicklink(link.url.clone()))),
    );

    entries.extend(
        config
            .fallbacks
            .iter()
            .map(|link| Entry::new(link.name.clone(), Target::Fallback(link.url.clone()))),
    );

    entries.into()
}

struct Launcher {
    me: AppHandle<Self>,
    query: TextEdit,
    config_path: PathBuf,
    config: Arc<config::Config>,
    history: history::History,
    shortcut: Option<(Shortcut, ShortcutId)>,
    entries: Arc<[Entry]>,
    searchable_count: usize,
    results: Vec<usize>,
    selected: usize,
    first_visible: usize,
    change: Change,
    /// Bumped when the selection wraps around, so the highlight enters at the other end
    /// instead of sweeping across every row.
    wraps: u32,
    refreshing: bool,
    catalogue_ready: bool,
    last_scan: Option<Instant>,
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
            Ok(config) => self.config = Arc::new(config),
            Err(error) => eprintln!("runelight: {error:#}"),
        }

        if let Err(error) = self.bind_hotkey() {
            eprintln!("runelight: {error:#}");
        }

        self.replace_entries(build_entries([], &self.config));
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

    fn replace_entries(&mut self, entries: Arc<[Entry]>) {
        let selected = self.selected_entry().cloned();

        self.searchable_count =
            entries.partition_point(|entry| !matches!(entry.target, Target::Fallback(_)));
        self.entries = entries;

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

        let history_scores = self.history.scores(&query);

        // Runelight applies its own history-aware ordering after the batch match.
        let config = complete::config().sort(SortStrategy::IndexAsc);
        let mut matcher = Matcher::new(query.as_str(), &config);
        let mut matches: Vec<_> = matcher
            .match_list(&self.entries[..self.searchable_count])
            .into_iter()
            .map(|matched| {
                let index = matched.index as usize;
                let usage = self.entries[index]
                    .history_key
                    .as_deref()
                    .and_then(|target| history_scores.get(target))
                    .copied()
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

        self.results.clear();
        self.results
            .extend(matches.into_iter().map(|(index, _, _)| index));
        if self.results.is_empty() && self.catalogue_ready {
            self.results
                .extend(self.searchable_count..self.entries.len());
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
        if self.refreshing {
            return;
        }

        self.refreshing = true;
        let scan_apps = self
            .last_scan
            .is_none_or(|scanned| scanned.elapsed() >= SCAN_COOLDOWN);

        let me = self.me.clone();
        let config_path = self.config_path.clone();
        let previous_config = self.config.clone();
        let previous_entries = self.entries.clone();

        std::thread::spawn(move || {
            let (config, config_valid) = match config::load(&config_path) {
                Ok(config) if config == *previous_config => (previous_config.clone(), true),
                Ok(config) => (Arc::new(config), true),
                Err(error) => {
                    eprintln!("runelight: {error:#}");
                    (previous_config.clone(), false)
                }
            };

            let discovery = scan_apps.then(apps::discover);
            let can_prune = config_valid
                && discovery
                    .as_ref()
                    .is_some_and(|discovery| discovery.complete);

            // Discovery is path-sorted. Compare the snapshot before rebuilding derived fields.
            let app_count = previous_entries
                .partition_point(|entry| matches!(entry.target, Target::Application(_)));
            let apps_changed = discovery.as_ref().is_some_and(|discovery| {
                discovery.apps.len() != app_count
                    || discovery
                        .apps
                        .iter()
                        .zip(&previous_entries[..app_count])
                        .any(|(app, entry)| {
                            app.name != entry.name
                                || !matches!(&entry.target, Target::Application(path) if *path == app.path)
                        })
            });

            let entries = match discovery {
                Some(discovery) if apps_changed => Some(build_entries(
                    discovery
                        .apps
                        .into_iter()
                        .map(|app| Entry::new(app.name, Target::Application(app.path))),
                    &config,
                )),
                _ if config.quicklinks != previous_config.quicklinks
                    || config.fallbacks != previous_config.fallbacks =>
                {
                    Some(build_entries(
                        previous_entries[..app_count].iter().cloned(),
                        &config,
                    ))
                }
                _ => None,
            };

            let targets = can_prune.then(|| {
                entries
                    .as_ref()
                    .unwrap_or(&previous_entries)
                    .iter()
                    .filter_map(|entry| entry.history_key.clone())
                    .collect()
            });

            me.update_if(move |state, tx| {
                tx.motion(RESIZE_MOTION);
                state.refreshing = false;
                let became_ready = scan_apps && !state.catalogue_ready;
                if scan_apps {
                    state.last_scan = Some(Instant::now());
                    state.catalogue_ready = true;
                }

                state.config = config;
                if let Err(error) = state.bind_hotkey() {
                    eprintln!("runelight: {error:#}");
                }

                let changed = entries.is_some() || became_ready;
                if let Some(entries) = entries {
                    state.replace_entries(entries);
                } else if became_ready {
                    state.search();
                }

                if let Some(targets) = targets {
                    state.history.prune(&targets);
                }

                changed
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
                    if let Some(target) = entry.history_key {
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
        Key::Text(text)
            if event.mods.ctrl
                && !event.mods.cmd
                && !event.mods.alt
                && !event.mods.shift
                && matches!(text.as_str(), "n" | "p") =>
        {
            state.step(text == "n");
        }
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
            .compositor(glow::BorderLight::default())
            .srgb_blend(true);

        let me = app.handle();
        let ready = me.clone();

        let app = app
            .init(move || Launcher {
                me,
                config_path,
                query: TextEdit::new(""),
                config: Arc::new(config::Config::default()),
                history,
                shortcut: None,
                entries: Arc::from([]),
                searchable_count: 0,
                results: Vec::new(),
                selected: 0,
                first_visible: 0,
                change: Change::Search,
                wraps: 0,
                refreshing: false,
                catalogue_ready: false,
                last_scan: None,
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
