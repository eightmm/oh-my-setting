#!/usr/bin/env python3
from __future__ import annotations

import datetime as dt
import importlib.util
import json
import os
import pathlib
import shutil
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "lib" / "work_journal.py"


def load_module():
    spec = importlib.util.spec_from_file_location("work_journal", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load work_journal")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


wj = load_module()


NOW = dt.datetime(2026, 7, 31, 3, 0, tzinfo=dt.timezone.utc)


def base_event(source_id: str = "source-1", occurred_at: str = "2026-07-31T02:00:00Z"):
    return {
        "event_type": "validation",
        "occurred_at": occurred_at,
        "source": {"type": "agent-task", "id": source_id},
        "outcome": {"summary": "focused verification passed", "status": "success"},
        "verification_status": "passed",
        "evidence": [{"type": "task", "ref": source_id}],
    }


class JournalTestCase(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-test."))
        self.repo = self.tmp / "demo"
        self.repo.mkdir()
        self.store = wj.JournalStore(
            self.repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW,
            project_id="proj_test",
            project_name="demo",
        )

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def read_events(self):
        return self.store.load_events()

    def test_schema_validation_and_version(self):
        with self.assertRaises(wj.SchemaError):
            self.store.record_event({"event_type": "validation"})
        with self.assertRaises(wj.SchemaError):
            self.store.record_event(
                {
                    **base_event(),
                    "verification_status": "optimistic",
                }
            )
        event, created = self.store.record_event(base_event())
        self.assertTrue(created)
        self.assertEqual(wj.SCHEMA_VERSION, event["schema_version"])
        self.assertRegex(event["event_id"], r"^wj_[0-9a-f]{32}$")
        self.assertEqual("2026-07-31", event["local_date"])
        self.assertEqual("2026-W31", event["iso_week"])

    def test_unsupported_and_malformed_events_are_quarantined(self):
        self.store._ensure_layout()
        self.store.events_path.write_text(
            json.dumps({"schema_version": 99, "event_id": "future"})
            + "\n"
            + "{malformed-sentinel\n",
            encoding="utf-8",
        )
        self.assertEqual([], self.store.load_events())
        report = json.loads(self.store.quarantine_path.read_text(encoding="utf-8"))
        self.assertEqual(2, len(report["entries"]))
        self.assertEqual(
            {"malformed", "unsupported_schema"},
            {entry["reason"] for entry in report["entries"]},
        )
        self.assertTrue(
            all(
                set(entry) == {"record", "sha256", "reason"}
                for entry in report["entries"]
            )
        )
        self.assertNotIn("malformed-sentinel", json.dumps(report))

    def test_source_references_and_only_observed_fields_survive(self):
        payload = base_event()
        payload["refs"] = [
            {"type": "ledger", "id": "ledger-7", "path": "docs/EXPERIMENTS.jsonl"},
            {"type": "capsule", "id": "cap-7"},
            {"type": "artifact", "id": "artifact-7"},
        ]
        event, _ = self.store.record_event(payload)
        self.assertEqual(["ledger", "capsule", "artifact"], [r["type"] for r in event["refs"]])
        self.assertNotIn("decision", event)
        self.assertNotIn("blocker", event)
        self.assertNotIn("next_action", event)
        self.assertNotIn("metric", event)

    def test_research_fields_are_projected_structurally_not_parsed_from_note(self):
        source = self.tmp / "research-row.json"
        source.write_text(
            json.dumps(
                {
                    "schema": 1,
                    "id": "ledger-research",
                    "ts": "2026-07-31T02:00:00Z",
                    "exit": 0,
                    "duration_s": 4,
                    "note": "free prose must not be parsed",
                    "cmd": ["python", "train.py"],
                    "research": {
                        "question": "Does the bounded change help?",
                        "hypothesis": "The held-out metric increases.",
                        "prediction": "positive direction",
                        "baseline": "run-0",
                        "metric": "val_auc/scaffold",
                        "success": "at least 0.01",
                        "change": "optimizer only",
                    },
                }
            ),
            encoding="utf-8",
        )
        payload = wj.source_payload(self.repo, "run-ledger", source)
        event, _ = self.store.record_event(payload)
        self.assertEqual("Does the bounded change help?", event["goal"])
        self.assertEqual(
            "The held-out metric increases.", event["provenance"]["hypothesis"]
        )
        self.assertEqual("val_auc/scaffold", event["provenance"]["primary_metric"])
        self.assertNotIn("metrics", event)

    def test_metric_without_evidence_is_dropped(self):
        payload = base_event()
        payload.pop("evidence")
        payload["metrics"] = [{"name": "val_auc", "value": 0.8}]
        event, _ = self.store.record_event(payload)
        self.assertNotIn("metrics", event)

    def test_agent_state_update_does_not_inherit_old_verification(self):
        task = self.tmp / "TASK.md"
        task.write_text(
            "\n".join(
                [
                    "# Agent Task",
                    "- task_id: task-verified",
                    "- status: verified",
                    "",
                    "## Goal",
                    "- ship the feature",
                    "",
                    "## Decisions",
                    "- keep the local-first design",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        update = wj.source_payload(
            self.repo,
            "agent-task",
            task,
            operation="update",
        )
        close = wj.source_payload(
            self.repo,
            "agent-task",
            task,
            operation="close",
        )
        self.assertEqual("not_verified", update["verification_status"])
        self.assertEqual("passed", close["verification_status"])

    def test_correction_supersedes_without_rewrite(self):
        original, _ = self.store.record_event(base_event("old"))
        correction = base_event("correction")
        correction["event_type"] = "correction"
        correction["outcome"]["summary"] = "focused verification failed"
        correction["outcome"]["status"] = "failure"
        correction["verification_status"] = "failed"
        correction["supersedes_event_id"] = original["event_id"]
        revised, _ = self.store.record_event(correction)
        self.assertNotEqual(original["event_id"], revised["event_id"])
        self.assertEqual(2, len(self.store.events_path.read_text().splitlines()))
        active = self.store.active_events(self.store.load_events())
        self.assertEqual([revised["event_id"]], [event["event_id"] for event in active])

    def test_duplicate_source_and_task_retry_are_idempotent(self):
        first, created = self.store.record_event(base_event("task-1:verify"))
        second, created_again = self.store.record_event(base_event("task-1:verify"))
        self.assertTrue(created)
        self.assertFalse(created_again)
        self.assertEqual(first["event_id"], second["event_id"])
        self.assertEqual(1, len(self.read_events()))

    def test_event_id_priority_source_then_operation_then_stable_fields(self):
        source_first = base_event("authoritative")
        changed = base_event("authoritative")
        changed["outcome"]["summary"] = "a changed replay must not fork the source"
        first, _ = self.store.record_event(source_first)
        replay, created = self.store.record_event(changed)
        self.assertFalse(created)
        self.assertEqual(first["event_id"], replay["event_id"])

        operation = base_event()
        operation["source"] = {"type": "hook"}
        operation["correlation"] = {"operation_id": "operation-a"}
        operation_a, _ = self.store.record_event(operation)
        operation_replay = json.loads(json.dumps(operation))
        operation_replay["outcome"]["summary"] = "changed operation replay"
        normalized_replay = wj.normalize_event(
            operation_replay,
            project_id=self.store.project_id,
            project_name=self.store.project_name,
            timezone_name=self.store.timezone_name,
            timezone_info=self.store.timezone_info,
            recorded_at=NOW,
        )
        self.assertEqual(operation_a["event_id"], normalized_replay["event_id"])
        operation["correlation"]["operation_id"] = "operation-b"
        operation_b, _ = self.store.record_event(operation)
        self.assertNotEqual(operation_a["event_id"], operation_b["event_id"])

        fallback_a = base_event()
        fallback_a["source"] = {"type": "legacy"}
        fallback_b = json.loads(json.dumps(fallback_a))
        fallback_b["occurred_at"] = "2026-08-01T02:00:00Z"
        normalized_a = wj.normalize_event(
            fallback_a,
            project_id="proj_fallback",
            project_name="demo",
            timezone_name=self.store.timezone_name,
            timezone_info=self.store.timezone_info,
            recorded_at=NOW,
        )
        normalized_b = wj.normalize_event(
            fallback_b,
            project_id="proj_fallback",
            project_name="demo",
            timezone_name=self.store.timezone_name,
            timezone_info=self.store.timezone_info,
            recorded_at=NOW + dt.timedelta(days=2),
        )
        self.assertEqual(normalized_a["event_id"], normalized_b["event_id"])

    def test_distinct_same_time_events_are_not_merged(self):
        a = base_event("test-a")
        b = base_event("test-b")
        b["outcome"]["summary"] = "second verification passed"
        first, _ = self.store.record_event(a)
        second, _ = self.store.record_event(b)
        self.assertNotEqual(first["event_id"], second["event_id"])
        self.assertEqual(2, len(self.read_events()))

    def test_daily_weekly_and_rebuild_from_canonical_events(self):
        event, _ = self.store.record_event(base_event())
        self.store.materialize()
        daily = self.store.daily_dir / "2026-07-31.md"
        weekly = self.store.weekly_dir / "2026-W31.md"
        self.assertTrue(daily.is_file())
        self.assertTrue(weekly.is_file())
        text = daily.read_text(encoding="utf-8")
        self.assertIn("# Daily Work Journal — 2026-07-31", text)
        self.assertIn("## 검증된 것", text)
        self.assertIn(event["event_id"], text)
        shutil.rmtree(self.store.daily_dir)
        shutil.rmtree(self.store.weekly_dir)
        self.store.index_path.write_text("{damaged", encoding="utf-8")
        self.store.rebuild()
        self.assertTrue(daily.is_file())
        self.assertTrue(weekly.is_file())
        self.assertEqual(1, json.loads(self.store.index_path.read_text())["event_count"])

    def test_incremental_index_avoids_full_log_scan_and_untouched_rewrite(self):
        self.store.record_event(base_event("old", "2026-07-01T02:00:00Z"))
        self.store.materialize()
        old_daily = self.store.daily_dir / "2026-07-01.md"
        old_weekly = self.store.weekly_dir / "2026-W27.md"
        old_daily_bytes = old_daily.read_bytes()
        old_weekly_bytes = old_weekly.read_bytes()
        real_atomic_write_text = wj.atomic_write_text
        rewritten = []

        def record_write(path, content):
            rewritten.append(pathlib.Path(path))
            return real_atomic_write_text(path, content)

        with mock.patch.object(
            self.store,
            "load_events",
            side_effect=AssertionError("canonical log rescan"),
        ), mock.patch.object(wj, "atomic_write_text", side_effect=record_write):
            duplicate, created = self.store.record_event(
                base_event("old", "2026-07-01T02:00:00Z")
            )
            self.assertFalse(created)
            self.assertEqual("old", duplicate["source"]["id"])
            self.store.record_event(base_event("new", "2026-07-31T02:00:00Z"))
            index = self.store.materialize()

        self.assertEqual(2, index["event_count"])
        self.assertEqual(old_daily_bytes, old_daily.read_bytes())
        self.assertEqual(old_weekly_bytes, old_weekly.read_bytes())
        self.assertNotIn(old_daily, rewritten)
        self.assertNotIn(old_weekly, rewritten)
        self.assertTrue(self.store.index_db_path.is_file())

    def test_sync_candidates_use_derived_index_not_markdown_rescan(self):
        self.store.record_event(base_event("indexed-summary"))
        self.store.materialize()
        with mock.patch.object(
            pathlib.Path,
            "read_text",
            side_effect=AssertionError("summary file rescan"),
        ):
            rows = self.store._summary_rows(include_open=True)
        self.assertEqual(
            {
                "proj_test:daily:2026-07-31",
                "proj_test:weekly:2026-W31",
            },
            {row["summary_key"] for row in rows},
        )

    def test_incremental_correction_removes_empty_superseded_period(self):
        original, _ = self.store.record_event(
            base_event("original", "2026-07-01T02:00:00Z")
        )
        self.store.materialize()
        old_daily = self.store.daily_dir / "2026-07-01.md"
        old_weekly = self.store.weekly_dir / "2026-W27.md"
        self.assertTrue(old_daily.is_file())
        self.assertTrue(old_weekly.is_file())

        correction = base_event("replacement", "2026-07-31T02:00:00Z")
        correction["event_type"] = "correction"
        correction["supersedes_event_id"] = original["event_id"]
        self.store.record_event(correction)
        self.store.materialize()

        self.assertFalse(old_daily.exists())
        self.assertFalse(old_weekly.exists())
        self.assertTrue((self.store.daily_dir / "2026-07-31.md").is_file())
        self.assertTrue((self.store.weekly_dir / "2026-W31.md").is_file())

    def test_rebuild_recovers_derived_sqlite_index(self):
        self.store.record_event(base_event("one"))
        self.store.materialize()
        stale = self.store.daily_dir / "1999-01-01.md"
        stale.write_text("stale\n", encoding="utf-8")
        self.store.index_db_path.write_bytes(b"not a sqlite database")
        self.store.rebuild()
        self.assertEqual(1, self.store.status()["event_count"])
        self.assertEqual(1, len(self.store.load_events()))
        self.assertFalse(stale.exists())

    def test_notion_configuration_persists_no_credential(self):
        config_path = self.tmp / "config" / "work-journal.json"
        access_value = "test-" + "credential"
        access_name = "OMS_WORK_JOURNAL_NOTION_" + "TOKEN"
        configured = {
            "OMS_WORK_JOURNAL_CONFIG": str(config_path),
            access_name: access_value,
        }
        with mock.patch.dict(os.environ, configured, clear=False):
            wj.configure_notion(
                "ea343dea-4a66-4421-9653-dfc4fe68ed10",
                validate=False,
            )
            settings = wj.notion_settings()
        raw = config_path.read_text(encoding="utf-8")
        self.assertNotIn(access_value, raw)
        self.assertEqual(
            "ea343dea-4a66-4421-9653-dfc4fe68ed10",
            settings["data_source_id"],
        )
        self.assertEqual(access_value, settings["access_value"])
        self.assertEqual("Project", settings["project_property"])
        self.assertEqual("Kind", settings["kind_property"])
        self.assertEqual("Period", settings["period_property"])
        self.assertEqual("Has Blocker", settings["blocker_property"])

    def test_timezone_midnight_rollover(self):
        payload = base_event("midnight", "2026-07-30T15:30:00Z")
        event, _ = self.store.record_event(payload)
        self.assertEqual("2026-07-31", event["local_date"])
        self.store.materialize()
        self.assertTrue((self.store.daily_dir / "2026-07-31.md").is_file())

    def test_utc_works_without_platform_timezone_database(self):
        with mock.patch.object(wj, "ZoneInfo", None):
            name, timezone = wj.resolve_timezone("UTC")
        self.assertEqual("UTC", name)
        self.assertEqual(dt.timedelta(), timezone.utcoffset(NOW))

    def test_system_timezone_fallback_uses_per_event_os_conversion(self):
        with mock.patch.dict(
            os.environ, {"OMS_WORK_JOURNAL_TIMEZONE": ""}
        ), mock.patch.object(wj, "_zoneinfo_candidate", return_value=None):
            name, timezone = wj.resolve_timezone()
        self.assertEqual("system-local", name)
        self.assertIsNone(timezone)

    def test_explicit_iana_without_timezone_database_falls_back_explicitly(self):
        with mock.patch.object(wj, "ZoneInfo", side_effect=RuntimeError("no tzdata")):
            name, timezone = wj.resolve_timezone("Asia/Seoul")
        self.assertEqual("system-local", name)
        self.assertIsNone(timezone)
        self.assertFalse((self.store.daily_dir / "2026-07-30.md").exists())

    def test_iso_week_year_boundary(self):
        event, _ = self.store.record_event(base_event("year-edge", "2021-01-01T03:00:00Z"))
        self.assertEqual("2020-W53", event["iso_week"])
        self.store.materialize()
        weekly = self.store.weekly_dir / "2020-W53.md"
        self.assertIn("2020-W53", weekly.read_text(encoding="utf-8"))

    def test_catch_up_multiple_dates_without_empty_periods(self):
        for source, stamp in (
            ("a", "2026-07-01T00:00:00Z"),
            ("b", "2026-07-04T00:00:00Z"),
            ("c", "2026-07-20T00:00:00Z"),
        ):
            self.store.record_event(base_event(source, stamp))
        self.store.materialize()
        days = sorted(path.name for path in self.store.daily_dir.glob("*.md"))
        self.assertEqual(["2026-07-01.md", "2026-07-04.md", "2026-07-20.md"], days)

    def test_late_event_rematerializes_past_day_and_week(self):
        self.store.record_event(base_event("new", "2026-07-31T02:00:00Z"))
        self.store.materialize()
        self.store.record_event(base_event("late", "2026-07-01T02:00:00Z"))
        self.store.materialize()
        daily = (self.store.daily_dir / "2026-07-01.md").read_text(encoding="utf-8")
        weekly = (self.store.weekly_dir / "2026-W27.md").read_text(encoding="utf-8")
        self.assertIn("late", daily)
        self.assertIn("late", weekly)

    def test_materialization_is_deterministic(self):
        self.store.record_event(base_event("b", "2026-07-31T02:00:00Z"))
        self.store.record_event(base_event("a", "2026-07-31T01:00:00Z"))
        self.store.materialize()
        before = {
            path.relative_to(self.store.root).as_posix(): path.read_bytes()
            for path in sorted(self.store.root.rglob("*.md"))
        }
        self.store.materialize()
        after = {
            path.relative_to(self.store.root).as_posix(): path.read_bytes()
            for path in sorted(self.store.root.rglob("*.md"))
        }
        self.assertEqual(before, after)

    def test_comparable_metric_trend_only(self):
        for source, value in (("run-1", 0.7), ("run-2", 0.8)):
            payload = base_event(source)
            payload["event_type"] = "experiment"
            payload["verification_status"] = "not_verified"
            payload["metrics"] = [
                {
                    "name": "val_auc",
                    "value": value,
                    "unit": "ratio",
                    "dataset": "D",
                    "split": "scaffold",
                    "conditions": "seed=1",
                    "evidence_ref": source,
                }
            ]
            self.store.record_event(payload)
        for source, value in (("run-3", 0.9), ("run-4", 0.95)):
            incomplete = base_event(source)
            incomplete["event_type"] = "experiment"
            incomplete["metrics"] = [
                {
                    "name": "val_auc",
                    "value": value,
                    "dataset": "D",
                    "split": "random",
                    "evidence_ref": source,
                }
            ]
            self.store.record_event(incomplete)
        self.store.materialize()
        weekly = (self.store.weekly_dir / "2026-W31.md").read_text(encoding="utf-8")
        self.assertIn("0.7 → 0.8", weekly)
        self.assertNotIn("0.9 → 0.95", weekly)
        self.assertNotIn("improved", weekly.lower())
        self.assertNotIn("개선", weekly)

    def test_recursive_secret_redaction_and_raw_field_exclusion(self):
        payload = base_event("secret")
        sensitive_key = "API_" + "TOKEN"
        sensitive_value = "super-" + "secret"
        payload["decision"] = (
            "use Authori"
            + "zation: Bea"
            + "rer abcdefghijklmnop; pass"
            + "word=hunter3"
            + "; Authori"
            + "zation: Ba"
            + "sic dXNlcjpwYXNz"
        )
        payload["refs"] = [
            {
                "type": "remote",
                "url": "https://alice" + ":hunter2@example.com/org/repo.git",
                "to" + "ken": "gh" + "p_abcdefghijklmnopqrstuvwxyz",
            }
        ]
        payload["refs"].append(
            {
                "type": "remote",
                "url": "https://single-userinfo@example.com/org/repo.git",
            }
        )
        payload["provenance"] = {
            "config": {sensitive_key: sensitive_value, "safe": "kept"},
            "environment": {"HOME": "/ho" + "me/person"},
            "stdout": "raw output",
            "transcript": "raw transcript",
        }
        event, _ = self.store.record_event(payload)
        raw = json.dumps(event, ensure_ascii=False)
        for secret in (
            "abcdefghijklmnop",
            "hunter2",
            "hunter3",
            "dXNlcjpwYXNz",
            "single-userinfo",
            "ghp_",
            "super-secret",
            "raw output",
            "raw transcript",
        ):
            self.assertNotIn(secret, raw)
        self.assertIn("[REDACTED]", raw)
        self.assertNotIn("environment", event.get("provenance", {}))

    def test_bounded_large_values(self):
        payload = base_event("large")
        payload["outcome"]["summary"] = "x" * 100000
        payload["refs"] = [{"type": "artifact", "id": str(i)} for i in range(1000)]
        event, _ = self.store.record_event(payload)
        self.assertLessEqual(len(event["outcome"]["summary"].encode()), wj.MAX_TEXT_BYTES)
        self.assertLessEqual(len(event["refs"]), wj.MAX_COLLECTION_ITEMS)

    def test_optional_enrichment_timeout_falls_back_to_template(self):
        def timeout(_text, _content_hash):
            raise TimeoutError("provider timeout")

        template = "# deterministic"
        self.assertEqual(template, wj.optional_enrichment(template, timeout))
        self.assertEqual(template, wj.optional_enrichment(template, None))

    def test_append_and_renderer_exceptions_can_be_fail_open(self):
        with mock.patch.object(self.store, "_write_event", side_effect=OSError("read only")):
            result = wj.observe_fail_open(self.store, base_event("append-fail"))
        self.assertFalse(result)
        with mock.patch.object(self.store, "_render_all", side_effect=RuntimeError("renderer")):
            result = wj.observe_fail_open(self.store, base_event("render-fail"))
        self.assertFalse(result)

    def test_summary_and_index_atomic_replacement_leave_valid_files(self):
        self.store.record_event(base_event())
        self.store.materialize()
        prior_index = self.store.index_path.read_bytes()
        prior_daily = (self.store.daily_dir / "2026-07-31.md").read_bytes()
        real_replace = os.replace

        def fail_index_replace(source, target):
            if pathlib.Path(target) == self.store.index_path:
                raise OSError("injected replace failure")
            return real_replace(source, target)

        with mock.patch.object(wj.os, "replace", side_effect=fail_index_replace):
            with self.assertRaises(OSError):
                self.store.materialize()
        self.assertEqual(prior_index, self.store.index_path.read_bytes())
        self.assertEqual(
            prior_daily, (self.store.daily_dir / "2026-07-31.md").read_bytes()
        )
        json.loads(self.store.index_path.read_text(encoding="utf-8"))
        leftovers = [p for p in self.store.root.rglob("*") if ".tmp-" in p.name]
        self.assertEqual([], leftovers)

    def test_head_commit_capture_uses_git_object_as_source(self):
        subprocess = __import__("subprocess")
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.name", "Test"], check=True
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "config", "user.email", "test@example.com"],
            check=True,
        )
        (self.repo / "tracked.txt").write_text("one\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(self.repo), "add", "tracked.txt"], check=True)
        environment = dict(os.environ)
        environment.update(
            {
                "GIT_AUTHOR_DATE": "2026-07-30T10:00:00+00:00",
                "GIT_COMMITTER_DATE": "2026-07-30T10:00:00+00:00",
            }
        )
        subprocess.run(
            ["git", "-C", str(self.repo), "commit", "-qm", "bounded subject"],
            check=True,
            env=environment,
        )
        self.assertTrue(self.store.capture_head_commit())
        self.assertFalse(self.store.capture_head_commit())
        event = self.store.load_events()[0]
        self.assertEqual("commit", event["event_type"])
        self.assertEqual("git-commit", event["source"]["type"])
        self.assertEqual(40, len(event["source"]["id"]))

    def test_show_helpers_recent_events_and_open_annotations(self):
        annotated = base_event("annotated")
        annotated["blocker"] = "CI red on shellcheck"
        annotated["next_action"] = "wire the journal read path"
        self.store.record_event(annotated)
        self.store.record_event(
            {**base_event("plain", occurred_at="2026-07-31T02:30:00Z")}
        )
        # A repeated annotation must not appear twice.
        duplicate = base_event("annotated-again", occurred_at="2026-07-31T02:45:00Z")
        duplicate["blocker"] = "CI red on shellcheck"
        self.store.record_event(duplicate)
        self.store.materialize()

        recent = self.store.recent_events(limit=2)
        self.assertEqual(2, len(recent))
        self.assertEqual(
            ["2026-07-31T02:45:00Z", "2026-07-31T02:30:00Z"],
            [event["occurred_at"] for event in recent],
        )

        annotations = self.store.open_annotations()
        self.assertEqual("2026-07-25", annotations["since"])
        self.assertEqual(1, len(annotations["blockers"]))
        self.assertEqual("CI red on shellcheck", annotations["blockers"][0]["text"])
        self.assertEqual(
            ["wire the journal read path"],
            [row["text"] for row in annotations["next_actions"]],
        )

    def test_summary_text_returns_none_for_unknown_period(self):
        self.store.record_event(base_event())
        self.store.materialize()
        self.assertIn(
            "Daily Work Journal — 2026-07-31",
            self.store.summary_text("daily", "2026-07-31"),
        )
        self.assertIsNone(self.store.summary_text("daily", "1999-01-01"))

    def test_prompt_digest_fires_once_per_local_day_and_stays_bounded(self):
        self.assertEqual("", self.store.prompt_digest())
        self.assertFalse(self.store.digest_state_path.exists())

        annotated = base_event("annotated")
        annotated["blocker"] = "CI red on shellcheck"
        annotated["next_action"] = "wire the journal read path"
        self.store.record_event(annotated)
        self.store.materialize()

        digest = self.store.prompt_digest()
        self.assertIn("CI red on shellcheck", digest)
        self.assertIn("wire the journal read path", digest)
        self.assertIn("oms journal show", digest)
        self.assertLessEqual(len(digest.splitlines()), 12)
        self.assertEqual("", self.store.prompt_digest())

        next_day = wj.JournalStore(
            self.repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW + dt.timedelta(days=1),
            project_id="proj_test",
            project_name="demo",
        )
        follow_up = next_day.prompt_digest()
        self.assertIn("Last journal day 2026-07-31: 1 events, 1 verified.", follow_up)
        self.assertEqual("", next_day.prompt_digest())

    def test_renderer_language_is_configurable_and_blocker_scan_survives(self):
        annotated = base_event("annotated")
        annotated["blocker"] = "CI red on shellcheck"
        self.store.record_event(annotated)
        with mock.patch.dict(
            os.environ, {"OMS_WORK_JOURNAL_LANG": "en"}, clear=False
        ):
            self.store.rebuild()
            daily = (self.store.daily_dir / "2026-07-31.md").read_text(
                encoding="utf-8"
            )
            self.assertIn("## Key progress", daily)
            self.assertIn("- none recorded", daily)
            self.assertNotIn("기록 없음", daily)
            self.assertTrue(
                self.store._summary_has_blocker(
                    self.store.summary_text("daily", "2026-07-31")
                )
            )
        self.assertFalse(
            self.store._summary_has_blocker(
                "## Blockers\n- none recorded\n## Next\n"
            )
        )
        # The Korean default renders unchanged.
        self.store.rebuild()
        daily = (self.store.daily_dir / "2026-07-31.md").read_text(encoding="utf-8")
        self.assertIn("## 핵심 진전", daily)

    def test_project_identity_is_create_once_and_recoverable_from_events(self):
        self.store.record_event(base_event("identity"))
        original = self.store.project_id
        changed = wj.JournalStore(
            self.repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW,
        )
        self.assertEqual(original, changed.project_id)
        self.store.project_path.unlink()
        canonical = self.store.events_path.read_bytes()
        self.store.events_path.write_bytes(b"{malformed-prefix\n" + canonical)
        recovered = wj.JournalStore(
            self.repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW,
        )
        self.assertEqual(original, recovered.project_id)


if __name__ == "__main__":
    unittest.main()
