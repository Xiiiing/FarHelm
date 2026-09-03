use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FARHELM_CONSOLE_EMBED_DIR");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"))
        .join("embedded_console.rs");
    let mut generated = String::from("pub(crate) static EMBEDDED_CONSOLE: &[(&str, &[u8])] = &[\n");

    if let Some(root) = env::var_os("FARHELM_CONSOLE_EMBED_DIR").map(PathBuf::from) {
        let root = root
            .canonicalize()
            .expect("console embed directory is invalid");
        assert!(
            root.join("index.html").is_file(),
            "embedded Console is missing index.html"
        );
        let mut files = Vec::new();
        collect_files(&root, &root, &mut files);
        files.sort();
        for path in files {
            println!("cargo:rerun-if-changed={}", path.display());
            let relative = path
                .strip_prefix(&root)
                .expect("embedded asset escaped Console root")
                .to_string_lossy()
                .replace('\\', "/");
            let absolute = path.to_string_lossy();
            generated.push_str(&format!(
                "    ({relative:?}, include_bytes!({absolute:?}) as &[u8]),\n"
            ));
        }
    }
    generated.push_str("];\n");
    fs::write(output, generated).expect("failed to generate embedded Console table");
}

fn collect_files(root: &std::path::Path, directory: &std::path::Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("failed to read Console directory") {
        let entry = entry.expect("failed to inspect Console entry");
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).expect("failed to inspect Console metadata");
        assert!(
            !metadata.file_type().is_symlink(),
            "embedded Console must not contain symbolic links"
        );
        if metadata.is_dir() {
            collect_files(root, &path, files);
        } else if metadata.is_file() {
            assert!(
                path.starts_with(root),
                "embedded asset escaped Console root"
            );
            files.push(path);
        }
    }
}
