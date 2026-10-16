"""Owned filesystem tests for the actual worker functions; no SSH host is needed.
Run: python3 crates/rook-tools/tests/ssh_stream_worker.py
This does not exercise SSH authentication, Linux pidfds or remote process cleanup.
"""
import ast
import base64
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest

SOURCE = Path(__file__).resolve().parents[1] / "src/environments/ssh_worker.py"
LIMITS = dict(file_bytes=1024 * 1024, total_bytes=4 * 1024 * 1024, max_files=16, max_entries=32, exclude=[])


def functions(source, limits=LIMITS):
    tree = ast.parse(source)
    # Execute the unchanged production function bodies without launching the
    # Linux command/process supervisor at module top level.
    nodes = [node for node in tree.body if isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef))]
    namespace = dict(cfg={"limits": limits, "token": "owned-test"}, limits=limits,
                     MAX_META=8192, MAX_INPUT=1024 * 1024, CHUNK=65536, MAX_FRAME=73728)
    exec(compile(ast.Module(body=nodes, type_ignores=[]), str(SOURCE), "exec"), namespace)
    return namespace


def lines(rows):
    return b"".join(json.dumps(row, separators=(",", ":")).encode() + b"\n" for row in rows)


def frames(ns, files):
    source = {"dirs": [], "files": {name: {"mode": 0o600, "data": base64.b64encode(data).decode()}
                                        for name, data in sorted(files.items())}}
    rows = [{"type": "stream", "version": 2}]
    for name, data in sorted(files.items()):
        digest = hashlib.sha256(data).hexdigest()
        rows.append(dict(type="file_start", path=name, mode=0o600, bytes=len(data), sha256=digest))
        for offset in range(0, len(data), 49152):
            rows.append(dict(type="chunk", offset=offset, data=base64.b64encode(data[offset:offset + 49152]).decode()))
        rows.append(dict(type="file_end", sha256=digest))
    rows.append(dict(type="end", hash=ns["source_hash"](source)))
    return rows


@contextlib.contextmanager
def incoming(data):
    previous = sys.stdin
    sys.stdin = types.SimpleNamespace(buffer=io.BytesIO(data))
    try:
        yield
    finally:
        sys.stdin = previous


class WorkerStreamTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="rook-worker-stream-")
        self.addCleanup(self.root.cleanup)
        self.path = Path(self.root.name)
        (self.path / "workspace").mkdir()
        self.fd = os.open(self.path, os.O_RDONLY | os.O_DIRECTORY)
        self.workspace = os.open(self.path / "workspace", os.O_RDONLY | os.O_DIRECTORY)
        self.addCleanup(os.close, self.fd)
        self.addCleanup(os.close, self.workspace)
        self.ns = functions(SOURCE.read_text())

    def clean(self):
        self.assertEqual(os.listdir(self.path), ["workspace"])
        self.assertFalse(any(name.startswith(".rook-sync-") for name in os.listdir(self.workspace)))

    def test_binary_and_empty_sources_are_staged_certified_then_published_and_cleaned(self):
        data = bytes(range(251)) * 1000
        rows = frames(self.ns, {"binary": data, "empty": b""})
        self.assertGreater(sum(row["type"] == "chunk" for row in rows), 3)
        self.assertLessEqual(max(map(len, lines(rows).splitlines())), self.ns["MAX_FRAME"])
        before = self.ns["snapshot"](self.workspace)
        with incoming(lines(rows)), self.ns["read_source"](self.fd) as after:
            self.assertFalse((self.path / "workspace/binary").exists(), "parsing must not publish")
            self.assertNotIn("data", after["files"]["binary"], "the worker retains a disk source, not a whole encoded file")
            self.ns["publish"](self.workspace, before, after)
        self.assertEqual((self.path / "workspace/binary").read_bytes(), data)
        self.assertEqual((self.path / "workspace/empty").read_bytes(), b"")
        self.clean()

    def test_incomplete_bad_offset_hash_and_legacy_frames_never_publish_partial_files(self):
        (self.path / "workspace/target").write_bytes(b"original")
        rows = frames(self.ns, {"target": b"replacement"})
        variants = [rows[:-1], [{"type": "file", "path": "target", "data": ""}]]
        wrong_offset = json.loads(json.dumps(rows)); wrong_offset[2]["offset"] = 1; variants.append(wrong_offset)
        wrong_hash = json.loads(json.dumps(rows)); wrong_hash[1]["sha256"] = "0" * 64; variants.append(wrong_hash)
        oversized = json.loads(json.dumps(rows)); oversized[1]["bytes"] = LIMITS["file_bytes"] + 1; variants.append(oversized)
        traversal = json.loads(json.dumps(rows)); traversal[1]["path"] = "../escape"; variants.append(traversal)
        for variant in variants:
            with self.subTest(first=variant[0]):
                with incoming(lines(variant)), self.assertRaises((RuntimeError, ValueError)):
                    with self.ns["read_source"](self.fd):
                        self.fail("invalid input yielded an admitted source")
                self.assertEqual((self.path / "workspace/target").read_bytes(), b"original")
                self.clean()

    def test_explicit_release_reclaims_a_killed_upload_stage_without_following_links(self):
        name = '.incoming-' + 'a' * 24
        stage = self.path / name; stage.mkdir(); (stage / '0').write_bytes(b'partial')
        self.ns['cleanup_staging'](self.fd); self.clean()
        stage.mkdir(); outside = self.path / 'workspace/original'; outside.write_bytes(b'keep')
        (stage / '0').symlink_to(outside)
        with self.assertRaises(RuntimeError): self.ns['cleanup_staging'](self.fd)
        self.assertEqual(outside.read_bytes(), b'keep')
        (stage / '0').unlink(); stage.rmdir(); self.clean()

    def test_changed_staging_and_failed_copy_preserve_existing_file_and_remove_temporary_bytes(self):
        target = self.path / "workspace/target"; target.write_bytes(b"original")
        rows = frames(self.ns, {"target": b"replacement"})
        before = self.ns["snapshot"](self.workspace)
        with incoming(lines(rows)), self.ns["read_source"](self.fd) as after:
            file = after["files"]["target"]
            child = os.open(file["_name"], os.O_WRONLY | os.O_NOFOLLOW, dir_fd=file["_fd"])
            try:
                os.write(child, b"X")
            finally:
                os.close(child)
            with self.assertRaises(RuntimeError):
                self.ns["publish"](self.workspace, before, after)
        self.assertEqual(target.read_bytes(), b"original"); self.clean()
        original = self.ns["file_chunks"]
        def failed_copy(file):
            yield b"partial"
            raise OSError(28, "injected disk-full write failure")
        self.ns["file_chunks"] = failed_copy
        try:
            with incoming(lines(rows)), self.ns["read_source"](self.fd) as after:
                with self.assertRaises(OSError):
                    self.ns["publish"](self.workspace, before, after)
        finally:
            self.ns["file_chunks"] = original
        self.assertEqual(target.read_bytes(), b"original"); self.clean()


def benchmark():
    """Compare parser auxiliary allocations; prebuilt input buffers are excluded in both arms.
    This is not an SSH latency or whole-daemon RSS benchmark.
    """
    import gc
    import subprocess
    import time
    import tracemalloc
    limits = dict(LIMITS, file_bytes=8 * 1024 * 1024, total_bytes=64 * 1024 * 1024)
    current = functions(SOURCE.read_text(), limits)
    baseline_text = subprocess.check_output(["git", "show", "ed5e4442c0388ddf0b5508cf9b8398f3f647547d:crates/rook-tools/src/environments/ssh_worker.py"], text=True)
    baseline = functions(baseline_text, limits)
    raw_file = bytes(range(256)) * 32768
    files = {f"file{n}": raw_file for n in range(8)}
    encoded = base64.b64encode(raw_file).decode()
    source = {"dirs": [], "files": {name: {"mode": 0o600, "data": encoded} for name in files}}
    expected = current["source_hash"](source)
    legacy = lines([dict(type="file", path=name, **file) for name, file in source["files"].items()] + [dict(type="end", hash=expected)])
    streamed = lines(frames(current, files))
    results = []
    with tempfile.TemporaryDirectory(prefix="rook-worker-benchmark-") as directory:
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            for name, wire in [("buffered-v1", legacy), ("staged-v2", streamed)]:
                gc.collect()
                with incoming(wire):
                    tracemalloc.start(); began = time.perf_counter()
                    if name == "buffered-v1":
                        admitted = baseline["read_source"]()
                        assert baseline["source_hash"](admitted) == expected
                        del admitted
                    else:
                        with current["read_source"](fd) as admitted:
                            assert current["source_hash"](admitted) == expected
                    elapsed = time.perf_counter() - began
                    _, peak = tracemalloc.get_traced_memory(); tracemalloc.stop()
                results.append(dict(case=name, files=8, raw_bytes=64 * 1024 * 1024, wire_bytes=len(wire), parser_peak_bytes=peak, elapsed_seconds=elapsed))
                assert not os.listdir(fd), "benchmark must remove staged bytes"
        finally:
            os.close(fd)
    print(json.dumps(dict(scope="worker parser auxiliary Python allocations; excludes prebuilt input, SSH, retained Rust snapshots and kernel buffers", results=results), indent=2))


if __name__ == "__main__":
    if sys.argv[1:] == ["--bench"]:
        benchmark()
    else:
        unittest.main()
