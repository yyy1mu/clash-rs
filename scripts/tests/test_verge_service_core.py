"""Offline transaction tests: all executable files live in TemporaryDirectory."""
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import platform
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('verge_core', Path(__file__).parents[1] / 'verge-service-core.py')
core = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(core)


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='verge-test-')
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name).resolve()
        self.target = self.base / 'service core'
        self.release = self.base / 'release core'
        self.target.write_bytes(b'old core')
        self.target.chmod(0o751)
        self.release.write_bytes(b'new core')
        self.old = core.digest(self.target)
        self.new = core.digest(self.release)
        self.stop = patch.object(core, 'stopped')
        self.stop.start()
        self.addCleanup(self.stop.stop)

    def install(self):
        with context_output():
            return core.replace(self.target, self.release, self.old, self.new)

    def test_install_and_rollback_keep_backups_and_metadata(self):
        backup = self.install()
        self.assertEqual(core.digest(self.target), self.new)
        self.assertEqual(core.digest(backup / 'original'), self.old)
        self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), 0o751)
        self.assertEqual(self.target.stat().st_uid, os.geteuid())
        manifest = json.loads((backup / 'manifest.json').read_text())
        self.assertEqual(manifest['target'], str(self.target))
        with context_output():
            undo = core.replace(self.target, backup / 'original', self.new, manifest['original_sha256'])
        self.assertEqual(core.digest(self.target), self.old)
        self.assertEqual(core.digest(undo / 'original'), self.new)

    def test_extended_attributes_survive_install_and_rollback(self):
        try:
            set_attribute(self.target, b'original metadata')
            set_attribute(self.release, b'release metadata')
        except (OSError, subprocess.CalledProcessError):
            self.skipTest('filesystem does not support user xattrs')
        backup = self.install()
        self.assertEqual(get_attribute(self.target), b'release metadata')
        self.assertEqual(get_attribute(backup / 'original'), b'original metadata')
        with context_output():
            core.replace(self.target, backup / 'original', self.new, self.old)
        self.assertEqual(get_attribute(self.target), b'original metadata')

    def test_bad_hashes_make_no_backup(self):
        for old, new in [('0' * 64, self.new), (self.old, '0' * 64)]:
            with self.assertRaises(ValueError):
                core.replace(self.target, self.release, old, new)
        self.assertEqual(len(list(self.base.iterdir())), 2)
        self.assertEqual(core.digest(self.target), self.old)

    def test_copy_failure_leaves_original(self):
        real_copy = core.copy_file
        def fail_stage(src, dst, info):
            if dst.name == 'staged':
                raise OSError('disk full')
            real_copy(src, dst, info)
        with patch.object(core, 'copy_file', side_effect=fail_stage), self.assertRaises(OSError):
            self.install()
        self.assertEqual(core.digest(self.target), self.old)
        backup = next(self.base.glob('*.backup-*'))
        self.assertEqual(core.digest(backup / 'original'), self.old)

    def test_post_replace_failure_restores_original(self):
        real_sync = core.sync_directory
        calls = 0
        def fail_after_replace(path):
            nonlocal calls
            calls += 1
            if calls == 3:
                raise OSError('injected post-replace fsync failure')
            real_sync(path)
        with patch.object(core, 'sync_directory', side_effect=fail_after_replace), self.assertRaises(OSError):
            self.install()
        self.assertEqual(core.digest(self.target), self.old)
        self.assertEqual(core.digest(next(self.base.glob('*.backup-*')) / 'original'), self.old)

    def test_staged_source_change_refused(self):
        real_copy = core.copy_file
        def corrupt(src, dst, info):
            real_copy(src, dst, info)
            if dst.name == 'staged':
                dst.write_bytes(b'changed source')
        with patch.object(core, 'copy_file', side_effect=corrupt), self.assertRaises(ValueError):
            self.install()
        self.assertEqual(core.digest(self.target), self.old)

    def test_symlink_source_target_and_parent_refused(self):
        link = self.base / 'link'
        link.symlink_to(self.release)
        with self.assertRaises(ValueError):
            core.replace(self.target, link, self.old, self.new)
        parent = self.base / 'parent'
        parent.symlink_to(self.base, target_is_directory=True)
        with self.assertRaises(ValueError):
            core.regular(parent / self.target.name)
        with self.assertRaises(ValueError):
            core.replace(link, self.release, self.new, self.new)

    def test_recovery_failure_reports_retained_backup(self):
        real_copy = core.copy_file
        real_sync = core.sync_directory
        calls = 0
        def fail_sync(path):
            nonlocal calls
            calls += 1
            if calls == 3:
                raise OSError('post-replace failure')
            real_sync(path)
        def fail_recovery(src, dst, info):
            if dst.name == 'recovery':
                raise OSError('recovery disk full')
            real_copy(src, dst, info)
        with patch.object(core, 'sync_directory', side_effect=fail_sync), \
             patch.object(core, 'copy_file', side_effect=fail_recovery), \
             self.assertRaisesRegex(RuntimeError, 'automatic recovery failed; preserve backup'):
            self.install()
        backup = next(self.base.glob('*.backup-*'))
        self.assertEqual(core.digest(backup / 'original'), self.old)
        self.assertTrue((backup / 'manifest.json').exists())

    def test_darwin_attribute_api_failure_aborts(self):
        from unittest.mock import MagicMock
        library = MagicMock()
        library.fcopyfile.return_value = -1
        with patch.object(core.platform, 'system', return_value='Darwin'), \
             patch.object(core.ctypes, 'CDLL', return_value=library), \
             patch.object(core.ctypes, 'get_errno', return_value=13), self.assertRaises(OSError):
            core.copy_attributes(10, 11)
        library.fcopyfile.assert_called_once_with(10, 11, None, 4)

    def test_special_mode_refused(self):
        self.target.chmod(0o4755)
        with self.assertRaises(ValueError):
            self.install()

    def test_read_only_default_does_not_execute_or_write(self):
        with patch.object(core, 'processes', return_value=([], [])), patch.object(core, 'version') as probe, context_output():
            self.assertEqual(core.main(['--core', str(self.target), '--release', str(self.release)]), 0)
        probe.assert_not_called()
        self.assertEqual(len(list(self.base.iterdir())), 2)

    def test_running_or_unresolved_refused(self):
        # Remove transaction guard mock to test the real guard.
        self.stop.stop()
        for result in [([{'pid': 42, 'path': str(self.target)}], []), ([], [{'pid': 42}])]:
            with patch.object(core, 'processes', return_value=result), self.assertRaises(RuntimeError):
                core.stopped(self.target)

    def test_root_never_executes_version(self):
        with patch.object(core.os, 'geteuid', return_value=0), patch.object(core.subprocess, 'run') as run:
            self.assertIn('error', core.version(self.release))
        run.assert_not_called()

    def test_lock_excludes_second_writer(self):
        with core.lock(self.base):
            with self.assertRaises(BlockingIOError):
                with core.lock(self.base):
                    self.fail('second writer admitted')

    def test_privileged_confirmation_declined(self):
        args = ['--core', str(self.target), '--release', str(self.release), '--install',
                '--sha256', self.new, '--expected-current-sha256', self.old]
        with patch.object(core.platform, 'system', return_value='Darwin'), \
             patch.object(core.platform, 'machine', return_value='arm64'), \
             patch.object(core.os, 'geteuid', return_value=0), \
             patch.object(core, 'trusted_directory'), \
             patch.object(core, 'processes', return_value=([], [])), \
             patch.object(core.subprocess, 'run'), \
             patch.object(core.subprocess, 'check_output', return_value='-rwxr-xr-x 1 root wheel core'), \
             patch.object(core.sys.stdin, 'isatty', return_value=True), \
             patch('builtins.input', return_value='no'), context_output(), \
             self.assertRaises(RuntimeError):
            core.main(args)
        self.assertEqual(core.digest(self.target), self.old)
        self.assertFalse(list(self.base.glob('*.backup-*')))


def set_attribute(path, value):
    if platform.system() == 'Darwin':
        subprocess.run(['/usr/bin/xattr', '-wx', 'user.verge_test', value.hex(), str(path)], check=True)
    else:
        os.setxattr(path, 'user.verge_test', value)


def get_attribute(path):
    if platform.system() == 'Darwin':
        return bytes.fromhex(subprocess.check_output(
            ['/usr/bin/xattr', '-px', 'user.verge_test', str(path)], text=True))
    return os.getxattr(path, 'user.verge_test')


def context_output():
    return patch('sys.stdout', new_callable=io.StringIO)


if __name__ == '__main__':
    unittest.main()
