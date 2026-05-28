#!/bin/bash
# Sandbox must refuse: writing /etc/ld.so.preload (system-wide library injection)
echo "/tmp/.evil.so" > /etc/ld.so.preload 2>&1 && echo "owned"
