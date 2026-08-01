#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import datetime as dt
import email.utils
import json
import os
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "lib" / "notion_journal.py"


def load_module():
    spec = importlib.util.spec_from_file_location("notion_journal", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load notion_journal")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


notion = load_module()

WORK_JOURNAL_PATH = ROOT / "scripts" / "lib" / "work_journal.py"


def load_work_journal():
    spec = importlib.util.spec_from_file_location("work_journal_for_notion", WORK_JOURNAL_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load work_journal")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeTransport:
    def __init__(self, responses=()):
        self.responses = list(responses)
        self.calls = []

    def request(self, method, path, payload, timeout):
        self.calls.append((method, path, payload, timeout))
        if not self.responses:
            raise AssertionError("unexpected network call")
        response = self.responses.pop(0)
        if isinstance(response, BaseException):
            raise response
        return response


class NotionJournalTest(unittest.TestCase):
    def exporter(
        self,
        transport,
        sleep=None,
        retries=3,
        now=None,
        monotonic=None,
        budget=8.0,
    ):
        return notion.NotionJournalExporter(
            **{
                "to" + "ken": "secret-" + "token",
                "database_id": "database-id",
                "transport": transport,
                "sleep": sleep or (lambda _seconds: None),
                "max_attempts": retries,
                "timeout": 0.25,
                "budget_seconds": budget,
                "now": now or (lambda: 0.0),
                "monotonic": monotonic or (lambda: 0.0),
            }
        )

    def test_disabled_without_credentials_makes_zero_calls(self):
        for access_value, target in (("", ""), ("access", ""), ("", "target")):
            transport = FakeTransport()
            exporter = notion.NotionJournalExporter.from_config(
                **{
                    "to" + "ken": access_value,
                    "data_source_id": target,
                    "transport": transport,
                }
            )
            self.assertFalse(exporter.enabled)
            self.assertEqual(
                {"status": "disabled"},
                exporter.upsert("key", "title", "hash", "content"),
            )
            self.assertEqual([], transport.calls)

    def test_cli_authenticated_transport_enables_export_without_token(self):
        transport = FakeTransport([{"object": "data_source", "properties": {}}])
        exporter = notion.NotionJournalExporter.from_config(
            access_value="",
            data_source_id="target",
            auth_mode="ntn",
            transport=transport,
        )
        self.assertTrue(exporter.enabled)

    def test_notion_cli_transport_sends_json_on_stdin_without_token_argument(self):
        completed = mock.Mock(returncode=0, stdout='{"id":"page"}\n', stderr="")
        with mock.patch.object(notion.subprocess, "run", return_value=completed) as run:
            transport = notion.NotionCLITransport("ntn", "2026-03-11")
            result = transport.request(
                "POST", "/v1/pages", {"parent": {"page_id": "parent"}}, 2.0
            )
        self.assertEqual("page", result["id"])
        command = run.call_args.args[0]
        self.assertEqual("ntn", command[0])
        self.assertIn("v1/pages", command)
        self.assertNotIn("token", " ".join(command).lower())
        self.assertEqual(
            {"parent": {"page_id": "parent"}},
            json.loads(run.call_args.kwargs["input"]),
        )

    def test_discovers_unique_work_journal_data_source_by_schema(self):
        properties = {
            "Name": {"type": "title"},
            "Work Journal Key": {"type": "rich_text"},
            "Content Hash": {"type": "rich_text"},
            "Project": {"type": "rich_text"},
            "Kind": {"type": "select"},
            "Period": {"type": "date"},
            "Has Blocker": {"type": "checkbox"},
        }
        transport = FakeTransport(
            [
                {
                    "results": [
                        {"object": "data_source", "id": "journal-source"},
                        {"object": "data_source", "id": "other-source"},
                    ],
                    "has_more": False,
                    "next_cursor": None,
                },
                {"id": "journal-source", "properties": properties},
                {"id": "other-source", "properties": {"Name": {"type": "title"}}},
            ]
        )
        self.assertEqual(
            "journal-source",
            notion.discover_work_journal_data_source(transport),
        )

    def test_core_has_no_optional_sdk_import(self):
        original_import = __import__

        def guarded_import(name, *args, **kwargs):
            if name in {"notion_client", "requests"}:
                raise ImportError("optional SDK blocked")
            return original_import(name, *args, **kwargs)

        with mock.patch("builtins.__import__", side_effect=guarded_import):
            loaded = load_work_journal()
        self.assertEqual(1, loaded.SCHEMA_VERSION)

    def test_create_after_remote_key_miss(self):
        transport = FakeTransport(
            [
                {"results": []},
                {"id": "page-created"},
            ]
        )
        result = self.exporter(transport).upsert("daily:proj:2026-07-31", "Daily", "h1", "# text")
        self.assertEqual("synced", result["status"])
        self.assertEqual("page-created", result["page_id"])
        self.assertEqual("POST", transport.calls[0][0])
        self.assertIn("/query", transport.calls[0][1])
        self.assertEqual("POST", transport.calls[1][0])
        payload_text = str(transport.calls[1][2])
        self.assertNotIn("secret-token", payload_text)

    def test_current_data_source_endpoint_and_parent(self):
        transport = FakeTransport(
            [
                {"results": []},
                {"id": "page-created"},
            ]
        )
        exporter = notion.NotionJournalExporter(
            **{
                "to" + "ken": "secret-" + "token",
                "data_source_id": "data-source-id",
                "transport": transport,
                "sleep": lambda _seconds: None,
                "timeout": 0.25,
            }
        )
        exporter.upsert("key", "title", "hash", "content")
        self.assertEqual(
            "/v1/data_sources/data-source-id/query",
            transport.calls[0][1],
        )
        self.assertEqual(
            {
                "type": "data_source_id",
                "data_source_id": "data-source-id",
            },
            transport.calls[1][2]["parent"],
        )

    def test_optional_human_properties_are_written_when_configured(self):
        transport = FakeTransport([{"results": []}, {"id": "page-created"}])
        exporter = notion.NotionJournalExporter(
            **{
                "to" + "ken": "secret-" + "token",
                "data_source_id": "data-source-id",
                "transport": transport,
                "sleep": lambda _seconds: None,
                "timeout": 0.25,
                "project_property": "Project",
                "kind_property": "Kind",
                "period_property": "Period",
                "blocker_property": "Has Blocker",
            }
        )
        exporter.upsert(
            "key",
            "title",
            "hash",
            "content",
            project_name="oh-my-setting",
            kind="daily",
            period="2026-07-31",
            has_blocker=True,
        )
        properties = transport.calls[1][2]["properties"]
        self.assertEqual(
            "oh-my-setting",
            properties["Project"]["rich_text"][0]["text"]["content"],
        )
        self.assertEqual("Daily", properties["Kind"]["select"]["name"])
        self.assertEqual("2026-07-31", properties["Period"]["date"]["start"])
        self.assertTrue(properties["Has Blocker"]["checkbox"])

    def test_update_known_page(self):
        transport = FakeTransport(
            [
                {"results": [{"id": "block-1"}], "has_more": False},
                {},
                {},
                {"id": "page-1"},
            ]
        )
        result = self.exporter(transport).upsert(
            "weekly:proj:2026-W31", "Weekly", "new-hash", "# changed", page_id="page-1"
        )
        self.assertEqual("page-1", result["page_id"])
        methods = [call[0] for call in transport.calls]
        self.assertEqual(["GET", "DELETE", "PATCH", "PATCH"], methods)
        self.assertIn("properties", transport.calls[-1][2])

    def test_unchanged_is_local_noop(self):
        transport = FakeTransport()
        result = self.exporter(transport).upsert(
            "key",
            "title",
            "same",
            "content",
            page_id="page-1",
            previous_content_hash="same",
        )
        self.assertEqual("unchanged", result["status"])
        self.assertEqual([], transport.calls)

    def test_remote_lookup_can_avoid_duplicate_and_unchanged_update(self):
        transport = FakeTransport(
            [
                {
                    "results": [
                        {
                            "id": "existing",
                            "properties": {
                                "Content Hash": {
                                    "rich_text": [{"plain_text": "same"}]
                                }
                            },
                        }
                    ]
                }
            ]
        )
        result = self.exporter(transport).upsert("key", "title", "same", "content")
        self.assertEqual("unchanged", result["status"])
        self.assertEqual("existing", result["page_id"])
        self.assertEqual(1, len(transport.calls))

    def test_429_respects_retry_after(self):
        sleeps = []
        transport = FakeTransport(
            [
                notion.NotionHTTPError(429, retry_after="2"),
                {"results": []},
                {"id": "page"},
            ]
        )
        result = self.exporter(transport, sleep=sleeps.append).upsert(
            "key", "title", "hash", "content"
        )
        self.assertEqual("synced", result["status"])
        self.assertEqual([2.0], sleeps)

    def test_retry_after_http_date_is_respected(self):
        sleeps = []
        retry_at = email.utils.format_datetime(
            dt.datetime.fromtimestamp(120, tz=dt.timezone.utc), usegmt=True
        )
        transport = FakeTransport(
            [
                notion.NotionHTTPError(429, retry_after=retry_at),
                {"results": []},
                {"id": "page"},
            ]
        )
        with self.assertRaises(notion.NotionDeferredError) as raised:
            self.exporter(
                transport, sleep=sleeps.append, now=lambda: 100.0
            ).upsert("key", "title", "hash", "content")
        self.assertEqual(20.0, raised.exception.retry_after_seconds)
        self.assertEqual([], sleeps)
        self.assertEqual(1, len(transport.calls))

    def test_long_retry_after_is_deferred_to_a_later_lifecycle(self):
        sleeps = []
        transport = FakeTransport(
            [
                notion.NotionHTTPError(429, retry_after="999"),
                {"results": []},
                {"id": "page"},
            ]
        )
        with self.assertRaises(notion.NotionDeferredError) as raised:
            self.exporter(transport, sleep=sleeps.append).upsert(
                "key", "title", "hash", "content"
            )
        self.assertEqual(999.0, raised.exception.retry_after_seconds)
        self.assertEqual([], sleeps)
        self.assertEqual(1, len(transport.calls))

    def test_retryable_5xx_is_bounded(self):
        transport = FakeTransport(
            [
                notion.NotionHTTPError(503),
                notion.NotionHTTPError(503),
                notion.NotionHTTPError(503),
            ]
        )
        with self.assertRaises(notion.NotionDeferredError):
            self.exporter(transport, retries=3).upsert("key", "title", "hash", "content")
        self.assertEqual(3, len(transport.calls))

    def test_last_attempt_keeps_retry_after_for_next_lifecycle(self):
        sleeps = []
        transport = FakeTransport(
            [
                notion.NotionHTTPError(429, retry_after="1"),
                notion.NotionHTTPError(429, retry_after="1"),
                notion.NotionHTTPError(429, retry_after="1"),
            ]
        )
        with self.assertRaises(notion.NotionDeferredError) as raised:
            self.exporter(
                transport,
                retries=3,
                sleep=sleeps.append,
            ).upsert("key", "title", "hash", "content")
        self.assertEqual(1.0, raised.exception.retry_after_seconds)
        self.assertEqual([1.0, 1.0], sleeps)
        self.assertEqual(3, len(transport.calls))

    def test_timeout_is_bounded(self):
        transport = FakeTransport([TimeoutError(), TimeoutError()])
        with self.assertRaises(TimeoutError):
            self.exporter(transport, retries=2).upsert("key", "title", "hash", "content")
        self.assertEqual(2, len(transport.calls))

    def test_total_sync_budget_is_bounded(self):
        ticks = iter((0.0, 9.0))
        transport = FakeTransport()
        with self.assertRaises(TimeoutError):
            self.exporter(
                transport,
                monotonic=lambda: next(ticks),
                budget=8.0,
            ).upsert("key", "title", "hash", "content")
        self.assertEqual([], transport.calls)

    def test_retry_after_survives_remaining_budget_exhaustion(self):
        ticks = iter((0.0, 0.0, 7.5))
        sleeps = []
        transport = FakeTransport(
            [notion.NotionHTTPError(429, retry_after="2")]
        )
        with self.assertRaises(notion.NotionDeferredError) as raised:
            self.exporter(
                transport,
                monotonic=lambda: next(ticks),
                sleep=sleeps.append,
                budget=8.0,
            ).upsert("key", "title", "hash", "content")
        self.assertEqual(2.0, raised.exception.retry_after_seconds)
        self.assertEqual([], sleeps)
        self.assertEqual(1, len(transport.calls))

    def test_permanent_4xx_has_no_retry(self):
        transport = FakeTransport([notion.NotionHTTPError(400)])
        with self.assertRaises(notion.NotionHTTPError):
            self.exporter(transport).upsert("key", "title", "hash", "content")
        self.assertEqual(1, len(transport.calls))

    def test_markdown_becomes_native_bounded_blocks(self):
        transport = FakeTransport([{"results": []}, {"id": "page"}])
        content = "\n".join(
            (
                "# Daily Work Journal",
                "## 핵심 진전",
                "- first item",
                "- [x] verified",
                "1. next action",
                "---",
                "x" * 5000,
            )
        )
        self.exporter(transport).upsert("key", "title", "hash", content)
        children = transport.calls[-1][2]["children"]
        self.assertEqual(
            [
                "heading_1",
                "heading_2",
                "bulleted_list_item",
                "to_do",
                "numbered_list_item",
                "divider",
                "paragraph",
                "paragraph",
                "paragraph",
            ],
            [child["type"] for child in children],
        )
        for child in children:
            block = child.get(child["type"], {})
            for rich_text in block.get("rich_text", []):
                text = rich_text["text"]["content"]
                self.assertLessEqual(len(text), notion.NOTION_TEXT_CHUNK)

    def test_default_sync_only_exports_closed_periods_and_force_exports_live(self):
        work_journal = load_work_journal()
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-notion-policy."))
        self.addCleanup(lambda: shutil.rmtree(tmp))
        repo = tmp / "repo"
        repo.mkdir()
        store = work_journal.JournalStore(
            repo,
            timezone_name="UTC",
            clock=lambda: dt.datetime(2026, 7, 31, 12, tzinfo=dt.timezone.utc),
            project_id="proj_policy",
            project_name="demo",
        )
        for source, occurred_at in (
            ("closed", "2026-07-30T02:00:00Z"),
            ("live", "2026-07-31T02:00:00Z"),
        ):
            store.record_event(
                {
                    "event_type": "validation",
                    "occurred_at": occurred_at,
                    "source": {"type": "test", "id": source},
                    "outcome": {"summary": source, "status": "success"},
                    "verification_status": "passed",
                    "evidence": [{"type": "test", "ref": source}],
                }
            )
        store.materialize()
        calls = []

        class SuccessExporter:
            def upsert(self, *args, **kwargs):
                calls.append((args, kwargs))
                return {"status": "synced", "page_id": "page-%d" % len(calls)}

        access_name = "OMS_WORK_JOURNAL_NOTION_" + "TOKEN"
        configured = {
            access_name: "test-" + "credential",
            "OMS_WORK_JOURNAL_NOTION_DATABASE_ID": "database",
        }
        sys.path.insert(0, str(ROOT / "scripts" / "lib"))
        self.addCleanup(lambda: sys.path.remove(str(ROOT / "scripts" / "lib")))
        with mock.patch.dict(os.environ, configured, clear=False), mock.patch.dict(
            sys.modules, {"notion_journal": notion}
        ), mock.patch.object(
            notion.NotionJournalExporter,
            "from_config",
            return_value=SuccessExporter(),
        ):
            store.sync_notion()
            self.assertEqual(
                ["proj_policy:daily:2026-07-30"],
                [call[0][0] for call in calls],
            )
            store.sync_notion(force=True)

        self.assertEqual(
            {
                "proj_policy:daily:2026-07-30",
                "proj_policy:daily:2026-07-31",
                "proj_policy:weekly:2026-W31",
            },
            {call[0][0] for call in calls},
        )

    def test_coordinator_materializes_before_remote_and_retries_pending(self):
        work_journal = load_work_journal()
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-notion."))
        self.addCleanup(lambda: shutil.rmtree(tmp))
        repo = tmp / "repo"
        repo.mkdir()
        store = work_journal.JournalStore(
            repo,
            timezone_name="UTC",
            clock=lambda: dt.datetime(2026, 7, 31, 3, tzinfo=dt.timezone.utc),
            project_id="proj_notion",
            project_name="demo",
        )
        store.record_event(
            {
                "event_type": "validation",
                "occurred_at": "2026-07-31T02:00:00Z",
                "source": {"type": "test", "id": "validation-1"},
                "outcome": {"summary": "validation passed", "status": "success"},
                "verification_status": "passed",
                "evidence": [{"type": "test", "ref": "validation-1"}],
                "decision": "keep pass" + "word=hunter4 out of every export",
            }
        )
        store.materialize()
        daily = store.daily_dir / "2026-07-31.md"
        local_before = daily.read_bytes()
        calls = []
        testcase = self

        class TimeoutExporter:
            def upsert(self, *args, **kwargs):
                del self, args, kwargs
                testcase.assertTrue(daily.is_file())
                calls.append("timeout")
                raise TimeoutError()

        class SuccessExporter:
            def upsert(self, *args, **kwargs):
                calls.append((args, kwargs))
                return {"status": "synced", "page_id": "page-1"}

        access_name = "OMS_WORK_JOURNAL_NOTION_" + "TOKEN"
        configured = {
            access_name: "test-" + "credential",
            "OMS_WORK_JOURNAL_NOTION_DATABASE_ID": "database",
        }
        sys.path.insert(0, str(ROOT / "scripts" / "lib"))
        self.addCleanup(lambda: sys.path.remove(str(ROOT / "scripts" / "lib")))
        with mock.patch.dict(os.environ, configured, clear=False), mock.patch.dict(
            sys.modules, {"notion_journal": notion}
        ):
            with mock.patch.object(
                notion.NotionJournalExporter,
                "from_config",
                return_value=TimeoutExporter(),
            ):
                store.sync_notion(force=True)
            self.assertEqual(local_before, daily.read_bytes())
            state = json.loads(store.notion_state_path.read_text(encoding="utf-8"))
            entry = next(iter(state["summaries"].values()))
            self.assertEqual("failed", entry["status"])
            self.assertNotIn(configured[access_name], json.dumps(state))

            with mock.patch.object(
                notion.NotionJournalExporter,
                "from_config",
                return_value=SuccessExporter(),
            ):
                store.sync_notion(force=True)
                state = json.loads(store.notion_state_path.read_text(encoding="utf-8"))
                entry = next(iter(state["summaries"].values()))
                self.assertEqual("synced", entry["status"])
                self.assertEqual("page-1", entry["page_id"])
                exported = json.dumps(calls, ensure_ascii=False)
                self.assertNotIn("hunter4", exported)
                self.assertNotIn(configured[access_name], exported)
                call_count = len(calls)
                store.sync_notion(force=True)
                self.assertEqual(call_count, len(calls))
                os.environ["OMS_WORK_JOURNAL_NOTION_DATABASE_ID"] = "database-2"
                store.sync_notion(force=True)
                self.assertGreater(len(calls), call_count)
                state = json.loads(store.notion_state_path.read_text(encoding="utf-8"))
                self.assertNotIn("database-2", json.dumps(state))

    def test_coordinator_defers_long_retry_after_until_due(self):
        work_journal = load_work_journal()
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-notion-defer."))
        self.addCleanup(lambda: shutil.rmtree(tmp))
        repo = tmp / "repo"
        repo.mkdir()
        clock = [dt.datetime(2026, 7, 31, 3, tzinfo=dt.timezone.utc)]
        store = work_journal.JournalStore(
            repo,
            timezone_name="UTC",
            clock=lambda: clock[0],
            project_id="proj_defer",
            project_name="demo",
        )
        store.record_event(
            {
                "event_type": "validation",
                "occurred_at": "2026-07-31T02:00:00Z",
                "source": {"type": "test", "id": "defer-1"},
                "outcome": {"summary": "local first", "status": "success"},
                "verification_status": "passed",
                "evidence": [{"type": "test", "ref": "defer-1"}],
            }
        )
        store.materialize()
        calls = []

        class DeferredExporter:
            def upsert(self, *args, **kwargs):
                del self, args, kwargs
                calls.append("deferred")
                raise notion.NotionDeferredError(429, 20)

        class SuccessExporter:
            def upsert(self, *args, **kwargs):
                del self, args, kwargs
                calls.append("synced")
                return {"status": "synced", "page_id": "page-deferred"}

        access_name = "OMS_WORK_JOURNAL_NOTION_" + "TOKEN"
        configured = {
            access_name: "test-" + "credential",
            "OMS_WORK_JOURNAL_NOTION_DATA_SOURCE_ID": "",
            "OMS_WORK_JOURNAL_NOTION_DATABASE_ID": "database",
        }
        sys.path.insert(0, str(ROOT / "scripts" / "lib"))
        self.addCleanup(lambda: sys.path.remove(str(ROOT / "scripts" / "lib")))
        with mock.patch.dict(os.environ, configured, clear=False), mock.patch.dict(
            sys.modules, {"notion_journal": notion}
        ):
            with mock.patch.object(
                notion.NotionJournalExporter,
                "from_config",
                return_value=DeferredExporter(),
            ):
                store.sync_notion(force=True)
            state = json.loads(store.notion_state_path.read_text(encoding="utf-8"))
            self.assertTrue(
                all(
                    row["status"] == "pending" and row.get("next_retry_at")
                    for row in state["summaries"].values()
                )
            )

            with mock.patch.object(
                notion.NotionJournalExporter,
                "from_config",
                return_value=SuccessExporter(),
            ):
                store.sync_notion(force=True)
                self.assertNotIn("synced", calls)
                clock[0] += dt.timedelta(seconds=21)
                store.sync_notion(force=True)
            self.assertIn("synced", calls)


if __name__ == "__main__":
    unittest.main()
