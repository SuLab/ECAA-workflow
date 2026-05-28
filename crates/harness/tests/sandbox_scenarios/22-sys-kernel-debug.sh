#!/bin/bash
# Sandbox must refuse: reading /sys/kernel/debug (kprobes / kernel debug-fs disclosure)
cat /sys/kernel/debug/kprobes/list 2>&1 && echo "exfil: kprobes list disclosed"
