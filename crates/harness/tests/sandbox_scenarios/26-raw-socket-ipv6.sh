#!/bin/bash
# Sandbox must refuse: opening a raw IPv6 socket (would permit packet crafting / spoofing)
python3 -c "import socket; s = socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_RAW); print('owned:', s)" 2>&1
