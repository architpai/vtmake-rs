#!/usr/bin/env python3
"""Run felt's Makefile tests in isolated directories. Use Python 3.10 or later."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

TOOLS = 'tippecanoe tile-join tippecanoe-decode tippecanoe-enumerate tippecanoe-overzoom tippecanoe-json-tool'.split()
HERE = Path(__file__).resolve().parent


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--source', type=Path, default=os.environ.get('FELT_SRC'),
                   help="felt's source tree (default: $FELT_SRC)")
    p.add_argument('--tools', type=Path, required=True)
    p.add_argument('--work', type=Path, required=True, help='New directory for this run')
    p.add_argument('--threads', type=int, default=2)
    p.add_argument('--jobs', type=int, default=30)
    p.add_argument('--timeout', type=int, default=180, help='Seconds per case')
    p.add_argument('--match', default='', help='Run IDs that contain this text')
    a = p.parse_args()
    if not a.source:
        p.error("set --source or FELT_SRC to felt's source tree")
    a.source, a.tools, a.work = [v.resolve() for v in (a.source, a.tools, a.work)]
    if min(a.threads, a.jobs, a.timeout) < 1:
        p.error('--threads, --jobs, and --timeout must be positive')
    if a.work.exists():
        p.error('--work must be a new directory')
    for tool in TOOLS:
        if not (a.tools / tool).is_file():
            p.error(f'Missing tool: {a.tools / tool}')
    # Only the test part is needed. Keep all variable expansions and recipes.
    text = (a.source / 'Makefile').read_text()
    test_part = text[text.index('TESTS = '):text.index('# Use this target to regenerate')]
    makefile = ('SHELL := ' + str(HERE / 'shell.sh') + '\nADVSHELL := $(SHELL)\n'
                + test_part + '\n.PHONY: unit run-case\nunit:\n\t./unit\nrun-case: $(CASE)\n')
    tracked = subprocess.check_output(['git', '-C', str(a.source), 'ls-files', '-z', 'tests']).decode().split('\0')
    tracked = [Path(s) for s in tracked if s and (a.source / s).is_file()]
    expected = sorted(s for s in tracked if len(s.parts) == 4 and s.parts[2] == 'out' and s.suffix == '.json')
    # Use the actual prerequisite list, rather than a second target list.
    prereqs = re.search(r'^test: (.*)$', test_part, re.M).group(1)
    special = prereqs[prereqs.index(' raw-tiles-test') + 1:].split()
    cases = [(str(s) + '.check', 'json.check') for s in expected]
    cases += [(s, s) for s in special]
    cases = [c for c in cases if a.match in c[0]]
    a.work.mkdir(parents=True)
    (a.work / 'metadata.json').write_text(json.dumps({
        'source': str(a.source), 'source_head': subprocess.check_output(['git', '-C', str(a.source), 'rev-parse', 'HEAD']).decode().strip(),
        'makefile_sha256': hashlib.sha256(text.encode()).hexdigest(),
        'tools': {t: {'path': str((a.tools / t).resolve()), 'sha256': hashlib.sha256((a.tools / t).read_bytes()).hexdigest()} for t in TOOLS},
        'threads': a.threads, 'jobs': a.jobs, 'timeout': a.timeout,
    }, indent=2) + '\n')

    def run(item):
        case, target = item
        start = time.monotonic()
        d = a.work / 'cases' / case
        d.mkdir(parents=True)
        for s in tracked:
            dest = d / s
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.symlink_to(a.source / s)
        # Some targets use directories that contain no tracked files.
        for name in ('allow-existing', 'readfile', 'parallel', 'radix-sort', 'pmtiles'):
            (d / 'tests' / name).mkdir(parents=True, exist_ok=True)
        for tool in TOOLS:
            (d / tool).symlink_to(a.tools / tool)
        # unit is a C++ internal test, not a public tool. Never use felt's unit
        # binary as a substitute when the selected implementation has none.
        if (a.tools / 'unit').exists():
            (d / 'unit').symlink_to(a.tools / 'unit')
        (d / 'Makefile').write_text(makefile)
        env = dict(os.environ, TIPPECANOE_MAX_THREADS=str(a.threads), LC_ALL='C',
                   FELT_CASE=str(d), PATH=str(HERE / 'bin') + ':' + os.environ['PATH'])
        env.pop('MAKEFLAGS', None)
        env.pop('MFLAGS', None)
        with (d / 'output.log').open('wb') as log:
            proc = subprocess.Popen(['make', '--no-print-directory', '-rR', 'CASE=' + case, 'run-case'], cwd=d, env=env,
                                    stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                rc = proc.wait(timeout=a.timeout)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
                rc = 124
        commands = (d / 'commands').read_text().split('\0')[:-1] if (d / 'commands').exists() else []
        failure = (d / 'failure').read_text().strip() if (d / 'failure').exists() else ''
        status = 'PASS' if rc == 0 else 'FAIL'
        reason = ''
        if rc:
            status = 'ERROR' if rc == 124 or not failure else 'FAIL'
            output = (d / 'output.log').read_text(errors='replace').splitlines()
            reason = 'timeout' if rc == 124 else ' | '.join(output[-3:])[:400]
        if failure and int(failure.split()[0]) in (125, 126, 127):
            status = 'ERROR'
        checks = [json.loads(s) for s in (d / 'checks.jsonl').read_text().splitlines()] if (d / 'checks.jsonl').exists() else []
        return dict(id=case, target=target, command='\n'.join(commands), status=status,
                    exit_code=int(failure.split()[0]) if failure else rc, reason=reason,
                    seconds=round(time.monotonic() - start, 3), checks=checks,
                    fixed_threads=any('TIPPECANOE_MAX_THREADS=' in c for c in commands))

    start = time.monotonic()
    results = []
    with (a.work / 'results.jsonl').open('w') as js, (a.work / 'results.tsv').open('w') as tsv:
        tsv.write('id\ttarget\tcommand\tstatus\texit_code\treason\n')
        with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
            for r in pool.map(run, cases):
                results.append(r)
                js.write(json.dumps(r) + '\n'); js.flush()
                fields = [r[k] for k in ('id', 'target', 'command', 'status', 'exit_code', 'reason')]
                tsv.write('\t'.join(str(v).replace('\t', ' ').replace('\n', ' ; ') for v in fields) + '\n'); tsv.flush()
    summary = dict(passed=sum(r['status'] == 'PASS' for r in results), total=len(results),
                   seconds=round(time.monotonic() - start, 2))
    (a.work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary))
    return 0 if summary['passed'] == summary['total'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
