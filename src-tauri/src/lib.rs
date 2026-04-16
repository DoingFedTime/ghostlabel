mod bt_scan;
mod config;
mod printer;

use bt_scan::{scan_async, BtDevice};
use config::AppConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

struct AppState {
    config: Mutex<AppConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
    pub path: String,
    pub page: u32,
}

#[tauri::command]
fn get_config(state: State<AppState>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn save_config(config: AppConfig, state: State<AppState>) {
    config.save();
    *state.config.lock().unwrap() = config;
}

#[derive(Clone, Serialize)]
struct DeviceList {
    devices: Vec<DeviceInfo>,
    scanning: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub mac: String,
    pub name: String,
    pub looks_like_printer: bool,
}

impl From<BtDevice> for DeviceInfo {
    fn from(d: BtDevice) -> Self {
        let looks = d.looks_like_printer();
        Self { mac: d.mac, name: d.name, looks_like_printer: looks }
    }
}

#[tauri::command]
fn list_devices() -> Vec<DeviceInfo> {
    bt_scan::list_paired().into_iter().map(DeviceInfo::from).collect()
}

#[tauri::command]
async fn scan_devices(app: AppHandle, timeout_secs: u64) {
    let (tx, rx) = std::sync::mpsc::channel();
    scan_async(tx, timeout_secs);

    std::thread::spawn(move || {
        loop {
            match rx.recv() {
                Ok(devices) => {
                    let info: Vec<DeviceInfo> =
                        devices.into_iter().map(DeviceInfo::from).collect();
                    let _ = app.emit("scan-update", DeviceList { devices: info, scanning: true });
                }
                Err(_) => {
                    let _ = app.emit(
                        "scan-update",
                        DeviceList { devices: vec![], scanning: false },
                    );
                    break;
                }
            }
        }
    });
}

#[tauri::command]
fn test_connection(mac: String, channel: u8) -> Result<String, String> {
    printer::connection_test(&mac, channel)
}

#[tauri::command]
fn pdf_page_count(path: String) -> u32 {
    printer::pdf_page_count(Path::new(&path))
}

#[tauri::command]
fn render_preview(path: String, page: u32) -> Result<String, String> {
    let (w, h, pixels) = printer::render_preview(Path::new(&path), page)?;

    let mut rgba: Vec<u8> = Vec::with_capacity((w * h * 4) as usize);
    for px in &pixels {
        rgba.extend_from_slice(px);
    }

    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let enc = image::codecs::png::PngEncoder::new(&mut png_bytes);
        use image::ImageEncoder;
        enc.write_image(&rgba, w, h, image::ColorType::Rgba8.into())
            .map_err(|e| e.to_string())?;
    }

    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}

#[derive(Clone, Serialize)]
struct PrintEvent {
    id: usize,
    status: String,
    message: String,
}

#[tauri::command]
async fn print_pdf(
    app: AppHandle,
    id: usize,
    path: String,
    page: u32,
    dry_run: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let profile = state.config.lock().unwrap().active().clone();
    let path_buf = PathBuf::from(&path);

    let _ = app.emit(
        "print-event",
        PrintEvent {
            id,
            status: "printing".into(),
            message: format!(
                "Processing {}",
                path_buf.file_name().and_then(|n| n.to_str()).unwrap_or(&path)
            ),
        },
    );

    let result = if dry_run {
        printer::dry_run_pdf(&path_buf, &profile, page).map(|msg| {
            let _ = app.emit("print-event", PrintEvent { id, status: "dryrun".into(), message: msg });
        })
    } else {
        let app2 = app.clone();
        printer::print_pdf_with_retry(
            &path_buf, &profile, page,
            5,
            3000,
            move |attempt, err| {
                let _ = app2.emit("print-event", PrintEvent {
                    id,
                    status: "printing".into(),
                    message: format!("Retry {}/4 - {}", attempt, err),
                });
            },
        ).map(|_| {
            let _ = app.emit("print-event", PrintEvent { id, status: "done".into(), message: "Printed ✓".into() });
        })
    };

    if let Err(e) = result {
        let _ = app.emit("print-event", PrintEvent { id, status: "error".into(), message: e });
    }

    Ok(())
}

fn queue_path() -> PathBuf {
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(".config").join("ghostlabel").join("queue.json")
}

#[tauri::command]
fn load_queue() -> Vec<QueueEntry> {
    let path = queue_path();
    if path.exists() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(q) = serde_json::from_str(&data) {
                return q;
            }
        }
    }
    vec![]
}

#[tauri::command]
fn save_queue(queue: Vec<QueueEntry>) {
    let path = queue_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(&queue) {
        let _ = std::fs::write(path, data);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            config: Mutex::new(AppConfig::load()),
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_devices,
            scan_devices,
            test_connection,
            pdf_page_count,
            render_preview,
            print_pdf,
            load_queue,
            save_queue,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ghostlabel");
}
