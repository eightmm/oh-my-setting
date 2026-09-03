#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import datetime as dt
import importlib.util
import io
import json
import os
import pathlib
import shutil
import subprocess
import sys
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
        # These cases assert Korean labels, which are no longer the fixed
        # default: pin the language so the runner's locale cannot decide it.
        language = mock.patch.dict(
            os.environ, {"OMS_WORK_JOURNAL_LANG": "ko"}, clear=False
        )
        language.start()
        self.addCleanup(language.stop)
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

    def test_stated_judgment_survives_every_source_type(self):
        """--decision/--blocker/--next-action are caller-stated overrides.

        They were attached inside the lifecycle-verb branch only, so every
        other source type accepted them on the command line and dropped them:
        agent-task, run-ledger, ci-status and any generic JSON record. That is
        the capture gap, not the rendering one.
        """
        source = self.repo / "record.json"
        source.write_text(
            json.dumps({"id": "rec-1", "ts": "2026-08-22T06:00:00Z", "summary": "a record"}),
            encoding="utf-8",
        )
        for source_type in ("goal-drive", "run-ledger", "commit"):
            payload = wj.source_payload(
                self.repo, source_type, source,
                decision="이 경로를 선택했다",
                blocker="원격이 응답하지 않는다",
                next_action="다음 세션에서 재시도",
            )
            self.assertEqual("이 경로를 선택했다", payload.get("decision"), source_type)
            self.assertEqual("원격이 응답하지 않는다", payload.get("blocker"), source_type)
            self.assertEqual("다음 세션에서 재시도", payload.get("next_action"), source_type)
        # And it survives admission, which is what the reader ends up seeing.
        event, _ = self.store.record_event(
            wj.source_payload(self.repo, "commit", source, decision="기록된 결정")
        )
        self.assertEqual("기록된 결정", event["decision"])

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

    def test_summaries_open_with_counts_and_judgment_context(self):
        decided = base_event("decided")
        decided["decision"] = "map-style sharding을 유지한다"
        decided["blocker"] = "Windows 검증 대기"
        decided["next_action"] = "Windows runner 결과를 확인한다"
        pending = base_event("pending", "2026-08-01T02:00:00Z")
        pending["outcome"] = {
            "summary": "Windows runner queued",
            "status": "recorded",
        }
        pending["verification_status"] = "not_verified"
        self.store.record_event(decided)
        self.store.record_event(pending)
        self.store.materialize()

        daily = self.store.summary_text("daily", "2026-07-31")
        weekly = self.store.summary_text("weekly", "2026-W31")
        self.assertIn("## 한눈에 보기", daily)
        self.assertIn("기록 1", daily)
        self.assertIn("검증 1", daily)
        self.assertIn("Blocker 1", daily)
        self.assertIn(
            "- map-style sharding을 유지한다 — 관련 작업: focused verification passed",
            daily,
        )
        self.assertIn(
            "- Windows 검증 대기 — 관련 작업: focused verification passed",
            daily,
        )
        self.assertIn("## 한눈에 보기", weekly)
        self.assertIn("기록 2", weekly)
        self.assertIn("미검증 1", weekly)
        self.assertIn(
            "- 2026-07-31 · map-style sharding을 유지한다"
            " — 관련 작업: focused verification passed",
            weekly,
        )

        daily_notion = wj.notion_presentation(daily)
        weekly_notion = wj.notion_presentation(weekly)
        self.assertIn(
            "> map-style sharding을 유지한다 — 관련 작업: focused verification passed",
            daily_notion,
        )
        self.assertIn(
            "- 2026-07-31 · map-style sharding을 유지한다"
            " — 관련 작업: focused verification passed",
            weekly_notion,
        )
        self.assertNotIn(
            "> 2026-07-31 · map-style sharding을 유지한다", weekly_notion
        )
        noisy = dict(decided)
        noisy["decision"] = "짧은 결정"
        noisy["outcome"] = {"summary": "x" * 300, "status": "recorded"}
        self.assertEqual(
            "짧은 결정",
            wj._judgment_summary(noisy, "decision", wj._headings()),
        )

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

    def test_renderer_version_change_rematerializes_existing_periods(self):
        self.store.record_event(base_event("renderer-upgrade"))
        self.store.materialize()
        daily = self.store.daily_dir / "2026-07-31.md"
        daily.write_text("stale renderer\n", encoding="utf-8")

        with mock.patch.object(wj, "RENDERER_VERSION", wj.RENDERER_VERSION + 1):
            self.store.materialize()

        refreshed = daily.read_text(encoding="utf-8")
        self.assertIn("# Daily Work Journal — 2026-07-31", refreshed)
        self.assertIn("## 한눈에 보기", refreshed)

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
        self.assertTrue(event["outcome"]["summary"].endswith("…"))
        self.assertLessEqual(len(event["refs"]), wj.MAX_COLLECTION_ITEMS)
        legacy = {
            "decision": "가" * 666,
            "local_date": "2026-07-31",
            "outcome": {},
        }
        self.assertTrue(
            wj._judgment_summary(legacy, "decision", wj._headings()).endswith("…")
        )

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
        # An idle tick rewrites nothing, so give the index a real change on
        # another day; the 07-31 view stays untouched either way.
        self.store.record_event(base_event("other-day", "2026-07-30T02:00:00Z"))
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

    def test_prompt_digest_points_at_the_newest_recent_handoff(self):
        annotated = base_event("annotated")
        annotated["blocker"] = "CI red on shellcheck"
        self.store.record_event(annotated)
        self.store.materialize()
        handoffs = self.repo / ".oms" / "handoffs"
        handoffs.mkdir(parents=True)
        for name, hours_old in (("older.md", 5), ("newest.md", 1)):
            path = handoffs / name
            path.write_text("digest", encoding="utf-8")
            stamp = (NOW - dt.timedelta(hours=hours_old)).timestamp()
            os.utime(path, (stamp, stamp))
        digest = self.store.prompt_digest()
        self.assertIn("oms session-handoff show newest.md", digest)
        self.assertNotIn("older.md", digest)

        # A handoff older than the pointer window is silence, not a pointer.
        stale_repo = self.tmp / "stale"
        stale_repo.mkdir()
        stale_store = wj.JournalStore(
            stale_repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW,
            project_id="proj_stale",
            project_name="stale",
        )
        stale_store.record_event(base_event("stale-source"))
        stale_store.materialize()
        stale_handoffs = stale_repo / ".oms" / "handoffs"
        stale_handoffs.mkdir(parents=True)
        stale_path = stale_handoffs / "old.md"
        stale_path.write_text("digest", encoding="utf-8")
        stamp = (NOW - dt.timedelta(hours=60)).timestamp()
        os.utime(stale_path, (stamp, stamp))
        next_day = wj.JournalStore(
            stale_repo,
            timezone_name="Asia/Seoul",
            clock=lambda: NOW + dt.timedelta(days=1),
            project_id="proj_stale",
            project_name="stale",
        )
        follow_up = next_day.prompt_digest()
        self.assertTrue(follow_up)
        self.assertNotIn("session-handoff", follow_up)

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
            self.assertIn("## At a glance", daily)
            self.assertIn("Events 1", daily)
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

    def test_bounded_summary_keeps_judgment_sections_and_marks_omission(self):
        for index in range(6):
            payload = base_event("verbose-%d" % index)
            payload["outcome"] = {
                "summary": ("상세 구현 결과 %d " % index) + ("x" * 420),
                "status": "success",
            }
            if index == 5:
                payload["blocker"] = "Windows 검증 대기"
                payload["next_action"] = "Windows runner 결과를 확인한다"
            self.store.record_event(payload)

        with mock.patch.object(wj, "MAX_EXPORT_BYTES", 1800):
            self.store.materialize()
        summary = self.store.summary_text("daily", "2026-07-31")
        self.assertLessEqual(len(summary.encode("utf-8")), 1800)
        self.assertIn("## Blockers", summary)
        self.assertIn("Windows 검증 대기", summary)
        self.assertIn("## 다음 우선순위", summary)
        self.assertIn("상세 항목 일부 생략", summary)
        self.assertTrue(self.store._summary_has_blocker(summary))

    def test_materialized_summary_preserves_detail_indentation(self):
        payload = base_event("nested-details")
        payload["outcome"]["interpretation"] = "token expiry가 해소됨"
        self.store.record_event(payload)
        self.store.materialize()

        summary = self.store.summary_text("daily", "2026-07-31")
        self.assertIn("\n  - 결과: success", summary)
        self.assertIn("\n  - 해석: token expiry가 해소됨", summary)

    def test_notion_mirror_of_a_rendered_day_carries_no_raw_reference(self):
        sha = "0e0390aa95893b50e14bdf78d60f5c5d3090cf8d"
        handoff = "b" * 64
        payload = base_event("evidence-leak")
        payload["evidence"] = [
            {"type": "git-commit", "ref": sha},
            {"type": "handoff", "ref": handoff},
        ]
        self.store.record_event(payload)
        self.store.materialize()
        daily = (self.store.daily_dir / "2026-07-31.md").read_text(encoding="utf-8")
        # The local file is the evidence layer and keeps both references,
        # including the indented bullet the mirror has to drop.
        self.assertIn("  - 관련 evidence: git-commit:%s" % sha, daily)

        rendered = wj.notion_presentation(daily)
        self.assertNotRegex(rendered, r"[0-9a-f]{40}")
        self.assertNotIn(handoff, rendered)
        self.assertNotIn("관련 evidence", rendered)
        self.assertIn("- 작업: focused verification passed", rendered)

    def test_notion_mirror_folds_a_bullet_into_its_highest_ranked_section(self):
        alpha = base_event("alpha")
        alpha["outcome"] = {"summary": "alpha check passed", "status": "success"}
        beta = base_event("beta", "2026-07-31T02:30:00Z")
        beta["outcome"] = {"summary": "beta still open", "status": "recorded"}
        beta["verification_status"] = "not_verified"
        self.store.record_event(alpha)
        self.store.record_event(beta)
        self.store.materialize()
        daily = (self.store.daily_dir / "2026-07-31.md").read_text(encoding="utf-8")
        # Progress and verified/unverified list the same facts locally.
        self.assertEqual(2, daily.count("- alpha check passed"))

        rendered = wj.notion_presentation(daily)
        self.assertEqual(1, rendered.count("- alpha check passed"))
        self.assertEqual(1, rendered.count("- beta still open"))
        # 검증된 것 and 아직 검증되지 않은 것 outrank 핵심 진전, which the
        # source file lists first, so the fold has to follow the presented
        # order rather than the file order.
        self.assertLess(
            rendered.index("## 검증된 것"), rendered.index("- alpha check passed")
        )
        self.assertLess(
            rendered.index("## 아직 검증되지 않은 것"),
            rendered.index("- beta still open"),
        )
        # Having given up every bullet, the duplicate progress listing goes.
        self.assertNotIn("## 핵심 진전", rendered)
        # The by-project section keeps its own labeled bullets and heading.
        self.assertIn("### demo", rendered)
        self.assertIn("- 작업: alpha check passed", rendered)

    def test_daily_sessions_section_and_decision_callout_marks(self):
        alpha = base_event("alpha")
        alpha["correlation"] = {"session_id": "sess-alpha-one"}
        alpha["decision"] = "going with map-style sharding"
        beta = base_event("beta", "2026-07-31T02:30:00Z")
        beta["correlation"] = {"session_id": "sess-alpha-one"}
        self.store.record_event(alpha)
        self.store.record_event(beta)
        self.store.materialize()
        daily = (self.store.daily_dir / "2026-07-31.md").read_text(encoding="utf-8")
        self.assertIn("## 세션", daily)
        self.assertIn("- sess-alpha-one (2 event(s))", daily)
        self.assertIn("oms session-handoff list", daily)

        rendered = wj.notion_presentation(daily)
        # The decision keeps its text but trades its bullet for a quote mark,
        # which the exporter renders as a callout.
        self.assertIn(
            "> going with map-style sharding"
            " — 관련 작업: focused verification passed",
            rendered,
        )
        self.assertNotIn("- going with map-style sharding", rendered)
        # Sessions are reference material: they rank below the verified work.
        self.assertLess(
            rendered.index("## 검증된 것"), rendered.index("## 세션")
        )

    def test_daily_period_aggregates_count_sessions_commits_verified(self):
        alpha = base_event("alpha")
        alpha["correlation"] = {"session_id": "sess-alpha-one"}
        commit = base_event("commit-1", "2026-07-31T03:00:00Z")
        commit["event_type"] = "commit"
        commit["verification_status"] = "not_verified"
        commit["correlation"] = {"session_id": "sess-beta-two"}
        self.store.record_event(alpha)
        self.store.record_event(commit)
        self.store.materialize()
        aggregates = self.store._period_aggregates("daily", "2026-07-31")
        self.assertEqual(
            ["sess-alpha-one", "sess-beta-two"], aggregates["sessions"]
        )
        self.assertEqual(1, aggregates["commits"])
        self.assertEqual(1, aggregates["verified"])
        self.assertEqual({}, self.store._period_aggregates("weekly", "2026-W31"))

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

    def test_github_identity_normalizes_ssh_and_https_remotes(self):
        repos = []
        for index, remote in enumerate(
            (
                "git@github.com:EightMM/Oh-My-Setting.git",
                "https://github.com/eightmm/oh-my-setting.git",
                "ssh://git@github.com/eightmm/oh-my-setting",
            )
        ):
            repo = self.tmp / ("github-%d" % index)
            repo.mkdir()
            subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
            subprocess.run(
                ["git", "-C", str(repo), "remote", "add", "origin", remote],
                check=True,
            )
            repos.append(repo)

        identities = [wj.project_identity(repo) for repo in repos]
        self.assertEqual(1, len({row[0] for row in identities}))
        self.assertEqual(
            {"eightmm/oh-my-setting"},
            {row[1] for row in identities},
        )


class IdleMaterializeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-idle."))
        self.store = wj.JournalStore(
            self.tmp / "demo", timezone_name="Asia/Seoul", clock=lambda: NOW,
            project_id="proj_test", project_name="demo",
        )

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_materialize_with_nothing_dirty_rewrites_no_view(self):
        """Every prompt, Stop, and session start ticks the journal; a tick with
        no new event must not fsync-rewrite the summary index or any view."""
        self.store.record_event(base_event("first"))
        self.store.materialize()
        index = self.store.index_path
        daily = self.store.daily_dir / "2026-07-31.md"
        before = (index.stat().st_ino, index.stat().st_mtime_ns,
                  daily.stat().st_ino, daily.stat().st_mtime_ns)
        self.assertEqual(self.store.materialize(), self.store.materialize())
        after = (index.stat().st_ino, index.stat().st_mtime_ns,
                 daily.stat().st_ino, daily.stat().st_mtime_ns)
        self.assertEqual(before, after)
        # A new event still re-renders its period and the index.
        self.store.record_event(base_event("second"))
        self.store.materialize()
        self.assertNotEqual(before[:2], (index.stat().st_ino, index.stat().st_mtime_ns))


class NotionFailureBackoffTest(unittest.TestCase):
    """A summary the exporter keeps refusing must not be retried on every
    tick: the prompt hook's one-row sync spent a live HTTPS round trip per
    prompt re-failing the same page for weeks, because only a Retry-After
    scheduled a next attempt."""

    def setUp(self) -> None:
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-backoff."))
        self.now = NOW
        self.store = wj.JournalStore(
            self.tmp / "demo", timezone_name="Asia/Seoul",
            clock=lambda: self.now, project_id="proj_test", project_name="demo",
        )
        self.store.record_event(base_event("refused-summary"))
        self.store.materialize()
        self.upserts = 0
        test = self

        class FakeExporter:
            @classmethod
            def from_config(cls, **_settings):
                return cls()

            def upsert(self, *_args, **_kwargs):
                test.upserts += 1
                raise RuntimeError("refused")

        fake_module = type(sys)("notion_journal")
        fake_module.NotionJournalExporter = FakeExporter
        patches = [
            mock.patch.dict(sys.modules, {"notion_journal": fake_module}),
            mock.patch.object(wj, "notion_repo_excluded", lambda _repo: False),
            mock.patch.object(wj, "notion_auth_available", lambda _settings: True),
            mock.patch.object(wj, "notion_settings", lambda: {
                "data_source_id": "ds-1", "database_id": "",
                **{key: key for key in (
                    "title_property", "key_property", "hash_property",
                    "project_property", "kind_property", "period_property",
                    "blocker_property", "sessions_property",
                    "commits_property", "verified_property")},
            }),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def state(self):
        return json.loads(self.store.notion_state_path.read_text(encoding="utf-8"))

    def sync(self):
        return self.store.sync_notion(force=True, max_per_tick=1, budget_seconds=2)

    def row(self):
        return self.state()["summaries"]["proj_test:daily:2026-07-31"]

    def test_a_refused_summary_backs_off_from_its_second_failure_until_its_content_changes(self):
        first = self.sync()
        self.assertEqual(1, first["attempted"])
        self.assertEqual("failed", self.row()["status"])
        self.assertEqual(1, self.row()["attempts"])
        self.assertNotIn("next_retry_at", self.row())
        # One immediate retry: a timeout is usually transient.
        second = self.sync()
        self.assertEqual(1, second["attempted"])
        self.assertEqual(2, self.row()["attempts"])
        self.assertEqual(wj.parse_rfc3339(self.row()["next_retry_at"]),
                         NOW + dt.timedelta(seconds=60))
        # Same tick again: the backed-off daily is skipped (the one-row tick
        # goes to the weekly instead, so a stuck page starves nothing), and
        # the report says a row is waiting rather than reading as idle.
        third = self.sync()
        self.assertEqual(1, third["waiting"])
        self.assertEqual(wj.parse_rfc3339(third["next_retry_at"]),
                         NOW + dt.timedelta(seconds=60))
        self.assertEqual(2, self.row()["attempts"])
        self.assertEqual(wj.parse_rfc3339(self.row()["next_retry_at"]),
                         NOW + dt.timedelta(seconds=60))
        # Past the deadline the retry happens once and the wait doubles.
        self.now = NOW + dt.timedelta(seconds=61)
        self.sync()
        self.assertEqual(3, self.row()["attempts"])
        self.assertEqual(wj.parse_rfc3339(self.row()["next_retry_at"]),
                         self.now + dt.timedelta(seconds=120))
        # New content for the period is a fresh attempt, not a backed-off one.
        self.store.record_event(base_event("fresh-content"))
        self.store.materialize()
        self.sync()
        self.assertEqual(1, self.row()["attempts"])
        self.assertNotIn("next_retry_at", self.row())

    def test_the_wait_is_capped_at_a_day(self):
        self.sync()
        for _ in range(13):
            self.sync()
            self.now = wj.parse_rfc3339(self.row()["next_retry_at"]) + dt.timedelta(seconds=1)
        self.assertEqual(14, self.row()["attempts"])
        self.assertLessEqual(
            wj.parse_rfc3339(self.row()["next_retry_at"]) - (self.now - dt.timedelta(seconds=1)),
            dt.timedelta(days=1))


class ObserveDiagnosticsTest(unittest.TestCase):
    """A refused observe must say why: the operator front door printed
    nothing and exited 1 for a wrong --verification-status or an unregistered
    source type whose file is not JSON, which reads as "nothing happened"."""

    def setUp(self) -> None:
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        (self.repo / ".oms").mkdir(parents=True)
        subprocess.run(["git", "-C", str(self.repo), "init", "-q"], check=True)
        self.source = self.repo / "notes.md"
        self.source.write_text("not json\n", encoding="utf-8")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def observe(self, *extra: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["python3", str(MODULE_PATH), "observe", "--repo", str(self.repo),
             "--source-file", str(self.source), *extra],
            capture_output=True, text=True, check=False)

    def test_wrong_verification_status_is_refused_with_the_allowed_values(self):
        result = self.observe("--source-type", "oms-run",
                              "--verification-status", "verified")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("verification-status", result.stderr)
        self.assertIn("passed", result.stderr)
        self.assertFalse((self.repo / ".oms" / "work-journal" / "events.jsonl").exists())

    def test_session_handoff_summary_is_the_last_assistant_line(self):
        self.source.write_text("# Session handoff\n\n## Last assistant summary\n\n"
                               "**Done: pushed x**\n\n## Open dissents\n", encoding="utf-8")
        result = self.observe("--source-type", "session-handoff", "--source-id", "s1")
        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.repo / ".oms" / "work-journal" / "events.jsonl"
        row = json.loads(events.read_text(encoding="utf-8").splitlines()[-1])
        self.assertEqual(row["outcome"]["summary"], "Done: pushed x")
        self.source.write_text("## Last assistant summary\n\n## Open dissents\n", encoding="utf-8")
        self.assertEqual(self.observe("--source-type", "session-handoff", "--source-id", "s2").returncode, 0)
        row = json.loads(events.read_text(encoding="utf-8").splitlines()[-1])
        self.assertEqual(row["outcome"]["summary"], "Session handoff captured")

    def test_unregistered_source_type_without_a_json_record_says_so(self):
        result = self.observe("--source-type", "evolution-round")
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: work journal: source record is not JSON", result.stderr)
        self.assertNotIn(str(self.source), result.stderr)


class NotionTransportPersistenceTest(unittest.TestCase):
    def test_configure_persists_and_settings_prefer_the_transport_choice(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = pathlib.Path(tmp) / "work-journal.json"
            env = {
                "OMS_WORK_JOURNAL_CONFIG": str(config),
                "OMS_NOTION_CLI": "/opt/bin/ntn",
                "OMS_NOTION_KEYRING": "file",
                "OMS_WORK_JOURNAL_NOTION_AUTH": "ntn",
            }
            with mock.patch.dict(os.environ, env, clear=False):
                wj.configure_notion("0" * 32, validate=False, auth_mode="ntn")
            stored = json.loads(config.read_text())["notion"]
            self.assertEqual("/opt/bin/ntn", stored["cli_command"])
            self.assertEqual("file", stored["keyring"])

            # A hook environment carries neither env var: the persisted
            # choice must win, and env must still override when present.
            clean = {
                "OMS_WORK_JOURNAL_CONFIG": str(config),
                "OMS_NOTION_CLI": "",
                "OMS_NOTION_KEYRING": "",
            }
            with mock.patch.dict(os.environ, clean, clear=False):
                settings = wj.notion_settings()
            self.assertEqual("/opt/bin/ntn", settings["cli_command"])
            self.assertEqual("file", settings["keyring"])
            override = dict(clean, OMS_NOTION_CLI="ntn2", OMS_NOTION_KEYRING="os")
            with mock.patch.dict(os.environ, override, clear=False):
                settings = wj.notion_settings()
            self.assertEqual("ntn2", settings["cli_command"])
            self.assertEqual("os", settings["keyring"])


class JournalLanguageTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-lang."))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.config = self.tmp / "work-journal.json"

    def env(self, **overrides):
        # An empty string reads as unset everywhere in the resolution chain,
        # so the matrix stays independent of the runner's own locale.
        base = {
            "OMS_WORK_JOURNAL_CONFIG": str(self.config),
            "OMS_WORK_JOURNAL_LANG": "",
            "LC_ALL": "",
            "LC_MESSAGES": "",
            "LANG": "",
        }
        base.update(overrides)
        return mock.patch.dict(os.environ, base, clear=False)

    def test_resolution_order_env_then_config_then_locale_then_english(self):
        with self.env(LANG="en_US.UTF-8"):
            wj.set_journal_language("ko")
            # A config pin beats a machine locale that says otherwise: the
            # journal is written in its author's language.
            self.assertEqual("ko", wj.journal_language())
            with mock.patch.dict(
                os.environ, {"OMS_WORK_JOURNAL_LANG": "en"}, clear=False
            ):
                self.assertEqual("en", wj.journal_language())
        self.config.unlink()
        with self.env(LANG="ko_KR.UTF-8"):
            self.assertEqual("ko", wj.journal_language())
        with self.env(LANG="en_US.UTF-8"):
            self.assertEqual("en", wj.journal_language())
        with self.env(LC_ALL="C.UTF-8", LANG="ko_KR.UTF-8"):
            self.assertEqual("en", wj.journal_language())
        with self.env():
            self.assertEqual("en", wj.journal_language())

    def test_pinned_language_reaches_the_rendered_summary(self):
        repo = self.tmp / "repo"
        repo.mkdir()
        with self.env(LANG="ko_KR.UTF-8"):
            wj.set_journal_language("en")
            store = wj.JournalStore(
                repo,
                timezone_name="Asia/Seoul",
                clock=lambda: NOW,
                project_id="proj_lang",
                project_name="lang",
            )
            store.record_event(base_event("lang"))
            store.materialize()
        daily = (store.daily_dir / "2026-07-31.md").read_text(encoding="utf-8")
        self.assertIn("## Key progress", daily)
        self.assertNotIn("핵심 진전", daily)

    def cli(self, *argv):
        """Run the CLI front door, keeping its output out of the test log."""

        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = wj.main(list(argv))
        return status, out.getvalue()

    def test_configure_lang_is_a_front_door_that_survives_notion_rewrite(self):
        with self.env():
            status, out = self.cli("configure", "--lang", "ko")
            self.assertEqual(0, status)
            self.assertIn("journal language: ko", out)
            self.assertIn("rebuild", out)
            stored = json.loads(self.config.read_text(encoding="utf-8"))
            self.assertEqual("ko", stored["language"])
            # The Notion object is present but empty, so every other reader
            # still loads the file.
            self.assertEqual({}, stored["notion"])
            self.assertEqual("ko", wj.journal_language())
            # A typo has to be visibly rejected: the __main__ guard swallows
            # JournalError text, so argparse owns this message.
            with self.assertRaises(SystemExit):
                self.cli("configure", "--lang", "kr")

            # Reconfiguring Notion rewrites the whole file; the pin survives.
            wj.configure_notion("0" * 32, validate=False)
            stored = json.loads(self.config.read_text(encoding="utf-8"))
            self.assertEqual("ko", stored["language"])
            self.assertEqual("0" * 32, stored["notion"]["data_source_id"])
            self.assertEqual("ko", wj.journal_language())

    def test_unreadable_config_falls_back_instead_of_failing_the_render(self):
        self.config.write_text("{damaged", encoding="utf-8")
        with self.env(LANG="ko_KR.UTF-8"):
            self.assertEqual("", wj.configured_journal_language())
            self.assertEqual("ko", wj.journal_language())
            with self.assertRaises(wj.JournalError):
                wj.set_journal_language("en")


class NotionPresentationTest(unittest.TestCase):
    def test_strips_citations_and_folds_citation_only_duplicates(self):
        content = "\n".join(
            [
                "# Daily Work Journal — 2026-08-01",
                "",
                "## 핵심 진전",
                "",
                "- Commit 0e0390aa9589: test: capture status output [wj_a4b7c0f523496f5d; source git-commit:0e0390aa; evidence git-commit:0e0390aa]",
                "",
                "## 의사결정",
                "",
                "- decided the thing [wj_0392687b9e2ed75e; source agent-task:ta[REDACTED]:update:b99491a47ec430fa; evidence agent-state:ta[REDACTED]]",
                "- decided the thing [wj_767833cf7f99e79b; source agent-task:ta[REDACTED]:close:a4026fc73de9baaa; evidence agent-state:ta[REDACTED]]",
                "- another decision [wj_a9286f0d548fa0aa; source git-commit:abc123; evidence git-commit:abc123]",
                "",
                "## 다음 우선순위",
                "",
                "- 기록 없음",
            ]
        )
        rendered = wj.notion_presentation(content)
        self.assertNotIn("wj_", rendered)
        self.assertNotIn("[REDACTED]", rendered)
        # Decision bullets trade their dash for a quote mark (the exporter
        # renders those as callouts); the fold still collapses duplicates.
        self.assertEqual(rendered.count("> decided the thing"), 1)
        self.assertNotIn("- decided the thing", rendered)
        self.assertIn("> another decision", rendered)
        self.assertIn("# Daily Work Journal — 2026-08-01", rendered)
        # Commit bullets lose their hash prefix but keep the message.
        self.assertIn("- test: capture status output", rendered)
        self.assertNotIn("Commit 0e0390aa9589", rendered)
        # Decisions float above the progress listing.
        self.assertLess(
            rendered.index("## 의사결정"), rendered.index("## 핵심 진전")
        )
        # A section that says nothing is dropped from the human view.
        self.assertNotIn("## 다음 우선순위", rendered)

    def test_drops_evidence_bullets_and_labeled_commit_prefixes(self):
        content = "\n".join(
            [
                "## 프로젝트별 작업",
                "### proj",
                "- 작업: Commit 0e0390aa9589: test: capture status output",
                "  - 관련 evidence: git-commit:0e0390aa95893b50e14bdf78d60f5c5d3090cf8d",
                "  - 결과: recorded",
                "- 작업: second item",
                "  - 결과: recorded",
            ]
        )
        rendered = wj.notion_presentation(content)
        self.assertIn("- 작업: test: capture status output", rendered)
        self.assertNotIn("Commit 0e0390aa9589", rendered)
        self.assertNotIn("관련 evidence", rendered)
        # The indented sub-bullets that are not evidence stay, and repeat as
        # often as the work items they belong to.
        self.assertEqual(2, rendered.count("  - 결과: recorded"))

    def test_dedup_spans_sections_and_follows_the_presented_order(self):
        content = "\n".join(
            [
                "## 핵심 진전",
                "- same line [wj_0392687b9e2ed75e; source x; evidence y]",
                "- progress only [wj_a9286f0d548fa0aa; source x; evidence y]",
                "## 의사결정",
                "- same line [wj_767833cf7f99e79b; source x; evidence y]",
            ]
        )
        rendered = wj.notion_presentation(content)
        # 의사결정 is presented first, so it keeps the shared bullet even
        # though the source file lists it under 핵심 진전 first — and as the
        # decision copy it carries the quote mark instead of the dash.
        self.assertEqual(1, rendered.count("> same line"))
        self.assertNotIn("- same line", rendered)
        self.assertLess(rendered.index("## 의사결정"), rendered.index("> same line"))
        self.assertLess(rendered.index("> same line"), rendered.index("## 핵심 진전"))
        self.assertIn("- progress only", rendered)

    def test_abbreviates_hashes_the_commit_prefix_never_reached(self):
        content = "\n".join(
            [
                "## 핵심 진전",
                "- CI success for commit 6bdad1f79bae90aa34f9cd7781439453b037c571"
                " [wj_a4b7c0f523496f5d; source x; evidence y]",
                "- 작업: CI failure for commit"
                " ccaec14ac6974f60f2a3a679181eacfd318f3192",
            ]
        )
        rendered = wj.notion_presentation(content)
        # A CI bullet names its commit mid-sentence, where the ``Commit
        # <hash>:`` prefix anchor never reached. It keeps a short hash rather
        # than losing it: unlike a commit bullet it has no subject to fall
        # back on.
        self.assertIn("- CI success for commit 6bdad1f", rendered)
        self.assertIn("- 작업: CI failure for commit ccaec14", rendered)
        self.assertNotIn("6bdad1f79bae", rendered)
        self.assertNotIn("ccaec14ac697", rendered)

    def test_a_folded_bullet_takes_its_detail_lines_with_it(self):
        content = "\n".join(
            [
                "## 프로젝트별 작업",
                "### proj",
                "- 작업: Commit da3661c808d3: fix: same subject twice",
                "  - 결과: success",
                "- 작업: Commit 5669cfc265b1: fix: same subject twice",
                "  - 결과: recorded",
            ]
        )
        rendered = wj.notion_presentation(content)
        # Two commits sharing a subject render identical bullets once the hash
        # prefix is stripped, so the fold drops the second. Its outcome line
        # has to leave with it: left behind, it restated the survivor's result
        # as the dropped commit's.
        self.assertEqual(1, rendered.count("- 작업: fix: same subject twice"))
        self.assertIn("  - 결과: success", rendered)
        self.assertNotIn("  - 결과: recorded", rendered)


class JudgmentCaptureTest(unittest.TestCase):
    """The fields that make a summary readable, and the nudge when they are absent."""

    def _store(self, tmp):
        repo = pathlib.Path(tmp)
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.email", "t@e.com"],
                       check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.name", "T"], check=True)
        (repo / "s.txt").write_text("seed\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(repo), "add", "s.txt"], check=True)
        subprocess.run(["git", "-C", str(repo), "commit", "-qm", "seed"], check=True)
        return repo, wj.JournalStore(repo)

    def _yesterday(self):
        return (
            dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=1)
        ).strftime("%Y-%m-%dT%H:%M:%SZ")

    def test_a_lifecycle_verb_carries_its_own_judgment_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, store = self._store(tmp)
            # A park states its blocker and the next step; an adopted contract
            # states the sentence the operator wrote. The adapter carries them
            # rather than deriving anything.
            payload = wj.source_payload(
                repo, "goal-drive", repo / "s.txt", source_id="gd-1:park",
                outcome="Goal drive parked: verifier-floor-deadlock",
                outcome_status="parked", blocker="verifier-floor-deadlock",
                next_action="inspect the typed review outcome",
            )
            self.assertEqual("verifier-floor-deadlock", payload["blocker"])
            self.assertEqual(
                "inspect the typed review outcome", payload["next_action"]
            )
            store.record_event(payload)
            store.record_event(
                wj.source_payload(
                    repo, "intent", repo / "s.txt", source_id="in-1",
                    outcome="Contract adopted: in-1", outcome_status="confirmed",
                    verification_status="not_applicable",
                    decision="adopted contract: block the sibling readers",
                )
            )
            store.materialize()
            rendered = wj.notion_presentation(
                store.summary_text("daily", store._current_periods()[0])
            )
            # All three judgment sections were permanently empty before these
            # verbs carried anything into them.
            self.assertIn("> adopted contract: block the sibling readers", rendered)
            self.assertIn("- verifier-floor-deadlock", rendered)
            self.assertIn("- inspect the typed review outcome", rendered)

    def test_the_daily_digest_names_a_day_that_recorded_no_decision(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, store = self._store(tmp)
            store.record_event(
                wj.source_payload(
                    repo, "goal-drive", repo / "s.txt", source_id="gd-quiet",
                    occurred_at=self._yesterday(),
                    outcome="Goal drive reached acceptance", outcome_status="done",
                )
            )
            store.materialize()
            self.assertIn("recorded no decision", store.prompt_digest())

    def test_the_digest_stays_quiet_when_the_day_recorded_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, store = self._store(tmp)
            store.record_event(
                wj.source_payload(
                    repo, "goal-drive", repo / "s.txt", source_id="gd-loud",
                    occurred_at=self._yesterday(),
                    outcome="Goal drive reached acceptance", outcome_status="done",
                    decision="landed the sibling-reader sweep",
                )
            )
            store.materialize()
            # The bounded recent-event descriptors drop the judgment fields, so
            # a digest reading those would call every day decision-less.
            self.assertNotIn("recorded no decision", store.prompt_digest())


if __name__ == "__main__":
    unittest.main()
