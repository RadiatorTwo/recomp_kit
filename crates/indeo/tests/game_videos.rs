// Indeo Video 5 decoder: test with the game's own videos.
//
// Part of the indeo crate, a port of FFmpeg's indeo5.c / ivi.c / ivi_dsp.c; licensed under the
// GNU Lesser General Public License, version 2.1 or later (see COPYING).

//! Decodes every frame of the Recoil videos (the player's own game files, not in the
//! repository) and writes a few frames as PNG files to the temp folder for looking at.
//!
//! Run: `cargo test --release -p indeo -- --include-ignored`

use std::path::{Path, PathBuf};
use std::time::Instant;

use indeo::Decoder;

/// The video chunks of an AVI file, in order (a minimal reader for this test only), and the
/// picture size and codec of the video stream.
struct Avi {
    width: u32,
    height: u32,
    codec: [u8; 4],
    frames: Vec<Vec<u8>>,
}

fn u32_at(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[i..i + 4].try_into().unwrap())
}

fn read_avi(path: &Path) -> Avi {
    let data = std::fs::read(path).unwrap();
    let mut avi = Avi {
        width: 0,
        height: 0,
        codec: [0; 4],
        frames: Vec::new(),
    };
    walk(&data, &mut avi, false);
    avi
}

/// Walk a list of RIFF chunks; `in_movi`: we are inside the `movi` list.
fn walk(d: &[u8], avi: &mut Avi, in_movi: bool) {
    let mut i = 0;
    while i + 8 <= d.len() {
        let id = &d[i..i + 4];
        let size = u32_at(d, i + 4) as usize;
        let start = i + 8;
        let end = (start + size).min(d.len());
        let body = &d[start..end];
        match id {
            b"RIFF" | b"LIST" if body.len() >= 4 => {
                let movi = in_movi || &body[..4] == b"movi";
                walk(&body[4..], avi, movi);
            }
            // the first video stream format: BITMAPINFOHEADER
            b"strf" if avi.width == 0 && body.len() >= 20 && u32_at(body, 0) >= 40 => {
                avi.width = u32_at(body, 4);
                avi.height = u32_at(body, 8);
                avi.codec.copy_from_slice(&body[16..20]);
            }
            b"00dc" | b"00db" if in_movi => avi.frames.push(body.to_vec()),
            _ => {}
        }
        i = start + size + (size & 1);
    }
}

fn write_png(path: &Path, dec: &Decoder) {
    let (w, h) = (dec.width(), dec.height());
    let mut rgb = vec![0u32; (w * h) as usize];
    dec.to_rgb(&mut rgb);
    let bytes: Vec<u8> = rgb
        .iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
        .collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&bytes)
        .unwrap();
}

/// FFmpeg's pictures of a video as raw YUV 4:1:0 (the codec's own format, no conversion),
/// one picture after the other; `None` if there is no `ffmpeg` on the PATH.
fn ffmpeg_frames(path: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv410p",
            "-",
        ])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// `game_files/Video` in the repository, or in a folder above it (for git worktrees).
fn video_dir() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.ancestors()
        .map(|a| a.join("game_files/Video"))
        .find(|p| p.is_dir())
        .unwrap_or_else(|| here.join("../../game_files/Video"))
}

#[test]
#[ignore = "needs the game files in game_files/Video"]
fn decode_game_videos() {
    let dir = video_dir();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("avi")))
        .collect();
    names.sort();
    let out_dir = std::env::temp_dir().join("indeo_test");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut decoded_files = 0;
    for path in &names {
        let avi = read_avi(path);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if !avi.codec.eq_ignore_ascii_case(b"IV50") {
            println!(
                "{name}: codec {:?}, skipped",
                String::from_utf8_lossy(&avi.codec)
            );
            continue;
        }
        let mut dec = Decoder::new(avi.width, avi.height);
        let (mut pictures, mut none) = (0, 0);
        let mut errors = Vec::new();

        // FFmpeg's pictures to compare with, when there is an ffmpeg
        let reference = ffmpeg_frames(path);
        let (w, h) = (avi.width as usize, avi.height as usize);
        let picture_size = w * h + 2 * w.div_ceil(4) * h.div_ceil(4);
        let mut mine = Vec::new();

        let t = Instant::now();
        let last = avi.frames.len().saturating_sub(1);
        for (n, frame) in avi.frames.iter().enumerate() {
            match dec.decode(frame) {
                Ok(true) => {
                    pictures += 1;
                    if reference.is_some() {
                        let (y, _, _) = dec.planes();
                        let (u, v) = dec.chroma_quarter();
                        mine.extend_from_slice(y);
                        mine.extend_from_slice(u);
                        mine.extend_from_slice(v);
                    }
                }
                Ok(false) => none += 1,
                Err(e) => errors.push(format!("frame {n}: {e}")),
            }
            if [0, 100, 500].contains(&n) || n == last {
                write_png(&out_dir.join(format!("{name}_{n:04}.png")), &dec);
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{name}: {}x{} {} frames, {pictures} pictures, {none} without picture, {} errors, {:.2} ms/frame",
            dec.width(),
            dec.height(),
            avi.frames.len(),
            errors.len(),
            ms / avi.frames.len().max(1) as f64
        );
        assert!(errors.is_empty(), "{name}: {errors:#?}");
        assert!(pictures > 0, "{name}: no pictures");

        match &reference {
            None => println!("{name}: no ffmpeg, not compared"),
            Some(r) => {
                let theirs: Vec<&[u8]> = r.chunks(picture_size).collect();
                let ours: Vec<&[u8]> = mine.chunks(picture_size).collect();
                let differ: Vec<usize> = (0..ours.len().min(theirs.len()))
                    .filter(|&i| ours[i] != theirs[i])
                    .collect();
                println!(
                    "{name}: compared with ffmpeg: {} / {} pictures, {} differ{}",
                    ours.len(),
                    theirs.len(),
                    differ.len(),
                    differ
                        .first()
                        .map(|i| format!(" (first: picture {i})"))
                        .unwrap_or_default()
                );
                assert_eq!(
                    ours.len(),
                    theirs.len(),
                    "{name}: picture count differs from ffmpeg"
                );
                assert!(differ.is_empty(), "{name}: pictures differ from ffmpeg");
            }
        }

        // damaged frames must give errors or pictures, never a panic
        let mut seed = 0x2545_f491u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for frame in avi.frames.iter().take(30).filter(|f| !f.is_empty()) {
            for _ in 0..20 {
                let mut bad = frame.clone();
                for _ in 0..1 + rand() % 8 {
                    let i = rand() % bad.len();
                    bad[i] ^= 1 << (rand() % 8);
                }
                if rand() % 4 == 0 {
                    bad.truncate(rand() % bad.len());
                }
                let _ = dec.decode(&bad);
            }
        }

        decoded_files += 1;
    }
    println!("PNGs in {}", out_dir.display());
    assert!(
        decoded_files > 0,
        "no IV50 videos found in {}",
        dir.display()
    );
}
