use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterProfile {
    #[serde(default = "default_profile_name")]
    pub name: String,

    pub bt_mac: String,
    pub bt_channel: u8,

    pub dpi: u32,
    pub max_width_px: u32,
    pub threshold: u8,
    #[serde(default)]
    pub use_dithering: bool,
    #[serde(default = "default_render_scale")]
    pub render_scale: u32,
    #[serde(default = "default_sharpen")]
    pub sharpen: f32,
    #[serde(default = "default_slice_lines")]
    pub slice_lines: u32,
    #[serde(default = "default_slice_delay_ms")]
    pub slice_delay_ms: u64,
    #[serde(default = "default_heating_time")]
    pub heating_time: u8,
    #[serde(default = "default_heating_dots")]
    pub heating_dots: u8,
}

fn default_profile_name()   -> String { "My Printer".to_string() }
fn default_render_scale()   -> u32    { 2 }
fn default_sharpen()        -> f32    { 0.7 }
fn default_slice_lines()    -> u32    { 48 }
fn default_slice_delay_ms() -> u64    { 80 }
fn default_heating_time()   -> u8     { 255 }
fn default_heating_dots()   -> u8     { 15 }

impl Default for PrinterProfile {
    fn default() -> Self {
        Self {
            name:           default_profile_name(),
            bt_mac:         "AA:BB:CC:DD:EE:FF".to_string(),
            bt_channel:     1,
            dpi:            203,
            max_width_px:   832,
            threshold:      160,
            use_dithering:  false,
            render_scale:   default_render_scale(),
            sharpen:        default_sharpen(),
            slice_lines:    default_slice_lines(),
            slice_delay_ms: default_slice_delay_ms(),
            heating_time:   default_heating_time(),
            heating_dots:   default_heating_dots(),
        }
    }
}

impl PrinterProfile {
    #[allow(dead_code)]
    pub fn needs_setup(&self) -> bool {
        self.bt_mac == "AA:BB:CC:DD:EE:FF" || self.bt_mac.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub profiles: Vec<PrinterProfile>,
    pub active_profile: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            profiles: vec![PrinterProfile::default()],
            active_profile: 0,
        }
    }
}

impl AppConfig {
    pub fn active(&self) -> &PrinterProfile {
        let i = self.active_profile.min(self.profiles.len().saturating_sub(1));
        &self.profiles[i]
    }

    #[allow(dead_code)]
    pub fn active_mut(&mut self) -> &mut PrinterProfile {
        let i = self.active_profile.min(self.profiles.len().saturating_sub(1));
        &mut self.profiles[i]
    }

    #[allow(dead_code)]
    pub fn needs_setup(&self) -> bool {
        self.active().needs_setup()
    }

    fn path() -> PathBuf {
        let base = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        base.join(".config").join("ghostlabel").join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if path.exists()
            && let Ok(data) = fs::read_to_string(&path)
        {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&data) {
                return cfg;
            }
            if let Ok(profile) = serde_json::from_str::<PrinterProfile>(&data) {
                return AppConfig {
                    profiles: vec![profile],
                    active_profile: 0,
                };
            }
        }
        AppConfig::default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_needs_setup() {
        let p = PrinterProfile::default();
        assert!(p.needs_setup());
    }

    #[test]
    fn configured_profile_does_not_need_setup() {
        let p = PrinterProfile {
            bt_mac: "AA:BB:CC:DD:EE:FF".to_string(),
            ..PrinterProfile::default()
        };
        assert!(p.needs_setup());

        let p2 = PrinterProfile {
            bt_mac: "11:22:33:44:55:66".to_string(),
            ..PrinterProfile::default()
        };
        assert!(!p2.needs_setup());
    }

    #[test]
    fn migration_from_old_flat_format() {
        let old_json = r#"{"bt_mac":"11:22:33:44:55:66","bt_channel":1,"dpi":203,"max_width_px":832,"threshold":140}"#;
        let profile: PrinterProfile = serde_json::from_str(old_json).unwrap();
        assert_eq!(profile.bt_mac, "11:22:33:44:55:66");
        assert_eq!(profile.name, "My Printer");
        assert!(!profile.use_dithering);
    }

    #[test]
    fn active_clamps_to_valid_index() {
        let mut cfg = AppConfig::default();
        cfg.active_profile = 99;
        let _ = cfg.active();
    }
}
