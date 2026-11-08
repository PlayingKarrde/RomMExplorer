use crate::api::Auth;
use crate::library::FolderNaming;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum ViewMode {
    #[default]
    Grid,
    List,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub server_url: String,
    pub username: String,
    pub password: String,
    pub api_token: String,
    pub accept_invalid_certs: bool,
    pub library_dir: Option<PathBuf>,
    pub folder_naming: FolderNaming,
    pub extract_multi_file: bool,
    pub view_mode: ViewMode,
    pub card_width: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            username: String::new(),
            password: String::new(),
            api_token: String::new(),
            accept_invalid_certs: false,
            library_dir: None,
            folder_naming: FolderNaming::default(),
            extract_multi_file: true,
            view_mode: ViewMode::Grid,
            card_width: 168.0,
        }
    }
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("io", "RomMExplorer", "RomMExplorer")
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("ROMM_EXPLORER_CONFIG") {
        return PathBuf::from(p);
    }
    project_dirs()
        .map(|d| d.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from("romm-explorer.json"))
}

pub fn cache_dir() -> PathBuf {
    project_dirs()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("RomMExplorer"))
}

/// Server details from a `.env` file (`server=`, `user=`, `password=`, optional
/// `token=`), looked up in the working directory and next to the executable.
/// Keys may also be prefixed with `ROMM_`.
#[derive(Default, Debug, Clone)]
pub struct DotEnv {
    pub server: String,
    pub user: String,
    pub password: String,
    pub token: String,
}

pub fn load_dotenv() -> Option<DotEnv> {
    let mut candidates = vec![PathBuf::from(".env")];
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        candidates.push(dir.join(".env"));
        // `target/release/` → project root, handy during development.
        if let Some(root) = dir.parent().and_then(|p| p.parent()) {
            candidates.push(root.join(".env"));
        }
    }
    let text = candidates.iter().find_map(|p| std::fs::read_to_string(p).ok())?;
    let mut env = DotEnv::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        let key = k.trim().to_lowercase();
        match key.strip_prefix("romm_").unwrap_or(&key) {
            "server" | "url" | "server_url" => env.server = v,
            "user" | "username" => env.user = v,
            "password" | "pass" => env.password = v,
            "token" | "api_token" => env.token = v,
            _ => {}
        }
    }
    (!env.server.is_empty()).then_some(env)
}

impl Config {
    pub fn load() -> Self {
        let mut cfg: Config = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Pre-fill server details from .env when nothing is configured yet.
        if cfg.server_url.trim().is_empty() {
            if let Some(env) = load_dotenv() {
                cfg.server_url = env.server;
                cfg.username = env.user;
                cfg.password = env.password;
                cfg.api_token = env.token;
            }
        }
        cfg
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn auth(&self) -> Auth {
        if !self.api_token.trim().is_empty() {
            Auth::Token(self.api_token.trim().to_string())
        } else if !self.username.trim().is_empty() {
            Auth::Basic { username: self.username.trim().to_string(), password: self.password.clone() }
        } else {
            Auth::None
        }
    }

    pub fn server_settings_differ(&self, other: &Config) -> bool {
        self.server_url != other.server_url
            || self.username != other.username
            || self.password != other.password
            || self.api_token != other.api_token
            || self.accept_invalid_certs != other.accept_invalid_certs
    }
}
