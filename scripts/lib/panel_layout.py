"""Terminal layout, tmux bindings and source reload presentation."""

from pathlib import Path
import hashlib
import os
import panel_reader
import re
import room
import shlex
import subprocess
import sys
import time

# Call-local facade imports preserve the public entry point and avoid import cycles.


class ReloadWatcher(Exception):
    """Raised by a watcher after its OMS sources changed and compile; main re-executes it."""


def tmux_command(*args):
    return ["tmux"] + list(args)


def shell_command(command):
    # tmux shell-command is a shell string, unlike the native subprocess argv.
    return "exec " + " ".join(shlex.quote(str(arg)) for arg in command)


def panel_shell(repo, session, view="auto", attention_only=False):
    from terminal_panel import ROOT, panel_environment, shell_command
    return shell_command(panel_environment() + ["OMS_PANEL_SESSION=" + session,
                          "bash", str(ROOT / "scripts" / "panel.sh"),
                          "--repo", str(repo), "--layout", "inline", "--view", view] +
                         (["--attention-only"] if attention_only else []))


def watch_shell(repo, provider=None, session=None):
    from terminal_panel import ENTRY, main_owner, panel_environment, shell_command
    command = panel_environment() + (["OMS_PANEL_SESSION=" + session] if session else []) + ["bash", str(ENTRY), "panel", "--repo", str(repo), "--watch"]
    if provider:
        command += ["--owner", main_owner(provider)]
    return shell_command(command)


def panel_environment():
    from terminal_panel import EXECUTION_ENV
    return ["env"] + [name + "=" + os.environ[name] for name in
                      ("OMS_PANEL_NO_ANIMATION", "OMS_PANEL_COLOR", "NO_COLOR", "OMS_CODEX_NOTIFY", "OMS_CLAUDE_NOTIFY",
                       "CLAUDE_CONFIG_DIR",
                       "OMS_ROOM_ID", "OMS_ROOM_REPO", "OMS_PANEL_HOST", "OMS_PANEL_VIEW",
                       "OMS_PANEL_THEME", "COLORFGBG", EXECUTION_ENV) if name in os.environ]


def board_view(view):
    return "graph" if view == "auto" else view


def reader_shell(repo, target, board, room_id=None):
    """The reader pane's read-only command for `board`'s window; every argument is quoted."""
    from terminal_panel import ENTRY, panel_environment, shell_command
    arguments = panel_reader.target_args(target)
    if target[0] == "mail":
        arguments += ["--room", room.identifier(room_id, "room")]
    return shell_command(panel_environment() + [panel_reader.BOARD + "=" + board, "bash", str(ENTRY), "panel",
                                                "--repo", str(repo)] + arguments)


BOARD_SHARE = 28  # percent of the window width a first landscape board takes
BOARD_HEIGHT_SHARE = 45
OVERVIEW_SHARE = 62
MIN_BOARD = MIN_CHAT = 20
WORK_CHAT_RESERVE = 120
PANEL_MODES = ("work", "overview", "focus")


def parse_split(text):
    """A window board axis as (orientation, cells, legacy-percent flag)."""
    match = re.fullmatch(r"(left):(\d{1,4})(c?)|(top):(\d{1,4})r", (text or "").strip())
    if not match:
        return None
    if match.group(4):
        number = int(match.group(5))
        return ("top", number, False) if number else None
    number = int(match.group(2))
    return ("left", number, False) if match.group(3) and number else \
        ("left", number, True) if not match.group(3) and 5 <= number <= 95 else None


def parse_work_drag(text):
    match = re.fullmatch(r"(left|top):(\d{1,4})c?@(\d{1,4})", (text or "").strip())
    return (match.group(1), int(match.group(2)), int(match.group(3))) if match else None


def split_text(cells):
    return "left:%dc" % cells


def top_text(rows):
    return "top:%dr" % rows


def split_cells(stored, window):
    """Board cells on a `window`-cell axis for a parsed geometry, leaving the chat pane (and the border) at least
    MIN_CHAT cells and the board at least MIN_BOARD; a window too small for both splits in half."""
    from terminal_panel import MIN_BOARD, MIN_CHAT
    cells = (2 * stored[1] * window + 100) // 200 if stored[2] else stored[1]
    low, high = MIN_BOARD, window - MIN_CHAT - 1
    return max(low, min(high, cells)) if high >= low else max(1, window // 2)


NATIVE_MIN = 40  # a native CLI prompt below this wraps every word (42-cell dogfood: Claude Code got 21)


def board_split_cells(stored, window, explicit=False):
    """Board cells, or None when both panes cannot fit: landscape needs NATIVE_MIN CLI columns,
    portrait needs MIN_CHAT CLI rows. Explicit sizes keep the existing chat floor; defaults reserve the CLI minimum."""
    from terminal_panel import MIN_BOARD, MIN_CHAT, NATIVE_MIN, split_cells
    minimum = MIN_CHAT if stored[0] == "top" else NATIVE_MIN
    if window < MIN_BOARD + 1 + minimum:
        return None
    cells = split_cells(stored, window)
    return cells if explicit else min(cells, window - 1 - minimum)


def portrait_window(window, current=None):
    """True when the board belongs above the CLI for a (columns, rows) window. A window narrower than the landscape
    minimum is always portrait. A tall one is too: cells are about twice as tall as wide, so rows * 2 > columns is
    physically taller than wide. The aspect rule has a +/-10 percent band (enter at rows * 2 >= 1.1 * columns, leave
    at rows * 2 <= 0.9 * columns) so a near-square window keeps its `current` edge instead of flipping on every
    resize; with no current edge the enter threshold decides. It needs the rows a portrait split requires."""
    columns, rows = window
    # The narrow rule keeps its 4-column release band, applied by the caller via `current`.
    if columns < MIN_BOARD + 1 + NATIVE_MIN + (4 if current == "top" else 0):
        return True
    if rows < MIN_BOARD + 1 + MIN_CHAT:
        return False
    return rows * 20 > columns * 9 if current == "top" else rows * 20 >= columns * 11


def work_split_cells(stored, window):
    """Apply the Work-mode board cap while preserving the narrow-window split contract."""
    from terminal_panel import MIN_BOARD, WORK_CHAT_RESERVE, split_cells
    cells = split_cells(stored, window)
    if stored[0] == "top" or window < MIN_BOARD + WORK_CHAT_RESERVE + 1:
        return cells
    # tmux keeps a one-cell divider between the board and the CLI pane.
    cap = min((38 * window + 50) // 100, window - WORK_CHAT_RESERVE - 1)
    return max(MIN_BOARD, min(cells, cap))


def next_panel_mode(mode):
    from terminal_panel import PANEL_MODES
    return PANEL_MODES[(PANEL_MODES.index(mode) + 1) % len(PANEL_MODES)] if mode in PANEL_MODES else "overview"


def cycle_panel_mode(pane):
    """Cycle this window's layout without changing its Work split record."""
    from terminal_panel import next_panel_mode, tmux_command
    try:
        found = subprocess.run(tmux_command("display-message", "-p", "-t", pane, "#{@oms_panel_mode}"),
                               capture_output=True, text=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
        current = found.stdout.replace("\r", "").strip() if found.returncode == 0 else "work"
        mode = next_panel_mode(current)
        subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_mode", mode),
                       check=False, timeout=2, stdin=subprocess.DEVNULL)
        return mode
    except (OSError, subprocess.SubprocessError):
        return None


def split_resize(stored, pane, window):
    """Cells a board of `pane` cells in a `window`-cell axis should take to match the stored geometry, or None
    when it is already within one cell (a 2-cell gap is the threshold that keeps windows from ping-ponging)."""
    from terminal_panel import split_cells
    wanted = split_cells(stored, window)
    return wanted if abs(wanted - pane) >= 2 else None


def terminal_window():
    """The launching terminal as the window an attaching client will get (one row goes to the tmux status line)."""
    for fd in (1, 0, 2):
        try:
            columns, rows = os.get_terminal_size(fd)
            return columns, rows - 1
        except (OSError, ValueError):
            pass
    return None


def board_split(pane, repo, command, main_watch=False):
    """Open this window's board on the left when possible, otherwise above the CLI.
    Leave narrow windows unsplit so both the board and native CLI remain usable."""
    from terminal_panel import BOARD_HEIGHT_SHARE, BOARD_SHARE, NATIVE_MIN, OVERVIEW_SHARE, PANEL_MODES
    from terminal_panel import board_split_cells, parse_split, parse_work_drag, split_text, terminal_window
    from terminal_panel import tmux_command, top_text, work_split_cells
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "-t", pane,
                                           "#{window_width} #{window_height} #{session_attached} #{session_id}"
                                           "\t#{window_name}\t#{@oms_panel_split}\t#{@oms_panel_mode}"
                                           "\t#{@oms_panel_work_drag}"),
                              capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        if result.returncode:
            raise ValueError("window geometry unavailable")
        parts = (result.stdout.replace("\r", "").rstrip("\n").split("\t") + ["", "", "", "", ""])[:5]
        fields = parts[0].split()
        window, attached, session = (int(fields[0]), int(fields[1])), int(fields[2]) > 0, fields[3]
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        window, attached, session, parts = None, True, "", ["", "", "", "", ""]
    stored = parse_split(parts[2])
    mode = parts[3] if parts[3] in PANEL_MODES else "work"
    # A detached session still has tmux's 80x24 window; the terminal that is about to attach is the better size.
    sized = window if attached else terminal_window()
    share = BOARD_SHARE
    configured = os.environ.get("OMS_PANEL_BOARD_PERCENT", "")
    if re.fullmatch(r"[+-]?[0-9]+", configured):
        try:
            share = max(25, min(35, int(configured)))
        except ValueError:
            pass
    available = sized or window
    left_fits = bool(available and board_split_cells(("left", BOARD_SHARE, True), available[0]) is not None)
    top_fits = bool(available and board_split_cells(("top", BOARD_HEIGHT_SHARE, True), available[1]) is not None)
    if not available:
        # Unknown geometry keeps the recorded edge, else the left default.
        orientation = stored[0] if stored else "left"
    else:
        current = stored[0] if stored else None
        if portrait_window(available, current) and top_fits:
            orientation = "top"
        elif left_fits:
            orientation = "left"
        else:
            orientation = "top" if top_fits else "left"
    if mode == "overview" and orientation == "left":
        share = OVERVIEW_SHARE
    width = "%d%%" % share
    if available:
        axis = available[1] if orientation == "top" else available[0]
        default = BOARD_HEIGHT_SHARE if orientation == "top" else (BOARD_SHARE if mode == "overview" else share)
        geometry = stored if stored and stored[0] == orientation else (orientation, default, True)
        drag = parse_work_drag(parts[4])
        deliberate = bool(drag and stored and drag[0] == stored[0] and drag[1] == stored[1]
                          and drag[2] == axis)
        base_cells = board_split_cells(geometry, axis, explicit=stored is not None)
        if base_cells is None:
            return
        cells = (board_split_cells((orientation, (OVERVIEW_SHARE * axis + 50) // 100, False), axis)
                 if mode == "overview" and orientation == "left" else
                 base_cells if mode == "overview" or deliberate else work_split_cells((orientation, base_cells, False), axis))
        if orientation == "left":
            # Saved widths remain preferences; constrain only the pane applied at this window size.
            cells = min(cells, axis - 1 - NATIVE_MIN)
        record_cells = work_split_cells((orientation, base_cells, False), axis)
        percent = max(1, min(99, (200 * cells + axis) // (2 * axis)))
        width = str(cells) if sized and attached else "%d%%" % percent
        if sized and session and parts[1] != "control" and (stored is None or stored[2]):
            value = top_text(record_cells) if orientation == "top" else split_text(record_cells)
            subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_split", value),
                           check=False, timeout=3, stdin=subprocess.DEVNULL)
    # -f keeps the board spanning the full window axis beside a reader pane.
    direction = "-v" if orientation == "top" else "-h"
    split = tmux_command("split-window", "-f", direction, "-b", "-l", width, "-t", pane, "-c", str(repo), command)
    if main_watch:
        split[2:2] = ["-P", "-F", "#{pane_id}"]
    result = subprocess.run(split, check=True, capture_output=main_watch, text=main_watch)
    watch_pane = result.stdout.replace("\r", "").strip() if main_watch else ""
    if main_watch and re.fullmatch(r"%[0-9]+", watch_pane):
        try:
            subprocess.run(tmux_command("set-option", "-p", "-t", watch_pane,
                                        "@oms_panel_main_watch", "1"), check=True, timeout=3,
                           stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError):
            pass  # Cancellation fails closed if the watcher pane cannot be marked.


def panel_geometry():
    """One tmux read of this board: (window active, zoomed, panes, window w/h, pane w/h, stored split text);
    None when tmux cannot say or this is the control window (only main windows share the split)."""
    from terminal_panel import PANEL_MODES, tmux_command
    try:
        result = subprocess.run(tmux_command("display-message", "-p", "-t", os.environ.get("TMUX_PANE", ""),
                                             "#{window_active} #{window_zoomed_flag} #{window_panes} #{window_width} "
                                             "#{window_height} #{pane_width} #{pane_height} #{pane-border-status}\t#{window_name}\t#{@oms_panel_split}"
                                             "\t#{@oms_panel_mode}\t#{@oms_panel_native_pane}"
                                             "\t#{@oms_panel_work_drag}"),
                                capture_output=True, text=True, check=False, timeout=2, stdin=subprocess.DEVNULL)
        # Tabs keep a window name with spaces (an attention mark "! ") from shifting the split text.
        parts = result.stdout.replace("\r", "").rstrip("\n").split("\t") if result.returncode == 0 else []
        fields = parts[0].split() if len(parts) >= 2 else []
        if len(fields) < 7 or parts[1] == "control" or fields[0] not in ("0", "1") or fields[1] not in ("0", "1"):
            return None
        mode = parts[3].strip() if len(parts) > 3 and parts[3].strip() in PANEL_MODES else "work"
        native = parts[4].strip() if len(parts) > 4 else ""
        drag = parts[5].strip() if len(parts) > 5 else ""
        # tmux excludes the header row from pane_height but includes it in the layout's window_height.
        header_rows = int(len(fields) > 7 and fields[7] in ("top", "bottom"))
        return (fields[0] == "1", fields[1] == "1", int(fields[2]), (int(fields[3]), int(fields[4])),
                (int(fields[5]), int(fields[6]) + header_rows), parts[2].strip() if len(parts) > 2 else "", mode, native, drag)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


class PanelModeSync:
    """Apply window mode effects independently from Work split synchronization."""

    def __init__(self):
        self.mode = None

    def tick(self, geometry):
        from terminal_panel import OVERVIEW_SHARE, split_cells, tmux_command
        if geometry is None:
            return None
        active, zoomed, panes, window, pane, text = geometry[:6]
        mode, native = (geometry[6:8] + ("work", ""))[:2] if len(geometry) > 6 else ("work", "")
        previous, self.mode = self.mode, mode
        board = os.environ.get("TMUX_PANE", "")
        if zoomed:
            if native and ((mode == "focus" and previous != "focus") or
                           (mode != "focus" and previous == "focus")):
                subprocess.run(tmux_command("resize-pane", "-Z", "-t", native), check=False, timeout=3,
                               stdin=subprocess.DEVNULL)
                return "unzoom"
            return None
        if mode == "focus" and native:
            subprocess.run(tmux_command("resize-pane", "-Z", "-t", native), check=False, timeout=3,
                           stdin=subprocess.DEVNULL)
            return "focus"
        landscape_board = panes >= 2 and pane[1] == window[1] and pane[0] < window[0]
        if mode == "overview" and landscape_board and window[1] * 2 <= window[0] + 4:
            target = split_cells(("left", (OVERVIEW_SHARE * window[0] + 50) // 100, False), window[0])
            if abs(target - pane[0]) >= 2:
                subprocess.run(tmux_command("resize-pane", "-t", board, "-x", str(target)), check=False,
                               timeout=3, stdin=subprocess.DEVNULL)
                return "overview"
        return None


class SplitSync:
    """Keep this recorded main window's board on the portrait or landscape edge and size."""

    def __init__(self):
        # (window, board cells, active, orientation) of the previous tick; None after this watcher's own resize, so its
        # result is never read back as a drag.
        self.last = None
        self.mode = None
        self.manual = None
        self.work_geometry = None

    def tick(self, geometry):
        """Apply one geometry read; return the name of what was done ("record", "follow") or None."""
        from terminal_panel import BOARD_HEIGHT_SHARE, BOARD_SHARE, MIN_BOARD, NATIVE_MIN, PANEL_MODES
        from terminal_panel import WORK_CHAT_RESERVE, parse_split, parse_work_drag, split_cells, tmux_command
        from terminal_panel import work_split_cells
        if geometry is None:
            return None
        active, zoomed, panes, window, pane, text = geometry[:6]
        mode = geometry[6] if len(geometry) > 6 and geometry[6] in PANEL_MODES else "work"
        # Focus can zoom the CLI pane; leave the pending Work transition for
        # the first unzoomed geometry so its recorded proportion is restored.
        if zoomed:
            return None
        drag = parse_work_drag(geometry[8] if len(geometry) > 8 else "")
        returning_to_work = self.mode is not None and self.mode != "work" and mode == "work"
        before, self.last = self.last, None
        if self.mode != mode:
            if mode == "work" or self.mode is None:
                before = None
            self.mode = mode
        board = os.environ.get("TMUX_PANE", "")
        stored = parse_split(text)
        if panes < 2:
            return None
        # The board fills the perpendicular dimension; this also identifies its current orientation
        # when the stored preference has just become stale after a client resize.
        current = "top" if pane[0] == window[0] and pane[1] < window[1] else \
            "left" if pane[1] == window[1] and pane[0] < window[0] else None
        if current is None:
            return None
        orientation = "top" if portrait_window(window, current) else "left"
        if self.manual is not None and (self.manual[0] != window or self.manual[1] != orientation):
            self.manual = None
        old_axis = window[1] if current == "top" else window[0]
        axis = window[1] if orientation == "top" else window[0]
        current_extent = pane[1] if current == "top" else pane[0]
        self.last = (window, current_extent, active, current)

        def record(cells):
            from terminal_panel import split_text, tmux_command, top_text
            if mode != "work":
                return False
            value = top_text(cells) if orientation == "top" else split_text(cells)
            self.work_geometry = (window, orientation, (orientation, cells, False))
            if value == text:
                return False
            subprocess.run(tmux_command("set-option", "-w", "-t", board, "@oms_panel_split", value),
                           check=False, timeout=3, stdin=subprocess.DEVNULL)
            return True

        marked_drag = bool(mode == "work" and drag and stored and drag[:2] == stored[:2] and drag[0] == orientation)
        if marked_drag and drag[2] != axis and drag[2] > 0:
            scaled = (drag[1] * axis + drag[2] // 2) // drag[2]
            record(scaled)
            drag_value = "%s:%d@%d" % (orientation, scaled, axis)
            subprocess.run(tmux_command("set-option", "-w", "-t", board, "@oms_panel_work_drag", drag_value),
                           check=False, timeout=3, stdin=subprocess.DEVNULL)
            stored = (orientation, scaled, False)
            drag = (orientation, scaled, axis)
            marked_drag = True
        if marked_drag:
            self.manual = (window, orientation, axis)
        elif mode == "work" and drag:
            subprocess.run(tmux_command("set-option", "-wu", "-t", board, "@oms_panel_work_drag"),
                           check=False, timeout=3, stdin=subprocess.DEVNULL)

        if stored is None:
            return None
        legacy = stored[2]
        if legacy:
            # Resolve legacy percentages before orientation or size normalization.
            stored = (orientation, split_cells(stored, axis), False)
        native_width_clamped = orientation == "left" and stored[1] > axis - 1 - NATIVE_MIN
        returned_record = False
        if returning_to_work and self.work_geometry is not None:
            work_window, work_orientation, work_stored = self.work_geometry
            work_axis = work_window[1] if work_orientation == "top" else work_window[0]
            if work_axis > 0 and (work_axis != axis or work_orientation != orientation):
                cells = (work_stored[1] * axis + work_axis // 2) // work_axis
                cells = split_cells((orientation, cells, False), axis) if marked_drag else \
                    work_split_cells((orientation, cells, False), axis)
                stored = (orientation, cells, False)
                returned_record = record(cells)
        if current != orientation:
            # Carry the user's selected fraction across the axis change.
            if stored[0] == current:
                source_axis = (before[0][1] if current == "top" else before[0][0]) \
                    if before is not None and before[3] == current else old_axis
                cells = split_cells((orientation, (stored[1] * axis + source_axis // 2) // source_axis, False), axis)
            else:
                cells = split_cells((orientation, stored[1], False), axis)
            if mode == "work":
                cells = work_split_cells((orientation, cells, False), axis)
                record(cells)
            panes_result = subprocess.run(tmux_command("list-panes", "-t", board,
                                                       "-F", "#{pane_id} #{pane_width} #{pane_height}"),
                                         capture_output=True, text=True, check=False, timeout=3,
                                         stdin=subprocess.DEVNULL)
            candidates = []
            for line in panes_result.stdout.replace("\r", "").splitlines():
                fields = line.split()
                if len(fields) == 3 and fields[0] != os.environ.get("TMUX_PANE", ""):
                    try:
                        candidates.append((int(fields[1]) * int(fields[2]), fields[0]))
                    except ValueError:
                        pass
            if candidates:
                destination = max(candidates)[1]
                direction = "-v" if orientation == "top" else "-h"
                subprocess.run(tmux_command("join-pane", "-d", "-f", "-s", board, "-t", destination,
                                            "-b", direction, "-l", str(cells)), check=False, timeout=3,
                               stdin=subprocess.DEVNULL)
                self.last = None
                return "follow"
            return "record"
        if mode == "work" and stored[0] != orientation:
            source_axis = (before[0][1] if before[3] == "top" else before[0][0]) \
                if before is not None and before[3] != orientation else 0
            if source_axis > 0:
                cells = (before[1] * axis + source_axis // 2) // source_axis
            else:
                share = BOARD_HEIGHT_SHARE if orientation == "top" else BOARD_SHARE
                cells = (share * axis + 50) // 100
            cells = work_split_cells((orientation, cells, False), axis)
            stored = (orientation, cells, False)
            record(cells)
        if mode != "work":
            return None
        done = None
        if mode == "work" and active and before is not None and before[2] and before[0] == window and before[3] == orientation \
                and before[1] != current_extent:
            current_cells = split_cells((orientation, current_extent, False), axis)
            record(current_cells)
            drag_value = "%s:%d@%d" % (orientation, current_cells, axis)
            subprocess.run(tmux_command("set-option", "-w", "-t", board, "@oms_panel_work_drag", drag_value),
                           check=False, timeout=3, stdin=subprocess.DEVNULL)
            self.manual = (window, orientation, axis)
            return "record"
        previous_axis = (before[0][1] if orientation == "top" else before[0][0]) if before is not None else axis
        if before is not None and previous_axis != axis and before[3] == orientation and mode == "work" \
                and not marked_drag and not legacy:
            cells = split_cells((orientation, (stored[1] * axis + previous_axis // 2) // previous_axis, False), axis)
            cells = work_split_cells((orientation, cells, False), axis)
            stored = (orientation, cells, False)
            if record(cells):
                done = "record"
            self.manual = None
            if drag:
                subprocess.run(tmux_command("set-option", "-wu", "-t", board, "@oms_panel_work_drag"),
                               check=False, timeout=3, stdin=subprocess.DEVNULL)
        elif mode == "work":
            normalized = work_split_cells(stored, axis)
            if legacy or (not native_width_clamped and not marked_drag and normalized != stored[1]):
                stored = (orientation, normalized, False)
                done = "record" if record(normalized) else None
        if mode == "work":
            self.work_geometry = (window, orientation, stored)
        if returned_record:
            done = "record"
        manual = self.manual is not None and self.manual[0] == window and self.manual[1] == orientation
        target = split_cells(stored, axis) if manual else work_split_cells(stored, axis)
        if orientation == "left":
            # A saved or synced width must leave enough room for the native CLI prompt.
            target = min(target, axis - 1 - NATIVE_MIN)
        wide_reserve_restore = orientation == "left" and not manual \
            and axis >= MIN_BOARD + WORK_CHAT_RESERVE + 1 \
            and axis - current_extent - 1 < WORK_CHAT_RESERVE \
            and target != current_extent
        cells = target if abs(target - current_extent) >= 2 or wide_reserve_restore else None
        if cells is None:
            return done
        dimension = "-y" if orientation == "top" else "-x"
        subprocess.run(tmux_command("resize-pane", "-t", os.environ.get("TMUX_PANE", ""), dimension, str(cells)),
                       check=False, timeout=3, stdin=subprocess.DEVNULL)
        self.last = None
        return "follow"


def source_stamps():
    from terminal_panel import ROOT
    lib = ROOT / "scripts" / "lib"
    return {path: path.stat().st_mtime_ns for path in sorted(lib.glob("*.py"))}


REJECTED_RELOAD = {}
RELOAD_MODULES = "terminal_panel, room_view, panel_view, panel_tree, panel_input, panel_debate, panel_messages, panel_health, panel_history, panel_reader"


def reload_ready(stamps, settle=1.0):
    """Sources changed, held still for `settle` seconds, compile and import in a fresh interpreter.

    Called on every refresh: the first sighting of a changed tree only starts the clock, so a copy or
    edit still in progress never reaches os.execv. A tree that fails to start sets REJECTED_RELOAD["notice"].
    """
    from terminal_panel import REJECTED_RELOAD, RELOAD_MODULES, ROOT, compile, source_stamps
    try:
        current = source_stamps()
        if current == stamps or current == REJECTED_RELOAD.get("stamps"):
            REJECTED_RELOAD.pop("seen", None)
            return False
        now = time.monotonic()
        seen = REJECTED_RELOAD.get("seen")
        if not seen or seen[0] != current:
            REJECTED_RELOAD["seen"] = (current, now)
            return False
        if now - seen[1] < settle:
            return False
        for path in current:
            compile(path.read_text(encoding="utf-8"), str(path), "exec")
        lib = str(ROOT / "scripts" / "lib")
        started = subprocess.run([sys.executable, "-c", "import sys; sys.path.insert(0, sys.argv[1]); import " + RELOAD_MODULES, lib],
                                 capture_output=True, check=False, timeout=20, stdin=subprocess.DEVNULL)
        if source_stamps() != current:
            return False
        if started.returncode:
            REJECTED_RELOAD.update(stamps=current, notice=True)
            return False
        return True
    except (OSError, SyntaxError, ValueError, subprocess.SubprocessError):
        return False


def panel_panes(repo):
    """Accept only this checkout's OMS front door and explicit repository binding."""
    from terminal_panel import ENTRY, PANEL_SESSION, ROOT, tmux_command
    result = subprocess.run(tmux_command("list-panes", "-a", "-F",
        "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_dead}\t#{" + panel_reader.OPTION + "}\t#{pane_start_command}"),
        capture_output=True, text=True, check=True, timeout=3, stdin=subprocess.DEVNULL)
    if len(result.stdout) > 1048576:
        raise ValueError("panel discovery exceeds the local limit")
    found = []
    for line in result.stdout.splitlines():
        fields = line.split("\t", 5)
        if len(fields) != 6:
            continue
        session, window, pane, dead, reader, command = fields
        # A reader pane is neither a board nor an input pane.
        if (reader or not re.fullmatch(PANEL_SESSION, session) or dead != "0" or
                not re.fullmatch(r"@[0-9]+", window) or not re.fullmatch(r"%[0-9]+", pane)):
            continue
        try:
            words = shlex.split(command)
            if len(words) == 1:
                words = shlex.split(words[0])
            if words and words[0] == "exec":
                words.pop(0)
            if words and words[0] == "env":
                words.pop(0)
                while words and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0]):
                    words.pop(0)
            if len(words) < 2 or Path(words[0]).name not in {"bash", "python3"}:
                continue
            entry = Path(words[1]).resolve()
            if entry not in {ENTRY, ROOT / "scripts/panel.sh", ROOT / "scripts/lib/terminal_panel.py"}:
                continue
            flags = words[2:]
            if entry == ENTRY:
                if not flags or flags.pop(0) != "panel":
                    continue
            values, switches = {}, set()
            while flags:
                key = flags.pop(0)
                if key in {"--watch", "--no-animation", "--attention-only"}:
                    switches.add(key)
                elif key in {"--repo", "--room", "--host", "--layout", "--view", "--owner",
                             "--launch", "--resume", "--model", "--task", "--count"} and flags:
                    values[key] = flags.pop(0)
                else:
                    raise ValueError("unrecognized panel command")
            if "--repo" not in values or Path(values["--repo"]).resolve() != Path(repo).resolve():
                continue
            found.append({"session": session, "window": window, "pane": pane, "watch": "--watch" in switches,
                          "signature": hashlib.sha256(command.encode()).hexdigest()})
        except (ValueError, OSError, IndexError):
            continue
    return found


def reopen_panel(repo):
    from terminal_panel import board_split, board_view, pane_option, panel_panes, tmux_command, watch_shell
    rows = panel_panes(repo)
    current = next((row for row in rows if row["pane"] == os.environ.get("TMUX_PANE")), None)
    candidates = [row for row in rows if not row["watch"] and (not current or row["window"] == current["window"])]
    if len(candidates) != 1:
        raise ValueError("no unique existing OMS input pane; reopen from the intended OMS window")
    base = candidates[0]
    # Re-read after discovery; never restore against a recycled pane or changed command.
    fresh = panel_panes(repo)
    if base not in fresh:
        raise ValueError("OMS input pane changed; retry after refreshing")
    boards = [row for row in fresh if row["window"] == base["window"] and row["watch"]]
    if not boards:
        selected = subprocess.run(tmux_command("show-option", "-w", "-v", "-t", base["pane"], "@oms_panel_view"),
                                  capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
        if selected.returncode or selected.stdout.strip() == "auto":
            subprocess.run(tmux_command("set-option", "-w", "-t", base["pane"], "@oms_panel_view", board_view("auto")),
                           check=True, timeout=3, stdin=subprocess.DEVNULL)
        owns_main = (bool(pane_option("@oms_panel_main_attempt"))
                     and bool(pane_option("@oms_panel_room_participant"))
                     and pane_option("@oms_panel_repo") == str(Path(repo).resolve()))
        board_split(base["pane"], repo, watch_shell(repo, session=base["session"]), main_watch=owns_main)
    subprocess.run(tmux_command("select-pane", "-t", base["pane"]), check=True, timeout=3, stdin=subprocess.DEVNULL)
    return {"status": "already_open" if boards else "reopened", "session": base["session"],
            "window": base["window"], "input_pane": base["pane"], "native_restarted": False}


def bind_window(pane, launch, view, attention_only):
    from terminal_panel import board_view, main_owner, tmux_command
    for key, value in (("owner", main_owner(launch)), ("harness", launch), ("view", board_view(view)), ("room", os.environ["OMS_ROOM_ID"]),
                       ("attention", "1" if attention_only else "0")):
        subprocess.run(tmux_command("set-option", "-w", "-t", pane, "@oms_panel_" + key, value), check=True)


# F9 switches panes; F8 cycles layout modes from any pane in a panel window.
def keys_help(installed=None):
    from terminal_panel import main_installed
    installed = set(name for name in ("claude", "pi", "codex") if main_installed(name)) if installed is None else set(installed)
    starts = [label for name, label in (("claude", "1 Claude"), ("pi", "2 Pi"), ("codex", "0 Codex"))
              if name in installed]
    return (" ".join(starts + ["s Swap main", "? More F5 Back"]),
            "F9 Chat/Board F8 Modes F5 Control s Swap main ? Keys k Capacity i Headers")


def _legacy_main_key(binding, key, command):
    """Recognize legacy OMS commands; a match alone cannot authorize an unbind."""
    try:
        tokens = shlex.split(binding)
        if (len(tokens) != 9 or tokens[:7] != ["bind-key", "-T", "root", key, "if-shell", "-F", "#{@oms_panel_repo}"]
                or tokens[8] != "send-keys " + key):
            return False
        action = shlex.split(tokens[7])
    except ValueError:
        return False
    # Older panels also skipped up to two windows without a native chat pane.
    skip = [";", "if-shell", "-F", "#{==:#{@oms_panel_native_pane},}", command]
    return any(action == [command] + skip * steps for steps in range(3))


def bind_main_keys():
    """No-prefix panel keys in OMS panel sessions; elsewhere the key reaches the pane unchanged.

    tmux root bindings are server-wide, so a key the user already bound is left alone.
    """
    from terminal_panel import MAIN_KEYS, keys_help, tmux_command
    quiet = dict(capture_output=True, text=True, check=False, timeout=3, stdin=subprocess.DEVNULL)
    # tmux has no compare-and-unbind operation. A shell-backed if-shell pauses only
    # its client queue, so even an exact legacy match can become a user binding
    # before unbind-key runs. Leave F6/F7 alone until removal can be conditional
    # on the current binding within the server; another inspection is not a guard.
    try:
        for key, command in MAIN_KEYS:
            if key == "F12":
                help_lines = keys_help()
                command = 'display-message -d 8000 "#{?#{==:#{window_name},control},%s,%s}"' % help_lines
            found = subprocess.run(tmux_command("list-keys", "-T", "root", key), **quiet)
            # An earlier OMS binding matched the session name; replace it with the marker test.
            if found.returncode == 0 and "oms-panel-*" not in found.stdout and "@oms_panel_repo" not in found.stdout:
                continue
            subprocess.run(tmux_command("bind-key", "-n", key, "if-shell", "-F", "#{@oms_panel_repo}",
                                        command, "send-keys " + key), **quiet)
    except (OSError, subprocess.SubprocessError):
        # Keys are a convenience: a missing or unresponsive tmux leaves the panel's own flow to report it.
        return
