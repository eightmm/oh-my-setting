# Scratch repository selection shares state without moving native execution.
import room
import room_repository as repo_paths
shared_root = temporary / "shared root 'resolution'"
subprocess.run(["git", "clone", "--quiet", "--no-local", str(project), str(shared_root)], check=True)
registry = temporary / "resolution-registry"
scratch_parent = registry / "oh-my-setting-scratch.resolution"
scratch_parent.mkdir(parents=True)
working_root = scratch_parent / "wt"
subprocess.run(["git", "-C", str(shared_root), "worktree", "add", "--quiet", "--detach", str(working_root)], check=True)
working_child = working_root / "nested"
working_child.mkdir()
proof = scratch_parent / ".oh-my-setting-tmp"
proof_fields = {"kind": "oh-my-setting-temp", "temporary": "1", "pid": "99999999",
                "repo": str(shared_root), "worktree": str(working_root)}
proof_bytes = "".join(key + "=" + value + "\n" for key, value in proof_fields.items()).encode()
proof.write_bytes(proof_bytes)
resolver_env = dict(environment, OMS_DELEGATE_WORKTREE_ROOT=str(registry))
shared_id = json.loads(call(["bash", str(panel.ENTRY), "room", "new", "--repo", str(shared_root), "--json"], resolver_env))["id"]
call(["bash", str(panel.ENTRY), "room", "join", "--repo", str(shared_root), "--id", shared_id,
      "--participant", "resolution-owner", "--native-session", "resolution-native-consumer", "--json"], resolver_env)
selected_env = dict(resolver_env, OMS_ROOM_ID=shared_id, OMS_ROOM_REPO=str(shared_root),
                    OMS_ROOM_PARTICIPANT="resolution-owner")
consumer_before = room.participant(room.status(shared_root, shared_id), "resolution-owner")
for requested in (working_root, working_child):
    shown = json.loads(call(["bash", str(panel.ENTRY), "room", "show", "--repo", str(requested), "--json"], selected_env))
    assert shown["id"] == shared_id, shown
    board = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(requested), "--json"], selected_env))
    assert board["dashboard"]["room"]["id"] == shared_id, board
    with patch.dict(os.environ, resolver_env, clear=True):
        assert panel.repository(requested) == shared_root
        assert repo_paths.work_repository(requested) == working_root
for front in ("room", "panel"):
    default_command = ["bash", str(panel.ENTRY), front] + (["show"] if front == "room" else []) + ["--json"]
    default_view = subprocess.run(default_command, cwd=working_child, env=selected_env,
                                  capture_output=True, text=True, timeout=30)
    assert default_view.returncode == 0, default_view
    default_data = json.loads(default_view.stdout)
    assert (default_data if front == "room" else default_data["dashboard"]["room"])["id"] == shared_id

ordinary_root = registry / "ordinary"
subprocess.run(["git", "-C", str(shared_root), "worktree", "add", "--quiet", "--detach", str(ordinary_root)], check=True)
plain_directory = temporary / "resolution-non-git"
plain_directory.mkdir()
panel_front = ["bash", str(panel.ENTRY), "panel"]
missing_repo_message = "oms panel: %s is not a git repository; open it in a git repository or pass --repo PATH" % plain_directory
for options in ([], ["--watch", "--count", "1"], ["--launch", "codex", "--dry-run"],
                ["--spawn-main", "codex", "--json"], ["--reopen"]):
    refused = subprocess.run(panel_front + options, cwd=plain_directory, env=resolver_env,
                             capture_output=True, text=True, timeout=30)
    assert refused.returncode == 3 and missing_repo_message in refused.stderr, refused
assert not list(plain_directory.iterdir())
for options in (["--providers", "--json"], ["--routes"], ["--help"]):
    informational = subprocess.run(panel_front + options, cwd=plain_directory, env=selected_env,
                                   capture_output=True, text=True, timeout=30)
    assert informational.returncode == 0, informational
    wrapped = subprocess.run(["bash", str(root / "scripts/panel.sh")] + options, cwd=plain_directory,
                             env=selected_env, capture_output=True, text=True, timeout=30)
    assert wrapped.returncode == 0, wrapped
gone_repo = temporary / "resolution-missing-directory"
for options in (["--dry-run"], ["--watch", "--count", "1"]):
    vanished = subprocess.run(panel_front + ["--repo", str(gone_repo)] + options, cwd=plain_directory,
                              env=resolver_env, capture_output=True, text=True, timeout=30)
    assert vanished.returncode == 3 and "Traceback" not in vanished.stderr and \
        "is not a usable git repository" in vanished.stderr, vanished
with patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired(["git"], 5)), \
        patch.object(panel.sys, "stderr", io.StringIO()) as timed_err:
    assert panel.main(["--repo", str(plain_directory), "--dry-run"]) == 3
assert "TimeoutExpired" in timed_err.getvalue() and "Traceback" not in timed_err.getvalue(), timed_err.getvalue()
explicit = subprocess.run(panel_front + ["--repo", str(plain_directory), "--dry-run"],
                          cwd=plain_directory, env=resolver_env, capture_output=True, text=True, timeout=30)
assert explicit.returncode == 3 and missing_repo_message in explicit.stderr, explicit
wrapper = subprocess.run(["bash", str(root / "scripts/panel.sh")], cwd=plain_directory,
                         env=resolver_env, capture_output=True, text=True, timeout=30)
assert wrapper.returncode == 3 and missing_repo_message in wrapper.stderr, wrapper
equals_explicit = subprocess.run(panel_front + ["--repo=" + str(plain_directory), "--dry-run"],
                                 cwd=ordinary_root, env=resolver_env, capture_output=True,
                                 text=True, timeout=30)
assert equals_explicit.returncode == 3 and missing_repo_message in equals_explicit.stderr, equals_explicit
repeated_repo = subprocess.run(panel_front + ["--repo", str(plain_directory), "--repo", str(ordinary_root), "--dry-run"],
                               cwd=ordinary_root, env=resolver_env, capture_output=True,
                               text=True, timeout=30)
assert repeated_repo.returncode == 0 and json.loads(repeated_repo.stdout)["repo"] == str(ordinary_root), repeated_repo
assert not list(plain_directory.iterdir()), list(plain_directory.iterdir())
ordinary_child = ordinary_root / "nested"
ordinary_child.mkdir()
ordinary_alias = temporary / "resolution-ordinary-link"
ordinary_alias.symlink_to(ordinary_child, target_is_directory=True)
for directory in (ordinary_child, ordinary_alias):
    implicit = subprocess.run(panel_front + ["--dry-run"], cwd=directory, env=resolver_env,
                              capture_output=True, text=True, timeout=30)
    assert implicit.returncode == 0 and json.loads(implicit.stdout)["repo"] == str(ordinary_root), implicit
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
        [], 0, str(ordinary_alias / "..") + "\r\n", "")), \
        patch.object(panel, "state_repository", return_value=ordinary_root) as state_resolution:
    assert panel.repository(None) == ordinary_root
    assert state_resolution.call_args.args == (ordinary_root,)
with patch.dict(os.environ, resolver_env, clear=True):
    assert panel.repository(ordinary_root) == ordinary_root
    try:
        panel.panel_repository(plain_directory)
    except ValueError as error:
        assert str(error) == "%s is not a git repository; open it in a git repository or pass --repo PATH" % plain_directory
    else:
        raise AssertionError("panel accepted a non-repository explicit path")
    non_git_scratch = registry / "oh-my-setting-scratch.non-git" / "wt"
    non_git_scratch.mkdir(parents=True)
    (non_git_scratch.parent / ".oh-my-setting-tmp").write_bytes(proof_bytes)
    assert panel.repository(non_git_scratch) == non_git_scratch
    for delegation in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "2"}):
        with patch.dict(os.environ, delegation):
            assert panel.repository(working_child) == working_root
    # The dead original PID never invalidates a registered parent scratch.
    assert panel.repository(working_root) == shared_root
    # A copied Git pointer with the same common directory is not registration.
    orphan_parent = registry / "oh-my-setting-scratch.unregistered"
    orphan_worktree = orphan_parent / "wt"
    orphan_worktree.mkdir(parents=True)
    (orphan_worktree / ".git").write_bytes((working_root / ".git").read_bytes())
    orphan_proof = dict(proof_fields, worktree=str(orphan_worktree))
    (orphan_parent / ".oh-my-setting-tmp").write_text("".join(key + "=" + value + "\n" for key, value in orphan_proof.items()))
    assert panel.repository(orphan_worktree) == orphan_worktree
    for field, invalid_value in (("repo", str(project)), ("worktree", str(ordinary_root)),
                                 ("worktree", str(registry / "missing" / "wt")), ("temporary", "0"),
                                 ("kind", "foreign-temp")):
        invalid = dict(proof_fields, **{field: invalid_value})
        proof.write_text("".join(key + "=" + value + "\n" for key, value in invalid.items()))
        assert panel.repository(working_root) == working_root, (field, invalid_value)
    for invalid in (proof_bytes + b"repo=conflict\n", b"x" * (repo_paths.MARKER_LIMIT + 1), b"kind=oh-my-setting-temp"):
        proof.write_bytes(invalid)
        assert panel.repository(working_root) == working_root
    proof.write_bytes(proof_bytes.replace(b"\n", b"\r\n"))
    assert panel.repository(working_root) == shared_root
    proof.unlink()
    proof_source = scratch_parent / "proof-source"
    proof_source.write_bytes(proof_bytes)
    proof.symlink_to(proof_source)
    assert panel.repository(working_root) == working_root
    proof.unlink()
    os.link(proof_source, proof)
    assert panel.repository(working_root) == working_root
    proof.unlink()
    if hasattr(os, "mkfifo"):
        os.mkfifo(proof)
        assert panel.repository(working_root) == working_root
        proof.unlink()
    proof.write_bytes(proof_bytes)
    alias_registry = temporary / "resolution-registry-link"
    alias_registry.symlink_to(registry, target_is_directory=True)
    with patch.dict(os.environ, {"OMS_DELEGATE_WORKTREE_ROOT": str(alias_registry)}):
        assert panel.repository(working_root) == working_root

foreign_id = json.loads(call(["bash", str(panel.ENTRY), "room", "new", "--repo", str(project), "--json"], resolver_env))["id"]
foreign_state = project / ".oms"
before = {str(p.relative_to(foreign_state)): (p.read_bytes() if p.is_file() else None)
          for p in foreign_state.rglob("*")}
room_front = ["bash", str(panel.ENTRY), "room"]
for option in (["--repo", str(project)], ["--repo=" + str(project)]):
    listed = json.loads(call(room_front + ["list"] + option + ["--json"], selected_env))
    assert foreign_id in [item["id"] for item in listed["rooms"]], listed
    shown = json.loads(call(room_front + ["show"] + option + ["--id", foreign_id, "--json"], selected_env))
    assert shown["id"] == foreign_id, shown
    for action in (["show"], ["show", "--id", ""], ["show", "--id", "../bad"]):
        refused = subprocess.run(room_front + action + option, env=selected_env, capture_output=True, text=True, timeout=30)
        assert refused.returncode, refused
    refused = subprocess.run(room_front + ["send"] + option + ["--id", foreign_id, "--text", "hello"],
                             env=selected_env, capture_output=True, text=True, timeout=30)
    assert refused.returncode and "selected repository conflicts with the inherited room repository" in refused.stderr, refused
after = {str(p.relative_to(foreign_state)): (p.read_bytes() if p.is_file() else None)
         for p in foreign_state.rglob("*")}
assert after == before, (before.keys(), after.keys())
conflict = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--json"]
refused = subprocess.run(conflict, env=selected_env, capture_output=True, text=True, timeout=30)
assert refused.returncode and "conflicts" in refused.stderr, refused
for restricted in (["updates", "--participant", "not-enrolled"],
                   ["join", "--participant", "resolution-orphan", "--role", "worker", "--parent", "not-enrolled"]):
    refused = subprocess.run(["bash", str(panel.ENTRY), "room"] + restricted +
                             ["--repo", str(working_root), "--id", shared_id, "--json"], env=resolver_env,
                             capture_output=True, text=True, timeout=30)
    assert refused.returncode, refused

execution_log = temporary / "resolution-native.jsonl"
call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(shared_root), "start",
      "--provider", "codex", "--tool", "panel-main", "--ref", "panel_role=main",
      "--ref", "panel_room_id=" + shared_id, "--ref", "panel_room_participant=resolution-owner",
      "--then", "starting", "--then", "cancelled"], resolver_env)
with patch.dict(os.environ, dict(selected_env, PANEL_TEST_LOG=str(execution_log)), clear=True), \
        patch.object(panel.sys.stdin, "isatty", return_value=True), \
        patch.object(panel.sys.stdout, "isatty", return_value=True):
    assert panel.main(["--repo", str(working_child), "--host", "inline", "--layout", "inline", "--launch", "codex"]) == 0
native_observed = json.loads(execution_log.read_text().splitlines()[-1])
assert native_observed["cwd"] == str(working_root) and native_observed["repo"] == str(shared_root), native_observed
assert room.participant(room.status(shared_root, shared_id), "resolution-owner") == consumer_before

assert not (working_root / ".oms").exists()
with patch.dict(os.environ, dict(selected_env, **{repo_paths.EXECUTION_ENV: str(working_root)}), clear=True):
    # Carry execution context into a canonical --repo child without changing
    # tmux's existing state ownership and command identity checks.
    assert repo_paths.EXECUTION_ENV + "=" + str(working_root) in shlex.split(panel.launch_command(shared_root, "oms-resolution", "codex"))
    with patch.object(panel.subprocess, "check_output", return_value="%resolution\n") as created, \
            patch.object(panel, "bind_window"), patch.object(panel, "board_split"):
        panel.add_main_window(shared_root, "oms-resolution", "codex", "auto", False, os.environ.copy())
        command = created.call_args.args[0]
        assert command[command.index("-c") + 1] == str(working_root), command
    import panel_native as native_seam  # ownership resolves an adapter main before launch
    with patch.object(panel, "native_binary", return_value="resolution-adapter"), \
            patch.object(native_seam, "native_adapter", return_value="resolution-adapter"):
        adapter = panel.native_command("lumen", shared_root)
        assert adapter[adapter.index("--workdir") + 1] == str(working_root), adapter
    with patch.object(panel.panel_host, "herdr_active", return_value=True), \
            patch.object(panel.panel_host, "open_native") as host_launch:
        panel.open_native("codex", shared_root)
        directory, provider, native_args, watcher_args = host_launch.call_args.args
        assert directory == working_root and provider == "codex"
        assert all(args[args.index("--repo") + 1] == str(working_root) for args in (native_args, watcher_args))
    with patch.dict(os.environ, {repo_paths.EXECUTION_ENV: str(ordinary_root)}):
        try:
            repo_paths.native_repository(shared_root)
        except ValueError:
            pass
        else:
            raise AssertionError("ordinary linked worktree inherited canonical execution authority")
room.command(shared_root, "close", "--id", shared_id)
for closed_command in (["panel", "--room", shared_id],
                       ["room", "join", "--participant", "resolution-owner"]):
    refused = subprocess.run(["bash", str(panel.ENTRY)] + closed_command + ["--repo", str(working_root), "--json"],
                             env=selected_env, capture_output=True, text=True, timeout=30)
    assert refused.returncode and "closed" in refused.stderr, refused
closed_read = json.loads(call(["bash", str(panel.ENTRY), "room", "updates", "--participant", "resolution-owner",
                               "--repo", str(working_root), "--json"], selected_env))
assert not closed_read["turns"], "closed-room reads remain available"
print("PASS: native scratch-state resolver and execution directory boundaries")
