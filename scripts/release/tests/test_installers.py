#!/usr/bin/env python3
"""Regression tests for install.sh and install.ps1 against a mock GitHub Release.

A local HTTP server imitates https://github.com/<repo>/releases: `/latest`
redirects to `/tag/v<latest>`, assets live under `/download/v<version>/`, and
individual paths can be made to fail. POSIX tests use synthetic archives whose
`repogi` is a tiny script, plus a shim `uname`/`getconf` to imitate other
platforms. Set REPOGI_TEST_ARCHIVES to a directory of real packaged archives
(and REPOGI_TEST_VERSION to their version) to also install the real build for
this host and run it; on Windows only those real-archive tests run, through
install.ps1. Run: python3 scripts/release/tests/test_installers.py -v
"""
from __future__ import annotations

import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/release'))
import contract  # noqa: E402

SHELL = os.environ.get('REPOGI_INSTALLER_SHELL', 'sh')
REAL_ARCHIVES = Path(os.environ['REPOGI_TEST_ARCHIVES']) if os.environ.get('REPOGI_TEST_ARCHIVES') else None
REAL_VERSION = os.environ.get('REPOGI_TEST_VERSION')
WINDOWS = os.name == 'nt'


def host_target() -> str | None:
    system, machine = platform.system(), platform.machine().lower()
    arch = {'x86_64': 'x86_64', 'amd64': 'x86_64', 'aarch64': 'aarch64', 'arm64': 'aarch64'}.get(machine)
    return {('Linux', 'x86_64'): 'x86_64-unknown-linux-gnu', ('Linux', 'aarch64'): 'aarch64-unknown-linux-gnu',
            ('Darwin', 'x86_64'): 'x86_64-apple-darwin', ('Darwin', 'aarch64'): 'aarch64-apple-darwin',
            ('Windows', 'x86_64'): 'x86_64-pc-windows-msvc'}.get((system, arch))


def tar_bytes(entries: list[tuple]) -> bytes:
    """entries: (name, data|None for dir, mode) or (name, 'symlink', target)."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w:gz') as tar:
        for name, data, extra in entries:
            info = tarfile.TarInfo(name)
            if data is None:
                info.type, info.mode = tarfile.DIRTYPE, 0o755
                tar.addfile(info)
            elif data == 'symlink':
                info.type, info.linkname = tarfile.SYMTYPE, extra
                tar.addfile(info)
            else:
                info.size, info.mode = len(data), extra
                tar.addfile(info, io.BytesIO(data))
    return raw.getvalue()


def fake_archive(version: str, target: str, reported: str | None = None) -> bytes:
    stem = contract.stem(version, target)
    script = f'#!/bin/sh\necho "{reported or "repogi " + version}"\n'.encode()
    return tar_bytes([(stem, None, 0), (f'{stem}/repogi', script, 0o755), (f'{stem}/LICENSE', b'MIT\n', 0o644)])


class Release:
    """Mock releases tree served over loopback HTTP."""

    def __init__(self, root: Path):
        self.root = root
        self.latest: str | None = None
        self.fail: dict[str, int] = {}
        self.requests: list[str] = []

    def publish(self, version: str, assets: dict[str, bytes], checksums: dict[str, str] | None = None) -> Path:
        folder = self.root / 'download' / f'v{version}'
        folder.mkdir(parents=True, exist_ok=True)
        for name, data in assets.items():
            (folder / name).write_bytes(data)
        sums = checksums if checksums is not None else {n: hashlib.sha256(d).hexdigest() for n, d in assets.items()}
        (folder / 'SHA256SUMS').write_text(''.join(f'{h}  {n}\n' for n, h in sorted(sums.items())))
        return folder

    def serve(self) -> str:
        release = self

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(release.root), **kwargs)

            def log_message(self, *args):
                pass

            def route(self) -> bool:
                release.requests.append(self.path)
                path = self.path.split('?')[0]
                if not path.startswith('/releases/'):
                    self.send_error(404)
                    return True
                self.path = path[len('/releases'):]
                for suffix, code in release.fail.items():
                    if self.path.endswith(suffix):
                        self.send_error(code)
                        return True
                if self.path == '/latest':
                    self.send_response(302)
                    self.send_header('Location', f'/releases/tag/v{release.latest}' if release.latest else '/releases')
                    self.end_headers()
                    return True
                if self.path.startswith('/tag/') or self.path == '':
                    self.send_response(200)
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return True
                return False

            def do_GET(self):
                if not self.route():
                    super().do_GET()

            def do_HEAD(self):
                if not self.route():
                    super().do_HEAD()

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return f'http://127.0.0.1:{self.server.server_address[1]}/releases'

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class InstallerCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix='repogi installer test '))
        self.release = Release(self.tmp / 'srv' / 'releases')
        self.release.root.mkdir(parents=True)
        self.url = self.release.serve()
        self.home = self.tmp / 'home dir'
        self.home.mkdir()
        self.shim = self.tmp / 'shim'

    def tearDown(self):
        self.release.stop()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def snapshot(self, root: Path) -> dict[str, str]:
        if not root.exists():
            return {}
        return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else 'dir'
                for p in root.rglob('*')}


@unittest.skipIf(WINDOWS, 'install.sh targets macOS and Linux')
class ShellInstallerTests(InstallerCase):
    def fake_platform(self, system: str, machine: str, glibc: str | None = 'glibc 2.39',
                      translated: str = '0', musl: bool = False) -> None:
        self.shim.mkdir(exist_ok=True)
        scripts = {
            'uname': f'#!/bin/sh\ncase "$1" in -s) echo "{system}";; -m) echo "{machine}";; *) echo "{system}";; esac\n',
            'getconf': ('#!/bin/sh\ncase "$1" in GNU_LIBC_VERSION) ' +
                        (f'echo "{glibc}"' if glibc else 'exit 1') + ';; LONG_BIT) echo 64;; *) exit 1;; esac\n'),
            'sysctl': f'#!/bin/sh\necho {translated}\n',
            'ldd': '#!/bin/sh\necho "musl libc (x86_64)" >&2\nexit 1\n' if musl else '#!/bin/sh\necho "ldd (GNU libc) 2.39"\n',
        }
        for name, body in scripts.items():
            path = self.shim / name
            path.write_text(body)
            path.chmod(0o755)

    def run_installer(self, *args: str, env: dict | None = None, path: str | None = None) -> subprocess.CompletedProcess:
        environment = {'HOME': str(self.home), 'REPOGI_RELEASES_URL': self.url, 'SHELL': '/bin/bash',
                       'PATH': path or (f'{self.shim}:' if self.shim.exists() else '') + os.environ['PATH']}
        environment.update(env or {})
        return subprocess.run([SHELL, str(ROOT / 'install.sh'), *args], env=environment,
                              capture_output=True, text=True, timeout=120)

    def publish_fake(self, version: str, targets=None, **kwargs) -> None:
        targets = targets or contract.TARGETS
        self.release.publish(version, {contract.archive_name(version, t): fake_archive(version, t, **kwargs)
                                       for t in targets if not t.endswith('windows-msvc')})

    def installed(self) -> Path:
        return self.home / '.local' / 'bin' / 'repogi'

    def assert_failed(self, result: subprocess.CompletedProcess, message: str) -> None:
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn(message, result.stderr)

    # --- platform detection and asset selection ---------------------------------

    def test_selects_archive_for_each_supported_platform(self):
        self.publish_fake('0.1.0')
        cases = [('Linux', 'x86_64', '0', 'x86_64-unknown-linux-gnu'), ('Linux', 'amd64', '0', 'x86_64-unknown-linux-gnu'),
                 ('Linux', 'aarch64', '0', 'aarch64-unknown-linux-gnu'), ('Linux', 'arm64', '0', 'aarch64-unknown-linux-gnu'),
                 ('Darwin', 'arm64', '0', 'aarch64-apple-darwin'), ('Darwin', 'x86_64', '0', 'x86_64-apple-darwin'),
                 ('Darwin', 'x86_64', '1', 'aarch64-apple-darwin')]
        for system, machine, translated, target in cases:
            with self.subTest(system=system, machine=machine, rosetta=translated):
                self.fake_platform(system, machine, translated=translated)
                self.release.requests.clear()
                result = self.run_installer('--version', '0.1.0')
                self.assertEqual(result.returncode, 0, result.stderr)
                wanted = f'/releases/download/v0.1.0/{contract.archive_name("0.1.0", target)}'
                self.assertIn(wanted, self.release.requests)
                self.assertEqual([r for r in self.release.requests if r.endswith('.tar.gz')], [wanted])

    def test_rejects_unsupported_platforms_with_guidance(self):
        self.publish_fake('0.1.0')
        cases = [('FreeBSD', 'amd64', 'unsupported operating system: FreeBSD'),
                 ('Linux', 'riscv64', 'unsupported Linux CPU architecture: riscv64'),
                 ('Linux', 'i686', 'unsupported Linux CPU architecture: i686'),
                 ('Darwin', 'ppc', 'unsupported macOS CPU architecture: ppc'),
                 ('MINGW64_NT-10.0', 'x86_64', 'use install.ps1')]
        for system, machine, message in cases:
            with self.subTest(system=system, machine=machine):
                self.fake_platform(system, machine)
                result = self.run_installer('--version', '0.1.0')
                self.assert_failed(result, message)
                self.assertFalse(self.installed().exists())
                self.assertFalse(any(r.endswith('.tar.gz') for r in self.release.requests))

    def test_rejects_old_glibc_and_musl(self):
        self.publish_fake('0.1.0')
        self.fake_platform('Linux', 'x86_64', glibc='glibc 2.31')
        self.assert_failed(self.run_installer('--version', '0.1.0'), 'glibc 2.31 is older than 2.34')
        self.fake_platform('Linux', 'x86_64', glibc=None, musl=True)
        self.assert_failed(self.run_installer('--version', '0.1.0'), 'musl-based Linux')
        self.fake_platform('Linux', 'x86_64', glibc='glibc 2.34')
        self.assertEqual(self.run_installer('--version', '0.1.0').returncode, 0)

    # --- version resolution ------------------------------------------------------

    def test_latest_stable_and_pinned_preview(self):
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0')
        self.publish_fake('0.2.0-preview.1')
        self.release.latest = '0.1.0'
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('latest stable release: 0.1.0', result.stderr)
        self.assertEqual(subprocess.run([str(self.installed()), '--version'], capture_output=True, text=True).stdout.strip(), 'repogi 0.1.0')
        for pin in ('0.2.0-preview.1', 'v0.2.0-preview.1'):
            result = self.run_installer('--version', pin)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('repogi 0.2.0-preview.1', subprocess.run([str(self.installed()), '--version'], capture_output=True, text=True).stdout)
        result = self.run_installer(env={'REPOGI_VERSION': '0.1.0'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('latest stable', result.stderr)

    def test_no_stable_release_yet(self):
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0-preview.1')
        self.assert_failed(self.run_installer(), 'no stable Repogi release is published yet')
        self.assertFalse(self.installed().exists())

    def test_rejects_invalid_versions_and_insecure_sources(self):
        self.fake_platform('Linux', 'x86_64')
        for bad in ('latest', '1.2', '../0.1.0', '0.1.0/../../x'):
            with self.subTest(version=bad):
                self.assert_failed(self.run_installer('--version', bad), 'invalid version')
        for url in ('http://example.com/releases', 'ftp://127.0.0.1/releases', 'file:///tmp/releases'):
            with self.subTest(url=url):
                self.assert_failed(self.run_installer('--version', '0.1.0', env={'REPOGI_RELEASES_URL': url}),
                                   'must be an https URL')

    # --- verification and fail-closed behaviour -----------------------------------

    def preinstall(self) -> bytes:
        self.installed().parent.mkdir(parents=True)
        old = b'#!/bin/sh\necho "repogi 0.0.9"\n'
        self.installed().write_bytes(old)
        self.installed().chmod(0o755)
        (self.installed().parent / 'unrelated-tool').write_text('keep me\n')
        return old

    def test_checksum_mismatch_keeps_existing_install(self):
        self.fake_platform('Linux', 'x86_64')
        old = self.preinstall()
        before = self.snapshot(self.home)
        name = contract.archive_name('0.1.0', 'x86_64-unknown-linux-gnu')
        self.release.publish('0.1.0', {name: fake_archive('0.1.0', 'x86_64-unknown-linux-gnu')}, checksums={name: '0' * 64})
        self.assert_failed(self.run_installer('--version', '0.1.0'), 'checksum mismatch')
        self.assertEqual(self.installed().read_bytes(), old)
        self.assertEqual(self.snapshot(self.home), before)

    def test_missing_or_malformed_checksum_entry(self):
        self.fake_platform('Linux', 'x86_64')
        name = contract.archive_name('0.1.0', 'x86_64-unknown-linux-gnu')
        archive = fake_archive('0.1.0', 'x86_64-unknown-linux-gnu')
        for sums in ({}, {'other.tar.gz': hashlib.sha256(archive).hexdigest()}, {name: 'not-a-digest'}):
            with self.subTest(sums=sums):
                self.release.publish('0.1.0', {name: archive}, checksums=sums)
                self.assert_failed(self.run_installer('--version', '0.1.0'), 'SHA256SUMS has no valid entry')
                self.assertFalse(self.installed().exists())

    def test_http_errors_fail_closed_and_roll_back(self):
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0', targets=['aarch64-unknown-linux-gnu'])
        install_dir = self.tmp / 'new dir' / 'nested' / 'bin'
        result = self.run_installer('--version', '0.1.0', '--install-dir', str(install_dir))
        self.assert_failed(result, 'could not download repogi-0.1.0-x86_64-unknown-linux-gnu.tar.gz')
        self.assertFalse((self.tmp / 'new dir').exists())
        self.assert_failed(self.run_installer('--version', '9.9.9'), 'could not download SHA256SUMS')
        self.publish_fake('0.1.0')
        old = self.preinstall()
        for suffix, code in (('.tar.gz', 500), ('SHA256SUMS', 403)):
            with self.subTest(fail=suffix):
                self.release.fail = {suffix: code}
                self.assertNotEqual(self.run_installer('--version', '0.1.0').returncode, 0)
                self.assertEqual(self.installed().read_bytes(), old)
                self.assertEqual(sorted(p.name for p in self.installed().parent.iterdir()), ['repogi', 'unrelated-tool'])

    def test_rejects_unsafe_archives(self):
        self.fake_platform('Linux', 'x86_64')
        stem = contract.stem('0.1.0', 'x86_64-unknown-linux-gnu')
        good = b'#!/bin/sh\necho "repogi 0.1.0"\n'
        cases = {
            'traversal': [(f'{stem}/repogi', good, 0o755), (f'{stem}/../../escaped', b'x', 0o644)],
            'absolute': [(f'{stem}/repogi', good, 0o755), ('/tmp/repogi-absolute-escape', b'x', 0o644)],
            'foreign top level': [(f'{stem}/repogi', good, 0o755), ('other/repogi', good, 0o755)],
            'symlink executable': [(stem, None, 0), (f'{stem}/repogi', 'symlink', '/bin/sh')],
            'symlink member': [(f'{stem}/repogi', good, 0o755), (f'{stem}/link', 'symlink', '/etc/passwd')],
            'missing executable': [(stem, None, 0), (f'{stem}/LICENSE', b'MIT', 0o644)],
        }
        for label, entries in cases.items():
            with self.subTest(case=label):
                name = contract.archive_name('0.1.0', 'x86_64-unknown-linux-gnu')
                self.release.publish('0.1.0', {name: tar_bytes(entries)})
                result = self.run_installer('--version', '0.1.0')
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertFalse(self.installed().exists())
                self.assertFalse((self.tmp / 'escaped').exists())
                self.assertFalse(Path('/tmp/repogi-absolute-escape').exists())

    def test_rejects_binary_reporting_wrong_version(self):
        self.fake_platform('Linux', 'x86_64')
        old = self.preinstall()
        self.publish_fake('0.1.0', reported='repogi 6.6.6')
        self.assert_failed(self.run_installer('--version', '0.1.0'), "reports 'repogi 6.6.6'")
        self.assertEqual(self.installed().read_bytes(), old)

    # --- install location, repeat installs, PATH -------------------------------

    def test_custom_dir_with_spaces_repeat_install_and_path_hint(self):
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0')
        self.publish_fake('0.1.1')
        install_dir = self.tmp / 'my tools' / 'bin dir'
        for version in ('0.1.0', '0.1.0', '0.1.1', '0.1.0'):
            result = self.run_installer('--version', version, '--install-dir', str(install_dir))
            self.assertEqual(result.returncode, 0, result.stderr)
            exe = install_dir / 'repogi'
            self.assertEqual(stat.S_IMODE(exe.stat().st_mode), 0o755)
            self.assertEqual(subprocess.run([str(exe), '--version'], capture_output=True, text=True).stdout.strip(), f'repogi {version}')
            self.assertEqual(sorted(p.name for p in install_dir.iterdir()), ['repogi'])
            self.assertIn('is not on your PATH', result.stderr)
        result = self.run_installer('--version', '0.1.0', env={'REPOGI_INSTALL_DIR': str(install_dir),
                                                               'PATH': f'{self.shim}:{install_dir}:{os.environ["PATH"]}'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('is not on your PATH', result.stderr)
        self.assertEqual(sorted(p.name for p in self.home.iterdir()), [], 'nothing written to HOME when a directory is given')

    def test_unwritable_directory_and_directory_target(self):
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0')
        locked = self.tmp / 'locked'
        locked.mkdir()
        locked.chmod(0o555)
        try:
            if os.access(locked, os.W_OK):
                self.skipTest('running with privileges that ignore directory permissions')
            self.assert_failed(self.run_installer('--version', '0.1.0', '--install-dir', str(locked)), 'is not writable')
        finally:
            locked.chmod(0o755)
        (self.tmp / 'd' / 'repogi').mkdir(parents=True)
        self.assert_failed(self.run_installer('--version', '0.1.0', '--install-dir', str(self.tmp / 'd')), 'is a directory')

    def test_wget_fallback(self):
        if shutil.which('wget') is None:
            self.skipTest('wget not installed')
        self.fake_platform('Linux', 'x86_64')
        self.publish_fake('0.1.0')
        self.release.latest = '0.1.0'
        tools = self.tmp / 'no-curl'
        tools.mkdir()
        for tool in ('sh', 'dash', 'bash', 'awk', 'sed', 'grep', 'tar', 'gzip', 'mktemp', 'rm', 'mkdir', 'cp', 'chmod',
                     'mv', 'dirname', 'rmdir', 'cat', 'sha256sum', 'shasum', 'wget', 'tr', 'head', 'tail', 'pwd'):
            found = shutil.which(tool)
            if found:
                (tools / tool).symlink_to(found)
        result = self.run_installer(path=f'{self.shim}:{tools}')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('latest stable release: 0.1.0', result.stderr)

    def test_installer_never_escalates_or_runs_downloaded_scripts(self):
        # Messages (say/die) may quote commands for the user; code lines may not run them.
        code = [line for line in (ROOT / 'install.sh').read_text().splitlines()
                if line.strip() and not line.lstrip().startswith('#') and not re.search(r'\b(say|die) ', line)]
        for pattern in (r'(^|[;&|(]\s*)sudo\b', r'\beval\b', r'\|\s*(ba|z|da)?sh\b', r'(^|\s)(source|\.)\s+\S',
                        r'>>?\s*~?/?\S*(rc|profile)\b', r'\s/etc/'):
            with self.subTest(pattern=pattern):
                self.assertEqual([line for line in code if re.search(pattern, line)], [])

    # --- real packaged build for this host ----------------------------------------

    @unittest.skipUnless(REAL_ARCHIVES and host_target(), 'set REPOGI_TEST_ARCHIVES to packaged archives')
    def test_real_archive_installs_and_runs(self):
        version, target = REAL_VERSION, host_target()
        name = contract.archive_name(version, target)
        data = (REAL_ARCHIVES / name).read_bytes()
        self.release.publish(version, {name: data})
        self.release.latest = version
        install_dir = self.tmp / 'real install' / 'bin'
        for _ in range(2):
            result = self.run_installer('--install-dir', str(install_dir))
            self.assertEqual(result.returncode, 0, result.stderr)
        exe = install_dir / 'repogi'
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            packaged = tar.extractfile(f'{contract.stem(version, target)}/repogi').read()
        self.assertEqual(exe.read_bytes(), packaged)
        self.assertEqual(stat.S_IMODE(exe.stat().st_mode), 0o755)
        smoke = subprocess.run([sys.executable, str(ROOT / 'scripts/release/smoke.py'), '--version', version, str(exe)],
                               capture_output=True, text=True, timeout=600)
        self.assertEqual(smoke.returncode, 0, smoke.stdout + smoke.stderr)
        self.assertTrue(json.loads(smoke.stdout)['passed'])


@unittest.skipUnless(WINDOWS and REAL_ARCHIVES, 'install.ps1 runs on Windows with REPOGI_TEST_ARCHIVES')
class PowerShellInstallerTests(InstallerCase):
    target = 'x86_64-pc-windows-msvc'

    def run_installer(self, *args: str, env: dict | None = None) -> subprocess.CompletedProcess:
        shell = shutil.which('pwsh') or shutil.which('powershell')
        environment = dict(os.environ, REPOGI_RELEASES_URL=self.url, LOCALAPPDATA=str(self.home))
        environment.update(env or {})
        return subprocess.run([shell, '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
                               str(ROOT / 'install.ps1'), *args], env=environment, capture_output=True, text=True, timeout=300)

    def real(self) -> tuple[str, bytes]:
        name = contract.archive_name(REAL_VERSION, self.target)
        return name, (REAL_ARCHIVES / name).read_bytes()

    def installed(self) -> Path:
        return self.home / 'Programs' / 'Repogi' / 'bin' / 'repogi.exe'

    def test_latest_pinned_repeat_and_spaces(self):
        name, data = self.real()
        self.release.publish(REAL_VERSION, {name: data})
        self.release.latest = REAL_VERSION
        for args in ((), ('-Version', REAL_VERSION), ('-Version', 'v' + REAL_VERSION)):
            result = self.run_installer(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('is not on your PATH', result.stderr)
        with zipfile.ZipFile(io.BytesIO(data)) as zf:
            self.assertEqual(self.installed().read_bytes(), zf.read(f'{contract.stem(REAL_VERSION, self.target)}/repogi.exe'))
        smoke = subprocess.run([sys.executable, str(ROOT / 'scripts/release/smoke.py'), '--version', REAL_VERSION,
                                str(self.installed())], capture_output=True, text=True, timeout=600)
        self.assertEqual(smoke.returncode, 0, smoke.stdout + smoke.stderr)
        custom = self.tmp / 'custom dir' / 'bin'
        result = self.run_installer('-Version', REAL_VERSION, '-InstallDir', str(custom))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((custom / 'repogi.exe').is_file())

    def test_no_stable_release_yet(self):
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no stable Repogi release is published yet', result.stderr)

    def test_checksum_mismatch_and_http_errors_keep_existing(self):
        name, data = self.real()
        self.installed().parent.mkdir(parents=True)
        self.installed().write_bytes(b'old')
        self.release.publish(REAL_VERSION, {name: data}, checksums={name: '0' * 64})
        result = self.run_installer('-Version', REAL_VERSION)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.release.publish(REAL_VERSION, {name: data})
        self.release.fail = {'.zip': 500}
        self.assertNotEqual(self.run_installer('-Version', REAL_VERSION).returncode, 0)
        self.assertEqual(self.installed().read_bytes(), b'old')
        self.assertEqual([p.name for p in self.installed().parent.iterdir()], ['repogi.exe'])
        fresh = self.tmp / 'fresh' / 'bin'
        self.assertNotEqual(self.run_installer('-Version', REAL_VERSION, '-InstallDir', str(fresh)).returncode, 0)
        self.assertFalse((self.tmp / 'fresh').exists())

    def test_rejects_unsafe_zip(self):
        stem = contract.stem(REAL_VERSION, self.target)
        _, data = self.real()
        with zipfile.ZipFile(io.BytesIO(data)) as zf:
            exe = zf.read(f'{stem}/repogi.exe')
        for label, extra in (('traversal', f'{stem}/../../escaped.txt'), ('foreign', 'other/repogi.exe'),
                             ('backslash', f'{stem}\\..\\escaped.txt')):
            with self.subTest(case=label):
                raw = io.BytesIO()
                with zipfile.ZipFile(raw, 'w') as zf:
                    zf.writestr(f'{stem}/repogi.exe', exe)
                    zf.writestr(extra, b'x')
                name = contract.archive_name(REAL_VERSION, self.target)
                self.release.publish(REAL_VERSION, {name: raw.getvalue()})
                result = self.run_installer('-Version', REAL_VERSION)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('unsafe or unexpected path', result.stderr)
                self.assertFalse(self.installed().exists())
                self.assertFalse((self.tmp / 'escaped.txt').exists())


if __name__ == '__main__':
    unittest.main()
