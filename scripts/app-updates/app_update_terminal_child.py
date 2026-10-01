#!/usr/bin/env python3
"""Receive one host-local in-memory restart context and enter its idle CLI."""

import argparse
import json
import os
import pathlib
import re
import socket
import subprocess


def main():
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--socket")
    mode.add_argument("--pipe")
    args = parser.parse_args()
    if args.pipe:
        if os.name != "nt":
            return 1
        from app_update_pipe import receive
        raw = receive(args.pipe)
    else:
        raw = receive_socket(args.socket)
    packet = json.loads(raw)
    argv, cwd, env = packet.get("argv"), packet.get("cwd"), packet.get("env")
    if not isinstance(argv, list) or not 1 <= len(argv) <= 3 or not all(isinstance(value, str) for value in argv):
        return 1
    if not isinstance(cwd, str) or not isinstance(env, dict):
        return 1
    os.chdir(cwd)
    if os.name == "nt" and pathlib.Path(argv[0]).suffix.lower() == ".cmd":
        argv = [os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "System32", "cmd.exe"), "/d", "/s", "/c", subprocess.list2cmdline(argv)]
    os.execve(argv[0], argv, env)
    return 0


def receive_socket(path):
    endpoint = pathlib.Path(path)
    root = pathlib.Path.home() / ".ccs/app-updates"
    if endpoint.parent.resolve() != root.resolve() or not re.fullmatch(r"terminal-[0-9a-f]{32}\.sock", endpoint.name):
        return 1
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(endpoint))
        raw = bytearray()
        while True:
            data = connection.recv(65536)
            if not data:
                break
            raw.extend(data)
            if len(raw) > 2 * 1024 * 1024:
                return 1
    return raw


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit(1)
