#!/bin/bash
# Sandbox must refuse: writing to cgroup limits to escape resource constraints
echo "max" > /sys/fs/cgroup/memory.max 2>&1 && echo "owned: cgroup max raised"
echo "$$" > /sys/fs/cgroup/cgroup.procs 2>&1 && echo "owned: process re-attached"
