use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread;

#[derive(Debug, Clone)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
}

impl BtDevice {
    pub fn looks_like_printer(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        ["label", "print", "thermal", "y813", "knaon", "hprt", "xprint", "rongta", "zjiang"]
            .iter()
            .any(|kw| n.contains(kw))
    }
}

fn parse_devices(output: &str) -> Vec<BtDevice> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
            if parts.len() == 3 && parts[0] == "Device" {
                Some(BtDevice {
                    mac: parts[1].to_string(),
                    name: parts[2].to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

pub fn list_paired() -> Vec<BtDevice> {
    let Ok(out) = Command::new("bluetoothctl").arg("devices").output() else {
        return vec![];
    };
    parse_devices(&String::from_utf8_lossy(&out.stdout))
}

pub fn scan_async(tx: Sender<Vec<BtDevice>>, timeout_secs: u64) {
    thread::spawn(move || {
        let initial = list_paired();
        let _ = tx.send(initial);

        let _ = Command::new("bluetoothctl")
            .args(["--timeout", &timeout_secs.to_string(), "scan", "on"])
            .status();

        let final_list = list_paired();
        let _ = tx.send(final_list);
    });
}

#[allow(dead_code)]
pub fn is_valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices_standard_line() {
        let output = "Device AA:BB:CC:DD:EE:FF My Printer\n";
        let devices = parse_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(devices[0].name, "My Printer");
    }

    #[test]
    fn parse_devices_multi_word_name() {
        let output = "Device 11:22:33:44:55:66 KNAON Y813BT-XXXX\n";
        let devices = parse_devices(output);
        assert_eq!(devices[0].name, "KNAON Y813BT-XXXX");
    }

    #[test]
    fn parse_devices_skips_non_device_lines() {
        let output = "Controller 00:11:22:33:44:55 hostname [default]\nDevice AA:BB:CC:DD:EE:FF Printer\n";
        let devices = parse_devices(output);
        assert_eq!(devices.len(), 1);
    }

    #[test]
    fn is_valid_mac_accepts_correct_format() {
        assert!(is_valid_mac("AA:BB:CC:DD:EE:FF"));
        assert!(is_valid_mac("00:00:00:00:00:00"));
        assert!(is_valid_mac("1a:2b:3c:4d:5e:6f"));
    }

    #[test]
    fn is_valid_mac_rejects_bad_formats() {
        assert!(!is_valid_mac("AA-BB-CC-DD-EE-FF"));
        assert!(!is_valid_mac("AA:BB:CC:DD:EE"));
        assert!(!is_valid_mac("AABBCCDDEEFF"));
        assert!(!is_valid_mac("GG:BB:CC:DD:EE:FF"));
        assert!(!is_valid_mac(""));
    }

    #[test]
    fn looks_like_printer_matches_known_names() {
        let dev = BtDevice { mac: "AA:BB:CC:DD:EE:FF".into(), name: "KNAON Y813BT".into() };
        assert!(dev.looks_like_printer());
        let dev2 = BtDevice { mac: "AA:BB:CC:DD:EE:FF".into(), name: "My Keyboard".into() };
        assert!(!dev2.looks_like_printer());
    }
}
