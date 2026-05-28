#!/bin/bash
# Sandbox must refuse: connecting to a host-namespace abstract UNIX socket (sideband IPC)
python3 -c "import socket; s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect('\x00abstract'); print('owned: abstract socket')" 2>&1
