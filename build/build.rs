use std::{
    env,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

mod attacks;
mod magics;
mod maps;

const BASE_URL: &str = "https://github.com/codedeliveryservice/RecklessNetworks/releases/download/networks";
const NETWORK_NAME: &str = "v60-7f587dfb.nnue";

fn main() {
    let use_pext = use_pext();

    println!("cargo:rustc-check-cfg=cfg(use_pext)");
    if use_pext {
        println!("cargo:rustc-cfg=use_pext");
    }

    generate_model_env();
    generate_attack_maps(use_pext);
    generate_compiler_info();
    generate_engine_version();

    #[cfg(feature = "syzygy")]
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("wasm32") {
        generate_syzygy_binding();
    }

    if !Path::new("networks").join(NETWORK_NAME).exists() && env::var("EVALFILE").is_err() {
        download_network();
    }

    println!("cargo:rerun-if-env-changed=EVALFILE");
    println!("cargo:rerun-if-env-changed=WRECKLESS_PEXT");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    println!("cargo:rerun-if-changed=networks/{NETWORK_NAME}");
}

#[cfg(feature = "syzygy")]
fn generate_syzygy_binding() {
    cc::Build::new()
        .compiler("clang")
        .include("./deps/Fathom")
        .file("./deps/Fathom/tbprobe.c")
        .flag("-Wno-deprecated-declarations")
        .flag("-Wno-sign-compare")
        .flag("-Wno-macro-redefined")
        .flag("-O3")
        .compile("fathom");

    bindgen::Builder::default()
        .header("./deps/Fathom/tbprobe.h")
        .layout_tests(false)
        .generate()
        .expect("Failed to generate Fathom bindings")
        .write_to_file("src/bindings.rs")
        .unwrap();
}

fn generate_model_env() {
    let mut path = env::var("EVALFILE").map(PathBuf::from).unwrap_or_else(|_| Path::new("networks").join(NETWORK_NAME));

    if path.is_relative() {
        path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    }

    println!("cargo:rustc-env=MODEL={}", path.display());
}

/// Decides whether sliding-piece attacks should be indexed with BMI2 `pext`
/// instead of magic multiplication. Requires BMI2 on the target, and avoids
/// AMD Zen 1/2 (family 0x17), where `pext` is microcoded and far slower than
/// a magic multiply. The check runs on the build host, which matches the
/// target machine for the default `-C target-cpu=native` builds; use
/// `WRECKLESS_PEXT=0|1` to override when cross-compiling.
fn use_pext() -> bool {
    match env::var("WRECKLESS_PEXT").as_deref() {
        Ok("0") => return false,
        Ok("1") => return true,
        _ => {}
    }

    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if !features.split(',').any(|feature| feature == "bmi2") {
        return false;
    }

    !is_amd_zen1_or_zen2()
}

#[cfg(target_arch = "x86_64")]
fn is_amd_zen1_or_zen2() -> bool {
    use std::arch::x86_64::__cpuid;

    let id0 = __cpuid(0);
    let vendor = [id0.ebx.to_le_bytes(), id0.edx.to_le_bytes(), id0.ecx.to_le_bytes()].concat();
    if vendor != b"AuthenticAMD" {
        return false;
    }

    let id1 = __cpuid(1);
    let base_family = (id1.eax >> 8) & 0xF;
    let ext_family = (id1.eax >> 20) & 0xFF;
    let family = if base_family == 0xF { base_family + ext_family } else { base_family };

    family == 0x17
}

#[cfg(not(target_arch = "x86_64"))]
fn is_amd_zen1_or_zen2() -> bool {
    false
}

fn generate_attack_maps(use_pext: bool) {
    let dir = env::var("OUT_DIR").unwrap();
    let path = Path::new(&dir).join("lookup.rs");
    let out = File::create(path).unwrap();
    write(BufWriter::new(out), use_pext).unwrap();
}

fn write(mut buf: BufWriter<File>, use_pext: bool) -> Result<(), std::io::Error> {
    macro_rules! write_map {
        ($name:tt, $type:tt, $items:expr) => {
            writeln!(buf, "static {}: [{}; {}] = {:?};", $name, $type, $items.len(), $items)?;
        };
    }

    write_map!("DIAGONALS", "[u64; 64]", maps::generate_diagonal_tables());

    write_map!("KING_MAP", "u64", maps::generate_king_map());
    write_map!("KNIGHT_MAP", "u64", maps::generate_knight_map());

    write_map!("PAWN_MAP", "[u64; 64]", maps::generate_pawn_map());

    write_map!("RAYPASS", "[u64; 64]", maps::generate_rays_map());
    write_map!("BETWEEN", "[u64; 64]", maps::generate_between_map());

    write_map!("ROOK_MAP", "u64", maps::generate_rook_map(use_pext));
    write_map!("BISHOP_MAP", "u64", maps::generate_bishop_map(use_pext));

    write_map!("ROOK_MAGICS", "MagicEntry", magics::ROOK_MAGICS);
    write_map!("BISHOP_MAGICS", "MagicEntry", magics::BISHOP_MAGICS);

    writeln!(
        buf,
        "#[allow(dead_code)] struct MagicEntry {{ pub mask: u64, pub magic: u64, pub shift: u32, pub offset: u32 }}"
    )
}

fn download_network() {
    let response = Command::new("curl")
        .arg("-sfL")
        .arg(format!("{BASE_URL}/{NETWORK_NAME}"))
        .output()
        .expect("Failed to execute `curl` to download network");

    if response.status.success() {
        std::fs::create_dir_all("networks").unwrap();
        std::fs::write(format!("networks/{NETWORK_NAME}"), response.stdout).unwrap();
    } else {
        panic!("Failed to download the network");
    }
}

fn generate_compiler_info() {
    fn get_env(key: &str) -> String {
        env::var(key).unwrap_or("unknown".to_owned())
    }

    let version = Command::new("rustc")
        .arg("--version")
        .output()
        .map(|v| String::from_utf8_lossy(&v.stdout).to_string())
        .unwrap_or("unknown".to_owned());

    println!("cargo:rustc-env=COMPILER_VERSION={version}");
    println!("cargo:rustc-env=COMPILER_TARGET={}", get_env("TARGET"));
    println!("cargo:rustc-env=COMPILER_FEATURES={}", get_env("CARGO_CFG_TARGET_FEATURE"));
}

fn generate_engine_version() {
    let version = env!("CARGO_PKG_VERSION");

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|v| v.status.success())
        .and_then(|v| String::from_utf8(v.stdout).ok())
        .map(|v| v.trim().to_string());

    if let Some(sha) = git_sha {
        println!("cargo:rustc-env=ENGINE_VERSION={version}-{sha}")
    } else {
        println!("cargo:rustc-env=ENGINE_VERSION={version}")
    }
}
