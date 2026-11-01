#!/usr/bin/env python3
"""JavaScript graph evaluation against an independent oracle. Executes no corpus code.

usage: javascript_graph.py BINARY CORPUS --typescript DIR [--output OUT.json]

Copies CORPUS (without .git and node_modules) to a temporary directory, indexes
it with BINARY (cold, then unchanged/warm), and measures index size and query
latency. Then the TypeScript language service (`--typescript` names an
installed `typescript` package directory; it only reads files, with `allowJs`)
is asked for the definition at:

  * every resolved Repogi edge located in a JavaScript-family file
    (precision: does TypeScript agree on the target declaration?), and
  * every unresolved or unsupported CALLS site in those files
    (recall: would TypeScript have resolved it to a declaration in the corpus?).

TypeScript is an oracle, not ground truth: disagreements are listed for review.
"""
import argparse
import hashlib
import json
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

JS = ('.js', '.jsx', '.mjs', '.cjs')
NAME_CHAIN = re.compile(r'[\w$]+(\.[\w$#]+)*')
QUERIES = ['render', 'update options', 'parse data', 'animation', 'user login', 'register']
ORACLE = r"""
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
const [root, sitesFile, tsDir] = process.argv.slice(2);
const ts = createRequire(path.join(tsDir, 'package.json'))(tsDir);
const sites = JSON.parse(fs.readFileSync(sitesFile, 'utf8'));
const exts = ['.js', '.jsx', '.mjs', '.cjs', '.ts', '.tsx', '.mts', '.cts'];
const files = [];
(function walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (['node_modules', '.git', '.code-intel', '.repogi', 'dist', 'build', 'coverage'].includes(entry.name)) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full);
    else if (exts.includes(path.extname(entry.name))) files.push(full);
  }
})(root);
const options = { allowJs: true, checkJs: false, jsx: ts.JsxEmit.Preserve, module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler, target: ts.ScriptTarget.ESNext, noEmit: true,
  skipLibCheck: true, allowImportingTsExtensions: true };
const texts = new Map();
const host = {
  getScriptFileNames: () => files, getScriptVersion: () => '1',
  getScriptSnapshot: (f) => { if (!fs.existsSync(f)) return undefined; return ts.ScriptSnapshot.fromString(fs.readFileSync(f, 'utf8')); },
  getCurrentDirectory: () => root, getCompilationSettings: () => options,
  getDefaultLibFileName: (o) => ts.getDefaultLibFilePath(o), fileExists: ts.sys.fileExists, readFile: ts.sys.readFile,
  readDirectory: ts.sys.readDirectory, directoryExists: ts.sys.directoryExists, getDirectories: ts.sys.getDirectories,
};
const service = ts.createLanguageService(host, ts.createDocumentRegistry());
const bytes = (file) => { if (!texts.has(file)) texts.set(file, fs.readFileSync(file)); return texts.get(file); };
const toUtf16 = (file, byte) => bytes(file).subarray(0, byte).toString('utf8').length;
const toByte = (file, unit) => Buffer.byteLength(fs.readFileSync(file, 'utf8').slice(0, unit), 'utf8');
const out = [];
for (const site of sites) {
  const file = path.join(root, site.path);
  let definitions = [];
  try { definitions = service.getDefinitionAtPosition(file, toUtf16(file, site.byte)) || []; } catch (e) { definitions = []; }
  out.push({ key: site.key, definitions: definitions.map((d) => {
    const inside = d.fileName.startsWith(root + path.sep);
    return inside ? { path: path.relative(root, d.fileName).split(path.sep).join('/'), byte: toByte(d.fileName, d.textSpan.start), kind: d.kind }
                  : { external: true, kind: d.kind };
  }) });
}
fs.writeFileSync(sitesFile + '.out', JSON.stringify(out));
"""


def run(binary, cwd, args, timeout=900):
    started = time.monotonic()
    p = subprocess.run([str(binary), *args], cwd=cwd, capture_output=True, timeout=timeout)
    if p.returncode != 0:
        raise SystemExit(f'{args}: {p.stderr.decode()[-500:]}')
    return p.stdout, time.monotonic() - started


def site_byte(edge):
    """The oracle position: the last identifier of a use, the first of an import."""
    loc = edge['location']
    return loc['start_byte'] if edge['kind'] == 'IMPORTS' else max(loc['start_byte'], loc['end_byte'] - 1)


def parse_id(target):
    kind_path, start, end = target.rsplit(':', 2)
    return kind_path.split(':', 1)[1], int(start), int(end)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('binary', type=Path)
    parser.add_argument('corpus', type=Path)
    parser.add_argument('--typescript', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--max-review', type=int, default=40)
    args = parser.parse_args()
    corpus = args.corpus.resolve()
    commit = subprocess.run(['git', '-C', str(corpus), 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / 'corpus'
        shutil.copytree(corpus, root, ignore=shutil.ignore_patterns('.git', 'node_modules'))
        root = root.resolve()
        report_bytes, cold = run(args.binary, root, ['index', '.'])
        index_report = json.loads(report_bytes)
        _, warm = run(args.binary, root, ['index', '.'])
        latency = []
        for query in QUERIES:
            latency.append(run(args.binary, root, ['context', query, '--format', 'text'])[1])
        _, graph_s = run(args.binary, root, ['graph', '--limit', '10000'])
        # `graph` caps its output; read every semantic edge read-only instead.
        database = sqlite3.connect(f"file:{root / '.code-intel/index.db'}?mode=ro", uri=True)
        edges = [json.loads(row[0]) for row in database.execute('SELECT data FROM semantic_edges ORDER BY ordinal')]
        declarations = {}
        for (data,) in database.execute("SELECT data FROM facts"):
            fact = json.loads(data)
            if fact['kind'] not in ('file', 'import', 'export'):
                declarations.setdefault(fact['path'], []).append(fact)
        database.close()

        def indexed_declaration(definition):
            """Does the oracle point at the name of a declaration Repogi indexes?"""
            text = (root / definition['path']).read_bytes()
            for fact in declarations.get(definition['path'], []):
                name = fact['name'].rsplit('.', 1)[-1].encode()
                if fact['start_byte'] <= definition['byte'] < fact['end_byte'] and \
                        text[definition['byte']:definition['byte'] + len(name)] == name:
                    return True
            return False
        status = json.loads(run(args.binary, root, ['status'])[0])
        js_edges = [e for e in edges if e['location']['path'].endswith(JS)]
        counts = {}
        for e in js_edges:
            key = f"{e['kind']}:{e['status']}"
            counts[key] = counts.get(key, 0) + 1
        precision_sites, recall_sites = [], []
        for i, e in enumerate(js_edges):
            if e['kind'] in ('EXPORTS',):
                continue
            site = {'key': i, 'path': e['location']['path'], 'byte': site_byte(e)}
            if e['status'] == 'resolved' and e['target_id'] and not e['target_id'].startswith('file:'):
                precision_sites.append(site)
            elif e['status'] != 'resolved' and e['kind'] == 'CALLS' and NAME_CHAIN.fullmatch(e['evidence']['syntax']):
                # Only static names (`f`, `a.b`, `this.m`); a callee that is
                # itself a call or index expression has no declaration to find.
                recall_sites.append(site)
        sites_file = Path(tmp) / 'sites.json'
        oracle_file = Path(tmp) / 'oracle.mjs'
        oracle_file.write_text(ORACLE)
        sites_file.write_text(json.dumps(precision_sites + recall_sites))
        started = time.monotonic()
        subprocess.run(['node', str(oracle_file), str(root), str(sites_file), str(args.typescript.resolve())],
                       check=True, timeout=3600, cwd=tmp)
        oracle_s = time.monotonic() - started
        answers = {a['key']: a['definitions'] for a in json.loads(Path(str(sites_file) + '.out').read_text())}
        agreed, disagree, no_answer, external = set(), [], 0, 0
        for site in precision_sites:
            e = js_edges[site['key']]
            path, start, end = parse_id(e['target_id'])
            defs = answers[site['key']]
            inside = [d for d in defs if not d.get('external')]
            if not defs:
                no_answer += 1
            elif not inside:
                external += 1
                disagree.append((e, defs))
            elif any(d['path'] == path and start <= d['byte'] < end for d in inside):
                agreed.add(site['key'])
            else:
                disagree.append((e, defs))
        # Recall counts only sites where TypeScript names a declaration Repogi
        # indexes. Other answers are outside the declaration model (the local
        # alias of a package that is not in the corpus, object-literal members,
        # parameters and locals) and are reported separately.
        missed, out_of_model, oracle_unresolved = [], {}, 0
        for site in recall_sites:
            inside = [d for d in answers[site['key']] if not d.get('external')]
            if not inside:
                oracle_unresolved += 1
            elif any(indexed_declaration(d) for d in inside):
                missed.append((js_edges[site['key']], inside))
            else:
                kind = 'import_alias_of_absent_package' if all(d['kind'] == 'alias' for d in inside) else 'not_an_indexed_declaration'
                out_of_model[kind] = out_of_model.get(kind, 0) + 1
        agree = len(agreed)
        resolved_calls_agree = sum(1 for key in agreed if js_edges[key]['kind'] == 'CALLS')
        reasons = {}
        for e, _ in missed:
            reasons[e['evidence']['rule']] = reasons.get(e['evidence']['rule'], 0) + 1
        result = {
            'corpus': str(corpus), 'commit': commit,
            'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            'typescript': json.loads((args.typescript / 'package.json').read_text())['version'],
            'files': {'scanned': index_report['files_scanned'], 'skipped': index_report['files_skipped'],
                      'syntax_error_files': status['syntax_error_files'], 'documents': index_report['documents']['scanned']},
            'timing_s': {'cold_index': round(cold, 3), 'warm_index': round(warm, 3), 'graph_10k_edges': round(graph_s, 3),
                         'context_median': round(statistics.median(latency), 4), 'context_max': round(max(latency), 4),
                         'oracle': round(oracle_s, 1)},
            'index_bytes': (root / '.code-intel/index.db').stat().st_size,
            'javascript_edges': counts,
            'precision': {'checked': len(precision_sites), 'agree': agree, 'disagree': len(disagree),
                          'oracle_no_definition': no_answer, 'oracle_external_only': external,
                          'rate_where_oracle_answered': round(agree / (agree + len(disagree)), 4) if agree + len(disagree) else None},
            'recall_calls': {'unresolved_call_sites': len(recall_sites), 'missed_indexed_declarations': len(missed),
                             'oracle_target_outside_declaration_model': out_of_model,
                             'oracle_also_unresolved': oracle_unresolved, 'resolved_calls_confirmed_by_oracle': resolved_calls_agree,
                             'missed_by_rule': reasons,
                             'estimated_recall': round(resolved_calls_agree / (resolved_calls_agree + len(missed)), 4)
                             if resolved_calls_agree + len(missed) else None},
            'disagreements_for_review': [
                {'site': f"{e['location']['path']}:{e['location']['start_line']}", 'kind': e['kind'],
                 'syntax': e['evidence']['syntax'][:80], 'repogi': e['target_id'], 'typescript': defs}
                for e, defs in disagree[:args.max_review]],
            'missed_sample': [
                {'site': f"{e['location']['path']}:{e['location']['start_line']}", 'syntax': e['evidence']['syntax'][:80],
                 'rule': e['evidence']['rule'], 'typescript': defs[:2]} for e, defs in missed[:args.max_review]],
        }
    text = json.dumps(result, indent=2, ensure_ascii=False)
    if args.output:
        args.output.write_text(text + '\n')
    print(json.dumps({k: result[k] for k in ('files', 'timing_s', 'index_bytes', 'precision', 'recall_calls')}, indent=1))


if __name__ == '__main__':
    sys.exit(main())
