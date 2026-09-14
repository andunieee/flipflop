use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub downloads_dir: Option<String>,
    /// "default" | "disabled" | "custom"
    pub relay_mode: String,
    pub relay_urls: Vec<String>,
    pub relay_token: Option<String>,
    pub history_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            downloads_dir: None,
            relay_mode: "default".to_string(),
            relay_urls: Vec::new(),
            relay_token: None,
            history_enabled: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).expect("settings serialize");
        std::fs::write(path, json)
    }

    pub fn relay_mode(&self) -> engine::RelayModeOption {
        use std::str::FromStr;
        match self.relay_mode.as_str() {
            "disabled" => engine::RelayModeOption::Disabled,
            "custom" => engine::RelayModeOption::Custom {
                urls: self
                    .relay_urls
                    .iter()
                    .filter_map(|raw| {
                        let raw = raw.trim();
                        (!raw.is_empty()).then(|| iroh::RelayUrl::from_str(raw).ok())?
                    })
                    .collect(),
                auth_token: self.relay_token.clone().filter(|t| !t.is_empty()),
            },
            _ => engine::RelayModeOption::Default,
        }
    }

    pub fn downloads_path(&self) -> Option<std::path::PathBuf> {
        self.downloads_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(dirs::download_dir)
    }
}
