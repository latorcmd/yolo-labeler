use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/app.ico");
    let source = out.join("app.rc");
    let resource = out.join("app.res");
    std::fs::write(
        &source,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write Windows icon resource");
    let compiler = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        let kits = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK required"))
            .join("Windows Kits/10/bin");
        let mut versions: Vec<_> = std::fs::read_dir(kits)
            .expect("Install Windows SDK with Visual Studio C++ Build Tools")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("x64/rc.exe"))
            .filter(|path| path.is_file())
            .collect();
        versions.sort();
        versions
            .pop()
            .expect("Windows SDK resource compiler rc.exe")
    });
    let result = Command::new(compiler)
        .args(["/nologo", "/fo"])
        .arg(&resource)
        .arg(source)
        .status()
        .expect("run Windows resource compiler");
    assert!(result.success(), "Windows icon resource compilation failed");
    println!(
        "cargo:rustc-link-arg-bin=yolo-labeler={}",
        resource.display()
    );
}
