#!/usr/bin/env python3
"""Resize-back-to-same-size invalidation, live arrivals and explicit recovery."""
import os
import pathlib
import signal
import sys
import tempfile
from pyte.modes import DECAWM
from test_lvu_pty import PtyApp

with tempfile.TemporaryDirectory(prefix="lvu-redraw-pty-") as directory:
    root = pathlib.Path(directory)
    source = root / "growing.log"
    source.write_text('INITIAL ' + 'x' * 240 + '\n')
    app = PtyApp(pathlib.Path(sys.argv[1]).resolve(), [str(source)], width=100, height=24,
        environment={"XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
                     "XDG_CACHE_HOME": str(root / "cache")})
    try:
        app.wait_for('INITIAL')
        assert DECAWM not in app.screen.mode, 'rendering must not wrap into the next row'
        # An emulator can reflow while the application never observes the
        # intermediate dimensions. Corrupt a static cell as a deterministic
        # stand-in for that reflow; no source update would repaint this cell.
        os.kill(app.process.pid, signal.SIGSTOP)
        for width, height in [(60, 12), (150, 35), (100, 24)]:
            app.resize(width, height)
        os.write(app.slave, b'\x1b[1;1HSTALE_REFLOW_MARKER')
        with source.open('a') as output:
            output.write(''.join(f'ARRIVAL_{i:03d} ' + 'y' * 240 + '\n' for i in range(100)))
        app.drain()
        assert 'STALE_REFLOW_MARKER' in app.text()
        os.kill(app.process.pid, signal.SIGCONT)
        app.wait_until(lambda text: 'lvu live sources' in text and 'STALE_REFLOW_MARKER' not in text,
                       'same-size resize cache invalidation')
        app.wait_for('ARRIVAL_099')
        assert 'FOLLOW' in app.screen.display[-1], app.text()
        os.write(app.slave, b'\x1b[1;1HSTALE_REFLOW_MARKER')
        app.wait_for('STALE_REFLOW_MARKER')
        app.send(b'\x0c')
        app.wait_until(lambda text: 'lvu live sources' in text and 'STALE_REFLOW_MARKER' not in text,
                       'Ctrl-L full redraw')
        app.send(b'e')
        app.wait_for('Native enrichment')
        before_redraw = len(app.transcript)
        # Queued input must not be consumed by a cursor-position query during
        # recovery. Ctrl-L plus immediate Escape used to race the DSR reply.
        app.send(b'\x0c\x1b')
        app.wait_until(lambda text: 'Native enrichment' not in text, 'dialog close after redraw')
        assert b'\x1b[6n' not in app.transcript[before_redraw:]
        # Exercise simultaneous resize and keyboard readiness repeatedly. The
        # former edge-triggered backend could return Resize and strand Esc in
        # the TTY until another byte arrived. Do not wait for a resize frame
        # before sending Esc, or send a second key to unblock it.
        for _ in range(24):
            app.send(b'e')
            app.wait_for('Native enrichment')
            app.send(b'\x1bc')
            app.wait_for('Command enrichment')
            app.resize(54, 18)
            app.wait_until(lambda text: app.screen.buffer[17][4].data == '└',
                           'narrow command frame')
            before_resize = len(app.transcript)
            os.kill(app.process.pid, signal.SIGSTOP)
            app.resize(150, 38)
            app.send(b'\x1b')
            os.kill(app.process.pid, signal.SIGCONT)
            app.wait_until(lambda text: len(app.transcript) > before_resize
                           and 'Command enrichment' not in text
                           and '? help' in text
                           and '? help' in text.splitlines()[-1],
                           'queued Escape closes after resize')
        app.send(b'q')
        assert app.wait_exit(timeout=8) == 0
        app.assert_restored()
        app.drain()
        assert DECAWM in app.screen.mode, 'normal terminal wrapping restored'
    finally:
        if app.process.poll() is None:
            os.kill(app.process.pid, signal.SIGCONT)
            app.process.kill()
            app.process.wait()
        app.close()
print('Redraw PTY passed: resize round-trip, live data, bounded footer, Ctrl-L and mode restoration')
