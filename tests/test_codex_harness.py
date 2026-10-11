import base64
import contextlib
import http.client
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts" / "lib"))
from codex_auth_broker import AuthBroker, BridgeError
from codex_bridge import Gateway, MAX_BODY, SubscriptionBackend, response_events
from codex_harness import cli_command, harness_env, main, validate_tail
from codex_messages import MessageStream, from_messages, message, subscription_request


def completed(output=None):
    return {"type": "response.completed", "response": {"id": "resp_fixture", "status": "completed",
            "output": output or [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "fixture ok"}]}],
            "usage": {"input_tokens": 19, "output_tokens": 4, "input_tokens_details": {"cached_tokens": 7}}}}


def text_events():
    return [{"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "fixture "},
            {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "ok"}, completed()]


class FakeConnection:
    sock = None

    def close(self):
        pass


class FakeBackend:
    def __init__(self, events=None):
        self.events = text_events() if events is None else events
        self.calls = []

    def open(self, body):
        self.calls.append(body)
        payload = b"".join(b"data: " + json.dumps(row).encode() + b"\n\n" for row in self.events)
        return FakeConnection(), io.BytesIO(payload)


@contextlib.contextmanager
def gateway(backend=None):
    instance = Gateway(backend or FakeBackend(), "gpt-fixture", "low")
    thread = threading.Thread(target=instance.serve_forever, daemon=True)
    thread.start()
    try:
        yield instance
    finally:
        instance.close()
        thread.join(timeout=2)


def post(server, body, path="/v1/messages?beta=true", headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=3)
    auth = {"Authorization": "Bearer " + server.capability, "Content-Type": "application/json"}
    auth.update(headers or {})
    connection.request("POST", path, json.dumps(body), headers=auth)
    result = connection.getresponse()
    status, data = result.status, result.read()
    connection.close()
    return status, data


class MessageContractTests(unittest.TestCase):
    def test_tool_roundtrip_keeps_order_results_images_and_reasoning(self):
        reasoning = {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque", "summary": []}
        result = message(completed([reasoning, {"type": "function_call", "call_id": "call_1", "name": "Read", "arguments": '{"path":"a"}'}])["response"], "gpt-fixture")
        body = {"system": [{"type": "text", "text": "rules"}], "messages": [
            {"role": "user", "content": "read"}, {"role": "assistant", "content": result["content"]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": [
                {"type": "text", "text": "found"}, {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "YQ=="}}]},
                {"type": "text", "text": "continue"}]}],
            "tools": [{"name": "Read", "input_schema": {"type": "object"}}], "tool_choice": {"type": "tool", "name": "Read"}}
        outgoing = from_messages(body)
        self.assertEqual(outgoing["input"][1], reasoning)
        self.assertEqual(outgoing["input"][2]["call_id"], "call_1")
        self.assertEqual(outgoing["input"][3]["output"][1]["image_url"], "data:image/png;base64,YQ==")
        self.assertEqual(outgoing["input"][4]["content"][0]["text"], "continue")
        self.assertEqual(outgoing["tool_choice"], {"type": "function", "name": "Read"})
        self.assertEqual(result["stop_reason"], "tool_use")
        self.assertEqual(result["usage"]["input_tokens"], 12)

    def test_reject_unmatched_calls_server_tools_and_unsupported_content(self):
        for body in [
            {"messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "wrong"}]}]},
            {"messages": [{"role": "assistant", "content": [{"type": "tool_use", "id": "x", "name": "Read"}]}]},
            {"messages": [{"role": "user", "content": [{"type": "document", "source": {}}]}]},
            {"messages": [{"role": "user", "content": "x"}], "tools": [{"type": "web_search_20250305", "name": "search"}]},
        ]:
            with self.subTest(body=body), self.assertRaises(BridgeError):
                from_messages(body)

    def test_subscription_forces_pin_and_stateless_transport(self):
        result = subscription_request({"model": "other", "store": True, "stream": False,
            "max_output_tokens": 100, "instructions": "rules", "input": [{"role": "system", "content": "extra"}, {"role": "user", "content": "hi"}]}, "gpt-fixture", "low")
        self.assertEqual(result["model"], "gpt-fixture")
        self.assertFalse(result["store"])
        self.assertTrue(result["stream"])
        self.assertEqual(result["instructions"], "rules\n\nextra")
        self.assertNotIn("max_output_tokens", result)
        with self.assertRaises(BridgeError):
            subscription_request({"input": "x", "previous_response_id": "other"}, "gpt-fixture", "low")

    def test_native_claude_midconversation_system_message_preserves_position(self):
        body = {"messages": [{"role": "user", "content": "hi"},
                             {"role": "system", "content": [{"type": "text", "text": "current turn rules"}]}]}
        result = subscription_request(from_messages(body), "gpt-fixture", "low")
        self.assertEqual([row["role"] for row in result["input"]], ["user", "developer"])
        self.assertEqual(result["input"][1]["content"][0]["text"], "current turn rules")

    def test_classifier_stop_sequences_keep_first_match_and_require_completed_text(self):
        body = {"messages": [{"role": "user", "content": "classify"}], "stop_sequences": ["</block>"]}
        self.assertNotIn("stop_sequences", from_messages(body))
        output = [{"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": "<block>no</blo"},
            {"type": "output_text", "text": "ck>do not return this"}]}]
        response = completed(output)["response"]
        result = message(response, "gpt-fixture", ["later", "</block>"])
        self.assertEqual("".join(row["text"] for row in result["content"]), "<block>no")
        self.assertEqual(result["stop_reason"], "stop_sequence")
        self.assertEqual(result["stop_sequence"], "</block>")
        self.assertEqual(message(response, "gpt-fixture", ["absent"])["stop_reason"], "end_turn")
        for override in ({"stream": True}, {"tools": [{"name": "Bash", "input_schema": {"type": "object"}}]},
                         {"stop_sequences": "</block>"}, {"stop_sequences": [""]}, {"stop_sequences": [None]},
                         {"stop_sequences": ["x"] * 17}):
            with self.subTest(override=override), self.assertRaises(BridgeError):
                from_messages(dict(body, **override))
        response["status"] = "incomplete"
        with self.assertRaises(BridgeError):
            message(response, "gpt-fixture", ["</block>"])
        with self.assertRaises(BridgeError):
            message(completed([{"type": "function_call", "call_id": "call_1", "name": "Bash", "arguments": "{}"}])["response"],
                    "gpt-fixture", ["</block>"])

    def test_stream_emits_text_tool_arguments_and_terminal_usage(self):
        converter = MessageStream("gpt-fixture")
        rows = []
        for event in text_events()[:2] + [
            {"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "call_id": "call_1", "name": "Read"}},
            {"type": "response.function_call_arguments.delta", "output_index": 1, "delta": '{"path":"a"}'},
            completed([{ "type": "function_call", "call_id": "call_1", "name": "Read", "arguments": '{"path":"a"}'}])]:
            rows.extend(converter.feed(event))
        self.assertEqual(rows[0][0], "message_start")
        self.assertEqual(rows[-1][0], "message_stop")
        self.assertEqual(rows[-2][1]["delta"]["stop_reason"], "tool_use")
        self.assertEqual(rows[-2][1]["usage"]["cache_read_input_tokens"], 7)
        self.assertEqual([row[1]["delta"]["text"] for row in rows if row[0] == "content_block_delta" and row[1]["delta"]["type"] == "text_delta"], ["fixture ", "ok"])


class GatewayTests(unittest.TestCase):
    def test_native_auto_classifier_request_roundtrip_and_incomplete_verdict_denial(self):
        body = {"model": "gpt-fixture", "max_tokens": 64, "system": "Classify the requested action.",
                "messages": [{"role": "user", "content": "authorized fixture"}], "stop_sequences": ["</block>"]}
        event = completed([{"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": "<block>no</block>discarded"}]}])
        with gateway(FakeBackend([event])) as server:
            status, data = post(server, body)
            self.assertEqual(status, 200)
            result = json.loads(data)
            self.assertEqual(result["content"], [{"type": "text", "text": "<block>no"}])
            self.assertEqual(result["stop_sequence"], "</block>")
            self.assertNotIn("stop_sequences", server.backend.calls[0])
        event["type"], event["response"]["status"] = "response.incomplete", "incomplete"
        with gateway(FakeBackend([event])) as server:
            self.assertEqual(post(server, body)[0], 502)

    def test_completion_stops_upstream_reader_before_socket_trailer(self):
        class Response(io.BytesIO):
            trailer_reads = 0

            def readline(self, limit):
                if self.tell() == len(self.getvalue()):
                    self.trailer_reads += 1
                    raise AssertionError("completion must not read the closing socket trailer")
                return super().readline(limit)

        backend = FakeBackend()
        response = Response(b"data: " + json.dumps(completed()).encode() + b"\n\n")
        backend.open = lambda _body: (FakeConnection(), response)
        with gateway(backend) as server:
            status, data = post(server, {"messages": [{"role": "user", "content": "hi"}]})
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(data)["stop_reason"], "end_turn")
        self.assertEqual(response.trailer_reads, 0)

    def test_messages_and_responses_stream_and_nonstream(self):
        with gateway() as server:
            for stream in (False, True):
                status, data = post(server, {"messages": [{"role": "user", "content": "hi"}], "stream": stream})
                self.assertEqual(status, 200)
                if stream:
                    self.assertIn(b"event: message_stop", data)
                else:
                    self.assertEqual(json.loads(data)["content"], [{"type": "text", "text": "fixture ok"}])
                status, data = post(server, {"input": "hi", "stream": stream}, "/v1/responses")
                self.assertEqual(status, 200)
                if stream:
                    self.assertIn(b"event: response.completed", data)
                else:
                    self.assertEqual(json.loads(data)["id"], "resp_fixture")
            self.assertEqual(len(server.backend.calls), 4)
            self.assertTrue(all(row["model"] == "gpt-fixture" for row in server.backend.calls))

    def test_auth_origin_host_size_and_endpoint_rejections_do_not_call_backend(self):
        body = {"messages": [{"role": "user", "content": "hi"}]}
        with gateway() as server:
            for headers in ({"Authorization": "Bearer wrong"}, {"Authorization": "Bearer é"},
                            {"Origin": "https://attacker.example"}, {"Host": "attacker.example"}):
                self.assertEqual(post(server, body, headers=headers)[0], 403)
            self.assertEqual(post(server, body, "/v1/messages/count_tokens")[0], 404)
            self.assertEqual(post(server, body, headers={"Content-Length": str(MAX_BODY + 1)})[0], 413)
            self.assertEqual(server.backend.calls, [])

    def test_truncated_and_failed_streams_are_not_success_and_errors_are_sanitized(self):
        invalid = completed()
        invalid["response"]["status"] = "failed"
        for events in (text_events()[:1], [invalid], [{"type": "response.failed", "response": {"error": {"message": "secret payload"}}}]):
            with self.subTest(events=events), gateway(FakeBackend(events)) as server:
                status, data = post(server, {"messages": [{"role": "user", "content": "hi"}], "stream": True})
                self.assertEqual(status, 200)
                self.assertIn(b"event: error", data)
                self.assertNotIn(b"event: message_stop", data)
                self.assertNotIn(b"secret payload", data)

    def test_upstream_credentials_only_go_to_fixed_origin_and_401_refresh_is_bounded(self):
        broker = unittest.mock.Mock()
        broker.credentials.return_value = ("private-access", "private-account")
        connections = [unittest.mock.Mock(), unittest.mock.Mock()]
        for connection, status in zip(connections, [401, 200]):
            connection.getresponse.return_value.status = status
            connection.getresponse.return_value.getheader.return_value = None
        with patch("codex_bridge.http.client.HTTPSConnection", side_effect=connections) as factory:
            connection, _ = SubscriptionBackend(broker).open({"input": "x"})
        self.assertIs(connection, connections[1])
        self.assertEqual([call.args[0] for call in factory.call_args_list], ["chatgpt.com", "chatgpt.com"])
        self.assertEqual([call.kwargs["refresh"] for call in broker.credentials.call_args_list], [False, True])
        self.assertEqual(connections[0].request.call_args.args[1], "/backend-api/codex/responses")
        self.assertEqual(connections[0].request.call_args.args[3]["Authorization"], "Bearer " + "private-access")

    def test_sse_parser_rejects_truncated_event(self):
        with self.assertRaises(BridgeError):
            list(response_events(io.BytesIO(b'data: {"type":"response.completed"}\n')))
        with self.assertRaises(BridgeError):
            list(response_events(io.BytesIO(b'{"error":"unexpected JSON body"}\n')))


class LaunchTests(unittest.TestCase):
    def test_cli_route_and_child_environment_do_not_export_subscription_tokens(self):
        inherited = dict.fromkeys(("OPENAI_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"), "private")
        with gateway() as server, patch.dict(os.environ, dict(inherited, HTTP_PROXY="http://proxy.invalid")):
            for host in ("codex", "pi", "claude"):
                command = cli_command(host, "gpt-fixture", ["--help"])
                env = harness_env(host, server, "gpt-fixture", 200000)
                self.assertEqual(command[0], host)
                self.assertNotIn("private", " ".join(command))
                self.assertNotIn("OPENAI_API_KEY", env)
                if host != "codex":
                    self.assertNotIn("HTTP_PROXY", env)
            self.assertEqual(env["ANTHROPIC_AUTH_TOKEN"], server.capability)
            self.assertNotIn("CLAUDE_CODE_OAUTH_TOKEN", env)
            self.assertEqual(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "gpt-fixture")
            routes = ("ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_USE_BEDROCK")
            with patch.dict(os.environ, dict.fromkeys(routes, "sentinel")):
                for host in ("codex", "pi"):
                    scrubbed = harness_env(host, server, "gpt-fixture", 200000)
                    self.assertEqual([key for key in routes if key in scrubbed], [])
                    self.assertEqual(scrubbed.get("OMS_HARNESS_BRIDGE"), None if host == "codex" else "1")
            with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": "pi-main", "OMS_PANEL_OWNER": "codex",
                                         "OMS_PANEL_HARNESS": "pi", "OMS_HOOK_AGENT": "codex"}):
                pi_env = harness_env("pi", server, "gpt-fixture", 200000)
                self.assertEqual(pi_env["OMS_AGENT"], "codex")
                self.assertEqual(pi_env["OMS_PANEL_HARNESS"], "pi")
                self.assertNotIn("OMS_HOOK_AGENT", pi_env)

    def test_route_override_and_nested_worker_denied(self):
        for host, args in (("codex", ["--config", "model_provider=other"]), ("claude", ["--model=other"]), ("pi", ["--api-key", "private"])):
            with self.subTest(host=host), self.assertRaises(BridgeError):
                validate_tail(host, args)
        for flags in ({"OMS_HARNESS_CHILD": "1", "OMS_HARNESS_DELEGATE_DEPTH": "0"},
                      {"OMS_HARNESS_CHILD": "0", "OMS_HARNESS_DELEGATE_DEPTH": "1"}):
            with self.subTest(flags=flags), patch.dict(os.environ, flags), \
                    patch("codex_harness.AuthBroker") as broker, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["claude", "--model", "gpt-fixture", "--dry-run"]), 2)
                broker.assert_not_called()

    def test_dispatcher_dry_run_does_not_start_auth_or_model_processes(self):
        root = Path(__file__).resolve().parents[1]
        env = dict(os.environ)
        env.pop("OMS_HARNESS_CHILD", None)
        env.pop("OMS_HARNESS_DELEGATE_DEPTH", None)
        result = subprocess.run([str(root / "scripts" / "oms"), "harness", "pi", "--model", "gpt-fixture", "--dry-run", "--", "--print", "hi"],
                                env=env, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["argv"][-2:], ["--print", "hi"])

    def test_native_auth_broker_fake_rpc_and_refresh(self):
        claims = {"exp": time.time() + 3600, "https://api.openai.com/auth": {"chatgpt_account_id": "fixture-account"}}
        signed = "header." + base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=") + ".signature"
        with tempfile.TemporaryDirectory(prefix="oms-auth-test-") as directory:
            binary = Path(directory) / "codex.py"
            binary.write_text('import sys,json\n' +
                'for line in sys.stdin:\n' + ' row=json.loads(line)\n' + ' if "id" not in row: continue\n' +
                ' result={"initialize":{},"account/read":{"account":{"type":"chatgpt"}},"getAuthStatus":{"authMethod":"chatgpt",' + json.dumps("auth" "Token") + ':' + repr(signed) +
                '},"model/list":{"data":[{"id":"gpt-fixture","model":"gpt-fixture"}]}}[row["method"]]\n' +
                ' print(json.dumps({"id":row["id"],"result":result}),flush=True)\n')
            broker = AuthBroker([sys.executable, str(binary)])
            try:
                self.assertEqual(broker.credentials()[1], "fixture-account")
                self.assertEqual(broker.credentials(refresh=True)[1], "fixture-account")
                self.assertEqual(broker.model("gpt-fixture")["id"], "gpt-fixture")
                with self.assertRaises(BridgeError):
                    broker.model("missing")
            finally:
                broker.close()
            self.assertIsNotNone(broker.process.poll())


if __name__ == "__main__":
    unittest.main()
