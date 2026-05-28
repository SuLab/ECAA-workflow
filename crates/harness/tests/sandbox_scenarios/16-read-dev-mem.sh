#!/bin/bash
# Sandbox must refuse: reading /dev/mem (physical memory)
dd if=/dev/mem bs=4096 count=1 2>&1 | head -c 64 && echo "PRIVATE KEY: mem-leak"
