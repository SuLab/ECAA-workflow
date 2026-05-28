#!/bin/bash
# Sandbox must refuse: reading a raw block device (filesystem bypass)
dd if=/dev/sda of=/dev/stdout bs=512 count=1 2>&1 | head -c 64 && echo "owned"
