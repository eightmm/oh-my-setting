import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("provider_latest", ROOT / "scripts/lib/provider-latest.py")
LATEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LATEST)


class ProviderLatestTests(unittest.TestCase):
    def setUp(self):
        self.lock = LATEST.lock_tools.load(ROOT / "tools.lock.json")

    def metadata(self, url):
        if "/manifests/" in url:
            platform = url.rsplit("/", 1)[1][:-5].replace("_", "-")
            row = copy.deepcopy(self.lock["antigravity"]["platforms"][platform])
            row.pop("archive")
            row["version"] = self.lock["antigravity"]["version"]
            return row
        package, release = LATEST.urllib.parse.unquote(url.removeprefix("https://registry.npmjs.org/")).rsplit("/", 1)
        for row in self.lock["npm"].values():
            for item in [row, *row.get("native", {}).values()]:
                if item["package"] == package and release in {"latest", item["version"]}:
                    version = item["version"]
                    return {"name": package, "version": version, "dist": {
                        "integrity": item["integrity"],
                        "tarball": "https://registry.npmjs.org/%s/-/%s-%s.tgz" % (package, package.rsplit("/", 1)[-1], version)}}
        self.fail("unexpected metadata URL: " + url)

    def test_valid_snapshot_and_selective_resolution_preserve_bootstrap(self):
        for providers in (["codex", "claude", "agy"], ["codex"]):
            with self.subTest(providers=providers), patch.object(LATEST, "fetch_json", side_effect=self.metadata) as fetch:
                result = LATEST.resolve(self.lock, providers)
                self.assertEqual(result, self.lock)
                self.assertIsNot(result, self.lock)
                self.assertEqual(fetch.call_count, 22 if len(providers) == 3 else 7)
        def newer(url):
            row = self.metadata(url.replace("99.1.0", self.lock["npm"]["codex"]["version"]))
            old = self.lock["npm"]["codex"]["version"]
            row["version"] = row["version"].replace(old, "99.1.0")
            row["dist"]["tarball"] = row["dist"]["tarball"].replace(old, "99.1.0")
            return row
        with patch.object(LATEST, "fetch_json", side_effect=newer):
            result = LATEST.resolve(self.lock, ["codex"])
        self.assertEqual(result["npm"]["codex"]["version"], "99.1.0")
        for key in ("python", "node", "nvm", "uv", "gh", "antigravity"):
            self.assertEqual(result[key], self.lock[key])

    def test_invalid_metadata_fails_without_mutating_input(self):
        original = copy.deepcopy(self.lock)
        for provider, mutation in [
            ("codex", lambda row: row.update(version="latest")),
            ("claude", lambda row: row["dist"].update(integrity="sha512-invalid")),
            ("codex", lambda row: row["dist"].update(tarball="https://evil.example/payload.tgz")),
            ("agy", lambda row: row.update(sha512="invalid")),
            ("agy", lambda row: row.update(url=row["url"].replace("antigravity-public", "foreign-bucket"))),
        ]:
            def altered(url):
                row = self.metadata(url)
                mutation(row)
                return row
            with self.subTest(provider=provider, mutation=mutation), patch.object(LATEST, "fetch_json", side_effect=altered):
                with self.assertRaises(LATEST.LockError):
                    LATEST.resolve(self.lock, [provider])
                self.assertEqual(self.lock, original)
        for candidate in ("99.1.0-rc.1", "0.0.1", "99.1.0+build"):
            with self.subTest(candidate=candidate), self.assertRaises(LATEST.LockError):
                LATEST.stable_version(candidate, "1.0.0")
        self.assertEqual(LATEST.stable_version("2.0.0", "1.99.99"), "2.0.0")

    def test_transport_rejects_foreign_hosts_and_redirects(self):
        for url in ("http://registry.npmjs.org/x", "https://evil.example/x", "https://user@registry.npmjs.org/x"):
            with self.assertRaises(LATEST.LockError):
                LATEST.fetch_json(url)
        with self.assertRaises(LATEST.LockError):
            LATEST.NoRedirect().redirect_request(None, None, 302, "", {}, "https://evil.example")

    def test_private_atomic_output_and_symlink_rejection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "state" / "snapshot.json"
            LATEST.save_snapshot(path, self.lock)
            self.assertEqual(json.loads(path.read_text()), self.lock)
            if os.name == "posix":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
                link = root / "linked"
                link.symlink_to(path.parent, target_is_directory=True)
                with self.assertRaises(LATEST.LockError):
                    LATEST.save_snapshot(link / "snapshot.json", self.lock)

    def test_previous_snapshot_merges_only_providers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            previous = root / "previous.json"
            output = root / "output.json"
            prior = copy.deepcopy(self.lock)
            prior["python"]["version"] = "99.0.0"
            row = prior["npm"]["claude"]
            row["version"] = "99.1.0"
            for native in row["native"].values():
                native["version"] = "99.1.0"
            LATEST.save_snapshot(previous, prior)
            argv = ["provider-latest.py", "--lock", str(ROOT / "tools.lock.json"),
                    "--provider", "codex", "--previous", str(previous), "--output", str(output)]
            with patch.object(LATEST.sys, "argv", argv), patch.object(LATEST, "fetch_json", side_effect=self.metadata):
                self.assertEqual(LATEST.main(), 0)
            result = json.loads(output.read_text())
            self.assertEqual(result["python"], self.lock["python"])
            self.assertEqual(result["npm"]["claude"], prior["npm"]["claude"])


if __name__ == "__main__":
    unittest.main()
