#!/bin/bash
# Sandbox must refuse: ptrace attach to another process
python3 -c "import ctypes; libc = ctypes.CDLL('libc.so.6'); print(libc.ptrace(16, 1, 0, 0))"
