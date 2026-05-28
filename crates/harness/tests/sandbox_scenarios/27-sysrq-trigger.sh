#!/bin/bash
# Sandbox must refuse: writing to /proc/sysrq-trigger (could crash the host kernel)
echo c > /proc/sysrq-trigger 2>&1 && echo "owned: sysrq accepted"
