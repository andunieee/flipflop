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
    /// "strict" | "public" — what to do when a custom relay is unreachable.
    pub relay_fallback: String,
    /// "default" | "custom"
    pub discovery_mode: String,
    pub discovery_pkarr_relay_url: Option<String>,
    pub discovery_dns_origin: Option<String>,
    pub history_enabled: bool,
    /// "everyone" | "paired-only" | "off"
    pub discoverability: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            downloads_dir: None,
            relay_mode: "default".to_string(),
            relay_urls: Vec::new(),
            relay_token: None,
            relay_fallback: "strict".to_string(),
            discovery_mode: "default".to_string(),
            discovery_pkarr_relay_url: None,
            discovery_dns_origin: None,
            history_enabled: true,
            discoverability: "everyone".to_string(),
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

    /// Relay config in the engine's IPC shape, for verify/status/fallback calls.
    pub fn relay_config_arg(&self) -> engine::RelayConfigArg {
        engine::RelayConfigArg {
            mode: self.relay_mode.clone(),
            urls: self.relay_urls.clone(),
            auth_token: self.relay_token.clone().filter(|t| !t.trim().is_empty()),
            fallback: Some(self.relay_fallback.clone()),
        }
    }

    pub fn relay_fallback(&self) -> engine::RelayFallbackPolicy {
        match self.relay_fallback.as_str() {
            "public" => engine::RelayFallbackPolicy::Public,
            _ => engine::RelayFallbackPolicy::Strict,
        }
    }

    /// Discovery config in the engine's IPC shape, for verify/status calls.
    pub fn discovery_config_arg(&self) -> engine::DiscoveryConfigArg {
        engine::DiscoveryConfigArg {
            mode: self.discovery_mode.clone(),
            pkarr_relay_url: self
                .discovery_pkarr_relay_url
                .clone()
                .filter(|s| !s.trim().is_empty()),
            dns_origin: self
                .discovery_dns_origin
                .clone()
                .filter(|s| !s.trim().is_empty()),
        }
    }

    /// Resolved discovery mode; invalid persisted config falls back to default.
    pub fn discovery_mode(&self) -> engine::DiscoveryModeOption {
        engine::build_discovery_mode(Some(self.discovery_config_arg()))
            .unwrap_or(engine::DiscoveryModeOption::Default)
    }

    pub fn discoverability(&self) -> engine::Discoverability {
        match self.discoverability.as_str() {
            "paired-only" => engine::Discoverability::PairedOnly,
            "off" => engine::Discoverability::Off,
            _ => engine::Discoverability::Everyone,
        }
    }

    pub fn downloads_path(&self) -> Option<std::path::PathBuf> {
        self.downloads_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(default_downloads_dir)
    }
}

#[cfg(target_os = "android")]
fn default_downloads_dir() -> Option<std::path::PathBuf> {
    // No system Downloads access without SAF plumbing; keep received files
    // in the app-private downloads folder instead.
    Some(crate::android::downloads_dir())
}

#[cfg(not(target_os = "android"))]
fn default_downloads_dir() -> Option<std::path::PathBuf> {
    dirs::download_dir()
}
