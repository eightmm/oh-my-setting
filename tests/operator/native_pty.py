if os.name == "posix":
    # Raw single keys: no Enter, and no "oms> " prompt to wait for.
    terminal(["bash", str(panel.ENTRY), "panel", "--layout", "inline"], b"9q", "OMS · ".encode("utf-8"), trigger=b"Needs you")
    motion_attempt = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                           "--provider", "claude", "--tool", "panel-motion-fixture",
                           "--ref", "panel_role=worker", "--ref", "panel_model=claude-sonnet-5-5",
                           "--ref", "panel_label=Explain the active fixture task", "--then", "starting",
                           "--then", "working"]).strip()
    ledger = project / ".oms/lifecycle/events.jsonl"
    before_motion = ledger.read_bytes()
    # Two collections sit five seconds apart and each can take seconds under a parallel gate.
    terminal(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "2"], b"", b"\033[2K", timeout=60)
    assert ledger.read_bytes() == before_motion, "animation wrote lifecycle state"
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition",
          "--attempt", motion_attempt, "--state", "cancelled"])
    for owner in panel.PROVIDERS:
        terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", owner, "--resume", "exact-session-id",
                  "--task", "Explain fixture parser"],
                 b"", b"NATIVE_FIXTURE_READY")
    length_room = room.create(project, "native-label-length", "Native title length")
    for length in (120, 121, 160):
        terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", "codex", "--room", length_room,
                  "--resume", "label-session-id", "--task", "x" * length], b"", b"NATIVE_FIXTURE_READY")
    assert all(len(p["label"]) <= 120 for p in room.status(project, length_room)["participants"])
    private_task = "Explain " + "/" + "home" + "/fixture-private/input.py"
    terminal(["bash", str(panel.ENTRY), "panel", "--host", "inline", "--launch", "codex", "--resume", "exact-session-id",
              "--task", private_task], b"", b"NATIVE_FIXTURE_READY")
    launched = [json.loads(line) for line in log.read_text().splitlines()]
    assert {r["agent"] for r in launched} == set(panel.PROVIDERS)
    for length in (120, 121, 160):
        assert any(("x" * length) in " ".join(row["argv"]) for row in launched)
    for row in launched:
        assert row["cwd"] == row["repo"] == str(project.resolve())
        assert row["entry"] == str(panel.ENTRY)
        assert any(session in row["argv"] for session in ("exact-session-id", "label-session-id"))
        assert "peer-delegate" in " ".join(row["argv"])
    native_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    assert all(row["state"] == "done" for row in native_attempts
               if row["tool"] == "panel-main" and row["attempt_id"] not in main_attempts.values()), native_attempts
    titles = [row["refs"]["panel_label"] for row in native_attempts
              if row["tool"] == "panel-main" and row["attempt_id"] not in main_attempts.values()]
    assert titles.count("Explain fixture parser") == 2 and "Native task not recorded" in titles, titles
    assert private_task not in json.dumps(native_attempts), "private native task leaked into lifecycle metadata"
    assert private_task in " ".join(launched[-1]["argv"]), "native task was lost instead of hiding its title"
    from contextlib import ExitStack
    # Swapping away from Pi starts a fresh replacement, never an unbound Pi resume.
    # Exercise failure boundaries without a provider CLI or a real session file.
    for target, outcome in (("claude", "started"), ("codex", "started"), ("claude", "old_stays"),
                            ("claude", "startup_timeout"), ("claude", "launch_error")):
        pi_attempt = "att_" + "a" * 32
        next_attempt = "att_" + "b" * 32
        pi_main = {"attempt_id": pi_attempt, "tool": "panel-main", "provider": "codex",
                   "terminal": False, "state": "working", "refs": {
                       "panel_room_participant": "pi-owner", "panel_host": "tmux",
                       "panel_harness": "pi", "panel_model": "gpt-6.1-sol"}}
        ended_pi = dict(pi_main, terminal=True, state="abandoned", reason_code="main_swapped")
        next_main = {"attempt_id": next_attempt, "provider": target, "terminal": False,
                     "refs": {"panel_harness": target}}
        handoff_file = temporary / ("pi-swap-" + target + "-" + outcome + ".md")
        handoff_file.write_text("Pi handoff fixture\n", encoding="utf-8")
        clock = [0]
        def swap_tick():
            clock[0] += 10
            return clock[0]
        with ExitStack() as swap_patches:
            commands = swap_patches.enter_context(patch.object(panel.subprocess, "run",
                return_value=subprocess.CompletedProcess([], 0, '{"active":false}', "")))
            transfer = swap_patches.enter_context(patch.object(panel.room, "swap_main"))
            submit = swap_patches.enter_context(patch.object(panel, "_swap_submit"))
            launch = swap_patches.enter_context(patch.object(panel, "launch_command", return_value="fresh replacement",
                side_effect=ValueError("fixture launch failed") if outcome == "launch_error" else None))
            for context in (
                    patch.dict(os.environ, dict(environment, OMS_PANEL_SWAP_HANDOFF_SECONDS="0"), clear=True),
                    patch.object(panel.shutil, "which", return_value="fixture-tmux"),
                    patch.object(panel, "main_installed", return_value=True),
                    patch.object(panel, "resolve_session", return_value=("fixture", str(project))),
                    patch.object(panel, "_swap_main_window", return_value=(
                        "@1", pi_attempt, "%1", "pi-owner", "pi-room")),
                    patch.object(panel, "live_mains", return_value=1),
                    patch.object(panel, "main_history", side_effect=[
                        [pi_main]] + [[pi_main if outcome == "old_stays" else ended_pi, next_main]] * 20),
                    patch.object(panel.room, "status", return_value={}),
                    patch.object(panel.room, "participant", return_value={"role": "main", "provider": "codex"}),
                    patch.object(panel, "_swap_digest", return_value="Pi handoff fixture"),
                    patch.object(panel, "_swap_handoff_file", return_value=handoff_file),
                    patch.object(panel, "_main_window", return_value=(
                        "@1", next_attempt, "%1", str(project), "pi-owner"),
                        side_effect=ValueError("fixture ownership unknown") if outcome == "startup_timeout" else None),
                    patch.object(panel.time, "monotonic", side_effect=swap_tick),
                    patch.object(panel.time, "sleep")):
                swap_patches.enter_context(context)
            try:
                pi_swap = panel.swap_main(project, target)
            except ValueError as error:
                assert outcome != "started", error
                if outcome == "old_stays":
                    assert "old main did not exit" in str(error) and "will not be restored" not in str(error), error
                    transfer.assert_not_called()
                    launch.assert_not_called()
                    assert not handoff_file.exists(), "preserved main cannot retry its handoff"
                else:
                    assert "Pi main exited and will not be restored; no verified replacement main" in str(error), error
                    assert str(handoff_file) in str(error) and handoff_file.exists(), error
            else:
                assert outcome == "started" and pi_swap["participant"] == "pi-owner", pi_swap
                assert pi_swap["room"] == "pi-room" and pi_swap["handoff"] == "automatic_digest", pi_swap
            if outcome != "old_stays":
                transfer.assert_called_once_with(project, "pi-room", "pi-owner", target,
                                                 panel.policy()["main"][target]["model"], pi_attempt, target)
                launch.assert_called_once()
                assert launch.call_args.args == (project, "fixture", target)
                assert "resume" not in launch.call_args.kwargs, launch.call_args
                respawns = [call for call in commands.call_args_list if "if-shell" in call.args[0]]
                assert len(respawns) == (0 if outcome == "launch_error" else 1), "swap attempted restoration"
            assert [call.args[1] for call in submit.call_args_list][1:] == ["/quit"]
            assert "@oms_panel_swap_state" in commands.call_args.args[0], "swap status was not cleared"
    actual_tmux = shutil.which("tmux")
    if actual_tmux:
        socket = "o" + str(os.getpid())
        wrapper = binary / "tmux"
        wrapper.write_text("#!/usr/bin/env bash\nexec %s -L %s \"$@\"\n" %
                           (shlex.quote(actual_tmux), shlex.quote(socket)))
        wrapper.chmod(0o755)
        tmux = [actual_tmux, "-L", socket]
        # Keep the unrelated session alive through the native/reader fixtures; cleanup kills the private server.
        subprocess.run(tmux + ["new-session", "-d", "-s", "fixture-existing", "sleep 600"],
                       env=dict(environment, TERM="xterm"), check=True)
        # Mains are opened one by one from the menu here; automatic opening is checked separately.
        env = dict(environment, TERM="xterm", OMS_PANEL_NO_ANIMATION="1", NO_COLOR="",
                   OMS_PANEL_MAIN="")
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe("bash", ["bash", str(panel.ENTRY)], env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        finished = False
        client_output = bytearray()
        try:
            wait_scale = float(os.environ.get("OMS_TMUX_WAIT_SCALE", "1"))
            if wait_scale <= 0:
                raise AssertionError("OMS_TMUX_WAIT_SCALE must be positive")

            def tmux_wait_until(observe, description, timeout=30, interval=.15, stable_for=0):
                deadline = time.monotonic() + timeout * wait_scale
                stable_since = None
                last = None
                while time.monotonic() < deadline:
                    last, ready = observe()
                    now = time.monotonic()
                    if ready:
                        if stable_since is None:
                            stable_since = now
                        if now - stable_since >= stable_for * wait_scale:
                            return last
                    else:
                        stable_since = None
                    time.sleep(interval)
                raise AssertionError((description, last, bytes(client_output[-4000:])))

            def tmux_wait(arguments, expected, timeout=30, absent=()):
                def observe():
                    if select.select([fd], [], [], .1)[0]:
                        try:
                            client_output.extend(os.read(fd, 65536))
                        except OSError:
                            pass
                    result = subprocess.run(tmux + arguments, capture_output=True, text=True, env=env)
                    text = result.stdout
                    return text, result.returncode == 0 and expected in text and not any(needle in text for needle in absent)
                return tmux_wait_until(observe, (arguments, expected), timeout)

            sessions = tmux_wait(["list-sessions", "-F", "#{session_name}"], "oms-panel-")
            session = next(s for s in sessions.splitlines() if s.startswith("oms-panel-"))
            # Panel keys act only in OMS panel sessions; other sessions receive the key itself.
            tmux_wait(["list-keys", "-T", "root", "F9"], "select-pane")
            tmux_wait(["list-keys", "-T", "root", "F9"], "#{@oms_panel_repo}")
            tmux_wait(["list-keys", "-T", "root", "F5"], "select-window -t :=control")
            tmux_wait(["list-keys", "-T", "root", "F5"], "select-window -l")
            tmux_wait(["list-keys", "-T", "root", "F12"], "window_name")
            subprocess.run(tmux + ["set-environment", "-t", session, "PANEL_TEST_NATIVE_HOLD", "1"], env=env, check=True)
            panes = tmux_wait(["list-panes", "-t", session, "-F", "#{pane_id}\t#{pane_start_command}"], "--watch").splitlines()
            control = next(row.split("\t")[0] for row in panes if "--watch" not in row)
            unselected = tmux_wait(["capture-pane", "-p", "-t", control], "session room is unset")
            assert "START" not in unselected and "Press 1" not in unselected, unselected
            control_room = panel.room.create(project, title="Control fixture")
            subprocess.run(tmux + ["set-environment", "-t", session, "OMS_ROOM_ID", control_room], env=env, check=True)
            onboarded = tmux_wait(["capture-pane", "-p", "-t", control], "Press 1 (Claude)")
            # The start keys name only harnesses installed in this fixture.
            pi_ready = all(panel.shutil.which(name, path=env["PATH"]) for name in ("pi", "codex"))
            assert ("2 (Pi)" in onboarded) == pi_ready, onboarded
            control_options = {name: subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", control, name],
                                                             env=env, text=True).strip()
                               for name in ("@oms_panel_view", "@oms_panel_attention")}
            for key in ("v", "b"):
                subprocess.run(tmux + ["send-keys", "-t", control, key], env=env, check=True)
                tmux_wait(["capture-pane", "-p", "-t", control], "choose")
                for name, value in control_options.items():
                    assert subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", control, name],
                                                   env=env, text=True).strip() == value
            for owner, key in (("codex", "0"), ("claude", "1")):
                identity = "MAIN Sol 6.1" if owner == "codex" else "MAIN Opus 5.5"
                subprocess.run(tmux + ["send-keys", "-t", control, key], env=env, check=True)
                tmux_wait(["list-windows", "-t", session, "-F", "#{window_name}"], owner)
                # [native chat, board]: the board is the window's first (left) pane.
                listing = tmux_wait(["list-panes", "-t", session + ":" + owner,
                                     "-F", "#{pane_id}"], "\n%").splitlines()[::-1]
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], "NATIVE_FIXTURE_READY")
                occupied_control = tmux_wait(["capture-pane", "-p", "-t", control], "[o] Rooms",
                                             absent=("START", "Press 1"))
                assert "START" not in occupied_control and "Press 1" not in occupied_control, occupied_control
                assert subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[1],
                    "@oms_panel_view"], env=env, text=True).strip() == panel.board_view(control_options["@oms_panel_view"])
                assert subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[1],
                    "@oms_panel_attention"], env=env, text=True).strip() == control_options["@oms_panel_attention"]
                # Compact rendering and the board's b key retain their own coverage.
                subprocess.run(tmux + ["set-option", "-w", "-t", listing[1], "@oms_panel_view", "compact"], env=env, check=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "b"], env=env, check=True)
                tmux_wait(["show-option", "-w", "-v", "-t", listing[1], "@oms_panel_attention"], "1")
                # The option changes before the watcher paints; MAIN also
                # appears in the preceding frame, so wait for the new filter.
                sidebar = tmux_wait(["capture-pane", "-p", "-t", listing[1]], "attention")
                # Two-column identities have no room for the clipping ellipsis.
                assert any(label in sidebar for label in ("MAIN ", "MA…", "MA  [")) and "OMS · " in sidebar, sidebar
                native_header = tmux_wait(["show-option", "-p", "-v", "-t", listing[0], "@oms_panel_header"], identity)
                assert identity in native_header, native_header
                assert "activity" in sidebar.lower(), sidebar
                assert "attention" in sidebar, sidebar
                assert subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[1],
                    "@oms_panel_view"], env=env, text=True).strip() == "compact"
                pane_cmd = subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[1],
                                                           "#{pane_start_command}"], env=env, text=True)
                assert "OMS_PANEL_NO_ANIMATION=1" in pane_cmd and "NO_COLOR=" in pane_cmd, pane_cmd
                captured = subprocess.check_output(tmux + ["capture-pane", "-e", "-p", "-t", listing[1]],
                                                   env=env, text=True)
                assert "\033" not in captured, captured
                # Select tree in the watcher and click its own current main.
                # The existing native pane is focused, with no second launch.
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "t"], env=env, check=True)
                tmux_wait(["show-option", "-w", "-v", "-t", listing[1], "@oms_panel_view"], "tree")
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "Home"], env=env, check=True)
                tree_sidebar = tmux_wait(["capture-pane", "-p", "-t", listing[1]], "╭╌ " + identity)
                clicked_row = next(n + 1 for n, line in enumerate(tree_sidebar.splitlines()) if identity in line)
                subprocess.run(tmux + ["select-pane", "-t", listing[1]], env=env, check=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "\033[<0;8;%sM" % clicked_row], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", session + ":" + owner, "#{pane_id}"], listing[0])
                native_pid = subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0],
                    "#{pane_pid}"], env=env, text=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", "v"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "1")
                # The tree dock also labels its auto preview MAIN; wait for the graph card.
                enlarged = tmux_wait(["capture-pane", "-p", "-t", listing[1]], "╭╌ ▸ " + identity)
                main_row, main_text = next((n + 1, line) for n, line in enumerate(enlarged.splitlines()) if identity in line)
                main_col = main_text.index("MAIN ") + 2
                card_click = "\033[<0;%s;%sM" % (main_col, main_row)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", card_click], env=env, check=True)
                # Graph cards preview on every click and Enter, including the main.
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "-l", card_click], env=env, check=True)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "Enter"], env=env, check=True)
                def zoomed_and_focused():
                    value = subprocess.run(tmux + ["display-message", "-p", "-t", session + ":" + owner,
                        "#{window_zoomed_flag} #{pane_id}"], env=env, capture_output=True, text=True)
                    observed = value.stdout.strip() if value.returncode == 0 else value.stderr.strip()
                    return observed, value.returncode == 0 and observed == "1 " + listing[1]
                tmux_wait_until(zoomed_and_focused, "Enter keeps the graph zoomed and focused")
                assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[1],
                    "#{window_zoomed_flag}"], env=env, text=True).strip() == "1", \
                    "Enter did not leave the graph zoomed"
                assert subprocess.check_output(tmux + ["display-message", "-p", "-t", session + ":" + owner,
                    "#{pane_id}"], env=env, text=True).strip() == listing[1], \
                    "Enter left focus on another pane after graph selection"
                tmux_wait(["capture-pane", "-p", "-t", listing[1]], "╭╌ ▸ " + identity)
                subprocess.run(tmux + ["send-keys", "-t", listing[1], "Escape"], env=env, check=True)
                tmux_wait(["display-message", "-p", "-t", listing[1], "#{window_zoomed_flag}"], "0")
                tmux_wait(["show-option", "-w", "-v", "-t", listing[1], "@oms_panel_view"], "graph")
                assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0],
                    "#{pane_pid}"], env=env, text=True) == native_pid
                assert subprocess.check_output(tmux + ["show-option", "-g", "-v", "mouse"], env=env, text=True).strip() == "off"
                assert subprocess.check_output(tmux + ["show-option", "-t", session, "-v", "mouse"], env=env, text=True).strip() == "on"
                subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], owner + " session exited (0)")
                tmux_wait(["capture-pane", "-p", "-t", listing[0]], "r resume/restart here")
                if owner == "claude":
                    before_windows = subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                             env=env, text=True)
                    first_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[0],
                                                                   "@oms_panel_main_attempt"], env=env, text=True).strip()
                    first_main = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                                  "show", "--attempt", first_attempt, "--json"], env))
                    assert first_main["terminal"] and first_main["reason_code"] == "native_exit", first_main
                    native_id = first_main["refs"]["native_session"]
                    assert uuid.UUID(native_id).version == 4
                    first_launch = json.loads(log.read_text().splitlines()[-1])
                    assert first_launch["argv"][first_launch["argv"].index("--session-id") + 1] == native_id
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "-l", "r"], env=env, check=True)
                    deadline = time.monotonic() + 15
                    while True:
                        next_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", listing[0],
                                                                      "@oms_panel_main_attempt"], env=env, text=True).strip()
                        if next_attempt != first_attempt:
                            break
                        assert time.monotonic() < deadline, "restart did not replace the attempt"
                        time.sleep(.1)
                    tmux_wait(["display-message", "-p", "-t", listing[0], "#{@oms_panel_native_pane}"], listing[0])
                    next_main = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                                 "show", "--attempt", next_attempt, "--json"], env))
                    assert next_main["refs"]["native_session"] != native_id
                    assert next_main["refs"].get("panel_room_participant", next_main["attempt_id"]) == next_attempt
                    assert subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                   env=env, text=True) == before_windows, "restart added a window"
                    assert subprocess.check_output(tmux + ["display-message", "-p", "-t", listing[0], "#{pane_pid}"],
                                                   env=env, text=True) == native_pid, "restart replaced the pane"
                    deadline = time.monotonic() + 15
                    while True:
                        next_launch = json.loads(log.read_text().splitlines()[-1])
                        if next_launch != first_launch and "--session-id" in next_launch["argv"]:
                            break
                        assert time.monotonic() < deadline, "restart did not start a fresh native session"
                        time.sleep(.1)
                    assert "--resume" not in next_launch["argv"], next_launch
                    assert next_launch["argv"][next_launch["argv"].index("--session-id") + 1] == \
                        next_main["refs"]["native_session"]
                    # Once that session is stored on disk, the next restart resumes it instead.
                    stored = (Path(env["CLAUDE_CONFIG_DIR"]) / "projects" / "fixture" /
                              (next_main["refs"]["native_session"] + ".jsonl"))
                    stored.parent.mkdir(parents=True, exist_ok=True)
                    stored.write_text("{}\n")
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
                    # The screen still shows the previous exit prompt; wait for one after the latest session start.
                    deadline = time.monotonic() + 15
                    while True:
                        screen = subprocess.run(tmux + ["capture-pane", "-p", "-t", listing[0]], env=env,
                                                capture_output=True, text=True).stdout
                        if "r resume/restart here" in screen.rsplit("NATIVE_FIXTURE_READY", 1)[-1]:
                            break
                        assert time.monotonic() < deadline, ("fresh session did not exit", screen[-800:])
                        time.sleep(.1)
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "-l", "r"], env=env, check=True)
                    deadline = time.monotonic() + 15
                    while True:
                        resumed_launch = json.loads(log.read_text().splitlines()[-1])
                        if "--resume" in resumed_launch["argv"]:
                            break
                        assert time.monotonic() < deadline, ("restart did not resume the stored native session", subprocess.run(tmux + ["capture-pane", "-p", "-t", listing[0]], env=env, capture_output=True, text=True).stdout[-1500:])
                        time.sleep(.1)
                    assert resumed_launch["argv"][resumed_launch["argv"].index("--resume") + 1] == \
                        next_main["refs"]["native_session"], resumed_launch
                    subprocess.run(tmux + ["send-keys", "-t", listing[0], "Enter"], env=env, check=True)
                    tmux_wait(["display-message", "-p", "-t", listing[0], "[#{@oms_panel_native_pane}]"], "[]")
                # The control menu refuses replacements until it reads the terminal owner state.
                tmux_wait(["capture-pane", "-p", "-t", control], "START")
            # F5 reaches controls from a native window and returns to that window.
            subprocess.run(tmux + ["select-window", "-t", session + ":claude"], env=env, check=True)
            os.write(fd, b"\033[15~")
            tmux_wait(["display-message", "-p", "-t", session, "#{window_name}"], "control")
            os.write(fd, b"\033[15~")
            tmux_wait(["display-message", "-p", "-t", session, "#{window_name}"], "claude")
            # Board geometry is recorded per window: a drag affects its own main, and a zoomed sibling stays put.
            shared = {}
            for owner in ("codex", "claude"):
                rows = subprocess.check_output(tmux + ["list-panes", "-t", session + ":" + owner, "-F",
                                                       "#{pane_id}\t#{pane_start_command}"], env=env, text=True)
                shared[owner] = next(row.split("\t")[0] for row in rows.splitlines() if "--watch" in row)
            subprocess.run(tmux + ["select-window", "-t", session + ":codex"], env=env, check=True)
            def codex_geometry():
                value = subprocess.run(tmux + ["display-message", "-p", "-t", shared["codex"],
                    "#{window_active} #{window_width} #{window_height} #{pane_width}"],
                    env=env, capture_output=True, text=True)
                observed = value.stdout.strip() if value.returncode == 0 else value.stderr.strip()
                try:
                    active, window_width, unused_height, pane_width = map(int, observed.split())
                    ready = value.returncode == 0 and active == 1 and pane_width < window_width
                except ValueError:
                    ready = False
                return observed, ready
            tmux_wait_until(codex_geometry,
                "active codex board remains split before drag", stable_for=2.5)
            geometry = subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["codex"],
                "#{window_width} #{window_height} #{pane_width}"], env=env, text=True).split()
            window, extent = int(geometry[0]), "#{pane_width}"
            assert int(geometry[2]) < window, "codex board was not split before drag: %r" % geometry
            claude_initial = subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["claude"], extent],
                                                     env=env, text=True).strip()
            dragged = panel.split_cells(("left", window * 3 // 10, False), window)
            subprocess.run(tmux + ["resize-pane", "-t", shared["codex"], "-x", str(dragged)], env=env, check=True)
            tmux_wait(["show-options", "-w", "-v", "-t", shared["codex"], "@oms_panel_split"], panel.split_text(dragged))
            tmux_wait(["display-message", "-p", "-t", shared["claude"], extent], claude_initial, timeout=20)
            tmux_wait(["display-message", "-p", "-t", shared["codex"], extent], str(dragged), timeout=20)
            subprocess.run(tmux + ["resize-pane", "-t", shared["claude"], "-Z"], env=env, check=True)
            wider = panel.split_cells(("left", window * 7 // 10, False), window)
            subprocess.run(tmux + ["set-option", "-w", "-t", shared["claude"], "@oms_panel_split", panel.split_text(wider)],
                           env=env, check=True)
            def zoom_state():
                value = subprocess.run(tmux + ["display-message", "-p", "-t", shared["claude"],
                    "#{window_zoomed_flag}"], env=env, capture_output=True, text=True)
                observed = value.stdout.strip() if value.returncode == 0 else value.stderr.strip()
                return observed, value.returncode == 0 and observed == "1"
            tmux_wait_until(zoom_state,
                "zoomed claude board stays zoomed", stable_for=3)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", shared["claude"],
                "#{window_zoomed_flag}"], env=env, text=True).strip() == "1", \
                "a zoomed board was resized after its split option changed"
            subprocess.run(tmux + ["resize-pane", "-t", shared["claude"], "-Z"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", shared["claude"], extent],
                      str(min(panel.work_split_cells(("left", wider, False), window), window - 1 - panel.NATIVE_MIN)), timeout=20)
            control_sidebar = next(row.split("\t")[0] for row in panes if "--watch" in row)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "claude")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Claude --")
            subprocess.run(tmux + ["send-keys", "-t", control, "4"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", control], "Main harness (claude/pi/codex)")
            subprocess.run(tmux + ["send-keys", "-t", control, "codex", "Enter"], env=env, check=True)
            tmux_wait(["show-option", "-w", "-v", "-t", control, "@oms_panel_owner"], "codex")
            tmux_wait(["capture-pane", "-p", "-t", control_sidebar], "Codex --")
            records = [json.loads(line) for line in log.read_text().splitlines()]
            # Claude launched first, restarted fresh, then resumed its stored session.
            assert [r["agent"] for r in records[-4:]] == ["codex", "claude", "claude", "claude"]
            assert all(r["cwd"] == str(project.resolve()) for r in records[-4:])
            # Select the destination with a real attached client on another session.
            # Provenance rejection is covered by the catalog fixtures below.
            other = subprocess.check_output(tmux + ["new-session", "-d", "-s", "fixture-navigation",
                "-P", "-F", "#{session_id}\t#{window_id}\t#{pane_id}", "cat"], env=env, text=True).strip().split("\t")
            target = {"session": other[0], "window": other[1], "pane": other[2]}
            with patch.dict(os.environ, dict(env, TMUX_PANE=control), clear=True), \
                    patch("panel_chats.catalog", return_value={"rows": [{"participant": "fixture-destination",
                        "provider": "codex", "terminal": target, "uri": None, "status": "native"}]}):
                import panel_chats
                assert panel_chats.open_chat(project, "fixture-room", "fixture-destination")["status"] == "navigation_requested"
            assert subprocess.check_output(tmux + ["list-clients", "-F", "#{session_name}"], env=env, text=True).strip() == "fixture-navigation"
            subprocess.run(tmux + ["switch-client", "-t", session], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", control, "q"], env=env, check=True)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .1)[0]:
                    try:
                        os.read(fd, 65536)
                    except OSError:
                        pass
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
            assert finished, "detaching hung the launcher"
            assert subprocess.run(tmux + ["has-session", "-t", session], env=env).returncode == 0
            # One command opens a native chat below a full-width graph. The
            # watcher can zoom/collapse without replacing the native process.
            # The checkout keeps one panel session; a later launch adds a native window to it.
            assert session == panel.resolve_session(project, wait=False)[0], session
            assert subprocess.check_output(tmux + ["show-options", "-v", "-t", session, "@oms_panel_repo"],
                                           env=env, text=True).strip() == str(project.resolve())
            known_sessions = set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env, text=True).splitlines())
            known_windows = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                        env=env, text=True).splitlines())
            subprocess.run(tmux + ["set-environment", "-g", "PANEL_TEST_NATIVE_HOLD", "1"], env=env, check=True)
            os.close(fd)
            pid, fd = pty.fork()
            if pid == 0:
                os.chdir(project)
                os.execvpe("bash", ["bash", str(panel.ENTRY), "panel", "--host", "tmux",
                                    "--launch", "codex", "--task", "Native conversation fixture"],
                           dict(env, PANEL_TEST_NATIVE_HOLD="1"))
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 110, 0, 0))
            finished = False
            deadline = time.monotonic() + 20
            top_window = None
            while time.monotonic() < deadline:
                windows = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                      env=env, text=True).splitlines()) - known_windows
                if windows:
                    top_window = next(iter(windows))
                    break
                time.sleep(.1)
            assert top_window, "native chat board did not start"
            assert set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env,
                                               text=True).splitlines()) == known_sessions, "a second panel session opened"
            # Menu-opened and externally launched mains share the panel's recorded room.
            menu_room = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", session + ":codex",
                                                        "@oms_panel_room"], env=env, text=True).strip()
            # Window options are bound right after the window appears; wait for them instead of racing.
            assert menu_room, "menu-opened main recorded no room"
            tmux_wait(["show-option", "-w", "-v", "-t", top_window, "@oms_panel_room"], menu_room)
            pane_rows = tmux_wait(["list-panes", "-t", top_window, "-F",
                                   "#{pane_id}\t#{pane_left}\t#{pane_height}\t#{pane_start_command}"], "--watch").splitlines()
            chat_row = next(row.split("\t", 3) for row in pane_rows if "--launch" in row)
            graph_row = next(row.split("\t", 3) for row in pane_rows if "--watch" in row)
            chat_pane, graph_pane = chat_row[0], graph_row[0]
            assert int(graph_row[1]) < int(chat_row[1]) and graph_row[2] == chat_row[2], pane_rows
            tmux_wait(["capture-pane", "-p", "-t", chat_pane], "NATIVE_FIXTURE_READY")
            board_width = int(subprocess.check_output(tmux + ["display-message", "-p", "-t", graph_pane,
                                                              "#{pane_width}"], env=env, text=True))
            assert board_width < 76, board_width
            board_picture = tmux_wait(["capture-pane", "-p", "-t", graph_pane], "MAIN ")
            assert "Activity" in board_picture and "OMS · " in board_picture, board_picture
            tmux_wait(["show-option", "-p", "-v", "-t", chat_pane, "@oms_panel_header"], "MAIN Sol 6.1")
            board_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", top_window,
                                                            "@oms_panel_main_attempt"], env=env, text=True).strip()
            board_main = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                                          "show", "--attempt", board_attempt, "--json"], env))
            assert board_main["refs"]["panel_model"] == "gpt-6.1-sol", board_main
            native_pid = subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", top_window, "#{pane_id}"], env=env, text=True).strip() == chat_pane
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "v"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "1")
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "0")
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            # After the person unzooms by hand, Esc must not zoom the board again.
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "v"], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "1")
            subprocess.run(tmux + ["resize-pane", "-Z", "-t", graph_pane], env=env, check=True)
            tmux_wait(["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"], "0")
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            def graph_zoom_state():
                value = subprocess.run(tmux + ["display-message", "-p", "-t", graph_pane,
                    "#{window_zoomed_flag}"], env=env, capture_output=True, text=True)
                observed = value.stdout.strip() if value.returncode == 0 else value.stderr.strip()
                return observed, value.returncode == 0 and observed == "0"
            tmux_wait_until(graph_zoom_state,
                "Esc leaves the unzoomed board unzoomed", stable_for=.8)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", graph_pane, "#{window_zoomed_flag}"],
                                           env=env, text=True).strip() == "0", "Esc re-zoomed an unzoomed board"
            # F9 (the attached client's xterm F9) swaps focus between chat and board and keeps both alive;
            # q on a managed board does nothing. If a pane is explicitly killed, restore only the board
            # and keep the CLI PID.
            subprocess.run(tmux + ["select-pane", "-t", graph_pane], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "-l", "q"], env=env, check=True)
            def focused_pane():
                value = subprocess.run(tmux + ["display-message", "-p", "-t", top_window,
                    "#{pane_id}"], env=env, capture_output=True, text=True)
                observed = value.stdout.strip() if value.returncode == 0 else value.stderr.strip()
                return observed, value.returncode == 0 and observed == graph_pane
            tmux_wait_until(focused_pane, "q leaves focus on the board", stable_for=.5)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", top_window, "#{pane_id}"],
                                           env=env, text=True).strip() == graph_pane, "q moved focus"
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], chat_pane)
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], graph_pane)
            os.write(fd, b"\033[20~")
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], chat_pane)
            subprocess.run(tmux + ["kill-pane", "-t", graph_pane], env=env, check=True)
            reopen_env = dict(env, TMUX="fixture", TMUX_PANE=chat_pane, OMS_PANEL_SESSION=session)
            restore = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                       "--reopen"], reopen_env))
            assert restore["status"] == "reopened" and restore["native_restarted"] is False, restore
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            restored = tmux_wait(["list-panes", "-t", top_window, "-F", "#{pane_id}\t#{pane_start_command}"], "--watch")
            graph_pane = next(row.split("\t", 1)[0] for row in restored.splitlines() if "--watch" in row)
            tmux_wait(["capture-pane", "-p", "-t", graph_pane], "MAIN ")
            tmux_wait(["show-option", "-p", "-v", "-t", graph_pane, "@oms_panel_main_watch"], "1")
            restored_attempt = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", graph_pane,
                                                "@oms_panel_main_attempt"], env=env, text=True).strip()
            restored_owner = subprocess.check_output(tmux + ["show-option", "-w", "-v", "-t", graph_pane,
                                              "@oms_panel_room_participant"], env=env, text=True).strip()
            chat_watch = subprocess.run(tmux + ["show-option", "-p", "-v", "-t", chat_pane,
                                                "@oms_panel_main_watch"], env=env, capture_output=True, text=True)
            assert chat_watch.returncode != 0 or chat_watch.stdout.strip() != "1", \
                "reopen marked the native chat pane as the main board"
            with patch.dict(os.environ, dict(reopen_env, TMUX_PANE=graph_pane,
                                             OMS_ROOM_PARTICIPANT=restored_owner), clear=True):
                assert panel._cancel_watch_authority(project, restored_attempt, restored_owner), \
                    "reopened main board did not restore cancellation authority"
            assert int(subprocess.check_output(tmux + ["display-message", "-p", "-t", graph_pane,
                                                      "#{pane_width}"], env=env, text=True)) == board_width
            restored_picture = tmux_wait(["capture-pane", "-p", "-t", graph_pane], "MAIN ")
            assert "Activity" in restored_picture and "OMS · " in restored_picture, restored_picture
            with patch.dict(os.environ, reopen_env, clear=True), patch.object(panel.sys.stdin, "isatty", return_value=True), \
                    patch.object(panel.sys.stdout, "isatty", return_value=True), \
                    patch.object(panel, "reopen_panel", wraps=panel.reopen_panel) as reopen_again:
                assert panel.main(["--repo", str(project)]) == 0
                assert reopen_again.called
            assert len(subprocess.check_output(tmux + ["list-panes", "-t", top_window], env=env, text=True).splitlines()) == 2
            # A call opens in a reader pane right of the board, above the chat (the tree's click/Enter choice is unit
            # tested with panel_reader.picked); another call reuses that reader, --reopen ignores it, Esc on the board
            # closes only it, and q in the reader closes it and returns focus to the board. The native chat process
            # is never touched.
            def window_rows():
                return [row.split("\t") for row in subprocess.check_output(tmux + ["list-panes", "-t", top_window, "-F",
                        "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{@oms_panel_reader}"], env=env, text=True).splitlines()]
            def wait_panes(count, timeout=15):
                deadline = time.monotonic() + timeout
                while len(window_rows()) != count:
                    assert time.monotonic() < deadline, window_rows()
                    time.sleep(.1)
                return window_rows()
            with patch.dict(os.environ, reopen_env, clear=True):
                opened = panel.panel_reader.open_reader(graph_pane, panel.reader_shell(
                    project, ("result", "fixture-worker"), graph_pane), project)
            rows = wait_panes(3)
            reader = next(row[0] for row in rows if row[3] == graph_pane)
            assert opened == reader, (opened, rows)
            chat_at = next(row for row in rows if row[0] == chat_pane)
            reader_at = next(row for row in rows if row[0] == reader)
            assert reader_at[1] == chat_at[1] and int(reader_at[2]) < int(chat_at[2]), rows
            assert next(row for row in rows if row[0] == graph_pane)[1] == "0", rows
            # The reused reader's previous view already has the close footer.
            tmux_wait(["capture-pane", "-p", "-t", reader], "WORKER model unknown")
            tmux_wait(["capture-pane", "-p", "-t", reader], "q/Esc close")
            with patch.dict(os.environ, reopen_env, clear=True):
                assert panel.panel_reader.open_reader(graph_pane, panel.reader_shell(
                    project, ("debate", ("fixture-task", "fixture-seat")), graph_pane), project) == reader
            assert len(wait_panes(3)) == 3
            assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--reopen"],
                                   reopen_env))["status"] == "already_open"
            subprocess.run(tmux + ["send-keys", "-t", graph_pane, "Escape"], env=env, check=True)
            assert reader not in [row[0] for row in wait_panes(2)]
            with patch.dict(os.environ, reopen_env, clear=True):
                reader = panel.panel_reader.open_reader(graph_pane, panel.reader_shell(project, ("result", "fixture-worker"),
                                                                                 graph_pane), project)
            tmux_wait(["capture-pane", "-p", "-t", reader], "q/Esc close")
            subprocess.run(tmux + ["select-pane", "-t", reader], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", reader, "-l", "q"], env=env, check=True)
            assert reader not in [row[0] for row in wait_panes(2)]
            tmux_wait(["display-message", "-p", "-t", top_window, "#{pane_id}"], graph_pane)
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", chat_pane, "#{pane_pid}"], env=env, text=True) == native_pid
            # After the first main ends, a replacement starts without a terminal in the same panel and room,
            # no new session, no focus change.
            starter_id = next(p["participant"] for p in room.status(project, menu_room)["participants"]
                              if p["role"] == "main" and p["provider"] == "codex" and p["joined"])
            windows_before = set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                         env=env, text=True).splitlines())
            current_before = subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                                     env=env, text=True).strip()
            spawn_env = dict(env, OMS_ROOM_ID=menu_room, OMS_ROOM_PARTICIPANT=starter_id, OMS_PANEL_SESSION="")
            refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude", "--json"],
                                     env=spawn_env, capture_output=True, text=True, timeout=15)
            assert refused.returncode != 0 and "already has a live main" in refused.stderr, refused.stderr
            assert set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                               env=env, text=True).splitlines()) == windows_before
            # Direct inline launch cannot bypass the process-lifetime lease held by the native fixture.
            bypass = subprocess.run([sys.executable, "-c",
                "import sys; from pathlib import Path; sys.path.insert(0, sys.argv[1]); import terminal_panel as p; p.run_native('claude', Path(sys.argv[2]))",
                str(root / "scripts/lib"), str(project)], env=spawn_env, capture_output=True, text=True, timeout=15)
            assert bypass.returncode != 0 and "panel operation is busy" in bypass.stderr, bypass.stderr
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "-l", "hello native chat"], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "Enter"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-J", "-t", chat_pane], "NATIVE_CHAT_REPLY:hello native chat")
            tmux_wait(["capture-pane", "-p", "-t", chat_pane], "codex session exited (0)")
            spawned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--spawn-main", "claude",
                                       "--task", "Spawned fixture", "--json"], spawn_env))
            assert spawned["provider"] == "claude" and spawned["room"] == menu_room and isinstance(spawned["window"], int), spawned
            new_window = next(iter(set(subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                               env=env, text=True).splitlines()) - windows_before))
            assert subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{window_id}"],
                                           env=env, text=True).strip() == current_before, "spawning moved focus"
            assert set(subprocess.check_output(tmux + ["list-sessions", "-F", "#{session_name}"], env=env,
                                               text=True).splitlines()) == known_sessions, "spawning opened a session"
            tmux_wait(["show-option", "-w", "-v", "-t", new_window, "@oms_panel_room"], menu_room)
            tmux_wait(["list-panes", "-t", new_window, "-F", "#{pane_id}"], "\n%")
            watched_env = dict(env, TMUX="fixture", OMS_PANEL_SESSION=session, OMS_ROOM_ID=menu_room,
                               COLUMNS="140", LINES="44")
            board_text = ""
            for unused in range(50):
                board_text = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", menu_room,
                                   "--watch", "--count", "1", "--view", "graph"], watched_env)
                if "MAIN " in board_text and "[+ Claude main]" not in board_text and "[+ Codex main]" not in board_text:
                    break
                time.sleep(.2)
            # Occupied boards omit start actions while B's retired-main controls still work.
            assert "[+ Claude main]" not in board_text and "[+ Codex main]" not in board_text, board_text
            assert "One main active or unverified" not in board_text and board_text.splitlines()[-1].endswith("F12 Keys"), board_text
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "-l", "r"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-J", "-t", chat_pane], "another main is live or unverified")
            # Enter closes the retired main's entire window and its board watcher.
            subprocess.run(tmux + ["send-keys", "-t", chat_pane, "Enter"], env=env, check=True)
            deadline = time.monotonic() + 15
            while top_window in subprocess.check_output(tmux + ["list-windows", "-t", session, "-F", "#{window_id}"],
                                                        env=env, text=True).splitlines():
                assert time.monotonic() < deadline, "Enter left the old board window running"
                time.sleep(.1)
            # The next main takes the same native pane and room participant. A plan claim and
            # worker parent remain anchored to that participant while the provider changes.
            previous_pane = tmux_wait(["show-option", "-w", "-v", "-t", new_window,
                                       "@oms_panel_native_pane"], "%").strip()
            subprocess.run(tmux + ["send-keys", "-t", previous_pane, "-l", "finish"], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", previous_pane, "Enter"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", previous_pane], "claude session exited (0)")
            no_main = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                      "--swap-main", "codex", "--json"], env=env, capture_output=True, text=True)
            assert no_main.returncode != 0 and "requires exactly one live main" in no_main.stderr, no_main.stderr
            swap_spawn = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                          "--spawn-main", "claude", "--json"], env))
            swap_window = subprocess.check_output(tmux + ["display-message", "-p", "-t",
                                                   session + ":" + str(swap_spawn["window"]), "#{window_id}"],
                                                  env=env, text=True).strip()
            swap_pane = tmux_wait(["show-option", "-w", "-v", "-t", swap_window,
                                   "@oms_panel_native_pane"], "%").strip()
            tmux_wait(["capture-pane", "-p", "-t", swap_pane], "NATIVE_FIXTURE_READY")
            swap_member = tmux_wait(["show-option", "-w", "-v", "-t", swap_window,
                                     "@oms_panel_room_participant"], "att_").strip()
            swap_attempt = tmux_wait(["show-option", "-w", "-v", "-t", swap_window,
                                      "@oms_panel_main_attempt"], "att_").strip()
            assert swap_spawn["room"] == menu_room
            room.join(project, menu_room, "swap-worker", "codex", "worker", parent=swap_member)
            worker_attempt = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                                   "--provider", "codex", "--tool", "panel-worker", "--parent-attempt-id", swap_attempt,
                                   "--ref", "panel_role=worker", "--ref", "panel_room_id=" + menu_room,
                                   "--ref", "panel_room_participant=swap-worker", "--then", "starting",
                                   "--then", "working"], env).strip()
            plan_cli = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(project)]
            call(plan_cli + ["init", "--goal", "Swap fixture"], env)
            call(plan_cli + ["add", "--id", "swap-held", "--title", "Keep the claim"], env)
            claim_env = dict(env, OMS_ROOM_PARTICIPANT=swap_member, OMS_PANEL_MAIN_ATTEMPT=swap_attempt)
            call(plan_cli + ["claim", "--id", "swap-held", "--provider", "claude"], claim_env)
            claim_before = json.loads(call(plan_cli + ["show", "--id", "swap-held"], env))
            # The already-running Claude fixture will answer; only the next Codex fixture
            # inherits this setting and forces the bounded automatic-digest path.
            subprocess.run(tmux + ["set-environment", "-g", "PANEL_TEST_SWAP_HANDOFF", "0"], env=env, check=True)
            unknown_swap = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                           "--swap-main", "missing"], env=env, capture_output=True, text=True)
            assert unknown_swap.returncode != 0 and "unknown or not installed" in unknown_swap.stderr
            # Pi may exist on the host PATH; model its absence explicitly.
            uninstalled_error = io.StringIO()
            with patch.dict(os.environ, env, clear=True), \
                    patch.object(panel, "main_installed", return_value=False), \
                    patch.object(panel.sys, "stderr", uninstalled_error):
                uninstalled_status = panel.main(["--repo", str(project), "--swap-main", "pi"])
            assert uninstalled_status != 0 and "unknown or not installed" in uninstalled_error.getvalue()
            with patch.object(panel, "main_installed", return_value=True), \
                    patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                    ["land", "status"], 3, '{"active":true}', "")) as land_probe:
                try:
                    panel.swap_main(project, "codex")
                except ValueError as error:
                    assert "land is in progress" in str(error), error
                else:
                    raise AssertionError("main swap passed an active landing")
                assert land_probe.call_count == 1, "land refusal touched a native pane"
            for state, expected in (("running", "land is in progress"), ("passed", "probe passed")):
                with patch.object(panel, "main_installed", return_value=True), \
                        patch.object(panel, "resolve_session", side_effect=ValueError("probe passed")), \
                        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                        ["land", "status"], 4, json.dumps({"active": None, "state": state}), "")):
                    try:
                        panel.swap_main(project, "codex")
                    except ValueError as error:
                        assert expected in str(error), (state, error)
                    else:
                        raise AssertionError("main swap ignored an unknown land lock")
            swapped = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                       "--swap-main", "codex", "--model", "gpt-6.1-sol", "--json"],
                                      dict(env, OMS_PANEL_SWAP_HANDOFF_SECONDS="15")))
            assert swapped["handoff"] == "main" and swapped["window"] == swap_window, swapped
            assert swapped["room"] == menu_room and swapped["participant"] == swap_member, swapped
            handoff_path = project / ".oms" / "handoffs" / ("room-%s-swap-%s.md" % (menu_room, swap_attempt))
            assert "Source: main" in handoff_path.read_text() and "Goal fixture" in handoff_path.read_text()
            released = json.loads(panel.events(project, "show", "--attempt", swap_attempt, "--json", output=True))
            assert released["state"] == "abandoned" and released["reason_code"] == "main_swapped"
            assert released["refs"].get("native_session"), "old native session id was discarded"
            tmux_wait(["capture-pane", "-p", "-t", swap_pane], "NATIVE_FIXTURE_READY")
            assert room.participant(room.status(project, menu_room), swap_member)["provider"] == "codex"
            assert room.participant(room.status(project, menu_room), "swap-worker")["parent"] == swap_member
            room.send(project, menu_room, "swap-worker", swap_member, "worker result after swap", "note")
            assert swap_member in room.status(project, menu_room)["messages"][-1]["targets"]
            assert json.loads(call(plan_cli + ["show", "--id", "swap-held"], env))["lease_id"] == claim_before["lease_id"]
            assert json.loads(call(plan_cli + ["show", "--id", "swap-held"], env))["claimed_by_participant"] == swap_member
            assert panel.events(project, "show", "--attempt", worker_attempt, "--json", output=True)
            codex_attempt = tmux_wait(["show-option", "-w", "-v", "-t", swap_window,
                                       "@oms_panel_main_attempt"], "att_").strip()
            assert swap_attempt in panel.lineage_ids(project, "codex", swap_member, codex_attempt)
            swapped_main = json.loads(panel.events(project, "show", "--attempt", codex_attempt,
                                                    "--json", output=True))
            assert swapped_main["refs"]["panel_model"] == "gpt-6.1-sol", swapped_main
            swap_board = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", menu_room,
                               "--watch", "--count", "1", "--view", "graph"], watched_env)
            assert "MAIN Sol 6.1 · took over from Opus 5.5" in swap_board, swap_board
            assert "[s] Swap main" in "\n".join(panel.menu_rows(100, 28, occupied=True))
            fallback = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                        "--swap-main", "claude", "--json"],
                                       dict(env, OMS_PANEL_SWAP_HANDOFF_SECONDS="1")))
            assert fallback["handoff"] == "automatic_digest" and fallback["window"] == swap_window, fallback
            fallback_path = project / ".oms" / "handoffs" / ("room-%s-swap-%s.md" % (menu_room, codex_attempt))
            assert "Source: automatic_digest" in fallback_path.read_text()
            assert "swap-held" in fallback_path.read_text() and worker_attempt in fallback_path.read_text()
            tmux_wait(["capture-pane", "-p", "-t", swap_pane], "NATIVE_FIXTURE_READY")
            assert room.participant(room.status(project, menu_room), swap_member)["provider"] == "claude"
            assert json.loads(call(plan_cli + ["show", "--id", "swap-held"], env))["lease_id"] == claim_before["lease_id"]
            fallback_prompt = " ".join(json.loads(log.read_text().splitlines()[-1])["argv"])
            assert all(part in fallback_prompt for part in ("Automatic OMS handoff", "swap-held", worker_attempt)), fallback_prompt
            resumed_attempt = tmux_wait(["show-option", "-w", "-v", "-t", swap_window,
                                         "@oms_panel_main_attempt"], "att_").strip()
            resumed_session = json.loads(panel.events(project, "show", "--attempt", resumed_attempt,
                                                       "--json", output=True))["refs"]["native_session"]
            subprocess.run(tmux + ["send-keys", "-t", swap_pane, "-l", "finish"], env=env, check=True)
            subprocess.run(tmux + ["send-keys", "-t", swap_pane, "Enter"], env=env, check=True)
            tmux_wait(["capture-pane", "-p", "-t", swap_pane], "claude session exited (0)")
            with patch.object(panel, "_run_native", return_value="resume accepted"):
                assert panel.run_single_native("claude", project, resume=resumed_session) == "resume accepted"
            continued_room = room.rollover(project, menu_room, actor=swap_member)
            assert continued_room != menu_room
            carried_swap = panel._latest_swap(project, room.status(project, continued_room))
            assert carried_swap["attempt"] == codex_attempt and carried_swap["to"] == "claude"
            continued_board = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", continued_room,
                                    "--watch", "--count", "1", "--view", "graph"], watched_env)
            assert "MAIN Opus 5.5 · took over from Sol 6.1" in continued_board, continued_board
            subprocess.run(tmux + ["detach-client", "-s", session], env=env, check=True)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .1)[0]:
                    try:
                        os.read(fd, 65536)
                    except OSError:
                        pass
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == 0
                    break
            assert finished, "native chat detach hung"
        finally:
            subprocess.run(tmux + ["kill-server"], env=env, capture_output=True)
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)
