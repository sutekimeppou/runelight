use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSBundle, NSFileManager, NSString};

pub struct Application {
    pub name: String,
    pub path: PathBuf,
}

pub struct Discovery {
    pub apps: Vec<Application>,
    pub complete: bool,
}

pub fn discover() -> Discovery {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/CoreServices/Applications"),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    let mut complete = true;
    for root in roots {
        complete &= collect(&root, &mut apps, &mut seen);
    }
    Discovery { apps, complete }
}

fn collect(directory: &Path, apps: &mut Vec<Application>, seen: &mut HashSet<PathBuf>) -> bool {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(error) => {
            eprintln!("runelight: cannot scan {}: {error}", directory.display());
            return false;
        }
    };
    let mut complete = true;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!(
                    "runelight: cannot read entry in {}: {error}",
                    directory.display()
                );
                complete = false;
                continue;
            }
        };
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("runelight: cannot resolve {}: {error}", path.display());
                complete = false;
                continue;
            }
        };
        if !seen.insert(canonical) {
            continue;
        }
        let attributes = match fs::metadata(&path) {
            Ok(attributes) => attributes,
            Err(error) => {
                eprintln!("runelight: cannot inspect {}: {error}", path.display());
                complete = false;
                continue;
            }
        };
        if !attributes.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "app") {
            match metadata(&path) {
                Some(app) => apps.push(app),
                None => {
                    eprintln!("runelight: unreadable app bundle {}", path.display());
                    complete = false;
                }
            }
        } else if !path.join("Contents/Info.plist").is_file() {
            // Renamed app bundles, including updater backups, still contain helper apps.
            // Recurse through ordinary folders only.
            complete &= collect(&path, apps, seen);
        }
    }
    complete
}

fn metadata(path: &Path) -> Option<Application> {
    autoreleasepool(|_| {
        let string = NSString::from_str(&path.to_string_lossy());
        NSBundle::bundleWithPath(&string)?;
        let display_name = NSFileManager::defaultManager()
            .displayNameAtPath(&string)
            .to_string();
        Some(Application {
            name: display_name
                .strip_suffix(".app")
                .unwrap_or(&display_name)
                .to_owned(),
            path: path.to_owned(),
        })
    })
}
