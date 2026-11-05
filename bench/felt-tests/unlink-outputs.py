"""Protect the source tree. Unlink an input link if a recipe writes that path."""
import pathlib
import shlex
import sys
if sys.argv[1].lstrip().startswith('#'):
    sys.exit(0)
lexer = shlex.shlex(sys.argv[1], posix=True, punctuation_chars='<>|;&()')
lexer.whitespace_split = True
lexer.commenters = ''
tokens = list(lexer)
for i, token in enumerate(tokens[:-1]):
    if token in ('>', '>>', '-o', '-e'):
        path = pathlib.Path(tokens[i + 1])
        if path.is_symlink() and not path.is_absolute():
            path.unlink()
