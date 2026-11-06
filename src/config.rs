use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rune::Shortcut;
use serde::Deserialize;
use url::Url;

pub struct Config {
    pub hotkey: String,
    pub quicklinks: Vec<Quicklink>,
    pub fallbacks: Vec<Quicklink>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    hotkey: Option<String>,
    quicklinks: Vec<Quicklink>,
    fallbacks: Vec<Quicklink>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quicklink {
    pub name: String,
    pub url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "alt+space".into(),
            quicklinks: Vec::new(),
            fallbacks: Vec::new(),
        }
    }
}

pub fn path() -> Result<PathBuf> {
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("Could not locate your home directory")?;
    Ok(root.join("runelight/config.json"))
}

pub fn load(path: &Path) -> Result<Config> {
    let mut config = Config::default();
    for file in [path.to_owned(), path.with_file_name("config.local.json")] {
        let layer = read(&file).with_context(|| file.display().to_string())?;
        if let Some(hotkey) = layer.hotkey {
            config.hotkey = hotkey;
        }
        config.quicklinks.extend(layer.quicklinks);
        config.fallbacks.extend(layer.fallbacks);
    }
    Ok(config)
}

fn read(path: &Path) -> Result<ConfigFile> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(error) => return Err(error).context("Could not read config"),
    };
    let config: ConfigFile = serde_json::from_slice(&contents).context("Invalid config JSON")?;
    if let Some(hotkey) = &config.hotkey {
        hotkey
            .parse::<Shortcut>()
            .context("Invalid hotkey")?
            .require_modifier()
            .context("Hotkey needs Command, Control, or Option")?;
    }
    for link in &config.quicklinks {
        ensure!(
            !link.name.trim().is_empty(),
            "Quicklink name cannot be empty"
        );
        let url = Url::parse(&link.url)
            .with_context(|| format!("Invalid URL for quicklink {:?}", link.name))?;
        ensure!(
            matches!(url.scheme(), "http" | "https" | "mailto"),
            "Quicklink {:?} needs an http, https, or mailto URL",
            link.name
        );
    }
    for fallback in &config.fallbacks {
        ensure!(
            !fallback.name.trim().is_empty(),
            "Fallback name cannot be empty"
        );
        ensure!(
            fallback.url.contains("{query}"),
            "Fallback {:?} URL needs a {{query}} placeholder",
            fallback.name
        );
        let url = Url::parse(&fallback.url)
            .with_context(|| format!("Invalid URL for fallback {:?}", fallback.name))?;
        ensure!(
            matches!(url.scheme(), "http" | "https"),
            "Fallback {:?} needs an http or https URL",
            fallback.name
        );
    }
    Ok(config)
}
