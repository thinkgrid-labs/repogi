#!/usr/bin/env python3
"""Independent schema-1 artifact consumer. No Atlas Python/Rust imports.

Use `verify FILE` without an MCP dependency, or `fetch --binary ... --root ...`.
Hashing uses the Python blake3 package or an explicitly selected stdin oracle.
"""
from __future__ import annotations

import argparse
import asyncio
import base64
import binascii
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

LIMIT = 8_000_000
ID = re.compile(r"blake3:[0-9a-f]{64}\Z")
SECTIONS = ('analysis', 'snapshots', 'changes', 'affected_code', 'verification', 'coverage', 'completeness')


class Invalid(ValueError):
    def __init__(self, code, message):
        self.code = code
        super().__init__(f'{code}: {message}')


def require(ok, code, message):
    if not ok:
        raise Invalid(code, message)


def compact(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')


def parse(data):
    def pairs(items):
        value = {}
        for key, child in items:
            require(key not in value, 'json', 'duplicate key')
            value[key] = child
        return value

    def noninteger(_):
        raise Invalid('json', 'noninteger number')

    try:
        # Decode explicitly: json.loads(bytes) otherwise accepts UTF-16/32.
        return json.loads(data.decode('utf-8') if isinstance(data, bytes) else data,
                          object_pairs_hook=pairs, parse_float=noninteger, parse_constant=noninteger)
    except (UnicodeError, ValueError, RecursionError) as error:
        raise Invalid('json', str(error)) from error


def digest(data):
    command = os.environ.get('ATLAS_BLAKE3_COMMAND')
    if command:
        result = subprocess.run([command], input=data, capture_output=True, check=True, timeout=30)
        value = result.stdout.decode('ascii').strip()
        require(bool(ID.fullmatch(value)), 'hash_provider', 'invalid oracle output')
        return value
    try:
        from blake3 import blake3
    except ImportError as error:
        raise Invalid('hash_provider', 'install blake3 or select --digest-command') from error
    return 'blake3:' + blake3(data).hexdigest()


def fields(value, names, code):
    require(type(value) is dict and set(names) <= value.keys(), code, 'missing required fields')


def integer(value, low, high):
    return type(value) is int and low <= value <= high


def identifier(value):
    return type(value) is str and bool(ID.fullmatch(value))


def descriptor(value):
    fields(value, ('kind', 'is_manifest', 'protocol', 'transport_version', 'schema_version', 'manifest_id',
                   'artifact_id', 'envelope_digest', 'digest_algorithm', 'total_bytes', 'snapshots',
                   'availability', 'retrieval'), 'descriptor')
    require(value['kind'] == 'verification_manifest_descriptor' and value['is_manifest'] is False,
            'descriptor', 'expected descriptor, not manifest')
    require(value['protocol'] == 'atlas-manifest-bytes' and type(value['transport_version']) is int
            and value['transport_version'] == 1, 'transport_version', 'unsupported transport')
    require(type(value['schema_version']) is int and value['schema_version'] == 1,
            'schema_version', 'unsupported manifest schema')
    require(all(identifier(value[k]) for k in ('manifest_id', 'artifact_id', 'envelope_digest'))
            and value['artifact_id'] == value['envelope_digest'], 'identity', 'invalid descriptor identities')
    require(value['digest_algorithm'] == 'blake3-256' and value['availability'] == 'retained',
            'descriptor', 'unsupported digest or availability')
    require(integer(value['total_bytes'], 1, LIMIT), 'length', 'artifact exceeds consumer limit')
    fields(value['snapshots'], ('base', 'head'), 'descriptor')
    require(all(type(value['snapshots'][s]) is dict for s in ('base', 'head')), 'descriptor', 'snapshot objects required')
    r = value['retrieval']
    fields(r, ('tool', 'encoding', 'offset_unit', 'max_chunk_bytes', 'advance_by', 'arguments'), 'descriptor')
    require(r['tool'] == 'atlas_impact' and r['encoding'] == 'base64' and r['offset_unit'] == 'decoded_bytes'
            and r['advance_by'] == 'next_offset' and type(r['max_chunk_bytes']) is int
            and r['max_chunk_bytes'] == 16000, 'descriptor', 'unsupported retrieval instructions')
    fields(r['arguments'], ('artifact',), 'descriptor')
    a = r['arguments']['artifact']
    fields(a, ('id', 'offset', 'length'), 'descriptor')
    require(a['id'] == value['artifact_id'] and type(a['offset']) is int and a['offset'] == 0
            and integer(a['length'], 1, 16000), 'descriptor', 'inconsistent retrieval identity/range')
    return value


def chunk(value, desc, offset, requested=16000):
    fields(value, ('kind', 'is_manifest', 'protocol', 'transport_version', 'artifact_id', 'envelope_digest',
                   'digest_algorithm', 'total_bytes', 'offset', 'next_offset', 'decoded_bytes', 'encoding',
                   'data', 'final'), 'chunk')
    require(value['kind'] == 'verification_manifest_chunk' and value['is_manifest'] is False, 'chunk', 'expected chunk')
    for key in ('protocol', 'transport_version', 'artifact_id', 'envelope_digest', 'digest_algorithm', 'total_bytes'):
        require(type(value[key]) is type(desc[key]) and value[key] == desc[key], 'chunk_metadata', key)
    require(type(value['offset']) is int and value['offset'] == offset, 'range', 'gap, duplicate, overlap or reordered chunk')
    require(value['encoding'] == 'base64' and type(value['data']) is str, 'base64', 'unsupported encoding')
    require(len(value['data']) <= 4*((16000+2)//3), 'length', 'oversized base64')
    try:
        raw = base64.b64decode(value['data'], validate=True)
    except (ValueError, binascii.Error) as error:
        raise Invalid('base64', 'invalid base64') from error
    require(base64.b64encode(raw).decode('ascii') == value['data'], 'base64', 'noncanonical padding')
    require(integer(value['decoded_bytes'], 1, min(16000, requested)) and len(raw) == value['decoded_bytes'],
            'length', 'decoded length mismatch')
    end = offset + len(raw)
    require(type(value['next_offset']) is int and value['next_offset'] == end and end <= desc['total_bytes'],
            'range', 'incorrect next offset')
    require(type(value['final']) is bool and value['final'] == (end == desc['total_bytes']), 'range', 'incorrect final flag')
    return raw


def check_sets(value, ordered=False):
    if type(value) is dict:
        for key, child in value.items():
            check_sets(child, key in ('via', 'binding_chain'))
    elif type(value) is list:
        for child in value:
            check_sets(child)
        if not ordered:
            encoded = [compact(child) for child in value]
            require(all(a < b for a, b in zip(encoded, encoded[1:])), 'canonical', 'unsorted or duplicate set')
    elif type(value) is int:
        require(-(2**63) <= value < 2**64, 'json', 'integer outside schema-1 range')


def verify(data, desc=None, expected_id=None, expected_digest=None):
    require(0 < len(data) <= LIMIT, 'length', 'empty/oversized/incomplete artifact')
    if desc is not None:
        descriptor(desc)
        require(len(data) == desc['total_bytes'], 'length', 'incomplete artifact')
        expected_id, expected_digest = desc['manifest_id'], desc['envelope_digest']
    actual_digest = digest(data)
    require(expected_digest is None or actual_digest == expected_digest, 'envelope_digest', 'exact bytes do not match')
    value = parse(data)
    fields(value, ('schema_version', 'manifest_id', *SECTIONS), 'schema')
    require(type(value['schema_version']) is int and value['schema_version'] == 1, 'schema_version', 'unsupported manifest schema')
    require(set(value) == {'schema_version', 'manifest_id', *SECTIONS}, 'schema', 'unknown top-level field')
    require(all(type(value[s]) is dict for s in SECTIONS), 'schema', 'section must be object')
    require(identifier(value['manifest_id']), 'identity', 'invalid manifest ID')
    try:
        require(compact(value)+b'\n' == data, 'canonical', 'envelope must be compact UTF-8 plus one newline')
        check_sets(value)
    except (UnicodeError, RecursionError) as error:
        raise Invalid('canonical', str(error)) from error
    payload = {k: v for k, v in value.items() if k != 'manifest_id'}
    require(digest(compact(payload)) == value['manifest_id'], 'manifest_id', 'canonical payload identity mismatch')
    require(expected_id is None or value['manifest_id'] == expected_id, 'manifest_id', 'unexpected identity')
    required = {
        'analysis': {'build': str, 'requested_base': str, 'depth': int, 'max_results': int},
        'snapshots': {'base': dict, 'head': dict, 'file_inputs_verified': bool, 'git_metadata_verified': bool},
        'changes': {'declarations': list, 'files': list, 'modules': list},
        'affected_code': {'files': list, 'relationships': list, 'binding_changes': list},
        'verification': {'confirmed': list, 'possible': list, 'unknown': list, 'gaps': list,
                         'behavioral_coverage': str, 'execution': str},
        'coverage': {'base': dict, 'head': dict, 'limitations': list, 'unsupported': list, 'warnings': list},
        'completeness': {'complete': bool, 'analysis_limited': bool, 'output_truncated': bool,
                         'output_omissions': dict, 'excluded_by_scope': dict, 'source_coverage_unknown': bool},
    }
    for section, keys in required.items():
        fields(value[section], keys, 'schema')
        for key, typ in keys.items():
            require(type(value[section][key]) is typ, 'schema', f'{section}.{key} has incorrect type')
    for side in ('base', 'head'):
        fields(value['snapshots'][side], ('kind', 'commit', 'tree', 'requested', 'source_digest',
                                         'file_manifest_digest', 'resolution_digest', 'test_inventory_digest'), 'schema')
        require(value['snapshots'][side]['kind'] in ('commit', 'worktree'), 'enum', 'unknown snapshot kind')
    if desc:
        require(desc['snapshots'] == {s: value['snapshots'][s] for s in ('base', 'head')}, 'snapshot', 'descriptor provenance mismatch')
    v, c = value['verification'], value['completeness']
    require(v['behavioral_coverage'] == 'unknown' and v['execution'] == 'not_performed', 'enum', 'unsupported coverage/execution claim')
    require(c['output_truncated'] is False and c['output_omissions'] == {} and c['excluded_by_scope'] == {},
            'incomplete', 'artifact contains output/scope omissions')
    for section, priorities in (('confirmed', ('direct', 'transitive')), ('possible', ('possible',))):
        for row in v[section]:
            fields(row, ('path', 'priority', 'linked_changes', 'reasons', 'snapshots'), 'schema')
            require(type(row['path']) is str and row['priority'] in priorities, 'enum', 'unknown candidate classification')
            if section == 'confirmed':
                fields(row, ('evidence',), 'schema')
    report = dict(valid=True, schema_version=1, manifest_id=value['manifest_id'], envelope_digest=actual_digest, total_bytes=len(data))
    return value, report


def extract(value):
    """Copy evidence with original classifications, including all uncertainty."""
    confirmed, possible, unclassified = [], [], []
    known = {'direct_call', 'direct_reference', 'direct_jsx', 're_export', 'transitive'}
    for row in value['affected_code']['files']:
        fields(row, ('path', 'categories'), 'schema')
        require(type(row['categories']) is dict, 'schema', 'categories must be object')
        (confirmed if any(k in known for k in row['categories']) else possible).append(row)
        if any(k not in known and k not in ('possible', 'unconfirmed') for k in row['categories']):
            unclassified.append(row)
    return dict(schema_version=value['schema_version'], manifest_id=value['manifest_id'],
                snapshots=value['snapshots'], changed_declarations=value['changes']['declarations'],
                confirmed_affected_files=confirmed, possible_affected_files=possible, opaque_categories=unclassified,
                affected_relationships=value['affected_code']['relationships'],
                confirmed_tests=value['verification']['confirmed'], possible_tests=value['verification']['possible'],
                unknown_tests=value['verification']['unknown'], verification_gaps=value['verification']['gaps'],
                behavioral_coverage=value['verification']['behavioral_coverage'], execution=value['verification']['execution'],
                coverage=value['coverage'], completeness=value['completeness'])


def reconstruct(desc, chunks):
    descriptor(desc)
    data = bytearray()
    for piece in chunks:
        require(len(data) < desc['total_bytes'], 'range', 'chunk after final')
        data.extend(chunk(piece, desc, len(data)))
    value, report = verify(bytes(data), desc)
    return bytes(data), value, report


async def call(client, arguments, metrics=None, retries=2):
    require(integer(retries, 0, 5), 'arguments', 'retry limit must be 0..5')
    for attempt in range(retries+1):
        started = time.perf_counter()
        result = await client.call_tool('atlas_impact', arguments)
        elapsed = (time.perf_counter()-started)*1000
        if metrics is not None:
            # Logical SDK-serialized CallToolResult; framing/request IDs are excluded.
            metrics.append(dict(arguments=arguments, request_bytes=len(compact(dict(name='atlas_impact', arguments=arguments))),
                                response_bytes=len(result.model_dump_json(by_alias=True, exclude_none=True).encode()),
                                elapsed_ms=elapsed, retry=attempt))
        require(len(result.content) == 1 and result.content[0].type == 'text', 'mcp_result', 'one text block required')
        text = result.content[0].text
        if not result.is_error:
            require(len(text.encode()) <= arguments.get('max_bytes', 8000), 'length', 'oversized response')
            return parse(text)
        match = re.match(r'error\[([a-z_]+)\]:', text)
        code = match[1] if match else 'mcp_error'
        if code != 'artifact_cache_busy' or attempt == retries:
            raise Invalid(code, text)
        await asyncio.sleep(0.05*(attempt+1))
    raise AssertionError('unreachable')


async def retrieve(client, desc, budget=8000, root=None, metrics=None, chunks=None, length=16000):
    descriptor(desc)
    require(integer(budget, 2000, 32000) and integer(length, 1, 16000), 'arguments', 'invalid budget/chunk length')
    data = bytearray()
    started = time.perf_counter()
    # Every validated reply advances >=1 byte; total length bounds the loop.
    while len(data) < desc['total_bytes']:
        args = dict(artifact=dict(id=desc['artifact_id'], offset=len(data), length=length), max_bytes=budget)
        if root is not None:
            args['root'] = root
        piece = await call(client, args, metrics)
        data.extend(chunk(piece, desc, len(data), length))
        if chunks is not None:
            chunks.append(piece)
    retrieved = time.perf_counter()
    value, report = verify(bytes(data), desc)
    report.update(retrieval_ms=(retrieved-started)*1000, verification_ms=(time.perf_counter()-retrieved)*1000)
    return bytes(data), value, report


def publish(path, data):
    """Atomically install only caller-validated bytes; refuse existing outputs."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, pending = tempfile.mkstemp(prefix='.manifest-pending-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.link(pending, path)  # atomic no-clobber publication on one filesystem
    finally:
        os.unlink(pending)


async def fetch(args):
    from mcp import Client, StdioServerParameters
    require(not args.output.resolve().is_relative_to(args.root.resolve()), 'output', 'output must be outside analysed root')
    params = StdioServerParameters(command=str(args.binary.resolve()),
        args=['mcp', '--root', str(args.root.resolve()), '--tools', 'atlas_impact'])
    async with Client(params, mode='legacy', read_timeout_seconds=600) as client:
        request = dict(manifest=True, base=args.base, max_results=args.max_results, max_bytes=args.budget)
        if args.head:
            request['head'] = args.head
        metrics = []
        desc = descriptor(await call(client, request, metrics))
        data, _, report = await retrieve(client, desc, args.budget, metrics=metrics)
    publish(args.output, data)
    report['calls'] = metrics
    report['descriptor'] = desc
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--digest-command', type=Path, help='trusted raw-stdin BLAKE3 executable; no shell')
    sub = parser.add_subparsers(dest='mode', required=True)
    v = sub.add_parser('verify')
    v.add_argument('file', type=Path)
    v.add_argument('--expected-manifest-id')
    v.add_argument('--expected-envelope-digest')
    v.add_argument('--extract', action='store_true')
    f = sub.add_parser('fetch')
    f.add_argument('--binary', type=Path, required=True)
    f.add_argument('--root', type=Path, required=True)
    f.add_argument('--base', required=True)
    f.add_argument('--head')
    f.add_argument('--max-results', type=int, default=1000)
    f.add_argument('--budget', type=int, default=8000)
    f.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.digest_command:
        os.environ['ATLAS_BLAKE3_COMMAND'] = str(args.digest_command.resolve())
    try:
        if args.mode == 'fetch':
            report = asyncio.run(fetch(args))
        else:
            with args.file.open('rb') as stream:
                data = stream.read(LIMIT+1)
            value, report = verify(data, expected_id=args.expected_manifest_id, expected_digest=args.expected_envelope_digest)
            if args.extract:
                report['evidence'] = extract(value)
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    except (Invalid, OSError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
