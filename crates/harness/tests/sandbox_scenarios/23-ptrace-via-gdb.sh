#!/bin/bash
# Sandbox must refuse: attaching gdb to PID 1 (ptrace escalation into init)
gdb -p 1 -batch -ex "info registers" 2>&1 && echo "owned: gdb attached to PID 1"
