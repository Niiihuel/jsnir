//! Project configuration (`jsnir.toml`) with zero-config defaults.
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CONFIG_FILES: &[&str] = &["jsnir.toml"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypecheckMode {
    #[default]
    Async,
    Blocking,
    Off,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub entry: PathBuf,
    pub title: String,
    pub size: [u32; 2],
    pub args: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            entry: PathBuf::from("src/app.tsx"),
            title: String::from("JSNIR"),
            size: [960, 640],
            args: String::from("{}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DevSettings {
    pub typecheck: TypecheckMode,
    pub overlay: bool,
    pub clear_screen: bool,
    pub debounce_ms: u64,
}

impl Default for DevSettings {
    fn default() -> Self {
        Self {
            typecheck: TypecheckMode::Async,
            overlay: true,
            clear_screen: false,
            debounce_ms: 40,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AssetsConfig {
    pub fonts: Vec<PathBuf>,
    pub images_dir: Option<PathBuf>,
    pub default_family: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub app: AppConfig,
    pub dev: DevSettings,
    pub assets: AssetsConfig,
}

/// Resolved configuration used by both the `jsnir` CLI and embedded Rust hosts (`jsnir_dev::start_dev`).
#[derive(Debug, Clone)]
pub struct DevConfig {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub app: AppConfig,
    pub dev: DevSettings,
    pub assets: AssetsConfig,
}

impl DevConfig {
    /// Loads `jsnir.toml` starting from `entry_or_dir` (or current dir), falling back to zero-config defaults.
    pub fn discover(entry_or_dir: Option<&Path>) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|e| format!("current_dir: {e}"))?;
        let target = entry_or_dir.map(|p| if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) });

        let (search_dir, explicit_entry) = match &target {
            Some(path) if path.is_file() => (
                path.parent().unwrap_or(&cwd).to_path_buf(),
                Some(path.clone()),
            ),
            Some(path) if path.is_dir() => (path.clone(), None),
            Some(path) => {
                // Non-existent path: treat as an entry file path if it has an extension, else directory.
                if path.extension().is_some() {
                    (
                        path.parent().unwrap_or(&cwd).to_path_buf(),
                        Some(path.clone()),
                    )
                } else {
                    (path.clone(), None)
                }
            }
            None => (cwd.clone(), None),
        };

        let config_path = find_config(&search_dir);
        let root = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| search_dir.clone());

        let mut parsed = match &config_path {
            Some(path) => load_file_config(path)?,
            None => FileConfig::default(),
        };

        if let Some(entry) = explicit_entry {
            parsed.app.entry = entry;
        } else {
            parsed.app.entry = resolve_path(&root, &parsed.app.entry);
        }

        parsed.assets.fonts = parsed
            .assets
            .fonts
            .into_iter()
            .map(|p| resolve_path(&root, &p))
            .collect();
        parsed.assets.images_dir = parsed
            .assets
            .images_dir
            .map(|p| resolve_path(&root, &p));

        Ok(Self {
            root,
            config_path,
            app: parsed.app,
            dev: parsed.dev,
            assets: parsed.assets,
        })
    }

    /// Creates a minimal configuration directly from an entry file (ideal for embedded Rust apps).
    pub fn for_entry(entry: impl Into<PathBuf>) -> Self {
        let entry = entry.into();
        let root = entry
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let config_path = find_config(&root);
        let mut cfg = Self {
            root,
            config_path,
            app: AppConfig {
                entry,
                ..Default::default()
            },
            dev: DevSettings::default(),
            assets: AssetsConfig::default(),
        };
        if let Some(path) = cfg.config_path.clone()
            && let Ok(file) = load_file_config(&path)
        {
            let root = path.parent().unwrap_or(&cfg.root).to_path_buf();
            cfg.root = root.clone();
            cfg.dev = file.dev;
            cfg.assets.fonts = file
                .assets
                .fonts
                .into_iter()
                .map(|p| resolve_path(&root, &p))
                .collect();
            cfg.assets.images_dir = file
                .assets
                .images_dir
                .map(|p| resolve_path(&root, &p));
            cfg.assets.default_family = file.assets.default_family;
        }
        cfg
    }

    /// Reloads `jsnir.toml` if this config came from one.
    pub fn reload_from_disk(&self) -> Result<Self, String> {
        let Some(path) = &self.config_path else {
            return Ok(self.clone());
        };
        let mut parsed = load_file_config(path)?;
        let root = path.parent().unwrap_or(&self.root).to_path_buf();
        parsed.app.entry = resolve_path(&root, &parsed.app.entry);
        parsed.assets.fonts = parsed
            .assets
            .fonts
            .into_iter()
            .map(|p| resolve_path(&root, &p))
            .collect();
        parsed.assets.images_dir = parsed
            .assets
            .images_dir
            .map(|p| resolve_path(&root, &p));
        Ok(Self {
            root,
            config_path: Some(path.clone()),
            app: parsed.app,
            dev: parsed.dev,
            assets: parsed.assets,
        })
    }
}

pub fn load_file_config(path: &Path) -> Result<FileConfig, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str::<FileConfig>(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        for name in CONFIG_FILES {
            let candidate = current.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        dir = current.parent();
    }
    None
}

fn resolve_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub fn parse_size(value: &str) -> Result<[u32; 2], String> {
    let (w, h) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .ok_or_else(|| format!("invalid size {value:?}: expected WIDTHxHEIGHT (e.g. 1024x700)"))?;
    let width: u32 = w
        .parse()
        .map_err(|_| format!("invalid width in {value:?}"))?;
    let height: u32 = h
        .parse()
        .map_err(|_| format!("invalid height in {value:?}"))?;
    if width == 0 || height == 0 {
        return Err("window dimensions must be positive".into());
    }
    Ok([width, height])
}
