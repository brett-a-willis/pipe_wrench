//! Build script embedding the procedural multi-resolution (`32x32`, `64x64`, `256x256`)
//! `|` + plumbing pipe wrench `.ico` into the Windows `pipe_wrench.exe` binary (`Phase 18`).

#[cfg(windows)]
mod icon_render {
    include!("src/icon_render.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon_render.rs");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let out_dir = match std::env::var("OUT_DIR") {
            Ok(v) => std::path::PathBuf::from(v),
            Err(_) => return,
        };
        let ico_path = out_dir.join("pipe_wrench.ico");
        let ico_bytes = icon_render::encode_ico(&[32, 64, 256]);
        if let Err(err) = std::fs::write(&ico_path, ico_bytes) {
            println!("cargo:warning=Failed to write pipe_wrench.ico: {err}");
            return;
        }
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&ico_path.to_string_lossy());
        if let Err(err) = res.compile() {
            println!("cargo:warning=Failed to embed Windows icon resource: {err}");
        }
    }
}
