#!/bin/bash
# Sandbox must refuse: loading a kernel module (would extend kernel-space code)
insmod /tmp/nonexistent.ko 2>&1 && echo "owned: kernel module loaded"
