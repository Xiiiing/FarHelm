use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FARHELM_BOOTSTRAP_ROLE");
    println!("cargo:rerun-if-env-changed=FARHELM_BOOTSTRAP_BUNDLE");

    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("bundle.tar.gz");
    match env::var_os("FARHELM_BOOTSTRAP_BUNDLE") {
        Some(source) => {
            let source = PathBuf::from(source);
            println!("cargo:rerun-if-changed={}", source.display());
            fs::copy(&source, &output).expect("copy embedded FarHelm release bundle");
            println!("cargo:rustc-env=FARHELM_BOOTSTRAP_DISTRIBUTABLE=1");
        }
        None => {
            fs::write(&output, []).expect("create development bootstrap placeholder");
            println!("cargo:rustc-env=FARHELM_BOOTSTRAP_DISTRIBUTABLE=0");
        }
    }

    let role = env::var("FARHELM_BOOTSTRAP_ROLE").unwrap_or_else(|_| "development".to_owned());
    println!("cargo:rustc-env=FARHELM_BOOTSTRAP_ROLE={role}");
}
