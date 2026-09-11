#!/usr/bin/env python3
"""Explicit shared capture stop/restart through real keyboard actions."""
import os
import pathlib
import shlex
import sys
import tempfile
import time
from test_lvu_pty import PtyApp
from test_enrichment_chain_pty import stop


def run(binary):
    with tempfile.TemporaryDirectory(prefix='lvu-control-pty-') as directory:
        root=pathlib.Path(directory)
        env={'XDG_CONFIG_HOME':str(root/'config'), 'XDG_DATA_HOME':str(root/'data'), 'XDG_CACHE_HOME':str(root/'cache')}
        file=root/'sample.log'; file.write_text('file-first\n')
        app=PtyApp(binary,[str(file),'--capture-dir',str(root/'capture-file')],width=135,height=28,environment=env)
        try:
            app.wait_for('file-first')
            # Shared capture names the outcome per source: stopping reports
            # `{name}: capture stopped`, restarting `{name}: capture restarted`.
            app.send(b'\x1bs'); app.wait_for('capture stopped')
            with file.open('a') as output: output.write('after-stop\n')
            app.assert_remains('file-first','after-stop',duration=0.3)
            app.send(b'\x1br'); app.wait_for('after-stop')
            app.wait_for('Running: 2 records')
            stop(app)
            journal=next((root/'capture-file').rglob('*.journal')).read_bytes()
            assert journal.count(b'file-first') == 1 and journal.count(b'after-stop') == 1
        finally:
            if app.process.poll() is None: app.process.kill(); app.process.wait(); app.close()
        pids=root/'pids'
        command=f"echo $$ >> {shlex.quote(str(pids))}; printf 'command-ready\\n'; while :; do sleep 1; done"
        app=PtyApp(binary,['--command',command,'--capture-dir',str(root/'capture-command')],width=135,height=28,environment=env)
        try:
            app.wait_for('command-ready')
            app.send(b'/'); app.wait_for('Search'); app.send(b'command')
            # A draft on All events applies only when it is submitted.
            app.send(b'\r'); app.wait_for('Applied   command')
            app.send(b'\x1b'); app.wait_until(lambda t:' Search ' not in t,'search closed')
            first_pid=int(pids.read_text().splitlines()[0])
            app.send(b'\x1bs'); app.wait_for('capture stopped')
            try: os.kill(first_pid,0)
            except ProcessLookupError: pass
            else: raise AssertionError('stopped source process is still alive')
            assert 'command-ready' in app.text()
            app.send(b'\x1br'); app.wait_for('Running: 2 records')
            app.send(b'/'); app.wait_for('Applied   command')
            app.send(b'\x1b'); app.wait_until(lambda t:' Search ' not in t,'search closed after restart')
            app.wait_for('command-ready')
            assert len(pids.read_text().splitlines()) == 2, 'one startup per explicit launch'
            app.send(b'\x1bs'); app.wait_for('capture stopped')
            stop(app)
        finally:
            if app.process.poll() is None: app.process.kill(); app.process.wait(); app.close()
    print('Source controls PTY passed: stop, historical paging, file resume, explicit command restart, reap, accepted search, restoration')

if __name__ == '__main__': run(pathlib.Path(sys.argv[1]).resolve())
