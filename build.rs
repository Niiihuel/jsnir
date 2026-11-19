//! Compiles the TSX apps with tsc. JSX is not erasable syntax, so Node's type stripping is not enough.
//! TypeScript 7 dropped `outFile`: each file is emitted separately and the SDK and the app are concatenated here.
use std::{env, fs, path::Path, process::Command};

fn main() {
    let root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = env::var_os("OUT_DIR").unwrap();
    let out = Path::new(&out);
    for path in ["app/sdk.ts", "app/jsnir.d.ts", "app/tsconfig.base.json"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let tsc = Path::new(&root).join("node_modules/.bin/tsc");
    assert!(
        tsc.exists(),
        "TypeScript is missing: run `npm ci` at the JSNIR root"
    );
    for app in ["chat", "counter", "stepper"] {
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
        fs::write(
            out.join(format!("{app}.js")),
            read("sdk.js") + &read(&format!("{app}.js")),
        )
        .unwrap();
    }
}
