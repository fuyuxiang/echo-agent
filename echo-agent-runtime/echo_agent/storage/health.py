"""Available space on the filesystem actually holding runtime state."""

from pathlib import Path
import shutil

MIN_FREE_BYTES = 64 * 1024 * 1024
LOW_FREE_BYTES = 512 * 1024 * 1024


def filesystem_free_bytes(path: Path) -> int:
    """Use user-available blocks, including for not-yet-created data paths.

    A cloud disk's capacity and root-reserved ext4 blocks are not space the
    service can use. shutil.disk_usage uses statvfs.f_bavail on Unix.
    """
    probe = path.expanduser()
    while not probe.exists() and probe.parent != probe:
        probe = probe.parent
    return shutil.disk_usage(probe).free
