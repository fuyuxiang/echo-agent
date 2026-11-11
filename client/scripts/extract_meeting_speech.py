"""Extract verified speech archives without spawning an external decompressor."""

import argparse
from pathlib import Path, PurePosixPath
import shutil
import tarfile


def safe_parts(name):
    path = PurePosixPath(name)
    if path.is_absolute() or not path.parts or any(
        part == ".." or ":" in part or "\\" in part for part in path.parts
    ):
        raise ValueError(f"Unsafe archive member: {name}")
    return path.parts


def ensure_directory(root, parts):
    path = root
    for part in parts:
        path = path / part
        if path.is_symlink():
            raise ValueError(f"Symbolic directory in extraction destination: {path}")
        path.mkdir(exist_ok=True)
    return path


def extract(archive, directory, members=()):
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("Extraction requires an existing ordinary directory")
    wanted = set(members)
    for name in wanted:
        safe_parts(name)
    found = set()
    count = 0
    # Stream bz2 and tar together so large static libraries are decoded once.
    # Do not use extractall: permit only ordinary files/directories, never links.
    with tarfile.open(archive, "r|bz2") as stream:
        for member in stream:
            if wanted and member.name not in wanted:
                continue
            parts = safe_parts(member.name)
            if member.name in found:
                raise ValueError(f"Duplicate archive member: {member.name}")
            found.add(member.name)
            if member.isdir():
                ensure_directory(directory, parts)
            elif member.isfile():
                destination = ensure_directory(directory, parts[:-1]) / parts[-1]
                with stream.extractfile(member) as source, destination.open("xb") as output:
                    shutil.copyfileobj(source, output, length=1024 * 1024)
                count += 1
            else:
                raise ValueError(f"Unsupported archive member type: {member.name}")
    if wanted - found:
        raise ValueError(f"Missing archive members: {sorted(wanted - found)}")
    if not count:
        raise ValueError("Archive contained no requested regular files")
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True)
    parser.add_argument("--directory", required=True)
    parser.add_argument("members", nargs="*")
    args = parser.parse_args()
    count = extract(args.archive, args.directory, args.members)
    print(f"Extracted {count} speech resource files using Python bz2")


if __name__ == "__main__":
    main()
