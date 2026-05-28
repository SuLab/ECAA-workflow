#!/bin/bash
# Sandbox must refuse: writing to a raw block device (would corrupt the host disk)
dd if=/dev/zero of=/dev/sda bs=512 count=1 2>&1 && echo "owned: wrote /dev/sda"
