#!/usr/bin/env python3
"""Read-only Verge core inventory; explicit, stopped-service atomic replacement.

No service, proxy, TUN, route or controller mutations. Python standard library only.
"""
import argparse
import contextlib
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
import tempfile

DEFAULT_CORE = Path('/Library/Application Support/clash-verge-service/cores/verge-mihomo-alpha')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def regular(path):
    # Never silently resolve a service path, backup or ancestor through a symlink.
    for item in (path, *path.parents):
        if item.is_symlink():
            raise ValueError(f'symlink refused: {item}')
    info = path.stat()
    if not stat.S_ISREG(info.st_mode):
        raise ValueError(f'not a regular file: {path}')
    return info


def version(path):
    # Only explicitly requested by --probe-versions, never execute a core as root.
    if os.geteuid() == 0:
        return {'error': 'version execution refused as root; probe as normal user'}
    try:
        result = subprocess.run([str(path), '-v'], capture_output=True, text=True,
                                timeout=5, check=True)
        return {'output': (result.stdout + result.stderr).strip()}
    except (OSError, subprocess.SubprocessError) as exc:
        return {'error': str(exc)}


def inventory(path, probe=False):
    result = {'path': str(path)}
    try:
        info = regular(path)
        result.update(sha256=digest(path), size=info.st_size, uid=info.st_uid,
                      gid=info.st_gid, mode=oct(stat.S_IMODE(info.st_mode)))
        if probe:
            result['version'] = version(path)
        else:
            result['version'] = 'unprobed; use --probe-versions as a normal user'
    except (OSError, ValueError) as exc:
        result['error'] = str(exc)
    return result


def processes(target=DEFAULT_CORE):
    """Darwin proc_pidpath is authoritative; ps command strings are not paths."""
    if platform.system() != 'Darwin':
        raise RuntimeError('process discovery requires macOS')
    lib = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    lib.proc_pidpath.restype = ctypes.c_int
    output = subprocess.check_output(['/bin/ps', '-axo', 'pid=,comm='], text=True)
    found, unresolved = [], []
    for line in output.splitlines():
        pid, command = line.strip().split(None, 1)
        buf = ctypes.create_string_buffer(4096)
        if lib.proc_pidpath(int(pid), buf, len(buf)) > 0:
            path = os.fsdecode(buf.value)
            if Path(path) == target or 'clash' in path.lower() or 'mihomo' in path.lower():
                found.append({'pid': int(pid), 'path': path})
        elif command == str(target) or 'clash' in command.lower() or 'mihomo' in command.lower():
            unresolved.append({'pid': int(pid), 'command': command})
    return found, unresolved


def stopped(target):
    found, unresolved = processes(target)
    if unresolved or any(Path(p['path']) == target for p in found):
        raise RuntimeError('target running or core identity unresolved; stop Verge core/service manually first')


def trusted_directory(path):
    for item in (path, *path.parents):
        info = item.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise ValueError(f'untrusted directory (owner/write permissions): {item}')


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def copy_attributes(src_fd, dst_fd):
    if platform.system() == 'Darwin':
        # CPython's os.*xattr API is Linux-only. Use Darwin's native fd API;
        # COPYFILE_XATTR = 1 << 2, without copying data/ownership or invoking a core.
        lib = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
        lib.fcopyfile.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        lib.fcopyfile.restype = ctypes.c_int
        if lib.fcopyfile(src_fd, dst_fd, None, 4) != 0:
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code))
    else:
        for name in os.listxattr(src_fd):
            os.setxattr(dst_fd, name, os.getxattr(src_fd, name))


def copy_file(source, destination, info):
    # Exclusive creation and no-follow reads; backups/stages live beside target.
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as src, destination.open('xb') as dst:
        if not stat.S_ISREG(os.fstat(src.fileno()).st_mode):
            raise ValueError('source changed to a non-regular file')
        while chunk := src.read(1024 * 1024):
            dst.write(chunk)
        dst.flush()
        # Preserve code-signing/quarantine/resource-fork attributes as well as bytes.
        # Attribute failures abort before installation rather than silently stripping them.
        copy_attributes(src.fileno(), dst.fileno())
        os.fchown(dst.fileno(), info.st_uid, info.st_gid)
        os.fchmod(dst.fileno(), stat.S_IMODE(info.st_mode))
        os.fsync(dst.fileno())


@contextlib.contextmanager
def lock(parent):
    fd = os.open(parent / '.verge-core-upgrade.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(fd)


def replace(target, source, expected_current, expected_source):
    """Caller confirms and holds lock; retain durable original even on failure."""
    info = regular(target)
    regular(source)
    if info.st_mode & 0o7000:
        raise ValueError('special executable mode refused')
    if target.samefile(source) or digest(target) != expected_current or digest(source) != expected_source:
        raise ValueError('source/target identity or SHA-256 mismatch')
    stopped(target)
    backup = Path(tempfile.mkdtemp(prefix=target.name + '.backup-', dir=target.parent))
    original = backup / 'original'
    stage = backup / 'staged'
    replaced = False
    try:
        copy_file(target, original, info)
        if digest(original) != expected_current:
            raise ValueError('backup verification failed')
        manifest = {'target': str(target), 'original_sha256': expected_current,
                    'installed_sha256': expected_source}
        with (backup / 'manifest.json').open('x') as stream:
            json.dump(manifest, stream, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        sync_directory(backup)
        sync_directory(target.parent)
        print(f'Backup: {backup}', flush=True)
        copy_file(source, stage, info)
        if digest(stage) != expected_source:
            raise ValueError('staged SHA-256 mismatch')
        stopped(target)
        if digest(target) != expected_current:
            raise ValueError('target changed during preparation')
        os.replace(stage, target)
        replaced = True
        sync_directory(target.parent)
        if digest(target) != expected_source:
            raise ValueError('installed SHA-256 mismatch')
    except BaseException:
        if replaced:
            # Preserve original backup for explicit rollback/recovery retries.
            try:
                recovery = backup / 'recovery'
                copy_file(original, recovery, info)
                if digest(recovery) != expected_current:
                    raise ValueError('recovery verification failed')
                os.replace(recovery, target)
                sync_directory(target.parent)
            except BaseException as exc:
                raise RuntimeError(f'automatic recovery failed; preserve backup {backup}: {exc}') from exc
        raise
    finally:
        stage.unlink(missing_ok=True)
    return backup


def sha(value):
    if not re.fullmatch('[0-9a-fA-F]{64}', value):
        raise argparse.ArgumentTypeError('expected 64 hexadecimal SHA-256 characters')
    return value.lower()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--core', type=Path, default=DEFAULT_CORE)
    parser.add_argument('--release', type=Path, help='trusted, unpacked release executable (not an archive)')
    parser.add_argument('--probe-versions', action='store_true', help='execute trusted binaries with -v as normal user')
    action = parser.add_mutually_exclusive_group()
    action.add_argument('--install', action='store_true')
    action.add_argument('--rollback', type=Path, metavar='BACKUP_DIR')
    parser.add_argument('--sha256', type=sha, help='independently verified release SHA-256')
    parser.add_argument('--expected-current-sha256', type=sha)
    args = parser.parse_args(argv)
    target = Path(os.path.abspath(args.core))
    report = {'service_file': inventory(target, args.probe_versions)}
    if args.release:
        args.release = Path(os.path.abspath(args.release))
        report['release'] = inventory(args.release, args.probe_versions)
        report['disk_matches_release'] = (report['service_file'].get('sha256') is not None and
                                          report['service_file'].get('sha256') == report['release'].get('sha256'))
    try:
        found, unresolved = processes(target)
        report.update(processes=found, unresolved_processes=unresolved)
        report['process_files'] = [inventory(Path(p['path']), args.probe_versions) for p in found]
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        report['process_discovery_error'] = str(exc)
    report['runtime_note'] = 'Disk hashes and -v probe files, not loaded process images. Recheck after manual restart.'
    print(json.dumps(report, indent=2, ensure_ascii=False), flush=True)
    if not (args.install or args.rollback):
        return 0
    if platform.system() != 'Darwin' or os.geteuid() != 0:
        parser.error('replacement requires macOS and explicit administrator invocation; no automatic sudo')
    if args.probe_versions:
        parser.error('do not execute binaries during privileged replacement')
    if not args.expected_current_sha256:
        parser.error('--expected-current-sha256 is required')
    trusted_directory(target.parent)
    # Python's stdlib cannot faithfully copy macOS ACLs or BSD file flags.
    # Refuse these targets rather than provide an incomplete backup/restore.
    if getattr(regular(target), 'st_flags', 0):
        raise ValueError('target has BSD file flags; manual metadata-aware upgrade required')
    listing = subprocess.check_output(['/bin/ls', '-lde', str(target)], text=True)
    if re.search(r'^\s*\d+:', listing, re.MULTILINE):
        raise ValueError('target has ACL entries; manual metadata-aware upgrade required')
    with lock(target.parent):
        if args.rollback:
            backup = Path(os.path.abspath(args.rollback))
            trusted_directory(backup)
            regular(backup / 'manifest.json')
            manifest = json.loads((backup / 'manifest.json').read_text())
            if manifest['target'] != str(target):
                raise ValueError('backup belongs to another target')
            source, expected = backup / 'original', sha(manifest['original_sha256'])
        else:
            if not args.release or not args.sha256:
                parser.error('--release and --sha256 are required for install')
            source, expected = args.release, args.sha256
        # Display the exact operation before confirmation; never accept a --yes shortcut.
        regular(source)
        if digest(source) != expected or digest(target) != args.expected_current_sha256:
            raise ValueError('SHA-256 mismatch; no changes made')
        architecture = {'arm64': 'arm64', 'x86_64': 'x86_64'}.get(platform.machine())
        if architecture is None:
            raise ValueError('unsupported macOS host architecture')
        subprocess.run(['/usr/bin/lipo', str(source), '-verify_arch', architecture],
                       check=True, capture_output=True, timeout=10)
        stopped(target)
        phrase = f'REPLACE {target} {expected}'
        print(f'Service must remain stopped. Type exactly:\n{phrase}', flush=True)
        if not sys.stdin.isatty() or input('> ') != phrase:
            raise RuntimeError('confirmation declined; no replacement')
        backup = replace(target, source, args.expected_current_sha256, expected)
        print(f'Installed disk SHA-256: {expected}\nRecovery backup: {backup}\n'
              'No service restart performed. Runtime and TUN remain unverified.')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Error: {error}', file=sys.stderr)
        sys.exit(1)
