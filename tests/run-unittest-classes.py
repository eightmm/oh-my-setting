#!/usr/bin/env python3
"""Run the unittest modules matching a pattern one TestCase class per process.

One slow class held a whole suite serial: test_oms_graph_runtime.py alone took
93s of graph-smoke's 161s (2026-10-01). Classes are independent fixtures, so
they run --jobs at a time; output and exit status follow unittest's.
"""
import argparse
import concurrent.futures
import os
import subprocess
import sys
import unittest


def classes(start, pattern):
    def walk(suite):
        for item in suite:
            if isinstance(item, unittest.TestSuite):
                yield from walk(item)
            else:
                yield item
    names = []
    for test in walk(unittest.defaultTestLoader.discover(start, pattern=pattern)):
        name = "%s.%s" % (type(test).__module__, type(test).__qualname__)
        if name.startswith("unittest.loader."):
            # A module that failed to import: run it whole to report the error.
            name = test.id().rsplit(".", 1)[-1]
        if name not in names:
            names.append(name)
    return names


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("-s", "--start", required=True)
    parser.add_argument("-p", "--pattern", required=True)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    sys.path.insert(0, args.start)
    names = classes(args.start, args.pattern)
    if not names:
        print("error: no tests match %s" % args.pattern, file=sys.stderr)
        return 1

    def run(name):
        proc = subprocess.run([sys.executable, "-m", "unittest", "-v", name], cwd=args.start,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        return name, proc.returncode, proc.stdout

    failed = []
    with concurrent.futures.ThreadPoolExecutor(max(1, args.jobs)) as pool:
        for name, code, output in pool.map(run, names):
            sys.stdout.write(output)
            if code:
                failed.append(name)
    print("unittest classes: %d run, %d failed%s" % (
        len(names), len(failed), (": " + ", ".join(failed)) if failed else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
