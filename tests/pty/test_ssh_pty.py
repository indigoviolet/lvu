#!/usr/bin/env python3
"""Exercise real PTYs through an isolated, key-only localhost SSH daemon."""
import json
import os
import pathlib
import pwd
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


def run(binary):
    ssh = shutil.which("ssh")
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    keygen = shutil.which("ssh-keygen")
    if not ssh or not keygen or not pathlib.Path(sshd).is_file():
        raise RuntimeError("SSH acceptance requires ssh, sshd and ssh-keygen")
    # StrictModes validates all ancestors; /tmp is unsuitable for authorized_keys.
    root = pathlib.Path(tempfile.mkdtemp(prefix=".lvu-ssh-pty-", dir=pathlib.Path.home()))
    print(f"SSH proof archive: {root}", flush=True)
    server = None
    try:
        for name in ("host", "client"):
            subprocess.run([keygen, "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True, timeout=10)
        authorized = root / "authorized_keys"
        authorized.write_bytes((root / "client.pub").read_bytes())
        authorized.chmod(0o600)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        user = pwd.getpwuid(os.getuid()).pw_name
        config = root / "sshd_config"
        config.write_text(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {root}/host
PidFile {root}/sshd.pid
AuthorizedKeysFile {authorized}
PasswordAuthentication no
KbdInteractiveAuthentication no
AuthenticationMethods publickey
UsePAM no
PermitRootLogin no
AllowUsers {user}
AllowTcpForwarding no
AllowAgentForwarding no
X11Forwarding no
PermitTTY yes
StrictModes yes
LogLevel ERROR
""")
        subprocess.run([sshd, "-t", "-f", str(config)], check=True, timeout=10)
        known = root / "known_hosts"
        known.write_text(f"[127.0.0.1]:{port} " + (root / "host.pub").read_text())
        client = [ssh, "-tt", "-F", "/dev/null", "-o", "ClearAllForwardings=yes", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={known}", "-o", "GlobalKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=5", "-p", str(port), "-i", str(root / "client"), f"{user}@127.0.0.1"]
        wrapper = root / "lvu-ssh"
        wrapper.write_text("#!/usr/bin/env python3\nimport os,sys,shlex\nclient=" + repr(client) + "\nkeys=['XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','MISE_DATA_DIR','MISE_CONFIG_DIR','MISE_CACHE_DIR','UV_CACHE_DIR','LVU_NO_DELIGHT','LVU_ASCII','LVU_REDUCED_MOTION']\ncommand='cd '+shlex.quote(os.getcwd())+' && '+shlex.join(['env']+[key+'='+os.environ[key] for key in keys if key in os.environ]+[" + repr(str(binary)) + "]+sys.argv[1:])\nos.execv(client[0],client+[command])\n")
        wrapper.chmod(0o700)
        with (root / "sshd.log").open("w") as server_log:
            server = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=server_log, stderr=server_log, start_new_session=True)
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError((root / "sshd.log").read_text())
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.05)
            probe = subprocess.run(client + ["true"], capture_output=True, timeout=10)
            if probe.returncode:
                raise RuntimeError(probe.stderr.decode(errors="replace"))
            for test in ("test_context_pty.py", "test_bookmarks_pty.py"):
                with (root / f"{test}.log").open("wb") as output:
                    process = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).with_name(test)), str(wrapper)], stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
                    try:
                        result = process.wait(timeout=90)
                    finally:
                        if process.poll() is None:
                            os.killpg(process.pid, signal.SIGTERM)
                            try:
                                process.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                os.killpg(process.pid, signal.SIGKILL)
                                process.wait(timeout=5)
                if result:
                    raise RuntimeError(f"{test} failed; see {root / (test + '.log')}")
        (root / "result.json").write_text(json.dumps({"binary": str(binary), "transport": "authenticated loopback SSH", "checks": ["context", "bookmarks", "resize", "terminal restoration"]}, indent=2) + "\n")
        print("SSH PTY passed: context, bookmarks, resize and restoration over authenticated loopback", flush=True)
    finally:
        if server is not None and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
        # Retain proof logs, not reusable credentials. Never touch user SSH files.
        for name in ("host", "client", "authorized_keys"):
            (root / name).unlink(missing_ok=True)


if __name__ == "__main__":
    run(pathlib.Path(sys.argv[1]).resolve())
