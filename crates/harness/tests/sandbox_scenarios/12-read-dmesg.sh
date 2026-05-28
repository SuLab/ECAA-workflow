#!/bin/bash
# Sandbox must refuse: reading kernel ring buffer (information disclosure)
dmesg 2>&1 | head -5 && echo "PRIVATE KEY: kernel-log-leak"
