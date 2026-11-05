# Run felt's tests

Use Python 3.10 or later, GNU Make, Bash, Perl, and the usual POSIX tools.
Use Rust 1.99 to build vtmake. Run from the repository root.
The felt source tree must be at commit `4f26211`.

```sh
export FELT_SRC=/path/to/felt/tippecanoe  # felt's source tree, at 4f26211
export FELT_DIR=/path/to/felt/bin         # built felt tools (optional)
WORK=${TMPDIR:-/tmp}/vtmake-felt-tests    # outside the repository

source ~/.cargo/env
taskset -c 2-31 cargo build --release -p vtmake

python3 bench/felt-tests/prepare-tools.py \
  --output "$WORK/felt-tools"
python3 bench/felt-tests/prepare-tools.py \
  --vtmake "$PWD/target/release/vtmake" \
  --output "$WORK/vtmake-tools"

taskset -c 2-31 python3 bench/felt-tests/run.py \
  --tools "$WORK/felt-tools" \
  --work "$WORK/felt-2" --threads 2
taskset -c 2-31 python3 bench/felt-tests/run.py \
  --tools "$WORK/felt-tools" \
  --work "$WORK/felt-1" --threads 1
taskset -c 2-31 python3 bench/felt-tests/run.py \
  --tools "$WORK/vtmake-tools" \
  --work "$WORK/vtmake-2" --threads 2

python3 bench/felt-tests/report.py \
  --felt-one "$WORK/felt-1" \
  --felt-two "$WORK/felt-2" \
  --vtmake "$WORK/vtmake-2" \
  --output bench/results/felt-tests-baseline.md
```

Use a new `--work` path for each run. A run returns 1 if any case fails.
This is expected for the present baseline. Use `--match TEXT` to select cases.
Use `--jobs` to change the number of workers (default: 30).
Use `--timeout` to change the time limit per case (default: 180 seconds).
Use `--source` to give the felt source path (default: `$FELT_SRC`).

`prepare-tools.py` links all six names to vtmake when `--vtmake` is set.
It does not replace missing vtmake tools with felt tools. For felt, it first
uses the binary directory (`--felt-bin`, default `$FELT_DIR`), then existing
source build products.
If a helper is missing, it copies the source without `.git` or `tests` to the
external build directory (`--build-dir`, default in the system temporary
folder) and builds the helper there. It also links felt's
C++ `unit` binary. vtmake has no such binary, so its `unit` case is an ERROR.

The runner reads felt's Makefile at run time. It keeps the test variables,
recipes, shell exit rules, filters, and byte comparisons. It removes build
rules and the rule that changes expected files. It adds one target to run
`./unit`. For each case, it links tracked test files into an empty work
directory. This keeps relative file names in metadata the same as felt's.
It removes a link before a recipe writes to that path. It never copies test
data. It sets `LC_ALL=C` and `TIPPECANOE_MAX_THREADS` for each case.

Each expected file in `tests/*/out/*.json` is one case. Each other prerequisite
of `make test` is one case. Tests inside a special target run in their original
order and stop at the first failed command. A PASS applies to the whole target.
`geobuf-test` is not part of `make test` and is not included.

Each case has a time limit. On timeout, the runner kills the process group,
including recursive Make processes and filter tools. Output stays in the
external work directory:

- `results.tsv`: one line per case, with ID, target, commands, status, exit code,
  and a short reason.
- `results.jsonl`: the same data, plus time and comparison hashes.
- `metadata.json`: source revision, Makefile hash, tool paths and hashes,
  thread count, worker count, and time limit.
- `cases/`: the command log, full output log, and remaining files for each case.

FAIL means a command or comparison returned a nonzero status. ERROR means a
timeout, a missing executable, a command that cannot execute, or a runner shell
error. The exit code is the first failed recipe's code. A timeout uses 124.
A pipeline uses Bash's normal last-command exit rule, as in felt's Makefile.
The `cmp` wrapper records hashes, then executes `/usr/bin/cmp` with the same
arguments. No JSON normalization or extra filter is used.

For each merge, run the vtmake command above with two threads. Compare its
results with the last accepted baseline. Keep felt's known failures separate
from vtmake failures. Run felt at one and two threads again when the reference
source changes. The upstream parallel test uses random input. Its hashes cannot
show thread dependence across runs.
