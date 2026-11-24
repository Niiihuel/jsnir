//! Compiles the optional TSX examples for source builds.
//! TypeScript 7 dropped `outFile`: each file is emitted separately and the SDK and the app are concatenated here.
use std::{env, fs, path::Path, process::Command};

#[path = "src/typescript.rs"]
mod typescript;

fn main() {
    let mut apps = Vec::new();
    if cfg!(feature = "examples") {
        apps.extend(["chat", "counter", "sharp"]);
    }
    if cfg!(feature = "stepper") {
        apps.extend([
            "stepper",
            "hosted",
            "images",
            "lines",
            "keyed",
            "scroll",
            "scrollchat",
            "states",
            "masks",
            "text",
            "fields",
            "spans",
            "borders",
            "scrollbar",
            "overlay",
            "fill",
            "containers",
            "composer",
            "reload",
        ]);
    }
    if apps.is_empty() {
        println!("cargo:rerun-if-changed=build.rs");
        return;
    }

    let root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = env::var_os("OUT_DIR").unwrap();
    let out = Path::new(&out);
    println!("cargo:rerun-if-env-changed=JSNIR_TSC");
    for path in [
        "app/sdk.ts",
        "app/jsnir.d.ts",
        "app/tsconfig.base.json",
        "package-lock.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let tsc = typescript::compiler(Path::new(&root));
    assert!(
        tsc.is_file(),
        "TypeScript compiler not found at {}. Set JSNIR_TSC to the native compiler or run `npm ci`.",
        tsc.display()
    );
    println!("cargo:rerun-if-changed={}", tsc.display());
    for app in apps {
        println!("cargo:rerun-if-changed=app/{app}.tsx");
        println!("cargo:rerun-if-changed=app/tsconfig.{app}.json");
        let dir = out.join(app);
        let result = Command::new(&tsc)
            .args([
                "-p",
                &format!("{root}/app/tsconfig.{app}.json"),
                "--noEmit",
                "false",
                "--outDir",
            ])
            .arg(&dir)
            .output()
            .expect("run tsc");
        // A type error does not stop the build, so jsnir.d.ts can still be regenerated from Rust;
        // the contract_typescript_apps_typecheck test makes `cargo test` fail instead.
        if !result.status.success() {
            for line in String::from_utf8_lossy(&result.stdout).lines() {
                println!("cargo:warning={app}: {line}");
            }
        }
        let read = |name: &str| {
            fs::read_to_string(dir.join(name))
                .unwrap_or_else(|e| panic!("tsc did not emit {name}: {e}"))
        };
        let sdk = read("sdk.js");
        if app == "stepper" {
            fs::write(out.join("sdk.js"), &sdk).unwrap();
        }
        fs::write(
            out.join(format!("{app}.js")),
            sdk + &read(&format!("{app}.js")),
        )
        .unwrap();
    }
}
