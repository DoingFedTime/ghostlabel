use image::{GrayImage, ImageReader, Luma};
use std::io::{self, Write};
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::{fs, thread};

use crate::config::PrinterProfile;

fn parse_mac(mac: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 {
        return Err(format!("Invalid MAC address: {mac}"));
    }
    let mut bytes = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        bytes[i] = u8::from_str_radix(part, 16)
            .map_err(|_| format!("Invalid MAC byte: {part}"))?;
    }
    bytes.reverse();
    Ok(bytes)
}

fn bt_connect(mac: &str, channel: u8) -> io::Result<std::fs::File> {
    #[repr(C)]
    struct SockaddrRc {
        rc_family: libc::sa_family_t,
        rc_bdaddr: [u8; 6],
        rc_channel: u8,
    }

    let bdaddr = parse_mac(mac)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let fd = unsafe { libc::socket(31, libc::SOCK_STREAM, 3) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let tv = libc::timeval { tv_sec: 10, tv_usec: 0 };
    unsafe {
        libc::setsockopt(
            fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }

    let addr = SockaddrRc { rc_family: 31, rc_bdaddr: bdaddr, rc_channel: channel };
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const SockaddrRc as *const libc::sockaddr,
            std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        unsafe { libc::close(fd); }
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub fn connection_test(mac: &str, channel: u8) -> Result<String, String> {
    let mut sock = bt_connect(mac, channel)
        .map_err(|e| format!("Connect failed: {e}"))?;
    sock.write_all(b"\x1b\x40")
        .map_err(|e| format!("Write failed: {e}"))?;
    sock.flush().ok();
    Ok("Connected".to_string())
}

pub fn pdf_page_count(pdf_path: &Path) -> u32 {
    let out = Command::new("pdfinfo").arg(pdf_path).output().ok();
    out.and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.starts_with("Pages:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
    })
    .unwrap_or(1)
}

pub fn pdf_to_gray(pdf_path: &Path, dpi: u32, page: u32) -> Result<GrayImage, String> {
    let tmp = tempfile::Builder::new()
        .prefix("ghostlabel_")
        .tempdir()
        .map_err(|e| e.to_string())?;

    let out_base = tmp.path().join("label");
    let page_str = page.to_string();

    let status = Command::new("pdftoppm")
        .args([
            "-r", &dpi.to_string(),
            "-png",
            "-f", &page_str, "-l", &page_str,
            pdf_path.to_str().unwrap_or(""),
            out_base.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("pdftoppm not found - install poppler-utils: {e}"))?;

    if !status.success() {
        return Err(format!("pdftoppm failed on {} (page {page})", pdf_path.display()));
    }

    let files: Vec<_> = fs::read_dir(tmp.path())
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .collect();

    if files.is_empty() {
        return Err("pdftoppm produced no output".to_string());
    }

    let img = ImageReader::open(files[0].path())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?
        .into_luma8();

    Ok(img)
}

pub fn render_preview(pdf_path: &Path, page: u32) -> Result<(u32, u32, Vec<[u8; 4]>), String> {
    let img = pdf_to_gray(pdf_path, 72, page)?;
    let (w, h) = img.dimensions();
    let pixels: Vec<[u8; 4]> = img
        .pixels()
        .map(|Luma([p])| {
            let v = if *p < 128 { 0u8 } else { 255u8 };
            [v, v, v, 255]
        })
        .collect();
    Ok((w, h, pixels))
}

fn unsharp_mask(img: &mut GrayImage, amount: f32) {
    let (w, h) = img.dimensions();
    let w = w as usize;
    let h = h as usize;
    let src: Vec<i16> = img.pixels().map(|Luma([p])| *p as i16).collect();

    let mut blurred = vec![0i16; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0i32;
            let mut count = 0i32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && nx < w as i32 && ny >= 0 && ny < h as i32 {
                        sum += src[ny as usize * w + nx as usize] as i32;
                        count += 1;
                    }
                }
            }
            blurred[y * w + x] = (sum / count) as i16;
        }
    }

    for (i, Luma([p])) in img.pixels_mut().enumerate() {
        let sharp = src[i] as f32 + amount * (src[i] as f32 - blurred[i] as f32);
        *p = sharp.clamp(0.0, 255.0) as u8;
    }
}

fn autocontrast(img: &mut GrayImage, cutoff_pct: f32) {
    let mut hist = [0u64; 256];
    for Luma([p]) in img.pixels() { hist[*p as usize] += 1; }
    let total = img.width() as u64 * img.height() as u64;
    let cut = (total as f32 * cutoff_pct / 100.0) as u64;

    let mut lo = 0usize;
    let mut acc = 0u64;
    while lo < 255 {
        acc += hist[lo];
        if acc > cut { break; }
        lo += 1;
    }
    let mut hi = 255usize;
    acc = 0;
    while hi > 0 {
        acc += hist[hi];
        if acc > cut { break; }
        hi -= 1;
    }
    if hi <= lo { return; }
    let scale = 255.0 / (hi - lo) as f32;
    for Luma([p]) in img.pixels_mut() {
        let v = (*p as isize - lo as isize).clamp(0, (hi - lo) as isize);
        *p = (v as f32 * scale).clamp(0.0, 255.0) as u8;
    }
}

pub fn dither_floyd_steinberg(img: &mut GrayImage, threshold: u8) {
    let (w, h) = img.dimensions();
    let w = w as usize;
    let h = h as usize;

    let mut buf: Vec<i16> = img.pixels().map(|Luma([p])| *p as i16).collect();

    for y in 0..h {
        for x in 0..w {
            let old = buf[y * w + x];
            let new_val: i16 = if old < threshold as i16 { 0 } else { 255 };
            buf[y * w + x] = new_val;
            let err = old - new_val;

            let push = |buf: &mut Vec<i16>, nx: usize, ny: usize, weight: i16| {
                if nx < w && ny < h {
                    buf[ny * w + nx] = (buf[ny * w + nx] + err * weight / 16).clamp(0, 255);
                }
            };
            push(&mut buf, x + 1,             y,     7);
            push(&mut buf, x.wrapping_sub(1), y + 1, 3);
            push(&mut buf, x,                 y + 1, 5);
            push(&mut buf, x + 1,             y + 1, 1);
        }
    }

    for (pixel, &val) in img.pixels_mut().zip(buf.iter()) {
        pixel.0[0] = val as u8;
    }
}

pub fn image_to_escpos(img: &GrayImage, max_width: u32, threshold: u8, use_dithering: bool, sharpen: f32) -> Vec<u8> {
    let (orig_w, orig_h) = img.dimensions();

    let (w, h, scaled) = if orig_w > max_width {
        let new_h = (orig_h as f32 * max_width as f32 / orig_w as f32) as u32;
        let resized = image::imageops::resize(
            img, max_width, new_h, image::imageops::FilterType::Lanczos3,
        );
        (max_width, new_h, resized)
    } else {
        (orig_w, orig_h, img.clone())
    };

    let mut work = scaled;
    autocontrast(&mut work, 1.0);
    if sharpen > 0.0 { unsharp_mask(&mut work, sharpen); }

    if use_dithering {
        dither_floyd_steinberg(&mut work, threshold);
    }

    let row_bytes = w.div_ceil(8);
    let mut buf = Vec::with_capacity((row_bytes * h + 64) as usize);

    buf.extend_from_slice(b"\x1b\x40");
    buf.extend_from_slice(b"\x1b\x32");

    let m: u8 = 0;
    buf.extend_from_slice(&[
        0x1d, 0x76, 0x30, m,
        (row_bytes & 0xFF) as u8, (row_bytes >> 8) as u8,
        (h & 0xFF) as u8, (h >> 8) as u8,
    ]);

    for y in 0..h {
        for byte_idx in 0..row_bytes {
            let mut byte: u8 = 0;
            for bit in 0..8u32 {
                let x = byte_idx * 8 + bit;
                if x < w {
                    let Luma([px]) = *work.get_pixel(x, y);
                    if px < threshold {
                        byte |= 0x80 >> bit;
                    }
                }
            }
            buf.push(byte);
        }
    }

    buf.extend_from_slice(b"\x1b\x64\x04");
    buf.extend_from_slice(b"\x1d\x56\x41\x03");
    buf
}

fn print_pdf_once(pdf_path: &Path, profile: &PrinterProfile, page: u32) -> Result<(), String> {
    let render_dpi = profile.dpi * profile.render_scale;
    let img = pdf_to_gray(pdf_path, render_dpi, page)?;

    let (orig_w, orig_h) = img.dimensions();
    let target_w = profile.max_width_px;
    let target_h = (orig_h as f64 * target_w as f64 / orig_w as f64) as u32;
    let mut work = image::imageops::resize(&img, target_w, target_h, image::imageops::FilterType::Lanczos3);
    let (w, h) = (target_w, target_h);

    autocontrast(&mut work, 1.0);
    if profile.sharpen > 0.0 { unsharp_mask(&mut work, profile.sharpen); }
    if profile.use_dithering {
        dither_floyd_steinberg(&mut work, profile.threshold);
    }

    let row_bytes = w.div_ceil(8) as usize;

    let mut sock = bt_connect(&profile.bt_mac, profile.bt_channel)
        .map_err(|e| format!("Bluetooth connect failed: {e}"))?;

    let mut init: Vec<u8> = std::iter::repeat_n(0u8, 64)
        .chain(b"\x1b\x40".iter().copied())
        .collect();
    if profile.heating_time > 0 {
        init.extend_from_slice(&[0x1b, 0x37, profile.heating_dots, profile.heating_time, 0x00]);
    }
    init.extend_from_slice(b"\x1b\x32");
    init.extend_from_slice(b"\x1b\x33\x00");
    sock.write_all(&init).map_err(|e| format!("Init write failed: {e}"))?;
    sock.flush().ok();
    thread::sleep(Duration::from_millis(300));

    let mut y = 0u32;
    while y < h {
        let slice_h = profile.slice_lines.min(h - y);

        let row_bytes_lo = (row_bytes & 0xFF) as u8;
        let row_bytes_hi = (row_bytes >> 8) as u8;
        let slice_h_lo   = (slice_h & 0xFF) as u8;
        let slice_h_hi   = ((slice_h >> 8) & 0xFF) as u8;
        let header = [0x1d, 0x76, 0x30, 0u8, row_bytes_lo, row_bytes_hi, slice_h_lo, slice_h_hi];
        sock.write_all(&header).map_err(|e| format!("Slice header write failed: {e}"))?;

        let mut bmp = Vec::with_capacity(row_bytes * slice_h as usize);
        for row in y..(y + slice_h) {
            for byte_idx in 0..(row_bytes as u32) {
                let mut byte: u8 = 0;
                for bit in 0..8u32 {
                    let x = byte_idx * 8 + bit;
                    if x < w {
                        let Luma([px]) = *work.get_pixel(x, row);
                        if px < profile.threshold {
                            byte |= 0x80 >> bit;
                        }
                    }
                }
                bmp.push(byte);
            }
        }
        sock.write_all(&bmp).map_err(|e| format!("Slice data write failed: {e}"))?;
        sock.flush().ok();

        y += slice_h;

        if y < h {
            thread::sleep(Duration::from_millis(profile.slice_delay_ms));
        }
    }

    sock.write_all(b"\x1b\x64\x04").ok();
    sock.write_all(b"\x1d\x56\x41\x03").ok();
    sock.flush().ok();
    Ok(())
}

pub fn print_pdf_with_retry(
    pdf_path: &Path,
    profile: &PrinterProfile,
    page: u32,
    max_attempts: u32,
    retry_delay_ms: u64,
    mut on_retry: impl FnMut(u32, &str),
) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 0..max_attempts {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(retry_delay_ms));
        }
        match print_pdf_once(pdf_path, profile, page) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if attempt + 1 < max_attempts {
                    on_retry(attempt + 1, &last_err);
                }
            }
        }
    }
    Err(last_err)
}

pub fn dry_run_pdf(pdf_path: &Path, profile: &PrinterProfile, page: u32) -> Result<String, String> {
    let img = pdf_to_gray(pdf_path, profile.dpi * profile.render_scale, page)?;
    let escpos =
        image_to_escpos(&img, profile.max_width_px, profile.threshold, profile.use_dithering, profile.sharpen);
    let (w, h) = img.dimensions();
    let out = std::env::temp_dir().join("ghostlabel-dryrun.bin");
    let _ = std::fs::write(&out, &escpos);
    Ok(format!(
        "DRY RUN - {}×{}px  {} ESC/POS bytes → {}",
        w, h, escpos.len(), out.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GrayImage;

    const HEADER_LEN: usize = 12;
    const FOOTER_LEN: usize = 7;

    #[test]
    fn parse_mac_reverses_bytes_for_bluez() {
        let b = parse_mac("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(b, [0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]);
    }

    #[test]
    fn parse_mac_all_zeros() {
        assert_eq!(parse_mac("00:00:00:00:00:00").unwrap(), [0u8; 6]);
    }

    #[test]
    fn parse_mac_rejects_too_few_octets() {
        assert!(parse_mac("AA:BB:CC:DD:EE").is_err());
    }

    #[test]
    fn parse_mac_rejects_too_many_octets() {
        assert!(parse_mac("AA:BB:CC:DD:EE:FF:00").is_err());
    }

    #[test]
    fn parse_mac_rejects_non_hex_digits() {
        assert!(parse_mac("GG:BB:CC:DD:EE:FF").is_err());
    }

    #[test]
    fn parse_mac_rejects_wrong_separator() {
        assert!(parse_mac("AA-BB-CC-DD-EE-FF").is_err());
    }

    #[test]
    fn escpos_starts_with_esc_at_init() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(&buf[0..2], b"\x1b\x40");
    }

    #[test]
    fn escpos_has_esc2_line_spacing() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(&buf[2..4], b"\x1b\x32");
    }

    #[test]
    fn escpos_has_gs_v0_command() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(&buf[4..7], &[0x1D, 0x76, 0x30]);
        assert_eq!(buf[7], 0, "mode byte m must be 0");
    }

    #[test]
    fn escpos_gs_v0_encodes_dimensions_8x1() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[8], 1,  "xL = 1 for 8px");
        assert_eq!(buf[9], 0,  "xH = 0");
        assert_eq!(buf[10], 1, "yL = 1 for 1 row");
        assert_eq!(buf[11], 0, "yH = 0");
    }

    #[test]
    fn escpos_gs_v0_encodes_wide_image_row_bytes() {
        let img = GrayImage::from_pixel(16, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        let row_bytes = (buf[8] as u32) | ((buf[9] as u32) << 8);
        assert_eq!(row_bytes, 2);
    }

    #[test]
    fn escpos_all_black_image_sets_all_bits() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0xFF);
    }

    #[test]
    fn escpos_all_white_image_clears_all_bits() {
        let img = GrayImage::from_pixel(8, 1, Luma([255u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0x00);
    }

    #[test]
    fn escpos_msb_is_leftmost_pixel() {
        let mut img = GrayImage::from_pixel(8, 1, Luma([255u8]));
        img.put_pixel(0, 0, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0x80);
    }

    #[test]
    fn escpos_lsb_is_rightmost_pixel_in_byte() {
        let mut img = GrayImage::from_pixel(8, 1, Luma([255u8]));
        img.put_pixel(7, 0, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0x01);
    }

    #[test]
    fn escpos_threshold_exclusive_below() {
        let img = GrayImage::from_pixel(8, 1, Luma([139u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0xFF);
    }

    #[test]
    fn escpos_threshold_inclusive_at_value() {
        let img = GrayImage::from_pixel(8, 1, Luma([140u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf[HEADER_LEN], 0x00);
    }

    #[test]
    fn escpos_payload_length_for_8x1() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf.len(), HEADER_LEN + 1 + FOOTER_LEN);
    }

    #[test]
    fn escpos_payload_length_for_16x2() {
        let img = GrayImage::from_pixel(16, 2, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        assert_eq!(buf.len(), HEADER_LEN + (2 * 2) + FOOTER_LEN);
    }

    #[test]
    fn escpos_scales_image_wider_than_max() {
        let img = GrayImage::from_pixel(1024, 4, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        let row_bytes = (buf[8] as u32) | ((buf[9] as u32) << 8);
        assert_eq!(row_bytes, 104);
    }

    #[test]
    fn escpos_does_not_upscale_narrow_image() {
        let img = GrayImage::from_pixel(100, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        let row_bytes = (buf[8] as u32) | ((buf[9] as u32) << 8);
        assert_eq!(row_bytes, 13);
    }

    #[test]
    fn escpos_ends_with_feed_command() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        let n = buf.len();
        assert_eq!(&buf[n - 7..n - 4], b"\x1b\x64\x04");
    }

    #[test]
    fn escpos_ends_with_partial_cut() {
        let img = GrayImage::from_pixel(8, 1, Luma([0u8]));
        let buf = image_to_escpos(&img, 832, 140, false, 0.0);
        let n = buf.len();
        assert_eq!(&buf[n - 4..], &[0x1D, 0x56, 0x41, 0x03]);
    }

    #[test]
    fn dither_binarizes_all_pixels_to_0_or_255() {
        let pixels: Vec<u8> = (0u8..=255).cycle().take(16 * 16).collect();
        let mut img = GrayImage::from_raw(16, 16, pixels).unwrap();
        dither_floyd_steinberg(&mut img, 128);
        assert!(img.pixels().all(|Luma([p])| *p == 0 || *p == 255));
    }

    #[test]
    fn dither_produces_mixed_output_for_midgray() {
        let mut img = GrayImage::from_pixel(16, 16, Luma([128u8]));
        dither_floyd_steinberg(&mut img, 140);
        let blacks = img.pixels().filter(|Luma([p])| *p == 0).count();
        let whites = img.pixels().filter(|Luma([p])| *p == 255).count();
        assert!(blacks > 0 && whites > 0, "FS dither must mix output for mid-gray");
    }

    #[test]
    fn dither_all_black_stays_all_black() {
        let mut img = GrayImage::from_pixel(8, 8, Luma([0u8]));
        dither_floyd_steinberg(&mut img, 140);
        assert!(img.pixels().all(|Luma([p])| *p == 0));
    }

    #[test]
    fn dither_all_white_stays_all_white() {
        let mut img = GrayImage::from_pixel(8, 8, Luma([255u8]));
        dither_floyd_steinberg(&mut img, 140);
        assert!(img.pixels().all(|Luma([p])| *p == 255));
    }

    #[test]
    fn autocontrast_stretches_limited_range_to_full() {
        let pixels: Vec<u8> = (100u8..=150).cycle().take(510).collect();
        let mut img = GrayImage::from_raw(51, 10, pixels).unwrap();
        autocontrast(&mut img, 0.0);
        let min_val = img.pixels().map(|Luma([p])| *p).min().unwrap();
        let max_val = img.pixels().map(|Luma([p])| *p).max().unwrap();
        assert_eq!(min_val, 0);
        assert_eq!(max_val, 255);
    }

    #[test]
    fn autocontrast_noop_on_uniform_image() {
        let mut img = GrayImage::from_pixel(10, 10, Luma([128u8]));
        autocontrast(&mut img, 0.0);
        assert!(img.pixels().all(|Luma([p])| *p == 128));
    }

    #[test]
    fn autocontrast_noop_on_already_full_range() {
        let mut img = GrayImage::from_raw(2, 1, vec![0u8, 255u8]).unwrap();
        autocontrast(&mut img, 0.0);
        assert_eq!(img.get_pixel(0, 0).0[0], 0);
        assert_eq!(img.get_pixel(1, 0).0[0], 255);
    }

    #[test]
    fn pdf_page_count_returns_1_for_missing_file() {
        assert_eq!(pdf_page_count(Path::new("/nonexistent_ghostlabel_test.pdf")), 1);
    }
}
