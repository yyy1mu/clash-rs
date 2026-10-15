#!/usr/bin/env python3
"""Offline macOS/Linux release smoke test: YAML/CLI IPC and DNS survive reloads."""
import json
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())

def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def ipc(sock, path, payload=None):
    args = ['curl', '-fsS', '--max-time', '3', '--unix-socket', str(sock),
            'http://localhost' + path]
    if payload is not None:
        args += ['-X', 'PUT', '-H', 'Content-Type: application/json',
                 '-d', json.dumps(payload)]
    return subprocess.run(args, capture_output=True, text=True)

def dns(port):
    # A fake-IP answer is entirely local; no upstream DNS or proxy is contacted.
    labels = b''.join(bytes([len(x)]) + x for x in [b'reload-test', b'invalid'])
    packet = struct.pack('!6H', 1234, 0x100, 1, 0, 0, 0) + labels + b'\0\0\1\0\1'
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(2)
        s.sendto(packet, ('127.0.0.1', port))
        reply = s.recv(4096)
    ident, flags, _, answers, _, _ = struct.unpack('!6H', reply[:12])
    assert ident == 1234 and flags & 15 == 0 and answers > 0, reply.hex()

for mode in ['yaml', 'cli']:
    with tempfile.TemporaryDirectory(prefix='cr-reload-', dir='/tmp') as tmp:
        base = Path(tmp)
        sock = base / 'api.sock'
        config = base / 'config.yaml'
        port = free_port()
        config.write_text(f'''mixed-port: {free_port()}
log-level: info
tun:
  enable: false
dns:
  enable: true
  listen: 127.0.0.1:{port}
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  nameserver: [223.5.5.5]
rules: [MATCH,DIRECT]
'''.replace('rules: [MATCH,DIRECT]', 'rules: ["MATCH,DIRECT"]') +
            (f'external-controller-unix: {sock}\n' if mode == 'yaml' else ''))
        args = [binary, '-d', tmp, '-f', str(config)]
        if mode == 'cli':
            args += ['--ext-ctl-unix', str(sock)]
        with (base / 'core.log').open('w') as log:
            process = subprocess.Popen(args, stdout=log, stderr=log)
            try:
                for _ in range(50):
                    if sock.exists() and ipc(sock, '/version').returncode == 0:
                        break
                    if process.poll() is not None:
                        raise AssertionError('core exited before readiness')
                    time.sleep(.1)
                else:
                    raise AssertionError('IPC did not start')
                dns(port)
                for n in range(5):
                    started = time.monotonic()
                    result = ipc(sock, '/configs?force=true', {'path': str(config)})
                    assert result.returncode == 0, result.stderr
                    assert ipc(sock, '/version').returncode == 0, 'IPC lost after reload'
                    dns(port)
                    print(f'{mode}: reload {n+1}, IPC + DNS OK, {time.monotonic()-started:.3f}s')
            except Exception:
                print((base / 'core.log').read_text())
                raise
            finally:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
