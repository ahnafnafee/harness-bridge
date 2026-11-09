"""Verify release archive checksums, contents, architecture and optional native smoke tests."""

import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import subprocess
import tarfile
import tempfile
import zipfile

from verify_workbench import verify as verify_workbench


TARGETS = {
    "x86_64-pc-windows-msvc": (".zip", "harness-bridge.exe"),
    "x86_64-unknown-linux-gnu": (".tar.gz", "harness-bridge"),
    "aarch64-apple-darwin": (".tar.gz", "harness-bridge"),
}


def verify_archive(archive, version, checksums, smoke=False):
    target = next((name for name, (suffix, _) in TARGETS.items()
                   if archive.name == f"harness-bridge-v{version}-{name}{suffix}"), None)
    if not target:
        raise ValueError(f"unexpected archive name: {archive.name}")
    suffix, executable = TARGETS[target]
    expected_hashes = {}
    for line in checksums.read_text(encoding="utf-8").splitlines():
        digest, filename = line.split(maxsplit=1)
        filename = filename.lstrip(" *")
        if filename in expected_hashes:
            raise ValueError(f"duplicate checksum: {filename}")
        expected_hashes[filename] = digest
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    assert expected_hashes.get(archive.name) == digest, "archive checksum mismatch"
    directory = archive.name[:-len(suffix)]

    def validate_name(name):
        path = PurePosixPath(name.replace("\\", "/"))
        if path.is_absolute() or ".." in path.parts or ":" in name or not path.parts or path.parts[0] != directory:
            raise ValueError(f"unsafe archive path: {name}")

    with tempfile.TemporaryDirectory(prefix="bridge-release-") as temporary:
        destination = Path(temporary)
        if suffix == ".zip":
            with zipfile.ZipFile(archive) as package:
                for member in package.infolist():
                    validate_name(member.filename)
                    if stat.S_ISLNK(member.external_attr >> 16):
                        raise ValueError(f"archive symlink: {member.filename}")
                package.extractall(destination)
        else:
            with tarfile.open(archive, "r:gz") as package:
                for member in package.getmembers():
                    validate_name(member.name)
                    if not (member.isdir() or member.isfile()):
                        raise ValueError(f"unsupported archive entry: {member.name}")
                package.extractall(destination, filter="data")
        root = destination / directory
        required = [executable, "README.md", "LICENSE", "AGENTS.md", "docs/interfaces.md",
                    "docs/migration-guide.md", f"docs/releases/v{version}.md", "assets/icon.png", "assets/icon.ico",
                    "assets/fonts/ibm-plex-sans.woff2", "assets/fonts/ibm-plex-mono.woff2",
                    "assets/fonts/OFL.txt", "assets/licenses/GPUI-Kit-Apache-2.0.txt",
                    "assets/licenses/Lucide-ISC.txt", "assets/licenses/README.md"]
        for name in required:
            assert (root / name).is_file(), f"missing package file: {name}"
        binary = root / executable
        header = binary.read_bytes()
        if target.endswith("windows-msvc"):
            assert header[:2] == b"MZ", "expected Windows executable"
            pe_offset = struct.unpack_from("<I", header, 0x3C)[0]
            assert header[pe_offset:pe_offset + 4] == b"PE\0\0", "invalid PE header"
            assert struct.unpack_from("<H", header, pe_offset + 4)[0] == 0x8664, "expected x64 PE"
        elif target.endswith("linux-gnu"):
            assert header[:6] == b"\x7fELF\x02\x01", "expected little-endian ELF64"
            assert struct.unpack_from("<H", header, 18)[0] == 62, "expected x64 ELF"
        else:
            assert header[:4] == b"\xcf\xfa\xed\xfe", "expected Mach-O executable"
            assert struct.unpack_from("<I", header, 4)[0] == 0x0100000C, "expected arm64 Mach-O"
        print(f"PASS: {archive.name} checksum, package contents and architecture")
        if smoke:
            if os.name != "nt":
                assert os.access(binary, os.X_OK), "packaged binary is not executable"
            creation = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
            result = subprocess.run([str(binary), "--version"], capture_output=True, text=True,
                                    timeout=20, check=True, creationflags=creation)
            assert result.stdout.strip() == f"harness-bridge {version}", result.stdout
            verify_workbench(binary, expected_assets=root / "assets")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--version", required=True, help="Expected version without the v prefix")
    parser.add_argument("--checksums", type=Path, required=True)
    parser.add_argument("--smoke", action="store_true", help="Run the native executable against temporary stores")
    arguments = parser.parse_args()
    verify_archive(arguments.archive.resolve(), arguments.version, arguments.checksums.resolve(), arguments.smoke)
