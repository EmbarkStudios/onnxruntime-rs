#![allow(dead_code)]

use io::Write;
use std::{
    env, fs,
    io::{self, BufWriter, Read},
    path::{Path, PathBuf},
};

/// ONNX Runtime version
///
/// WARNING: If version is changed, bindings for all platforms will have to be re-generated.
///          To do so, run this:
///              cargo build --package onnxruntime-sys --features generate-bindings
const ORT_VERSION: &str = "1.5.2";

/// Base Url from which to download pre-built releases/
const ORT_RELEASE_BASE_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download";

/// Environment variable selecting which strategy to use for finding the library
/// Possibilities:
/// * "download": Download a pre-built library from upstream. This is the default if `ORT_STRATEGY` is not set.
/// * "system": Use installed library. Use `ORT_LIB_LOCATION` to point to proper location.
/// * "compile": Download source and compile (TODO).
const ORT_ENV_STRATEGY: &str = "ORT_STRATEGY";

/// Name of environment variable that, if present, contains the location of a pre-built library.
/// Only used if `ORT_STRATEGY=system`.
const ORT_ENV_SYSTEM_LIB_LOCATION: &str = "ORT_LIB_LOCATION";
/// Name of environment variable that, if present, controls wether to use CUDA or not.
const ORT_ENV_GPU: &str = "ORT_USE_CUDA";

/// Subdirectory (of the 'target' directory) into which to extract the prebuilt library.
const ORT_PREBUILT_EXTRACT_DIR: &str = "onnxruntime";

#[cfg(feature = "disable-sys-build-script")]
fn main() {
    println!("Build script disabled!");

    generate_file_including_platform_bindings().unwrap();
}

#[cfg(not(feature = "disable-sys-build-script"))]
fn main() {
    let libort_install_dir = prepare_libort_dir();

    let include_dir = libort_install_dir.join("include");
    let lib_dir = libort_install_dir.join("lib");

    println!("Include directory: {:?}", include_dir);
    println!("Lib directory: {:?}", lib_dir);

    // Tell cargo to tell rustc to link onnxruntime shared library.
    println!("cargo:rustc-link-lib=onnxruntime");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    println!("cargo:rerun-if-env-changed={}", ORT_ENV_STRATEGY);
    println!("cargo:rerun-if-env-changed={}", ORT_ENV_GPU);
    println!("cargo:rerun-if-env-changed={}", ORT_ENV_SYSTEM_LIB_LOCATION);

    generate_bindings(&include_dir);

    generate_file_including_platform_bindings().unwrap();
}

#[cfg(not(feature = "generate-bindings"))]
fn generate_bindings(_include_dir: &Path) {
    println!("Bindings not generated automatically, using committed files instead.");
    println!("Enable with the 'bindgen' cargo feature.");
}

#[cfg(feature = "generate-bindings")]
fn generate_bindings(include_dir: &Path) {
    let clang_arg = format!("-I{}", include_dir.display());

    // Tell cargo to invalidate the built crate whenever the wrapper changes
    println!("cargo:rerun-if-changed=wrapper.h");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .header("wrapper.h")
        // The current working directory is 'onnxruntime-sys'
        .clang_arg(clang_arg)
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to (source controlled) src/generated/<os>/<arch>/bindings.rs
    let generated_file = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("src")
        .join("generated")
        .join(env::var("CARGO_CFG_TARGET_OS").unwrap())
        .join(env::var("CARGO_CFG_TARGET_ARCH").unwrap())
        .join("bindings.rs");
    bindings
        .write_to_file(&generated_file)
        .expect("Couldn't write bindings!");
}

fn generate_file_including_platform_bindings() -> Result<(), std::io::Error> {
    let generic_binding_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("src")
        .join("generated")
        .join("bindings.rs");

    let mut fh = BufWriter::new(fs::File::create(&generic_binding_path)?);

    let platform_bindings = PathBuf::from("src")
        .join("generated")
        .join(env::var("CARGO_CFG_TARGET_OS").unwrap())
        .join(env::var("CARGO_CFG_TARGET_ARCH").unwrap())
        .join("bindings.rs");

    // Build a (relative) path, as a string, to the platform-specific bindings.
    // Required so that we can escape backslash (Windows path separators) before
    // writing to the file.
    let include_path = format!(
        "{}{}",
        std::path::MAIN_SEPARATOR,
        platform_bindings.display()
    )
    .replace(r#"\"#, r#"\\"#);
    fh.write_all(
        format!(
            r#"include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "{}"
));"#,
            include_path
        )
        .as_bytes(),
    )?;

    Ok(())
}

fn download<P: AsRef<Path>>(source_url: &str, target_file: P) {
    let resp = ureq::get(source_url)
        .timeout_connect(1_000) // 1 second
        .timeout(std::time::Duration::from_secs(300))
        .call();

    if resp.error() {
        panic!("ERROR: Failed to download {}: {:#?}", source_url, resp);
    }

    let len = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap();
    let mut reader = resp.into_reader();
    // FIXME: Save directly to the file
    let mut buffer = vec![];
    let read_len = reader.read_to_end(&mut buffer).unwrap();
    assert_eq!(buffer.len(), len);
    assert_eq!(buffer.len(), read_len);

    let f = fs::File::create(&target_file).unwrap();
    let mut writer = io::BufWriter::new(f);
    writer.write_all(&buffer).unwrap();
}

fn extract_archive(filename: &Path, output: &Path) {
    #[cfg(target_family = "unix")]
    extract_tgz(filename, output);
    #[cfg(target_family = "windows")]
    extract_zip(filename, output);
}

#[cfg(target_family = "unix")]
fn extract_tgz(filename: &Path, output: &Path) {
    let file = fs::File::open(&filename).unwrap();
    let buf = io::BufReader::new(file);
    let tar = flate2::read::GzDecoder::new(buf);
    let mut archive = tar::Archive::new(tar);
    archive.unpack(output).unwrap();
}

#[cfg(target_family = "windows")]
fn extract_zip(filename: &Path, outpath: &Path) {
    let file = fs::File::open(&filename).unwrap();
    let buf = io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(buf).unwrap();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let outpath = outpath.join(file.sanitized_name());
        if !(&*file.name()).ends_with('/') {
            println!(
                "File {} extracted to \"{}\" ({} bytes)",
                i,
                outpath.as_path().display(),
                file.size()
            );
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    fs::create_dir_all(&p).unwrap();
                }
            }
            let mut outfile = fs::File::create(&outpath).unwrap();
            io::copy(&mut file, &mut outfile).unwrap();
        }
    }
}

fn prebuilt_archive_url() -> (PathBuf, String) {
    let os = env::var("CARGO_CFG_TARGET_OS").expect("Unable to get TARGET_OS");

    let gpu_str = match env::var(ORT_ENV_GPU) {
        Ok(cuda_env) => {
            match cuda_env.as_str() {
                "1" | "yes"  | "Yes" | "YES" | "on" | "On" | "ON" => {
                    match os.as_str() {
                        "linux" | "windows" => "-gpu",
                        os_str => panic!(
                            "Use of CUDA was specified with `ORT_USE_CUDA` environment variable, but pre-built \
                             binaries with CUDA are only available for Linux and Windows, not: {}.",
                            os_str
                        ),
                    }
                },
                _ => "",
            }
        }
        Err(_) => "",
    };

    let arch_str = match os.as_str() {
        "windows" => {
            if gpu_str.is_empty() {
                "x86"
            } else {
                "x64"
            }
        }
        _ => "x64",
    };

    let (os_str, archive_extension) = match os.as_str() {
        "windows" => ("win", "zip"),
        "macos" => ("osx", "tgz"),
        "linux" => ("linux", "tgz"),
        _ => panic!("Unsupported target os {:?}", os),
    };

    let prebuilt_archive = format!(
        "onnxruntime-{}-{}{}-{}.{}",
        os_str, arch_str, gpu_str, ORT_VERSION, archive_extension
    );
    let prebuilt_url = format!(
        "{}/v{}/{}",
        ORT_RELEASE_BASE_URL, ORT_VERSION, prebuilt_archive
    );

    (PathBuf::from(prebuilt_archive), prebuilt_url)
}

fn prepare_libort_dir_prebuilt() -> PathBuf {
    let (prebuilt_archive, prebuilt_url) = prebuilt_archive_url();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let extract_dir = out_dir.join(ORT_PREBUILT_EXTRACT_DIR);
    let downloaded_file = out_dir.join(&prebuilt_archive);

    if !downloaded_file.exists() {
        println!("Creating directory {:?}", out_dir);
        fs::create_dir_all(&out_dir).unwrap();

        println!(
            "Downloading {} into {}",
            prebuilt_url,
            downloaded_file.display()
        );
        download(&prebuilt_url, &downloaded_file);
    }

    if !extract_dir.exists() {
        println!("Extracting to {}...", extract_dir.display());
        extract_archive(&downloaded_file, &extract_dir);
    }

    extract_dir.join(prebuilt_archive.file_stem().unwrap())
}

fn prepare_libort_dir() -> PathBuf {
    let strategy = env::var(ORT_ENV_STRATEGY);
    println!(
        "strategy: {:?}",
        strategy
            .as_ref()
            .map(String::as_str)
            .unwrap_or_else(|_| "unknown")
    );
    match strategy.as_ref().map(String::as_str) {
        Ok("download") | Err(_) => prepare_libort_dir_prebuilt(),
        Ok("system") => PathBuf::from(match env::var(ORT_ENV_SYSTEM_LIB_LOCATION) {
            Ok(p) => p,
            Err(e) => {
                panic!(
                    "Could not get value of environment variable {:?}: {:?}",
                    ORT_ENV_SYSTEM_LIB_LOCATION, e
                );
            }
        }),
        Ok("compile") => unimplemented!(),
        _ => panic!("Unknown value for {:?}", ORT_ENV_STRATEGY),
    }
}
