if os.name == "posix":
    import fcntl
    import pty
    import select
    import struct
    import termios
    import time

    def terminal(command, inputs, expected, trigger=None, signal_after=None, expected_exit=0, timeout=20, then=None):
        # then=(marker, bytes): sent once marker is drawn after the first inputs, so a later click meets its hit map.
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execvpe(command[0], command, dict(environment, TERM="xterm", OMS_AGENT="codex"))
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 28, 110, 0, 0))
        initial_tty = termios.tcgetattr(fd) if signal_after is not None or expected_exit in (129, 143) else None
        pending = inputs.splitlines(keepends=True)
        prompts = sent_at = 0
        output = b""
        deadline = time.monotonic() + timeout
        finished = False
        try:
            while time.monotonic() < deadline:
                if select.select([fd], [], [], .2)[0]:
                    try:
                        output += os.read(fd, 65536)
                    except OSError:
                        pass
                seen = output.count(b"oms> ")
                if pending and trigger is not None and trigger in output and not prompts:
                    os.write(fd, b"".join(pending))
                    pending = []
                    prompts = 1
                    sent_at = len(output)
                elif then and prompts and then[0] in output[sent_at:]:
                    os.write(fd, then[1])
                    then = None
                elif pending and seen > prompts:
                    os.write(fd, pending.pop(0))
                    prompts = seen
                if signal_after is not None and trigger in output and not prompts:
                    os.kill(pid, signal_after)
                    prompts = 1
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    finished = True
                    assert os.waitstatus_to_exitcode(status) == expected_exit, output
                    break
            if not finished:
                waited, status = os.waitpid(pid, os.WNOHANG)
                finished = bool(waited)
                assert finished, output
                assert os.waitstatus_to_exitcode(status) == expected_exit, output
            for unused in range(4):
                if not select.select([fd], [], [], 0)[0]:
                    break
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
            assert expected in output, output
            if initial_tty is not None:
                assert termios.tcgetattr(fd) == initial_tty, "watch shutdown did not restore the tty"
        finally:
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
            os.close(fd)
