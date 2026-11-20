# JSNIR releases

The preview targets Linux x86-64 with glibc and is built on Ubuntu 24.04. The archive contains the JSNIR engine with embedded QuickJS, the TSX SDK sources and license notices. The engine opens compiled app bundles supplied at launch. Running a bundle requires the system graphics stack and fonts. Windowed apps use Wayland or X11.

## Install a version

After the preview is published, download its `.tar.gz` and matching `.tar.gz.sha256` into the same directory. Choose the tag you want to install. The example below uses `v0.1.0`. Each archive extracts into a directory named after its version, so older versions can stay installed. [GitHub Releases](https://github.com/Niiihuel/jsnir/releases)

```sh
sha256sum --check jsnir-v0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf jsnir-v0.1.0-linux-x86_64.tar.gz
cd jsnir-v0.1.0-linux-x86_64
./jsnir-gpu --version
```

## Run an external app

Run a JSNIR bundle with the engine from the extracted archive. The bundle already contains the SDK and the app's JavaScript. It runs without Rust, Node, npm or a TypeScript compiler.

```sh
./jsnir-gpu run /path/to/app.js --title "My application" --size 800x600
```

Use `--offscreen file.ppm` with a new output path to render without creating a window.

To make a bundle, save an entry file named `app.tsx`.

```tsx
mount(() => (
    <Box style={{ padding: 24 }} paint={{ background: 0x182538ff }}>
        <Text paint={{ color: 0xffffffff }}>Hello from JSNIR</Text>
    </Box>
));
```

The included SDK build helper checks a single TSX file against the matching declarations and produces a standalone JavaScript bundle. Set `JSNIR_TSC` to the native TypeScript executable. The helper uses `tsc` from PATH if the variable is unset. [Pinned compiler setup](../README.md#run-the-stepper)

```sh
bash sdk/build-app.sh app.tsx app.js
./jsnir-gpu run app.js
```

QuickJS evaluates the bundle as a single script. The SDK and the app share the same global scope, so the entry uses `mount` and JSX primitives directly. To build from multiple files, emit one compatible script with the SDK first and the app's entry last.

## Build an archive locally

Run from the repository root on Linux x86-64 with Rust, a C compiler, binutils, curl, OpenSSL, jq, GNU coreutils and Mesa software rendering. The release workflow uses Rust 1.99.1 and cargo-about 0.9.0.

```sh
cargo install --locked --version 0.9.0 --features cli cargo-about
bash scripts/build-release.sh target/preview
```

The engine builds with Cargo alone. For the package smoke test, the script retrieves the native TypeScript executable and standard library declarations pinned by `package-lock.json`. It verifies the SHA-512 before extraction. Set `JSNIR_TSC` to an existing native compiler of the pinned version to reuse it.

The script tests the core and GPU backend with the packaged feature set. It generates and extracts the archive, builds a fresh TSX app with the packaged SDK and renders it with software OpenGL and an empty tool path. The test app lives in the temporary build directory.

## Prepare a GitHub release

Run the **Linux release** workflow manually to produce an Actions artifact for review. A pushed `vX.Y.Z` tag must match the `jsnir-gpu` package version and produces a prerelease draft with the verified archive and checksum. The draft stays available for review before publication. [Release workflow](../.github/workflows/release.yml)

The demos are source-build features. Use `--features examples` for the chat and `--features stepper` for the React Bits comparison. Git's export attributes exclude the comparison and TSX port from the source archives attached by GitHub. The workflow verifies those exclusions before building. [React Bits license](https://github.com/DavidHDev/react-bits/blob/fefc5b05ca34a7475b38a977127441fbfad2f177/LICENSE.md) · [GitHub source archives](https://docs.github.com/en/repositories/working-with-files/using-files/downloading-source-code-archives)
