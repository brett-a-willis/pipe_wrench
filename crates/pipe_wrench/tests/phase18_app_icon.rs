//! Phase 18 tests verifying the procedural HL7 pipe (`|`) + plumbing pipe wrench application icon
//! (`generate_app_icon`, `render_icon_rgba`, and multi-resolution `.ico` encoding).

use pipe_wrench::{encode_ico, generate_app_icon};

#[test]
fn procedural_app_icon_has_valid_rgba_pipe_steel_jaws_and_red_handle() {
    let icon = generate_app_icon(64);
    assert_eq!(icon.width, 64);
    assert_eq!(icon.height, 64);
    assert_eq!(icon.rgba.len(), 64 * 64 * 4);

    let mut blue_pipe_pixels = 0usize;
    let mut steel_jaw_pixels = 0usize;
    let mut red_handle_pixels = 0usize;
    let mut transparent_corner_pixels = 0usize;

    for y in 0..64usize {
        for x in 0..64usize {
            let idx = (y * 64 + x) * 4;
            let r = icon.rgba[idx];
            let g = icon.rgba[idx + 1];
            let b = icon.rgba[idx + 2];
            let a = icon.rgba[idx + 3];

            if (x < 3 && y < 3) && a == 0 {
                transparent_corner_pixels += 1;
            }
            if a < 200 {
                continue;
            }

            // Left region: vibrant HL7 blue pipe (`|`)
            if x < 24 && b > 190 && g > 140 && r < 150 {
                blue_pipe_pixels += 1;
            }
            // Right upper region: bright machined steel jaws
            if x >= 24 && y < 28 && r > 170 && g > 180 && b > 190 {
                steel_jaw_pixels += 1;
            }
            // Right lower region: classic pipe-wrench red handle
            if x >= 32 && y >= 28 && r > 180 && g < 100 && b < 100 {
                red_handle_pixels += 1;
            }
        }
    }

    assert!(
        transparent_corner_pixels > 0,
        "rounded badge should have transparent corners"
    );
    assert!(
        blue_pipe_pixels >= 80,
        "expected crisp blue `|` pipe pixels on left, found {blue_pipe_pixels}"
    );
    assert!(
        steel_jaw_pixels >= 40,
        "expected bright steel jaw pixels on upper right, found {steel_jaw_pixels}"
    );
    assert!(
        red_handle_pixels >= 80,
        "expected red pipe-wrench handle pixels on lower right, found {red_handle_pixels}"
    );

    // Multi-resolution .ico header validation (32x32, 64x64, 256x256)
    let ico = encode_ico(&[32, 64, 256]);
    assert!(ico.len() > 6 + 3 * 16);
    assert_eq!(&ico[0..6], &[0, 0, 1, 0, 3, 0]);
    assert_eq!(ico[6], 32); // 32x32 entry width
    assert_eq!(ico[22], 64); // 64x64 entry width
    assert_eq!(ico[38], 0); // 256x256 entry width encoded as 0 per ICO spec
}
