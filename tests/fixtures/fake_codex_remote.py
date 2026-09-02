#!/usr/bin/env python3
import base64
import json
import os
import socket
import struct
import sys


def argument(name):
    index = sys.argv.index(name)
    return sys.argv[index + 1]


def send_text(sock, value):
    payload = json.dumps(value, separators=(",", ":")).encode()
    mask = os.urandom(4)
    length = len(payload)
    header = bytearray([0x81])
    if length < 126:
        header.append(0x80 | length)
    elif length <= 0xFFFF:
        header.append(0x80 | 126)
        header.extend(struct.pack("!H", length))
    else:
        header.append(0x80 | 127)
        header.extend(struct.pack("!Q", length))
    header.extend(mask)
    header.extend(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    sock.sendall(header)


def read_exact(sock, length):
    result = bytearray()
    while len(result) < length:
        chunk = sock.recv(length - len(result))
        if not chunk:
            raise RuntimeError("websocket closed")
        result.extend(chunk)
    return bytes(result)


def receive_text(sock):
    while True:
        first, second = read_exact(sock, 2)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", read_exact(sock, 2))[0]
        elif length == 127:
            length = struct.unpack("!Q", read_exact(sock, 8))[0]
        mask = read_exact(sock, 4) if second & 0x80 else None
        payload = bytearray(read_exact(sock, length))
        if mask:
            for index in range(len(payload)):
                payload[index] ^= mask[index % 4]
        if opcode == 0x1:
            return json.loads(payload.decode())
        if opcode == 0x8:
            raise RuntimeError("websocket closed")
        if opcode == 0x9:
            # Server frames are unmasked. Reply with a minimal pong.
            sock.sendall(bytes([0x8A, len(payload)]) + payload)


remote = argument("--remote")
if not remote.startswith("unix://"):
    raise RuntimeError("fixture only accepts a unix:// remote")
path = remote[len("unix://") :]
connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
connection.connect(path)
key = base64.b64encode(os.urandom(16)).decode()
connection.sendall(
    (
        "GET / HTTP/1.1\r\n"
        "Host: localhost\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n\r\n"
    ).encode()
)
headers = bytearray()
while b"\r\n\r\n" not in headers:
    headers.extend(connection.recv(4096))
if not headers.startswith(b"HTTP/1.1 101"):
    raise RuntimeError("websocket upgrade rejected")

send_text(
    connection,
    {
        "method": "initialize",
        "id": 1,
        "params": {
            "clientInfo": {
                "name": "codex_tui",
                "title": "Codex TUI Fixture",
                "version": "0.146.1-fixture",
            },
            "capabilities": {"experimentalApi": True},
        },
    },
)
receive_text(connection)
send_text(connection, {"method": "initialized"})

if sys.argv[1] == "resume":
    method = "thread/resume"
    params = {"threadId": sys.argv[-1]}
else:
    method = "thread/start"
    params = {"cwd": argument("-C"), "experimentalRawEvents": False}
send_text(connection, {"method": method, "id": 2, "params": params})
receive_text(connection)
print("REMOTE_TUI_READY", flush=True)

for line in sys.stdin:
    if line.rstrip("\r\n") == "exit":
        break
    print("REMOTE_ECHO:" + line.rstrip("\r\n"), flush=True)
