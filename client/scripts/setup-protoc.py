"""Install the pinned compiler without querying GitHub's Releases listing API."""

import hashlib
import http.client
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

VERSION = "29.3"


class DownloadIntegrityError(ValueError):
    """A response arrived, but it was not the pinned release archive."""


# The Linux and macOS ARM hashes also appear in vendor/echo-agent-build/bin/protoc.
ASSETS = {
    ("Linux", "x86_64"): ("linux-x86_64", "3e866620c5be27664f3d2fa2d656b5f3e09b5152b42f1bedbf427b333e90021a"),
    ("Linux", "arm64"): ("linux-aarch_64", "6427349140e01f06e049e707a58709a4f221ae73ab9a0425bc4a00c8d0e1ab32"),
    ("Darwin", "arm64"): ("osx-aarch_64", "2b8a3403cd097f95f3ba656e14b76c732b6b26d7f183330b11e36ef2bc028765"),
    ("Darwin", "x86_64"): ("osx-x86_64", "9a788036d8f9854f7b03c305df4777cf0e54e5b081e25bf15252da87e0e90875"),
    ("Windows", "x86_64"): ("win64", "57ea59e9f551ad8d71ffaa9b5cfbe0ca1f4e720972a1db7ec2d12ab44bff9383"),
}


def asset_for(system, machine):
    arch = {"amd64": "x86_64", "aarch64": "arm64"}.get(machine.lower(), machine.lower())
    try:
        suffix, digest = ASSETS[(system, arch)]
    except KeyError:
        raise ValueError(f"Unsupported protoc host: {system}/{machine}") from None
    name = f"protoc-{VERSION}-{suffix}.zip"
    return f"https://github.com/protocolbuffers/protobuf/releases/download/v{VERSION}/{name}", digest


def download(url, destination, digest, *, open_url=urllib.request.urlopen, sleep=time.sleep):
    if destination.is_file():
        if hashlib.sha256(destination.read_bytes()).hexdigest() == digest:
            print(f"Using verified protoc archive: {destination}", flush=True)
            return
        print(f"Ignoring invalid cached protoc archive: {destination}", flush=True)
        destination.unlink()
    for attempt in range(1, 4):
        try:
            with open_url(url, timeout=30) as response:
                data = response.read()
            actual = hashlib.sha256(data).hexdigest()
            if actual != digest:
                raise DownloadIntegrityError(f"protoc SHA-256 mismatch: expected {digest}, got {actual}")
            # Cache only a fully verified response. An interrupted writer must
            # not leave a partial archive for the next job or local invocation.
            partial = None
            try:
                with tempfile.NamedTemporaryFile(dir=destination.parent, suffix=".part", delete=False) as output:
                    partial = Path(output.name)
                    output.write(data)
                partial.replace(destination)
            finally:
                if partial:
                    partial.unlink(missing_ok=True)
            return
        except urllib.error.HTTPError as error:
            if error.code != 429 and error.code < 500:
                raise
            failure = error
        except (DownloadIntegrityError, urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException) as error:
            failure = error
        if attempt == 3:
            if isinstance(failure, DownloadIntegrityError):
                raise failure
            raise RuntimeError(f"protoc download failed after {attempt} attempts: {url}") from failure
        print(f"protoc download attempt {attempt} failed: {failure}; retrying", flush=True)
        sleep(attempt * 2)


def extract(archive, destination):
    with zipfile.ZipFile(archive) as bundle:
        for name in bundle.namelist():
            target = (destination / name).resolve()
            if not target.is_relative_to(destination.resolve()):
                raise ValueError(f"Unsafe protoc archive path: {name}")
        bundle.extractall(destination)


def main():
    system = platform.system()
    url, digest = asset_for(system, platform.machine())
    root = Path(tempfile.mkdtemp(prefix="echo-protoc-", dir=os.environ.get("RUNNER_TEMP")))
    cache = os.environ.get("ECHO_PROTOC_CACHE")
    cache_root = Path(cache) if cache else root
    cache_root.mkdir(parents=True, exist_ok=True)
    archive = cache_root / url.rsplit("/", 1)[-1]
    print(f"Installing protoc {VERSION}: {url}", flush=True)
    download(url, archive, digest)
    extract(archive, root)
    executable = root / "bin" / ("protoc.exe" if system == "Windows" else "protoc")
    executable.chmod(0o755)
    version = subprocess.check_output([str(executable), "--version"], text=True).strip()
    if version != f"libprotoc {VERSION}":
        raise RuntimeError(f"Unexpected protoc version: {version}")
    if not (root / "include/google/protobuf/descriptor.proto").is_file():
        raise RuntimeError("protoc archive is missing the well-known type definitions")
    # PROTOC bypasses local dotslash wrappers and gives Rust the same fixed
    # executable whose version and archive we just checked on every platform.
    for key, value in (("GITHUB_PATH", str(executable.parent)), ("GITHUB_ENV", f"PROTOC={executable}")):
        if os.environ.get(key):
            with open(os.environ[key], "a", encoding="utf-8") as output:
                output.write(value + "\n")
    if not cache:
        archive.unlink()
    print(f"Installed {version}: {executable}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        if os.environ.get("GITHUB_ACTIONS") == "true":
            message = str(error).replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
            print(f"::error title=protoc installation failed::{message}", flush=True)
        raise
