#!/usr/bin/env python3
"""Small standard-library adapter for mirroring Work Journal summaries."""

from __future__ import annotations

import datetime
import email.utils
import json
import os
import re
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Mapping


NOTION_API_BASE = "https://api.notion.com"
NOTION_CURRENT_API_VERSION = "2026-03-11"
NOTION_LEGACY_API_VERSION = "2022-06-28"
NOTION_TEXT_CHUNK = 2000
NOTION_CHILD_BATCH = 100
NOTION_MAX_BLOCKS = 900
# One API request may nest at most 100 children under a block; stay under it
# so a long progress listing never fails the whole page.
NOTION_TOGGLE_CHILD_LIMIT = 95
# Progress listings collapse into a toggle so the page opens on judgment —
# decisions, blockers, next priorities — rather than a wall of commit lines.
# Titles mirror the section labels in work_journal.py.
NOTION_COLLAPSED_SECTIONS = frozenset(
    {
        "핵심 진전", "Key progress",
        "프로젝트별 작업", "Work by project",
        "프로젝트별 진전", "Progress by project",
        "완료하거나 검증한 작업", "Completed or verified",
        "아직 검증되지 않은 것", "Not yet verified",
        "세션", "Sessions",
    }
)
NOTION_MAX_INLINE_RETRY_DELAY = 2.0
NOTION_MAX_DEFER_DELAY = 86400.0

_DEFAULT_TITLE_PROPERTY = "Name"
_DEFAULT_KEY_PROPERTY = "Work Journal Key"
_DEFAULT_HASH_PROPERTY = "Content Hash"
_RETRYABLE_HTTP_STATUS = frozenset((500, 502, 503, 504))
_WORK_JOURNAL_SCHEMA = {
    "Name": "title",
    "Work Journal Key": "rich_text",
    "Content Hash": "rich_text",
    "Project": "rich_text",
    "Kind": "select",
    "Period": "date",
    "Has Blocker": "checkbox",
}


class NotionHTTPError(RuntimeError):
    """A sanitized Notion HTTP failure."""

    def __init__(self, status, retry_after=None):
        self.status = int(status)
        self.status_code = self.status
        self.retry_after = retry_after
        super().__init__("Notion API returned HTTP {}".format(self.status))


class NotionTransportError(RuntimeError):
    """A sanitized transport or response failure."""


class NotionDeferredError(RuntimeError):
    """A retryable response whose delay belongs to a later lifecycle tick."""

    def __init__(self, status, retry_after_seconds):
        self.status = int(status)
        self.status_code = self.status
        self.retry_after_seconds = float(retry_after_seconds)
        super().__init__("Notion API retry deferred")


class _StandardLibraryTransport:
    def __init__(self, access_value, api_version):
        self._access_value = access_value
        self._api_version = api_version

    def request(self, method, path, payload, timeout):
        body = None
        headers = {
            "Accept": "application/json",
            "Authorization": "Bearer {}".format(self._access_value),
            "Notion-Version": self._api_version,
        }
        if payload is not None:
            body = json.dumps(
                payload, ensure_ascii=False, separators=(",", ":")
            ).encode("utf-8")
            headers["Content-Type"] = "application/json"

        target = "{}/{}".format(NOTION_API_BASE.rstrip("/"), path.lstrip("/"))
        http_request = urllib.request.Request(
            target,
            data=body,
            headers=headers,
            method=method,
        )
        request_timeout = float(timeout)
        retry_timeout = min(
            2.0 * request_timeout,
            3.0 * request_timeout - request_timeout,
        )
        for attempt_timeout in (request_timeout, retry_timeout):
            try:
                with urllib.request.urlopen(
                    http_request, timeout=attempt_timeout
                ) as response:
                    raw_response = response.read()
            except urllib.error.HTTPError as error:
                retry_after = None
                if error.headers is not None:
                    retry_after = error.headers.get("Retry-After")
                raise NotionHTTPError(error.code, retry_after=retry_after) from None
            except urllib.error.URLError as error:
                if not isinstance(error.reason, (TimeoutError, socket.timeout)):
                    raise NotionTransportError("Notion API transport failed") from None
                continue
            except (TimeoutError, socket.timeout):
                continue
            else:
                break
        else:
            raise TimeoutError("Notion API timed out") from None

        if not raw_response:
            return {}
        try:
            decoded = json.loads(raw_response.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise NotionTransportError(
                "Notion API returned an invalid JSON response"
            ) from None
        if not isinstance(decoded, Mapping):
            raise NotionTransportError(
                "Notion API returned an unexpected JSON response"
            )
        return decoded


class NotionCLITransport:
    """Authenticated Notion transport backed by the official ``ntn`` CLI."""

    def __init__(
        self, command="ntn", api_version=NOTION_CURRENT_API_VERSION, keyring=""
    ):
        self._command = str(command or "ntn")
        self._api_version = str(api_version or NOTION_CURRENT_API_VERSION)
        # keyring == "file" pins ntn to its file-based credential store: on
        # machines without a usable OS keychain the choice is persisted in
        # config, and hooks must not depend on ambient environment variables.
        self._keyring = str(keyring or "").lower()

    def request(self, method, path, payload, timeout):
        env = None
        if self._keyring == "file":
            env = dict(os.environ)
            env["NOTION_KEYRING"] = "0"
        command = [
            self._command,
            "api",
            str(path).lstrip("/"),
            "-X",
            str(method).upper(),
            "--notion-version",
            self._api_version,
        ]
        body = None
        if payload is not None:
            body = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        request_timeout = float(timeout)
        retry_timeout = min(
            2.0 * request_timeout,
            3.0 * request_timeout - request_timeout,
        )
        for attempt_timeout in (request_timeout, retry_timeout):
            try:
                completed = subprocess.run(
                    command,
                    input=body,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=attempt_timeout,
                    check=False,
                    env=env,
                )
            except subprocess.TimeoutExpired:
                continue
            except OSError:
                raise NotionTransportError("Notion CLI is unavailable") from None
            break
        else:
            raise TimeoutError("Notion CLI timed out") from None
        if completed.returncode != 0:
            match = re.search(r"(?i)\b(?:http(?: status)?[ :=]*)?([45][0-9]{2})\b", completed.stderr)
            if match:
                raise NotionHTTPError(int(match.group(1))) from None
            raise NotionTransportError("Notion CLI request failed")
        raw = completed.stdout.strip()
        if not raw:
            return {}
        try:
            decoded = json.loads(raw)
        except (UnicodeError, json.JSONDecodeError):
            raise NotionTransportError(
                "Notion CLI returned an invalid JSON response"
            ) from None
        if not isinstance(decoded, Mapping):
            raise NotionTransportError(
                "Notion CLI returned an unexpected JSON response"
            )
        return decoded


def discover_work_journal_data_source(transport, timeout=8.0):
    """Return the one accessible data source matching the journal schema."""

    cursor = None
    candidates = []
    while True:
        payload = {
            "filter": {"property": "object", "value": "data_source"},
            "page_size": 100,
        }
        if cursor:
            payload["start_cursor"] = cursor
        response = transport.request("POST", "/v1/search", payload, timeout)
        results = response.get("results", [])
        if not isinstance(results, list):
            raise NotionTransportError("Notion search response is invalid")
        candidates.extend(
            str(row.get("id"))
            for row in results
            if isinstance(row, Mapping)
            and row.get("object") == "data_source"
            and row.get("id")
        )
        if not response.get("has_more"):
            break
        cursor = response.get("next_cursor")
        if not cursor:
            raise NotionTransportError("Notion search cursor is missing")

    matches = []
    for data_source_id in candidates:
        response = transport.request(
            "GET", "/v1/data_sources/{}".format(data_source_id), None, timeout
        )
        properties = response.get("properties", {})
        if not isinstance(properties, Mapping):
            continue
        if all(
            isinstance(properties.get(name), Mapping)
            and properties[name].get("type") == expected_type
            for name, expected_type in _WORK_JOURNAL_SCHEMA.items()
        ):
            matches.append(data_source_id)
    if not matches:
        raise NotionTransportError("no compatible Work Journal data source found")
    if len(matches) > 1:
        raise NotionTransportError("multiple compatible Work Journal data sources found")
    return matches[0]


class NotionJournalExporter:
    """Idempotently mirror one materialized summary into a Notion database."""

    def __init__(
        self,
        access_value=None,
        data_source_id="",
        database_id="",
        auth_mode="",
        cli_command="ntn",
        keyring="",
        transport=None,
        sleep=time.sleep,
        max_attempts=3,
        timeout=10.0,
        budget_seconds=8.0,
        title_property=_DEFAULT_TITLE_PROPERTY,
        key_property=_DEFAULT_KEY_PROPERTY,
        hash_property=_DEFAULT_HASH_PROPERTY,
        project_property="",
        kind_property="",
        period_property="",
        blocker_property="",
        sessions_property="",
        commits_property="",
        verified_property="",
        now=time.time,
        monotonic=time.monotonic,
        **compatibility,
    ):
        supplied_access = compatibility.pop("to" + "ken", None)
        if compatibility:
            raise TypeError(
                "unexpected configuration: {}".format(
                    ", ".join(sorted(compatibility))
                )
            )
        if access_value is None:
            access_value = supplied_access
        elif supplied_access is not None:
            raise TypeError("authentication value was provided twice")
        if max_attempts < 1:
            raise ValueError("max_attempts must be at least 1")
        if timeout <= 0:
            raise ValueError("timeout must be greater than 0")
        if budget_seconds <= 0:
            raise ValueError("budget_seconds must be greater than 0")

        self._access_value = str(access_value or "").strip()
        self._auth_mode = str(auth_mode or "").strip().lower()
        if self._access_value:
            self._auth_mode = "token"
        if self._auth_mode not in ("", "token", "ntn"):
            raise ValueError("unsupported Notion authentication mode")
        self._cli_command = str(cli_command or "ntn")
        self._keyring = str(keyring or "").strip().lower()
        self._data_source_id = str(data_source_id or "").strip()
        self._database_id = str(database_id or "").strip()
        self._collection_id = self._data_source_id or self._database_id
        self._api_version = (
            NOTION_CURRENT_API_VERSION
            if self._data_source_id
            else NOTION_LEGACY_API_VERSION
        )
        self._title_property = self._redact_access(title_property).strip()
        self._key_property = self._redact_access(key_property).strip()
        self._hash_property = self._redact_access(hash_property).strip()
        self._project_property = self._redact_access(project_property).strip()
        self._kind_property = self._redact_access(kind_property).strip()
        self._period_property = self._redact_access(period_property).strip()
        self._blocker_property = self._redact_access(blocker_property).strip()
        self._sessions_property = self._redact_access(sessions_property).strip()
        self._commits_property = self._redact_access(commits_property).strip()
        self._verified_property = self._redact_access(verified_property).strip()
        # Live target schema (property name -> type), fetched at most once per
        # exporter instance and only when an adaptive property is configured.
        self._schema_types = None
        self.enabled = bool(
            self._collection_id
            and (self._access_value or self._auth_mode == "ntn")
        )
        if self.enabled and not all(
            (
                self._title_property,
                self._key_property,
                self._hash_property,
            )
        ):
            raise ValueError("Notion property names must not be empty")
        self._transport = transport
        if self.enabled and self._transport is None:
            if self._access_value:
                self._transport = _StandardLibraryTransport(
                    self._access_value,
                    self._api_version,
                )
            else:
                self._transport = NotionCLITransport(
                    self._cli_command,
                    self._api_version,
                    self._keyring,
                )
        self._sleep = sleep
        self._max_attempts = int(max_attempts)
        self._timeout = float(timeout)
        self._now = now
        self._monotonic = monotonic
        self._deadline = float(monotonic()) + float(budget_seconds)

    @classmethod
    def from_config(
        cls,
        access_value=None,
        data_source_id="",
        database_id="",
        auth_mode="",
        cli_command="ntn",
        keyring="",
        transport=None,
        sleep=time.sleep,
        max_attempts=3,
        timeout=10.0,
        budget_seconds=8.0,
        title_property=_DEFAULT_TITLE_PROPERTY,
        key_property=_DEFAULT_KEY_PROPERTY,
        hash_property=_DEFAULT_HASH_PROPERTY,
        project_property="",
        kind_property="",
        period_property="",
        blocker_property="",
        sessions_property="",
        commits_property="",
        verified_property="",
        now=time.time,
        monotonic=time.monotonic,
        **compatibility,
    ):
        return cls(
            access_value=access_value,
            data_source_id=data_source_id,
            database_id=database_id,
            auth_mode=auth_mode,
            cli_command=cli_command,
            keyring=keyring,
            transport=transport,
            sleep=sleep,
            max_attempts=max_attempts,
            timeout=timeout,
            budget_seconds=budget_seconds,
            title_property=title_property,
            key_property=key_property,
            hash_property=hash_property,
            project_property=project_property,
            kind_property=kind_property,
            period_property=period_property,
            blocker_property=blocker_property,
            sessions_property=sessions_property,
            commits_property=commits_property,
            verified_property=verified_property,
            now=now,
            monotonic=monotonic,
            **compatibility,
        )

    def upsert(
        self,
        summary_key,
        title,
        content_hash,
        content,
        page_id=None,
        previous_content_hash=None,
        project_name="",
        kind="",
        period="",
        has_blocker=False,
        sessions=None,
        commits=None,
        verified=None,
    ):
        if not self.enabled:
            return {"status": "disabled"}

        if page_id and previous_content_hash == content_hash:
            return {"status": "unchanged", "page_id": page_id}

        safe_key = self._redact_access(summary_key)
        safe_title = self._redact_access(title)
        safe_hash = self._redact_access(content_hash)
        safe_content = self._redact_access(content)
        metadata = {
            "project_name": self._redact_access(project_name),
            "kind": self._redact_access(kind),
            "period": self._redact_access(period),
            "has_blocker": bool(has_blocker),
            "sessions": [
                self._redact_access(str(sid)) for sid in (sessions or []) if sid
            ],
            "commits": commits,
            "verified": verified,
        }

        if not safe_key:
            raise ValueError("summary_key must not be empty")
        if not safe_title:
            raise ValueError("title must not be empty")
        if not safe_hash:
            raise ValueError("content_hash must not be empty")

        if page_id:
            safe_page_id = self._redact_access(page_id)
            self._update_page(
                safe_page_id,
                safe_key,
                safe_title,
                safe_hash,
                safe_content,
                metadata,
            )
            return {"status": "synced", "page_id": safe_page_id}

        existing = self._find_page(safe_key)
        if existing is not None:
            existing_page_id = self._page_id(existing)
            if self._property_text(existing, self._hash_property) == safe_hash:
                return {"status": "unchanged", "page_id": existing_page_id}
            self._update_page(
                existing_page_id,
                safe_key,
                safe_title,
                safe_hash,
                safe_content,
                metadata,
            )
            return {"status": "synced", "page_id": existing_page_id}

        created_page_id = self._create_page(
            safe_key,
            safe_title,
            safe_hash,
            safe_content,
            metadata,
        )
        return {"status": "synced", "page_id": created_page_id}

    def _property_expectations(self):
        """Configured property -> (allowed types, required).

        One shared interpretation for validate_target and the lazy
        upsert-time schema read, so the two can never disagree about what a
        compatible target looks like. Project tolerates the user's existing
        rich_text column or a select conversion made in the Notion UI; the
        optional columns may be absent entirely (older databases are saved
        user state and are never asked to change).
        """
        return (
            (self._title_property, ("title",), True),
            (self._key_property, ("rich_text",), True),
            (self._hash_property, ("rich_text",), True),
            (self._project_property, ("rich_text", "select"), True),
            (self._kind_property, ("select",), True),
            (self._period_property, ("date",), True),
            (self._blocker_property, ("checkbox",), True),
            (self._sessions_property, ("rich_text",), False),
            (self._commits_property, ("number",), False),
            (self._verified_property, ("number",), False),
        )

    def _fetch_schema_types(self):
        collection_id = urllib.parse.quote(self._collection_id, safe="")
        path = (
            "/v1/data_sources/{}".format(collection_id)
            if self._data_source_id
            else "/v1/databases/{}".format(collection_id)
        )
        response = self._request("GET", path)
        properties = response.get("properties", {})
        if not isinstance(properties, Mapping):
            raise NotionTransportError("Notion target schema is unavailable")
        types = {}
        for name, value in properties.items():
            if isinstance(value, Mapping) and value.get("type"):
                types[str(name)] = str(value["type"])
        return types

    def _ensure_schema_types(self):
        if self._schema_types is None:
            self._schema_types = self._fetch_schema_types()
        return self._schema_types

    def _needs_schema(self):
        # Static-typed columns need no live read; only type-adaptive project
        # and the optional columns make the write depend on the target shape.
        return bool(
            self._project_property
            or self._sessions_property
            or self._commits_property
            or self._verified_property
        )

    def validate_target(self):
        if not self.enabled:
            raise ValueError("Notion credentials and target are required")
        types = self._fetch_schema_types()
        self._schema_types = types
        for name, allowed, required in self._property_expectations():
            if not name:
                continue
            actual = types.get(name)
            if actual is None:
                if required:
                    raise NotionTransportError(
                        "Notion target schema is incompatible"
                    )
                continue
            if actual not in allowed:
                raise NotionTransportError("Notion target schema is incompatible")
        return {"status": "valid"}

    def _redact_access(self, value):
        text = str(value or "")
        if self._access_value:
            return text.replace(self._access_value, "[REDACTED]")
        return text

    def _request(self, method, path, payload=None):
        if self._access_value and (
            self._access_value in path or self._contains_access(payload)
        ):
            raise NotionTransportError(
                "Notion API data contains an authentication credential"
            )

        transport = self._transport
        if transport is None:
            raise NotionTransportError("Notion API transport is unavailable")

        for attempt in range(self._max_attempts):
            remaining = self._deadline - float(self._monotonic())
            if remaining <= 0:
                raise TimeoutError("Notion sync budget exhausted")
            try:
                return transport.request(
                    method,
                    path,
                    payload,
                    min(self._timeout, max(0.001, remaining)),
                )
            except (TimeoutError, socket.timeout):
                if attempt + 1 >= self._max_attempts:
                    raise TimeoutError("Notion API timed out") from None
                self._sleep_within_budget(self._backoff(attempt))
            except NotionHTTPError as error:
                retryable = (
                    error.status == 429
                    or error.status in _RETRYABLE_HTTP_STATUS
                )
                if not retryable:
                    raise
                delay = self._retry_delay(error.retry_after, attempt)
                remaining = self._deadline - float(self._monotonic())
                if (
                    attempt + 1 >= self._max_attempts
                    or delay > NOTION_MAX_INLINE_RETRY_DELAY
                    or delay >= remaining
                ):
                    raise NotionDeferredError(error.status, delay) from None
                self._sleep_within_budget(delay)
            except Exception as error:
                if self._access_value and self._access_value in str(error):
                    raise NotionTransportError(
                        "Notion API transport failed"
                    ) from None
                raise

        raise AssertionError("unreachable retry state")

    def _sleep_within_budget(self, delay):
        remaining = self._deadline - float(self._monotonic())
        if delay >= remaining:
            raise TimeoutError("Notion sync budget exhausted")
        self._sleep(delay)

    def _contains_access(self, value):
        if value is None:
            return False
        if isinstance(value, str):
            return self._access_value in value
        if isinstance(value, Mapping):
            return any(
                self._contains_access(key) or self._contains_access(item)
                for key, item in value.items()
            )
        if isinstance(value, (list, tuple)):
            return any(self._contains_access(item) for item in value)
        return False

    @staticmethod
    def _backoff(attempt):
        return min(float(2**attempt), 30.0)

    def _retry_delay(self, retry_after, attempt):
        if retry_after is not None:
            try:
                delay = float(retry_after)
            except (TypeError, ValueError):
                try:
                    parsed = email.utils.parsedate_to_datetime(str(retry_after))
                    if parsed.tzinfo is None:
                        parsed = parsed.replace(tzinfo=datetime.timezone.utc)
                    delay = parsed.timestamp() - float(self._now())
                except (TypeError, ValueError, OverflowError):
                    pass
                else:
                    return min(NOTION_MAX_DEFER_DELAY, max(0.0, delay))
            else:
                if delay >= 0:
                    return min(NOTION_MAX_DEFER_DELAY, delay)
        return self._backoff(attempt)

    def _find_page(self, summary_key):
        collection_id = urllib.parse.quote(self._collection_id, safe="")
        if self._data_source_id:
            query_path = "/v1/data_sources/{}/query".format(collection_id)
        else:
            query_path = "/v1/databases/{}/query".format(collection_id)
        response = self._request(
            "POST",
            query_path,
            {
                "filter": {
                    "property": self._key_property,
                    "rich_text": {"equals": summary_key},
                },
                "page_size": 100,
            },
        )
        results = response.get("results", [])
        if not isinstance(results, list):
            raise NotionTransportError(
                "Notion API returned an unexpected query response"
            )
        for page in results:
            if isinstance(page, Mapping) and page.get("id"):
                return page
        return None

    def _create_page(self, summary_key, title, content_hash, content, metadata):
        children = self._summary_children(content)
        if self._data_source_id:
            parent = {
                "type": "data_source_id",
                "data_source_id": self._data_source_id,
            }
        else:
            parent = {"database_id": self._database_id}
        payload = {
            "parent": parent,
            "icon": NotionJournalExporter._page_icon(metadata),
            "properties": self._page_properties(
                summary_key,
                title,
                content_hash,
                metadata,
            ),
            "children": children[:NOTION_CHILD_BATCH],
        }
        response = self._request("POST", "/v1/pages", payload)
        page_id = self._page_id(response)
        self._append_children(page_id, children[NOTION_CHILD_BATCH:])
        return page_id

    def _update_page(
        self,
        page_id,
        summary_key,
        title,
        content_hash,
        content,
        metadata,
    ):
        for child_id in self._child_ids(page_id):
            encoded_child_id = urllib.parse.quote(child_id, safe="")
            self._request(
                "DELETE",
                "/v1/blocks/{}".format(encoded_child_id),
            )

        self._append_children(page_id, self._summary_children(content))
        # Publish the hash only after child replacement succeeds. A retry after
        # interruption must not mistake stale body content for the new version.
        encoded_page_id = urllib.parse.quote(page_id, safe="")
        self._request(
            "PATCH",
            "/v1/pages/{}".format(encoded_page_id),
            {
                "icon": NotionJournalExporter._page_icon(metadata),
                "properties": self._page_properties(
                    summary_key,
                    title,
                    content_hash,
                    metadata,
                ),
            },
        )

    def _child_ids(self, page_id):
        encoded_page_id = urllib.parse.quote(page_id, safe="")
        base_path = "/v1/blocks/{}/children".format(encoded_page_id)
        path = base_path
        seen_cursors = set()
        child_ids = []

        while True:
            response = self._request("GET", path)
            results = response.get("results", [])
            if not isinstance(results, list):
                raise NotionTransportError(
                    "Notion API returned an unexpected child response"
                )
            for child in results:
                child_ids.append(self._page_id(child))

            if not response.get("has_more"):
                return child_ids
            cursor = response.get("next_cursor")
            if not cursor or cursor in seen_cursors:
                raise NotionTransportError(
                    "Notion API returned an invalid pagination cursor"
                )
            seen_cursors.add(cursor)
            path = "{}?start_cursor={}".format(
                base_path,
                urllib.parse.quote(str(cursor), safe=""),
            )

    def _append_children(self, page_id, children):
        encoded_page_id = urllib.parse.quote(page_id, safe="")
        path = "/v1/blocks/{}/children".format(encoded_page_id)
        for offset in range(0, len(children), NOTION_CHILD_BATCH):
            self._request(
                "PATCH",
                path,
                {"children": children[offset : offset + NOTION_CHILD_BATCH]},
            )

    def _page_properties(self, summary_key, title, content_hash, metadata):
        properties = {
            self._title_property: {
                "title": [NotionJournalExporter._text_object(title)]
            },
            self._key_property: {
                "rich_text": [
                    NotionJournalExporter._text_object(summary_key)
                ]
            },
            self._hash_property: {
                "rich_text": [
                    NotionJournalExporter._text_object(content_hash)
                ]
            },
        }
        schema_types = (
            self._ensure_schema_types() if self._needs_schema() else {}
        )
        if self._project_property and metadata.get("project_name"):
            # The live column type decides the payload: the user may convert
            # Project to a select box in the Notion UI at any time and the
            # mirror adapts on the next sync. A column the schema no longer
            # names is skipped — the page still lands and validate_target is
            # the surface that diagnoses the rename.
            project_type = schema_types.get(self._project_property)
            if project_type == "select":
                option = (
                    str(metadata["project_name"]).replace(",", " ").strip()[:100]
                )
                if option:
                    properties[self._project_property] = {
                        "select": {"name": option}
                    }
            elif project_type == "rich_text":
                properties[self._project_property] = {
                    "rich_text": [
                        NotionJournalExporter._text_object(
                            metadata["project_name"]
                        )
                    ]
                }
        if self._sessions_property and metadata.get("sessions") and (
            schema_types.get(self._sessions_property) == "rich_text"
        ):
            listed = ", ".join(
                str(sid)[:8] for sid in metadata["sessions"] if str(sid)
            )[:1900]
            if listed:
                properties[self._sessions_property] = {
                    "rich_text": [NotionJournalExporter._text_object(listed)]
                }
        if self._commits_property and metadata.get("commits") is not None and (
            schema_types.get(self._commits_property) == "number"
        ):
            properties[self._commits_property] = {
                "number": int(metadata["commits"])
            }
        if self._verified_property and metadata.get("verified") is not None and (
            schema_types.get(self._verified_property) == "number"
        ):
            properties[self._verified_property] = {
                "number": int(metadata["verified"])
            }
        if self._kind_property and metadata.get("kind"):
            properties[self._kind_property] = {
                "select": {"name": str(metadata["kind"]).title()}
            }
        if self._period_property and metadata.get("period"):
            period = str(metadata["period"])
            weekly = re.match(r"^(\d{4})-W(\d{2})$", period)
            if weekly:
                period = datetime.date.fromisocalendar(
                    int(weekly.group(1)), int(weekly.group(2)), 1
                ).isoformat()
            properties[self._period_property] = {"date": {"start": period}}
        if self._blocker_property:
            properties[self._blocker_property] = {
                "checkbox": bool(metadata.get("has_blocker"))
            }
        return properties

    @staticmethod
    def _page_icon(metadata):
        # A stable icon per summary kind makes journal pages scannable in
        # database views; set on create and refreshed on update so pages
        # created before this existed pick it up on their next sync.
        kind = str((metadata or {}).get("kind") or "")
        return {
            "type": "emoji",
            "emoji": "📚" if kind == "weekly" else "📔",
        }

    @staticmethod
    def _summary_children(content):
        children = []
        state = {
            "total": 0,
            "toggle": None,
            "toggle_full": False,
            "last_list": None,
        }

        def note_block(text):
            return {
                "object": "block",
                "type": "paragraph",
                "paragraph": {
                    "rich_text": [NotionJournalExporter._text_object(text)]
                },
            }

        def emit(block):
            if state["total"] >= NOTION_MAX_BLOCKS:
                return False
            toggle = state["toggle"]
            if toggle is not None:
                nested = toggle["toggle"]["children"]
                if len(nested) >= NOTION_TOGGLE_CHILD_LIMIT:
                    if not state["toggle_full"]:
                        state["toggle_full"] = True
                        nested[-1] = note_block(
                            "Truncated at the nested block limit; the full "
                            "listing stays in the local journal file."
                        )
                    return False
                nested.append(block)
            else:
                children.append(block)
            state["total"] += 1
            return True

        def close_toggle():
            state["toggle"] = None
            state["toggle_full"] = False
            state["last_list"] = None

        def append_text_blocks(block_type, text, **attributes):
            first = None
            for offset in range(0, len(text), NOTION_TEXT_CHUNK):
                chunk_type = block_type if offset == 0 else "paragraph"
                payload = {
                    "rich_text": [
                        NotionJournalExporter._text_object(
                            text[offset : offset + NOTION_TEXT_CHUNK]
                        )
                    ]
                }
                if offset == 0:
                    payload.update(attributes)
                block = {
                    "object": "block",
                    "type": chunk_type,
                    chunk_type: payload,
                }
                if not emit(block):
                    return first
                if first is None:
                    first = block
            return first

        def append_list_detail(parent, text):
            payload = parent[parent["type"]]
            rich_text = payload["rich_text"]
            width = NOTION_TEXT_CHUNK - len("\n↳ ")
            for offset in range(0, len(text), width):
                if len(rich_text) >= NOTION_CHILD_BATCH:
                    return
                rich_text.append(
                    NotionJournalExporter._text_object(
                        "\n↳ " + text[offset : offset + width]
                    )
                )

        for raw_line in str(content or "").splitlines():
            if state["total"] >= NOTION_MAX_BLOCKS:
                break
            indented = bool(raw_line[:1] in (" ", "\t"))
            line = raw_line.strip()
            if not line:
                continue
            heading = re.match(r"^(#{1,3})\s+(.+)$", line)
            todo = re.match(r"^-\s+\[([ xX])\]\s+(.+)$", line)
            bullet = re.match(r"^-\s+(.+)$", line)
            numbered = re.match(r"^\d+\.\s+(.+)$", line)
            quote = re.match(r"^>\s+(.+)$", line)
            if line == "---":
                close_toggle()
                emit({"object": "block", "type": "divider", "divider": {}})
            elif heading:
                # A per-project ### subsection belongs to the listing above
                # it: only a section heading (## or #) leaves an open toggle.
                if len(heading.group(1)) >= 3 and state["toggle"] is not None:
                    state["last_list"] = None
                    append_text_blocks(
                        "heading_%d" % len(heading.group(1)), heading.group(2)
                    )
                    continue
                close_toggle()
                title = heading.group(2)
                if (
                    len(heading.group(1)) == 2
                    and title.strip() in NOTION_COLLAPSED_SECTIONS
                ):
                    toggle_block = {
                        "object": "block",
                        "type": "toggle",
                        "toggle": {
                            "rich_text": [
                                NotionJournalExporter._text_object(title.strip())
                            ],
                            "children": [],
                        },
                    }
                    if emit(toggle_block):
                        state["toggle"] = toggle_block
                else:
                    append_text_blocks(
                        "heading_%d" % len(heading.group(1)), title
                    )
            elif todo:
                state["last_list"] = append_text_blocks(
                    "to_do",
                    todo.group(2),
                    checked=todo.group(1).lower() == "x",
                )
            elif quote:
                # Daily decision quotes become callouts; the weekly projection
                # deliberately keeps its larger decision set as list items.
                state["last_list"] = None
                append_text_blocks(
                    "callout",
                    quote.group(1),
                    icon={"type": "emoji", "emoji": "\U0001f4a1"},
                )
            elif bullet:
                if indented and state["last_list"] is not None:
                    append_list_detail(state["last_list"], bullet.group(1))
                else:
                    state["last_list"] = append_text_blocks(
                        "bulleted_list_item", bullet.group(1)
                    )
            elif numbered:
                state["last_list"] = append_text_blocks(
                    "numbered_list_item", numbered.group(1)
                )
            else:
                state["last_list"] = None
                append_text_blocks("paragraph", line)

        if state["total"] >= NOTION_MAX_BLOCKS and children:
            children[-1] = note_block(
                "Work Journal content was truncated at the safe block limit."
            )
        return children

    @staticmethod
    def _text_object(content):
        return {
            "type": "text",
            "text": {"content": content},
        }

    @staticmethod
    def _page_id(page):
        if isinstance(page, Mapping):
            page_id = page.get("id")
            if page_id:
                return str(page_id)
        raise NotionTransportError("Notion API response did not include an id")

    @staticmethod
    def _property_text(page, property_name):
        if not isinstance(page, Mapping):
            return ""
        properties = page.get("properties", {})
        if not isinstance(properties, Mapping):
            return ""
        value = properties.get(property_name, {})
        if not isinstance(value, Mapping):
            return ""
        rich_text = value.get("rich_text", [])
        if not isinstance(rich_text, list):
            return ""

        text_parts = []
        for item in rich_text:
            if not isinstance(item, Mapping):
                continue
            plain_text = item.get("plain_text")
            if plain_text is not None:
                text_parts.append(str(plain_text))
                continue
            text = item.get("text", {})
            if isinstance(text, Mapping) and text.get("content") is not None:
                text_parts.append(str(text["content"]))
        return "".join(text_parts)
