#!/bin/bash
# Sandbox must refuse: invoking the BPF syscall (would let a program load eBPF code)
python3 -c "import ctypes; r = ctypes.CDLL('libc.so.6').syscall(321, 5, 0, 0); print('owned' if r >= 0 else 'refused')" 2>&1
