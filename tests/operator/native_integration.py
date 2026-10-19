# Ordered integration dependency: native navigation -> adapters -> lifecycle/restart.
_operator_block("native-navigation")
def end_fixture_mains():
    for row in json.loads(panel.events(project, "list", "--json", output=True)):
        if row.get("tool") == "panel-main" and row.get("terminal") is not True:
            panel.events(project, "transition", "--attempt", row["attempt_id"], "--state", "cancelled")
end_fixture_mains()

# A click can arrive as soon as a new frame is flushed, before drawing returns.
# Clear clicks for the old layout without consuming that newly targeted click.
pending_clicks = ["old layout"]
watch_input = BatchInput([])
def click_on_draw(*args, **kwargs):
    pending_clicks.append("new layout")
with patch.dict(os.environ), \
        patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "read_panel", return_value=(tree_room, 0, "codex", None, "tree", False, shared)), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "main_installed", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((110, 28))), \
        patch.object(panel, "draw_frame", side_effect=click_on_draw), \
        patch.object(watch_input, "discard_clicks", side_effect=pending_clicks.clear), \
        patch("panel_input.TerminalInput", return_value=watch_input):
    assert panel.watch(project, "codex", count=1, view="tree", no_animation=True) == 0
assert pending_clicks == ["new layout"], pending_clicks

# Exercise the real bounded JSON socket client without installing a host or
# running agents; lifecycle and room admission use the canonical OMS tools.
if os.name == "posix" and os.environ.get("OMS_OPERATOR_SOCKET_DENIED") != "1":
    import panel_host
    import panel_chats
    import socket
    import threading
    socket_path = temporary / "herdr.sock"
    host_pane = {"pane_id": "w1:p1", "terminal_id": "term_fixture", "workspace_id": "w1",
                 "tab_id": "w1:t1", "cwd": str(project.resolve()), "agent": "codex"}
    host_panes = {"w1:p1": host_pane, "w1:p2": dict(host_pane, pane_id="w1:p2", terminal_id="term_other")}
    pane_errors = set()
    host_calls, host_fault = [], []
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(socket_path))
    server.listen()
    server.settimeout(.1)
    stopped = threading.Event()
    def serve_host():
        while not stopped.is_set():
            try:
                connection, _ = server.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            with connection:
                connection.settimeout(2)
                data = bytearray()
                while b"\n" not in data:
                    data.extend(connection.recv(65536))
                message = json.loads(bytes(data).split(b"\n", 1)[0])
                host_calls.append(message)
                result = ({"pane": dict(host_panes[message["params"]["pane_id"]])} if message["method"] == "pane.get" else
                          {"read": {"pane_id": "w1:p1", "text": "Context 83% left / weekly 61% left"}}
                          if message["method"] == "pane.read" else {})
                reply = {"id": "wrong" if host_fault else message["id"], "result": result}
                if message["method"] == "pane.get" and message["params"]["pane_id"] in pane_errors:
                    reply = {"id": message["id"], "error": {"message": "closed pane"}}
                connection.sendall(json.dumps(reply).encode() + b"\n")
    host_thread = threading.Thread(target=serve_host, daemon=True)
    host_thread.start()
    try:
        host_env = dict(environment, HERDR_ENV="1", HERDR_SOCKET_PATH=str(socket_path),
                        HERDR_PANE_ID="w1:p1", OMS_PANEL_HOST="herdr")
        host_room = room.create(project, "herdr-room", "Host fixture")
        host_env.update(OMS_ROOM_ID=host_room, OMS_ROOM_REPO=str(project))
        with patch.dict(os.environ, host_env, clear=True):
            assert panel_host.current(project)["terminal_id"] == "term_fixture"
            host_ref = panel_host.refs(project)
            host_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-main",
                *[arg for key, value in host_ref.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            room.join(project, host_room, "herdr-main", "codex", label="Herdr main")
            host_state = room.status(project, host_room)
            destination = panel_host.targets(project, host_room, host_state)["herdr-main"][0]
            assert destination["host"] == "herdr" and destination["attempt"] == host_attempt
            assert pane_reading(destination, "codex")["weekly_used"] == 39
            with patch("panel_chats.native_index", return_value={}):
                before_focus = len(host_calls)
                planned = panel_chats.open_chat(project, host_room, "herdr-main", dry_run=True)
                assert planned["host"] == "herdr" and not any(c["method"] == "pane.focus" for c in host_calls[before_focus:])
                assert panel_chats.open_chat(project, host_room, "herdr-main")["status"] == "navigation_requested"
            try:
                panel.open_native("codex", project)
                raise AssertionError("a second legacy Herdr main was opened")
            except ValueError as error:
                assert "already has a live main" in str(error)
            other_refs = dict(host_ref, panel_pane_id="w1:p2", panel_terminal_id="term_other")
            other_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-other",
                *[arg for key, value in other_refs.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            legacy_main_join(project, host_room, "herdr-other", "codex")
            host_state = room.status(project, host_room)
            assert set(panel_host.targets(project, host_room, host_state)) == {"herdr-main", "herdr-other"}
            pane_errors.add("w1:p2")
            assert set(panel_host.targets(project, host_room, host_state)) == {"herdr-main"}
            panel_host.verify(destination)
            duplicate_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
                "--ref", "panel_role=main", "--ref", "panel_room_id=" + host_room,
                "--ref", "panel_room_participant=herdr-main",
                *[arg for key, value in other_refs.items() for arg in ("--ref", key + "=" + value)],
                "--then", "starting", "--then", "working", output=True)
            assert panel_host.targets(project, host_room, host_state) == {}, "failed duplicate established unique ownership"
            panel.events(project, "transition", "--attempt", duplicate_attempt, "--state", "cancelled")
            panel.events(project, "transition", "--attempt", other_attempt, "--state", "cancelled")
            pane_errors.clear()
            host_pane["terminal_id"] = "term_replaced"
            assert panel_host.targets(project, host_room, host_state) == {}
            try:
                panel_host.focus(destination)
            except ValueError:
                pass
            else:
                raise AssertionError("reused pane focused another terminal")
            host_pane["terminal_id"] = "term_fixture"
            host_pane["agent_session"] = {"agent": "claude", "value": "foreign-session"}
            assert panel_host.targets(project, host_room, host_state) == {}
            host_pane.pop("agent_session")
            panel.events(project, "transition", "--attempt", host_attempt, "--state", "cancelled")
            assert panel_host.targets(project, host_room, host_state) == {}
            panel.open_native("codex", project)
            layout = next(c["params"] for c in reversed(host_calls) if c["method"] == "layout.apply")
            # Herdr keeps the board as the left pane too.
            assert "tab_id" not in layout and layout["root"]["ratio"] == .28
            for configured, ratio in (("30", .30), ("1", .25), ("99", .35), ("-1", .25),
                                      ("+35", .35), ("garbage", .28), ("28.5", .28), ("2_8", .28)):
                with patch.dict(os.environ, {"OMS_PANEL_BOARD_PERCENT": configured}):
                    panel_host.open_native(project, "codex", layout["root"]["second"]["command"],
                                           layout["root"]["first"]["command"])
                assert host_calls[-1]["params"]["root"]["ratio"] == ratio
            assert layout["root"]["direction"] == "right" and "--watch" in layout["root"]["first"]["command"]
            main_command = layout["root"]["second"]["command"]
            assert str(panel.ENTRY.parent / "panel.sh") in main_command and "--launch" in main_command
            assert layout["root"]["first"]["env"]["OMS_ROOM_ID"] == host_room
            assert layout["root"]["first"]["env"]["OMS_PANEL_VIEW"] == "graph"
            assert not any(c["method"] in {"pane.send_text", "pane.send_input", "server.stop"} for c in host_calls)
            host_fault.append(True)
            try:
                panel_host.current(project)
            except ValueError:
                pass
            else:
                raise AssertionError("wrong response id was accepted")
    finally:
        stopped.set()
        server.close()
        host_thread.join(timeout=2)
        socket_path.unlink()
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(room, "status", return_value=tree_room["room"]), \
        patch("panel_chats.open_chat", return_value={"method": "existing-terminal"}) as opened:
    nav = {"room_id": shared}
    panel.navigate(project, ("chat", main_attempts["codex"]), tree_room, nav, 91)
    assert opened.call_args.kwargs == {"allowed_methods": {"existing-terminal", "app-uri", "windows-uri"}}, opened.call_args
    assert nav["notice"] == "Chat navigation requested in its window"
    left = deepcopy(tree_room["room"])
    for member in left["participants"]:
        member["joined"] = False
    with patch.object(room, "status", return_value=left):
        try:
            panel.navigate(project, ("chat", main_attempts["codex"]), tree_room, nav, 91)
        except ValueError:
            pass
        else:
            raise AssertionError("left participant was navigated")
    assert opened.call_count == 1
with patch("panel_chats.plan", return_value={"method": "claude-desktop"}), \
        patch("panel_chats.subprocess.run") as launched:
    import panel_chats
    try:
        panel_chats.open_chat(project, shared, main_attempts["claude"], allowed_methods={"existing-terminal", "app-uri", "windows-uri"})
    except ValueError:
        pass
    else:
        raise AssertionError("tree navigation started a desktop CLI")
    launched.assert_not_called()
if os.name != "nt" and os.environ.get("OMS_OPERATOR_SOCKET_DENIED") != "1":
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    try:
        with os.fdopen(os.dup(slave), "r") as input_tty, os.fdopen(os.dup(slave), "w") as output_tty, \
                patch.object(sys, "stdin", input_tty), patch.object(sys, "stdout", output_tty), \
                patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-panel-abcdef012345"}), \
                patch("panel_input.subprocess.run") as mouse_setting:
            with TerminalInput(active=False) as passive:
                assert passive.fd is None and termios.tcgetattr(slave) == original
            mouse_setting.assert_not_called()
            # A failed tmux mouse setting is best effort: the watcher keeps its raw keyboard input.
            mouse_setting.side_effect = subprocess.TimeoutExpired("tmux", 2)
            with TerminalInput(managed=True) as degraded:
                assert degraded.fd is not None and not termios.tcgetattr(slave)[3] & termios.ICANON
            assert termios.tcgetattr(slave) == original
            mouse_setting.side_effect = None
            mouse_setting.reset_mock()
            try:
                with TerminalInput(managed=True) as listener:
                    assert listener.fd is not None
                    changed = termios.tcgetattr(slave)
                    assert not changed[3] & termios.ICANON and not changed[3] & termios.ECHO
                    assert changed[3] & termios.ISIG == original[3] & termios.ISIG
                    os.write(master, b"\033[<0;8;7M")
                    assert listener.wait(.2) == [("click", 8, 7)]
                    raise KeyboardInterrupt
            except KeyboardInterrupt:
                pass
            assert termios.tcgetattr(slave) == original
            argv = mouse_setting.call_args.args[0]
            assert argv == ["tmux", "set-option", "-t", "oms-panel-abcdef012345", "mouse", "on"]
        output = b""
        deadline = time.monotonic() + 1
        while b"\033[?1000l" not in output and time.monotonic() < deadline:
            if select.select([master], [], [], max(0, deadline - time.monotonic()))[0]:
                output += os.read(master, 4096)
        assert b"\033[?1000h" in output and b"\033[?1000l" in output, output
    finally:
        os.close(master)
        os.close(slave)
    actual = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--json"], room_env))["dashboard"]
    # Match the watcher's start-row reservation before deriving click coordinates.
    nav = {"installed": list(panel.MAIN_HARNESSES)}
    render(actual, "codex", 110, 28, view="tree", navigation=nav)
    # The recorded result is past work: the dock's Past tab lists it, a click there shows it in Detail and f opens it.
    tab_hit = next(h for h in nav["hits"] if h["action"] == ("tab", "past"))
    assert panel.tab_event(("click", tab_hit["x1"], tab_hit["y"]), nav) and nav["tab"] == "past", nav
    past_picture = render(actual, "codex", 110, 28, view="tree", navigation=nav)
    hit = next(h for h in nav["hits"] if h.get("past") and h["action"][0] == "result")
    assert "Activity" in past_picture and "Past work" not in past_picture and "[Past " in past_picture, past_picture
    # The result opens from a background read; the watcher exits on its own after three snapshots, so a slow background read still lands.
    tab_click, result_click = [("\033[<0;%s;%sM" % (x, y)).encode() for x, y in ((tab_hit["x1"], tab_hit["y"]), (8, hit["y"]))]
    before_click = ledger.read_bytes()
    terminal(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
              "--watch", "--view", "tree", "--count", "3", "--no-animation"],
             tab_click, b"RECORDED RESULTS", trigger=b"Activity", timeout=60, then=(b"[Past ", result_click + b"f"))
    assert ledger.read_bytes() == before_click, "clicking a result wrote lifecycle state"
    import signal
    for signum in (signal.SIGHUP, signal.SIGTERM):
        terminal(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                  "--watch", "--view", "tree", "--count", "2", "--no-animation"], b"", b"\033[?1000l",
                 trigger=b"Activity", signal_after=signum, expected_exit=128 + signum)
# Publishing uses the saved receiver contract and persists intent before the call.
with patch.object(codex_app_notify, "deliver", return_value={"status": "persisted", "turn_id": "fixture-app-turn",
        "delivery_unknown": False}) as delivered:
    output = room.publish(project, shared)
    assert output["receipt"]["status"] == "persisted"
    assert room.publish(project, shared)["deduplicated"] and delivered.call_count == 1
room.send(project, shared, room_owner, main_attempts["codex"], "New bounded result")
with patch.object(codex_app_notify, "deliver", return_value={"status": "timeout", "delivery_unknown": True}) as delivered:
    room.publish(project, shared)
    assert room.publish(project, shared)["deduplicated"] and delivered.call_count == 1
    room.publish(project, shared, retry=True)
    assert delivered.call_count == 2
assert room.status(project, shared)["publication"]["status"] == "timeout"
# Native Claude app/terminal delivery has its own receiver and durable intent.
import hashlib
import claude_app_notify
legacy_main_join(project, shared, "opus-app", "claude", native_session="fixture-claude-app", label="Opus app")
before = room.records(project, shared)
with patch.object(claude_app_notify, "deliver", side_effect=AssertionError("wakeup was not authorized")):
    blocked = room.publish(project, shared, app="claude", recipient="opus-app")
    assert blocked["receipt"]["status"] == "wakeup_required"
    assert room.records(project, shared) == before
blocked = json.loads(call(["bash", str(panel.ENTRY), "room", "publish", "--repo", str(project),
                          "--id", shared, "--app", "claude", "--to", "opus-app"]))
assert blocked["receipt"]["status"] == "wakeup_required"
def claude_post(*args):
    pending = room.status(project, shared)["publications"]["claude.opus-app"]
    assert pending["status"] == "pending" and pending["delivery_unknown"], pending
    assert args[2] == hashlib.sha256(b"fixture-claude-app").hexdigest()[:32] and args[3] is True
    return {"status": "submitted", "submitted": True, "delivery_unknown": True}
with patch.object(claude_app_notify, "deliver", side_effect=claude_post) as delivered:
    sent = room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)
    assert sent["target"] == "claude.opus-app" and sent["receipt"]["status"] == "submitted"
    room.send(project, shared, room_owner, "opus-app", "New result must not silently resend an uncertain app post")
    assert room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)["deduplicated"]
    assert delivered.call_count == 1
    room.publish(project, shared, retry=True, app="claude", recipient="opus-app", allow_wakeup=True)
    assert delivered.call_count == 2
with patch.object(codex_app_notify, "deliver", side_effect=AssertionError("Codex uncertainty must remain independent")):
    assert room.publish(project, shared)["deduplicated"]
for recipient in (main_attempts["claude"], main_attempts["codex"], "missing"):
    with patch.object(claude_app_notify, "deliver", side_effect=AssertionError("unbound receiver")):
        try:
            room.publish(project, shared, app="claude", recipient=recipient, allow_wakeup=True)
        except ValueError:
            pass
        else:
            raise AssertionError("unbound or foreign app receiver accepted")
with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
    try:
        room.publish(project, shared, app="claude", recipient="opus-app", allow_wakeup=True)
    except ValueError:
        pass
    else:
        raise AssertionError("worker could publish")
# The current shortcut stores a native session hash, never the exported token/socket.
with patch.object(claude_app_notify, "current_session", return_value="fixture-current-app"):
    bound = legacy_main_join(project, shared, "current-app", "claude", native_session="current")
assert bound["consumer"] == hashlib.sha256(b"fixture-current-app").hexdigest()[:32]
assert "socket" not in bound and "token" not in bound
try:
    legacy_main_join(project, shared, "current-codex", "codex", native_session="current")
except ValueError as error:
    assert "current shortcut is for the calling Claude session" in str(error), error
else:
    raise AssertionError("a non-Claude join bound the literal current")
assert all(p["participant"] != "current-codex" for p in room.status(project, shared)["participants"])
room_view["room"] = room.status(project, shared)
for view in ("graph", "compact"):
    drawn = render(room_view, "claude", 100, 48, view=view)
    assert "APP DELIVERY" not in drawn and "att_" not in drawn, drawn
    assert all(display_width(line) <= 100 for line in drawn.splitlines()) and len(drawn.splitlines()) <= 48
# Old target-less receipts continue to protect the original Codex receiver.
legacy_app = room.create(project, "legacy-app-room", "Legacy delivery")
intent = {"kind": "publication", "id": "old-app-intent", "digest": "a" * 64,
          "status": "pending", "delivery_unknown": True}
room.append(project, legacy_app, intent, "Old app intent")
room.append(project, legacy_app, dict(intent, status="timeout"), "Old app receipt")
assert room.status(project, legacy_app)["publications"]["codex"]["status"] == "timeout"
with patch.object(codex_app_notify, "deliver", side_effect=AssertionError("old logs must not resend")):
    assert room.publish(project, legacy_app)["deduplicated"]
# Native chat lookup is explicit, private and read-only. A malformed enrolled
# record must not hide a different valid chat in the same directory.
import panel_chats
chat_codex = "11111111-1111-4111-8111-111111111111"
bad_codex = "22222222-2222-4222-8222-222222222222"
chat_claude = "33333333-3333-4333-8333-333333333333"
for who, provider, native in (("chat-sol", "codex", chat_codex), ("bad-sol", "codex", bad_codex),
                               ("chat-opus", "claude", chat_claude)):
    legacy_main_join(project, shared, who, provider)
    room.bind(project, shared, who, native)
codex_home = home / "chat-codex"
codex_day = codex_home / "sessions/2026/10/05"
codex_day.mkdir(parents=True)
(codex_day / ("rollout-x-" + bad_codex + ".jsonl")).write_text("bad metadata\n")
chat_file = codex_day / ("rollout-x-" + chat_codex + ".jsonl")
chat_file.write_text(json.dumps({"type": "session_meta", "payload": {"id": chat_codex, "cwd": str(project)}})
                     + "\nPRIVATE_TRANSCRIPT_BODY_MUST_NOT_BE_READ_OR_SHARED\n")
claude_home = home / "chat-claude"
claude_project = claude_home / "projects/local-project"
claude_project.mkdir(parents=True)
(claude_project / (chat_claude + ".jsonl")).write_text(json.dumps({"type": "user", "sessionId": chat_claude}) + "\n")
chat_env = dict(environment, CODEX_HOME=str(codex_home), CLAUDE_CONFIG_DIR=str(claude_home),
                OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project), TMUX="fixture-tmux", TMUX_PANE="%1")
room_before_chats = room.records(project, shared)
navigation_calls = []
chat_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + shared,
    "--ref", "panel_room_participant=chat-sol", "--then", "starting", "--then", "working", output=True)
navigation_run = subprocess.run
current_session = ["$1"]
client_listing = ["fixture-client\t$2\n"]
def tmux_navigation(command, **kwargs):
    if command[0] == "bash":
        return navigation_run(command, **kwargs)
    if command[1] == "list-windows":
        return subprocess.CompletedProcess(command, 0, "@42\t" + chat_attempt + "\t" + shared + "\t%42\tchat-sol\t" + str(project.resolve()) + "\t$1\n"
            + "@99\tchat-opus\tforeign-room\t%99\tchat-opus\t" + str(project.resolve()) + "\t$2\n", "")
    if command[1] == "list-panes":
        return subprocess.CompletedProcess(command, 0, "%42\t@42\n%99\t@99\n", "")
    if command[1] == "display-message":
        return subprocess.CompletedProcess(command, 0, current_session[0] + "\n", "")
    if command[1] == "list-clients":
        return subprocess.CompletedProcess(command, 0, client_listing[0], "")
    navigation_calls.append(command)
    return subprocess.CompletedProcess(command, 0, "", "")
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.Path, "home", return_value=home), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/launcher"), \
        patch.object(panel_chats.subprocess, "run", side_effect=tmux_navigation):
    chats = panel_chats.catalog(project, shared)
    assert "PRIVATE_TRANSCRIPT_BODY" not in json.dumps(chats)
    native = next(r for r in chats["rows"] if r["participant"] == "chat-sol")
    assert native["uri"] == "codex://threads/" + chat_codex and native["terminal"]["pane"] == "%42", native
    assert next(r for r in chats["rows"] if r["participant"] == "bad-sol")["status"] == "unresolved"
    assert next(r for r in chats["rows"] if r["participant"] == "chat-opus")["session_id"] == chat_claude
    chat_text = panel_chats.text(chats)
    assert "native session: " + chat_claude + " / Claude app sidebar" in chat_text
    assert "codex://threads/" + chat_codex in chat_text
    assert next(r for r in chats["rows"] if r["participant"] == child["participant"])["status"] == "artifacts"
    dry = panel_chats.open_chat(project, shared, "chat-sol", dry_run=True)
    assert dry["method"] == "existing-terminal" and not navigation_calls
    opened = panel_chats.open_chat(project, shared, "chat-sol")
    assert opened["status"] == "navigation_requested" and not opened["ui_observed"]
    assert navigation_calls == [["tmux", "select-window", "-t", "@42"], ["tmux", "select-pane", "-t", "%42"]]
    current_session[0] = "$2"
    navigation_calls.clear()
    assert panel_chats.open_chat(project, shared, "chat-sol")["status"] == "navigation_requested"
    assert navigation_calls == [["tmux", "select-window", "-t", "@42"], ["tmux", "select-pane", "-t", "%42"],
                                ["tmux", "switch-client", "-c", "fixture-client", "-t", "$1"]]
    client_listing[0] += "second-client\t$2\n"
    before_ambiguous = list(navigation_calls)
    try:
        panel_chats.open_chat(project, shared, "chat-sol")
    except ValueError as error:
        assert "ambiguous" in str(error)
    else:
        raise AssertionError("cross-session navigation chose between current clients")
    assert navigation_calls == before_ambiguous
    current_session[0] = "$1"
    navigation_calls[:] = navigation_calls[:2]
    with patch.object(panel_chats.sys, "platform", "linux"), \
            patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 0, "codex.desktop\n", "")) as handler_probe:
        app = panel_chats.open_chat(project, shared, "chat-sol", "app", dry_run=True)
    assert app["uri"] == native["uri"]
    assert len(navigation_calls) == 2
    if os.name != "nt":
        assert app["commands"] == [["xdg-open", native["uri"]]]
        assert handler_probe.call_args.args[0] == ["xdg-mime", "query", "default", "x-scheme-handler/codex"]
    else:
        assert app["method"] == "windows-uri" and not handler_probe.called
    with patch.object(panel_chats.sys, "platform", "linux"):
        try:
            panel_chats.open_chat(project, shared, "chat-opus", "app", dry_run=True)
        except ValueError:
            pass
        else:
            raise AssertionError("invented a Claude Code app deep link")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
        try:
            panel_chats.catalog(project, shared)
        except ValueError:
            pass
        else:
            raise AssertionError("worker navigated owner chats")
    with patch.object(panel, "read_field", side_effect=["1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat", return_value=opened) as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()):
        panel.browse_chats(project)
    assert selected.call_args.args[2] == "chat-sol"
    assert selected.call_args.kwargs == {"surface": "auto"}
    with patch.object(panel, "read_field", side_effect=["a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat", return_value=app) as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()):
        panel.browse_chats(project)
    assert selected.call_args.kwargs == {"surface": "app"}
    assert selected.call_args.args[2] == "chat-sol"
    with patch.object(panel, "read_field", side_effect=["", "a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [native]}), \
            patch.object(panel_chats, "open_chat") as selected:
        panel.browse_chats(project)
    selected.assert_not_called()
    claude_row = next(r for r in chats["rows"] if r["participant"] == "chat-opus")
    with patch.object(panel, "read_field", side_effect=["a1", "", ""]), \
            patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [claude_row]}), \
            patch.object(panel_chats, "open_chat") as selected, \
            patch.object(panel.sys, "stdout", io.StringIO()) as output:
        panel.browse_chats(project)
    selected.assert_not_called()
    assert "requires an enrolled native Codex main" in output.getvalue()
    for row, hint in ((native, "Codex app"), (claude_row, "Claude app sidebar")):
        with patch.object(panel, "read_field", side_effect=["1", "", ""]), \
                patch.object(panel_chats, "catalog", return_value={"room": shared, "rows": [row]}), \
                patch.object(panel_chats, "open_chat", side_effect=ValueError("navigation unavailable")), \
                patch.object(panel.sys, "stdout", io.StringIO()) as output:
            panel.browse_chats(project)
        assert hint in output.getvalue(), output.getvalue()
# A main's native model can change after launch; the view must follow its bound session.
import sqlite3
model_member = dict(next(m for m in room.status(project, shared)["participants"] if m["participant"] == "chat-sol"), state="working")
model_database = codex_home / "state_5.sqlite"
with sqlite3.connect(str(model_database)) as database:
    database.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, model TEXT, reasoning_effort TEXT, cwd TEXT)")
    database.execute("INSERT INTO threads VALUES (?,?,?,?)", (chat_codex, "gpt-6.1-sol", "xhigh", str(project.resolve())))
model_report = {"room": {"id": shared, "participants": [model_member]}, "attempts": {"active_recent": [
    {"attempt_id": chat_attempt, "state": "working", "panel": {"room_id": shared, "room_participant": "chat-sol",
     "role": "main", "model": "gpt-6-sol", "effort": "high"}}]}}
model_record_before = json.dumps(model_report, sort_keys=True)
with patch.dict(os.environ, chat_env, clear=True):
    current = panel_chats.current_models(project, [model_member])
    assert current["chat-sol"]["model"] == "gpt-6.1-sol" and current["chat-sol"]["effort"] == "xhigh"
    assert not panel_chats.current_models(project, [dict(model_member, role="worker")])
    assert not panel_chats.current_models(project, [dict(model_member, state="done")])
    assert not panel_chats.current_models(project, [dict(model_member, consumer="f" * 32)])
    status = collect(model_report, chat_attempt, repo=project)
    assert status["codex"]["model"] == "gpt-6.1-sol", status
    from room_view import nodes
    refreshed = dict(model_report, provider_status=status)
    member = nodes(refreshed)[0]
    assert member["model"] == "gpt-6.1-sol" and member["effort"] == "xhigh", member
    assert "Sol 6.1" in render(refreshed, "codex", 100, 26, view="graph"), refreshed
    from panel_view import hierarchy
    assert hierarchy(refreshed, [], chat_attempt)[0]["title"] == "MAIN Sol 6.1"
    for density in ("tree", "compact", "detail"):
        assert "Sol 6.1" in render(refreshed, "codex", 100, 40, view=density), density
    assert json.dumps(model_report, sort_keys=True) == model_record_before
    for phase in ("blocked", "orphaned", "waiting_input", "review"):
        paused = deepcopy(model_report)
        paused["attempts"]["active_recent"][0]["state"] = phase
        paused_status = collect(paused, chat_attempt, repo=project)
        assert paused_status["codex"]["model"] == "gpt-6.1-sol", (phase, paused_status)
        paused_status["codex"]["native_models"] = {}
        paused_status["codex"]["model"] = None
        missing = nodes(dict(paused, provider_status=paused_status))[0]
        assert missing["model"] is None and missing["effort"] is None and missing["model_at_launch"] == "gpt-6-sol", missing
    with patch.object(panel_chats, "current_models", return_value={}):
        paused_status = collect(paused, chat_attempt, repo=project)
        assert paused_status["codex"]["model"] is None, paused_status
    wrong = deepcopy(status)
    wrong["codex"]["native_models"]["chat-sol"]["consumer"] = "f" * 32
    assert nodes(dict(model_report, provider_status=wrong))[0]["model"] is None
    assert hierarchy(dict(model_report, provider_status=wrong), [], chat_attempt)[0]["title"] != "MAIN Sol 6"
    # Pi uses Codex's room provider identity, but its selected model is recorded
    # in the panel attempt rather than Codex's native thread database.
    pi_main = deepcopy(model_report)
    pi_main["attempts"]["active_recent"][0]["panel"]["harness"] = "pi"
    pi_member = dict(model_member, consumer="pi-session")
    pi_main["room"]["participants"] = [pi_member]
    pi_status = deepcopy(status)
    pi_status["codex"]["native_models"] = {}
    pi_main["provider_status"] = pi_status
    pi_node = nodes(pi_main)[0]
    assert pi_node["model"] == "gpt-6-sol" and pi_node["model_at_launch"] == "gpt-6-sol", pi_node
    assert hierarchy(pi_main, [], chat_attempt)[0]["title"] == "MAIN Sol 6"
    with sqlite3.connect(str(model_database)) as database:
        database.execute("UPDATE threads SET cwd=?", (str(temporary / "another-project"),))
    assert not panel_chats.current_models(project, [model_member]), "a foreign project supplied the model"
    with sqlite3.connect(str(model_database)) as database:
        database.execute("UPDATE threads SET cwd=?", (str(project.resolve()),))
    link = codex_home / "state_99.sqlite"
    os.link(model_database, link)
    try:
        assert not panel_chats.current_models(project, [model_member]), "linked native database was trusted"
    finally:
        link.unlink()
    old = codex_home / "state_4.sqlite"
    old.write_bytes(model_database.read_bytes())
    saved = model_database.with_suffix(".saved")
    model_database.rename(saved)
    model_database.write_text("unreadable newer database")
    try:
        assert not panel_chats.current_models(project, [model_member]), "an older model setting replaced a failed current read"
    finally:
        model_database.unlink()
        saved.rename(model_database)
    # The current setting can live only in WAL; immutable main-file reads would be stale.
    with sqlite3.connect(str(model_database)) as writer:
        writer.execute("PRAGMA journal_mode=WAL")
        writer.execute("UPDATE threads SET model=?, reasoning_effort=?", ("gpt-6.1-sol", "ultra"))
        writer.commit()
        wal = Path(str(model_database) + "-wal")
        before_native = [model_database.read_bytes(), wal.read_bytes()]
        assert panel_chats.current_models(project, [model_member])["chat-sol"]["effort"] == "ultra"
        assert [model_database.read_bytes(), wal.read_bytes()] == before_native, "reader changed native model rows"
        isolated = codex_home / "state_6.sqlite"
        isolated.write_bytes(model_database.read_bytes())
        isolated_wal = Path(str(isolated) + "-wal")
        isolated_wal.write_bytes(wal.read_bytes())
        native_files = sorted(p.name for p in codex_home.iterdir())
        assert not panel_chats.current_models(project, [model_member]), "missing WAL reader files must stay unavailable"
        assert sorted(p.name for p in codex_home.iterdir()) == native_files, "reader created missing native sidecars"
        isolated.unlink()
        isolated_wal.unlink()
    assert room.records(project, shared) == room_before_chats
    assert "PRIVATE_TRANSCRIPT_BODY" in chat_file.read_text(), "native history was changed"
# Navigation needs this repo's active main provenance; copied identifiers are insufficient.
valid_window = "@42\t" + chat_attempt + "\t" + shared + "\t%42\tchat-sol\t" + str(project.resolve()) + "\t$1\n"
for bad_window in (valid_window.replace(str(project.resolve()), str(temporary / "foreign-repo")),
                   valid_window.replace(chat_attempt, "att_" + "f" * 32),
                   "@42\tchat-sol\t" + shared + "\t%42\n"):
    def invalid_navigation(command, **kwargs):
        if command[0] == "bash":
            return navigation_run(command, **kwargs)
        return subprocess.CompletedProcess(command, 0,
            bad_window if command[1] == "list-windows" else "%42\t@42\n", "")
    with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
            patch.object(panel_chats.subprocess, "run", side_effect=invalid_navigation):
        assert panel_chats.windows(project, shared) == {}
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=tmux_navigation):
    mismatched = deepcopy(room.status(project, shared))
    next(p for p in mismatched["participants"] if p["participant"] == "chat-sol")["provider"] = "claude"
    assert panel_chats.windows(project, shared, mismatched) == {}
    wrong_room = deepcopy(mismatched)
    wrong_room["participants"] = [dict(wrong_room["participants"][0], participant="chat-sol", provider="codex", role="worker")]
    assert panel_chats.windows(project, shared, wrong_room) == {}
# A first launch has no native binding or stable-participant reference yet.
initial_room = room.create(project, "first-native-navigation", "First native navigation")
initial_attempt = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + initial_room,
    "--then", "starting", "--then", "working", output=True)
room.join(project, initial_room, initial_attempt, "codex")
def initial_navigation(command, **kwargs):
    if command[0] == "bash":
        return navigation_run(command, **kwargs)
    text = ("@42\t" + initial_attempt + "\t" + initial_room + "\t%42\t" + initial_attempt + "\t" + str(project.resolve()) + "\t$1\n"
            if command[1] == "list-windows" else "$1\n" if command[1] == "display-message" else "%42\t@42\n")
    return subprocess.CompletedProcess(command, 0, text, "")
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=initial_navigation):
    first_chat = panel_chats.catalog(project, initial_room)["rows"][0]
    assert first_chat["status"] == "native" and first_chat["session_id"] is None and first_chat["terminal"]["pane"] == "%42"
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition", "--attempt", initial_attempt, "--state", "cancelled"])
    assert panel_chats.windows(project, initial_room) == {}
# Explicit worker bindings retain routing, but are not owner app-navigation grants.
legacy_room = room.create(project, "legacy-native-worker", "Legacy role binding")
legacy_sid = "44444444-4444-4444-8444-444444444444"
legacy_hash = hashlib.sha256(legacy_sid.encode()).hexdigest()[:32]
legacy_rows = room.records(project, legacy_room)
legacy_row = dict(room.records(project, shared)[-1], thread=legacy_room, seq=legacy_rows[-1]["seq"] + 1,
                  text="Legacy fixture", room_event={"kind": "join", "participant": "legacy-worker",
                  "provider": "codex", "role": "worker", "label": "Legacy worker", "consumer": legacy_hash, "owns": []})
with (project / ".oms/threads" / (legacy_room + ".jsonl")).open("a") as output:
    output.write(json.dumps(legacy_row) + "\n")
(codex_day / ("rollout-x-" + legacy_sid + ".jsonl")).write_text(
    json.dumps({"type": "session_meta", "payload": {"id": legacy_sid}}) + "\n")
assert room.selected(project, legacy_hash) == (legacy_room, "legacy-worker")
assert room.selected(project, legacy_hash, legacy_room, "legacy-worker") == (legacy_room, "legacy-worker")
assert room.selected(project, legacy_hash, shared, child["participant"]) == (shared, child["participant"])
with patch.dict(os.environ, chat_env, clear=True):
    legacy_chats = panel_chats.catalog(project, legacy_room)
    assert legacy_chats["rows"][0]["status"] == "artifacts" and legacy_chats["rows"][0]["uri"] is None
    assert panel_chats.open_chat(project, legacy_room, "legacy-worker", "app", dry_run=True)["method"] == "artifacts"
room.join(project, legacy_room, "explicit-worker", "codex", role="worker", native_session="explicit-worker-session")
explicit_hash = hashlib.sha256(b"explicit-worker-session").hexdigest()[:32]
assert room.selected(project, explicit_hash) == (legacy_room, "explicit-worker")
reason_report = deepcopy(chats)
reason_report["rows"][0].update(joined=False, reason="ambiguous native identity")
assert " / left" in panel_chats.text(reason_report) and "ambiguous native identity" in panel_chats.text(reason_report)

# Desktop navigation uses the exact session and only an advertised native
# flag. Inspecting a plan must not run a model or force-close a live session.
if os.name != "nt":
  with patch.dict(os.environ, chat_env, clear=True), \
          patch.object(panel_chats.sys, "platform", "linux"), \
          patch.object(panel_chats, "catalog", return_value=chats):
    mime_command = ["xdg-mime", "query", "default", "x-scheme-handler/codex"]
    app_command = ["xdg-open", "codex://threads/" + chat_codex]
    for missing in ("xdg-open", "xdg-mime"):
        with patch.object(panel_chats.shutil, "which", side_effect=lambda tool: None if tool == missing else "/fixture/" + tool), \
                patch.object(panel_chats.subprocess, "run") as launched:
            try:
                panel_chats.open_chat(project, shared, "chat-sol", "app")
            except ValueError:
                pass
            else:
                raise AssertionError("missing Linux app tool was accepted")
            launched.assert_not_called()
    failures = (subprocess.CompletedProcess(mime_command, 0, "", ""),
                subprocess.CompletedProcess(mime_command, 1, "codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "../codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\nextra.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "x" * 257, ""),
                subprocess.TimeoutExpired(mime_command, 3))
    for failure in failures:
        with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
                patch.object(panel_chats.subprocess, "run", side_effect=(failure if isinstance(failure, Exception) else None),
                             return_value=(None if isinstance(failure, Exception) else failure)) as launched:
            try:
                panel_chats.open_chat(project, shared, "chat-sol", "app")
            except ValueError as error:
                assert "handler" in str(error)
            else:
                raise AssertionError("unverified Linux handler was accepted")
            assert launched.call_count == 1 and launched.call_args.args[0] == mime_command
    with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", side_effect=[
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\n", ""),
                subprocess.CompletedProcess(app_command, 0, "", "")]) as launched:
        requested = panel_chats.open_chat(project, shared, "chat-sol", "app")
        assert requested["status"] == "navigation_requested" and not requested["ui_observed"]
        assert requested["commands"] == [app_command]
        assert [call.args[0] for call in launched.call_args_list] == [mime_command, app_command]
    with patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", side_effect=[
                subprocess.CompletedProcess(mime_command, 0, "codex.desktop\n", ""),
                subprocess.CompletedProcess(mime_command, 0, "", "")]) as launched:
        assert panel_chats.open_chat(project, shared, "chat-sol", "app", dry_run=True)["method"] == "app-uri"
        try:
            panel_chats.open_chat(project, shared, "chat-sol", "app")
        except ValueError:
            pass
        else:
            raise AssertionError("a stale app plan launched without a current handler")
        assert [call.args[0] for call in launched.call_args_list] == [mime_command, mime_command]
    with patch.object(panel_chats, "terminal_commands", return_value=[]), \
            patch.object(panel_chats.shutil, "which", return_value="/fixture/tool"), \
            patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                mime_command, 0, "codex.desktop\n", "")) as launched:
        preferred = panel_chats.open_chat(project, shared, "chat-sol", dry_run=True)
        assert preferred["method"] == "existing-terminal"
        launched.assert_not_called()
for platform in ("darwin", "win32"):
    for supported in (True, False):
        with patch.dict(os.environ, chat_env, clear=True), \
                patch.object(panel_chats.sys, "platform", platform), \
                patch.object(panel_chats, "catalog", return_value=chats), \
                patch.object(panel_chats.shutil, "which", return_value="/fixture/claude"), \
                patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess(
                    [], 0, "--desktop" if supported else "--resume", "")) as probe:
            try:
                destination = panel_chats.open_chat(project, shared, "chat-opus", "app", dry_run=True)
                assert supported and destination["commands"] == [["/fixture/claude", "--desktop", "--resume", chat_claude]]
                assert not destination["ui_observed"]
            except ValueError:
                assert not supported
            assert probe.call_count == 1 and probe.call_args.args[0] == ["/fixture/claude", "--help"]
with patch.dict(os.environ, chat_env, clear=True), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/launcher"), \
        patch.object(panel_chats.subprocess, "run", side_effect=lambda command, **kwargs:
            subprocess.CompletedProcess(command, 0, "@42\tchat-sol\t" + shared + "\t%42\n" if command[1] == "list-windows" else "", "")):
    assert not next(r for r in panel_chats.catalog(project, shared)["rows"] if r["participant"] == "chat-sol")["terminal"]
with patch.dict(os.environ, chat_env, clear=True), patch.object(panel_chats, "catalog", return_value=chats), \
        patch.object(panel_chats.subprocess, "run", return_value=subprocess.CompletedProcess([], 1)):
    try:
        panel_chats.open_chat(project, shared, "chat-sol")
    except ValueError as error:
        assert "session was preserved" in str(error)
    else:
        raise AssertionError("failed navigation was reported as successful")
assert room.records(project, shared) == room_before_chats, "chat navigation cannot mutate shared history"
chat_cli_env = dict(chat_env)
chat_cli_env.pop("TMUX")
mime_fixture = binary / "xdg-mime"
mime_fixture.write_text("#!/usr/bin/env bash\n" +
                        "test \"$1 $2 $3\" = 'query default x-scheme-handler/codex' || exit 1\n" +
                        "printf 'codex.desktop\\n'\n")
mime_fixture.chmod(0o755)
opener_fixture = binary / "xdg-open"
opener_fixture.write_text("#!/usr/bin/env bash\nexit 0\n")
opener_fixture.chmod(0o755)
listed = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                        "--chats", "--json"], chat_cli_env))
assert listed["kind"] == "oms-panel-chats" and any(r["uri"] for r in listed["rows"])
chat_rows = listed["rows"]
assert all("not bound or outside bounded local scan" not in (row.get("reason") or "") for row in chat_rows)
chat_members = [
    {"participant": "unbound", "label": "Native task not recorded", "provider": "claude", "role": "main", "joined": True},
    {"participant": "outside", "label": "", "provider": "codex", "role": "main", "joined": True,
     "consumer": "a" * 32},
]
with patch.object(panel_chats.room, "status", return_value={"participants": chat_members}), \
        patch.object(panel_chats, "windows", return_value={}), \
        patch.object(panel_chats, "native_index", return_value={"codex": {}, "claude": {}}):
    chat_rows = panel_chats.catalog(project, "chat-copy")["rows"]
    assert chat_rows[0]["reason"] == "no native session recorded for this main", chat_rows
    assert chat_rows[1]["reason"] == "session not found in the recent local history (older sessions are not scanned)", chat_rows
    assert chat_rows[0]["title"] == "claude main (no task title)" and chat_rows[1]["title"] == "codex main (no task title)"
planned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                         "--open-chat", "chat-sol", "--chat-surface", "app", "--dry-run", "--json"], chat_cli_env))
assert planned["uri"] == "codex://threads/" + chat_codex and not planned["ui_observed"]
unselected_env = dict(chat_cli_env)
unselected_env.pop("OMS_ROOM_ID")
unselected = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--chats", "--json"],
                            env=unselected_env, capture_output=True, text=True)
assert unselected.returncode != 0 and "select an existing room first" in unselected.stderr
# Symlinked metadata must not become an app navigation target.
chat_file.unlink()
chat_file.symlink_to(codex_day / ("rollout-x-" + bad_codex + ".jsonl"))
with patch.dict(os.environ, chat_cli_env, clear=True):
    assert next(r for r in panel_chats.catalog(project, shared)["rows"] if r["participant"] == "chat-sol")["status"] == "unresolved"
end_fixture_mains()
# Historical app fixtures have no attempt to release; retire them before native-start coverage.
for fixture_main in ("opus-app", "current-app", "bad-sol", "chat-opus", "chat-sol"):
    room.append(project, shared, {"kind": "leave", "participant": fixture_main}, "Fixture main left")
# A new adapter opts into native open without changing a provider-specific panel branch.
_operator_block("native-adapters")
open_log = temporary / "native-open.json"
open_context = temporary / "native-context.txt"
native_adapter = binary / "oms-agent-adapter-roomtest"
native_adapter.write_text("#!/usr/bin/env python3\n" +
    "import json, os, pathlib, signal, sys, time\na=sys.argv[1:]\nassert a[0]=='open'\n" +
    "p=pathlib.Path(a[a.index('--context-file')+1])\n" +
    "pathlib.Path(os.environ['OMS_OPEN_CONTEXT']).write_text(p.read_text())\n" +
    "pathlib.Path(os.environ['OMS_OPEN_LOG']).write_text(json.dumps({'argv':a,'room':os.environ['OMS_ROOM_ID'],"
    "'participant':os.environ['OMS_ROOM_PARTICIPANT'],'attempt':os.environ['OMS_PANEL_MAIN_ATTEMPT'],'context':str(p)}))\n" +
    "if os.environ.get('OMS_OPEN_APPEND_LOG'):\n" +
    " with open(os.environ['OMS_OPEN_APPEND_LOG'],'a') as f: f.write(str(os.getpid())+'\\n')\n" +
    "if os.environ.get('OMS_OPEN_HOLD'): time.sleep(float(os.environ['OMS_OPEN_HOLD']))\n" +
    "if os.environ.get('OMS_OPEN_INT_LOG'):\n" +
    " signal.signal(signal.SIGINT, lambda n,f: pathlib.Path(os.environ['OMS_OPEN_INT_LOG']).write_text('interrupted turn'))\n" +
    " os.killpg(os.getpgrp(),signal.SIGINT)\n time.sleep(1)\n" +
    "if os.environ.get('OMS_OPEN_SIGNAL'):\n" +
    " os.kill(os.getppid(), getattr(signal, os.environ['OMS_OPEN_SIGNAL']))\n time.sleep(300)\n")
native_adapter.chmod(0o755)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                                OMS_OPEN_CONTEXT=str(open_context), OMS_PROVIDER_NATIVE_ADAPTERS="roomtest"), clear=True):
    inventory = panel.provider_catalog("roomtest")[0]
    assert inventory["native_launch"] and not open_log.exists(), "discovery must not execute open"
    assert panel.run_native("roomtest", project, task="Inspect source; no unrelated work") == 0
opened = json.loads(open_log.read_text())
assert opened["room"] == shared and opened["participant"]
assert not Path(opened["context"]).exists(), "native context lifetime ends with its process"
assert "OMS room" in open_context.read_text() and "Inspect source; no unrelated work" in open_context.read_text()
assert "--dangerously" not in " ".join(opened["argv"])
recovery_env = dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                    OMS_OPEN_CONTEXT=str(open_context), OMS_PROVIDER_NATIVE_ADAPTERS="roomtest")
with patch.dict(os.environ, recovery_env, clear=True):
    assert panel.run_native("roomtest", project, resume="fixture-stable-session") == 0
    original_main = json.loads(open_log.read_text())
    member = original_main["participant"]
    room.send(project, shared, room_owner, member, "Mail while the native main was offline", message_id="offline-mail")
    before_resume = room.records(project, shared)
    first_delta = room.updates(project, shared, member)
    delivered = list(first_delta["turns"])
    for _ in range(len(before_resume)):
        if not first_delta["has_more"]:
            break
        first_delta = room.updates(project, shared, member, first_delta["cursor"])
        delivered += first_delta["turns"]
    assert not first_delta["has_more"]
    assert any(row["room_event"]["id"] == "offline-mail" for row in delivered)
    with patch.object(room, "MAX_PARTICIPANTS", len(room.status(project, shared)["participants"])):
        assert panel.run_native("roomtest", project, resume="fixture-stable-session", model="fixture-model") == 0
    resumed = json.loads(open_log.read_text())
    assert resumed["participant"] == member and resumed["attempt"] != original_main["attempt"]
    assert room.records(project, shared) == before_resume, "resume cannot reset the subscription or consume a participant slot"
    assert room.status(project, shared)["pending_count"] > 0, "read does not acknowledge offline mail"
    assert not room.updates(project, shared, member, first_delta["cursor"])["turns"], "resume preserves cursor anchors"
    # An existing active successor blocks another resume even though the
    # original participant-named attempt was terminal.
    active = panel.events(project, "start", "--provider", "roomtest", "--tool", "panel-main", "--ref", "panel_role=main",
                          "--ref", "panel_room_id=" + shared, "--ref", "panel_room_participant=" + member,
                          "--then", "starting", "--then", "working", output=True)
    with patch.object(panel, "native_command", side_effect=AssertionError("unknown owner must not launch")):
        try:
            panel.run_native("roomtest", project, resume="fixture-stable-session")
        except ValueError as error:
            assert "live main or unverified owner" in str(error), error
            assert "--release-main " + active.strip() in str(error) and len(str(error)) <= 140, error
        else:
            raise AssertionError("active successor was ignored")
    panel.events(project, "transition", "--attempt", active, "--state", "cancelled")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
        try:
            panel.run_native("roomtest", project, resume="fixture-stable-session")
        except ValueError:
            pass
        else:
            raise AssertionError("worker opened a native main")
    for bad_tool, bad_provider in (("consult", "roomtest"), ("panel-main", "codex")):
        bad_room = room.create(project, "bad-owner-" + bad_tool, "Ownership evidence fixture")
        impostor = panel.events(project, "start", "--provider", bad_provider, "--tool", bad_tool, "--ref", "panel_role=main",
                                  "--ref", "panel_room_id=" + bad_room,
                                  "--then", "starting", "--then", "working", output=True)
        panel.events(project, "transition", "--attempt", impostor, "--state", "cancelled")
        room.join(project, bad_room, impostor, "roomtest", native_session="fixture-bad-owner")
        try:
            with patch.dict(os.environ, {"OMS_ROOM_ID": bad_room}), \
                    patch.object(panel, "native_command", side_effect=AssertionError("bad provenance must not launch")):
                panel.run_native("roomtest", project, resume="fixture-bad-owner")
        except ValueError as error:
            assert "terminal owner" in str(error)
        else:
            raise AssertionError("resume accepted unrelated ownership evidence")
# A foreign-room racer must not take the main lease away from the legitimate
# resume. The legitimate native stays active beyond the existing lock wait.
race_log = temporary / "native-resume-race.txt"
race_env = dict(recovery_env, OMS_OPEN_HOLD="14", OMS_OPEN_APPEND_LOG=str(race_log))
race_script = ("import sys\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
               "try: p.run_native('roomtest',Path(sys.argv[2]),resume='fixture-stable-session')\n"
               "except ValueError as e: print(str(e));sys.exit(75)\n")
other_race_room = room.create(project, "parallel-resume-room", "Concurrent resume fixture")
racers = [subprocess.Popen([sys.executable, "-c", race_script, str(root / "scripts/lib"), str(project)],
                           env=dict(race_env, OMS_ROOM_ID=ident), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
          for ident in (other_race_room, shared)]
try:
    outcomes = [r.communicate(timeout=35) for r in racers]
    assert [r.returncode for r in racers] == [75, 0], outcomes
    assert "native session is bound to another room" in outcomes[0][0], outcomes
    assert len(race_log.read_text().splitlines()) == 1, "concurrent resumes launched two native owners"
finally:
    for racer in racers:
        if racer.poll() is None:
            racer.kill(); racer.wait(timeout=5)
# Native references link the stable participant even when attempt ids differ,
# and an unrelated room's same spelling cannot override it.
import room_view as graph_view
native_report = {"room": room.status(project, shared), "attempts": {"active_recent": [
    {"attempt_id": "unqualified-attempt", "state": "failed", "panel": {"role": "main", "room_participant": member}},
    {"attempt_id": "fresh-attempt", "state": "working", "panel": {"role": "main", "room_id": shared,
     "room_participant": member, "model": "fixture-model"}},
    {"attempt_id": "foreign-attempt", "state": "failed", "panel": {"role": "main", "room_id": "foreign-room",
     "room_participant": member}}], "recent": []}}
linked = next(row for row in graph_view.nodes(native_report) if row["participant"] == member)
assert linked["state"] == "working" and linked["model"] == "fixture-model"
# Main turn presence changes only presentation, including the control graph's main rows.
activity_main = {"participant": "activity-main", "role": "main", "joined": True,
                 "provider": "claude", "model": "fixture-model", "seq": 1}
activity_worker = {"participant": "activity-worker", "role": "worker", "joined": True,
                   "provider": "codex", "model": "gpt-6-sol", "parent": "activity-attempt", "seq": 2}
activity_report = {"collection": {"ok": True}, "room": {"id": "activity-room", "participants": [activity_main, activity_worker]},
                   "attempts": {"recent": [], "active_recent": [
                       {"attempt_id": "activity-attempt", "state": "working", "panel": {"role": "main",
                        "room_id": "activity-room", "room_participant": "activity-main"}},
                       {"attempt_id": "activity-child", "state": "working", "panel": {"role": "worker",
                        "room_id": "activity-room", "room_participant": "activity-worker"}}]}}
activity_row = {"schema": 1, "participant": "activity-main", "room": "activity-room",
                "attempt": "activity-attempt", "state": "busy", "since": 100, "updated_at": 100}
activity_repo = temporary / "main-activity"
activity_path = activity_repo / ".oms/hooks/panel-activity/activity-main.json"
activity_path.parent.mkdir(parents=True)
activity_path.write_text(json.dumps(activity_row))
activity_windows = [["activity-main", "%777", str(activity_repo), "activity-attempt"]]
import hook_state
activity_env = {"OMS_PANEL_SESSION": "oms-fixture", "OMS_PANEL_MAIN_ATTEMPT": "activity-attempt",
                "OMS_ROOM_PARTICIPANT": "activity-main", "OMS_ROOM_ID": "activity-room",
                "OMS_ROOM_REPO": str(activity_repo)}
with patch.dict(os.environ, activity_env):
    activity_path.unlink()
    with patch.object(hook_state.time, "time", return_value=100):
        hook_state.record_panel_activity({"hook_event_name": "UserPromptSubmit", "prompt": "panel bootstrap"}, activity_repo)
    bootstrap = json.loads(activity_path.read_text())
    assert bootstrap["origin"] == "harness" and "panel bootstrap" not in activity_path.read_text(), bootstrap
    with patch.object(hook_state.time, "time", return_value=200):
        hook_state.record_panel_activity({"hook_event_name": "UserPromptSubmit", "prompt": "my manual request"}, activity_repo)
    user_prompt = json.loads(activity_path.read_text())
    assert user_prompt["origin"] == "user" and "my manual request" not in activity_path.read_text(), user_prompt
    assert graph_view.main_state(dict(activity_main, attempt="activity-attempt", state="working",
                                      panel_activity=user_prompt)) == "manual"
    assert graph_view.main_state(dict(activity_main, attempt="activity-attempt", state="working",
                                      panel_activity=dict(user_prompt, state="waiting", kind="approval"))) == "waiting_approval"
    assert graph_view.main_state(dict(activity_main, attempt="activity-attempt", state="working",
                                      panel_activity=dict(user_prompt, state="waiting", kind="input"))) == "waiting_input"
    with patch.object(hook_state.time, "time", return_value=250):
        hook_state.record_panel_activity({"hook_event_name": "Stop"}, activity_repo)
    completed_user_turn = json.loads(activity_path.read_text())
    assert completed_user_turn["state"] == "idle"
    assert graph_view.main_state(dict(activity_main, attempt="activity-attempt", state="working",
                                      panel_activity=completed_user_turn)) == "idle"
    with patch.object(hook_state.time, "time", return_value=300):
        hook_state.record_panel_activity({"hook_event_name": "UserPromptSubmit",
                                          "prompt": " \n<task-notification>background complete"}, activity_repo)
    harness_prompt = json.loads(activity_path.read_text())
    assert harness_prompt["origin"] == "harness" and "background complete" not in activity_path.read_text(), harness_prompt
    with patch.object(hook_state.time, "time", return_value=301):
        hook_state.record_panel_activity({"hook_event_name": "UserPromptSubmit",
                                          "prompt": 'Another Claude session sent a message:\n<agent-message from="a1">[Subagent hand-back] handback body'}, activity_repo)
    handback_prompt = json.loads(activity_path.read_text())
    assert handback_prompt["origin"] == "harness" and "handback body" not in activity_path.read_text(), handback_prompt
    assert graph_view.main_state(dict(activity_main, attempt="activity-attempt", state="working",
                                      panel_activity=harness_prompt)) == "working"

claude_home = temporary / "claude-home"
transcript = claude_home / "projects" / "fixture-project" / "session.jsonl"
transcript.parent.mkdir(parents=True)
transcript.write_text(json.dumps({"type": "user", "timestamp": "1970-01-01T00:05:01Z",
                                 "message": {"role": "user", "content": "[Request interrupted by user]"}}) + "\n")
interrupted_row = dict(activity_row, since=300, updated_at=300, origin="user", transcript_path=str(transcript))
activity_path.write_text(json.dumps(interrupted_row))
assert "[Request interrupted by user]" not in activity_path.read_text(), "transcript text stays out of activity records"
with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(claude_home)}):
    panel.MAIN_INTERRUPT_CACHE.clear()
    real_open = os.open
    transcript_opens = []
    def count_transcript_open(path, *args, **kwargs):
        if str(path) == str(transcript):
            transcript_opens.append(path)
        return real_open(path, *args, **kwargs)
    with patch.object(panel.os, "open", side_effect=count_transcript_open):
        interrupted_activity = panel.main_activity(activity_repo, [activity_main])
        cached_interrupted = panel.main_activity(activity_repo, [activity_main])
    assert len(transcript_opens) == 1, transcript_opens
    assert cached_interrupted["activity-main"].get("interrupted") is True, cached_interrupted
    quoted = transcript.parent / "quoted.jsonl"
    quoted.write_text(json.dumps({"type": "assistant", "timestamp": "1970-01-01T00:05:02Z",
                                  "message": {"role": "assistant", "content": "[Request interrupted by user]"}}) + "\n")
    activity_path.write_text(json.dumps(dict(interrupted_row, transcript_path=str(quoted))))
    assert not panel.main_activity(activity_repo, [activity_main])["activity-main"].get("interrupted"), \
        "assistant text quoting the interrupt marker is not an interrupt"
    harness_interrupt = transcript.parent / "harness-interrupt.jsonl"
    harness_interrupt.write_text(json.dumps({"type": "user", "timestamp": "1970-01-01T00:05:03Z",
        "message": {"role": "user", "content": [{"type": "text",
        "text": "[Request interrupted by user for tool use]"}]}}) + "\n")
    harness_activity = dict(interrupted_row, origin="harness", transcript_path=str(harness_interrupt))
    activity_path.write_text(json.dumps(harness_activity))
    assert panel.main_activity(activity_repo, [activity_main])["activity-main"].get("interrupted") is True, \
        "Claude's user-role text-block interrupt marks a harness-origin turn interrupted"
    partial = transcript.parent / "partial.jsonl"
    partial.write_bytes(b" " * 16384 + (json.dumps({"type": "user", "timestamp": "1970-01-01T00:05:03Z",
                   "message": {"role": "user", "content": "[Request interrupted by user]"}}).encode("utf-8") + b"\n"))
    activity_path.write_text(json.dumps(dict(interrupted_row, transcript_path=str(partial))))
    assert not panel.main_activity(activity_repo, [activity_main])["activity-main"].get("interrupted"), \
        "the partial first line of a transcript tail is ignored"
    activity_path.write_text(json.dumps(interrupted_row))
    interrupted_activity = panel.main_activity(activity_repo, [activity_main])
    assert interrupted_activity["activity-main"].get("interrupted") is True, interrupted_activity
    interrupted_report = dict(activity_report, main_activity=interrupted_activity)
    interrupted_main = next(m for m in graph_view.nodes(interrupted_report) if m["role"] == "main")
    assert graph_view.main_state(interrupted_main) == "manual"
    assert graph_view.execution_tag(interrupted_main) == status_tag("working")
    with patch.dict(os.environ, activity_env):
        with patch.object(hook_state.time, "time", return_value=302):
            hook_state.record_panel_activity({"hook_event_name": "UserPromptSubmit",
                                              "prompt": "<task-notification>next turn"}, activity_repo)
        assert "next turn" not in activity_path.read_text(), "harness prompt text stays out of activity records"
        cleared_activity = panel.main_activity(activity_repo, [activity_main])
        assert not cleared_activity["activity-main"].get("interrupted"), cleared_activity
        cleared_main = next(m for m in graph_view.nodes(dict(activity_report, main_activity=cleared_activity))
                            if m["role"] == "main")
        assert graph_view.main_state(cleared_main) == "working", cleared_main
activity_path.write_text(json.dumps(activity_row))
panel.MAIN_OUTPUT.clear()
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"native prompt", b"")) as capture:
    with patch.object(panel.time, "time", return_value=1000):
        assert "activity-main" in panel.main_activity(activity_repo, [activity_worker] * 129 + [activity_main])
        activity_before = activity_path.read_bytes()
        first = panel.main_activity(activity_repo, [activity_main, activity_worker], activity_windows)
        assert first["activity-main"]["state"] == "busy"
        assert first["activity-main"]["since"] == activity_row["since"]
        assert activity_path.read_bytes() == activity_before, "reading activity must not rewrite hook evidence"
    with patch.object(panel.time, "time", return_value=1005):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows) == first
    with patch.object(panel.time, "time", return_value=100 + 7 * 24 * 60 * 60):
        silent = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert silent["activity-main"]["state"] == "quiet" and silent["activity-main"]["since"] == 1000, silent
        activity_path.write_text(json.dumps(dict(activity_row, origin="user")))
        manual = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert manual["activity-main"]["state"] == "busy" and manual["activity-main"]["origin"] == "user", manual
        for kind in ("approval", "input"):
            activity_path.write_text(json.dumps(dict(activity_row, state="waiting", kind=kind)))
            before_capture = capture.call_count
            waiting = panel.main_activity(activity_repo, [activity_main], activity_windows)
            assert waiting["activity-main"]["state"] == "waiting" and waiting["activity-main"]["kind"] == kind, waiting
            assert capture.call_count == before_capture, "waiting is explicit hook evidence, not pane silence"
    activity_path.write_text(json.dumps(dict(activity_row, updated_at=1090)))
    with patch.object(panel.time, "time", return_value=1092):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy", "a fresh heartbeat prevents silence fallback"
    activity_path.write_text(json.dumps(activity_row))
    assert "native prompt" not in repr(panel.MAIN_OUTPUT)
    assert capture.call_args.args[0][-5:] == ["-p", "-t", "%777", "-S", "0"], capture.call_args
    capture.return_value = subprocess.CompletedProcess([], 0, b"new native output", b"")
    with patch.object(panel.time, "time", return_value=1102):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy"
    capture.return_value = subprocess.CompletedProcess([], 1, b"", b"")
    with patch.object(panel.time, "time", return_value=1200):
        assert panel.main_activity(activity_repo, [activity_main], activity_windows)["activity-main"]["state"] == "busy", "a failed capture cannot prove silence"
    assert not panel.MAIN_OUTPUT
    activity_path.write_text(json.dumps(dict(activity_row, attempt="fresh", since=1201, updated_at=1201)))
    activity_windows[0][-1] = "fresh"
    with patch.object(panel.time, "time", return_value=1202):
        restarted = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert restarted["activity-main"]["state"] == "busy" and restarted["activity-main"]["attempt"] == "fresh"
        assert restarted["activity-main"]["since"] == 1201, restarted
    for bad_row in (None, "{", dict(activity_row, participant="other"),
                    dict(activity_row, state="waiting"), dict(activity_row, state="waiting", kind="unknown")):
        if bad_row is None:
            activity_path.unlink()
        elif isinstance(bad_row, str):
            activity_path.write_text(bad_row)
        else:
            activity_path.write_text(json.dumps(bad_row))
        with patch.object(panel.time, "time", return_value=1300):
            assert not panel.main_activity(activity_repo, [activity_main], activity_windows), bad_row
    activity_path.write_text(json.dumps(dict(activity_row, attempt="old")))
    with patch.object(panel.time, "time", return_value=1300):
        mismatched = panel.main_activity(activity_repo, [activity_main], activity_windows)
        assert graph_view.main_state(dict(activity_main, state="working", attempt="fresh",
                                          panel_activity=mismatched["activity-main"])) == "live marker"
    assert graph_view.main_state(dict(activity_main, state="exited", attempt="fresh",
                                      panel_activity=dict(activity_row, attempt="fresh", state="busy"))) == "exited", \
        "terminal attempt state takes precedence over a matching busy activity row"
for reading, word in (({}, "live"), ({"activity-main": dict(activity_row, since=280)}, "working · 12m"),
                      ({"activity-main": dict(activity_row, state="quiet", since=280)}, "quiet · 12m"),
                      ({"activity-main": dict(activity_row, state="idle", since=280)}, "idle · 12m"),
                      ({"activity-main": dict(activity_row, origin="user", since=280)}, "manual"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="approval", origin="user", since=280)}, "needs approval · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="input", origin="user", since=280)}, "needs input · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="approval", since=280)}, "needs approval · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="input", since=280)}, "needs input · 12m"),
                      ({"activity-main": dict(activity_row, state="waiting", kind="approval", attempt="old")}, "live"),
                      ({"activity-main": dict(activity_row, state="idle", since=280, attempt="old")}, "live")):
    report = dict(activity_report, main_activity=reading)
    with patch.object(panel.time, "time", return_value=1000):
        projected_main = next(m for m in graph_view.nodes(report) if m["role"] == "main")
        assert projected_main["state"] == "working", "presentation must preserve attempt lifecycle"
        expected_state = ("waiting_approval" if word.startswith("needs approval") else "waiting_input" if word.startswith("needs input")
                          else "idle" if word.startswith("idle") else "quiet" if word.startswith("quiet")
                          else "manual" if word == "manual" else "working" if word.startswith("working") else "live marker")
        assert graph_view.main_state(projected_main) == expected_state
        assert graph_view.main_words(projected_main) == word
        if word == "manual":
            assert graph_view.execution_tag(projected_main) == status_tag("working")
        for ascii_only in (False, True):
            expected = word.replace(" · ", " / ") if ascii_only else word
            for activity_view, menu in (("graph", False), ("tree", False), ("graph", True)):
                navigation = {}
                screen = render(report, "claude", 120, 44, view=activity_view, menu=menu,
                                frame=2, unicode=not ascii_only, navigation=navigation)
                assert expected in screen and "running" in screen, (activity_view, screen)
                if word == "manual":
                    assert status_tag("working", unicode=not ascii_only) in screen and "status unknown" not in screen, screen
                if ascii_only:
                    screen.encode("ascii")
                if word.startswith("idle") and activity_view == "graph":
                    assert status_tag("idle", unicode=not ascii_only) in screen and expected in screen, screen
        for ascii_only in (False, True):
            legacy = render(report, "claude", 120, 44, view="compact", main_attempt="activity-attempt",
                            frame=2, unicode=not ascii_only)
            assert (word.replace(" · ", " / ") if ascii_only else word) in legacy, legacy
            if word == "manual":
                assert status_tag("working", unicode=not ascii_only) in legacy and "status unknown" not in legacy, legacy
            if ascii_only:
                legacy.encode("ascii")
        from panel_view import inbox_items
        waiting = word.startswith("needs")
        assert panel.main_needs_attention(report, "activity-attempt") == waiting
        assert panel.main_needs_attention(report, "activity-main") == waiting
        assert not panel.main_needs_attention(report, "unrelated-main")
        if waiting:
            items = inbox_items(report)
            assert len(items) == 1 and items[0]["what"] == "waiting for " + ("approval" if "approval" in word else "input"), items
            assert items[0]["action"] == ("chat", "activity-main") and items[0]["style"] == "alert", items
            ended = dict(report, attempts={"recent": [], "active_recent": [
                dict(activity_report["attempts"]["active_recent"][0], state="done")]})
            assert not panel.main_needs_attention(ended, "activity-attempt") and not inbox_items(ended)
        else:
            assert inbox_items(report) == inbox_items(activity_report)
with patch.object(panel.time, "time", return_value=1000):
    activity_path.write_text(json.dumps(dict(activity_row, updated_at=1001)))
    assert not panel.main_activity(activity_repo, [activity_main]), "future timestamps are untrusted"
    activity_path.write_text("x" * 4097)
    assert not panel.main_activity(activity_repo, [activity_main]), "activity reads must be bounded"
    activity_path.unlink()
    activity_path.symlink_to(activity_repo / "missing-target")
    assert not panel.main_activity(activity_repo, [activity_main]), "never follow activity symlinks"
# A council's seats inherit the main's participant; the graph shows the debate
# beside the main instead of relabeling the main card as a seat.
council_meta = {"role": "advisor", "label": "Pick next work", "room_id": shared, "room_participant": member}
native_report["attempts"]["active_recent"][:0] = [
    {"attempt_id": "seat-attempt", "parent_attempt_id": "fresh-attempt", "state": "working", "tool": "ask",
     "task_id": "decide", "panel": dict(council_meta, model="claude-fable-5-1")},
    {"attempt_id": "council-attempt", "parent_attempt_id": "fresh-attempt", "state": "working",
     "tool": "panel-council", "task_id": "decide", "panel": council_meta}]
linked = next(row for row in graph_view.nodes(native_report) if row["participant"] == member)
assert linked["attempt"] == "fresh-attempt" and linked["model"] == "fixture-model", linked
debate_graph = graph_view.render_graph(native_report, 110, 40)
assert "COUNCIL 1 seat(s) answering" in debate_graph and "Debate: Pick next work" in debate_graph, debate_graph
debate_navigation = {}
debate_connected = render(native_report, "claude", 91, 40, view="tree", main_attempt="fresh-attempt",
                          navigation=debate_navigation)
assert "COUNCIL (1)" in debate_connected and "Debate: Pick next work" in debate_connected, debate_connected
assert "1 seat(s) answering" in debate_connected and "MAIN fixture-model" in debate_connected, debate_connected
assert ("debate", ("decide", member)) in debate_navigation["items"]
council_only = dict(native_report, room=dict(native_report["room"], participants=[
    m for m in native_report["room"]["participants"] if m["participant"] == member]))
council_detail_nav = {}
council_detail = render(council_only, "claude", 91, 40, view="tree",
                        main_attempt="fresh-attempt", navigation=council_detail_nav)
assert "MAIN fixture-model" in council_detail and "1 seat(s) answering" in council_detail, council_detail
assert ("debate", ("decide", member)) in council_detail_nav["items"], council_detail_nav
# Claude Code's built-in advisor shows above its main only while unanswered, read from the
# main's own transcript tail; finalized failed calls leave the board.
from datetime import datetime, timezone
advisor_home = temporary / "advisor-claude"
(advisor_home / "projects" / "slug").mkdir(parents=True)
(advisor_home / "settings.json").write_text('{"advisorModel": "fable"}')
advisor_sid = "0b1c2d3e-4f50-4a6b-8c7d-9e0f1a2b3c4d"
advisor_consumer = hashlib.sha256(advisor_sid.encode()).hexdigest()[:32]
advisor_log = advisor_home / "projects" / "slug" / (advisor_sid + ".jsonl")
advisor_use = {"timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z"), "message": {"content": [
    {"type": "server_tool_use", "name": "advisor", "id": "srvtoolu_1"}]}}
advisor_log.write_text("partial{\n" + json.dumps(advisor_use) + "\nnot json advisor\n")
claude_main = {"participant": "adv-main", "provider": "claude", "role": "main", "joined": True, "label": "Claude",
               "consumer": advisor_consumer, "seq": 1}
with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(advisor_home)}):
    open_call = panel_chats.native_advisors([claude_main])
    assert open_call["adv-main"]["model"] == "Fable 5.1" and len(open_call["adv-main"]["started"]) == 5, open_call
    assert open_call["adv-main"]["running"] is True and "finished" not in open_call["adv-main"], open_call
    assert panel_chats.native_advisors([dict(claude_main, provider="codex")]) == {}
    advisor_log.write_text(advisor_log.read_text() + json.dumps({"timestamp": advisor_use["timestamp"], "message": {"content": [
        {"type": "advisor_tool_result", "tool_use_id": "srvtoolu_1", "content": "ciphertext"}]}}) + "\n")
    answered_call = panel_chats.native_advisors([claude_main])
    assert answered_call["adv-main"]["running"] is False and len(answered_call["adv-main"]["finished"]) == 5, answered_call
    stale_stamp = "2020-01-01T00:00:00.000Z"
    advisor_log.write_text(json.dumps(dict(advisor_use, timestamp=stale_stamp)) + "\n" + json.dumps({
        "timestamp": stale_stamp, "message": {"content": [{"type": "advisor_tool_result", "tool_use_id": "srvtoolu_1"}]}}) + "\n")
    assert panel_chats.native_advisors([claude_main]) == {}
board = {"room": {"id": "adv-room", "participants": [claude_main]}, "attempts": {"active_recent": [], "recent": []}}
plain_board = graph_view.render_graph(board, 100, 40)
assert "Fable 5.1" not in plain_board and "Built-in advisor" not in plain_board, plain_board
advisor_board = graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40)
assert "Fable 5.1" in advisor_board and "Built-in advisor" in advisor_board, advisor_board
nav = {"tab": "detail", "selected": ("result", "native-advisor-adv-main"), "preview": {"target": ("result", "native-advisor-adv-main"), "report": {}}}
native_detail = graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40, navigation=nav)
assert "answer stays inside the main" in native_detail, native_detail
assert "Artifact:" not in native_detail and "not recorded" not in native_detail, native_detail
# An answered call keeps its card above the main instead of counting as finished; the detail says when it answered.
finished_board = graph_view.render_graph(dict(board, native_advisors=answered_call), 120, 34,
                                         navigation={"dismissed": True})
assert "Fable 5.1" in finished_board and "Past work" not in finished_board, finished_board
finished_nav = {"tab": "detail", "selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
finished_detail = graph_view.render_graph(dict(board, native_advisors=answered_call), 100, 40, navigation=finished_nav)
assert "Built-in advisor: last answered " + answered_call["adv-main"]["finished"] in finished_detail, finished_detail
running_nav = {"tab": "detail", "selected": ("chat", "adv-main"), "preview": {"target": ("chat", "adv-main"), "report": {}}}
assert "Built-in advisor: asked " in graph_view.render_graph(dict(board, native_advisors=open_call), 100, 40, navigation=running_nav)
# Claude Code's own subagents and Workflow runs: read-only rows that show only what the session files prove.
session_dir = advisor_home / "projects" / "slug" / advisor_sid
agents_dir = session_dir / "subagents"
runs_dir = agents_dir / "workflows"
(session_dir / "workflows").mkdir(parents=True)
runs_dir.mkdir(parents=True)


def write_agent(ident, age=0, **meta):
    (agents_dir / ("agent-%s.meta.json" % ident)).write_text(json.dumps(meta))
    log = agents_dir / ("agent-%s.jsonl" % ident)
    log.write_text("")
    os.utime(log, (time.time() - age,) * 2)


def write_journal(run, rows, age=0):
    (runs_dir / run).mkdir(exist_ok=True)
    journal = runs_dir / run / "journal.jsonl"
    journal.write_text("".join(json.dumps(row) + "\n" for row in rows))
    os.utime(journal, (time.time() - age,) * 2)
    return journal


def sub_board(found, tab="log", selected=None):
    preview = {"target": selected, "report": {}} if selected else {}
    nav = {"group_tree": True, "tab": tab, "selected": selected, "preview": preview}
    return graph_view.render_graph(dict(board, native_subagents=found), 110, 44, navigation=nav), nav


def shown(found):
    return {kind: {row["id"]: row for row in found.get("adv-main", {}).get(kind, [])} for kind in ("agents", "workflows")}


started = [{"type": "launched", "key": "k1"}, {"type": "started", "key": "k1", "agentId": "w1", "phase": "Find"},
           {"type": "started", "key": "k2", "agentId": "w2", "phase": "Review"}, {"type": "result", "key": "k1"}]
stale = panel_chats.NATIVE_ACTIVE + 60
with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(advisor_home)}):
    # One rule for agents and workflows: a row lasts while its own file was written lately, and claims nothing more.
    write_agent("a1", description="Audit the parser", agentType="general-purpose")
    write_agent("a2", age=stale, name="stale")
    write_agent("a3", description="no meta file")
    (agents_dir / "agent-a3.meta.json").unlink()
    write_agent("a4")
    (agents_dir / "agent-a4.meta.json").write_text("{not json")
    (agents_dir / "agent-a5.jsonl").symlink_to(agents_dir / "agent-a1.jsonl")
    found = panel_chats.native_subagents([claude_main])
    agents = shown(found)["agents"]
    assert set(agents) == {"a1", "a3", "a4"} and found["adv-main"]["workflows"] == [], found
    assert agents["a1"]["title"] == "Audit the parser" and agents["a3"]["title"] == "Claude Code agent", agents
    with patch.object(panel_chats.os, "getuid", return_value=os.getuid() + 1):
        assert panel_chats.native_subagents([claude_main]) == {}, "a foreign-owned file must not be read"
    assert panel_chats.native_subagents([dict(claude_main, provider="codex")]) == {}
    text, nav = sub_board(found, "detail", ("chat", "adv-main"))
    assert "SUBAGENTS (3)" in text and "Audit the parser - active " in text and "[? status unknown]" in text, text
    assert "[⟳ running]" not in text and "not a panel worker" in text, text
    assert "Native agents active: 3 · workflows active: 0" in text, text
    assert ("result", "native-agent-adv-main-a1") not in nav["items"], nav
    assert "Past 0" in sub_board(found, "past")[0]
    for number in range(40):
        write_agent("b%02d" % number, description="bulk")
    assert len(shown(panel_chats.native_subagents([claude_main]))["agents"]) == panel_chats.NATIVE_MAX
    for leftover in agents_dir.glob("agent-*"):
        leftover.unlink()
    # A workflow row adds phase and approximate counts when its journal tail parses, and only its name otherwise.
    write_journal("wf_r1", started + [{"type": "future-kind"}])
    with (runs_dir / "wf_r1" / "journal.jsonl").open("a") as handle:
        handle.write('{"type": "resu')
    (runs_dir / "wf_r2").mkdir()
    (runs_dir / "wf_r2" / "journal.jsonl").write_text("not json at all\n")
    write_journal("wf_old", started, age=stale)
    (runs_dir / "wf_empty").mkdir()
    (session_dir / "workflows" / "wf_r1.json").write_text("{}")  # a completion file changes nothing
    found = panel_chats.native_subagents([claude_main])
    flows = shown(found)["workflows"]
    assert set(flows) == {"wf_r1", "wf_r2"} and flows["wf_r2"]["title"] == "Workflow wf_r2", flows
    assert flows["wf_r1"]["title"] == "Workflow wf_r1 · phase Review · agents ~2 started · ~1 done", flows
    text, nav = sub_board(found, "detail", ("chat", "adv-main"))
    assert "SUBAGENTS (2)" in text and "Native agents active: 0 · workflows active: 2" in text, text
    write_journal("wf_r1", started, age=3 * 3600)
    assert "wf_r1" not in shown(panel_chats.native_subagents([claude_main]))["workflows"]
    # No session directory, or an unreadable transcript index, leaves the board as it was.
    shutil.rmtree(session_dir)
    assert panel_chats.native_subagents([claude_main]) == {}
    panel_chats._transcripts.clear()
    with patch.object(panel_chats, "native_index", side_effect=RuntimeError("scan failed")):
        unreadable = panel_chats.native_subagents([claude_main])
    assert unreadable == {} and "SUBAGENT" not in sub_board(unreadable)[0]
    panel_chats._transcripts.clear()
# The board's last row starts a main: clickable only in a managed panel, never on a static render.
starter_board = dict(board, room={"id": "adv-room", "participants": [dict(claude_main, model="claude-opus-5-5"), dict(claude_main, participant="adv-sub",
                     provider="codex", seq=2, consumer=None, model="gpt-6-sol")]}, collection={"ok": True},
    attempts={"active_recent": [], "recent": [{"attempt_id": ident, "state": "done", "panel": {
        "role": "main", "room_id": "adv-room", "room_participant": ident}} for ident in ("adv-main", "adv-sub")]})
assert "[+ " not in graph_view.render_graph(starter_board, 100, 40, navigation={})
for bar_width, bar_height in ((80, 30), (157, 40)):
    for bar_unicode in (True, False):
        bar_nav = {"installed": ["codex", "claude"], "spawn_managed": True, "selected": ("chat", "adv-sub"),
                   "preview": {"target": ("chat", "adv-sub"), "report": {}}}
        for bar_draw in (lambda: graph_view.render_graph(starter_board, bar_width, bar_height, unicode=bar_unicode, navigation=bar_nav),
                         lambda: render(dict(starter_board, collection={"ok": True}), "claude", bar_width, bar_height,
                                        unicode=bar_unicode, view="tree", navigation=bar_nav)):
            bar_text = bar_draw()
            bar_rows = bar_text.splitlines()
            spots = {h["action"][1]: h for h in bar_nav["hits"] if h["action"][0] == "spawn"}
            # The start bar sits right above the pinned key hints, and its hits are on its own row.
            bar_row = next(i for i, row in enumerate(bar_rows) if "[+ Codex main]" in row)
            assert "Keys" in bar_rows[-1] and bar_row < len(bar_rows) - 1, bar_rows[-3:]
            assert set(spots) == {"codex", "claude"} and all(h["y"] == bar_row + 1 for h in spots.values()), bar_nav["hits"]
            assert bar_rows[bar_row][spots["codex"]["x1"] - 1:spots["codex"]["x2"]] == "[+ Codex main]", bar_rows[bar_row]
            assert bar_rows[bar_row][spots["claude"]["x1"] - 1:spots["claude"]["x2"]] == "[+ Claude main]", bar_rows[bar_row]
disabled_nav = {"installed": ["codex"], "spawn_managed": False}
assert "needs the tmux panel" in graph_view.render_graph(starter_board, 100, 40, navigation=disabled_nav).splitlines()[-2]
assert not [h for h in disabled_nav["hits"] if h["action"][0] == "spawn"]
armed_nav = {"installed": ["codex"], "spawn_managed": True, "spawn_busy": True}
assert graph_view.render_graph(starter_board, 100, 40, navigation=armed_nav).splitlines()[-2].startswith("Starting a main")
# --spawn-main needs no terminal, refuses workers, an absent panel and a full room, and never attaches.
for spawn_env, expected in (({"OMS_HARNESS_CHILD": "1"}, "worker cannot open owner sessions"),
                            ({"OMS_HARNESS_DELEGATE_DEPTH": "1"}, "worker cannot open owner sessions"),
                            # Without tmux or the CLI on the child's PATH (CI hosts) the tool check refuses first.
                            ({}, "no OMS panel is open for this checkout" if shutil.which("tmux", path=environment.get("PATH"))
                             and shutil.which("claude", path=environment.get("PATH")) else "starting a main needs tmux")):
    refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude", "--json"],
                             cwd=str(project), env=dict(environment, OMS_PANEL_SESSION="", **spawn_env),
                             capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    assert refused.returncode != 0 and expected in refused.stderr, (spawn_env, refused.stdout, refused.stderr)
# The cap is checked after the tool check, so hosts without tmux or a CLI (CI) still reach it.
with patch.object(panel, "session_owner", return_value=str(project)), patch.object(panel, "panel_room", return_value="r1"), \
        patch.object(panel.shutil, "which", return_value="/usr/bin/true"), \
        patch.object(panel, "live_mains", return_value=panel.MAX_LIVE_MAINS), \
        patch.object(panel, "events", return_value="[]"), patch.object(panel, "add_main_window") as opened_window:
    try:
        panel.spawn_main(project, "claude")
        raise AssertionError("a second live main was started")
    except ValueError as error:
        assert str(error).startswith("this panel already has a live main")
        assert "capacity screen k" in str(error) and "unverifiable" in str(error)
        assert "--release-main" not in str(error)
    assert not opened_window.called
    with patch.object(panel, "tmux_command", return_value=["true"]), patch.object(panel, "bind_main_keys"):
        try:
            panel.split_panel(project, launch="claude")
            raise AssertionError("--launch added a second live main")
        except ValueError as error:
            assert str(error).startswith("this panel already has a live main")
            assert "capacity screen k" in str(error) and "unverifiable" in str(error)
            assert "--release-main" not in str(error)
    assert not opened_window.called
    with patch.object(panel, "recorded_room", return_value="r1"):
        try:
            panel.check_legacy_main(project)
            raise AssertionError("an unverified main window allowed replacement")
        except ValueError as error:
            assert str(error).startswith("this panel already has a live main")
            assert "capacity screen k" in str(error) and "unverifiable" in str(error)
            assert "--release-main" not in str(error)
    cross_room_main = {"tool": "panel-main", "terminal": False,
                       "refs": {"panel_room_id": "room-a"}}
    with patch.object(panel, "events", return_value=json.dumps([cross_room_main])), \
            patch.dict(os.environ, {"OMS_ROOM_ID": "room-b"}):
        try:
            panel.check_legacy_main(project)
            raise AssertionError("a main in another room allowed replacement")
        except ValueError as error:
            assert str(error).startswith("this panel already has a live main"), error
# The cap counts the whole panel, not the selected room; a close needs exactly one proven window and records the leave only after the kill.
def fake_status(repo, ident):
    return {"participants": [{"participant": "%s-m%s" % (ident, n), "joined": True, "role": "main"} for n in range(4)]}
def fake_history(repo, ident, member=None):
    return [{"attempt_id": "%s-m%s" % (ident, n), "tool": "panel-main", "state": "working",
             "refs": {"panel_room_participant": "%s-m%s" % (ident, n)}} for n in range(4)]
two_rooms = "\n".join("claude\t%s\t%s-m%s\tclaude\t@%s" % (ident, ident, n, n) for ident in ("ra", "rb") for n in range(3)) + "\nclaude\trb\t\tclaude\t@9"
with patch.object(panel, "tmux_command", return_value=["true"]), patch.object(panel.room, "status", fake_status), \
        patch.object(panel, "main_history", fake_history), \
        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, two_rooms, "")):
    assert panel.live_mains(project, "s", "ra") == 9 and panel.live_mains(project, "s", "rb") == 9, "cap counted one room"
with patch.object(panel.room, "status", return_value={"participants": []}), \
        patch.object(panel, "main_history", return_value=[]), patch.object(panel.subprocess, "run") as window_query:
    window_query.return_value = subprocess.CompletedProcess([], 0, "codex\tr1\t\tcontrol\t@1\ncodex\tr1\t\tpi\t@2\n", "")
    assert panel.live_mains(project, "s", "r1") == 1, "control counted as a main or pending Pi was lost"
    window_query.return_value = subprocess.CompletedProcess([], 0, "codex\tr1\tmissing\tpi\t@2\n", "")
    assert panel.live_mains(project, "s", "r1") == 1, "missing evidence allowed replacement"
    window_query.return_value = subprocess.CompletedProcess([], 1, "", "")
    try:
        panel.live_mains(project, "s", "r1")
        raise AssertionError("failed window lookup allowed replacement")
    except ValueError as error:
        assert "status unavailable" in str(error)
# Legacy inline owners have no new lease or terminal-host binding; unknown lifecycle still blocks replacement.
with patch.object(panel, "events", return_value=json.dumps([{"tool": "panel-main", "state": "working", "terminal": False}])), \
        patch.object(panel.shutil, "which", return_value=None), patch.object(panel, "run_single_native") as inline_start:
    try:
        panel.run_native("claude", project)
        raise AssertionError("a legacy inline main was replaced")
    except ValueError as error:
        assert "already has a live main" in str(error)
    assert not inline_start.called
# Explicit recovery requires recorded host ownership and positive native termination evidence.
import panel_release
import panel_native
import panel_health
import panel_host
import panel_terminals
import codex_harness
import io
release_repo = temporary / "release-main"
release_repo.mkdir()
subprocess.run(["git", "init", "-q", str(release_repo)], check=True)
release_bin = temporary / "release-bin"
release_bin.mkdir()
release_socket = str(temporary / "release.sock")
release_server = panel_release.process_refs(os.getpid(), "tmux_server")
release_server["panel_socket_digest"] = hashlib.sha256(os.path.realpath(release_socket).encode()).hexdigest()
(release_bin / "tmux").write_text('#!/bin/sh\nif [ "$OMS_RELEASE_ERROR" = 1 ]; then echo "server error" >&2; exit 1; fi\n'
    'if [ "$OMS_RELEASE_NO_SERVER" = 1 ]; then echo "no server running" >&2; exit 1; fi\n'
    'if [ "$1" = display-message ]; then printf "%s\\t%s\\r\\n" "$OMS_RELEASE_SERVER" "$OMS_RELEASE_SOCKET"; '
    'elif [ "$1" = list-windows ]; then printf "%s\\r\\n" "$OMS_RELEASE_WINDOW"; '
    'else printf "%s\\r\\n" "$OMS_RELEASE_PANE"; fi\n')
(release_bin / "tmux").chmod(0o700)
release_now = time.time() + 600
release_env = dict(environment, PATH=str(release_bin) + os.pathsep + environment["PATH"],
                   OMS_RELEASE_ERROR="0", OMS_RELEASE_NO_SERVER="0", OMS_RELEASE_WINDOW="", OMS_RELEASE_PANE="",
                   OMS_RELEASE_SERVER=str(os.getpid()), OMS_RELEASE_SOCKET=release_socket, OMS_RELEASE_NOW=str(release_now))
release_cli = [sys.executable, "-c", "import os,sys; sys.path.insert(0,sys.argv[1]); import terminal_panel as p; "
               "import panel_release as r; r.time.time=lambda:float(os.environ['OMS_RELEASE_NOW']); "
               "sys.exit(p.main(sys.argv[2:]))", str(root / "scripts/lib"), "--repo", str(release_repo), "--release-main"]
live_release_process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(90)"])
dead_release_process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(90)"])
try:
    live_refs = panel_release.process_refs(live_release_process.pid, "native")
    dead_refs = panel_release.process_refs(dead_release_process.pid, "native")
    assert live_refs and dead_refs
    dead_wrapper = {key.replace("native", "wrapper"): value for key, value in dead_refs.items()}
    live_wrapper = {key.replace("native", "wrapper"): value for key, value in live_refs.items()}
    dead_release_process.terminate()
    dead_release_process.wait(timeout=10)
    def release_attempt(state="working", tool="panel-main", role="main", host="tmux", native=None, wrapper=None, reason="", server=None, harness="codex"):
        refs = dict(panel_role=role, panel_room_id="release-room", panel_label="Recovered main",
                    panel_heartbeat_interval="60", panel_heartbeat_ttl="300", panel_harness=harness)
        if host:
            refs["panel_host"] = host
        if host == "tmux":
            refs.update(release_server if server is None else server, panel_pane_id="%12")
        native_refs = dead_refs if native is None else native
        refs.update(dead_wrapper if wrapper is None else wrapper)
        args = [arg for key, value in refs.items() for arg in ("--ref", key + "=" + value)]
        ident = panel.events(release_repo, "start", "--provider", "codex", "--tool", tool, *args,
                             "--then", "starting", *([] if state == "starting" else ["--then", "working"]), output=True)
        if native_refs:
            panel.events(release_repo, "usage", "--attempt", ident, "--actor", "panel-main",
                         *[arg for key, value in native_refs.items() for arg in ("--ref", key + "=" + value)])
        if state not in {"starting", "working"}:
            panel.events(release_repo, "transition", "--attempt", ident, "--state", state, "--reason-code", reason)
        return ident
    working_release = release_attempt()
    blocked_release = release_attempt("blocked", host="inline")
    legacy_release = release_attempt(host=None, native={}, wrapper={})
    empty_refs_release = panel.events(release_repo, "start", "--provider", "codex", "--tool", "panel-main",
                                      "--then", "starting", "--then", "working", output=True)
    inline_alive = release_attempt(host="inline", native=live_refs, wrapper=live_wrapper)
    tmux_alive = release_attempt(native=live_refs)
    inline_orphan = release_attempt(host="inline", native=live_refs)
    inline_unknown = release_attempt(host="inline", wrapper={})
    unconfirmed_release = release_attempt("blocked", host="inline", native={}, reason="native_shutdown_unconfirmed")
    unconfirmed_gone = release_attempt("blocked", host="inline", reason="native_shutdown_unconfirmed")
    starting_release = release_attempt("starting")
    worker_release = release_attempt(tool="peer-delegate", role="worker")
    wrong_role = release_attempt(role="worker")
    herdr_release = release_attempt(host="herdr")
    waiting_release = release_attempt("waiting_input")
    finished_release = release_attempt("cancelled")
    confirmed_exit = release_attempt(host="inline", native={"panel_native_exit": "confirmed", "panel_native_exit_code": "0"})
    pi_launcher = release_attempt(host="inline", harness="pi", native={key.replace("native", "launcher"): value for key, value in dead_refs.items()})
    gone_server = dict(release_server, **{key.replace("native", "tmux_server"): value for key, value in dead_refs.items()})
    no_server_release = release_attempt(server=gone_server)
    restarted_release = release_attempt(server=gone_server)
    pi_release = release_attempt(host="inline", native={}, harness="pi")
    pi_env = dict(OMS_PANEL_MAIN_ATTEMPT=pi_release, OMS_ROOM_REPO=str(release_repo), OMS_PANEL_HARNESS="pi")
    launcher_refs = panel_release.process_refs(os.getpid(), "launcher")
    assert launcher_refs
    with patch.object(codex_harness.os, "getpid", return_value=live_release_process.pid):
        codex_harness.record_panel_child(pi_env, live_release_process.pid)
    early_pi_row = json.loads(panel_release.events(release_repo, "show", "--attempt", pi_release, "--json"))
    assert "panel_native_pid" not in early_pi_row["refs"], "early nested Pi claimed an unregistered launcher"
    panel_release.events(release_repo, "usage", "--attempt", pi_release, "--actor", "panel-main",
                         *[arg for key, value in launcher_refs.items() for arg in ("--ref", key + "=" + value)])
    with patch.object(panel_release, "process_refs", side_effect=lambda pid, kind: launcher_refs if kind == "launcher" else dead_refs):
        codex_harness.record_panel_child(pi_env, dead_release_process.pid)
    pi_row = json.loads(panel_release.events(release_repo, "show", "--attempt", pi_release, "--json"))
    assert all(pi_row["refs"][key] == value for key, value in dead_refs.items())
    pi_alive = release_attempt(host="inline", native={}, harness="pi")
    pi_live_env = dict(pi_env, OMS_PANEL_MAIN_ATTEMPT=pi_alive)
    empty_pi_refs = {key: value for key, value in pi_row["refs"].items() if not key.startswith(("panel_native_", "panel_launcher_"))}
    for protected_refs in (
            {},
            dict(launcher_refs, panel_native_pid=""), dict(launcher_refs, panel_native_identity=""),
            dict(launcher_refs, panel_launcher_pid=str(dead_release_process.pid)),
            dict(launcher_refs, panel_launcher_identity="0" * 64),
            {"panel_launcher_pid": launcher_refs["panel_launcher_pid"]},
            {"panel_launcher_identity": launcher_refs["panel_launcher_identity"]}):
        protected_row = dict(pi_row, refs=dict(empty_pi_refs, **protected_refs))
        with patch.object(panel_release, "events", return_value=json.dumps(protected_row)) as child_activity:
            codex_harness.record_panel_child(pi_live_env, live_release_process.pid)
        assert child_activity.call_count == 1, "protected Pi ownership was overwritten"
    panel_release.events(release_repo, "usage", "--attempt", pi_alive, "--actor", "panel-main",
                         *[arg for key, value in launcher_refs.items() for arg in ("--ref", key + "=" + value)])
    import threading
    read_started, allow_read, second_read, contender_started = (threading.Event() for _ in range(4))
    registration_errors = []
    registration_events = panel_release.events
    def concurrent_events(repo, *args):
        if args[0] == "show" and read_started.is_set():
            second_read.set()
        result = registration_events(repo, *args)
        if args[0] == "show" and not read_started.is_set():
            read_started.set()
            assert allow_read.wait(5), "registration reader stalled"
        return result
    def register_child(pid, contender=False):
        if contender:
            contender_started.set()
        try:
            codex_harness.record_panel_child(pi_live_env, pid)
        except Exception as error:
            registration_errors.append(error)
    first_registration = threading.Thread(target=register_child, args=(live_release_process.pid,))
    competing_registration = threading.Thread(target=register_child, args=(os.getpid(), True))
    with patch.object(panel_release, "events", side_effect=concurrent_events):
        first_registration.start()
        try:
            assert read_started.wait(5), "first registration never read ownership"
            competing_registration.start()
            assert contender_started.wait(5)
            assert not second_read.wait(0.2), "concurrent registrations read the same empty native refs"
        finally:
            allow_read.set()
            first_registration.join(timeout=10)
            if competing_registration.ident is not None:
                competing_registration.join(timeout=10)
    assert not first_registration.is_alive() and not competing_registration.is_alive()
    assert not registration_errors, registration_errors
    with patch.object(panel_release, "process_refs", side_effect=lambda pid, kind: launcher_refs if kind == "launcher" else dead_refs):
        codex_harness.record_panel_child(pi_live_env, dead_release_process.pid)
    pi_live_row = json.loads(panel_release.events(release_repo, "show", "--attempt", pi_alive, "--json"))
    assert all(pi_live_row["refs"][key] == value for key, value in live_refs.items()), "nested Pi replaced the original native identity"
    ledger = release_repo / ".oms/lifecycle/events.jsonl"
    for release_id, extra_env, extra_args, expected in (
            (working_release, {"OMS_RELEASE_PANE": working_release}, [], "live tagged tmux"),
            (working_release, {"OMS_RELEASE_WINDOW": working_release}, [], "live tagged tmux"),
            (working_release, {"OMS_RELEASE_ERROR": "1"}, [], "liveness unproven"),
            (working_release, {"OMS_RELEASE_NO_SERVER": "1"}, [], "liveness unproven"),
            (legacy_release, {}, [], "liveness unproven"),
            (empty_refs_release, {}, [], "liveness unproven"),
            (inline_alive, {}, [], "liveness unproven"),
            (tmux_alive, {}, [], "liveness unproven"),
            (inline_orphan, {}, [], "native termination is unconfirmed"),
            (inline_unknown, {}, [], "liveness unproven"),
            (unconfirmed_release, {}, [], "liveness unproven"),
            (pi_launcher, {}, [], "Pi child termination unconfirmed; separate recovery required"),
            (pi_alive, {}, [], "Pi child termination unconfirmed; separate recovery required"),
            (working_release, {"OMS_RELEASE_SOCKET": release_socket + ".other"}, [], "tmux server ownership changed"),
            (working_release, {"OMS_RELEASE_NOW": str(time.time())}, [], "liveness unproven"),
            (worker_release, {}, [], "requires a panel-main"),
            (wrong_role, {}, [], "requires a panel-main"),
            (starting_release, {}, [], "requires a panel-main"),
            (waiting_release, {}, [], "requires a panel-main"),
            (finished_release, {}, [], "requires a panel-main"),
            (herdr_release, {}, [], "liveness unproven"),
            ("../invalid", {}, [], "invalid panel main attempt"),
            (working_release, {}, ["--launch", "claude"], "unrecognized arguments"),
            (working_release, {"OMS_HARNESS_CHILD": "1"}, [], "worker cannot release")):
        before_release = ledger.read_bytes()
        refused = subprocess.run(release_cli + [release_id, "--json"] + extra_args,
                                 env=dict(release_env, **extra_env), capture_output=True, text=True, timeout=20)
        assert refused.returncode and expected in refused.stderr, (refused.stdout, refused.stderr)
        assert ledger.read_bytes() == before_release, "refused release changed lifecycle"
    for held_lock in ("panel-main-start", "panel-main-session"):
        before_release = ledger.read_bytes()
        with panel_results._lock(release_repo, held_lock):
            refused = subprocess.run(release_cli + [working_release], env=release_env,
                                     capture_output=True, text=True, timeout=20)
            assert refused.returncode and "panel operation is busy" in refused.stderr, refused.stderr
        assert ledger.read_bytes() == before_release, "release bypassed a live main lease"
    with patch.dict(os.environ, release_env, clear=True), patch.object(panel_release.time, "time", return_value=release_now):
        with patch.object(panel_release.subprocess, "run", wraps=panel_release.subprocess.run) as scans:
            dead = panel_release.dead_mains(release_repo)
            assert sum(c.args[0][1] == "list-panes" for c in scans.call_args_list) == 1
            assert sum(c.args[0][1] == "list-windows" for c in scans.call_args_list) == 1
        assert {row["attempt"] for row in dead["releasable"]} == {working_release, blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}, dead
        assert {row["attempt"] for row in dead["unverifiable"]} == {
            legacy_release, inline_alive, tmux_alive, inline_orphan, inline_unknown, unconfirmed_release, herdr_release, pi_launcher, pi_alive, empty_refs_release}, dead
        assert all(row["label"] == ("" if row["attempt"] == empty_refs_release else "Recovered main") and row["age"]
                   for group in dead.values() for row in group)
        capacity = panel_health.render({"dead_mains": dead}, 220, 100)
        assert "Dead mains" in capacity and "Unverifiable mains" in capacity, capacity
        assert "oms panel --release-main " + working_release in capacity
        assert "oms panel --release-main " + legacy_release not in capacity
        assert "Pi child termination unconfirmed; separate recovery required" in capacity
        with patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel, "read_choice", side_effect=["k", "q"]), \
                patch.object(panel, "read_field", return_value=""), \
                patch.object(panel, "private_capacity", return_value={}), \
                patch("panel_metrics.collect", return_value={}), \
                patch("panel_cache.TickReads.attempts", return_value=[]), \
                patch.object(panel_release, "dead_mains", return_value=dead), \
                patch.object(panel.sys, "stdout", io.StringIO()) as control_capacity, \
                patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((180, 80))):
            assert panel.interactive(release_repo) == 0
        assert "Dead mains" in control_capacity.getvalue() and working_release in control_capacity.getvalue()
        assert len(panel.live_main_refusal(release_repo, [{"attempt_id": working_release, "tool": "panel-main", "state": "waiting_approval"}])) <= 140
        assert "--release-main " + working_release in panel.live_main_refusal(release_repo, [{
            "attempt_id": working_release, "tool": "panel-main", "state": "waiting_input"}])
        for attempt_length in range(1, 161):
            attempt = "a" * attempt_length
            assert panel_release.ID.fullmatch(attempt)
            for state in ("working", "waiting_approval", "waiting_input"):
                refusal = panel.live_main_refusal(release_repo, [{
                    "attempt_id": attempt, "tool": "panel-main", "state": state}])
                command = "oms panel --release-main " + attempt
                assert len(refusal.splitlines()[0]) <= 140, refusal
                assert refusal.endswith(command), refusal
                if len(refusal) > 140:
                    assert refusal.splitlines()[1:] == [command], refusal
        maximum_attempt = "a" + "._:-09AZaz" * 15 + "._:-09AZa"
        assert len(maximum_attempt) == 160 and panel_release.ID.fullmatch(maximum_attempt)
        maximum_refusal = panel.live_main_refusal(release_repo, [{
            "attempt_id": maximum_attempt, "tool": "panel-main", "state": "working"}])
        assert len(maximum_refusal.splitlines()[0]) <= 140, maximum_refusal
        assert maximum_refusal.splitlines()[1:] == ["oms panel --release-main " + maximum_attempt]

        with patch.object(panel_release.subprocess, "run", wraps=panel_release.subprocess.run) as scans:
            reused = panel_release.dead_mains(release_repo, windows={working_release})
            assert {row["attempt"] for row in reused["releasable"]} == {blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}
            assert sum(c.args[0][1] == "list-panes" for c in scans.call_args_list) == 1
            assert not any(c.args[0][1] == "list-windows" for c in scans.call_args_list)
        with patch.object(panel_release.shutil, "which", return_value=None):
            try:
                panel_release.tagged_attempts()
                raise AssertionError("missing tmux proved host absence")
            except ValueError as error:
                assert "liveness unproven" in str(error)
        with patch.dict(os.environ, OMS_RELEASE_ERROR="1"):
            failed_scan = panel_release.dead_mains(release_repo)
            assert {row["attempt"] for row in failed_scan["releasable"]} == {blocked_release, confirmed_exit, unconfirmed_gone, no_server_release, restarted_release, pi_release}
            assert working_release in {row["attempt"] for row in failed_scan["unverifiable"]}
        with patch.dict(os.environ, OMS_RELEASE_NO_SERVER="1"):
            absent = panel_release.dead_mains(release_repo)
            assert {no_server_release, restarted_release} <= {row["attempt"] for row in absent["releasable"]}
            assert working_release in {row["attempt"] for row in absent["unverifiable"]}
        with patch.object(panel_release, "tmux_identity", side_effect=ValueError("unavailable")), \
                patch.object(panel_host, "herdr_active", return_value=False), patch.object(sys, "stderr", new_callable=io.StringIO) as warning:
            assert panel_release.start_refs(release_repo, True)["panel_host"] == "tmux"
            assert warning.getvalue().count("tmux ownership binding unavailable") == 1
        rows = json.loads(panel_release.events(release_repo, "list", "--json"))
        stale_inline = next(row for row in rows if row["attempt_id"] == blocked_release)
        with patch.object(panel_release, "identity_gone", return_value=True) as probes:
            assert panel_release.classify([]) == {"releasable": [], "unverifiable": []}
            assert not probes.called
            capped_tick = Mock()
            capped_tick.attempts.return_value = [dict(stale_inline, attempt_id="cap-%s" % i) for i in range(4)]
            with patch.object(panel_release, "MAX_TICK_PROBES", 2):
                limited = panel_release.dead_mains(release_repo, _tick=capped_tick)
            assert len(limited["releasable"]) == len(limited["unverifiable"]) == 2
            assert probes.call_count == 6, probes.call_count
            assert all("verification pending" in row["reason"] for row in limited["unverifiable"])
            pending_capacity = panel_health.render({"dead_mains": limited}, 220, 40)
            assert "verification pending" in pending_capacity and "oms panel --release-main cap-2" not in pending_capacity
        with patch.object(panel_release.time, "time", return_value=release_now - 600):
            assert panel_release.classify(rows) == {"releasable": [], "unverifiable": []}, "fresh attempt appeared as dead"
        young = dict(next(row for row in rows if row["attempt_id"] == working_release),
                     created_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 30)))
        assert panel_release.classify([young]) == {"releasable": [], "unverifiable": []}
        fresh = dict(young, created_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 600)),
                     last_heartbeat_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(release_now - 1)))
        assert panel_release.classify([fresh]) == {"releasable": [], "unverifiable": []}
        with patch.dict(os.environ, OMS_RELEASE_PANE=working_release):
            assert working_release not in {row["attempt"] for row in panel_release.dead_mains(release_repo)["releasable"]}
        for row, offered in ((next(row for row in rows if row["attempt_id"] == working_release), True),
                             (next(row for row in rows if row["attempt_id"] == legacy_release), False)):
            with patch.object(panel, "events", return_value=json.dumps([row])), \
                    patch.object(panel, "session_owner", return_value=str(release_repo)), \
                    patch.object(panel, "panel_room", return_value="release-room"), \
                    patch.object(panel, "live_mains", return_value=panel.MAX_LIVE_MAINS), \
                    patch.object(panel, "main_installed", return_value=True), \
                    patch.object(panel_host, "herdr_active", return_value=False), \
                    patch.object(panel_release, "release_held", side_effect=ValueError("re-proof failed")), \
                    patch.dict(os.environ, TMUX="fixture", OMS_PANEL_SESSION="oms-fixture"):
                for refuse in (lambda: panel.check_legacy_main(release_repo),
                               lambda: panel.spawn_main(release_repo, "claude"),
                               lambda: panel.construct_panel(release_repo, "oms-fixture", str(release_repo),
                                                             os.environ.copy(), "auto", False, "claude", None, None, None),
                               lambda: panel.open_native("claude", release_repo)):
                    try:
                        refuse()
                        raise AssertionError("unreleased main allowed replacement")
                    except ValueError as error:
                        assert str(error).startswith("this panel already has a live main"), error
                        assert ("oms panel --release-main " + row["attempt_id"] in str(error)) is offered, error
                        assert "--release-main ATTEMPT" not in str(error), error
                        if not offered:
                            assert "capacity screen k" in str(error) and "unverifiable" in str(error), error
        if os.name != "nt":
            with patch.object(panel_terminals, "process", return_value={"identity": "f" * 64, "state": "S"}):
                assert not panel_terminals.matches(live_release_process.pid, live_refs["panel_native_identity"])
                assert panel_release.identity_gone(live_refs, "native"), "PID reuse was treated as the recorded live process"
            with patch.object(panel_terminals, "process", return_value=None), patch.object(panel_terminals.os, "kill", side_effect=PermissionError):
                assert not panel_release.identity_gone(live_refs, "native"), "unknown process probe proved death"
        with patch.object(panel_host, "herdr_active", return_value=True), \
                patch.object(panel_host, "socket_digest", return_value="host-digest"), \
                patch.object(panel_host, "pane", return_value={}) as host_lookup:
            for host_error in (None, ValueError("missing pane")):
                host_lookup.side_effect = host_error
                try:
                    panel_release.prove_host_absent({"panel_host": "herdr", "panel_socket_digest": "host-digest",
                                                    "panel_pane_id": "w1:p1"}, herdr_release)
                    raise AssertionError("Herdr evidence allowed unsafe release")
                except ValueError as error:
                    assert "liveness unproven" in str(error)
        mock_native = Mock(pid=live_release_process.pid)
        mock_native.wait.return_value = mock_native.poll.return_value = 0
        with patch.object(panel, "events", return_value="att_recording") as recorded, \
                patch.object(panel, "main_installed", return_value=True), patch.object(panel, "ensure_room", return_value="release-room"), \
                patch.object(panel.room, "join"), patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel_host, "herdr_active", return_value=False), patch.object(panel, "bootstrap", return_value="fixture"), \
                patch.object(panel, "native_command", return_value=["fixture-native"]), \
                patch.object(panel.subprocess, "Popen", return_value=mock_native) as native_launch:
            assert panel._run_native("codex", release_repo) == 0
            start_args = recorded.call_args_list[0].args
            assert "panel_host=inline" in start_args and "panel_wrapper_pid=" + str(os.getpid()) in start_args
            assert any(isinstance(arg, str) and arg.startswith("panel_wrapper_identity=") for arg in start_args)
            assert any("panel_native_pid=" + str(live_release_process.pid) in call.args for call in recorded.call_args_list)
            assert any("panel_native_exit=confirmed" in call.args for call in recorded.call_args_list)
            recorded.reset_mock()
            def locked_pi_launch(*args, **kwargs):
                try:
                    with panel_results._lock(release_repo, "panel-pi-registration", timeout=0):
                        raise AssertionError("Pi launcher started before registration lock")
                except ValueError as error:
                    assert "panel operation is busy" in str(error)
                return mock_native
            native_launch.side_effect = locked_pi_launch
            assert panel._run_native("pi", release_repo) == 0
            assert any("panel_launcher_pid=" + str(live_release_process.pid) in call.args for call in recorded.call_args_list)
            assert any("panel_launcher_exit=confirmed" in call.args for call in recorded.call_args_list)
            assert not any("panel_native_exit=confirmed" in call.args for call in recorded.call_args_list)
        broker = Mock()
        broker.model.return_value = {}
        with patch.object(codex_harness, "AuthBroker", return_value=broker), \
                patch.object(codex_harness, "Gateway"), patch.object(codex_harness.threading, "Thread"), \
                patch.object(codex_harness.shutil, "which", return_value="fixture"), \
                patch.object(codex_harness, "harness_env", return_value=pi_live_env), \
                patch.object(codex_harness.subprocess, "Popen", return_value=mock_native), \
                patch.object(panel_release, "events", side_effect=[json.dumps(dict(pi_row, refs=dict(empty_pi_refs, **launcher_refs))), ""]) as child_activity, \
                patch.object(codex_harness, "record_panel_child", wraps=codex_harness.record_panel_child) as record_child, \
                patch.object(sys, "stderr", new_callable=io.StringIO) as child_notice:
            assert codex_harness.main(["pi", "--model", "fixture-model"]) == 0
            record_child.assert_called_once_with(pi_live_env, live_release_process.pid)
            assert "panel_native_pid=" + str(live_release_process.pid) in child_activity.call_args.args
            assert "Pi child identity unavailable" not in child_notice.getvalue()
        for release_id, previous, extra_env in ((working_release, "working", {}), (blocked_release, "blocked", {}),
                (confirmed_exit, "working", {}), (unconfirmed_gone, "blocked", {}),
                (no_server_release, "working", {"OMS_RELEASE_NO_SERVER": "1"}),
                (restarted_release, "working", {"OMS_RELEASE_SERVER": str(live_release_process.pid)}),
                (pi_release, "working", {})):
            released = json.loads(call(release_cli + [release_id, "--json"], env=dict(release_env, **extra_env)))
            assert released == {"attempt": release_id, "previous_state": previous, "state": "abandoned", "reason": "panel_main_released"}
            before_release = ledger.read_bytes()
            repeated = subprocess.run(release_cli + [release_id], env=release_env, capture_output=True, text=True, timeout=20)
            assert repeated.returncode and ledger.read_bytes() == before_release
    front_release = release_attempt(host="inline")
    panel.events(release_repo, "usage", "--attempt", front_release, "--actor", "panel-main",
                 "--ref", "panel_heartbeat_interval=1", "--ref", "panel_heartbeat_ttl=1")
    time.sleep(2)
    front = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(release_repo),
                             "--release-main", front_release, "--json"], env=release_env))
    assert front == {"attempt": front_release, "previous_state": "working", "state": "abandoned", "reason": "panel_main_released"}
    release_rows = [json.loads(line) for line in ledger.read_text().splitlines()]
    assert [(r["from_state"], r["to_state"]) for r in release_rows if r.get("reason_code") == "panel_main_released"] == [
        ("working", "blocked"), ("blocked", "abandoned"), ("blocked", "abandoned"), ("working", "blocked"), ("blocked", "abandoned"), ("blocked", "abandoned")
    ] + [("working", "blocked"), ("blocked", "abandoned")] * 4
    assert all(r["actor"]["name"] == "panel-main-release" for r in release_rows if r.get("reason_code") == "panel_main_released")
    terminal_release = json.loads(panel_release.events(release_repo, "show", "--attempt", working_release, "--json"))
    with patch.object(panel, "events", return_value=json.dumps([terminal_release])), patch.object(panel.shutil, "which", return_value=None):
        panel.check_legacy_main(release_repo)
    # A start after a crash or reboot abandons the proven-dead main itself instead of refusing.
    auto_release = release_attempt("blocked", host="inline")
    listed = panel.events
    def only_auto(repo, *args, **kwargs):
        rows = listed(repo, *args, **kwargs)
        return json.dumps([r for r in json.loads(rows) if r["attempt_id"] == auto_release]) if args[0] == "list" else rows
    def counted(*args):
        row = json.loads(panel_release.events(release_repo, "show", "--attempt", auto_release, "--json"))
        return 0 if row["terminal"] else panel.MAX_LIVE_MAINS
    with patch.dict(os.environ, dict(release_env, TMUX="fixture", OMS_PANEL_SESSION="oms-fixture"), clear=True), \
            patch.object(panel_release.time, "time", return_value=release_now), \
            patch.object(panel, "events", side_effect=only_auto), patch.object(panel.shutil, "which", return_value=None), \
            patch.object(panel, "main_installed", return_value=True), patch.object(panel_host, "herdr_active", return_value=False), \
            patch.object(panel, "panel_room", return_value="release-room"), patch.object(panel, "live_mains", side_effect=counted), \
            patch.object(panel, "add_main_window", return_value="%9") as opened_main, \
            patch.object(panel, "tmux_command", return_value=["true"]), \
            patch.object(sys, "stderr", new_callable=io.StringIO) as release_notice:
        assert panel.open_native("claude", release_repo) == 0
    assert opened_main.called and "released crashed main " + auto_release in release_notice.getvalue()
    assert json.loads(panel_release.events(release_repo, "show", "--attempt", auto_release, "--json"))["state"] == "abandoned"
    with patch.object(panel.room, "status", return_value={"participants": [{"participant": working_release, "joined": True, "role": "main"}]}), \
            patch.object(panel, "main_history", return_value=[terminal_release]), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")):
        assert panel.live_mains(release_repo, "s", "release-room") == 0
    assert "Dead mains" not in panel_health.render({}, 180, 80)
finally:
    for child in (live_release_process, dead_release_process):
        if child.poll() is None:
            child.terminate()
        child.wait(timeout=10)
occupied = {"installed": ["claude", "pi", "codex"], "spawn_managed": True, "main_occupied": True}
assert graph_view.spawn_bar(100, occupied) is None
assert graph_view.spawn_bar(100, dict(occupied, spawn_managed=False)) is None
armed_bar = graph_view.spawn_bar(100, {"installed": ["claude"], "spawn_managed": True, "spawn_armed": True})
assert "1 Claude" in armed_bar[0] and "2 Pi" not in armed_bar[0] and "0 Codex" not in armed_bar[0]
# The exit prompt must discard native CLI typeahead before waiting for fresh keys.
from unittest.mock import Mock
for flush_error in (None, OSError("unsupported terminal"), ImportError("no termios")):
    order = []
    def flush_exit_input(fd, queue):
        assert (fd, queue) == (42, 0)
        order.append("flush")
        if flush_error is not None:
            raise flush_error
    def wait_exit_input(readers, writers, errors):
        assert (readers, writers, errors) == ([42], [], [])
        assert order == ["flush"], order
        order.append("select")
    def read_exit_input(fd, size):
        assert (fd, size) == (42, 1)
        assert order == ["flush", "select"], order
        return b"r"
    with patch("panel_input.TerminalInput") as exit_input, \
            patch.dict(sys.modules, termios=Mock(TCIFLUSH=0, tcflush=flush_exit_input)), \
            patch("select.select", side_effect=wait_exit_input), \
            patch.object(panel.os, "read", side_effect=read_exit_input):
        exit_input.return_value.__enter__.return_value.fd = 42
        assert panel._main_exit_key() == "r"
        exit_input.return_value.__exit__.assert_called_once_with()
# Closing an exit prompt requires all three window fields and a successful lookup.
_operator_block("native-lifecycle")
exit_options = {"@oms_panel_main_attempt": "exit-attempt", "@oms_panel_room": shared,
                "@oms_panel_room_participant": "exit-attempt",
                "@oms_panel_repo": str(project.resolve())}
def exited_native(*args, **kwargs):
    kwargs["_exit_info"].update(attempt="exit-attempt", room=shared)
    return 0
for fields, query_status, closes in ((["exit-attempt", "", str(project.resolve())], 0, True),
                                    (["successor", "", str(project.resolve())], 0, False),
                                    (["exit-attempt", "%42", str(project.resolve())], 0, False),
                                    (["exit-attempt", "", "/foreign/repository"], 0, False),
                                    (["exit-attempt", "", str(project.resolve())], 1, False)):
    window_options = dict(exit_options, **{"@oms_panel_main_attempt": fields[0]})
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "pane_option", side_effect=window_options.get), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), \
            patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], query_status, "\t".join(["@42", *fields, "exit-attempt"]) + "\r\n", "")) as close_query:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0
        kills = [call for call in close_query.call_args_list if "if-shell" in call.args[0]]
        assert bool(kills) == closes, (fields, query_status, kills)
        if kills:
            args = kills[0].args[0]
            assert args[-1] == "kill-window -t @42" and args[args.index("-t") + 1] == "@42"
            assert "@oms_panel_main_attempt},exit-attempt" in args[-2] and "@oms_panel_native_pane},}" in args[-2]
            assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in args[-2]
# Restart refuses an unverified session/window or nonterminal previous attempt;
# a Pi restart deliberately ignores even a recorded session id and starts fresh.
for harness, owner, previous_terminal, succeeds in (("claude", None, True, False),
                                           ("claude", str(project.resolve()), False, False),
                                           ("claude", str(project.resolve()), True, True),
                                           ("codex", str(project.resolve()), True, True),
                                           ("pi", str(project.resolve()), True, True)):
    previous = {"attempt_id": "exit-attempt", "terminal": previous_terminal, "provider": panel.main_owner(harness),
                "tool": "panel-main", "refs": {"panel_role": "main", "panel_room_id": shared,
                                               "native_session": "recorded-session"}}
    restart_order = []
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "session_stored", return_value=True), patch.object(panel, "run_single_native", side_effect=exited_native) as restart_main, \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "_main_window", return_value=["@42", "exit-attempt", "", str(project.resolve()), "exit-attempt"]), \
            patch.object(panel, "session_owner", return_value=owner), \
            patch.object(panel, "live_mains", side_effect=lambda *a: restart_order.append("count") or 0), \
            patch.object(panel_native, "release_dead_mains", side_effect=lambda repo: restart_order.append("release")), \
            patch.object(panel, "main_history", return_value=[previous]), \
            patch.object(panel.room, "status", return_value={"participants": [{"participant": "exit-attempt",
                "joined": True, "role": "main", "provider": panel.main_owner(harness),
                "consumer": hashlib.sha256(b"recorded-session").hexdigest()[:32]}]}), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), \
            patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native(harness, project, task="push the release tag") == 0
        assert restart_main.call_count == (2 if succeeds else 1)
        assert restart_main.call_args_list[0].args[4] == "push the release tag"
        if succeeds:
            # A main that died while this window waited is released before the restart counts mains.
            assert restart_order[:2] == ["release", "count"], restart_order
            assert restart_main.call_args.args[2] == (None if harness == "pi" else "recorded-session")
            assert restart_main.call_args.args[4] is None, "restart resent the original task"
# Context or launch setup fails before a child exists, before or after replacement
# tagging. Retry and guarded close retain the terminal window owner;
# incomplete or changed ownership must never receive it.
from unittest.mock import Mock
for failure_step, recovery in ([("write", key) for key in
        ("retry", "close", "nonterminal", "participant", "room", "repository", "native-pane", "session", "unavailable")]
        + [(step, key) for step in ("directory", "command", "popen") for key in ("retry", "close")]):
    failure_room = room.create(project, "restart-failure-" + failure_step + "-" + recovery, "Failed restart")
    failure_tags, failure_attempts, failure_launches, failure_closes = {}, [], [], []
    failure_created = []
    real_run, real_popen, real_write, real_history = subprocess.run, subprocess.Popen, Path.write_text, panel.main_history
    real_events, real_directory = panel.events, panel.tempfile.TemporaryDirectory
    def failure_events(repo, operation, *args, **kwargs):
        result = real_events(repo, operation, *args, **kwargs)
        if operation == "start":
            failure_created.append(result)
        return result
    def failure_directory(*args, **kwargs):
        if failure_step == "directory" and len(failure_created) == 2:
            assert failure_tags["@oms_panel_main_attempt"] == failure_created[0]
            assert "@oms_panel_native_pane" not in failure_tags
            assert len(failure_launches) == 1, "replacement launched before directory creation"
            raise OSError("injected replacement context directory failure")
        return real_directory(*args, **kwargs)
    def failure_tmux(command, **kwargs):
        if command[0] != "fixture-tmux":
            return real_run(command, **kwargs)
        if command[1] == "set-option":
            if "-u" in command:
                failure_tags.pop(command[-1], None)
            else:
                failure_tags[command[-2]] = command[-1]
                if command[-2] == "@oms_panel_main_attempt":
                    failure_attempts.append(command[-1])
        elif command[1] == "display-message":
            tags = dict(failure_tags)
            if len(failure_attempts) == 2:
                changed = {"participant": ("@oms_panel_room_participant", "foreign-member"),
                           "repository": ("@oms_panel_repo", "/foreign/repository"),
                           "native-pane": ("@oms_panel_native_pane", "%42")}.get(recovery)
                if changed:
                    tags[changed[0]] = changed[1]
            fields = ["@42"] + [tags.get(name, "") for name in ("@oms_panel_main_attempt", "@oms_panel_native_pane",
                      "@oms_panel_repo", "@oms_panel_room_participant")]
            return subprocess.CompletedProcess(command, 0, "\t".join(fields) + "\r\n", "")
        elif command[1] == "if-shell":
            failure_closes.append(command)
        return subprocess.CompletedProcess(command, 0, "", "")
    assert panel.launch_failed_without_child(FileNotFoundError("missing")) and not panel.launch_failed_without_child(OSError(5, "I/O"))
    def failure_process(command, **kwargs):
        if command[0] != "fixture-native":
            return real_popen(command, **kwargs)
        if (failure_step == "popen" and len(failure_created) == 2
                and kwargs["env"]["OMS_PANEL_MAIN_ATTEMPT"] == failure_created[-1]):
            assert len(failure_launches) == 1, "replacement child already exists"
            raise FileNotFoundError("injected replacement executable missing")
        failure_launches.append(kwargs["env"]["OMS_PANEL_MAIN_ATTEMPT"])
        process = Mock()
        process.wait.return_value = process.poll.return_value = 0
        return process
    def failure_command(*args, **kwargs):
        if failure_step == "command" and len(failure_created) == 2:
            assert len(failure_launches) == 1, "replacement launched before argv construction"
            raise ValueError("injected replacement native command failure")
        return ["fixture-native"]
    def failure_write(path, *args, **kwargs):
        if failure_step == "write" and path.name == "context.txt" and len(failure_attempts) == 2:
            assert failure_tags["@oms_panel_native_pane"] == "%42"
            assert failure_tags["@oms_panel_repo"] == str(project.resolve())
            assert len(failure_launches) == 1, "replacement launched before the injected startup failure"
            raise OSError("injected replacement context write failure")
        return real_write(path, *args, **kwargs)
    def failure_history(*args, **kwargs):
        rows = real_history(*args, **kwargs)
        if len(failure_attempts) == 2:
            if recovery == "unavailable":
                raise ValueError("replacement history unavailable")
            rows = deepcopy(rows)
            for row in rows:
                if row["attempt_id"] == failure_attempts[-1]:
                    if recovery == "nonterminal":
                        row["terminal"] = False
                    elif recovery == "room":
                        row["refs"]["panel_room_id"] = "foreign-room"
        return rows
    failure_keys = iter(["r", "r", "\n"] if recovery == "retry" else ["r", "\n"])
    def failure_key():
        key = next(failure_keys)
        if len(failure_created) == 2:
            history = real_history(project, failure_room)
            replacement = next(row for row in history if row["attempt_id"] == failure_created[-1])
            assert replacement["terminal"] is True and replacement["state"] == "failed"
            if failure_step in ("command", "popen"):
                assert replacement["reason_code"] == ("native_command_failed" if failure_step == "command" else "native_launch_failed")
                assert "@oms_panel_native_pane" not in failure_tags
            assert all(row["terminal"] is True for row in history if row["tool"] == "panel-main")
        return key
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-failure",
                                    OMS_ROOM_ID=failure_room), clear=True), \
            patch.object(panel, "resolve_session", return_value=("", "")), \
            patch.object(panel, "main_installed", return_value=True), patch.object(panel, "events", side_effect=failure_events), \
            patch.object(panel.tempfile, "TemporaryDirectory", side_effect=failure_directory), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel, "pane_option", side_effect=lambda name: failure_tags.get(name, "")), \
            patch.object(panel, "session_owner", side_effect=lambda session: "/foreign/repository"
                         if recovery == "session" and len(failure_attempts) == 2 else str(project.resolve())), \
            patch.object(panel, "live_mains", return_value=0), patch.object(panel, "main_history", side_effect=failure_history), \
            patch.object(panel, "native_command", side_effect=failure_command), patch.object(panel, "bootstrap", return_value="fixture"), \
            patch.object(panel.subprocess, "run", side_effect=failure_tmux), \
            patch.object(panel.subprocess, "Popen", side_effect=failure_process), patch.object(Path, "write_text", new=failure_write), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch.object(panel, "_main_exit_key", side_effect=failure_key):
        failure_status = panel.run_native("claude", project)
    succeeds = recovery in ("retry", "close")
    assert len(failure_created) == (3 if recovery == "retry" else 2), "failed restart stranded the retry prompt"
    assert len(failure_attempts) == len(failure_created) - int(failure_step == "directory")
    assert len(failure_launches) == (2 if recovery == "retry" else 1)
    assert bool(failure_closes) == succeeds, "failed restart stranded close or adopted an unverified owner"
    if succeeds:
        assert failure_status == (0 if recovery == "retry" or failure_step == "directory" else 1)
        close = failure_closes[0]
        assert close[-1] == "kill-window -t @42" and close[close.index("-t") + 1] == "@42"
        assert "#{==:#{@oms_panel_main_attempt}," + failure_attempts[-1] + "}" in close[-2]
        assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in close[-2]
        assert "#{==:#{@oms_panel_native_pane},}" in close[-2]
    if failure_step in ("directory", "command", "popen"):
        # A failed startup must not block a later fresh launch through the real
        # legacy lifecycle guard, after either retry or guarded close.
        with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=failure_room), clear=True), \
                patch.object(panel, "resolve_session", return_value=("", "")), \
                patch.object(panel, "main_installed", return_value=True), patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel, "native_command", return_value=["fixture-native"]), \
                patch.object(panel, "bootstrap", return_value="fixture"), patch.object(panel.subprocess, "Popen", side_effect=failure_process):
            assert panel.run_native("claude", project) == 0
        assert all(row["terminal"] is True for row in real_history(project, failure_room) if row["tool"] == "panel-main")
# A successor takes the window after lookup. Only the server-side condition may
# delete the pinned window; a separate kill against the moved pane would delete it.
for change in ("attempt", "native-pane", "moved-pane"):
    deleted = []
    def close_race(command, **kwargs):
        if "display-message" in command:
            fields = "exit-attempt\t\t" + str(project.resolve())
            if "#{window_id}" in command[-1]:
                fields = "@42\t" + fields + "\texit-attempt"
            return subprocess.CompletedProcess(command, 0, fields + "\n", "")
        if "kill-window" in command:
            deleted.append("successor")
        elif "if-shell" in command:
            assert command[-1] == "kill-window -t @42" and command[command.index("-t") + 1] == "@42"
            condition = command[-2]
            assert "#{==:#{@oms_panel_main_attempt},exit-attempt}" in condition
            assert "#{==:#{@oms_panel_native_pane},}" in condition
            assert "#{==:#{@oms_panel_repo}," + str(project.resolve()) + "}" in condition
            if change == "moved-pane":
                deleted.append("original")
        return subprocess.CompletedProcess(command, 0, "", "")
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run", side_effect=close_race):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0
    assert "successor" not in deleted and deleted == (["original"] if change == "moved-pane" else [])
# Run the same close path against a real isolated server: ownership changes
# after display-message returns but before if-shell evaluates on that server.
close_tmux_binary = shutil.which("tmux")
if close_tmux_binary and os.name != "nt" and os.environ.get("OMS_OPERATOR_SOCKET_DENIED") != "1":
    close_tmux = [close_tmux_binary, "-S", str(temporary / "close-race.sock"), "-f", "/dev/null"]
    real_run = subprocess.run
    real_run(close_tmux + ["new-session", "-d", "-s", "close-race", "sleep 60"], check=True)
    try:
        for changed in (False, True):
            window_id, native_pane = subprocess.check_output(close_tmux + ["new-window", "-d", "-P", "-F",
                "#{window_id}\t#{pane_id}", "sleep 60"], text=True).strip().split("\t")
            for option, value in (("@oms_panel_main_attempt", "exit-attempt"), ("@oms_panel_native_pane", ""),
                                  ("@oms_panel_repo", str(project.resolve())), ("@oms_panel_room_participant", "exit-attempt")):
                real_run(close_tmux + ["set-option", "-w", "-t", window_id, option, value], check=True)
            def change_repository(command, **kwargs):
                result = real_run(command, **kwargs)
                if changed and "display-message" in command:
                    real_run(close_tmux + ["set-option", "-w", "-t", window_id,
                                          "@oms_panel_repo", "/foreign/repository"], check=True)
                return result
            with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE=native_pane, OMS_PANEL_SESSION="oms-exit"), clear=True), \
                    patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native), \
                    patch.object(panel, "managed_session", return_value=True), \
                    patch.object(panel, "tmux_command", side_effect=lambda *args: close_tmux + list(args)), \
                    patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
                    patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
                    patch.object(panel.subprocess, "run", side_effect=change_repository):
                exit_input.return_value.__enter__.return_value.fd = None
                assert panel.run_native("claude", project) == 0
            survivors = subprocess.check_output(close_tmux + ["list-windows", "-t", "close-race", "-F", "#{window_id}"], text=True).splitlines()
            assert (window_id in survivors) == changed, "server close guard ignored changed repository ownership"
    finally:
        real_run(close_tmux + ["kill-server"], capture_output=True)
# Ambiguous tmux format operands preserve the window before issuing if-shell.
from contextlib import nullcontext
for suffix in ("hash#repo", "comma,repo", "colon:repo", "brace{repo}", "backslash\\repo", "control\nrepo"):
    unsafe_repo = temporary / suffix
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "_lock", return_value=nullcontext()), patch.object(panel, "check_legacy_main"), \
            patch.object(panel, "run_single_native", side_effect=exited_native), patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "_main_window", return_value=["@42", "exit-attempt", "", str(unsafe_repo.resolve()), "exit-attempt"]), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", return_value=""), \
            patch.object(panel.subprocess, "run") as unsafe_close, patch.object(panel.sys, "stderr") as close_error:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", unsafe_repo) == 0
        assert not unsafe_close.called
        assert any("repository path cannot be safely embedded" in str(call) for call in close_error.write.call_args_list)
# A failed native marker query is unavailable evidence, even when pane_option
# would report an empty (unset) marker. Neither restart nor close may proceed.
for key in ("r", ""):
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-exit"), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=exited_native) as unavailable_start, \
            patch.object(panel, "managed_session", return_value=True), patch.object(panel, "pane_option", side_effect=exit_options.get), \
            patch.object(panel, "session_owner", return_value=str(project.resolve())), patch.object(panel, "live_mains", return_value=0), \
            patch.object(panel, "main_history", return_value=[dict(previous, provider="claude", terminal=True)]), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=[key, EOFError]), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")) as unavailable_query:
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("claude", project) == 0 and unavailable_start.call_count == 1
        assert not any("kill-window" in call.args[0] or "if-shell" in call.args[0] for call in unavailable_query.call_args_list)
import hook_state
from unittest.mock import Mock
# Fresh Codex receives its real session only through the first native hook.
# Without a stored native_session, r must start fresh despite that consumer.
for occupied in (False, True):
    fresh_room = room.create(project, "restart-fresh-codex-" + str(occupied).lower(), "Fresh Codex restart")
    fresh_launches, fresh_exit = [], {}
    real_popen = subprocess.Popen
    real_single = panel.run_single_native
    def fresh_process(command, **kwargs):
        if command[0] != "codex":
            return real_popen(command, **kwargs)
        assert "resume" not in command
        env = kwargs["env"]
        fresh_launches.append((command, env["OMS_ROOM_PARTICIPANT"], env["OMS_PANEL_MAIN_ATTEMPT"]))
        with patch.dict(os.environ, env, clear=True):
            hook_state._live_thread_hint({"hook_event_name": "UserPromptSubmit",
                "session_id": "fresh-codex-hook-session-%s-%s" % (occupied, len(fresh_launches))}, project)
        enrolled = room.participant(room.status(project, fresh_room), env["OMS_ROOM_PARTICIPANT"])
        assert enrolled.get("consumer"), "the first hook did not bind the fresh Codex consumer"
        process = Mock()
        process.wait.return_value = process.poll.return_value = 0
        return process
    def fresh_native(*args, **kwargs):
        with patch.object(panel, "managed_session", return_value=False):
            result = real_single(*args, **kwargs)
        fresh_exit.update(kwargs["_exit_info"])
        return result
    def fresh_window(pane):
        return ["@42", fresh_exit["attempt"], "", str(project.resolve()), fresh_launches[-1][1]]
    with patch.dict(os.environ, dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-fresh", OMS_ROOM_ID=fresh_room), clear=True), \
            patch.object(panel, "check_legacy_main"), patch.object(panel, "run_single_native", side_effect=fresh_native), \
            patch.object(panel, "managed_session", return_value=True), patch.object(panel, "_main_window", side_effect=fresh_window), \
            patch.object(panel, "session_owner", return_value=str(project.resolve())), \
            patch.object(panel, "live_mains", return_value=int(occupied)), patch.object(panel.subprocess, "Popen", side_effect=fresh_process), \
            patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
            patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
        exit_input.return_value.__enter__.return_value.fd = None
        assert panel.run_native("codex", project) == 0
    assert len(fresh_launches) == (1 if occupied else 2)
    for command, participant, attempt_id in fresh_launches:
        previous = next(row for row in panel.main_history(project, fresh_room) if row["attempt_id"] == attempt_id)
        assert previous["terminal"] and not previous["refs"].get("native_session")
# /clear, exit and r retain the actual participant, join boundary and mailbox.
clear_room = room.create(project, "restart-clear-room", "Restart after clear")
clear_session = "11111111-2222-4333-8444-555555555555"
clear_launches, clear_exit, clear_rooms = [], {}, []
real_popen = subprocess.Popen
def clear_process(command, **kwargs):
    if command[0] != "claude":
        return real_popen(command, **kwargs)
    env = kwargs["env"]
    clear_launches.append((command, env["OMS_ROOM_PARTICIPANT"], env["OMS_PANEL_MAIN_ATTEMPT"]))
    clear_rooms.append(env.get("OMS_ROOM_ID"))
    if len(clear_launches) == 1:
        room.join(project, clear_room, "clear-peer", "codex", role="worker")
        room.send(project, clear_room, "clear-peer", env["OMS_ROOM_PARTICIPANT"], "Keep this mailbox", message_id="clear-mail")
        with patch.dict(os.environ, env, clear=True):
            assert hook_state.rebind_cleared_session({"hook_event_name": "SessionStart", "source": "clear", "session_id": clear_session})
    else:
        assert command[command.index("--resume") + 1] == clear_session
    process = Mock()
    process.wait.return_value = process.poll.return_value = 0
    return process
real_single = panel.run_single_native
def clear_native(*args, **kwargs):
    with patch.object(panel, "managed_session", return_value=False):
        result = real_single(*args, **kwargs)
    clear_exit.update(kwargs["_exit_info"])
    return result
def clear_window(pane):
    return ["@42", clear_exit["attempt"], "", str(project.resolve()), clear_launches[0][1]]
def clear_option(name):
    return dict(zip(("@oms_panel_main_attempt", "@oms_panel_native_pane", "@oms_panel_repo", "@oms_panel_room_participant"),
                    clear_window("%42")[1:])).get(name, "")
clear_env = dict(environment, TMUX="fixture", TMUX_PANE="%42", OMS_PANEL_SESSION="oms-clear", OMS_ROOM_ID=clear_room)
with patch.dict(os.environ, clear_env, clear=True), patch.object(panel, "check_legacy_main"), \
        patch.object(panel, "session_stored", return_value=True), \
        patch.object(panel, "run_single_native", side_effect=clear_native), patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "_main_window", side_effect=clear_window, create=True), patch.object(panel, "pane_option", side_effect=clear_option), \
        patch.object(panel, "session_owner", return_value=str(project.resolve())), \
        patch.object(panel, "live_mains", return_value=0), patch.object(panel.subprocess, "Popen", side_effect=clear_process), \
        patch.object(panel.sys.stdin, "isatty", return_value=True), patch.object(panel.sys.stdout, "isatty", return_value=True), \
        patch("panel_input.TerminalInput") as exit_input, patch("builtins.input", side_effect=["r", EOFError]):
    exit_input.return_value.__enter__.return_value.fd = None
    assert panel.run_native("claude", project) == 0
assert len(clear_launches) == 2 and clear_launches[0][1] == clear_launches[1][1]
clear_member = clear_launches[0][1]
assert any(row["room_event"].get("id") == "clear-mail" for row in room.updates(project, clear_room, clear_member)["turns"])
# Current consumer binding also proves continuity for older stale attempt refs.
for command, who, attempt_id in clear_launches:
    panel.events(project, "usage", "--attempt", attempt_id, "--ref", "panel_session_digest=" + "0" * 32)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=clear_room), clear=True), \
        patch.object(panel, "managed_session", return_value=False), patch.object(panel.subprocess, "Popen", side_effect=clear_process):
    assert panel.run_single_native("claude", project, resume=clear_session) == 0
assert len(clear_launches) == 3 and clear_launches[-1][1] == clear_member
foreign_main = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                            "--ref", "panel_room_id=" + clear_room, "--ref", "panel_room_participant=" + clear_member,
                            "--ref", "panel_session_digest=" + "0" * 32, "--then", "starting", "--then", "working", output=True)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=clear_room), clear=True), \
        patch.object(panel, "native_command", side_effect=AssertionError("live main's session launched")):
    try:
        panel.run_single_native("claude", project, resume=clear_session)
    except ValueError as error:
        assert "terminal owner" in str(error), error
        assert "--release-main" not in str(error), error
    else:
        raise AssertionError("current consumer ignored another live main")
panel.events(project, "transition", "--attempt", foreign_main, "--state", "cancelled")
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=shared), clear=True), \
        patch.object(panel, "native_command", side_effect=AssertionError("another room's session launched")):
    try:
        panel.run_single_native("claude", project, resume=clear_session)
    except ValueError as error:
        assert "another room" in str(error)
    else:
        raise AssertionError("session moved to another room")
# A resumed main keeps the room its lineage started in after that room rolled over: proofs need one room id.
clear_next = room.rollover(project, clear_room, clear_member)
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=clear_next), clear=True), \
        patch.object(panel, "managed_session", return_value=False), patch.object(panel.subprocess, "Popen", side_effect=clear_process):
    assert panel.run_single_native("claude", project, resume=clear_session) == 0
assert len(clear_launches) == 4 and clear_launches[-1][1] == clear_member and clear_rooms[-1] == clear_room, clear_rooms
if os.name != "nt":
    import signal
    prompt_script = ("import sys,signal,os\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
        "os.environ['TMUX_PANE']='%42'\n"
        "p.check_legacy_main=lambda repo: None\np.managed_session=lambda: True\n"
        "def exited(*args,**kw): kw['_exit_info'].update(attempt='exit-attempt',room='exit-room'); return 0\n"
        "p.run_single_native=exited\n"
        "if sys.argv[3]=='wait':\n"
        " import select\n"
        " def wait(*args,**kw): os.kill(os.getpid(),int(sys.argv[4]))\n"
        " select.select=wait\n"
        "else:\n"
        " import panel_input\n"
        " original=panel_input.TerminalInput.__enter__\n"
        " def enter(self):\n"
        "  result=original(self); os.kill(os.getpid(),int(sys.argv[4])); return result\n"
        " panel_input.TerminalInput.__enter__=enter\n"
        "try: p.run_native('claude',Path(sys.argv[2]))\n"
        "except p.NativeShutdown as e: sys.exit(e.exit_code)\n")
    for number in (signal.SIGTERM, signal.SIGHUP):
        # Deliver shutdown at the exact input stage, independent of PTY output buffering.
        terminal([sys.executable, "-c", prompt_script, str(root / "scripts/lib"), str(project), "wait", str(number)], b"", b"\033[?1000l",
                 expected_exit=128 + number)
        terminal([sys.executable, "-c", prompt_script, str(root / "scripts/lib"), str(project), "enter", str(number)], b"", b"\033[?1000l",
                 expected_exit=128 + number)
with patch.object(panel.shutil, "which", return_value="/x"), \
        patch.object(panel, "resolve_session", return_value=("legacy-panel", str(project))), \
        patch.object(panel, "recorded_room", return_value="r1"), patch.object(panel, "live_mains", return_value=1), \
        patch.object(panel, "managed_session", return_value=False), patch.object(panel, "run_single_native") as native_start:
    try:
        panel.run_native("claude", project)
        raise AssertionError("an inline launch bypassed a legacy main")
    except ValueError as error:
        assert "already has a live main" in str(error)
    assert not native_start.called
failed_worker = {"participant": "adv-worker", "provider": "codex", "role": "worker", "joined": True, "label": "Worker",
                 "parent": "adv-main", "seq": 2}
failed_board = dict(board, room={"id": "adv-room", "participants": [claude_main, failed_worker]}, attempts={"active_recent": [
    {"attempt_id": "adv-attempt", "state": "failed", "task_id": "adv-task", "panel": {"role": "worker", "room_id": "adv-room",
     "room_participant": "adv-worker", "label": "Broken patch"}}], "recent": []})
assert "Broken patch" in graph_view.render_graph(failed_board, 100, 40)
assert "Broken patch" in graph_view.render_graph(dict(failed_board, finalized={"other": "accepted"}), 100, 40)
assert "Broken patch" not in graph_view.render_graph(dict(failed_board, finalized={"adv-task": "accepted"}), 100, 40)
# A call missing from a complete active list for ten minutes may be folded as unknown; a just-joined one stays,
# and a truncated active list (the projection keeps 8 rows) proves nothing.
untracked = dict(board, attempts={"available": True, "active": 0, "active_recent": [], "recent": []},
                 room={"id": "adv-room", "participants": [claude_main, dict(
    failed_worker, label="Old patch", joined_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 900)))]})
assert "Old patch" not in graph_view.render_graph(untracked, 100, 40)
assert "Old patch" in graph_view.render_graph(dict(untracked, attempts=dict(untracked["attempts"], active=9)), 100, 40)
untracked["room"]["participants"][1]["joined_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
assert "Old patch" in graph_view.render_graph(untracked, 100, 40)
# Review and history grouping shares settler evidence and changes no report/ledger state.
grouped_board = deepcopy(failed_board)
grouped_board["collection"] = {"ok": True}
grouped_board["awaiting_admission"] = ["pending-patch"]
grouped_board["attempts"].update(available=True, active=0)
for ident, phase in (("review-call", "review"), ("pending-patch", "done"), ("past-call", "cancelled"),
                     ("blocked-call", "blocked"), ("waiting-call", "waiting_input"), ("live-call", "working")):
    grouped_board["room"]["participants"].append(dict(failed_worker, participant=ident, label=ident))
    grouped_board["attempts"]["recent"].append({"attempt_id": ident, "state": phase, "panel": {
        "room_id": "adv-room", "room_participant": ident, "role": "worker"}})
grouped_board["room"]["participants"].append(dict(failed_worker, participant="old-unknown", label="old-unknown",
                                               joined_at=minutes_ago(20)))
grouped_board["room"]["participants"].append(dict(failed_worker, participant="fresh-unknown", label="fresh-unknown",
                                               joined_at=minutes_ago(1)))
frozen_grouped = json.dumps(grouped_board, sort_keys=True)
classify = graph_view.call_classifier(grouped_board)
classified = {m["participant"]: classify(m) for m in graph_view.nodes(grouped_board) if m["role"] != "main"}
assert classified == {"adv-worker": "live", "review-call": "review", "pending-patch": "review", "past-call": "past",
                      "blocked-call": "live", "waiting-call": "live", "live-call": "live",
                      "old-unknown": "past", "fresh-unknown": "live"}, classified
# An unadmitted patch follows the same Past classification as the board and the Needs-you strip.
review_fixture = deepcopy(grouped_board)
review_fixture["room"]["participants"] = [claude_main, dict(failed_worker, participant="stale-patch", label="Stale patch")]
review_fixture["attempts"] = {"available": True, "active": 0, "active_recent": [], "recent": [{
    "attempt_id": "stale-patch", "state": "done", "task_id": "retired-task", "panel": {
        "room_id": "adv-room", "room_participant": "stale-patch", "role": "worker"}}]}
review_fixture["awaiting_admission"] = ["stale-patch"]
review_fixture["plan"] = {"tasks": [{"id": "retired-task", "state": "retired"}]}
stale_node = next(m for m in graph_view.nodes(review_fixture) if m["participant"] == "stale-patch")
assert graph_view.call_classifier(review_fixture)(stale_node) == "past"
assert ("result", "stale-patch") not in {item["action"] for item in inbox_items(review_fixture)}
past_nav = {"tab": "past"}
render(review_fixture, "codex", 120, 80, view="tree", navigation=past_nav)
assert past_nav["past_ids"] == [("result", "stale-patch")], past_nav
review_fixture["plan"] = {"tasks": []}
next(m for m in review_fixture["room"]["participants"] if m["participant"] == "stale-patch")["joined_at"] = minutes_ago(13 * 60)
stale_node = next(m for m in graph_view.nodes(review_fixture) if m["participant"] == "stale-patch")
assert graph_view.call_classifier(review_fixture)(stale_node) == "past"
assert ("result", "stale-patch") not in {item["action"] for item in inbox_items(review_fixture)}
next(m for m in review_fixture["room"]["participants"] if m["participant"] == "stale-patch")["joined_at"] = minutes_ago(10)
stale_node = next(m for m in graph_view.nodes(review_fixture) if m["participant"] == "stale-patch")
assert graph_view.call_classifier(review_fixture)(stale_node) == "review"
assert ("result", "stale-patch") in {item["action"] for item in inbox_items(review_fixture)}
# The Attention strip counts the actionable items; the tab retains every row.
review_strip = needs_strip(review_fixture, 120, True, limit=100)
assert review_strip[0]["text"].startswith("Attention %s" % len(inbox_items(review_fixture))) \
    and len(review_strip) == 1 and {item["action"] for item in attention_items(review_fixture)} >= \
    {item["action"] for item in inbox_items(review_fixture)}, \
    "Needs-you strip count must match actionable inbox items"
# An exited owner with an exited successor did not adopt the patch; a live successor did.
handoff = deepcopy(review_fixture)
handoff["room"]["participants"].append({"participant": "successor", "provider": "codex", "role": "main",
    "joined": True, "parent": None, "seq": 3, "label": "Successor"})
handoff["attempts"]["recent"] += [
    {"attempt_id": "owner-main", "state": "done", "panel": {"room_id": "adv-room", "room_participant": "adv-main", "role": "main"}},
    {"attempt_id": "successor-main", "state": "done", "panel": {"room_id": "adv-room", "room_participant": "successor", "role": "main"}}]
stale_attempt = next(a for a in handoff["attempts"]["recent"] if a["attempt_id"] == "stale-patch")
stale_attempt["task_id"] = "held-review"
handoff["room"]["participants"][1]["parent"] = "adv-main"
handoff["room"]["participants"][1]["joined_at"] = minutes_ago(10)
handoff["plan"] = {"present": True, "goal": "Adopt patch", "idle_days": 0, "task_count": 1,
                   "by_state": {"running": 1}, "tasks": [{"id": "held-review", "state": "running",
                   "claimed_by": "successor"}]}
owner_successor_done = next(m for m in graph_view.nodes(handoff) if m["participant"] == "stale-patch")
assert graph_view.call_classifier(handoff)(owner_successor_done) == "past"
assert "held-review" not in graph_view.held_split(handoff)[0]
assert graph_view.plan_summary(handoff)[3] == 0 and not inbox_items(handoff)
handoff_banner = graph_view.goal_banner(handoff, 127, True)[0][0]
assert "review" not in handoff_banner, handoff_banner
for activity_state, kind in (("idle", None), ("quiet", None), ("waiting", "input"), ("waiting", "approval")):
    handoff["attempts"]["recent"][-1]["state"] = "working"
    reading = {"attempt": "successor-main", "state": activity_state}
    if kind:
        reading["kind"] = kind
    handoff["main_activity"] = {"successor": reading}
    live_review = next(m for m in graph_view.nodes(handoff) if m["participant"] == "stale-patch")
    assert graph_view.call_classifier(handoff)(live_review) == "review", (activity_state, kind)
    assert ("result", "stale-patch") in {item["action"] for item in inbox_items(handoff)}, (activity_state, kind)
    # The goal banner counts only plan review; the held task waits on its call instead.
    assert graph_view.plan_summary(handoff)[3] == 0 and "held-review" in graph_view.held_split(handoff)[0], \
        (activity_state, kind, graph_view.plan_summary(handoff), graph_view.held_split(handoff))
    handoff_banner = graph_view.goal_banner(handoff, 127, True)[0][0]
    assert "1 claimed" in handoff_banner and "review" not in handoff_banner, (activity_state, kind, handoff_banner)
handoff["attempts"]["recent"][-1]["state"] = "done"
handoff.pop("main_activity")
owner_successor_done = next(m for m in graph_view.nodes(handoff) if m["participant"] == "stale-patch")
assert graph_view.call_classifier(handoff)(owner_successor_done) == "past"
assert graph_view.plan_summary(handoff)[3] == 0
# The owner's recorded decision on a task closes its review and its block; a failed decision does not, a rejected one does.
for outcome, expected in (("completed", "past"), ("accepted", "past"), ("failed", None), ("rejected", "past")):
    decided = graph_view.call_classifier(dict(grouped_board, finalized={"decided-task": outcome}))
    for ident in ("review-call", "blocked-call"):
        node = dict(next(m for m in graph_view.nodes(grouped_board) if m["participant"] == ident), task_id="decided-task")
        assert decided(node) == (expected or classified[ident]), (outcome, ident, decided(node))
# A rejected predecessor review is no longer owed, surfaced, or counted as window attention.
rejected_predecessor = deepcopy(review_fixture)
old_review = next(m for m in rejected_predecessor["room"]["participants"] if m["participant"] == "stale-patch")
rejected_predecessor["room"]["participants"].remove(old_review)
rejected_predecessor["room"]["predecessor_calls"] = [dict(old_review, _predecessor="old-room", task_id="held-review")]
rejected_predecessor["room"]["lineage"] = ["old-room"]
rejected_predecessor["finalized"] = {"held-review": "rejected"}
assert "stale-patch" not in {m["participant"] for m in graph_view.nodes(rejected_predecessor)}
assert not any(item["action"] == ("result", "stale-patch") for item in attention_items(rejected_predecessor))
assert "held-review" in graph_view.closed_tasks(rejected_predecessor)
assert not panel.main_needs_attention(rejected_predecessor, "adv-main")
window = {"room": {"id": "current", "participants": [
    {"participant": "window-main", "role": "main", "joined": True},
    {"participant": "window-review", "role": "review", "parent": "window-main", "joined": True, "task_id": "window-task"}]},
    "attempts": {"recent": [
        {"attempt_id": "window-main", "state": "working", "panel": {"room_id": "current", "room_participant": "window-main", "role": "main"}},
        {"attempt_id": "window-review", "state": "review", "task_id": "window-task", "panel": {"room_id": "current", "room_participant": "window-review", "role": "review"}}]},
    "room_attempts": {}, "finalized": {"window-task": "rejected"}}
assert not panel.main_needs_attention(window, "window-main")
window["finalized"] = {}
assert panel.main_needs_attention(window, "window-main")
# A review that a later --continue round of its task superseded no longer waits on the owner.
superseded_review = dict(next(m for m in graph_view.nodes(grouped_board) if m["participant"] == "review-call"), superseded=True)
assert graph_view.call_classifier(grouped_board)(superseded_review) == "past", superseded_review
# A stale admission flag cannot hide current work, uncertainty or an urgent failure.
mixed = deepcopy(grouped_board)
mixed["awaiting_admission"] = list(classified)
mixed_classify = graph_view.call_classifier(mixed)
assert {m["participant"]: mixed_classify(m) for m in graph_view.nodes(mixed) if m["role"] != "main"} == classified
from panel_view import inbox_items
mixed_inbox = {item["action"]: item["what"] for item in inbox_items(mixed)}
assert "failed" in mixed_inbox[("result", "adv-worker")]
assert ("result", "blocked-call") not in mixed_inbox  # No stale patch-review alert replaces its live board state.
assert "input" in mixed_inbox[("result", "waiting-call")]
assert ("result", "live-call") not in mixed_inbox and ("result", "fresh-unknown") not in mixed_inbox
for phase in ("working", "verifying", "waiting_input", "waiting_approval", "blocked", "failed", "timed_out", "orphaned"):
    candidate = dict(next(m for m in graph_view.nodes(mixed) if m["participant"] == "live-call"), state=phase)
    assert mixed_classify(candidate) == "live", phase
for evidence in ({"available": False, "active": 0}, {"available": True, "active": 9}, {"available": True, "active": None}):
    uncertain = dict(grouped_board, attempts=dict(grouped_board["attempts"], **evidence))
    unknown = next(m for m in graph_view.nodes(uncertain) if m["participant"] == "old-unknown")
    assert graph_view.call_classifier(uncertain)(unknown) == "live"
# Calls whose main is gone group like linked ones; the graph counts only the open ones.
orphans = deepcopy(grouped_board)
orphans["room"]["participants"] = [claude_main] + [dict(m, parent="gone-main") for m in grouped_board["room"]["participants"]
                                                  if m["participant"] in {"pending-patch", "past-call", "review-call", "live-call"}]
for mode in ("tree", "graph"):
    for glyphs in (True, False):
        nav = {"dismissed": True}
        picture = render(orphans, "codex", 120, 80, view=mode, unicode=glyphs, navigation=nav)
        if mode == "tree":
            assert ("result", "live-call") in nav["items"] and ("result", "past-call") not in nav["items"], nav
            assert "Past work" not in picture and "Awaiting decision (2)" in picture, picture
            target = ("group", ("review", "unlinked"))
            hit = next(h for h in nav["hits"] if h["action"] == target)
            assert choose(("click", hit["x1"], hit["y"]), nav) == target
            panel.navigate(project, target, orphans, nav, 120)
            render(orphans, "codex", 120, 80, view=mode, unicode=glyphs, navigation=nav)
            assert ("result", "review-call") in nav["items"] and ("result", "past-call") not in nav["items"], nav
            # A settled call with no known main is listed in the Past tab, where it stays selectable.
            past_nav = dict(nav, tab="past")
            render(orphans, "codex", 120, 80, view=mode, unicode=glyphs, navigation=past_nav)
            assert past_nav["past_ids"] == [("result", "past-call")] and ("result", "past-call") in past_nav["items"], past_nav
        else:
            assert "3 calls with no known main (+1 past)" in picture, picture
for mode in ("tree", "graph"):
    nav = {"dismissed": True}
    picture = render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
    assert ("Awaiting decision (2)" if mode == "tree" else "awaiting decision") in picture and "Past work" not in picture and "Past 2" in picture, picture
    # Collapsed children stay out of the body; the Attention tab retains their links.
    pinned = {item["action"] for item in attention_items(grouped_board)}
    assert any(h["action"] == ("tab", "needs") for h in nav["hits"]), (mode, nav)
    body_top = (next(i + 1 for i, line in enumerate(picture.splitlines()) if line.startswith("╭─ Activity"))
                if mode == "tree" else min(b["y1"] for b in nav["bands"] if b["name"] != "detail"))
    for ident in ("review-call", "pending-patch", "past-call", "old-unknown"):
        target = ("result", ident)
        child_hits = [h for h in nav["hits"] if h["action"] == target]
        assert mode == "graph" and ident in {"review-call", "pending-patch"} or not any(h["y"] >= body_top for h in child_hits), (mode, ident, nav)
        if mode == "graph" and ident in {"review-call", "pending-patch"}:
            assert target in nav["items"] and child_hits, (mode, ident, nav)
        else:
            assert target not in nav["items"] and not child_hits, (mode, ident, nav)
        assert (target in pinned) == (ident in {"review-call", "pending-patch"}), (mode, ident, pinned)
    for ident in ("adv-worker", "blocked-call", "waiting-call", "live-call", "fresh-unknown"):
        assert ("result", ident) in nav["items"], (mode, ident, nav)
    mixed_nav = {"dismissed": True}
    mixed_picture = render(mixed, "codex", 120, 80, view=mode, navigation=mixed_nav)
    assert ("Awaiting decision (2)" if mode == "tree" else "awaiting decision") in mixed_picture and "Past 2" in mixed_picture
    assert mixed_nav["items"] == nav["items"], (mode, mixed_nav)
    group_hits = [h for h in nav["hits"] if h["action"][0] == "group"]
    assert len(group_hits) == (1 if mode == "tree" else 0), group_hits
    # Past work is not a board group: the Past tab lists it, newest first, unknown presence in words.
    past_nav = {"dismissed": True, "tab": "past"}
    past_picture = render(grouped_board, "codex", 120, 80, view=mode, navigation=past_nav)
    assert set(past_nav["past_ids"]) == {("result", "past-call"), ("result", "old-unknown")}, past_nav
    assert "status unknown" in past_picture and "Past work" not in past_picture, past_picture
    for kind in (("review",) if mode == "tree" else ()):
        target = ("group", (kind, "adv-main"))
        hit = next(h for h in nav["hits"] if h["action"] == target)
        assert choose(("click", hit["x1"], hit["y"]), nav) == target
        assert choose(("enter",), nav) == choose((" ",), nav) == target
        with patch.object(panel.room, "status", side_effect=AssertionError("group read room")), \
                patch.object(panel_chats, "open_chat", side_effect=AssertionError("group opened chat")):
            panel.navigate(project, target, grouped_board, nav, 120)
        picture = render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
        assert ("result", "pending-patch") in nav["items"]
    assert "patch awaits admission" in picture and "status unknown" in picture
    result_target = ("result", "pending-patch")
    result_hit = next(h for h in nav["hits"] if h["action"] == result_target)
    if nav.get("surface") == "graph":
        # The graph board shows the call in its own detail area; click and Enter keep that preview.
        assert choose(("click", result_hit["x1"], result_hit["y"]), nav) is None
        assert nav["preview"]["target"] == result_target
        assert choose(("enter",), nav) is None and nav["preview"]["target"] == result_target
    else:
        assert choose(("click", result_hit["x1"], result_hit["y"]), nav) == result_target
        assert choose(("enter",), nav) == result_target
    with patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel.room, "status", return_value=grouped_board["room"]):
        panel.navigate(project, result_target, grouped_board, nav, 120)
        assert nav.pop("opening") == result_target
    departed = deepcopy(grouped_board["room"])
    next(m for m in departed["participants"] if m["participant"] == "pending-patch")["joined"] = False
    with patch.object(panel, "managed_session", return_value=False), patch.object(panel.room, "status", return_value=departed):
        try:
            panel.navigate(project, result_target, grouped_board, nav, 120)
            raise AssertionError("departed history result was opened")
        except ValueError as error:
            assert "participant left" in str(error)
    # Group drill-down belongs to the tree view; the graph board lists every call without groups.
    if mode == "tree":
        assert nav["expanded_groups"] == {("review", "adv-main")}
        kept = panel.refreshed_navigation(nav, grouped_board)
        assert kept["expanded_groups"] == nav["expanded_groups"]
        reset = panel.refreshed_navigation(dict(nav), {"room": {"id": "different-room"}})
        assert not reset.get("expanded_groups")
    # Narrow boards fall back to the tree view; open the decision group as a user would.
    nav.setdefault("expanded_groups", set()).add(("review", "adv-main"))
    for columns in (40, 56):
        for glyphs in (True, False):
            narrow = render(grouped_board, "codex", columns, 80, view=mode, unicode=glyphs, navigation=nav)
            assert "patch awaits admission" in narrow and "finished" in narrow, narrow
            alert_row = next(i for i, line in enumerate(narrow.splitlines()) if "patch awaits admission" in line)
            label_row = next(i for i, line in enumerate(narrow.splitlines()) if "pending-patch" in line)
            assert label_row < alert_row, narrow
            assert all(any(h["y"] == row + 1 and h["action"] == result_target for h in nav["hits"])
                       for row in (label_row, alert_row)), (nav, narrow)
    for columns, rows in ((120, 80), (76, 16), (28, 12), (2, 8)):
        for glyphs in (True, False):
            small = render(grouped_board, "codex", columns, rows, view=mode, unicode=glyphs, navigation=nav)
            assert len(small.splitlines()) <= rows and all(display_width(line) <= columns for line in small.splitlines())
    render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
    if mode == "tree":
        # Choosing an open group again closes it.
        panel.navigate(project, ("group", ("review", "adv-main")), grouped_board, nav, 120)
        render(grouped_board, "codex", 120, 80, view=mode, navigation=nav)
        assert not nav["expanded_groups"]
    assert nav["surface"] == mode
# Both existing return controls exit inspection; room data and other local preferences stay intact.
for back_key in ("escape", "g"):
    clock = [0.0]
    inspected = []
    real_frame = panel.frame_view
    def group_frame(*args, **kwargs):
        nav = kwargs["navigation"]
        if not inspected:
            nav.update(group_tree=True, expanded_groups={("review", "adv-main")}, pinned=["adv-main"],
                       collapsed={"unrelated-main"}, selected=("group", ("review", "adv-main")))
        picture = real_frame(*args, **kwargs)
        inspected.append(deepcopy(nav))
        return picture
    def return_graph(seconds):
        clock[0] += 5
        return [(back_key,)]
    with patch.object(panel, "BackgroundRead", ImmediateRead), \
            patch.object(panel, "read_panel", return_value=(grouped_board, 0, "codex", None, "graph", False, "")), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((120, 80))), \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel, "frame_view", side_effect=group_frame), \
            patch.object(ResponsiveInput, "wait", side_effect=return_graph), \
            patch("panel_input.TerminalInput", ResponsiveInput):
        assert panel.watch(project, "codex", count=2, view="graph", no_animation=True) == 0
    assert inspected[0]["surface"] == "tree" and inspected[-1]["surface"] == "graph"
    assert not inspected[-1].get("group_tree") and not inspected[-1].get("expanded_groups")
    assert inspected[-1]["pinned"] == ["adv-main"] and inspected[-1]["collapsed"] == {"unrelated-main"}
keyboard_nav = {"surface": "graph", "items": [("chat", "adv-main"), ("group", ("review", "adv-main"))],
                "selected": ("chat", "adv-main")}
choose(("down",), keyboard_nav)
assert choose(("enter",), keyboard_nav) == ("group", ("review", "adv-main")) and "preview" not in keyboard_nav
for columns, rows in ((120, 80), (76, 16)):
    published = dict(grouped_board, room=dict(grouped_board["room"], publications={"claude": {
        "status": "submitted", "delivery_unknown": True}}))
    shown = render(published, "codex", columns, rows, view="graph", navigation={
        "group_tree": True, "expanded_groups": {("review", "adv-main")}})
    assert "APP DELIVERY" not in shown and "att_" not in shown, shown
    assert not any(item["who"] == "Main wake-up delivery failed" for item in inbox_items(published))
# The graph board has no group drill-down any more; the tree view shows the inspected group.
for native_call in (open_call, answered_call):
    for glyphs in (False, True):
        for selected in (("chat", "adv-main"), ("result", "native-advisor-adv-main")):
            native_nav = {"group_tree": True, "expanded_groups": {("review", "adv-main")}, "selected": selected}
            native_inspection = render(dict(grouped_board, native_advisors=native_call), "claude", 120, 80,
                                       view="tree", unicode=glyphs, navigation=native_nav)
            assert "Fable 5.1" in native_inspection and "answer stays in the main transcript" in native_inspection, native_inspection
            if glyphs:
                tree_rows = native_inspection.splitlines()
                assert not any("└╌╌" in left and "└╌╌" in right
                               for left, right in zip(tree_rows, tree_rows[1:])), native_inspection
            assert ("result", "native-advisor-adv-main") not in native_nav["items"], native_nav
attention_nav = {}
attention = render(grouped_board, "codex", 120, 80, view="tree", attention_only=True, navigation=attention_nav)
assert "Awaiting decision (2)" in attention and "Past work" not in attention
from panel_view import inbox_items
assert {item["action"] for item in inbox_items(grouped_board)} >= {("result", "review-call"), ("result", "pending-patch")}
needs_words = {item["action"]: item["what"] for item in inbox_items(grouped_board)}
assert needs_words[("result", "review-call")].startswith("awaiting decision") \
    and needs_words[("result", "pending-patch")].startswith("patch awaits admission"), needs_words
# The real board keys select an artifactless failure from Attention and Enter opens its call detail.
attention_keys = {"tab": "needs"}
render(grouped_board, "codex", 120, 80, view="tree", attention_only=True, navigation=attention_keys)
guard_target = ("result", "adv-worker")
assert guard_target in attention_keys["needs_ids"], attention_keys
for unused in range(attention_keys["needs_ids"].index(guard_target) + 1):
    assert panel.tab_event(("down",), attention_keys)
assert panel.tab_event(("enter",), attention_keys) and attention_keys["preview"]["target"] == guard_target, attention_keys
# Reading a call's own handoff handles its failure, but cannot settle a block or admit a patch.
for sender, recipient, pending, visible in (("adv-worker", "adv-main", [], False),
                                           ("adv-worker", "adv-main", ["adv-main"], True),
                                           ("blocked-call", "adv-main", [], True),
                                           ("adv-worker", "other-main", [], True)):
    handed_off = deepcopy(grouped_board)
    handed_off["room"]["messages"] = [{"id": "result-adv-worker", "message_kind": "handoff",
        "sender": sender, "recipient": recipient, "pending_for": pending}] + [
        {"id": "result-" + ident, "message_kind": "handoff", "sender": ident,
         "recipient": "adv-main", "pending_for": []} for ident in ("blocked-call", "review-call", "pending-patch")]
    frozen_handoff = json.dumps(handed_off, sort_keys=True)
    for mode in ("tree", "graph"):
        attention_nav = {"dismissed": True}
        attention_picture = render(handed_off, "codex", 120, 80, view=mode,
                                   attention_only=True, navigation=attention_nav)
        assert (("result", "adv-worker") in attention_nav["items"]) == visible, (mode, sender, recipient, pending, attention_nav)
        assert ("Broken patch" in attention_picture) == visible, (mode, sender, recipient, pending, attention_picture)
        assert ("result", "blocked-call") in attention_nav["items"] and "blocked-call" in attention_picture, (mode, attention_nav)
        assert ("Awaiting decision (2)" if mode == "tree" else "awaiting decision") in attention_picture, (mode, attention_picture)
    assert json.dumps(handed_off, sort_keys=True) == frozen_handoff
for mark in ("completed", "accepted"):
    handled = dict(grouped_board, finalized={"adv-task": mark})
    # A joined judge can have a call-result without a projected lifecycle attempt.
    attemptless = deepcopy(handled)
    attemptless["room"]["participants"].append({"participant": "attemptless-reviewer", "role": "reviewer",
        "joined": True, "parent": "adv-main", "model": "gpt-6.1-sol"})
    attemptless["room"].setdefault("call_results", {})["attemptless-reviewer"] = {"exit": 2, "ts": stamp,
        "task_id": "adv-task", "read": False}
    attemptless_call = next(m for m in graph_view.nodes(attemptless) if m["participant"] == "attemptless-reviewer")
    assert attemptless_call["state"] == "failed" and graph_view.call_classifier(attemptless)(attemptless_call) == "past"
    for mode in ("tree", "graph"):
        live_nav = {"dismissed": True}
        live_picture = render(attemptless, "codex", 120, 80, view=mode, navigation=live_nav)
        assert ("result", "attemptless-reviewer") not in live_nav["items"], (mode, live_picture)
        past_nav = {"dismissed": True, "tab": "past"}
        past_picture = render(attemptless, "codex", 120, 80, view=mode, navigation=past_nav)
        assert ("result", "attemptless-reviewer") in past_nav["past_ids"] and "REVIEWER Sol 6.1" in past_picture, (mode, past_picture)
    undecided = deepcopy(attemptless)
    undecided["finalized"] = {}
    undecided_call = next(m for m in graph_view.nodes(undecided) if m["participant"] == "attemptless-reviewer")
    assert graph_view.call_classifier(undecided)(undecided_call) == "live"
    for mode in ("tree", "graph"):
        live_nav = {"dismissed": True}
        live_picture = render(undecided, "codex", 120, 80, view=mode, navigation=live_nav)
        assert ("result", "attemptless-reviewer") in live_nav["items"], (mode, live_picture)
    # A review call whose task its main decided reads "decided" in its Detail, the main's Detail and Past alike.
    decided_board = deepcopy(handled)
    next(a for a in decided_board["attempts"]["recent"] if a["attempt_id"] == "review-call")["task_id"] = "adv-task"
    decided_people = graph_view.nodes(decided_board)
    decided_call = next(m for m in decided_people if m["participant"] == "review-call")
    assert decided_call["state"] == "review" and graph_view.call_classifier(decided_board)(decided_call) == "past"
    decided_head = graph_view.detail(decided_call, {"report": {"rows": []}}, 100, decided_people, {})[0][0]
    assert "· decided" in decided_head and "awaiting decision" not in decided_head, decided_head
    decided_nav = {"preview": {"target": ("chat", "adv-main"), "report": {}}, "dismissed": True}
    decided_main = graph_view.render_graph(decided_board, 127, 80, navigation=decided_nav)
    assert "review-call" not in decided_main and "Past" in decided_main, decided_main
    decided_past = render(decided_board, "codex", 120, 80, view="graph", navigation={"dismissed": True, "tab": "past"})
    decision_tag = "[accepted]" if mark == "accepted" else "[main decided]"
    assert any("review-call" in line and decision_tag in line for line in decided_past.splitlines()), decided_past
    failed = next(m for m in graph_view.nodes(handled) if m["participant"] == "adv-worker")
    assert graph_view.call_classifier(handled)(failed) == "past"
    for mode in ("tree", "graph"):
        filtered_nav = {"dismissed": True}
        filtered = render(handled, "codex", 120, 80, view=mode, attention_only=True, navigation=filtered_nav)
        assert "Broken patch" not in filtered and ("result", "adv-worker") not in filtered_nav["items"]
        assert ("result", "blocked-call") in filtered_nav["items"]
        history_nav = {"dismissed": True, "tab": "past"}
        history = render(handled, "codex", 120, 80, view=mode, navigation=history_nav)
        assert ("result", "adv-worker") in history_nav["past_ids"] and "Broken patch" in history, (mode, history)
    orphan_handled = deepcopy(handled)
    next(m for m in orphan_handled["room"]["participants"] if m["participant"] == "adv-worker")["parent"] = "gone-main"
    for mode in ("tree", "graph"):
        orphan_nav = {"dismissed": True}
        filtered = render(orphan_handled, "codex", 120, 80, view=mode, attention_only=True, navigation=orphan_nav)
        assert "Broken patch" not in filtered and ("result", "adv-worker") not in orphan_nav["items"]
        assert "call with no known main" not in filtered and "Calls with no known main" not in filtered
research_board = deepcopy(grouped_board)
research_board["room"]["participants"].append({"participant": "research-call", "role": "researcher",
    "provider": "codex", "model": "gpt-6-luna", "parent": "adv-main", "joined": True, "seq": 99})
research_board["attempts"]["active_recent"].append({"attempt_id": "research-attempt", "state": "working",
    "task_id": "research-task", "panel": {"role": "researcher", "room_id": "adv-room",
    "room_participant": "research-call", "model": "gpt-6-luna", "label": "Research citations"}})
for mode in ("tree", "graph"):
    for columns, rows in ((120, 80), (80, 24)):
        research_nav = {}
        candidate = deepcopy(research_board)
        if rows == 24:
            candidate['room']['participants'] = [m for m in candidate['room']['participants']
                if m['role'] == 'main' or m['participant'] == 'research-call']
        picture = render(candidate, "codex", columns, rows, view=mode, navigation=research_nav)
        assert "Research citations" in picture or "researcher" in picture.lower() or "RES" in picture, picture
        assert ("result", "research-call") in research_nav["items"], research_nav
assert json.dumps(grouped_board, sort_keys=True) == frozen_grouped
resumed_lifecycle = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                      "show", "--attempt", resumed["attempt"], "--json"]))
resumed_lifecycle.update(terminal=False, state="blocked")
def resumed_navigation(command, **kwargs):
    data = (json.dumps([resumed_lifecycle]) if command[0] == "bash" else
            "@42\t" + resumed["attempt"] + "\t" + shared + "\t%42\t" + member + "\t" + str(project.resolve()) + "\t$1\n"
            if command[1] == "list-windows" else "$1\n" if command[1] == "display-message" else "%42\t@42\n")
    return subprocess.CompletedProcess(command, 0, data, "")
with patch.dict(os.environ, dict(recovery_env, TMUX="fixture-tmux"), clear=True), \
        patch.object(panel_chats.shutil, "which", return_value="/fixture/tmux"), \
        patch.object(panel_chats.subprocess, "run", side_effect=resumed_navigation):
    assert panel_chats.windows(project, shared)[member][0]["pane"] == "%42"
    resumed_lifecycle["terminal"] = True
    assert panel_chats.windows(project, shared) == {}
if os.name != "nt":
    import signal
    int_log = temporary / "native-turn-interrupt.txt"
    int_env = dict(recovery_env, OMS_OPEN_INT_LOG=str(int_log))
    int_script = ("import sys,signal\nfrom pathlib import Path\nsys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
                  "before=signal.getsignal(signal.SIGINT)\nassert p.run_native('roomtest',Path(sys.argv[2]))==0\n"
                  "assert signal.getsignal(signal.SIGINT)==before\n")
    interrupted = subprocess.run([sys.executable, "-c", int_script, str(root / "scripts/lib"), str(project)],
                                 env=int_env, capture_output=True, text=True, start_new_session=True, timeout=20)
    assert interrupted.returncode == 0 and int_log.read_text() == "interrupted turn", interrupted.stderr
    for name in ("SIGTERM", "SIGHUP"):
        if not hasattr(signal, name):
            continue
        signal_env = dict(environment, OMS_ROOM_ID=shared, OMS_OPEN_LOG=str(open_log),
                          OMS_OPEN_CONTEXT=str(open_context), OMS_OPEN_SIGNAL=name,
                          OMS_PROVIDER_NATIVE_ADAPTERS="roomtest")
        script = ("import sys, signal\nfrom pathlib import Path\n"
                  "sys.path.insert(0,sys.argv[1])\nimport terminal_panel as p\n"
                  "before={n:signal.getsignal(getattr(signal,n)) for n in ('SIGTERM','SIGHUP')}\n"
                  "try: p.run_native('roomtest',Path(sys.argv[2]))\n"
                  "except KeyboardInterrupt: pass\n"
                  "else: raise AssertionError('native signal was not handled')\n"
                  "assert all(signal.getsignal(getattr(signal,n))==v for n,v in before.items())\n")
        finished = subprocess.run([sys.executable, "-c", script, str(root / "scripts/lib"), str(project)],
                                  env=signal_env, capture_output=True, text=True, timeout=25)
        assert finished.returncode == 0, finished.stderr
        signalled = json.loads(open_log.read_text())
        status = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                 "show", "--attempt", signalled["participant"], "--json"]))
        assert status["terminal"] and status["state"] == "cancelled", status
        assert not Path(signalled["context"]).exists()
if os.name != "nt":
    import time, signal, select
    # restore_signals does not reset an inherited SIG_IGN for SIGINT. Set the child's
    # disposition before exec so background suite launches exercise the same interrupt path.
    def default_sigint():
        signal.signal(signal.SIGINT, signal.SIG_DFL)

    def ignored_sigint():
        signal.signal(signal.SIGINT, signal.SIG_IGN)

    stop_log = temporary / "panel-stop-watch.log"
    with stop_log.open("w") as output:
        watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                    "--room", shared, "--watch", "--no-animation"],
                                   env=recovery_env, stdout=output, stderr=subprocess.STDOUT,
                                   preexec_fn=default_sigint)
        try:
            deadline = time.monotonic() + 10
            while "Goal" not in stop_log.read_text() and time.monotonic() < deadline:
                assert watcher.poll() is None
                time.sleep(0.1)
            assert "Goal" in stop_log.read_text(), "watcher did not become ready"
            watcher.send_signal(signal.SIGINT)
            assert watcher.wait(timeout=10) == 130, "public panel did not receive SIGINT"
        finally:
            if watcher.poll() is None:
                watcher.kill()
                watcher.wait(timeout=5)
    # Block each caller-thread phase on a pipe handshake, then interrupt it once.
    # Count=1 enters restore/finally naturally, so their blocks precede the signal.
    phase_script = r'''
import os, signal, sys, threading
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import terminal_panel as panel
import panel_input
phase = sys.argv[3]
ignored = os.environ.get('OMS_TEST_WATCH_SIGINT') == 'ignored'
expected = signal.SIG_IGN if ignored else signal.default_int_handler
assert signal.getsignal(signal.SIGINT) == expected
render = panel.frame_view
def blocked(*args, **kwargs):
    os.write(2, b'PHASE_BLOCKED\n')
    if ignored:
        assert os.read(0, 1) == b'x'
    else:
        threading.Event().wait(60)
    if phase == 'render':
        return render(*args, **kwargs)
panel.read_panel = lambda *args: ({'repo': {'name': 'fixture'}, 'collection': {'ok': True}},
                                 0, 'codex', None, 'auto', False, '')
panel.managed_session = lambda: False
panel.terminal_style = lambda: (False, False, True)
if phase == 'startup':
    panel.source_stamps = blocked
elif phase == 'render':
    panel.frame_view = blocked
elif phase == 'restore':
    panel_input.TerminalInput.__exit__ = blocked
elif phase in ('cleanup', 'cleanup-process'):
    panel.managed_session = lambda: True
    panel.pane_option = lambda name: ''
    panel.session_option = lambda name: ''
    if phase == 'cleanup':
        panel.panel_reader.close_reader = blocked
    else:
        def cleanup(*args):
            child = panel.subprocess.Popen([sys.executable, '-c',
                "import os,threading;os.write(2,b'PHASE_BLOCKED\\n');threading.Event().wait(60)"])
            try:
                child.wait(timeout=60)
            finally:
                child.kill()
                child.wait(timeout=2)
        panel.panel_reader.close_reader = cleanup
status = panel.main(['--repo', sys.argv[2], '--watch', '--count', '1', '--no-animation'])
assert signal.getsignal(signal.SIGINT) == expected
sys.exit(status)
'''
    # Use a python3 shim solely to inject probes while still exercising both public wrappers.
    shim_dir = temporary / "watch-phase-bin"
    shim_dir.mkdir(exist_ok=True)
    phase_file = temporary / "watch-phase.py"
    phase_file.write_text(phase_script)
    shim = shim_dir / "python3"
    shim.write_text("#!/usr/bin/env bash\nexec %s %s %s %s \"$OMS_TEST_WATCH_PHASE\"\n" % (
        shlex.quote(sys.executable), shlex.quote(str(phase_file)),
        shlex.quote(str(root / "scripts/lib")), shlex.quote(str(project))))
    shim.chmod(0o755)
    for phase in ("startup", "render", "restore", "cleanup", "cleanup-process"):
        watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                    "--watch", "--no-animation"],
            env=dict(recovery_env, PATH=str(shim_dir) + os.pathsep + recovery_env["PATH"], OMS_TEST_WATCH_PHASE=phase),
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
            start_new_session=True, preexec_fn=default_sigint)
        try:
            assert select.select([watcher.stderr], [], [], 5)[0], (phase, "phase never reached")
            assert watcher.stderr.readline() == b"PHASE_BLOCKED\n", phase
            watcher.send_signal(signal.SIGINT)
            assert watcher.wait(timeout=2) == 130, (phase, "default SIGINT delayed")
        finally:
            if watcher.poll() is None:
                os.killpg(watcher.pid, signal.SIGKILL)
                watcher.wait(timeout=5)
    # Inherited SIG_IGN stays ignored through the public wrappers and count=1 cleanup.
    # Release the blocked render only after sending SIGINT; a handler override exits 130.
    watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                "--watch", "--count", "1", "--no-animation"],
        env=dict(recovery_env, PATH=str(shim_dir) + os.pathsep + recovery_env["PATH"],
                 OMS_TEST_WATCH_PHASE="render", OMS_TEST_WATCH_SIGINT="ignored"),
        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        start_new_session=True, preexec_fn=ignored_sigint)
    try:
        assert select.select([watcher.stderr], [], [], 5)[0], "ignored watcher did not become ready"
        assert watcher.stderr.readline() == b"PHASE_BLOCKED\n"
        watcher.send_signal(signal.SIGINT)
        _, errors = watcher.communicate(input=b"x", timeout=2)
        assert watcher.returncode == 0, ("inherited SIG_IGN was overridden", errors)
        assert not errors, errors
    finally:
        if watcher.poll() is None:
            os.killpg(watcher.pid, signal.SIGKILL)
            watcher.wait(timeout=5)
    # SQLite lock waits defer Python's signal handler until the lock is released.
    # Keep that lock held through exit; readiness comes from SQLite's trace callback, not a delay.
    import select, sqlite3
    blocked_db = temporary / "panel-blocked-watch.sqlite"
    read_finished = temporary / "panel-blocked-watch-finished"
    blocked_script = """
import os, sqlite3, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import terminal_panel as panel
from panel_input import TerminalInput
with TerminalInput() as inputs:
    assert inputs.fd is None
def blocked_read(*args):
    with sqlite3.connect(sys.argv[3], timeout=60, isolation_level=None) as database:
        # SIGINT can arrive before a daemon's buffered stderr write releases its lock.
        database.set_trace_callback(lambda sql: os.write(2, b'READ_BLOCKED\\n'))
        try:
            database.execute('INSERT INTO held VALUES (1)')
        finally:
            Path(sys.argv[4]).touch()
    raise AssertionError('status read was released before watcher exit')
panel.read_panel = blocked_read
sys.exit(panel.main(['--repo', sys.argv[2], '--watch', '--no-animation']))
"""
    with sqlite3.connect(blocked_db) as database:
        database.execute("CREATE TABLE held (value)")
        database.execute("BEGIN IMMEDIATE")
        watcher = subprocess.Popen([sys.executable, "-c", blocked_script, str(root / "scripts/lib"),
                                    str(project), str(blocked_db), str(read_finished)],
                                   env=recovery_env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                   preexec_fn=default_sigint)
        try:
            assert select.select([watcher.stderr], [], [], 5)[0], "blocked watcher did not become ready"
            assert watcher.stderr.readline().strip() == "READ_BLOCKED", "status read did not reach its held lock"
            watcher.send_signal(signal.SIGINT)
            # The held read would block for its 60 s busy timeout; 10 s still proves
            # SIGINT did not wait for it while tolerating loaded CI runners.
            watcher_code = watcher.wait(timeout=10)
            watcher_tail = watcher.stderr.read() if watcher_code != 130 else ""
            assert watcher_code == 130, ("blocked status read delayed SIGINT", watcher_code, watcher_tail)
            assert not read_finished.exists(), "watcher waited for its blocked status read"
            output, errors = watcher.communicate(timeout=10)
            assert not output and not errors, (output, errors)
        finally:
            if watcher.poll() is None:
                watcher.kill()
                watcher.wait(timeout=5)
soak_seconds = int(os.environ.get("OMS_PANEL_SOAK_SECONDS", "0"))
if soak_seconds:
    assert os.name != "nt" and 30 <= soak_seconds <= 3600
    import time, signal
    watch_log = temporary / "panel-soak-watch.log"
    log_path = project / ".oms/threads" / (shared + ".jsonl")
    displaced = log_path.with_suffix(".soak-backup")
    started = time.monotonic()
    cycles = 0
    cursor = first_delta["cursor"]
    with watch_log.open("w") as output:
        watcher = subprocess.Popen(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                    "--room", shared, "--watch", "--no-animation"],
                                   env=recovery_env, stdout=output, stderr=subprocess.STDOUT)
        try:
            while time.monotonic() - started < soak_seconds:
                assert watcher.poll() is None, "watcher exited during sustained polling"
                mid = "soak-%s" % cycles
                with patch.dict(os.environ, recovery_env, clear=True):
                    room.send(project, shared, room_owner, member, "Controlled soak delivery", message_id=mid)
                    received = []
                    for _ in range(len(room.records(project, shared))):
                        delta = room.updates(project, shared, member, cursor)
                        cursor = delta["cursor"]
                        received.extend(row["room_event"]["id"] for row in delta["turns"])
                        if not delta["has_more"]:
                            break
                    assert mid in received
                    room.acknowledge(project, shared, member, [mid])
                    if cycles % 25 == 0:
                        try:
                            room.updates(project, shared, member, "invalid-cursor")
                        except ValueError:
                            pass
                        else:
                            raise AssertionError("invalid cursor was accepted")
                    if cycles % 50 == 0:
                        log_path.rename(displaced)
                        try:
                            time.sleep(7)
                            assert watcher.poll() is None
                        finally:
                            displaced.rename(log_path)
                cycles += 1
                time.sleep(5)
        finally:
            watcher.send_signal(signal.SIGINT)
            try:
                watcher.wait(timeout=45)
            except subprocess.TimeoutExpired:
                watcher.kill(); watcher.wait(timeout=5)
    assert watcher.returncode == 130
    observed = watch_log.read_text()
    assert "room unreadable:" in observed and "Messages:" in observed
    print("panel-soak: %s cycles / %.1fs / addressed delivery, explicit ack, invalid cursors and room-loss recovery" %
          (cycles, time.monotonic() - started), flush=True)
print("room: addressed handoff, graph bounds, saved receiver intents and native adapter passed")
print("terminal-panel: both directions, isolated patches, reviews, guards and PTY passed")
