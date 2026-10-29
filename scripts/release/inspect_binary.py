#!/usr/bin/env python3
"""Check that a release executable matches its target triple.

Parses ELF, Mach-O and PE headers with the standard library only, so the same
check runs on every release runner. Prints a JSON report; exits non-zero when
the format, architecture, dynamic dependencies or glibc floor are wrong.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import struct
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from contract import GLIBC_FLOOR, TARGETS  # noqa: E402

ELF_MACHINE = {'x86_64': 62, 'aarch64': 183}
MACHO_CPU = {'x86_64': 0x01000007, 'aarch64': 0x0100000C}
PE_MACHINE = {'x86_64': 0x8664, 'aarch64': 0xAA64}

# Shared libraries a glibc Rust binary may need; anything else means a
# system library leaked in (for example a non-bundled SQLite).
LINUX_LIBS = re.compile(r'^(libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|ld-linux-[a-z0-9-]+\.so\.\d)$')
MACOS_LIBS = re.compile(r'^(/usr/lib/libSystem\.B\.dylib|/usr/lib/libiconv\.2\.dylib|/usr/lib/libresolv\.9\.dylib|/System/Library/Frameworks/[A-Za-z]+\.framework/.+)$')
WINDOWS_CRT = re.compile(r'^(vcruntime\d+.*|msvcp\d+.*|api-ms-win-crt-.*|ucrtbase.*)\.dll$', re.I)


def cstr(data: bytes, offset: int) -> str:
    end = data.index(b'\0', offset)
    return data[offset:end].decode('utf-8', 'replace')


def elf(data: bytes) -> dict:
    if data[4] != 2:
        raise ValueError('not a 64-bit ELF')
    end = '<' if data[5] == 1 else '>'
    machine = struct.unpack_from(end + 'H', data, 18)[0]
    phoff, = struct.unpack_from(end + 'Q', data, 32)
    phentsize, phnum = struct.unpack_from(end + 'HH', data, 54)
    loads, interp, dynamic = [], None, None
    for i in range(phnum):
        p_type, _, p_offset, p_vaddr, _, p_filesz, _, _ = struct.unpack_from(end + 'IIQQQQQQ', data, phoff + i * phentsize)
        if p_type == 1:
            loads.append((p_vaddr, p_offset, p_filesz))
        elif p_type == 3:
            interp = cstr(data, p_offset)
        elif p_type == 2:
            dynamic = (p_offset, p_filesz)

    def file_offset(addr: int) -> int:
        for vaddr, offset, size in loads:
            if vaddr <= addr < vaddr + size:
                return offset + addr - vaddr
        raise ValueError(f'address {addr:#x} outside loaded segments')

    needed, entries = [], {}
    if dynamic:
        for pos in range(dynamic[0], dynamic[0] + dynamic[1], 16):
            tag, val = struct.unpack_from(end + 'qQ', data, pos)
            if tag == 0:
                break
            if tag == 1:
                needed.append(val)
            else:
                entries.setdefault(tag, val)
    strtab = file_offset(entries[5]) if 5 in entries else None
    libs = [cstr(data, strtab + n) for n in needed] if strtab is not None else []
    versions: dict[str, list[str]] = {}
    if strtab is not None and 0x6ffffffe in entries:
        pos = file_offset(entries[0x6ffffffe])
        for _ in range(entries.get(0x6fffffff, 0)):
            _, cnt, file_name, aux, nxt = struct.unpack_from(end + 'HHIII', data, pos)
            lib = cstr(data, strtab + file_name)
            apos = pos + aux
            for _ in range(cnt):
                _, _, _, name, anext = struct.unpack_from(end + 'IHHII', data, apos)
                versions.setdefault(lib, []).append(cstr(data, strtab + name))
                apos += anext
            pos += nxt
    glibc = sorted({tuple(int(x) for x in v[6:].split('.')) for vs in versions.values()
                    for v in vs if re.fullmatch(r'GLIBC_\d+(\.\d+)+', v)})
    return {'format': 'elf', 'machine': machine, 'interpreter': interp, 'needed': libs,
            'glibc_max': '.'.join(map(str, glibc[-1])) if glibc else None,
            'version_needs': {k: sorted(v) for k, v in versions.items()}}


def macho(data: bytes) -> dict:
    if data[:4] != b'\xcf\xfa\xed\xfe':
        raise ValueError('not a thin little-endian 64-bit Mach-O')
    cpu, _, filetype, ncmds = struct.unpack_from('<iiII', data, 4)
    pos, dylibs, minos = 32, [], None
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', data, pos)
        if cmd in (0xC, 0x80000018):  # LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB
            dylibs.append(cstr(data, pos + struct.unpack_from('<I', data, pos + 8)[0]))
        elif cmd == 0x32:  # LC_BUILD_VERSION
            v = struct.unpack_from('<I', data, pos + 12)[0]
            minos = f'{v >> 16}.{(v >> 8) & 0xff}.{v & 0xff}'
        pos += size
    return {'format': 'macho', 'machine': cpu & 0xffffffff, 'filetype': filetype,
            'needed': dylibs, 'macos_min': minos}


def pe(data: bytes) -> dict:
    lfanew, = struct.unpack_from('<I', data, 0x3C)
    if data[lfanew:lfanew + 4] != b'PE\0\0':
        raise ValueError('missing PE signature')
    machine, nsections = struct.unpack_from('<HH', data, lfanew + 4)
    opt_size, = struct.unpack_from('<H', data, lfanew + 20)
    opt = lfanew + 24
    if struct.unpack_from('<H', data, opt)[0] != 0x20B:
        raise ValueError('not a PE32+ executable')
    import_rva, _ = struct.unpack_from('<II', data, opt + 112 + 8)
    sections = []
    for i in range(nsections):
        base = opt + opt_size + i * 40
        vsize, vaddr, rawsize, rawptr = struct.unpack_from('<IIII', data, base + 8)
        sections.append((vaddr, max(vsize, rawsize), rawptr))

    def file_offset(rva: int) -> int:
        for vaddr, size, rawptr in sections:
            if vaddr <= rva < vaddr + size:
                return rawptr + rva - vaddr
        raise ValueError(f'RVA {rva:#x} outside sections')

    dlls, pos = [], file_offset(import_rva) if import_rva else None
    while pos is not None:
        fields = struct.unpack_from('<IIIII', data, pos)
        if not any(fields):
            break
        dlls.append(cstr(data, file_offset(fields[3])))
        pos += 20
    return {'format': 'pe', 'machine': machine, 'needed': dlls}


def inspect(path: Path, target: str) -> dict:
    spec = TARGETS[target]
    data = path.read_bytes()
    if data[:4] == b'\x7fELF':
        report = elf(data)
    elif data[:2] == b'MZ':
        report = pe(data)
    else:
        report = macho(data)
    expected_format = {'linux': 'elf', 'macos': 'macho', 'windows': 'pe'}[spec['os']]
    expected_machine = {'elf': ELF_MACHINE, 'macho': MACHO_CPU, 'pe': PE_MACHINE}[expected_format][spec['arch']]
    problems = []
    if report['format'] != expected_format:
        problems.append(f'format {report["format"]}, expected {expected_format}')
    elif report['machine'] != expected_machine:
        problems.append(f'machine {report["machine"]:#x}, expected {expected_machine:#x}')
    if report['format'] == 'elf':
        bad = [lib for lib in report['needed'] if not LINUX_LIBS.match(lib)]
        if bad:
            problems.append(f'unexpected shared libraries: {bad}')
        if not report['interpreter'] or 'ld-linux' not in report['interpreter']:
            problems.append(f'not a glibc dynamic executable (interpreter {report["interpreter"]})')
        floor = '.'.join(map(str, GLIBC_FLOOR))
        if report['glibc_max'] is None:
            problems.append('no glibc version requirements found')
        elif tuple(map(int, report['glibc_max'].split('.'))) > GLIBC_FLOOR:
            problems.append(f'requires glibc {report["glibc_max"]}, above the published floor {floor}')
        report['glibc_floor'] = floor
    elif report['format'] == 'macho':
        bad = [lib for lib in report['needed'] if not MACOS_LIBS.match(lib)]
        if bad:
            problems.append(f'unexpected dynamic libraries: {bad}')
        if report['filetype'] != 2:
            problems.append('not an MH_EXECUTE image')
    elif report['format'] == 'pe':
        crt = [dll for dll in report['needed'] if WINDOWS_CRT.match(dll)]
        if crt:
            problems.append(f'dynamic C runtime imports (build with +crt-static): {crt}')
    report.update(target=target, path=str(path), problems=problems)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    report = inspect(args.binary, args.target)
    print(json.dumps(report, indent=2))
    if report['problems']:
        sys.exit('binary check failed: ' + '; '.join(report['problems']))


if __name__ == '__main__':
    main()
