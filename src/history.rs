use std::{
    collections::{HashMap, HashSet},
    env, fs, io,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_QUERIES: usize = 500;

type Queries = Vec<Query>;

#[derive(Clone, Deserialize, Serialize)]
struct Query {
    text: String,
    targets: HashMap<String, u64>,
}

#[derive(Deserialize)]
struct PreviousQuery {
    targets: HashMap<String, u64>,
    last_used: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedHistory {
    Current(Queries),
    Previous(HashMap<String, PreviousQuery>),
    Legacy(HashMap<String, HashMap<String, u64>>),
}

#[derive(Default)]
pub struct History {
    queries: Queries,
    writer: Option<Sender<Queries>>,
}

impl History {
    pub fn load() -> Result<Self> {
        let root = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .context("Could not locate your home directory")?;
        let path = root.join("runelight/history.json");
        let queries = match fs::read(&path) {
            Ok(bytes) => {
                match serde_json::from_slice(&bytes)
                    .with_context(|| format!("Could not parse {}", path.display()))?
                {
                    SavedHistory::Current(queries) => queries,
                    SavedHistory::Previous(queries) => {
                        let mut queries: Vec<_> = queries.into_iter().collect();
                        queries.sort_unstable_by(|a, b| {
                            a.1.last_used
                                .cmp(&b.1.last_used)
                                .then_with(|| a.0.cmp(&b.0))
                        });
                        queries
                            .into_iter()
                            .map(|(text, query)| Query {
                                text,
                                targets: query.targets,
                            })
                            .collect()
                    }
                    SavedHistory::Legacy(counts) => counts
                        .into_iter()
                        .map(|(text, targets)| Query { text, targets })
                        .collect(),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Queries::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("Could not read {}", path.display()));
            }
        };
        let (writer, snapshots) = mpsc::channel();
        // One writer keeps successful launches ordered and disk I/O off the UI thread.
        std::thread::spawn(move || {
            for queries in snapshots {
                if let Err(error) = save(&path, &queries) {
                    eprintln!("runelight: could not save launch history: {error:#}");
                }
            }
        });
        let mut history = Self {
            queries,
            writer: Some(writer),
        };
        if history.trim() {
            history.persist();
        }
        Ok(history)
    }

    pub fn score(&self, query: &str, target: &str) -> (u64, u64) {
        let mut exact = 0;
        let mut prefix: u64 = 0;
        for entry in &self.queries {
            if entry.text == query {
                exact = entry.targets.get(target).copied().unwrap_or(0);
            } else if entry.text.starts_with(query) {
                prefix = prefix.saturating_add(entry.targets.get(target).copied().unwrap_or(0));
            }
        }
        // Exact-query choices rank ahead of preferences inherited from longer queries.
        (exact, prefix)
    }

    pub fn record(&mut self, query: String, target: String) {
        let text = query.trim().to_lowercase();
        if text.is_empty() {
            return;
        }
        let mut query = self
            .queries
            .iter()
            .position(|query| query.text == text)
            .map(|index| self.queries.remove(index))
            .unwrap_or_else(|| Query {
                text,
                targets: HashMap::new(),
            });
        let count = query.targets.entry(target).or_default();
        *count = count.saturating_add(1);
        self.queries.push(query);
        self.trim();
        self.persist();
    }

    pub fn prune(&mut self, targets: &HashSet<String>) {
        let mut changed = false;
        self.queries.retain_mut(|query| {
            let before = query.targets.len();
            query.targets.retain(|target, _| targets.contains(target));
            changed |= before != query.targets.len() || query.targets.is_empty();
            !query.targets.is_empty()
        });
        if changed {
            self.persist();
        }
    }

    fn trim(&mut self) -> bool {
        let excess = self.queries.len().saturating_sub(MAX_QUERIES);
        self.queries.drain(..excess);
        excess > 0
    }

    fn persist(&self) {
        if let Some(writer) = &self.writer
            && let Err(error) = writer.send(self.queries.clone())
        {
            eprintln!("runelight: launch history writer stopped: {error}");
        }
    }
}

fn save(path: &Path, queries: &Queries) -> Result<()> {
    let parent = path.parent().context("History path has no parent")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .with_context(|| format!("Could not create {}", parent.display()))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("Could not open {}", temporary.display()))?;
    file.write_all(&serde_json::to_vec(queries)?)
        .with_context(|| format!("Could not write {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("Could not replace {}", path.display()))?;
    Ok(())
}
