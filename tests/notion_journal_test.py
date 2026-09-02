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

    def human_exporter(self, transport, **extra):
        options = {
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
        options.update(extra)
        return notion.NotionJournalExporter(**options)

    def test_optional_human_properties_are_written_when_configured(self):
        transport = FakeTransport(
            [
                {"results": []},
                {"properties": {"Project": {"type": "rich_text"}}},
                {"id": "page-created"},
            ]
        )
        exporter = self.human_exporter(transport)
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
        properties = transport.calls[2][2]["properties"]
        self.assertEqual(
            "oh-my-setting",
            properties["Project"]["rich_text"][0]["text"]["content"],
        )
        self.assertEqual("Daily", properties["Kind"]["select"]["name"])
        self.assertEqual("2026-07-31", properties["Period"]["date"]["start"])
        self.assertTrue(properties["Has Blocker"]["checkbox"])

    def test_project_select_conversion_adapts_the_payload(self):
        transport = FakeTransport(
            [
                {"results": []},
                {"properties": {"Project": {"type": "select"}}},
                {"id": "page-created"},
            ]
        )
        exporter = self.human_exporter(transport)
        exporter.upsert(
            "key",
            "title",
            "hash",
            "content",
            project_name="team, alpha " + "x" * 200,
            kind="daily",
            period="2026-07-31",
        )
        option = transport.calls[2][2]["properties"]["Project"]["select"]["name"]
        self.assertNotIn(",", option)
        self.assertLessEqual(len(option), 100)
        self.assertTrue(option.startswith("team  alpha"))

    def test_optional_columns_follow_the_live_schema(self):
        absent = FakeTransport(
            [
                {"results": []},
                {"properties": {"Project": {"type": "rich_text"}}},
                {"id": "page-created"},
            ]
        )
        exporter = self.human_exporter(
            absent,
            sessions_property="Sessions",
            commits_property="Commits",
            verified_property="Verified",
        )
        exporter.upsert(
            "key",
            "title",
            "hash",
            "content",
            project_name="oh-my-setting",
            sessions=["sess-alpha-one", "sess-beta-two"],
            commits=3,
            verified=2,
        )
        properties = absent.calls[2][2]["properties"]
        self.assertNotIn("Sessions", properties)
        self.assertNotIn("Commits", properties)
        self.assertNotIn("Verified", properties)

        present = FakeTransport(
            [
                {"results": []},
                {
                    "properties": {
                        "Project": {"type": "rich_text"},
                        "Sessions": {"type": "rich_text"},
                        "Commits": {"type": "number"},
                        "Verified": {"type": "number"},
                    }
                },
                {"id": "page-created"},
            ]
        )
        exporter = self.human_exporter(
            present,
            sessions_property="Sessions",
            commits_property="Commits",
            verified_property="Verified",
        )
        exporter.upsert(
            "key",
            "title",
            "hash",
            "content",
            project_name="oh-my-setting",
            sessions=["sess-alpha-one", "sess-beta-two"],
            commits=3,
            verified=2,
        )
        properties = present.calls[2][2]["properties"]
        self.assertEqual(
            "sess-alp, sess-bet",
            properties["Sessions"]["rich_text"][0]["text"]["content"],
        )
        self.assertEqual(3, properties["Commits"]["number"])
        self.assertEqual(2, properties["Verified"]["number"])

    def test_schema_fetch_failure_fails_closed_before_any_write(self):
        transport = FakeTransport(
            [
                {"results": []},
                notion.NotionTransportError("schema unavailable"),
            ]
        )
        exporter = self.human_exporter(transport)
        with self.assertRaises(notion.NotionTransportError):
            exporter.upsert(
                "key", "title", "hash", "content", project_name="oh-my-setting"
            )
        for method, path, _payload, _timeout in transport.calls:
            self.assertFalse(
                method == "POST" and path == "/v1/pages",
                "no page may be written on an unknown schema",
            )

    def test_validate_target_tolerates_select_project_and_absent_optionals(self):
        schema = {
            "properties": {
                "Name": {"type": "title"},
                "Work Journal Key": {"type": "rich_text"},
                "Content Hash": {"type": "rich_text"},
                "Project": {"type": "select"},
                "Kind": {"type": "select"},
                "Period": {"type": "date"},
                "Has Blocker": {"type": "checkbox"},
            }
        }
        exporter = self.human_exporter(
            FakeTransport([schema]),
            sessions_property="Sessions",
            commits_property="Commits",
            verified_property="Verified",
        )
        self.assertEqual({"status": "valid"}, exporter.validate_target())

        wrong_typed = {
            "properties": dict(
                schema["properties"], Commits={"type": "rich_text"}
            )
        }
        exporter = self.human_exporter(
            FakeTransport([wrong_typed]),
            commits_property="Commits",
        )
        with self.assertRaises(notion.NotionTransportError):
            exporter.validate_target()

    def test_quote_lines_render_as_callout_blocks(self):
        children = notion.NotionJournalExporter._summary_children(
            "## 의사결정\n\n> going with map-style sharding\n"
        )
        callouts = [c for c in children if c["type"] == "callout"]
        self.assertEqual(1, len(callouts))
        self.assertEqual(
            "going with map-style sharding",
            callouts[0]["callout"]["rich_text"][0]["text"]["content"],
        )
        self.assertEqual("emoji", callouts[0]["callout"]["icon"]["type"])

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
                "toggle",
                "divider",
                "paragraph",
                "paragraph",
                "paragraph",
            ],
            [child["type"] for child in children],
        )
        # The progress listing nests inside the toggle in document order.
        self.assertEqual(
            ["bulleted_list_item", "to_do", "numbered_list_item"],
            [block["type"] for block in children[1]["toggle"]["children"]],
        )
        flattened = list(children)
        flattened.extend(children[1]["toggle"]["children"])
        for child in flattened:
            block = child.get(child["type"], {})
            for rich_text in block.get("rich_text", []):
                text = rich_text["text"]["content"]
                self.assertLessEqual(len(text), notion.NOTION_TEXT_CHUNK)

    def test_a_backlog_clears_under_the_caller_s_own_cap(self):
        """The per-tick cap belongs to the caller, not to one shared default.

        A session hook wants one or two summaries and a two-second budget; the
        operator's repair has to finish. Sharing the hook's bound is what made
        a backlog permanent: every tick spent its cap, recorded the rest as
        failures, and the failed count only grew.
        """
        work_journal = load_work_journal()
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-notion-backlog."))
        self.addCleanup(lambda: shutil.rmtree(tmp))
        repo = tmp / "repo"
        repo.mkdir()
        store = work_journal.JournalStore(
            repo,
            timezone_name="UTC",
            clock=lambda: dt.datetime(2026, 7, 20, 12, tzinfo=dt.timezone.utc),
            project_id="proj_backlog",
            project_name="demo",
        )
        for day in range(1, 8):
            store.record_event(
                {
                    "event_type": "validation",
                    "occurred_at": "2026-07-%02dT02:00:00Z" % day,
                    "source": {"type": "test", "id": "day-%d" % day},
                    "outcome": {"summary": "day-%d" % day, "status": "success"},
                    "verification_status": "passed",
                    "evidence": [{"type": "test", "ref": "day-%d" % day}],
                }
            )
        store.materialize()
        calls = []

        class SuccessExporter:
            def upsert(self, *args, **kwargs):
                calls.append(args[0])
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
            hook = store.sync_notion(max_per_tick=2, budget_seconds=2)
            self.assertEqual(2, hook["synced"])
            self.assertGreater(hook["remaining"], 0)
            operator = store.sync_notion(max_per_tick=20, budget_seconds=180)
            self.assertEqual(0, operator["remaining"])
            self.assertEqual(0, operator["failed"])
            # A second pass has nothing left to do: content hashes already
            # matched, so no summary is sent twice.
            again = store.sync_notion(max_per_tick=20, budget_seconds=180)
            self.assertEqual(0, again["attempted"])
        # Every finalized summary reached the remote exactly once.
        self.assertEqual(len(calls), len(set(calls)))
        self.assertGreaterEqual(len(calls), 7)

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

    def test_recent_sync_limits_daily_and_overlapping_weekly_pages(self):
        work_journal = load_work_journal()
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="oms-wj-notion-recent."))
        self.addCleanup(lambda: shutil.rmtree(tmp))
        repo = tmp / "repo"
        repo.mkdir()
        store = work_journal.JournalStore(
            repo,
            timezone_name="UTC",
            clock=lambda: dt.datetime(2026, 8, 5, 12, tzinfo=dt.timezone.utc),
            project_id="proj_recent",
            project_name="demo",
        )
        for source, occurred_at in (
            ("old-week", "2026-07-22T02:00:00Z"),
            ("outside-day", "2026-07-29T02:00:00Z"),
            ("range-start", "2026-07-30T02:00:00Z"),
            ("today", "2026-08-05T02:00:00Z"),
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
                del kwargs
                calls.append(args[0])
                return {"status": "synced", "page_id": "page-%d" % len(calls)}

        access_name = "OMS_WORK_JOURNAL_NOTION_" + "TOKEN"
        configured = {
            access_name: "test-credential",
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
            report = store.sync_notion(force=True, recent_days=7)

        self.assertEqual(4, report["synced"])
        self.assertEqual(
            {
                "proj_recent:daily:2026-07-30",
                "proj_recent:daily:2026-08-05",
                "proj_recent:weekly:2026-W31",
                "proj_recent:weekly:2026-W32",
            },
            set(calls),
        )
        with self.assertRaises(work_journal.JournalError):
            store.sync_notion(force=True, recent_days=0)

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


class HumanPageTest(unittest.TestCase):
    def test_progress_section_collapses_into_toggle(self):
        children = notion.NotionJournalExporter._summary_children(
            "## 핵심 진전\n- one\n- two\n\n## 의사결정\n- decided"
        )
        self.assertEqual("toggle", children[0]["type"])
        nested = children[0]["toggle"]["children"]
        self.assertEqual(
            ["bulleted_list_item", "bulleted_list_item"],
            [block["type"] for block in nested],
        )
        # The next section leaves the toggle: heading and bullet are top-level.
        self.assertEqual(
            ["heading_2", "bulleted_list_item"],
            [block["type"] for block in children[1:]],
        )

    def test_subsection_heading_stays_inside_the_toggle(self):
        children = notion.NotionJournalExporter._summary_children(
            "## 프로젝트별 작업\n### oh-my-setting\n- 작업: one\n\n## 의사결정\n- decided"
        )
        self.assertEqual("toggle", children[0]["type"])
        self.assertEqual(
            ["heading_3", "bulleted_list_item"],
            [block["type"] for block in children[0]["toggle"]["children"]],
        )
        self.assertEqual(
            ["heading_2", "bulleted_list_item"],
            [block["type"] for block in children[1:]],
        )

    def test_indented_details_render_under_their_work_item(self):
        children = notion.NotionJournalExporter._summary_children(
            "## 프로젝트별 작업\n"
            "- 작업: refresh token 처리\n"
            "  - 결과: success\n"
            "  - 해석: token expiry 해소\n"
        )
        listing = children[0]["toggle"]["children"]
        self.assertEqual(1, len(listing))
        parent = listing[0]
        self.assertEqual("bulleted_list_item", parent["type"])
        rich_text = parent["bulleted_list_item"]["rich_text"]
        self.assertEqual(
            [
                "작업: refresh token 처리",
                "\n↳ 결과: success",
                "\n↳ 해석: token expiry 해소",
            ],
            [item["text"]["content"] for item in rich_text],
        )

    def test_toggle_nested_overflow_is_truncated_with_a_note(self):
        lines = ["## Key progress"] + [
            "- item %d" % index
            for index in range(notion.NOTION_TOGGLE_CHILD_LIMIT + 10)
        ]
        children = notion.NotionJournalExporter._summary_children(
            "\n".join(lines)
        )
        nested = children[0]["toggle"]["children"]
        self.assertEqual(notion.NOTION_TOGGLE_CHILD_LIMIT, len(nested))
        self.assertEqual("paragraph", nested[-1]["type"])
        self.assertIn(
            "Truncated",
            nested[-1]["paragraph"]["rich_text"][0]["text"]["content"],
        )

    def test_file_keyring_mode_pins_the_credential_store(self):
        completed = mock.Mock(returncode=0, stdout='{"id":"x"}\n', stderr="")
        with mock.patch.object(notion.subprocess, "run", return_value=completed) as run:
            notion.NotionCLITransport("ntn", "2026-03-11", keyring="file").request(
                "GET", "/v1/x", None, 2.0
            )
        env = run.call_args.kwargs["env"]
        self.assertEqual("0", env["NOTION_KEYRING"])
        # Default transports must not touch the environment at all.
        with mock.patch.object(notion.subprocess, "run", return_value=completed) as run:
            notion.NotionCLITransport("ntn", "2026-03-11").request(
                "GET", "/v1/x", None, 2.0
            )
        self.assertIsNone(run.call_args.kwargs["env"])

    def test_page_icon_set_on_create(self):
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
        exporter.upsert("key", "title", "hash", "content", kind="daily")
        create_payload = transport.calls[1][2]
        self.assertEqual({"type": "emoji", "emoji": "📔"}, create_payload["icon"])
        self.assertEqual(
            {"type": "emoji", "emoji": "📚"},
            notion.NotionJournalExporter._page_icon({"kind": "weekly"}),
        )


if __name__ == "__main__":
    unittest.main()
