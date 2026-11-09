"""Build the optional GPUI browser frontend, or verify its checked-in bundle."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
UI = ROOT / "ui"
DIST = ROOT / "src/web/gpui"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def sources():
    files = [UI / "Cargo.toml", UI / "Cargo.lock", UI / "rust-toolchain.toml", UI / ".cargo/config.toml"]
    files += sorted((UI / "src").rglob("*.rs"))
    files += [ROOT / "assets/fonts" / name for name in ("IBMPlexSans-Regular.ttf", "IBMPlexSans-Medium.ttf", "IBMPlexSans-SemiBold.ttf", "IBMPlexMono.ttf")]
    return {p.relative_to(ROOT).as_posix(): hashlib.sha256(p.read_bytes().replace(b"\r\n", b"\n") if p.suffix != ".ttf" else p.read_bytes()).hexdigest() for p in files}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wasm-bindgen", default=shutil.which("wasm-bindgen"))
    parser.add_argument("--check", action="store_true", help="Verify sources and bundle without build tools")
    args = parser.parse_args()
    manifest_path = DIST / "build.json"
    if args.check:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest["sources"] != sources():
            raise RuntimeError("GPUI sources changed; rebuild with scripts/build_ui.py")
        for name, expected in manifest["assets"].items():
            if digest(DIST / name) != expected:
                raise RuntimeError(f"GPUI bundle checksum mismatch: {name}")
        print("PASS: GPUI bundle matches its Rust sources and pinned dependencies")
        return
    if not args.wasm_bindgen:
        raise RuntimeError("Install wasm-bindgen-cli 0.2.127 or pass --wasm-bindgen <path>")
    version = subprocess.check_output([args.wasm_bindgen, "--version"], text=True).strip()
    if version != "wasm-bindgen 0.2.127":
        raise RuntimeError(f"Expected wasm-bindgen 0.2.127; found {version}")
    subprocess.run(["cargo", "build", "--locked", "--target", "wasm32-unknown-unknown", "--release"], cwd=UI, check=True)
    wasm = UI / "target/wasm32-unknown-unknown/release/harness_bridge_ui.wasm"
    DIST.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hb-gpui-bindings-") as temporary:
        subprocess.run([args.wasm_bindgen, str(wasm), "--target", "web", "--no-typescript", "--out-dir", temporary], check=True)
        generated = Path(temporary)
        if any(p.is_dir() for p in generated.iterdir()):
            raise RuntimeError("Unexpected split bindings; update the explicit asset server before packaging")
        shutil.copyfile(generated / "harness_bridge_ui.js", DIST / "harness_bridge_ui.js")
        module = (generated / "harness_bridge_ui_bg.wasm").read_bytes()
        (DIST / "harness_bridge_ui_bg.wasm.gz").write_bytes(gzip.compress(module, compresslevel=9, mtime=0))
    names = ["harness_bridge_ui.js", "harness_bridge_ui_bg.wasm.gz"]
    manifest = {"gpui_kit": "0.9.0", "toolchain": "nightly-2026-10-15", "wasm_bindgen": "0.2.127", "wasm_bytes": len(module), "sources": sources(), "assets": {name: digest(DIST / name) for name in names}}
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Built GPUI frontend: {len(module):,} bytes, {(DIST / names[1]).stat().st_size:,} bytes compressed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
