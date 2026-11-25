// Pure-Rust deterministic RGBA pixel generator and multi-resolution `.ico` encoder
// for the `pipe_wrench` application icon: HL7 pipe (`|`) + RIDGID-style plumbing pipe wrench.
//
// Shared between runtime (`src/icon.rs`) and Windows resource embedding (`build.rs`).

/// Render the `pipe_wrench` (`|` + plumbing pipe wrench) icon into a `size * size * 4` RGBA buffer
/// using 4x4 supersampled SDF anti-aliasing.
pub fn render_icon_rgba(size: u32) -> Vec<u8> {
    let s = size.max(1) as usize;
    let mut out = vec![0u8; s * s * 4];
    let inv = 1.0_f32 / (s as f32);

    for py in 0..s {
        for px in 0..s {
            let mut r_acc = 0.0_f32;
            let mut g_acc = 0.0_f32;
            let mut b_acc = 0.0_f32;
            let mut a_acc = 0.0_f32;

            // 4x4 stratified supersampling for crisp anti-aliased edges at 32x32, 64x64, 256x256
            for sy in 0..4 {
                let v = (py as f32 + (sy as f32 + 0.5) * 0.25) * inv;
                for sx in 0..4 {
                    let u = (px as f32 + (sx as f32 + 0.5) * 0.25) * inv;
                    let [r, g, b, a] = sample_icon(u, v);
                    r_acc += r * a;
                    g_acc += g * a;
                    b_acc += b * a;
                    a_acc += a;
                }
            }

            let a = a_acc / 16.0;
            let idx = (py * s + px) * 4;
            if a > 1e-4 {
                out[idx] = ((r_acc / a_acc).clamp(0.0, 1.0) * 255.0).round() as u8;
                out[idx + 1] = ((g_acc / a_acc).clamp(0.0, 1.0) * 255.0).round() as u8;
                out[idx + 2] = ((b_acc / a_acc).clamp(0.0, 1.0) * 255.0).round() as u8;
                out[idx + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }

    out
}

/// Encode a multi-resolution Windows `.ico` file containing 32-bit BGRA DIB entries for each size in `sizes`.
#[allow(dead_code)]
pub fn encode_ico(sizes: &[u32]) -> Vec<u8> {
    let count = sizes.len() as u16;
    let mut images: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());

    for &sz in sizes {
        let rgba = render_icon_rgba(sz);
        let dib = encode_ico_dib_entry(sz, &rgba);
        images.push((sz, dib));
    }

    let header_size = 6usize + (sizes.len() * 16);
    let total_bytes: usize = header_size + images.iter().map(|(_, d)| d.len()).sum::<usize>();
    let mut ico = Vec::with_capacity(total_bytes);

    // ICONDIR header
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type = 1 (ICO)
    ico.extend_from_slice(&count.to_le_bytes());

    // ICONDIRENTRY table
    let mut offset = header_size as u32;
    for (sz, dib) in &images {
        let dim = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(dim); // width (0 = 256)
        ico.push(dim); // height (0 = 256)
        ico.push(0); // color count
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bit count
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset = offset.saturating_add(dib.len() as u32);
    }

    // Image payloads
    for (_, dib) in images {
        ico.extend_from_slice(&dib);
    }

    ico
}

#[allow(dead_code)]
fn encode_ico_dib_entry(size: u32, rgba: &[u8]) -> Vec<u8> {
    let s = size as usize;
    let xor_len = s * s * 4;
    let and_row_bytes = s.div_ceil(32) * 4;
    let and_len = and_row_bytes * s;
    let mut dib = Vec::with_capacity(40 + xor_len + and_len);

    // BITMAPINFOHEADER (40 bytes)
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight (XOR + AND)
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&(xor_len as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // XOR mask: bottom-up BGRA
    for y in (0..s).rev() {
        for x in 0..s {
            let idx = (y * s + x) * 4;
            dib.push(rgba[idx + 2]); // B
            dib.push(rgba[idx + 1]); // G
            dib.push(rgba[idx]); // R
            dib.push(rgba[idx + 3]); // A
        }
    }

    // AND mask: bottom-up 1-bit transparency mask
    for y in (0..s).rev() {
        let mut row = vec![0u8; and_row_bytes];
        for x in 0..s {
            let alpha = rgba[(y * s + x) * 4 + 3];
            if alpha == 0 {
                row[x / 8] |= 1u8 << (7 - (x % 8));
            }
        }
        dib.extend_from_slice(&row);
    }

    dib
}

fn hex(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

fn lerp_color(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let k = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
        a[3] + (b[3] - a[3]) * k,
    ]
}

fn sd_rounded_box(u: f32, v: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (u - cx).abs() - hx + r;
    let qy = (v - cy).abs() - hy + r;
    let out_dist = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let in_dist = qx.max(qy).min(0.0);
    out_dist + in_dist - r
}

fn sample_icon(u: f32, v: f32) -> [f32; 4] {
    // 1. Rounded dark-slate background badge (#1E293B) with subtle crisp border (#334155)
    let badge_sd = sd_rounded_box(u, v, 0.5, 0.5, 0.46, 0.46, 0.18);
    if badge_sd > 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }

    let bg_top = hex(0x24, 0x32, 0x47);
    let bg_bot = hex(0x19, 0x22, 0x31);
    let border_col = hex(0x33, 0x41, 0x55);
    let mut color = if badge_sd > -0.026 {
        border_col
    } else {
        lerp_color(bg_top, bg_bot, v)
    };

    // 2. Left Element: Bold HL7 Pipe Delimiter (`|`) in vibrant accent blue (#38BDF8)
    let pipe_sd = sd_rounded_box(u, v, 0.245, 0.50, 0.048, 0.34, 0.026);
    if pipe_sd <= 0.0 {
        let pipe_top = hex(0x7D, 0xD3, 0xFC);
        let pipe_mid = hex(0x38, 0xBD, 0xF8);
        let pipe_bot = hex(0x02, 0x84, 0xC7);
        color = if u < 0.225 {
            lerp_color(pipe_top, pipe_mid, v)
        } else {
            lerp_color(pipe_mid, pipe_bot, v)
        };
        return color;
    }

    // 3. Right Element: Classic RIDGID-Style Plumbing Pipe Wrench (facing left toward `|`)
    let steel_bright = hex(0xF1, 0xF5, 0xF9);
    let steel_mid = hex(0xCB, 0xD5, 0xE1);
    let steel_dark = hex(0x94, 0xA3, 0xB8);
    let groove_dark = hex(0x47, 0x55, 0x69);
    let red_bright = hex(0xEF, 0x44, 0x44);
    let red_mid = hex(0xDC, 0x26, 0x26);
    let red_web = hex(0x99, 0x1B, 0x1B);

    // 3a. Machined Steel Upper Hooked Jaw + Threaded Shank (behind housing)
    let in_shank = sd_rounded_box(u, v, 0.68, 0.31, 0.046, 0.155, 0.014) <= 0.0;
    let in_upper_arm = sd_rounded_box(u, v, 0.565, 0.195, 0.16, 0.046, 0.022) <= 0.0;
    let in_upper_hook = sd_rounded_box(u, v, 0.445, 0.225, 0.045, 0.052, 0.018) <= 0.0;

    // Serrated teeth notches facing downward along the underside of the upper jaw
    let in_upper_teeth = (0.455..=0.615).contains(&u) && (0.238..=0.264).contains(&v) && {
        let phase = ((u - 0.455) / 0.036).fract();
        let tooth_depth = (1.0 - (phase * 2.0 - 1.0).abs()) * 0.024;
        v <= 0.240 + tooth_depth
    };

    if in_shank || in_upper_arm || in_upper_hook || in_upper_teeth {
        let shade = ((v - 0.15) / 0.30).clamp(0.0, 1.0);
        color = lerp_color(steel_bright, steel_mid, shade);
        // Subtle thread lines on the vertical shank below the upper jaw
        if in_shank && !in_upper_arm && v > 0.25 && v < 0.36 {
            let thread_phase = ((v - 0.25) / 0.024).fract();
            if thread_phase < 0.35 {
                color = steel_dark;
            }
        }
    }

    // 3b. Machined Steel Lower Heel Jaw (facing upward toward upper jaw)
    let in_lower_jaw = sd_rounded_box(u, v, 0.54, 0.345, 0.105, 0.032, 0.014) <= 0.0;
    let in_lower_teeth = (0.455..=0.605).contains(&u) && (0.300..=0.316).contains(&v) && {
        let phase = ((u - 0.455) / 0.036).fract();
        let tooth_height = (1.0 - (phase * 2.0 - 1.0).abs()) * 0.018;
        v >= 0.316 - tooth_height
    };
    if in_lower_jaw || in_lower_teeth {
        color = lerp_color(steel_bright, steel_mid, ((v - 0.30) / 0.08).clamp(0.0, 1.0));
    }

    // 3c. Heavy Tapered Red Handle + Jaw Housing Collar
    // Tapered handle: v in [0.44, 0.85], centered at u = 0.68
    let handle_t = ((v - 0.44) / 0.41).clamp(0.0, 1.0);
    let handle_half_w = 0.072 + 0.018 * handle_t;
    let in_handle = sd_rounded_box(u, v, 0.68, 0.645, handle_half_w, 0.205, 0.045) <= 0.0;
    let in_housing = sd_rounded_box(u, v, 0.675, 0.415, 0.105, 0.072, 0.024) <= 0.0;

    // Hang-hole at bottom of handle
    let hole_dist = ((u - 0.68).powi(2) + (v - 0.785).powi(2)).sqrt();
    let in_hang_hole = hole_dist < 0.030;

    // Housing window where the knurled adjustment nut sits
    let in_nut_window = sd_rounded_box(u, v, 0.68, 0.415, 0.078, 0.038, 0.010) <= 0.0;

    if (in_handle || in_housing) && !in_hang_hole && !in_nut_window {
        let grad = ((u - 0.58) / 0.20).clamp(0.0, 1.0);
        color = lerp_color(red_bright, red_mid, grad);

        // Recessed darker I-beam web down the middle of the red handle
        if sd_rounded_box(u, v, 0.68, 0.625, 0.034, 0.105, 0.018) <= 0.0 {
            color = red_web;
        }
    }

    // 3d. Knurled Cylindrical Adjustment Nut seated inside the housing collar window
    let in_nut = sd_rounded_box(u, v, 0.68, 0.415, 0.090, 0.033, 0.010) <= 0.0;
    if in_nut {
        let knurl = ((u - 0.59) / 0.024).fract();
        color = if knurl < 0.42 {
            groove_dark
        } else if knurl < 0.72 {
            steel_mid
        } else {
            steel_bright
        };
    }

    color
}
