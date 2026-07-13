#!/usr/bin/env python3
"""Extract spell-check vocabulary from a toolchain's source tree.

Walks the given directories (or reads text from stdin with `-`), pulls out
every identifier, splits compound names (camelCase, snake_case) into their
component words — mirroring how src/spell.rs checks identifiers — and emits
a Hunspell .dic file on stdout: a count header followed by one lowercase
word per line.

Keywords are kept deliberately: they show up in comments ("continue unless
unsafe") where they are perfectly valid words.

Filters:
  * words of 2 characters or fewer are dropped
  * words made of a single repeated character (bbbbbbbb) are dropped
  * only pure-ASCII alphabetic words are kept

Usage:
  extract-dict.py DIR [DIR...] [--ext .rs --ext .rs.html] > lang.dic
  something | extract-dict.py -            # tokenize stdin instead
"""

import argparse
import os
import re
import sys

IDENT = re.compile(r"[A-Za-z][A-Za-z0-9_]*")
# camelCase / PascalCase / ALLCAPS run splitter, applied to each `_` segment.
CAMEL = re.compile(r"[A-Z]+(?![a-z])|[A-Z][a-z]+|[a-z]+")

# Directory names that add test/vendor noise rather than API vocabulary.
SKIP_DIRS = {".git", "node_modules", "__pycache__", "testdata", "vendor"}


def subwords(identifier: str):
    for segment in identifier.split("_"):
        for match in CAMEL.finditer(segment):
            yield match.group(0).lower()


def keep(word: str) -> bool:
    return (
        len(word) > 2
        and len(set(word)) > 1
        and word.isascii()
        and word.isalpha()
    )


def harvest(text: str, words: set):
    for ident in IDENT.findall(text):
        for word in subwords(ident):
            if keep(word):
                words.add(word)


def wanted(name: str, exts: list) -> bool:
    if not exts:
        return True
    return any(name.endswith(ext) for ext in exts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", help="directories to walk, or - for stdin")
    parser.add_argument(
        "--ext",
        action="append",
        default=[],
        help="only read files with this suffix (repeatable); default: all files",
    )
    args = parser.parse_args()

    words = set()
    for path in args.paths:
        if path == "-":
            harvest(sys.stdin.read(), words)
            continue
        for root, dirs, files in os.walk(path):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS and not d.startswith(".")]
            for name in files:
                if not wanted(name, args.ext):
                    continue
                full = os.path.join(root, name)
                try:
                    with open(full, encoding="utf-8", errors="ignore") as fh:
                        harvest(fh.read(), words)
                except OSError:
                    continue

    ordered = sorted(words)
    out = sys.stdout
    out.write(f"{len(ordered)}\n")
    for word in ordered:
        out.write(word + "\n")
    print(f"{len(ordered)} words", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
