#!/usr/bin/env python3
"""Make tool links. Build missing felt helpers outside the source tree."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
sys.dont_write_bytecode = True
from run import TOOLS

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=os.environ.get('FELT_SRC'),
               help="felt's source tree (default: $FELT_SRC)")
p.add_argument('--felt-bin', type=Path, default=os.environ.get('FELT_DIR'),
               help='a folder with built felt tools (default: $FELT_DIR; optional)')
p.add_argument('--vtmake', type=Path, help='Use this binary for all six tool names')
p.add_argument('--output', type=Path, required=True)
p.add_argument('--build-dir', type=Path, default=Path(tempfile.gettempdir()) / 'vtmake-felt-helper-build')
a = p.parse_args()
if not a.vtmake and not a.source:
    p.error("set --source or FELT_SRC to felt's source tree")
a.output.mkdir(parents=True, exist_ok=True)
for name in TOOLS + ([] if a.vtmake else ['unit']):
    binary = a.vtmake or (a.felt_bin / name if a.felt_bin else None)
    if not a.vtmake and not (binary and binary.is_file()):
        binary = a.source / name
    if not binary.is_file() and not a.vtmake:
        if not a.build_dir.exists():
            shutil.copytree(a.source, a.build_dir, ignore=shutil.ignore_patterns('.git', 'tests'))
        subprocess.run(['make', '-C', str(a.build_dir), '-j30', name], check=True)
        binary = a.build_dir / name
    if not binary.is_file():
        p.error(f'Missing binary: {binary}')
    dest = a.output / name
    if dest.is_symlink():
        dest.unlink()
    dest.symlink_to(binary.resolve())
