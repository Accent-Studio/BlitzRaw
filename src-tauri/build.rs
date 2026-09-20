use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

fn verify_sha256(path: &Path, expected_hash: &str) -> Result<bool, io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash_bytes = hasher.finalize();
    let calculated_hash = hex::encode(hash_bytes);
    Ok(calculated_hash == expected_hash)
}

fn download_and_verify(
    url: &str,
    dest_path: &Path,
    expected_hash: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let temp_filename = dest_path.file_name().unwrap();
    let temp_path = out_dir.join(temp_filename);

    println!(
        "cargo:warning=Downloading to temporary path: {:?}",
        temp_path
    );
    let mut response = reqwest::blocking::get(url)?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response
            .text()
            .unwrap_or_else(|_| "Could not read error body".to_string());
        return Err(format!("Download failed with status {}: {}", status, error_body).into());
    }

    let mut temp_file = fs::File::create(&temp_path)?;
    response.copy_to(&mut temp_file)?;
    println!("cargo:warning=Download complete. Verifying file integrity...");

    match verify_sha256(&temp_path, expected_hash) {
        Ok(true) => {
            fs::copy(&temp_path, dest_path)?;
            fs::remove_file(&temp_path)?;
            println!(
                "cargo:warning=Successfully downloaded and verified {:?}.",
                dest_path
            );
            Ok(())
        }
        Ok(false) => {
            fs::remove_file(&temp_path)?;
            Err("Verification failed! The downloaded file is corrupt.".into())
        }
        Err(e) => {
            fs::remove_file(&temp_path).ok();
            Err(format!("Could not verify file after download: {}", e).into())
        }
    }
}

// ============ BLITZRAW: the DirectML build of ONNX Runtime ============
// The runtime upstream downloads has no GPU support at all, so every AI model
// in the app runs on the processor. That is a graphics card sitting idle while
// the CPU takes minutes over a denoise.
//
// DirectML rather than CUDA. DirectML needs two DLLs and any DirectX 12 card;
// CUDA would be faster on an NVIDIA card but wants about 2.6 GB of NVIDIA
// libraries shipped alongside, which is not a trade worth making here.
//
// Both files come from Microsoft's own NuGet feed. The two versions belong
// together and must not be bumped separately: 1.22.0 is the runtime that
// `ort 2.0.0-rc.10` expects, and that package's own manifest names 1.15.4 as
// the DirectML it was built against.
//
// The DirectML build also contains the processor path, so this is a superset
// of what it replaces. A machine with no suitable card still works; the
// registration in `ai_processing.rs` falls back and says so in the log.
#[cfg(windows)]
const DIRECTML_PARTS: [(&str, &str, &str, &str); 2] = [
    (
        "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.22.0/microsoft.ml.onnxruntime.directml.1.22.0.nupkg",
        "runtimes/win-x64/native/onnxruntime.dll",
        "onnxruntime.dll",
        "95366724919f4e95ecc60010912ed538ad9804b6683fbd0aad389749102834b9",
    ),
    (
        "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
        "bin/x64-win/DirectML.dll",
        "DirectML.dll",
        "9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1",
    ),
];

#[cfg(windows)]
fn ensure_directml(dest_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Cursor;

    for (url, entry_name, out_name, expected_hash) in DIRECTML_PARTS {
        let dest_path = dest_dir.join(out_name);
        if dest_path.exists() && verify_sha256(&dest_path, expected_hash).unwrap_or(false) {
            println!("cargo:warning={out_name} is already the DirectML build. Skipping.");
            continue;
        }

        println!("cargo:warning=Downloading {out_name} from Microsoft's NuGet feed...");
        let response = reqwest::blocking::get(url)?;
        if !response.status().is_success() {
            return Err(format!("{url} returned {}", response.status()).into());
        }
        let package = response.bytes()?;

        let mut archive = zip::ZipArchive::new(Cursor::new(package))?;
        let mut entry = archive.by_name(entry_name)?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut bytes)?;

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let got = hex::encode(hasher.finalize());
        if got != expected_hash {
            return Err(format!("{out_name} hashed {got}, expected {expected_hash}").into());
        }

        // Written beside and renamed, so a build stopped part way through never
        // leaves a half-written DLL that the next one would trust.
        let temp = dest_path.with_extension("dll.part");
        fs::write(&temp, &bytes)?;
        fs::rename(&temp, &dest_path)?;
        println!("cargo:warning=Wrote {}", dest_path.display());
    }

    Ok(())
}
// ========== BLITZRAW END: the DirectML build of ONNX Runtime ==========

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // BLITZRAW: on Windows the DirectML pair replaces the processor-only
    // runtime that the download below would fetch, so that path is skipped.
    #[cfg(windows)]
    if target_os == "windows" && target_arch == "x86_64" {
        let dest_dir = manifest_dir.join("resources");
        fs::create_dir_all(&dest_dir).unwrap();
        if let Err(e) = ensure_directml(&dest_dir) {
            panic!("Failed to fetch the DirectML runtime: {e}");
        }
        println!("cargo:rerun-if-changed=build.rs");
        return tauri_build::build();
    }

    let (download_filename, lib_name, expected_hash) =
        match (target_os.as_str(), target_arch.as_str()) {
            ("windows", "x86_64") => (
                "onnxruntime-windows-x86_64.dll",
                "onnxruntime.dll",
                "579b636403983254346a5c1d80bd28f1519cd1e284cd204f8d4ff41f8d711559",
            ),
            ("windows", "aarch64") => (
                "onnxruntime-windows-aarch64.dll",
                "onnxruntime.dll",
                "79281671a386ed1baab9dbdbb09fe55f99577011472e9526cf9d0b468bb6bcc7",
            ),
            ("linux", "x86_64") => (
                "libonnxruntime-linux-x86_64.so",
                "libonnxruntime.so",
                "3da6146e14e7b8aaec625dde11d6114c7457c87a5f93d744897da8781e35c673",
            ),
            ("linux", "aarch64") => (
                "libonnxruntime-linux-aarch64.so",
                "libonnxruntime.so",
                "0afd69a0ae38c5099fd0e8604dda398ac43dee67cd9c6394b5142b19e82528de",
            ),
            ("macos", "x86_64") => (
                "libonnxruntime-macos-x86_64.dylib",
                "libonnxruntime.dylib",
                "283e595e61cf65df7a6b1d59a1616cbd35c8b6399dd90d799d99b71a3ff83160",
            ),
            ("macos", "aarch64") => (
                "libonnxruntime-macos-aarch64.dylib",
                "libonnxruntime.dylib",
                "2b885992d3d6fa4130d39ec84a80d7504ff52750027c547bb22c86165f19406a",
            ),
            ("android", "aarch64") => (
                "libonnxruntime-android-arm64-v8a.so",
                "libonnxruntime.so",
                "999ecfdb5b5a13e4097487773b6d71ce8a075408a237daab072e8f5e817bd78e",
            ),
            _ => panic!("Unsupported target: {}-{}", target_os, target_arch),
        };

    let dest_dir = if target_os == "android" {
        manifest_dir.join("libs").join("arm64-v8a")
    } else {
        manifest_dir.join("resources")
    };

    fs::create_dir_all(&dest_dir).unwrap();
    let dest_path = dest_dir.join(lib_name);

    let mut is_valid = false;
    if dest_path.exists() {
        match verify_sha256(&dest_path, expected_hash) {
            Ok(true) => {
                println!(
                    "cargo:warning=ONNX Runtime library already exists and is valid. Skipping download."
                );
                is_valid = true;
            }
            Ok(false) => {
                println!(
                    "cargo:warning=File {:?} exists but has incorrect hash. Deleting and re-downloading.",
                    dest_path
                );
                fs::remove_file(&dest_path).unwrap();
            }
            Err(e) => {
                println!(
                    "cargo:warning=Could not verify file {:?}: {}. Re-downloading.",
                    dest_path, e
                );
            }
        }
    }

    if !is_valid {
        println!(
            "cargo:warning=Downloading ONNX Runtime library for {}-{}...",
            target_os, target_arch
        );
        let base_url =
            "https://huggingface.co/CyberTimon/RapidRAW-Models/resolve/main/onnxruntimes-v1.22.0/";
        let download_url = format!("{}{}?download=true", base_url, download_filename);
        println!("cargo:warning=URL: {}", download_url);

        if let Err(e) = download_and_verify(&download_url, &dest_path, expected_hash) {
            panic!("Failed to download and verify ONNX Runtime library: {}", e);
        }
    }

    if target_os == "android" {
        let jni_libs_dir = manifest_dir.join("gen/android/app/src/main/jniLibs/arm64-v8a");
        fs::create_dir_all(&jni_libs_dir).unwrap();
        fs::copy(&dest_path, jni_libs_dir.join(lib_name)).unwrap();

        println!("cargo:rustc-env=ORT_LIB_LOCATION={}", dest_dir.display());
        println!("cargo:rustc-env=ORT_STRATEGY=manual");
        println!("cargo:rustc-link-search=native={}", dest_dir.display());
    }

    println!("cargo:rerun-if-changed=build.rs");

    tauri_build::build()
}
