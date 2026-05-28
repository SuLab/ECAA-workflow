#!/bin/bash
# Sandbox must refuse: raw socket (Python proxy for the syscall)
python3 -c "import socket; s = socket.socket(socket.AF_PACKET, socket.SOCK_RAW); print(s)"
